use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScoutError {
    #[error("invalid index name `{0}`: must be non-empty, contain no whitespace, contain no '/', and not start with '.'")]
    InvalidIndexName(String),
    #[error("invalid search result: {0}")]
    InvalidResult(String),
    #[error("unsupported operation: {0}")]
    Unsupported(String),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[cfg(any(
        feature = "elasticsearch",
        feature = "meilisearch",
        feature = "typesense",
        feature = "algolia"
    ))]
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[cfg(feature = "database")]
    #[error("SQLite error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[cfg(any(
        feature = "elasticsearch",
        feature = "meilisearch",
        feature = "typesense",
        feature = "algolia",
        feature = "xunsearch"
    ))]
    #[error("backend engine request failed: {0}")]
    Backend(String),
    #[cfg(feature = "xunsearch")]
    #[error("xunsearch error: {0}")]
    XunSearch(String),
    #[cfg(feature = "xunsearch")]
    #[error("xunsearch I/O error: {0}")]
    XunSearchIo(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ScoutError>;
