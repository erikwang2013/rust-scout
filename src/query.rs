//! Elasticsearch/OpenSearch 查询体构建与响应解析（纯函数，无 IO）。

use crate::{SearchBuilder, SearchHit, SearchResult};

pub(crate) fn build_query(builder: &SearchBuilder) -> serde_json::Value {
    let mut must = Vec::new();
    let mut filter = Vec::new();
    let mut must_not = Vec::new();
    if !builder.query.is_empty() {
        must.push(serde_json::json!({"query_string": {"query": builder.query}}));
    }
    for where_ in &builder.wheres {
        filter.push(serde_json::json!({"term": {where_.field.clone(): where_.value}}));
    }
    for (field, values) in &builder.where_ins {
        filter.push(serde_json::json!({"terms": {field: values}}));
    }
    for (field, values) in &builder.where_not_ins {
        must_not.push(serde_json::json!({"terms": {field: values}}));
    }
    match builder.trashed {
        // term 语义与 CollectionEngine 的 `as_bool().unwrap_or(false)` 对齐：
        // 值非 true（false/字符串/缺失）的文档在 Exclude 下可见。
        crate::TrashedFilter::Exclude => filter.push(serde_json::json!(
            {"bool": {"must_not": [{"term": {"__soft_deleted": true}}]}}
        )),
        crate::TrashedFilter::OnlyTrashed => {
            filter.push(serde_json::json!({"term": {"__soft_deleted": true}}));
        }
        crate::TrashedFilter::WithTrashed => {}
    }
    if must.is_empty() && filter.is_empty() && must_not.is_empty() {
        return serde_json::json!({"match_all": {}});
    }
    serde_json::json!({"bool": {"must": must, "filter": filter, "must_not": must_not}})
}

pub(crate) fn build_body(builder: &SearchBuilder, from: usize, size: usize) -> serde_json::Value {
    let mut body = serde_json::json!({
        "query": build_query(builder),
        "from": from,
        "size": size
    });
    // options 透传，但 query/from/size/sort 优先（options 不覆盖）。
    if let Some(options) = builder.options.as_object() {
        for (key, value) in options {
            body[key] = value.clone();
        }
    }
    body["query"] = build_query(builder);
    body["from"] = serde_json::json!(from);
    body["size"] = serde_json::json!(size);
    if !builder.orders.is_empty() {
        let sort = builder
            .orders
            .iter()
            .map(|order| {
                serde_json::json!({order.field.clone(): {"order": if order.desc {"desc"} else {"asc"}}})
            })
            .collect::<Vec<_>>();
        body["sort"] = serde_json::Value::Array(sort);
    }
    // 高亮激活：options["highlight"] == true 展开为 fields {"*": {}}；
    // options 已带 highlight 对象则透传（merge 已处理）；其它值（false/null）移除。
    match builder.options.get("highlight") {
        Some(serde_json::Value::Bool(true)) => {
            body["highlight"] = serde_json::json!({"fields": {"*": {}}});
        }
        Some(serde_json::Value::Object(_)) => {}
        Some(_) => {
            body.as_object_mut().expect("body is object").remove("highlight");
        }
        None => {}
    }
    body
}

pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

pub(crate) fn parse_search_response(raw: &serde_json::Value) -> SearchResult {
    let empty_hits = Vec::new();
    let hits = raw
        .get("hits")
        .and_then(|v| v.get("hits"))
        .and_then(|v| v.as_array())
        .unwrap_or(&empty_hits);
    let total = raw
        .get("hits")
        .and_then(|v| v.get("total"))
        .and_then(|v| v.get("value"))
        .and_then(|v| v.as_u64())
        .unwrap_or(hits.len() as u64) as usize;
    let mut results = Vec::with_capacity(hits.len());
    for hit in hits {
        let id = hit
            .get("_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let source = hit.get("_source").cloned().unwrap_or_default();
        let score = hit.get("_score").and_then(|v| v.as_f64());
        let highlight = hit.get("highlight").cloned();
        results.push(SearchHit {
            id,
            score,
            source,
            highlight,
        });
    }
    SearchResult {
        hits: results,
        total,
        aggregations: raw.get("aggregations").cloned(),
        facets: raw.get("facets").cloned(),
        ..SearchResult::default()
    }
}

/// 逐条检查 _bulk 响应 items；任一条失败返回 Backend（含该条 id 与错误）。
pub(crate) fn check_bulk_items(response: &serde_json::Value) -> crate::Result<()> {
    if let Some(items) = response.get("items").and_then(|v| v.as_array()) {
        for item in items {
            let entry = item
                .as_object()
                .and_then(|m| m.values().next())
                .unwrap_or(&serde_json::Value::Null);
            if let Some(error) = entry.get("error") {
                let id = entry
                    .get("_id")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                return Err(crate::ScoutError::Backend(format!(
                    "bulk item {} failed: {}",
                    id, error
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_query_translates_all_conditions() {
        let builder = SearchBuilder::new("rust")
            .where_field("status", "active")
            .where_in("tags", ["a", "b"])
            .where_not_in("deleted", [true]);
        assert_eq!(
            build_query(&builder),
            json!({
                "bool": {
                    "must": [{"query_string": {"query": "rust"}}],
                    "filter": [
                        {"term": {"status": "active"}},
                        {"terms": {"tags": ["a", "b"]}},
                        {"bool": {"must_not": [{"term": {"__soft_deleted": true}}]}}
                    ],
                    "must_not": [{"terms": {"deleted": [true]}}]
                }
            })
        );
    }

    #[test]
    fn build_query_empty_builder_matches_all() {
        assert_eq!(
            build_query(&SearchBuilder::default().with_trashed()),
            json!({"match_all": {}})
        );
    }

    #[test]
    fn build_body_adds_sort_orders() {
        let builder = SearchBuilder::default()
            .with_trashed()
            .order_by("created_at", true)
            .order_by("title", false);
        assert_eq!(
            build_body(&builder, 5, 20),
            json!({
                "query": {"match_all": {}},
                "from": 5,
                "size": 20,
                "sort": [
                    {"created_at": {"order": "desc"}},
                    {"title": {"order": "asc"}}
                ]
            })
        );
    }

    #[test]
    fn percent_encode_escapes_reserved_and_utf8() {
        assert_eq!(percent_encode("a b/c"), "a%20b%2Fc");
        assert_eq!(percent_encode("safe-._~AZ09"), "safe-._~AZ09");
        assert_eq!(percent_encode("中文"), "%E4%B8%AD%E6%96%87");
    }

    #[test]
    fn parse_search_response_extracts_total_aggregations_and_highlight() {
        let raw = json!({
            "hits": {
                "total": {"value": 42},
                "hits": [{
                    "_id": "a",
                    "_score": 1.5,
                    "_source": {"title": "x"},
                    "highlight": {"title": ["<em>x</em>"]}
                }]
            },
            "aggregations": {"by_tag": {"buckets": []}}
        });
        let result = parse_search_response(&raw);
        assert_eq!(result.total, 42);
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].id, "a");
        assert_eq!(result.hits[0].score, Some(1.5));
        assert_eq!(result.hits[0].source, json!({"title": "x"}));
        assert_eq!(result.hits[0].highlight, Some(json!({"title": ["<em>x</em>"]})));
        assert_eq!(result.aggregations, Some(json!({"by_tag": {"buckets": []}})));
        assert!(result.facets.is_none());
    }

    #[test]
    fn parse_search_response_falls_back_to_hit_count() {
        // total 缺失 → 回退 hits 长度；_source 缺失 → Null。
        let raw = json!({"hits": {"hits": [{"_id": "a"}, {"_id": "b"}]}});
        let result = parse_search_response(&raw);
        assert_eq!(result.total, 2);
        assert_eq!(result.hits[0].source, serde_json::Value::Null);
        assert!(result.aggregations.is_none());
        assert!(result.facets.is_none());
    }

    #[test]
    fn parse_search_response_extracts_facets() {
        let raw = json!({"hits": {"hits": []}, "facets": {"tags": {}}});
        let result = parse_search_response(&raw);
        assert_eq!(result.facets, Some(json!({"tags": {}})));
    }

    #[test]
    fn build_body_merges_options_but_keeps_query_from_size_sort() {
        let builder = SearchBuilder::default()
            .with_trashed()
            .option("size", 999)
            .option("track_total_hits", true)
            .option("custom", json!({"a": 1}))
            .take(3)
            .order_by("title", false);
        let body = build_body(&builder, 5, 20);
        assert_eq!(body["size"], 20);
        assert_eq!(body["from"], 5);
        assert_eq!(body["query"], json!({"match_all": {}}));
        assert_eq!(body["sort"], json!([{"title": {"order": "asc"}}]));
        assert_eq!(body["track_total_hits"], true);
        assert_eq!(body["custom"], json!({"a": 1}));
    }

    #[test]
    fn build_body_activates_highlight_from_options() {
        let builder = SearchBuilder::default().option("highlight", true);
        let body = build_body(&builder, 0, 10);
        assert_eq!(body["highlight"], json!({"fields": {"*": {}}}));
    }

    #[test]
    fn build_body_passes_through_highlight_object() {
        let builder = SearchBuilder::default()
            .option("highlight", json!({"fields": {"title": {}}, "pre_tags": ["<b>"]}));
        let body = build_body(&builder, 0, 10);
        assert_eq!(
            body["highlight"],
            json!({"fields": {"title": {}}, "pre_tags": ["<b>"]})
        );
    }

    #[test]
    fn build_body_drops_non_highlight_value() {
        let body = build_body(&SearchBuilder::default().option("highlight", false), 0, 10);
        assert!(body.get("highlight").is_none());
    }

    #[test]
    fn build_query_trashed_default_excludes_soft_deleted() {
        let query = build_query(&SearchBuilder::default());
        assert_eq!(
            query,
            json!({
                "bool": {
                    "must": [],
                    "filter": [{"bool": {"must_not": [{"term": {"__soft_deleted": true}}]}}],
                    "must_not": []
                }
            })
        );
    }

    #[test]
    fn build_query_only_trashed_uses_term() {
        let query = build_query(&SearchBuilder::default().only_trashed());
        assert_eq!(
            query,
            json!({
                "bool": {
                    "must": [],
                    "filter": [{"term": {"__soft_deleted": true}}],
                    "must_not": []
                }
            })
        );
    }

    #[test]
    fn build_query_with_trashed_no_filter() {
        assert_eq!(
            build_query(&SearchBuilder::default().with_trashed()),
            json!({"match_all": {}})
        );
    }

    #[test]
    fn check_bulk_items_reports_failed_item_id() {
        let response = json!({
            "items": [
                {"index": {"_id": "ok", "status": 201}},
                {"index": {"_id": "bad", "status": 400, "error": {"type": "mapper_parsing_exception", "reason": "boom"}}}
            ]
        });
        let err = check_bulk_items(&response).unwrap_err();
        assert!(
            err.to_string().contains("bad") && err.to_string().contains("boom"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn check_bulk_items_ok_on_all_success() {
        let response = json!({"items": [{"index": {"_id": "a", "status": 201}}]});
        assert!(check_bulk_items(&response).is_ok());
    }
}
