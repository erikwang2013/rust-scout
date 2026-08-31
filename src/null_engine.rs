#![cfg(feature = "null")]

use serde_json::Value;

use crate::engine::{Engine, EngineFuture};
use crate::{SearchBuilder, SearchDocument, SearchResult};

/// 空引擎：所有写操作直接成功，搜索返回空结果。用于测试或临时禁用搜索。
pub struct NullEngine;

impl NullEngine {
    pub fn new() -> Self {
        Self
    }
}

impl Engine for NullEngine {
    fn update<'a>(&'a self, _docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(async move { Ok(()) })
    }

    fn delete<'a>(&'a self, _ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move { Ok(()) })
    }

    fn delete_in<'a>(&'a self, _index: &'a str, _ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move { Ok(()) })
    }

    fn search<'a>(&'a self, _builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult> {
        Box::pin(async move { Ok(SearchResult::default()) })
    }

    fn paginate<'a>(
        &'a self,
        _builder: &'a SearchBuilder,
        _page: usize,
        _per_page: usize,
    ) -> EngineFuture<'a, SearchResult> {
        Box::pin(async move { Ok(SearchResult::default()) })
    }

    fn map_ids(&self, _result: &SearchResult) -> Vec<String> {
        Vec::new()
    }

    fn flush<'a>(&'a self, _index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move { Ok(()) })
    }

    fn create_index<'a>(
        &'a self,
        _index: &'a str,
        _settings: Value,
    ) -> EngineFuture<'a, ()> {
        Box::pin(async move { Ok(()) })
    }

    fn delete_index<'a>(&'a self, _index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move { Ok(()) })
    }

    fn update_bulk<'a>(&'a self, _docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(async move { Ok(()) })
    }

    fn delete_bulk<'a>(&'a self, _index: &'a str, _ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move { Ok(()) })
    }

    fn soft_delete<'a>(&'a self, _ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move { Ok(()) })
    }

    fn reindex<'a>(&'a self, _from: &'a str, _to: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_returns_empty() {
        let engine = NullEngine::new();
        let result = engine.search(&SearchBuilder::new("anything")).await.unwrap();
        assert_eq!(result.total, 0);
        assert!(result.hits.is_empty());
        assert!(engine.map_ids(&result).is_empty());

        let paginated = engine
            .paginate(&SearchBuilder::new("x"), 1, 10)
            .await
            .unwrap();
        assert!(paginated.hits.is_empty());

        engine
            .update(&[SearchDocument::new("1", serde_json::json!({"a": 1})).unwrap()])
            .await
            .unwrap();
    }
}
