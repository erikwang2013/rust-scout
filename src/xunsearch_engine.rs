//! XunSearch 引擎：xunsearchd 原生二进制 TCP 协议（index 默认 8383、search
//! 8384），封包/字段方案纯逻辑见 `xunsearch_query`。每操作一条短连接。

use std::sync::Mutex;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::engine::{Engine, EngineFuture};
use crate::xunsearch_query::*;
use crate::{SearchBuilder, SearchDocument, SearchHit, SearchResult};

pub struct XunSearchEngine {
    /// 项目名（CMD_USE 的 buf）：校验通过的值在 `Ok`，被拒的原始名留在 `Err`
    /// 仅供报错（构造签名 infallible，见 [`Self::new`]）。
    project: std::result::Result<String, String>,
    index_addr: String,
    search_addr: String,
    scheme: Mutex<FieldScheme>,
    has_ini: bool,
}

impl XunSearchEngine {
    /// `host` 形如 `"127.0.0.1:8383"`（index 端口；search 取 port+1）。`ini_path`
    /// 为字段方案 ini（vno 是客户端约定，update 与 search 必须用同一份）；缺省
    /// 用动态方案（id vno=0，其余字段 index=both 全字符串）。
    ///
    /// `project` 与库名同进 CMD_USE 包、同是服务端路径片段，因此走同一套
    /// [`crate::validate_index_name`]：`"../../other_project"` 会跳出项目 home。
    /// 签名是公共 API 不能返回 `Result`，校验失败存成 `Err`，首次请求时报错。
    pub fn new(host: &str, project: &str, ini_path: Option<&str>) -> Self {
        let ini = ini_path
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| FieldScheme::from_ini(&text));
        let (index_addr, search_addr) = Self::split_addrs(host);
        let has_ini = ini.is_some();
        let project = crate::validate_index_name(project)
            .map(|()| project.to_string())
            .map_err(|_| project.to_string());
        Self {
            project,
            index_addr,
            search_addr,
            scheme: Mutex::new(ini.unwrap_or_else(FieldScheme::default)),
            has_ini,
        }
    }

    /// CMD_USE 的项目名；构造时被拒的名字在此报 `InvalidIndexName`（所有
    /// 发请求的方法都经过 [`Self::use_project`]）。
    fn project_name(&self) -> crate::Result<&str> {
        self.project
            .as_ref()
            .map(String::as_str)
            .map_err(|raw| crate::ScoutError::InvalidIndexName(raw.clone()))
    }

    /// host 拆成 index/search 两端口；无端口或端口非法时默认 8383/8384。
    fn split_addrs(host: &str) -> (String, String) {
        match host.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && p.parse::<u16>().is_ok() => {
                let p = p.parse::<u16>().unwrap();
                match p.checked_add(1) {
                    Some(search) => (format!("{h}:{p}"), format!("{h}:{search}")),
                    // 65535 没有下一个端口；退回默认对，别溢出成 0（0 不是可连端口）
                    None => (format!("{h}:8383"), format!("{h}:8384")),
                }
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
        let project = self.project_name()?; // 项目名校验先于协议（同库名）
        with_timeout(stream.write_all(&pack_cmd(CMD_USE, 0, 0, project.as_bytes(), &[]))).await?;
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
        // 空集合走基准语义，不能和「本引擎不支持 where_in」混为一谈：
        // 空的 IN 集合 = 不匹配任何（短路空结果）；空的 NOT IN = 无过滤（等同没有该条件）。
        // 只有**有值**的 where_in/where_not_in 才报 Unsupported —— 否则
        // `where_in("tag", [])` 在别的驱动返回 Ok(空)，在这里却报错。
        if builder.where_ins.iter().any(|(_, v)| v.is_empty()) {
            return Ok(SearchResult::default());
        }
        let unsupported = !builder.where_ins.is_empty()
            || builder.where_not_ins.iter().any(|(_, v)| !v.is_empty());
        if unsupported {
            return Err(crate::ScoutError::Unsupported("xunsearch: where_in/where_not_in not supported; use where_field (QUERY_RANGE)".to_string()));
        }
        let mut stream = self.connect(true).await?;
        // index=None ≡ "default"（与其余七个驱动一致）：否则
        // `update([index Some("default")])` 写的是 default 库、
        // `search(无 index)` 读的却是服务端默认库，两边各自都是空。
        self.use_project(&mut stream, Some(builder.index.as_deref().unwrap_or("default"))).await?;
        let mut buf = self.build_silent(builder)?;
        // 发给服务端的窗口不变（take，缺省 10）；接受上限再套 MAX_HITS
        let limit = builder.take.unwrap_or(10);
        let accepted = limit.min(MAX_HITS);
        let offset_limit = [builder.skip.unwrap_or(0) as u32, limit as u32].map(u32::to_le_bytes).concat();
        buf.extend_from_slice(&pack_cmd(CMD_SEARCH_GET_RESULT, 0, 0, builder.query.as_bytes(), &offset_limit));
        with_timeout(stream.write_all(&buf)).await?;
        self.read_result(&mut stream, accepted).await
    }

    /// 读结果流。`limit` = 接受的上界（请求窗口与 `MAX_HITS` 取小）。
    ///
    /// 两道上限，缺一不可：
    /// - 条数：合法服务端不会回超过自己被告知的 limit 的文档数，`total` 是**服务端
    ///   自报**的、不能当上界，所以用 limit。超过即报错（不截断）—— 原先没有上限，
    ///   对端一直发 `CMD_SEARCH_RESULT_DOC` 帧（8 字节的零长度包也算一条）就能把
    ///   hits 撑到 OOM。
    /// - 整段流的总预算：5s 超时是 per-read 的，源源不断的包会把它一次次刷新，
    ///   条数上限挡不住只发 FACETS/MATCHED 帧的对端。
    async fn read_result(&self, stream: &mut TcpStream, limit: usize) -> crate::Result<SearchResult> {
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
        let body = async {
            let mut hits: Vec<SearchHit> = Vec::new();
            loop {
                let (cmd, arg, buf, _) = read_packet(stream).await?;
                match cmd {
                    CMD_OK if arg == OK_RESULT_END => break,
                    CMD_SEARCH_RESULT_DOC => {
                        if hits.len() >= limit {
                            // 静默截断会被读成「本来就这么多结果」，必须显式失败
                            return Err(crate::ScoutError::XunSearch(format!(
                                "SEARCH_GET_RESULT: peer sent more than the accepted {limit} documents"
                            )));
                        }
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
                            // vno↔名字是**客户端约定**（服务端不交换方案）：无映射说明
                            // 写入端用的是另一份方案，读回的名字会张冠李戴。宁可按
                            // 未知 vno 报错 —— 丢弃这个值会让调用方以为字段本来就是空的。
                            let name = name.ok_or_else(|| {
                                crate::ScoutError::XunSearch(format!(
                                    "search result: field vno {vno} not in the local field scheme (writer used a different scheme)"
                                ))
                            })?;
                            hit.source
                                .as_object_mut()
                                .unwrap()
                                .insert(name, serde_json::Value::String(value));
                        }
                    }
                    CMD_SEARCH_RESULT_FACETS | CMD_SEARCH_RESULT_MATCHED => {}
                    CMD_ERR => return Err(server_err(arg, &buf)),
                    other => {
                        return Err(crate::ScoutError::XunSearch(format!("search result: unexpected cmd {other}")))
                    }
                }
            }
            Ok(hits)
        };
        let hits = match tokio::time::timeout(RESULT_DEADLINE, body).await {
            Ok(Ok(hits)) => hits,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(crate::ScoutError::XunSearch(format!(
                    "SEARCH_GET_RESULT: result stream exceeded the {}s deadline",
                    RESULT_DEADLINE.as_secs()
                )))
            }
        };
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

/// 单次读写 5s 超时（connect 同）：服务端挂死不拖住调用方。
const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// 整段搜索结果流的总预算。per-read 的 5s 会被不断到来的包反复刷新，挡不住
/// 一个慢慢喂包的对端；20s = 4× 单包预算，远大于一页结果的正常耗时。
const RESULT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);
/// 单次搜索最多缓存的命中数（≈ 十几 MB）。`take` 是调用方自己给的，给成天文数字
/// 时它就不再是防线（20s 内环回也能灌进来几十万条），所以要有个和调用方无关的
/// 常数兜底。ponytail: 硬上限，需要更大窗口就调这里（只拒绝、绝不截断）。
const MAX_HITS: usize = 100_000;

async fn with_timeout<T>(fut: impl std::future::Future<Output = std::io::Result<T>>) -> crate::Result<T> {
    tokio::time::timeout(IO_TIMEOUT, fut).await
        .map_err(|_| crate::ScoutError::XunSearch("xunsearch I/O timed out".to_string()))?
        // I/O 失败保留原始 io::Error（调用方还能看 ErrorKind/source）；
        // 超时不是 io::Error，留在上面那条分支。
        .map_err(crate::ScoutError::XunSearchIo)
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

impl Engine for XunSearchEngine {
    fn update<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            if docs.is_empty() {
                return Ok(());
            }
            let mut stream = self.connect(false).await?;
            self.use_project(&mut stream, None).await?;
            // 按 doc.index 分组，`None` ≡ "default"（与其余七个驱动一致：search 那边
            // 读的也是 default 库，两边对得上）。每组都显式 SET_DB —— 组间串库不再
            // 可能，因此也不需要再把「无 index」那组排到最前。
            let mut groups: Vec<(&str, Vec<&SearchDocument>)> = Vec::new();
            for doc in docs {
                let index = doc.index.as_deref().unwrap_or("default");
                match groups.iter_mut().find(|(k, _)| *k == index) {
                    Some((_, g)) => g.push(doc),
                    None => groups.push((index, vec![doc])),
                }
            }
            for (index, group) in groups {
                crate::validate_index_name(index)?; // 系统边界：组级库名校验
                with_timeout(stream.write_all(&pack_cmd(CMD_INDEX_SET_DB, 0, 0, index.as_bytes(), &[]))).await?;
                expect_ok(&mut stream, OK_DB_CHANGED, "CMD_INDEX_SET_DB").await?;
                let mut buf = Vec::new();
                {
                    let mut scheme = self.scheme.lock().expect("xunsearch scheme poisoned");
                    for doc in group {
                        buf.extend_from_slice(&doc_commands(&mut scheme, doc, true)?);
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
            // 无 index ≡ "default"，与其余七个驱动的 `delete_in("default")` 一致
            self.use_project(&mut stream, Some("default")).await?;
            self.send_removes(&mut stream, ids).await
        })
    }

    fn delete_bulk<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        // 默认实现逐 id 调 delete_in：每个 id 一次连接 + CMD_USE/SET_DB 握手（≈4 个
        // 往返）。而 delete_in 本身就是「一次连接发完整批」，直接委托即可 ——
        // 100 个 id 从 100 次连接变成 1 次。
        self.delete_in(index, ids)
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


    /// COMMIT 保证 SUBMIT 数据落盘；504 BUSY / 406 RUNNING 视为成功（已入队）。
    ///
    /// 与其它驱动一样先校验索引名，并把索引作为 db 传给 CMD_USE —— `update` 是按
    /// 索引 SET_DB 写入的，提交默认库不会让命名索引的写入落盘（此前 `_index` 被直接
    /// 忽略，`flush("_all")` 还会静默成功）。
    fn flush<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let mut stream = self.connect(false).await?;
            self.use_project(&mut stream, Some(index)).await?;
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
#[path = "xunsearch_engine_tests.rs"]
mod tests;
