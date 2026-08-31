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
        // 锁在 build 期间保持（等价于 entry().or_insert_with 的原子性）：
        // 并发冷启动也不会构造出两个实例。
        let mut cache = self.engines.lock().expect("engine manager poisoned");
        if let Some(engine) = cache.get(&driver) {
            return Ok(engine.clone());
        }
        let engine = build_engine(&self.config)?;
        cache.insert(driver, engine.clone());
        Ok(engine)
    }
}

fn build_engine(config: &ScoutConfig) -> crate::Result<Arc<dyn Engine>> {
    match config.driver.as_str() {
        #[cfg(feature = "elasticsearch")]
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
            Ok(Arc::new(crate::elasticsearch_engine::ElasticsearchEngine::new(
                host, api_key,
            )))
        }
        #[cfg(feature = "database")]
        "database" => {
            let url = config
                .get("database.url")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    crate::ScoutError::Unsupported(
                        "database driver requires `database.url` config".to_string(),
                    )
                })?;
            let fields = config
                .get("database.fields")
                .and_then(|value| value.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Ok(Arc::new(crate::database_engine::DatabaseEngine::new(
                url, fields,
            )?))
        }
        #[cfg(feature = "meilisearch")]
        "meilisearch" => {
            let host = config
                .get("meilisearch.host")
                .and_then(|value| value.as_str())
                .unwrap_or("http://127.0.0.1:7700")
                .to_string();
            let api_key = config
                .get("meilisearch.api_key")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            Ok(Arc::new(crate::meilisearch_engine::MeilisearchEngine::new(
                host, api_key,
            )))
        }
        #[cfg(feature = "typesense")]
        "typesense" => {
            let host = config
                .get("typesense.host")
                .and_then(|value| value.as_str())
                .unwrap_or("http://127.0.0.1:8108")
                .to_string();
            let api_key = config
                .get("typesense.api_key")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            Ok(Arc::new(crate::typesense_engine::TypesenseEngine::new(
                host, api_key,
            )))
        }
        #[cfg(feature = "algolia")]
        "algolia" => {
            let app_id = config
                .get("algolia.app_id")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    crate::ScoutError::Unsupported(
                        "algolia driver requires `algolia.app_id` config".to_string(),
                    )
                })?;
            let api_key = config
                .get("algolia.api_key")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    crate::ScoutError::Unsupported(
                        "algolia driver requires `algolia.api_key` config".to_string(),
                    )
                })?;
            Ok(Arc::new(crate::algolia_engine::AlgoliaEngine::new(
                app_id.to_string(),
                api_key.to_string(),
            )))
        }
        #[cfg(feature = "null")]
        "null" => Ok(Arc::new(crate::null_engine::NullEngine::new())),
        #[cfg(feature = "xunsearch")]
        "xunsearch" => {
            let host = config
                .get("xunsearch.host")
                .and_then(|value| value.as_str())
                .unwrap_or("127.0.0.1:8383")
                .to_string();
            let project = config
                .get("xunsearch.project")
                .and_then(|value| value.as_str())
                .unwrap_or("default")
                .to_string();
            Ok(Arc::new(crate::xunsearch_engine::XunSearchEngine::new(
                &host, &project, None,
            )))
        }
        other => {
            if feature_missing(other) {
                return Err(crate::ScoutError::Unsupported(format!(
                    "engine driver `{other}` requires its feature to be enabled"
                )));
            }
            Ok(Arc::new(CollectionEngine::new()))
        }
    }
}

/// 已知但未启用对应 feature 的 driver 列表；启用后由上面的 cfg 分支接住，
/// 到不了这里。新增 feature 门控引擎时在 match 加分支、在匹配列表加名字。
fn feature_missing(driver: &str) -> bool {
    matches!(
        driver,
        "elasticsearch"
            | "opensearch"
            | "meilisearch"
            | "typesense"
            | "algolia"
            | "database"
            | "null"
            | "xunsearch"
    )
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
