use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 驱动配置。
///
/// 真正生效的只有 `driver` 与 `options`：各便捷构造器
/// （[`ScoutConfig::elasticsearch`] 等）也只是写入这两者。
///
/// 反序列化**不拒绝未知键**（未设 `deny_unknown_fields`），配置文件里多出来的
/// 字段会被忽略而不是报错。
///
/// **`Debug` 会把密钥打码**（见下方手写实现）：`options` 里
/// `*.api_key` / `*secret*` / `*password*` / `*token` 一律渲染成
/// `"<redacted>"`，启动时打印配置不会再漏出凭据。
///
/// [`Serialize`] 仍按原样输出密钥——序列化是写配置文件的正常路径，
/// 打码会破坏读回。要打日志请用 `{:?}`，别用 `serde_json::to_string`。
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ScoutConfig {
    #[serde(default = "default_driver")]
    pub driver: String,
    /// 驱动专属参数，**会被读取**：键形如 `elasticsearch.host` /
    /// `database.url` / `algolia.app_id`，由 [`EngineManager`](crate::EngineManager)
    /// 在构造驱动时取用。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub options: HashMap<String, serde_json::Value>,
}

/// 键名判定：这些 key 在 `Debug` 里打码。
fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.ends_with("api_key")
        || k.ends_with("apikey")
        || k.ends_with("token")
        || k.contains("secret")
        || k.contains("password")
}

impl std::fmt::Debug for ScoutConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let options: HashMap<&str, serde_json::Value> = self
            .options
            .iter()
            .map(|(k, v)| {
                let v = if is_secret_key(k) {
                    serde_json::Value::String("<redacted>".to_string())
                } else {
                    v.clone()
                };
                (k.as_str(), v)
            })
            .collect();
        f.debug_struct("ScoutConfig")
            .field("driver", &self.driver)
            .field("options", &options)
            .finish()
    }
}

impl ScoutConfig {
    pub fn collection() -> Self {
        Self {
            driver: "collection".to_string(),
            ..Self::default()
        }
    }

    pub fn elasticsearch(host: impl Into<String>, api_key: Option<String>) -> Self {
        let mut config = Self::collection();
        config.driver = "elasticsearch".to_string();
        config.insert("elasticsearch.host", host.into());
        if let Some(api_key) = api_key {
            config.insert("elasticsearch.api_key", api_key);
        }
        config
    }

    pub fn opensearch(host: impl Into<String>, api_key: Option<String>) -> Self {
        let mut config = Self::collection();
        config.driver = "opensearch".to_string();
        config.insert("opensearch.host", host.into());
        if let Some(api_key) = api_key {
            config.insert("opensearch.api_key", api_key);
        }
        config
    }

    pub fn meilisearch(host: impl Into<String>, api_key: impl Into<String>) -> Self {
        let mut config = Self::collection();
        config.driver = "meilisearch".to_string();
        config.insert("meilisearch.host", host.into());
        config.insert("meilisearch.api_key", api_key.into());
        config
    }

    pub fn typesense(host: impl Into<String>, api_key: impl Into<String>) -> Self {
        let mut config = Self::collection();
        config.driver = "typesense".to_string();
        config.insert("typesense.host", host.into());
        config.insert("typesense.api_key", api_key.into());
        config
    }

    pub fn algolia(app_id: impl Into<String>, api_key: impl Into<String>) -> Self {
        let mut config = Self::collection();
        config.driver = "algolia".to_string();
        config.insert("algolia.app_id", app_id.into());
        config.insert("algolia.api_key", api_key.into());
        config
    }

    pub fn database(url: impl Into<String>, fields: Vec<String>) -> Self {
        let mut config = Self::collection();
        config.driver = "database".to_string();
        config.insert("database.url", url.into());
        config.insert("database.fields", serde_json::json!(fields));
        config
    }

    pub fn null() -> Self {
        Self {
            driver: "null".to_string(),
            ..Self::collection()
        }
    }

    pub fn xunsearch(host: impl Into<String>, project: impl Into<String>) -> Self {
        let mut config = Self::collection();
        config.driver = "xunsearch".to_string();
        config.insert("xunsearch.host", host.into());
        config.insert("xunsearch.project", project.into());
        config
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) {
        self.options.insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.options.get(key)
    }
}

fn default_driver() -> String {
    "collection".to_string()
}

/// 校验索引名。所有驱动在写入 / 建索引 / 删索引前都应先过这里。
///
/// 除空白、路径分隔符与前导点外，还拒绝**会被后端解释成多索引表达式或通配符**
/// 的字符：`*` `?` `,` `+`，以及前导 `-` / `_`；另外拒绝引号与语句终结符
/// `"` `'` `;` 与反引号（当前查询全走 `?` 绑定，用不到这层，但别让本函数成为
/// 「将来某条拼接语句」的唯一假设）。这条边界很要紧——
/// Elasticsearch 把 `_all` 当「全部索引」，而 `_all` 里没有需要百分号编码的字符，
/// 会原样进到 `DELETE /_all`；ES 7.x 与 OpenSearch 默认
/// `action.destructive_requires_name=false`，一次调用就能删掉整个集群的索引。
/// 前导 `_` 同时也是 ES 保留给系统索引的前缀。
pub fn validate_index_name(index: &str) -> crate::Result<()> {
    let bad = index.is_empty()
        || index.starts_with('.')
        || index.starts_with('-')
        || index.starts_with('_')
        || index.chars().any(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    // 多索引表达式 / 通配符
                    '/' | '\\' | '*' | '?' | ',' | '+'
                    // 引号与语句终结符：当前所有查询都用 `?` 绑定，这些字符不会
                    // 被解释；加在这里是给「将来某条 format! 拼出来的语句」兜底——
                    // 别让这层校验成为唯一的假设。
                    | '"' | '\'' | ';' | '`'
                )
        });
    if bad {
        return Err(crate::ScoutError::InvalidIndexName(index.to_string()));
    }
    Ok(())
}

/// 校验驱动 host：拒绝内嵌 userinfo 的地址（`http://user:pass@host`）。
///
/// reqwest 的错误 [`Display`](std::fmt::Display) 会拼出完整 URL，所以带 userinfo
/// 的 host 只要请求失败一次，密码就跟着错误信息进了日志。凭据请走驱动自己的
/// 参数，别塞进 URL。
///
/// 校验失败时错误信息**不回显**原 host —— 回显就等于换个地方泄漏。
pub fn validate_host(host: &str) -> crate::Result<()> {
    let authority = host.split_once("://").map_or(host, |(_, rest)| rest);
    let authority = authority
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.contains('@') {
        return Err(crate::ScoutError::InvalidHost(
            "host 内嵌了 userinfo（`user:pass@`），请求失败时密码会随 URL 进日志 —— \
             请把凭据从 URL 里拿出来"
                .to_string(),
        ));
    }
    Ok(())
}

/// 校验过滤 / 排序表达式里的**字段名**。
///
/// 三个 HTTP 后端的过滤表达式都是「字段名 + 运算符 + 值」的文本拼接，值那边
/// 各家都转义了，字段名这边一直是裸拼：一个带空格或 `&&` 的字段名足以把表达式
/// 改写成另一条查询（Typesense 上 `where_field("x:=1 || y", ..)` 会因 `&&`
/// 优先级更高而把软删除守卫 OR 掉）。这个方法补上缺的那一半。
///
/// 允许 `.`：ES / Typesense 的嵌套字段靠它（`author.name`）。允许非 ASCII
/// 字母数字，中文列名不受影响。
pub fn validate_field_name(field: &str) -> crate::Result<()> {
    let ok = !field.is_empty()
        && field
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if !ok {
        return Err(crate::ScoutError::InvalidFieldName(field.to_string()));
    }
    Ok(())
}

/// 重定向策略：只跟随**同源**（scheme + host + port 全同）的重定向。
///
/// reqwest 换 host 时会摘掉 `Authorization`/`Cookie`，但**不会**动自定义头
/// （`redirect.rs` 只删那 5 个已知头）。Typesense 的 `X-TYPESENSE-API-KEY`、
/// Algolia 的 `X-Algolia-API-Key` 都属于自定义头，所以后端回一个
/// `302 http://evil/` 就能把 key 原样送出去。
///
/// 不直接 `Policy::none()` 是因为那会连反向代理的正常重定向（补尾斜杠、
/// 同源跳转）一起掐掉；限同源既堵了泄漏又留住正常用法。
///
/// 「同源」按 scheme + host + port 三元组全等判定，三个都不能省：
/// 只比 host 会放过 `127.0.0.1:8080` → `127.0.0.1:9090`（端口不同）；
/// 不比 scheme 会放过 `https://h:8080` → `http://h:8080` —— 那是**明文降级**，
/// 自定义认证头会以明文重发，端口相同也照样泄漏。
#[cfg(any(
    feature = "elasticsearch",
    feature = "meilisearch",
    feature = "typesense",
    feature = "algolia"
))]
pub(crate) fn same_origin_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        match attempt.previous().first() {
            Some(first) if same_origin(first, attempt.url()) => attempt.follow(),
            _ => attempt.stop(),
        }
    })
}

/// scheme + host + port 全等才算同源。独立出来是为了能脱离 HTTP 服务器直接测
/// （`Policy` 本身不可构造，靠起两个 stub server 才测得到就容易被漏掉）。
#[cfg(any(
    feature = "elasticsearch",
    feature = "meilisearch",
    feature = "typesense",
    feature = "algolia"
))]
fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    fn parts(url: &reqwest::Url) -> (&str, Option<&str>, Option<u16>) {
        (url.scheme(), url.host_str(), url.port_or_known_default())
    }
    parts(a) == parts(b)
}

/// RFC 3986 路径段百分号编码：仅保留 unreserved 字符，其余逐字节转 `%XX`
/// （含 UTF-8 多字节）。所有 HTTP 引擎的 index/id 进入 URL 前统一编码，
/// 防止 `?`/`#`/`&` 截断路径与 `%2F` 绕过 `/` 校验。
#[cfg(any(
    feature = "elasticsearch",
    feature = "meilisearch",
    feature = "typesense",
    feature = "algolia"
))]
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_index_name_accepts_valid() {
        assert!(validate_index_name("books").is_ok());
        assert!(validate_index_name("a-b_c.d~中文").is_ok());
    }

    #[test]
    fn validate_index_name_rejects_invalid() {
        for bad in [
            "", " ", "a b", "a/b", "a\\b", ".hidden", "\t", "\n", "a\"b", "a'b", "a;b", "a`b",
        ] {
            assert!(validate_index_name(bad).is_err(), "should reject {:?}", bad);
        }
    }

    #[test]
    fn config_debug_redacts_api_keys() {
        let cfg = ScoutConfig::meilisearch("http://localhost:7700", "super-secret-key");
        let rendered = format!("{cfg:?}");
        assert!(
            !rendered.contains("super-secret-key"),
            "api key leaked into Debug: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
        // 非密钥的 option 照常可读，否则 Debug 就没用了
        assert!(rendered.contains("http://localhost:7700"));
    }

    #[test]
    fn validate_host_rejects_userinfo_without_echoing_it() {
        let err = validate_host("http://elastic:hunter2@es:9200").unwrap_err();
        let text = err.to_string();
        assert!(
            !text.contains("hunter2"),
            "credential echoed back in error: {text}"
        );
        // 端口、路径、无 userinfo 的地址都要放行
        for good in [
            "http://es:9200",
            "https://es.example.com/prefix",
            "http://127.0.0.1:9200",
            "localhost:9200",
        ] {
            assert!(validate_host(good).is_ok(), "should accept {good:?}");
        }
    }

    #[cfg(any(
        feature = "elasticsearch",
        feature = "meilisearch",
        feature = "typesense",
        feature = "algolia"
    ))]
    #[test]
    fn same_origin_requires_scheme_host_and_port() {
        let u = |s: &str| reqwest::Url::parse(s).unwrap();
        // 同源：正常放行，默认端口显式写出等价
        assert!(same_origin(&u("https://a.com/x"), &u("https://a.com/y")));
        assert!(same_origin(&u("http://a.com:80/x"), &u("http://a.com/y")));
        // 跨 host
        assert!(!same_origin(&u("https://a.com/x"), &u("https://b.com/y")));
        // 同 host 不同端口：只比 host_str() 会漏掉这个
        assert!(!same_origin(&u("https://a.com/x"), &u("https://a.com:8443/y")));
        assert!(!same_origin(
            &u("http://127.0.0.1:8080/x"),
            &u("http://127.0.0.1:9090/y")
        ));
        // 明文降级：端口相同，只有 scheme 不同 —— 不比对 scheme 就会放行
        assert!(!same_origin(
            &u("https://a.com:8080/x"),
            &u("http://a.com:8080/y")
        ));
    }

    #[test]
    fn validate_field_name_rejects_expression_breaks() {
        for bad in [
            "",
            "a b",
            "x:=1 || y",
            "a&&b",
            "a,b",
            "a(b)",
            "a:b",
            "a=b",
            "field\"",
        ] {
            assert!(validate_field_name(bad).is_err(), "should reject {bad:?}");
        }
        // 嵌套字段与非 ASCII 列名是合法用法，别误伤
        for good in ["title", "author.name", "my_field", "a-b", "价格", "标题.子"] {
            assert!(validate_field_name(good).is_ok(), "should accept {good:?}");
        }
    }

    #[test]
    fn validate_index_name_rejects_wildcards_and_reserved_prefixes() {
        // `_all` 与通配符会被 ES 展开成多索引表达式：delete_index("_all")
        // 在 ES 7.x / OpenSearch 默认配置下会删掉集群里所有索引。
        for bad in [
            "_all", "_cat", "*", "a*", "a,b", "books*", "a?b", "+a", "-a", "_hidden",
        ] {
            assert!(validate_index_name(bad).is_err(), "should reject {:?}", bad);
        }
        // 非前导位置的同名字符仍然合法，别把正常名字一起禁掉
        for good in ["my_index", "a-b", "books-2024", "c#1", "a~b", "中文索引"] {
            assert!(validate_index_name(good).is_ok(), "should accept {:?}", good);
        }
    }

    #[test]
    fn invalid_index_name_message_states_the_actual_rules() {
        // 报错信息是用户唯一能看到的规则说明：`_all` 被拒时必须说得出「前导 `_`
        // 不允许」，否则只能靠猜（ES 上这个名字会删掉整个集群的索引，不是小事）。
        let msg = validate_index_name("_all").unwrap_err().to_string();
        for needle in ["must not start", "`_`", "`*`", "whitespace"] {
            assert!(
                msg.contains(needle),
                "error message should mention {needle:?}, got: {msg}"
            );
        }
        // 名字本身要回显，否则不知道是哪个索引出的问题
        assert!(msg.contains("_all"), "got: {msg}");
    }
}
