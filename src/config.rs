use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScoutConfig {
    #[serde(default = "default_driver")]
    pub driver: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub queue: bool,
    #[serde(default)]
    pub after_commit: bool,
    #[serde(default)]
    pub soft_delete: bool,
    #[serde(default)]
    pub identify: bool,
    #[serde(default = "default_chunk")]
    pub chunk_searchable: usize,
    #[serde(default = "default_chunk")]
    pub chunk_unsearchable: usize,
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

pub fn validate_index_name(index: &str) -> crate::Result<()> {
    if index
        .chars()
        .any(|c| c.is_whitespace() || c == '/' || c == '\\')
        || index.starts_with('.')
        || index.is_empty()
    {
        return Err(crate::ScoutError::InvalidIndexName(index.to_string()));
    }
    Ok(())
}

/// RFC 3986 路径段百分号编码：仅保留 unreserved 字符，其余逐字节转 `%XX`
/// （含 UTF-8 多字节）。所有引擎的 index/id 进入 URL 前统一编码，
/// 防止 `?`/`#`/`&` 截断路径与 `%2F` 绕过 `/` 校验。
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
}
