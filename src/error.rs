use thiserror::Error;

/// 各变体带 `cfg` 门控。**改动任何一条 `cfg` 时必须同步 `pet.rs::hint()` 的
/// 匹配臂**——两处 cfg 不一致会让 `hint()` 的 match 在某个 feature 组合下不穷尽，
/// 例如 `--features xunsearch`（`Backend` 存在但那个臂被门控掉）会直接编译失败。
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

impl ScoutError {
    /// 项目宠物「嗅探猎犬 Scout」针对这个错误的排查提示。
    ///
    /// 每个错误变体对应一句人话方向，例如索引名非法时会提示命名规则、
    /// `Unsupported` 时会提示检查 feature。渲染成完整文本用
    /// [`crate::pet::format_error`]。
    pub fn pet_hint(&self) -> &'static str {
        crate::pet::hint(self)
    }
}

pub type Result<T> = std::result::Result<T, ScoutError>;
