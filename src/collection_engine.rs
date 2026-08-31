use std::collections::HashMap;
use std::sync::Mutex;

use crate::engine::{Engine, EngineFuture};
use crate::{SearchBuilder, SearchDocument, SearchHit, SearchResult};

pub struct CollectionEngine {
    docs: Mutex<HashMap<String, HashMap<String, SearchDocument>>>,
}

impl Default for CollectionEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl CollectionEngine {
    pub fn new() -> Self {
        Self {
            docs: Mutex::new(HashMap::new()),
        }
    }

    fn index_for<'a>(&self, builder: &'a SearchBuilder) -> &'a str {
        builder.index.as_deref().unwrap_or("default")
    }

    fn collect_hits(&self, index: &str, builder: &SearchBuilder) -> Vec<SearchHit> {
        // 软删除过滤在 collect_hits 做；matches() 保持纯匹配语义。
        let guard = self.docs.lock().expect("collection engine poisoned");
        guard
            .get(index)
            .map(|map| {
                map.values()
                    .filter(|doc| match builder.trashed {
                        crate::TrashedFilter::Exclude => !soft_deleted(doc),
                        crate::TrashedFilter::OnlyTrashed => soft_deleted(doc),
                        crate::TrashedFilter::WithTrashed => true,
                    })
                    .filter(|doc| builder.matches(doc))
                    .map(SearchHit::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn selected(&self, index: &str, builder: &SearchBuilder) -> (Vec<SearchHit>, usize) {
        let mut hits = self.collect_hits(index, builder);
        builder.sort_hits(&mut hits);
        let total = hits.len();
        let offset = builder.skip.unwrap_or(0);
        let take = builder.take.unwrap_or(hits.len());
        let hits = hits.into_iter().skip(offset).take(take).collect();
        (hits, total)
    }
}

fn soft_deleted(doc: &SearchDocument) -> bool {
    doc.fields
        .get("__soft_deleted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

impl Engine for CollectionEngine {
    fn update<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            let mut guard = self.docs.lock().expect("collection engine poisoned");
            for doc in docs {
                let index = doc.index.clone().unwrap_or_else(|| "default".to_string());
                guard
                    .entry(index)
                    .or_default()
                    .insert(doc.id.clone(), doc.clone());
            }
            Ok(())
        })
    }

    fn delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            let mut guard = self.docs.lock().expect("collection engine poisoned");
            for ids_set in guard.values_mut() {
                for id in ids {
                    ids_set.remove(id);
                }
            }
            Ok(())
        })
    }

    fn delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            let mut guard = self.docs.lock().expect("collection engine poisoned");
            if let Some(ids_set) = guard.get_mut(index) {
                for id in ids {
                    ids_set.remove(id);
                }
            }
            Ok(())
        })
    }

    fn soft_delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            // 与 delete() 一致的跨索引语义：标记所有索引中匹配 id 的文档。
            let mut guard = self.docs.lock().expect("collection engine poisoned");
            for ids_set in guard.values_mut() {
                for id in ids {
                    if let Some(doc) = ids_set.get_mut(id) {
                        doc.set("__soft_deleted", true);
                    }
                }
            }
            Ok(())
        })
    }

    fn reindex<'a>(&'a self, from: &'a str, to: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            let mut guard = self.docs.lock().expect("collection engine poisoned");
            // from 不存在时 to 得到空索引（与 create_index 语义一致）。
            let source = guard.get(from).cloned().unwrap_or_default();
            guard.insert(to.to_string(), source);
            Ok(())
        })
    }

    fn search<'a>(&'a self, builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult> {
        Box::pin(async move {
            let (hits, total) = self.selected(self.index_for(builder), builder);
            Ok(SearchResult {
                hits,
                total,
                ..SearchResult::default()
            })
        })
    }

    fn paginate<'a>(
        &'a self,
        builder: &'a SearchBuilder,
        page: usize,
        per_page: usize,
    ) -> EngineFuture<'a, SearchResult> {
        let page = page.max(1);
        let per_page = per_page.max(1);
        Box::pin(async move {
            let mut base = builder.clone();
            base.skip = Some((page - 1) * per_page);
            base.take = Some(per_page);
            let (hits, total) = self.selected(self.index_for(builder), &base);
            Ok(SearchResult {
                hits,
                total,
                ..SearchResult::default()
            })
        })
    }

    fn map_ids(&self, result: &SearchResult) -> Vec<String> {
        result.ids()
    }

    fn flush<'a>(&'a self, _index: &'a str) -> EngineFuture<'a, ()> {
        // no-op: mirrors ES _refresh; delete_index is the explicit removal path
        Box::pin(async move { Ok(()) })
    }

    fn create_index<'a>(
        &'a self,
        index: &'a str,
        _settings: serde_json::Value,
    ) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            self.docs
                .lock()
                .expect("collection engine poisoned")
                .entry(index.to_string())
                .or_default();
            Ok(())
        })
    }

    fn delete_index<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            self.docs
                .lock()
                .expect("collection engine poisoned")
                .remove(index);
            Ok(())
        })
    }
}

impl From<&SearchDocument> for SearchHit {
    fn from(doc: &SearchDocument) -> Self {
        Self {
            id: doc.id.clone(),
            score: None,
            source: serde_json::to_value(&doc.fields).unwrap_or_default(),
            highlight: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str, index: Option<&str>) -> SearchDocument {
        let mut d = SearchDocument::new(id, serde_json::json!({"title": id})).unwrap();
        d.index = index.map(str::to_string);
        d
    }

    #[tokio::test]
    async fn update_respects_doc_index() {
        let engine = CollectionEngine::new();
        engine
            .update(&[doc("one", Some("books")), doc("two", None)])
            .await
            .unwrap();
        let result = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        let ids: Vec<&str> = result.hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["one"]);
    }

    #[tokio::test]
    async fn flush_keeps_data() {
        let engine = CollectionEngine::new();
        engine.update(&[doc("one", Some("books"))]).await.unwrap();
        engine.flush("books").await.unwrap();
        let result = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        assert_eq!(result.total, 1);
    }

    #[tokio::test]
    async fn delete_in_only_removes_from_given_index() {
        let engine = CollectionEngine::new();
        engine
            .update(&[doc("one", Some("books")), doc("one", Some("movies"))])
            .await
            .unwrap();
        engine.delete_in("books", &["one".to_string()]).await.unwrap();
        let books = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        let movies = engine
            .search(&SearchBuilder::new("").within("movies"))
            .await
            .unwrap();
        assert_eq!(books.total, 0);
        assert_eq!(movies.total, 1);
    }

    #[tokio::test]
    async fn reindex_copies_docs_keeps_source() {
        let engine = CollectionEngine::new();
        engine.update(&[doc("one", Some("books"))]).await.unwrap();
        engine.reindex("books", "archive").await.unwrap();
        let source = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        let copy = engine
            .search(&SearchBuilder::new("").within("archive"))
            .await
            .unwrap();
        assert_eq!(source.total, 1);
        assert_eq!(copy.total, 1);
    }

    #[tokio::test]
    async fn reindex_missing_source_creates_empty_target() {
        let engine = CollectionEngine::new();
        engine.reindex("nope", "target").await.unwrap();
        let result = engine
            .search(&SearchBuilder::new("").within("target"))
            .await
            .unwrap();
        assert_eq!(result.total, 0);
    }

    #[tokio::test]
    async fn soft_delete_filters_per_trashed() {
        let engine = CollectionEngine::new();
        engine
            .update(&[doc("one", Some("books")), doc("two", Some("books"))])
            .await
            .unwrap();
        engine.soft_delete(&["one".to_string()]).await.unwrap();

        let excluded = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        assert_eq!(excluded.total, 1);
        assert_eq!(excluded.hits[0].id, "two");

        let only = engine
            .search(&SearchBuilder::new("").within("books").only_trashed())
            .await
            .unwrap();
        assert_eq!(only.total, 1);
        assert_eq!(only.hits[0].id, "one");

        let all = engine
            .search(&SearchBuilder::new("").within("books").with_trashed())
            .await
            .unwrap();
        assert_eq!(all.total, 2);
    }

    #[tokio::test]
    async fn soft_delete_filter_ignores_non_true_markers() {
        // 只有 `__soft_deleted == true` 才算软删除：false/字符串/缺失值在
        // Exclude 下可见、OnlyTrashed 下不可见（与 ES term 语义对齐）。
        let engine = CollectionEngine::new();
        let mut marked_false = doc("false", Some("books"));
        marked_false.set("__soft_deleted", false);
        let mut marked_str = doc("str", Some("books"));
        marked_str.set("__soft_deleted", "x");
        let mut marked_true = doc("gone", Some("books"));
        marked_true.set("__soft_deleted", true);
        engine.update(&[marked_false, marked_str, marked_true]).await.unwrap();

        let excluded = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        let ids: Vec<&str> = excluded.hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["false", "str"]);

        let only = engine
            .search(&SearchBuilder::new("").within("books").only_trashed())
            .await
            .unwrap();
        let ids: Vec<&str> = only.hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["gone"]);
    }
}
