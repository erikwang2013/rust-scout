use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 驱动配置。
///
/// 字段对齐 `webman-scout` 的 Scout 配置形状，其中一部分目前**只是配置占位、
/// 驱动不会读取**（见各字段说明）。真正生效的是 `driver`、`options` 和
/// 各后端构造器写入的连接参数。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScoutConfig {
    #[serde(default = "default_driver")]
    pub driver: String,
    /// 索引名前缀。**尚未生效**：只有 [`ScoutConfig::index_name`] 会读它，
    /// 而各驱动都直接使用传入的索引名、不经过该方法。设置它不会给索引加前缀。
    #[serde(default)]
    pub prefix: String,
    /// 队列开关。配置占位，当前无写入队列实现（`queue` 与
    /// `after_commit` 都属此列）。
    #[serde(default)]
    pub queue: bool,
    /// 提交后同步。配置占位，见 [`ScoutConfig::queue`]。
    #[serde(default)]
    pub after_commit: bool,
    /// 软删除开关。配置占位；软删除能力由 [`Engine::soft_delete`] 与
    /// [`SearchBuilder::with_trashed`] 控制，与此字段无关。
    ///
    /// [`Engine::soft_delete`]: crate::Engine::soft_delete
    /// [`SearchBuilder::with_trashed`]: crate::SearchBuilder::with_trashed
    #[serde(default)]
    pub soft_delete: bool,
    /// 模型 id 识别开关。配置占位，当前无效。
    #[serde(default)]
    pub identify: bool,
    /// 批量写入分块大小。配置占位；批量接口由调用方自行分块。
    #[serde(default = "default_chunk")]
    pub chunk_searchable: usize,
    /// 批量删除分块大小。配置占位，见 [`ScoutConfig::chunk_searchable`]。
    #[serde(default = "default_chunk")]
    pub chunk_unsearchable: usize,
    /// 驱动专属参数，**会被读取**：键形如 `elasticsearch.host` /
    /// `database.url` / `algolia.app_id`，由 [`EngineManager`](crate::EngineManager)
    /// 在构造驱动时取用。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub options: HashMap<String, serde_json::Value>,
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

    /// 校验索引名并拼上 [`prefix`](ScoutConfig::prefix)。
    ///
    /// 注意：各驱动**不经过**这里，它们直接用传入的索引名。本方法目前只有
    /// 调用方自己用得到，`prefix` 字段因此不会自动生效。
    pub fn index_name(&self, index: &str) -> crate::Result<String> {
        crate::validate_index_name(index)?;
        Ok(format!("{}{}", self.prefix, index))
    }
}

fn default_driver() -> String {
    "collection".to_string()
}

fn default_chunk() -> usize {
    500
}

/// 校验索引名。所有驱动在写入 / 建索引 / 删索引前都应先过这里。
///
/// 除空白、路径分隔符与前导点外，还拒绝**会被后端解释成多索引表达式或通配符**
/// 的字符：`*` `?` `,` `+`，以及前导 `-` / `_`。这条边界很要紧——
/// Elasticsearch 把 `_all` 当「全部索引」，而 `_all` 里没有需要百分号编码的字符，
/// 会原样进到 `DELETE /_all`；ES 7.x 与 OpenSearch 默认
/// `action.destructive_requires_name=false`，一次调用就能删掉整个集群的索引。
/// 前导 `_` 同时也是 ES 保留给系统索引的前缀。
pub fn validate_index_name(index: &str) -> crate::Result<()> {
    let bad = index.is_empty()
        || index.starts_with('.')
        || index.starts_with('-')
        || index.starts_with('_')
        || index
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '*' | '?' | ',' | '+'));
    if bad {
        return Err(crate::ScoutError::InvalidIndexName(index.to_string()));
    }
    Ok(())
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
        for bad in ["", " ", "a b", "a/b", "a\\b", ".hidden", "\t", "\n"] {
            assert!(validate_index_name(bad).is_err(), "should reject {:?}", bad);
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
}
