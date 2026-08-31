//! XunSearch 引擎占位实现（stub）。
//!
//! XunSearch 使用自有的二进制 TCP 协议与服务端（默认 8383 端口）通信，Rust 侧
//! 没有可用的官方/社区客户端；其 PHP SDK 为 GPL 许可不可借鉴，协议逆向成本约
//! 600-1000 行。本轮只提供占位 stub，全部方法返回 Unsupported。
//!
//! 后续可选路径（跟踪：docs/superpowers/specs/2026-08-31-rust-scout-design.md）：
//! 1. 原生协议客户端：直接实现 index 端口二进制协议（工作量大，性能最好）；
//! 2. PHP CLI 桥接：通过 xunsearch PHP SDK 脚本做代理（工作量小，需 PHP 运行时）；
//! 3. 替代方案：tantivy + jieba-rs 本地全文索引，彻底绕开 XunSearch。

use crate::engine::{Engine, EngineFuture};
use crate::{SearchBuilder, SearchDocument, SearchResult};

// stub：host/project 留给后续协议客户端实现使用。
#[allow(dead_code)]
pub struct XunSearchEngine {
    host: String,
    _project: String,
}

impl XunSearchEngine {
    pub fn new(host: &str, project: &str) -> Self {
        Self {
            host: host.to_string(),
            _project: project.to_string(),
        }
    }
}

fn unsupported() -> crate::ScoutError {
    crate::ScoutError::Unsupported(
        "XunSearch native protocol client not yet implemented; tracking: see docs/superpowers/specs"
            .to_string(),
    )
}

impl Engine for XunSearchEngine {
    fn update<'a>(&'a self, _docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(async move { Err(unsupported()) })
    }

    fn delete<'a>(&'a self, _ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move { Err(unsupported()) })
    }

    fn delete_in<'a>(&'a self, _index: &'a str, _ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move { Err(unsupported()) })
    }

    fn search<'a>(&'a self, _builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult> {
        Box::pin(async move { Err(unsupported()) })
    }

    fn paginate<'a>(
        &'a self,
        _builder: &'a SearchBuilder,
        _page: usize,
        _per_page: usize,
    ) -> EngineFuture<'a, SearchResult> {
        Box::pin(async move { Err(unsupported()) })
    }

    fn map_ids(&self, result: &SearchResult) -> Vec<String> {
        result.ids()
    }

    fn flush<'a>(&'a self, _index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move { Err(unsupported()) })
    }

    fn create_index<'a>(
        &'a self,
        _index: &'a str,
        _settings: serde_json::Value,
    ) -> EngineFuture<'a, ()> {
        Box::pin(async move { Err(unsupported()) })
    }

    fn delete_index<'a>(&'a self, _index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move { Err(unsupported()) })
    }

    fn update_bulk<'a>(&'a self, _docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(async move { Err(unsupported()) })
    }

    fn delete_bulk<'a>(&'a self, _index: &'a str, _ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move { Err(unsupported()) })
    }

    fn soft_delete<'a>(&'a self, _ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move { Err(unsupported()) })
    }

    fn reindex<'a>(&'a self, _from: &'a str, _to: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move { Err(unsupported()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn constructs_and_returns_unsupported() {
        let engine = XunSearchEngine::new("127.0.0.1:8383", "books");
        let err = engine
            .update(&[SearchDocument::new(
                "one",
                serde_json::json!({"title": "x"}),
            )
            .unwrap()])
            .await
            .unwrap_err();
        assert!(matches!(err, crate::ScoutError::Unsupported(_)));
    }
}
