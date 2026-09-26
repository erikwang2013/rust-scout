use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchHit {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(default)]
    pub source: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlight: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchResult {
    #[serde(default)]
    pub hits: Vec<SearchHit>,
    #[serde(default)]
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregations: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facets: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

impl SearchResult {
    pub fn ids(&self) -> Vec<String> {
        self.hits.iter().map(|hit| hit.id.clone()).collect()
    }

    /// 清空 `hits`，保留 `total` 等其余字段。
    ///
    /// 供 `take(0)` 的契约对齐使用：Collection 与 ES 的 `total` 是「分页之前」的
    /// 全量匹配数，所以 `take(0)` 返回的是「命中总数 + 空 hits」。后端的 total 只能
    /// 从后端拿，因此那几个驱动仍会发一次请求（只取 1 条），拿到后用这个方法把
    /// hits 丢掉。没有它，同样的 `take(0)` 会在内存/ES 上报 N、在其它后端上报 0。
    ///
    /// cfg 必须与调用方一致（只有这三个驱动需要它）：Collection/ES 的 `total` 本来就
    /// 是分页前算的，不需要清 hits。不门控时默认构建会报 `without_hits` never used。
    #[cfg(any(feature = "meilisearch", feature = "typesense", feature = "algolia"))]
    pub(crate) fn without_hits(self) -> Self {
        Self {
            hits: Vec::new(),
            ..self
        }
    }
}

#[cfg(all(
    test,
    any(feature = "meilisearch", feature = "typesense", feature = "algolia")
))]
mod tests {
    use super::*;

    #[test]
    fn without_hits_keeps_total() {
        let result = SearchResult {
            hits: vec![SearchHit {
                id: "a".to_string(),
                ..SearchHit::default()
            }],
            total: 42,
            ..SearchResult::default()
        };
        let cleared = result.without_hits();
        assert!(cleared.hits.is_empty());
        assert_eq!(cleared.total, 42, "total 是分页前的全量匹配数，不能被清掉");
    }
}
