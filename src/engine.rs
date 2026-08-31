use std::future::Future;
use std::pin::Pin;

use crate::{Result, SearchBuilder, SearchDocument, SearchResult};

pub type EngineFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub trait Engine: Send + Sync {
    fn update<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()>;
    fn delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()>;
    fn search<'a>(&'a self, builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult>;
    fn paginate<'a>(
        &'a self,
        builder: &'a SearchBuilder,
        page: usize,
        per_page: usize,
    ) -> EngineFuture<'a, SearchResult>;
    fn map_ids(&self, result: &SearchResult) -> Vec<String>;
    fn flush<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()>;
    fn create_index<'a>(
        &'a self,
        index: &'a str,
        settings: serde_json::Value,
    ) -> EngineFuture<'a, ()>;
    fn delete_index<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()>;
}
