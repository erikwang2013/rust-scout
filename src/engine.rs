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
    /// 从查询结果里取出命中 id。默认即 [`SearchResult::ids`]，八个驱动行为
    /// 一致；只有需要改写 id 形态（前缀、规范化）的驱动才覆写。
    fn map_ids(&self, result: &SearchResult) -> Vec<String> {
        result.ids()
    }
    /// 刷新索引，让已写入的文档对搜索可见。
    ///
    /// **不是清空索引** —— 要删除索引用 [`Self::delete_index`]。默认实现是
    /// no-op（校验索引名后直接返回）：除 ES 外所有后端写入即对查询可见，
    /// 没有「刷新」这一步。只有 ES 需要覆写成 `_refresh`。
    ///
    /// 曾有三个驱动把它接到了清空接口上（Meilisearch `delete-all`、
    /// Typesense 删集合、Algolia `/clear`），而 README 的生命周期示例在
    /// update 与 search 之间调用 flush —— 照文档跑一遍就会把索引删空。
    fn flush<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            Ok(())
        })
    }
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
    ///
    /// **不带索引信息，语义因引擎而异**：`CollectionEngine` / `DatabaseEngine`
    /// 跨所有索引标记匹配 id 的文档；HTTP 后端做不到跨索引，会返回
    /// [`ScoutError::Unsupported`]（而不是静默什么都不做）——这些后端请用
    /// [`Self::soft_delete_in`]。
    fn soft_delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        let _ = ids;
        Box::pin(async move {
            Err(crate::ScoutError::Unsupported(
                "soft_delete (index-less) is not supported by this engine; \
                 use soft_delete_in(index, ids) instead"
                    .to_string(),
            ))
        })
    }
    /// 仅对指定索引做软删除；语义与 [`Self::delete_in`] 一致。
    fn soft_delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        let _ = (index, ids);
        Box::pin(async move {
            Err(crate::ScoutError::Unsupported(
                "soft_delete_in not implemented by this engine".to_string(),
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
