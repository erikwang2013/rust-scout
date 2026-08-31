pub mod builder;
pub mod collection_engine;
pub mod config;
pub mod document;
#[cfg(feature = "elasticsearch")]
pub mod elasticsearch_engine;
pub mod engine;
pub mod error;
pub mod manager;
pub mod result;
pub mod searchable;

pub use builder::SearchBuilder;
pub use collection_engine::CollectionEngine;
pub use config::{validate_index_name, ScoutConfig};
pub use document::SearchDocument;
#[cfg(feature = "elasticsearch")]
pub use elasticsearch_engine::ElasticsearchEngine;
pub use engine::Engine;
pub use error::{Result, ScoutError};
pub use manager::EngineManager;
pub use result::{SearchHit, SearchResult};
pub use searchable::{Searchable, SearchableStore};
