//! # rust-scout
//!
//! Scout 风格的全文搜索抽象：一份业务代码，换一个 feature 就换一个后端。
//! 八个驱动共用 [`Engine`] 一个契约，索引名、`total`、分页、软删除的语义在各
//! 驱动上保持一致；某个后端做不到的部分会**显式报错**，而不是静默给出错误结果。
//!
//! 默认驱动是进程内的 [`CollectionEngine`]，零额外依赖：
//!
//! ```
//! use rust_scout::{CollectionEngine, Engine, SearchBuilder, SearchDocument};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> rust_scout::Result<()> {
//! let engine = CollectionEngine::new();
//!
//! engine
//!     .update(&[SearchDocument::new(
//!         "a1",
//!         serde_json::json!({"title": "Rust 异步", "status": "published"}),
//!     )?])
//!     .await?;
//!
//! let result = engine
//!     .search(&SearchBuilder::new("rust").where_field("status", "published"))
//!     .await?;
//!
//! assert_eq!(result.total, 1);
//! assert_eq!(result.hits[0].id, "a1");
//! # Ok(())
//! # }
//! ```
//!
//! 换成真实后端只需改 Cargo feature 与 [`ScoutConfig`]，业务代码不动 ——
//! 各驱动做不到或语义不同的地方见 README 的「驱动能力差异」表。
//!
//! ## 边界校验
//!
//! 索引名、字段名、host 分别由 [`validate_index_name`]、[`validate_field_name`]、
//! [`validate_host`] 把关，所有驱动在触及后端之前都会先过这三道。自己写驱动时
//! 也请调用它们，并复用 `config::percent_encode` 与
//! `config::same_origin_redirect_policy` —— 漏掉后者正是自定义认证头
//! 会跟随跨域重定向外泄的原因。
//!
//! （后两个只在启用任一 HTTP 驱动 feature 时存在，所以这里写成普通代码字体而不是
//! 文档链接：链接在不开 HTTP feature 的组合下会解析失败。）

#[cfg(feature = "algolia")]
pub mod algolia_engine;
pub mod builder;
pub mod collection_engine;
pub mod config;
#[cfg(feature = "database")]
pub mod database_engine;
pub mod document;
#[cfg(feature = "elasticsearch")]
pub mod elasticsearch_engine;
#[cfg(feature = "elasticsearch")]
mod query;
pub mod engine;
pub mod error;
pub mod manager;
#[cfg(feature = "meilisearch")]
pub mod meilisearch_engine;
#[cfg(feature = "meilisearch")]
mod meilisearch_query;
#[cfg(feature = "null")]
pub mod null_engine;
pub mod pet;
pub mod result;
#[cfg(feature = "typesense")]
pub mod typesense_engine;
#[cfg(feature = "typesense")]
mod typesense_query;
#[cfg(feature = "xunsearch")]
pub mod xunsearch_engine;
#[cfg(feature = "xunsearch")]
mod xunsearch_query;
#[cfg(all(test, feature = "xunsearch"))]
mod xunsearch_mock;
#[cfg(all(test, feature = "xunsearch"))]
mod xunsearch_tests;

#[cfg(feature = "algolia")]
pub use algolia_engine::AlgoliaEngine;
#[cfg(test)]
mod conformance_harness;
#[cfg(test)]
mod conformance_tests;
pub use builder::{SearchBuilder, TrashedFilter};
pub use collection_engine::CollectionEngine;
pub use config::{
    validate_field_name, validate_host, validate_index_name, ScoutConfig,
};
#[cfg(feature = "database")]
pub use database_engine::DatabaseEngine;
pub use document::SearchDocument;
#[cfg(feature = "elasticsearch")]
pub use elasticsearch_engine::ElasticsearchEngine;
pub use engine::Engine;
#[cfg(feature = "meilisearch")]
pub use meilisearch_engine::MeilisearchEngine;
#[cfg(feature = "null")]
pub use null_engine::NullEngine;
#[cfg(feature = "typesense")]
pub use typesense_engine::TypesenseEngine;
#[cfg(feature = "xunsearch")]
pub use xunsearch_engine::XunSearchEngine;
pub use error::{Result, ScoutError};
pub use manager::EngineManager;
pub use result::{SearchHit, SearchResult};
