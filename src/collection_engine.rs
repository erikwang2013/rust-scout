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

    fn selected(&self, index: &str, builder: &SearchBuilder) -> (Vec<SearchHit>, usize) {
        // 软删除过滤在这里做；matches() 保持纯匹配语义。
        //
        // 先对文档**引用**排序、取完窗口再物化：命中上万条时，先给每条命中深拷贝
        // 一份 fields 只为扔掉落选的那些是纯浪费。引用借自 guard，所以整段必须在
        // 锁作用域内完成 —— 出不了这个函数的边界。
        let guard = self.docs.lock().expect("collection engine poisoned");
        let Some(map) = guard.get(index) else {
            return (Vec::new(), 0);
        };
        let mut docs: Vec<&SearchDocument> = map
            .values()
            .filter(|doc| match builder.trashed {
                crate::TrashedFilter::Exclude => !soft_deleted(doc),
                crate::TrashedFilter::OnlyTrashed => soft_deleted(doc),
                crate::TrashedFilter::WithTrashed => true,
            })
            .filter(|doc| builder.matches(doc))
            .collect();
        docs.sort_by(|a, b| {
            builder.sort_cmp((Some(&a.fields), a.id.as_str()), (Some(&b.fields), b.id.as_str()))
        });
        // total 是过滤后的总数，与窗口无关
        let total = docs.len();
        let offset = builder.skip.unwrap_or(0);
        let take = builder.take.unwrap_or(total);
        let hits = docs
            .into_iter()
            .skip(offset)
            .take(take)
            .map(SearchHit::from)
            .collect();
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
            // 与其余驱动、与本文件的 soft_delete_in 同序：保留名照样拒绝，别让
            // 「进程内驱动没有多索引展开风险」变成两套契约（delete_in("_all") 必须
            // 在八个驱动上给同一个答案）。
            crate::validate_index_name(index)?;
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

    fn soft_delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let mut guard = self.docs.lock().expect("collection engine poisoned");
            if let Some(ids_set) = guard.get_mut(index) {
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
            base.skip = Some((page - 1).saturating_mul(per_page));
            base.take = Some(per_page);
            let (hits, total) = self.selected(self.index_for(builder), &base);
            Ok(SearchResult {
                hits,
                total,
                ..SearchResult::default()
            })
        })
    }

    fn create_index<'a>(
        &'a self,
        index: &'a str,
        _settings: serde_json::Value,
    ) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            // 与 delete_index 同一道校验：ES 的 create_index 就拒保留名，
            // 内存驱动不该在这里破例，否则 `_all` 在哪能建、哪不能建就说不清了。
            crate::validate_index_name(index)?;
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
            // delete_in / soft_delete_in 既然拒保留名，同一职责的 delete_index 不能例外
            // （DatabaseEngine::delete_index 也走同一道校验）。
            crate::validate_index_name(index)?;
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
    async fn windowing_matches_full_sort_then_truncate() {
        // 「先排引用、再物化窗口」这个优化不能改变结果的顺序或 total：
        // take(k) 必须等于「全排序后取前 k 条」，skip 同理，total 与窗口无关。
        let engine = CollectionEngine::new();
        let docs: Vec<SearchDocument> = (0..40)
            .map(|i| {
                let mut d = doc(&format!("d{i:02}"), Some("books"));
                // 故意造重复值：排序的并列项要靠 id 兜底，最容易在这里错
                d.fields
                    .insert("rank".into(), serde_json::json!((i * 7) % 10));
                d
            })
            .collect();
        engine.update(&docs).await.unwrap();

        for desc in [false, true] {
            let all = engine
                .search(
                    &SearchBuilder::new("")
                        .within("books")
                        .order_by("rank", desc),
                )
                .await
                .unwrap();
            assert_eq!(all.total, 40);
            assert_eq!(all.hits.len(), 40);

            for (offset, take) in [(0, 3), (0, 10), (5, 7), (37, 10), (39, 5)] {
                let page = engine
                    .search(
                        &SearchBuilder::new("")
                            .within("books")
                            .order_by("rank", desc)
                            .skip(offset)
                            .take(take),
                    )
                    .await
                    .unwrap();
                let expected: Vec<&str> = all
                    .hits
                    .iter()
                    .skip(offset)
                    .take(take)
                    .map(|h| h.id.as_str())
                    .collect();
                let got: Vec<&str> = page.hits.iter().map(|h| h.id.as_str()).collect();
                assert_eq!(
                    got, expected,
                    "desc={desc} skip={offset} take={take}: 窗口与全排序不一致"
                );
                // total 始终是过滤后的总数，不随分页变小
                assert_eq!(page.total, 40, "total 不该被 take 截断");
            }
        }
    }

    #[tokio::test]
    async fn delete_paths_reject_reserved_index_name() {
        // 契约统一：保留名（`_all` 等）在进程内驱动上也要报 InvalidIndexName，
        // 不能静默 Ok（ES 上 delete_index("_all") 会删掉整个集群）。
        let engine = CollectionEngine::new();
        engine.update(&[doc("one", Some("books"))]).await.unwrap();
        let ids = vec!["one".to_string()];

        for err in [
            engine.delete_in("_all", &ids).await.unwrap_err(),
            engine.soft_delete_in("_all", &ids).await.unwrap_err(),
            engine.delete_index("_all").await.unwrap_err(),
            engine
                .create_index("_all", serde_json::json!({}))
                .await
                .unwrap_err(),
        ] {
            assert!(
                matches!(err, crate::ScoutError::InvalidIndexName(_)),
                "expected InvalidIndexName, got {err:?}"
            );
        }

        // 被拒 = 什么都没发生：文档仍在，且没被顺手软删
        let result = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        assert_eq!(result.total, 1);
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
