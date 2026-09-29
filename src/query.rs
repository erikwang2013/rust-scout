//! Elasticsearch/OpenSearch 查询体构建与响应解析（纯函数，无 IO）。

use crate::{SearchBuilder, SearchHit, SearchResult};

pub(crate) fn build_query(builder: &SearchBuilder) -> serde_json::Value {
    let mut must = Vec::new();
    let mut filter = Vec::new();
    let mut must_not = Vec::new();
    if !builder.query.is_empty() {
        // lenient 只忽略「类型不匹配」类错误（文本查数值字段）；
        // "(" / "foo AND" 这类语法错误它挡不住（实测 OpenSearch 2.19 仍 400），
        // 那部分由引擎按无命中处理，见 [`is_query_parse_error`]。
        must.push(serde_json::json!({"query_string": {"query": builder.query, "lenient": true}}));
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

/// 拦下 [`build_body`] 里说了不算的 options 键。
///
/// options 是驱动透传口（设计稿允许用它注入原生查询片段），但那只对非保留键成立：
/// `query` 由 [`build_query`]、`from`/`size` 由调用参数、`sort` 由 `.order_by()` 决定，
/// options 里的同名键永远进不了请求体，等于静默丢弃。最坏的是 `option("query", …)` ——
/// 调用方以为加了过滤条件，实际发出去的是 `match_all`，拿到全量未过滤结果且不报错。
/// 这里把静默换成报错，并指出该用哪个 builder 方法。
///
/// `sort` 只在同时用了 `.order_by()` 时才被丢弃：单独 `option("sort", …)` 照旧原样生效，
/// 没被丢弃就不该误伤。
fn reject_reserved_options(builder: &SearchBuilder) -> crate::Result<()> {
    let Some(options) = builder.options.as_object() else {
        return Ok(());
    };
    let reserved = [
        (
            "query",
            "options cannot override the query body; set SearchBuilder's `query` field instead",
        ),
        ("from", "use SearchBuilder::skip() instead"),
        ("size", "use SearchBuilder::take() instead"),
    ];
    for (key, hint) in reserved {
        if options.contains_key(key) {
            return Err(crate::ScoutError::Unsupported(format!(
                "option(\"{key}\") has no effect on the Elasticsearch driver: {hint}"
            )));
        }
    }
    if !builder.orders.is_empty() && options.contains_key("sort") {
        return Err(crate::ScoutError::Unsupported(
            "option(\"sort\") has no effect once .order_by() is set: \
             use SearchBuilder::order_by() instead"
                .to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn build_body(
    builder: &SearchBuilder,
    from: usize,
    size: usize,
) -> crate::Result<serde_json::Value> {
    reject_reserved_options(builder)?;
    let mut body = serde_json::json!({
        "query": build_query(builder),
        "from": from,
        "size": size,
        // ES 默认只精确统计前 10000 条（超出恒为 10000），其余驱动返回真实 total。
        // 默认值放在这里、保留键之外，调用方仍可用 option("track_total_hits", ..) 覆盖。
        "track_total_hits": true
    });
    // options 透传：会被覆盖的键已在 reject_reserved_options 里拦下，能到这里的都不会被丢弃。
    if let Some(options) = builder.options.as_object() {
        for (key, value) in options {
            body[key] = value.clone();
        }
    }
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
    Ok(body)
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

/// 判断 400 响应体是否为「查询语法错误」。
///
/// `lenient` 只覆盖类型不匹配类错误（文本查数值字段）；`(` / `foo AND` 这类
/// JavaCC 语法错误照样 400（实测 ES 7.10 血统的 OpenSearch 2.19），故需单独识别：
/// root_cause 为 query_shard_exception 且 reason 以 "Failed to parse query" 开头。
pub(crate) fn is_query_parse_error(body: &str) -> bool {
    let Ok(raw) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    raw.pointer("/error/root_cause/0/type")
        .and_then(|v| v.as_str())
        == Some("query_shard_exception")
        && raw
            .pointer("/error/root_cause/0/reason")
            .and_then(|v| v.as_str())
            .is_some_and(|reason| reason.starts_with("Failed to parse query"))
}

/// 逐条检查 _bulk 响应 items；任一条失败返回 Backend（含该条 id 与错误）。
pub(crate) fn check_bulk_items(response: &serde_json::Value) -> crate::Result<()> {
    check_bulk_items_inner(response, false)
}

/// 删除语义的 bulk 结果检查：`404` 视为成功。
///
/// ES 对「索引不存在」的删除项会回 `index_not_found_exception`（带 `error` 键），
/// 对「文档不存在」回 `result: not_found`。删除必须幂等 —— 与
/// [`crate::Engine::delete_in`] 及 collection/database/typesense/meilisearch 的
/// `delete_bulk` 行为对齐；否则同一个「重复删除」在别的驱动上返回 `Ok`、
/// 在 ES 上返回 `Err(Backend 404)`。其余错误照旧上抛。
pub(crate) fn check_bulk_delete_items(response: &serde_json::Value) -> crate::Result<()> {
    check_bulk_items_inner(response, true)
}

fn check_bulk_items_inner(
    response: &serde_json::Value,
    tolerate_404: bool,
) -> crate::Result<()> {
    if let Some(items) = response.get("items").and_then(|v| v.as_array()) {
        for item in items {
            let entry = item
                .as_object()
                .and_then(|m| m.values().next())
                .unwrap_or(&serde_json::Value::Null);
            if tolerate_404 && entry.get("status").and_then(|s| s.as_u64()) == Some(404) {
                continue;
            }
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
#[path = "query_tests.rs"]
mod tests;
