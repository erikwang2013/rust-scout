use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::collection_engine::CollectionEngine;
use crate::config::ScoutConfig;
use crate::engine::Engine;

pub struct EngineManager {
    config: ScoutConfig,
    // 按 driver 缓存引擎实例，首次创建后复用：update 之后再次 engine()
    // 拿到的是同一实例，数据不丢失。
    engines: Mutex<HashMap<String, Arc<dyn Engine>>>,
}

impl EngineManager {
    pub fn new(config: ScoutConfig) -> Self {
        Self {
            config,
            engines: Mutex::new(HashMap::new()),
        }
    }

    pub fn engine(&self) -> crate::Result<Arc<dyn Engine>> {
        let driver = self.config.driver.clone();
        #[cfg(not(feature = "elasticsearch"))]
        if matches!(driver.as_str(), "elasticsearch" | "opensearch") {
            return Err(crate::ScoutError::Unsupported(
                "elasticsearch feature is not enabled".to_string(),
            ));
        }
        // entry().or_insert_with 一次性完成查+构造+插入，
        // 消除并发冷启动下 check-then-insert 的双实例竞态。
        let mut cache = self.engines.lock().expect("engine manager poisoned");
        Ok(cache
            .entry(driver)
            .or_insert_with(|| build_engine(&self.config))
            .clone())
    }
}

#[cfg(feature = "elasticsearch")]
fn build_engine(config: &ScoutConfig) -> Arc<dyn Engine> {
    match config.driver.as_str() {
        "elasticsearch" | "opensearch" => {
            let host = config
                .get("elasticsearch.host")
                .or_else(|| config.get("opensearch.host"))
                .and_then(|value| value.as_str())
                .unwrap_or("http://127.0.0.1:9200")
                .to_string();
            let api_key = config
                .get("elasticsearch.api_key")
                .or_else(|| config.get("opensearch.api_key"))
                .and_then(|value| value.as_str())
                .map(str::to_string);
            Arc::new(crate::elasticsearch_engine::ElasticsearchEngine::new(host, api_key))
        }
        _ => Arc::new(CollectionEngine::new()),
    }
}

#[cfg(not(feature = "elasticsearch"))]
fn build_engine(_config: &ScoutConfig) -> Arc<dyn Engine> {
    Arc::new(CollectionEngine::new())
}

impl Default for EngineManager {
    fn default() -> Self {
        Self::new(ScoutConfig::collection())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SearchBuilder;

    #[test]
    fn engine_returns_cached_instance() {
        let manager = EngineManager::new(ScoutConfig::collection());
        let a = manager.engine().unwrap();
        let b = manager.engine().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn cached_engine_keeps_updates() {
        let manager = EngineManager::new(ScoutConfig::collection());
        let doc =
            crate::SearchDocument::new("one", serde_json::json!({"title": "hello"})).unwrap();
        manager.engine().unwrap().update(&[doc]).await.unwrap();
        // 再次 engine() 必须命中同一实例，update 的数据不丢。
        let result = manager
            .engine()
            .unwrap()
            .search(&SearchBuilder::new("hello"))
            .await
            .unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.hits[0].id, "one");
    }
}
