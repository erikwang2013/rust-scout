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
        let guard = self.docs.lock().expect("collection engine poisoned");
        guard
            .get(index)
            .map(|map| {
                map.values()
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
}
