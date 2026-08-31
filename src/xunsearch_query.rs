//! XunSearch（xunsearchd 1.4.x）原生二进制 TCP 协议的纯函数部分：封包/解包、
//! 字段方案（ini 解析 + 缺省动态方案）。网络 I/O 见 [`crate::xunsearch_engine`]。
//!
//! 协议（对照 PHP SDK XSServer.class.php / xs_cmd.h）：每个包 = 8 字节头
//! （cmd u8、arg1 u8、arg2 u8、blen1 u8、blen u32le）+ buf + buf1
//! （buf 在前 buf1 在后）。cmd 0-127 收响应；>=128 为静默命令（不回包），
//! 客户端须与下一条普通命令同一次 write 发出。
#![allow(dead_code)] // 协议纯函数 API：部分仅被本模块测试消费

use std::collections::HashMap;

use crate::document::SearchDocument;

// —— 命令号（xs_cmd.h）——
pub const CMD_USE: u8 = 1;
pub const CMD_INDEX_SET_DB: u8 = 32; // 搜索端口同号（CMD_SEARCH_SET_DB）
pub const CMD_INDEX_SUBMIT: u8 = 34;
pub const CMD_INDEX_REMOVE: u8 = 35;
pub const CMD_INDEX_EXDATA: u8 = 36; // 批量：buf 为多条完整命令序列，一次应答
pub const CMD_INDEX_CLEAN_DB: u8 = 37; // 清空当前库
pub const CMD_DELETE_PROJECT: u8 = 38; // 删除整个项目（rm -rf 语义，引擎禁用）
pub const CMD_INDEX_COMMIT: u8 = 39;
pub const CMD_SEARCH_GET_RESULT: u8 = 66;
pub const CMD_OK: u8 = 128;
pub const CMD_ERR: u8 = 129;
pub const CMD_SEARCH_RESULT_DOC: u8 = 140;
pub const CMD_SEARCH_RESULT_FIELD: u8 = 141;
pub const CMD_SEARCH_RESULT_FACETS: u8 = 142;
pub const CMD_SEARCH_RESULT_MATCHED: u8 = 143;
pub const CMD_DOC_VALUE: u8 = 161;
pub const CMD_DOC_INDEX: u8 = 162;
pub const CMD_INDEX_REQUEST: u8 = 163;
pub const CMD_SEARCH_SET_SORT: u8 = 192; // 静默
pub const CMD_SEARCH_SET_NUMERIC: u8 = 194; // 静默
pub const CMD_QUERY_INIT: u8 = 224; // 静默
pub const CMD_QUERY_PARSE: u8 = 225; // 静默
pub const CMD_QUERY_RANGE: u8 = 228; // 静默：arg2=vno，buf=buf1=值

// —— OK 码 ——
pub const OK_PROJECT: u16 = 201;
pub const OK_RESULT_BEGIN: u16 = 206;
pub const OK_RESULT_END: u16 = 207;
pub const OK_RQST_FINISHED: u16 = 250;
pub const OK_DB_CHANGED: u16 = 251;
pub const OK_DB_CLEAN: u16 = 253;
pub const OK_PROJECT_DEL: u16 = 255;
pub const OK_DB_COMMITED: u16 = 256;

// —— 字段/索引标志 ——
pub const MIXED_VNO: u8 = 255; // 混合索引（body 槽）
pub const WITHPOS: u8 = 0x40; // 索引带位置信息
pub const SAVEVALUE: u8 = 0x80; // 值另存（self 槽可免 DOC_VALUE）
pub const NUMERIC_FLAG: u8 = 0x80; // DOC_VALUE 数值标志
pub const SORT_ASCENDING: u8 = 0x80; // SET_SORT 升序标志
pub const SORT_TYPE_VALUE: u8 = 2; // SET_SORT 按字段值排序

/// 封包：8 字节头 + buf + buf1（blen1 是 u8，buf1 上限 255 字节）。
pub fn pack_cmd(cmd: u8, arg1: u8, arg2: u8, buf: &[u8], buf1: &[u8]) -> Vec<u8> {
    debug_assert!(buf1.len() <= 255, "buf1 exceeds 255 bytes");
    let mut out = Vec::with_capacity(8 + buf.len() + buf1.len());
    out.push(cmd);
    out.push(arg1);
    out.push(arg2);
    out.push(buf1.len() as u8);
    out.extend_from_slice(&(buf.len() as u32).to_le_bytes());
    out.extend_from_slice(buf);
    out.extend_from_slice(buf1);
    out
}

/// 解包：返回 (cmd, arg, buf, buf1)；数据不足 8 字节或长度不符返回 None。
pub fn parse_packet(data: &[u8]) -> Option<(u8, u16, &[u8], &[u8])> {
    if data.len() < 8 {
        return None;
    }
    let blen1 = data[3] as usize;
    let blen = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    if 8 + blen + blen1 > data.len() {
        return None;
    }
    Some((
        data[0],
        (u16::from(data[1]) << 8) | u16::from(data[2]),
        &data[8..8 + blen],
        &data[8 + blen..8 + blen + blen1],
    ))
}

/// JSON 值 → 协议值字节（数值/布尔用其字符串形式；对象/数组/空用空串）。
pub fn value_bytes(value: &serde_json::Value) -> Vec<u8> {
    match value {
        serde_json::Value::String(s) => s.as_bytes().to_vec(),
        serde_json::Value::Number(n) => n.to_string().into_bytes(),
        serde_json::Value::Bool(b) => vec![if *b { b'1' } else { b'0' }],
        _ => Vec::new(),
    }
}

// —— 文档编码 ——

/// 单个文档的索引命令块（不含 SUBMIT）：`update=true` 走 UPDATE（arg1=1、
/// buf=小写主键）；缺省方案新字段动态分配 vno，ini 方案未声明字段跳过。
pub fn doc_commands(scheme: &mut FieldScheme, doc: &SearchDocument, update: bool) -> Vec<u8> {
    let id_vno = scheme.id_vno();
    let id_name = scheme.id_name().to_string();
    let mut out = Vec::new();
    if update {
        out.extend_from_slice(&pack_cmd(
            CMD_INDEX_REQUEST,
            1,
            id_vno,
            doc.id.to_lowercase().as_bytes(),
            &[],
        ));
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
        let bytes = value_bytes(value);
        if bytes.is_empty() {
            continue; // 空值跳过（xapian 拒绝空词）
        }
        scheme.add_dynamic(name);
        if let Some(field) = scheme.field(name) {
            out.extend_from_slice(&index_field(field, &bytes));
        }
    }
    out
}

/// 字段索引命令：内置分词器按 index 标志发 DOC_INDEX（mixed vno=255 / self
/// vno+SAVEVALUE），无 self 槽或 numeric 再补 DOC_VALUE；自定义分词器只存值。
pub fn index_field(field: &FieldDef, value: &[u8]) -> Vec<u8> {
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

// —— 字段方案 ——

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FieldKind {
    Id,
    Title,
    Body,
    String,
    Numeric,
    Date,
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub vno: u8,
    pub kind: FieldKind,
    pub index_self: bool, // 独立索引槽（DOC_INDEX arg2=vno）
    pub index_mixed: bool, // 混合索引（DOC_INDEX arg2=255）
    pub with_pos: bool, // 索引带位置（WITHPOS）
    pub weight: u8,
    pub tokenizer: Option<String>,
}

impl FieldDef {
    /// 自定义分词器（非 full）→ 只存值不索引。
    pub fn custom_tokenizer(&self) -> bool {
        self.tokenizer.as_deref().is_some_and(|t| t != "full")
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self.kind, FieldKind::Numeric | FieldKind::Date)
    }
}

/// 字段方案：vno 是客户端约定（服务端不交换方案），update 与 search 必须
/// 用同一份。缺省方案全动态：id vno=0，新字段按出现顺序取 vno=1,2,…；
/// ini 方案固定 vno：id=0、body=255（不占序号），其余按声明顺序。
pub struct FieldScheme {
    fields: Vec<FieldDef>,
    by_name: HashMap<String, usize>,
    dynamic: bool,
}

/// ini 的单个字段段（`[name]` + type/index/weight/tokenizer 键）。
struct Section {
    name: String,
    kind: FieldKind,
    index: String,
    weight: u8, // 0 = 未声明，按 kind 取默认
    tokenizer: Option<String>,
}

impl Default for Section {
    fn default() -> Self {
        Self { name: String::new(), kind: FieldKind::String, index: String::new(), weight: 0, tokenizer: None }
    }
}

fn parse_kind(value: &str) -> Option<FieldKind> {
    match value {
        "id" => Some(FieldKind::Id),
        "title" => Some(FieldKind::Title),
        "body" => Some(FieldKind::Body),
        "string" => Some(FieldKind::String),
        "numeric" => Some(FieldKind::Numeric),
        "date" => Some(FieldKind::Date),
        _ => None,
    }
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value)
        .to_string()
}

impl FieldScheme {
    pub fn default() -> Self {
        let mut scheme = Self { fields: Vec::new(), by_name: HashMap::new(), dynamic: true };
        scheme.push(FieldDef {
            name: "id".to_string(),
            vno: 0,
            kind: FieldKind::Id,
            index_self: true,
            index_mixed: false,
            with_pos: false,
            weight: 1,
            tokenizer: None,
        });
        scheme
    }

    /// 解析 xunsearch 项目 ini（xs-ctl 生成，如 `[id] type=id`、
    /// `[title] type=title index=both weight=5`）。无 id 字段返回 None。
    pub fn from_ini(ini: &str) -> Option<Self> {
        let mut scheme = Self { fields: Vec::new(), by_name: HashMap::new(), dynamic: false };
        let mut section: Option<Section> = None;
        for raw in ini.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                if let Some(sec) = section.take() {
                    scheme.add_section(sec);
                }
                section = Some(Section { name: name.trim().to_string(), ..Section::default() });
            } else if let Some((key, value)) = line.split_once('=') {
                if let Some(sec) = section.as_mut() {
                    match key.trim() {
                        "type" => sec.kind = parse_kind(&unquote(value)).unwrap_or(FieldKind::String),
                        "index" => sec.index = unquote(value),
                        "weight" => sec.weight = unquote(value).parse().unwrap_or(0),
                        "tokenizer" => sec.tokenizer = Some(unquote(value)),
                        _ => {} // 其余键（charset 等）忽略
                    }
                }
            }
        }
        if let Some(sec) = section {
            scheme.add_section(sec);
        }
        if !scheme.fields.iter().any(|f| f.kind == FieldKind::Id) {
            return None;
        }
        Some(scheme)
    }

    fn push(&mut self, field: FieldDef) {
        self.by_name.insert(field.name.clone(), self.fields.len());
        self.fields.push(field);
    }

    /// 缺省方案下动态登记新字段；ini 方案或字段已存在时为 no-op。
    pub fn add_dynamic(&mut self, name: &str) {
        if !self.dynamic || self.has_field(name) || name == self.id_name() {
            return;
        }
        let vno = (1..=254).find(|v| !self.fields.iter().any(|f| f.vno == *v)).unwrap_or(1);
        self.push(FieldDef {
            name: name.to_string(),
            vno,
            kind: FieldKind::String,
            index_self: true,
            index_mixed: true,
            with_pos: false,
            weight: 1,
            tokenizer: None,
        });
    }

    pub fn has_field(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    pub fn field(&self, name: &str) -> Option<&FieldDef> {
        self.by_name.get(name).map(|&i| &self.fields[i])
    }

    pub fn id_vno(&self) -> u8 {
        self.fields.iter().find(|f| f.kind == FieldKind::Id).map(|f| f.vno).unwrap_or(0)
    }

    pub fn id_name(&self) -> &str {
        self.fields.iter().find(|f| f.kind == FieldKind::Id).map(|f| f.name.as_str()).unwrap_or("id")
    }

    /// 数值字段 vno 列表（SET_NUMERIC 用，按声明序）。
    pub fn numeric_vnos(&self) -> Vec<u8> {
        self.fields.iter().filter(|f| f.is_numeric()).map(|f| f.vno).collect()
    }

    pub fn name_for_vno(&self, vno: u8) -> Option<&str> {
        self.fields.iter().find(|f| f.vno == vno).map(|f| f.name.as_str())
    }

    fn add_section(&mut self, sec: Section) {
        let index = if sec.index.is_empty() {
            match sec.kind {
                FieldKind::Id => "self",
                FieldKind::Title | FieldKind::Body => "both",
                _ => "none",
            }
        } else {
            sec.index.as_str()
        };
        let (index_self, index_mixed, with_pos) = match index {
            "both" => (true, true, true),
            "self" => (true, false, false),
            _ => (false, false, false),
        };
        let weight = if sec.weight > 0 {
            sec.weight
        } else if sec.kind == FieldKind::Title {
            5 // xunsearch 默认 title 权重
        } else {
            1
        };
        let vno = if sec.kind == FieldKind::Id {
            0
        } else if sec.kind == FieldKind::Body {
            MIXED_VNO
        } else {
            (1..=254).find(|v| !self.fields.iter().any(|f| f.vno == *v)).unwrap_or(1)
        };
        // id 重复声明时首个为准
        if sec.kind == FieldKind::Id && self.fields.iter().any(|f| f.kind == FieldKind::Id) {
            return;
        }
        self.push(FieldDef {
            name: sec.name,
            vno,
            kind: sec.kind,
            index_self,
            index_mixed,
            with_pos,
            weight,
            tokenizer: sec.tokenizer,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_cmd_matches_php_format() {
        // 对照 PHP SDK pack('CCCCIN', ...)：cmd/arg1/arg2/blen1 各 1 字节，blen u32le
        assert_eq!(
            pack_cmd(CMD_QUERY_INIT, 0, 0, &[], &[]),
            vec![CMD_QUERY_INIT, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            pack_cmd(1, 2, 3, b"hi", b"x"),
            vec![1, 2, 3, 1, 2, 0, 0, 0, b'h', b'i', b'x']
        );
        assert_eq!(
            pack_cmd(CMD_QUERY_RANGE, 0, 7, &[1, 2, 3], &[1, 2, 3]),
            vec![228, 0, 7, 3, 3, 0, 0, 0, 1, 2, 3, 1, 2, 3]
        );
    }

    #[test]
    fn parse_packet_round_trips_and_rejects_short_data() {
        let data = pack_cmd(CMD_USE, 0, 0, b"books", &[]);
        let (cmd, arg, buf, buf1) = parse_packet(&data).unwrap();
        assert_eq!(cmd, CMD_USE);
        assert_eq!(arg, 0);
        assert_eq!(buf, b"books");
        assert!(buf1.is_empty());
        assert_eq!(parse_packet(&data[..7]), None); // 头不足 8 字节
        let bad = pack_cmd(CMD_OK, 0, 0, b"abc", &[]);
        assert_eq!(parse_packet(&bad[..9]), None); // 长度字段超出实际数据
    }

    #[test]
    fn ini_scheme_parses_vnos_and_flags() {
        let ini = r#"
project.name = demo

[id]
type = id

[title]
type = title
index = both
weight = 5

[content]
type = body

[status]
type = numeric
index = none

[tags]
type = string
tokenizer = none
"#;
        let scheme = FieldScheme::from_ini(ini).unwrap();
        assert_eq!(scheme.id_vno(), 0);
        assert_eq!(scheme.id_name(), "id");
        let title = scheme.field("title").unwrap();
        assert_eq!(title.vno, 1);
        assert!(title.index_self && title.index_mixed && title.with_pos);
        assert_eq!(title.weight, 5);
        let body = scheme.field("content").unwrap();
        assert_eq!(body.vno, MIXED_VNO);
        let status = scheme.field("status").unwrap();
        assert_eq!(status.vno, 2);
        assert!(!status.index_self && !status.index_mixed);
        assert_eq!(scheme.numeric_vnos(), vec![2]);
        let tags = scheme.field("tags").unwrap();
        assert!(tags.custom_tokenizer()); // tokenizer=none 非 full → 只存值
        assert!(!tags.index_self);
        assert_eq!(scheme.name_for_vno(1), Some("title"));
        assert_eq!(scheme.name_for_vno(255), Some("content"));
        assert_eq!(scheme.name_for_vno(9), None);
    }

    #[test]
    fn ini_without_id_field_is_rejected() {
        assert!(FieldScheme::from_ini("[title]\ntype = title\n").is_none());
        assert!(FieldScheme::from_ini("").is_none());
    }

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
}
