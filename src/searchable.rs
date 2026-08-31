use crate::{Result, SearchDocument, SearchResult};

pub trait Searchable {
    fn searchable_id(&self) -> String;
    fn to_searchable_json(&self) -> serde_json::Value;
}

pub trait SearchableStore: Send + Sync {
    fn index_documents(&self, docs: &[SearchDocument]) -> Result<()>;
    fn remove_documents(&self, ids: &[String]) -> Result<()>;
    fn search(&self, query: &str) -> Result<SearchResult>;
}
