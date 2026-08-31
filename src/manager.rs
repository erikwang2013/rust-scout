use crate::collection_engine::CollectionEngine;
use crate::config::ScoutConfig;
use crate::engine::Engine;

pub struct EngineManager {
    config: ScoutConfig,
}

impl EngineManager {
    pub fn new(config: ScoutConfig) -> Self {
        Self { config }
    }

    pub fn engine(&self) -> crate::Result<Box<dyn Engine>> {
        match self.config.driver.as_str() {
            "elasticsearch" | "opensearch" => {
                #[cfg(feature = "elasticsearch")]
                {
                    let host = self
                        .config
                        .get("elasticsearch.host")
                        .or_else(|| self.config.get("opensearch.host"))
                        .and_then(|value| value.as_str())
                        .unwrap_or("http://127.0.0.1:9200")
                        .to_string();
                    let api_key = self
                        .config
                        .get("elasticsearch.api_key")
                        .or_else(|| self.config.get("opensearch.api_key"))
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    Ok(Box::new(
                        crate::elasticsearch_engine::ElasticsearchEngine::new(host, api_key),
                    ))
                }
                #[cfg(not(feature = "elasticsearch"))]
                {
                    let _ = &self.config;
                    Err(crate::ScoutError::Unsupported(
                        "elasticsearch feature is not enabled".to_string(),
                    ))
                }
            }
            _ => Ok(Box::new(CollectionEngine::new())),
        }
    }
}

impl Default for EngineManager {
    fn default() -> Self {
        Self::new(ScoutConfig::collection())
    }
}
