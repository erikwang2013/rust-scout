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
#[cfg(feature = "null")]
pub mod null_engine;
pub mod result;
pub mod searchable;
#[cfg(feature = "typesense")]
pub mod typesense_engine;
#[cfg(feature = "typesense")]
mod typesense_query;
#[cfg(feature = "xunsearch")]
pub mod xunsearch_engine;

#[cfg(feature = "algolia")]
pub use algolia_engine::AlgoliaEngine;
pub use builder::{SearchBuilder, TrashedFilter};
pub use collection_engine::CollectionEngine;
pub use config::{validate_index_name, ScoutConfig};
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
pub use searchable::{Searchable, SearchableStore};
