use std::future::Future;
use std::pin::Pin;

use crate::{Result, SearchBuilder, SearchDocument, SearchResult};

pub type EngineFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub trait Engine: Send + Sync {
    fn update<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()>;
    /// 无索引信息的删除；CollectionEngine 跨索引删除，ElasticsearchEngine 仅作用于
    /// default 索引——需要精确语义请用 [`Self::delete_in`]。
    fn delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()>;
    /// 仅从指定索引删除文档。
    fn delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        let _ = (index, ids);
        Box::pin(async move {
            Err(crate::ScoutError::Unsupported(
                "delete_in not implemented by this engine".to_string(),
            ))
        })
    }
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
    /// 批量写入；默认实现逐条调用 [`Self::update`]。
    fn update_bulk<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            for doc in docs {
                self.update(std::slice::from_ref(doc)).await?;
            }
            Ok(())
        })
    }
    /// 批量删除；默认实现逐条调用 [`Self::delete_in`]。
    fn delete_bulk<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            for id in ids {
                self.delete_in(index, std::slice::from_ref(id)).await?;
            }
            Ok(())
        })
    }
    /// 软删除：给文档打上 `__soft_deleted: true` 标记，配合
    /// `SearchBuilder::with_trashed()` / `only_trashed()` 过滤。
    fn soft_delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        let _ = ids;
        Box::pin(async move {
            Err(crate::ScoutError::Unsupported(
                "soft_delete not implemented by this engine".to_string(),
            ))
        })
    }
    /// 重建索引：把 from 索引的内容复制到 to 索引。语义因引擎而异：
    /// CollectionEngine 直接替换 to 索引的既有内容；ElasticsearchEngine 委托
    /// 后端 `_reindex`（合并进 to，发生冲突时整批中止）。
    fn reindex<'a>(&'a self, from: &'a str, to: &'a str) -> EngineFuture<'a, ()> {
        let _ = (from, to);
        Box::pin(async move {
            Err(crate::ScoutError::Unsupported(
                "reindex not implemented by this engine".to_string(),
            ))
        })
    }
}
