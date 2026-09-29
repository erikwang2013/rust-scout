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
        let cmp = |a: &&SearchDocument, b: &&SearchDocument| {
            builder.sort_cmp((Some(&a.fields), a.id.as_str()), (Some(&b.fields), b.id.as_str()))
        };
        // total 是过滤后的总数，与窗口无关
        let total = docs.len();
        let offset = builder.skip.unwrap_or(0);
        let take = builder.take.unwrap_or(total);
        // 只要窗口内的前 offset+take 条：select_nth_unstable_by 分区一次就把窗口外
        // 的元素甩到后面（无序），truncate 直接丢掉，省掉对整份命中集的排序 ——
        // 排序正是 100k 条时 400ms 里的大头。
        //
        // sort_cmp 是**全序**（并列时用 id 兜底），所以「top-K 再排序」与
        // 「全排序再截断」逐条相同，total 不受影响。
        let k = offset.saturating_add(take).min(total);
        if k < total {
            docs.select_nth_unstable_by(k, cmp);
            docs.truncate(k);
        }
        docs.sort_by(cmp);
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
            // 写入的索引名来自 doc.index（None ≡ "default"），与五个网络驱动
            // 同契约：先整批校验再落库，避免「写进去几条才报错」的半截状态。
            for doc in docs {
                crate::validate_index_name(doc.index.as_deref().unwrap_or("default"))?;
            }
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
            // 两端都要校验（与 ES/Algolia 的 reindex 同序）：`_all` 在这里 Ok
            // 而在网络上 Err 就是同一输入两个答案。
            crate::validate_index_name(from)?;
            crate::validate_index_name(to)?;
            let mut guard = self.docs.lock().expect("collection engine poisoned");
            // from 不存在时 to 得到空索引（与 create_index 语义一致）。
            let source = guard.get(from).cloned().unwrap_or_default();
            guard.insert(to.to_string(), source);
            Ok(())
        })
    }

    fn search<'a>(&'a self, builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult> {
        Box::pin(async move {
            // 校验是这个 future 里第一个可能失败的东西（与 ES `search_hits` 同序）：
            // 别让「保留名」在进程内驱动上返回 Ok(空结果) 而在网络上 Err。
            crate::validate_index_name(self.index_for(builder))?;
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
            // paginate 与 search 走同一个 builder.index，必须同行为
            crate::validate_index_name(self.index_for(builder))?;
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
#[path = "collection_engine_tests.rs"]
mod tests;
