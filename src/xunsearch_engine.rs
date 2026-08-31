//! XunSearch 引擎：xunsearchd 原生二进制 TCP 协议（index 默认 8383、search
//! 8384），封包/字段方案纯逻辑见 `xunsearch_query`。每操作一条短连接。

use std::sync::Mutex;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::engine::{Engine, EngineFuture};
use crate::xunsearch_query::*;
use crate::{SearchBuilder, SearchDocument, SearchHit, SearchResult};

pub struct XunSearchEngine {
    project: String,
    index_addr: String,
    search_addr: String,
    scheme: Mutex<FieldScheme>,
    has_ini: bool,
}

impl XunSearchEngine {
    /// `host` 形如 `"127.0.0.1:8383"`（index 端口；search 取 port+1）。`ini_path`
    /// 为字段方案 ini（vno 是客户端约定，update 与 search 必须用同一份）；缺省
    /// 用动态方案（id vno=0，其余字段 index=both 全字符串）。
    pub fn new(host: &str, project: &str, ini_path: Option<&str>) -> Self {
        let ini = ini_path
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| FieldScheme::from_ini(&text));
        let (index_addr, search_addr) = Self::split_addrs(host);
        let has_ini = ini.is_some();
        Self {
            project: project.to_string(),
            index_addr,
            search_addr,
            scheme: Mutex::new(ini.unwrap_or_else(FieldScheme::default)),
            has_ini,
        }
    }

    /// host 拆成 index/search 两端口；无端口或端口非法时默认 8383/8384。
    fn split_addrs(host: &str) -> (String, String) {
        match host.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && p.parse::<u16>().is_ok() => {
                let p = p.parse::<u16>().unwrap();
                (format!("{h}:{p}"), format!("{h}:{}", p + 1))
            }
            Some((h, _)) if !h.is_empty() => (format!("{h}:8383"), format!("{h}:8384")),
            _ => (format!("{host}:8383"), format!("{host}:8384")),
        }
    }

    async fn connect(&self, search: bool) -> crate::Result<TcpStream> {
        let addr = if search { &self.search_addr } else { &self.index_addr };
        with_timeout(TcpStream::connect(addr)).await
    }

    async fn use_project(&self, stream: &mut TcpStream, db: Option<&str>) -> crate::Result<()> {
        with_timeout(stream.write_all(&pack_cmd(CMD_USE, 0, 0, self.project.as_bytes(), &[]))).await?;
        expect_ok(stream, OK_PROJECT, "CMD_USE").await?;
        if let Some(db) = db {
            crate::validate_index_name(db)?; // 系统边界：库名校验先于协议
            with_timeout(stream.write_all(&pack_cmd(CMD_INDEX_SET_DB, 0, 0, db.as_bytes(), &[]))).await?;
            expect_ok(stream, OK_DB_CHANGED, "CMD_INDEX_SET_DB").await?;
        }
        Ok(())
    }

    /// 搜索静默命令（QUERY_INIT/PARSE、SET_SORT、SET_NUMERIC、QUERY_RANGE），与 GET_RESULT 同一次 write 发出。
    fn build_silent(&self, builder: &SearchBuilder) -> crate::Result<Vec<u8>> {
        // SET_SORT 单字段；多字段排序服务端不支持，明确 Unsupported。
        if builder.orders.len() > 1 {
            return Err(crate::ScoutError::Unsupported("xunsearch: multiple order_by not supported (SET_SORT 单字段)".to_string()));
        }
        let scheme = self.scheme.lock().expect("xunsearch scheme poisoned");
        let mut out = Vec::new();
        out.extend_from_slice(&pack_cmd(CMD_QUERY_INIT, 0, 0, &[], &[]));
        out.extend_from_slice(&pack_cmd(CMD_QUERY_PARSE, 0, 0, builder.query.as_bytes(), &[]));
        if let Some(order) = builder.orders.first() {
            let vno = field_vno(&scheme, &order.field, "order")?;
            let flag = if order.desc { 0 } else { SORT_ASCENDING };
            out.extend_from_slice(&pack_cmd(CMD_SEARCH_SET_SORT, SORT_TYPE_VALUE | flag, vno, &[], &[]));
        }
        for vno in scheme.numeric_vnos() {
            out.extend_from_slice(&pack_cmd(CMD_SEARCH_SET_NUMERIC, 0, vno, &[], &[]));
        }
        // wheres 等值 → QUERY_RANGE(from==to)，走存储值槽，不依赖字段被索引。
        for w in &builder.wheres {
            let vno = field_vno(&scheme, &w.field, "where")?;
            let value = value_bytes(&w.value);
            if value.len() > 255 {
                return Err(crate::ScoutError::XunSearch(format!("xunsearch: where_field `{}` value exceeds 255 bytes (QUERY_RANGE buf1 上限)", w.field)));
            }
            out.extend_from_slice(&pack_cmd(CMD_QUERY_RANGE, 0, vno, &value, &value));
        }
        Ok(out)
    }

    async fn do_search(&self, builder: &SearchBuilder) -> crate::Result<SearchResult> {
        if builder.trashed == crate::TrashedFilter::OnlyTrashed {
            return Err(crate::ScoutError::Unsupported("xunsearch: only_trashed requires soft_delete, which this engine does not implement".to_string()));
        }
        if !builder.where_ins.is_empty() || !builder.where_not_ins.is_empty() {
            return Err(crate::ScoutError::Unsupported("xunsearch: where_in/where_not_in not supported; use where_field (QUERY_RANGE)".to_string()));
        }
        let mut stream = self.connect(true).await?;
        self.use_project(&mut stream, builder.index.as_deref()).await?;
        let mut buf = self.build_silent(builder)?;
        let offset_limit = [builder.skip.unwrap_or(0) as u32, builder.take.unwrap_or(10) as u32].map(u32::to_le_bytes).concat();
        buf.extend_from_slice(&pack_cmd(CMD_SEARCH_GET_RESULT, 0, 0, builder.query.as_bytes(), &offset_limit));
        with_timeout(stream.write_all(&buf)).await?;
        self.read_result(&mut stream).await
    }

    async fn read_result(&self, stream: &mut TcpStream) -> crate::Result<SearchResult> {
        let id_vno = self.scheme.lock().expect("xunsearch scheme poisoned").id_vno();
        let (cmd, arg, buf, _) = read_packet(stream).await?;
        if cmd == CMD_ERR {
            return Err(server_err(arg, &buf));
        }
        if cmd != CMD_OK || arg != OK_RESULT_BEGIN {
            return Err(crate::ScoutError::XunSearch(format!("SEARCH_GET_RESULT: unexpected response cmd={cmd} arg={arg}")));
        }
        let total = buf
            .get(..4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .unwrap_or(0) as usize;
        let mut hits: Vec<SearchHit> = Vec::new();
        loop {
            let (cmd, arg, buf, _) = read_packet(stream).await?;
            match cmd {
                CMD_OK if arg == OK_RESULT_END => break,
                CMD_SEARCH_RESULT_DOC => {
                    // 20 字节：docid/rank/ccount u32le + percent i32le + weight f32le
                    let weight = buf
                        .get(16..20)
                        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                        .unwrap_or(0.0);
                    hits.push(SearchHit {
                        id: String::new(),
                        score: Some(f64::from(weight)),
                        source: serde_json::Value::Object(Default::default()),
                        highlight: None,
                    });
                }
                CMD_SEARCH_RESULT_FIELD => {
                    let vno = arg as u8;
                    let value = String::from_utf8_lossy(&buf).to_string();
                    let hit = hits.last_mut().ok_or_else(|| {
                        crate::ScoutError::XunSearch("result field before any doc".to_string())
                    })?;
                    if vno == id_vno {
                        hit.id = value;
                    } else {
                        let name = {
                            let scheme = self.scheme.lock().expect("xunsearch scheme poisoned");
                            scheme
                                .name_for_vno(vno)
                                .map(str::to_string)
                                .or_else(|| (vno == MIXED_VNO).then(|| "body".to_string()))
                        };
                        if let Some(name) = name {
                            hit.source
                                .as_object_mut()
                                .unwrap()
                                .insert(name, serde_json::Value::String(value));
                        }
                    }
                }
                CMD_SEARCH_RESULT_FACETS | CMD_SEARCH_RESULT_MATCHED => {}
                CMD_ERR => return Err(server_err(arg, &buf)),
                other => {
                    return Err(crate::ScoutError::XunSearch(format!("search result: unexpected cmd {other}")))
                }
            }
        }
        Ok(SearchResult { hits, total, ..SearchResult::default() })
    }

    async fn send_removes(&self, stream: &mut TcpStream, ids: &[String]) -> crate::Result<()> {
        let id_vno = self.scheme.lock().expect("xunsearch scheme poisoned").id_vno();
        let mut buf = Vec::new();
        for id in ids {
            // 主键小写化（服务端 term 一律小写）。
            buf.extend_from_slice(&pack_cmd(CMD_INDEX_REMOVE, 0, id_vno, id.to_lowercase().as_bytes(), &[]));
        }
        with_timeout(stream.write_all(&buf)).await?;
        for _ in ids {
            expect_ok(stream, OK_RQST_FINISHED, "CMD_INDEX_REMOVE").await?;
        }
        Ok(())
    }
}

fn field_vno(scheme: &FieldScheme, field: &str, what: &str) -> crate::Result<u8> {
    scheme.field(field).map(|f| f.vno).ok_or_else(|| {
        crate::ScoutError::Unsupported(format!("xunsearch: {what} field `{field}` not in field scheme (缺省方案下需先 update 该字段，或提供项目 ini)"))
    })
}

/// 5s 超时读写（connect 同）：服务端挂死不拖住调用方。
const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
async fn with_timeout<T>(fut: impl std::future::Future<Output = std::io::Result<T>>) -> crate::Result<T> {
    tokio::time::timeout(IO_TIMEOUT, fut).await
        .map_err(|_| crate::ScoutError::XunSearch("xunsearch I/O timed out".to_string()))?
        .map_err(|e| crate::ScoutError::XunSearch(format!("xunsearch I/O failed: {e}")))
}

const MAX_PACKET: usize = 16 * 1024 * 1024; // 脏包防护：超过视为协议错误

async fn read_packet(stream: &mut TcpStream) -> crate::Result<(u8, u16, Vec<u8>, Vec<u8>)> {
    let mut header = [0u8; 8];
    with_timeout(stream.read_exact(&mut header)).await?;
    let blen1 = header[3] as usize;
    let blen = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    if blen > MAX_PACKET {
        return Err(crate::ScoutError::XunSearch(format!("xunsearch packet too large: {blen} bytes")));
    }
    let mut buf = vec![0u8; blen];
    with_timeout(stream.read_exact(&mut buf)).await?;
    let mut buf1 = vec![0u8; blen1];
    with_timeout(stream.read_exact(&mut buf1)).await?;
    Ok((header[0], (u16::from(header[1]) << 8) | u16::from(header[2]), buf, buf1))
}

fn server_err(code: u16, buf: &[u8]) -> crate::ScoutError {
    crate::ScoutError::XunSearch(format!("server error {code}: {}", String::from_utf8_lossy(buf).trim()))
}

async fn expect_ok(stream: &mut TcpStream, ok_code: u16, what: &str) -> crate::Result<()> {
    let (cmd, arg, buf, _) = read_packet(stream).await?;
    match cmd {
        CMD_ERR => Err(server_err(arg, &buf)),
        CMD_OK if arg == ok_code => Ok(()),
        CMD_OK => Err(crate::ScoutError::XunSearch(format!("{what}: unexpected OK code {arg}, want {ok_code}"))),
        other => Err(crate::ScoutError::XunSearch(format!("{what}: unexpected cmd {other}"))),
    }
}

/// 单文档索引命令块：`update=true` 走 UPDATE（arg1=1、buf=小写主键）；
/// 缺省方案新字段动态分配 vno，ini 方案未声明字段跳过。
fn doc_commands(scheme: &mut FieldScheme, doc: &SearchDocument, update: bool) -> Vec<u8> {
    let id_vno = scheme.id_vno();
    let id_name = scheme.id_name().to_string();
    let mut out = Vec::new();
    if update {
        out.extend_from_slice(&pack_cmd(CMD_INDEX_REQUEST, 1, id_vno, doc.id.to_lowercase().as_bytes(), &[]));
    } else {
        out.extend_from_slice(&pack_cmd(CMD_INDEX_REQUEST, 0, 0, &[], &[]));
    }
    out.extend_from_slice(&index_field(
        scheme.field(scheme.id_name()).expect("id field missing from scheme"),
        doc.id.as_bytes(),
    ));
    for (name, value) in &doc.fields {
        if name == &id_name {
            continue; // id 值以 doc.id 为准
        }
        scheme.add_dynamic(name);
        if let Some(field) = scheme.field(name) {
            out.extend_from_slice(&index_field(field, &value_bytes(value)));
        }
    }
    out
}

/// 字段索引命令：内置分词器按 index 标志发 DOC_INDEX（mixed vno=255 / self
/// vno+SAVEVALUE），无 self 槽或 numeric 再补 DOC_VALUE；自定义分词器只存值。
fn index_field(field: &FieldDef, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if !field.custom_tokenizer() {
        let base = field.weight | if field.with_pos { WITHPOS } else { 0 };
        if field.index_mixed {
            out.extend_from_slice(&pack_cmd(CMD_DOC_INDEX, base, MIXED_VNO, value, &[]));
        }
        if field.index_self {
            let save = if field.is_numeric() { 0 } else { SAVEVALUE };
            out.extend_from_slice(&pack_cmd(CMD_DOC_INDEX, base | save, field.vno, value, &[]));
        }
        if !field.index_self || field.is_numeric() {
            let flag = if field.is_numeric() { NUMERIC_FLAG } else { 0 };
            out.extend_from_slice(&pack_cmd(CMD_DOC_VALUE, flag, field.vno, value, &[]));
        }
    } else {
        out.extend_from_slice(&pack_cmd(CMD_DOC_VALUE, 0, field.vno, value, &[]));
    }
    out
}

impl Engine for XunSearchEngine {
    fn update<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            if docs.is_empty() {
                return Ok(());
            }
            let mut stream = self.connect(false).await?;
            self.use_project(&mut stream, None).await?;
            // 按 doc.index 分组；无 index 组不 SET_DB，走服务端默认库（"db"），
            // 与 search/delete 的 use_project(None) 读写一致。
            let mut groups: Vec<(Option<&str>, Vec<&SearchDocument>)> = Vec::new();
            for doc in docs {
                let index = doc.index.as_deref();
                match groups.iter_mut().find(|(k, _)| *k == index) {
                    Some((_, g)) => g.push(doc),
                    None => groups.push((index, vec![doc])),
                }
            }
            // 无 index 组排最前，避免落在前一组的 SET_DB 库中（串库）。
            groups.sort_by_key(|(index, _)| index.is_some());
            for (index, group) in groups {
                if let Some(index) = index {
                    crate::validate_index_name(index)?; // 系统边界：组级库名校验
                    with_timeout(stream.write_all(&pack_cmd(CMD_INDEX_SET_DB, 0, 0, index.as_bytes(), &[]))).await?;
                    expect_ok(&mut stream, OK_DB_CHANGED, "CMD_INDEX_SET_DB").await?;
                }
                let mut buf = Vec::new();
                {
                    let mut scheme = self.scheme.lock().expect("xunsearch scheme poisoned");
                    for doc in group {
                        buf.extend_from_slice(&doc_commands(&mut scheme, doc, true));
                    }
                }
                buf.extend_from_slice(&pack_cmd(CMD_INDEX_SUBMIT, 0, 0, &[], &[]));
                with_timeout(stream.write_all(&buf)).await?;
                expect_ok(&mut stream, OK_RQST_FINISHED, "CMD_INDEX_SUBMIT").await?;
            }
            Ok(())
        })
    }

    fn update_bulk<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        // 默认实现逐条 update（每条一连接）；委托 update() 让 M1 分组生效（一批一连接）。
        Box::pin(async move { self.update(docs).await })
    }

    fn delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            if ids.is_empty() {
                return Ok(());
            }
            let mut stream = self.connect(false).await?;
            self.use_project(&mut stream, None).await?;
            self.send_removes(&mut stream, ids).await
        })
    }

    fn delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            if ids.is_empty() {
                return Ok(());
            }
            let mut stream = self.connect(false).await?;
            self.use_project(&mut stream, Some(index)).await?;
            self.send_removes(&mut stream, ids).await
        })
    }

    fn search<'a>(&'a self, builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult> {
        Box::pin(async move { self.do_search(builder).await })
    }

    fn paginate<'a>(
        &'a self,
        builder: &'a SearchBuilder,
        page: usize,
        per_page: usize,
    ) -> EngineFuture<'a, SearchResult> {
        let page = page.max(1);
        let per_page = per_page.max(1);
        Box::pin(async move {
            let mut base = builder.clone();
            base.skip = Some((page - 1).saturating_mul(per_page));
            base.take = Some(per_page);
            self.do_search(&base).await
        })
    }

    fn map_ids(&self, result: &SearchResult) -> Vec<String> {
        result.ids()
    }

    fn flush<'a>(&'a self, _index: &'a str) -> EngineFuture<'a, ()> {
        // COMMIT 保证 SUBMIT 数据落盘；504 BUSY / 406 RUNNING 视为成功（已入队）。
        Box::pin(async move {
            let mut stream = self.connect(false).await?;
            self.use_project(&mut stream, None).await?;
            with_timeout(stream.write_all(&pack_cmd(CMD_INDEX_COMMIT, 0, 0, &[], &[]))).await?;
            match read_packet(&mut stream).await? {
                (CMD_OK, OK_DB_COMMITED, _, _) => Ok(()),
                (CMD_ERR, 504 | 406, _, _) => Ok(()),
                (CMD_ERR, code, buf, _) => Err(server_err(code, &buf)),
                (cmd, arg, _, _) => Err(crate::ScoutError::XunSearch(format!("CMD_INDEX_COMMIT: unexpected cmd={cmd} arg={arg}"))),
            }
        })
    }

    fn create_index<'a>(&'a self, _index: &'a str, _settings: serde_json::Value) -> EngineFuture<'a, ()> {
        // CMD_USE 只建项目 home；ini 无法经协议上传，构造时已提供 → no-op。
        Box::pin(async move {
            if self.has_ini {
                Ok(())
            } else {
                Err(crate::ScoutError::Unsupported("xunsearch: create_index requires a field scheme ini passed to XunSearchEngine::new".to_string()))
            }
        })
    }

    fn delete_index<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        // CLEAN_DB 只清空该库；DELETE_PROJECT 毁掉整个项目，禁用。
        Box::pin(async move {
            let mut stream = self.connect(false).await?;
            self.use_project(&mut stream, None).await?;
            crate::validate_index_name(index)?; // 系统边界
            with_timeout(stream.write_all(&pack_cmd(CMD_INDEX_SET_DB, 0, 0, index.as_bytes(), &[]))).await?;
            expect_ok(&mut stream, OK_DB_CHANGED, "CMD_INDEX_SET_DB").await?;
            with_timeout(stream.write_all(&pack_cmd(CMD_INDEX_CLEAN_DB, 0, 0, &[], &[]))).await?;
            expect_ok(&mut stream, OK_DB_CLEAN, "CMD_INDEX_CLEAN_DB").await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_commands_emit_expected_bytes() {
        let mut scheme = FieldScheme::default();
        let doc = SearchDocument::new("one", serde_json::json!({"title": "rust"})).unwrap();
        let cmds = doc_commands(&mut scheme, &doc, true);
        assert_eq!(
            cmds,
            vec![
                163, 1, 0, 0, 3, 0, 0, 0, b'o', b'n', b'e', // INDEX_REQUEST(UPDATE, vno=0, "one")
                162, 0x81, 0, 0, 3, 0, 0, 0, b'o', b'n', b'e', // DOC_INDEX(id: weight1|SAVEVALUE)
                162, 1, 255, 0, 4, 0, 0, 0, b'r', b'u', b's', b't', // DOC_INDEX(mixed, vno=255)
                162, 0x81, 1, 0, 4, 0, 0, 0, b'r', b'u', b's', b't', // DOC_INDEX(self+SAVEVALUE, vno=1)
            ]
        );
        assert_eq!(scheme.field("title").map(|f| f.vno), Some(1)); // 动态方案已记录新字段
    }

    #[tokio::test]
    async fn search_round_trip_with_mock_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (cmd, _, _, _) = read_packet(&mut sock).await.unwrap();
            assert_eq!(cmd, CMD_USE);
            sock.write_all(&pack_cmd(CMD_OK, 0, OK_PROJECT as u8, &[], &[])).await.unwrap();
            let (cmd, _, _, _) = read_packet(&mut sock).await.unwrap();
            assert_eq!(cmd, CMD_QUERY_INIT);
            let (cmd, _, buf, _) = read_packet(&mut sock).await.unwrap();
            assert_eq!(cmd, CMD_QUERY_PARSE);
            assert_eq!(String::from_utf8_lossy(&buf), "hello");
            let (cmd, _, _, buf1) = read_packet(&mut sock).await.unwrap();
            assert_eq!(cmd, CMD_SEARCH_GET_RESULT);
            assert_eq!(buf1, [0, 0, 0, 0, 10, 0, 0, 0]);
            sock.write_all(&pack_cmd(CMD_OK, 0, OK_RESULT_BEGIN as u8, &1u32.to_le_bytes(), &[])).await.unwrap();
            let mut doc = vec![1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]; // docid/rank/ccount
            doc.extend_from_slice(&100i32.to_le_bytes()); // percent
            doc.extend_from_slice(&3.5f32.to_le_bytes()); // weight
            sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_DOC, 0, 0, &doc, &[])).await.unwrap();
            sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_FIELD, 0, 0, b"one", &[])).await.unwrap();
            sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_FIELD, 0, 255, b"hello world", &[])).await.unwrap();
            sock.write_all(&pack_cmd(CMD_OK, 0, OK_RESULT_END as u8, &[], &[])).await.unwrap();
        });
        // new() 把给定端口当 index 端口、search 取 port+1；监听器开在 search 端口上。
        let engine = XunSearchEngine::new(&format!("127.0.0.1:{}", addr.port() - 1), "books", None);
        let result = engine.search(&SearchBuilder::new("hello")).await.unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].id, "one");
        assert_eq!(result.hits[0].score, Some(3.5));
        assert_eq!(result.hits[0].source, serde_json::json!({"body": "hello world"}));
        server.await.unwrap();
    }
}
