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
