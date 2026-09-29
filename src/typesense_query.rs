#![cfg(feature = "typesense")]

use serde_json::Value;

use crate::{SearchBuilder, SearchDocument, SearchHit, SearchResult, TrashedFilter};

/// filter_by 值：字符串加双引号（转义 `\` 与 `"`），数字/bool 裸值。
fn filter_value(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// `id:=[...]` 删除过滤器（delete-by-query 用）。值走同一个 [`filter_value`] 转义。
pub(crate) fn id_filter(ids: &[String]) -> String {
    let list = ids
        .iter()
        .map(|id| filter_value(&Value::String(id.clone())))
        .collect::<Vec<_>>()
        .join(", ");
    format!("id:=[{}]", list)
}

/// 等值/IN/软删除 → filter_by 表达式（` && ` 连接）；无任何条件时返回 None。
///
/// 字段名先过 [`crate::validate_field_name`]：表达式是文本拼接，值转了义而字段名
/// 曾经裸拼，`where_field("x:=1 || y", "z")` 能改写整条表达式——`&&` 优先级更高，
/// 本函数追加的 `__soft_deleted:!=true` 守卫会被 OR 掉。
pub(crate) fn build_filter_by(builder: &SearchBuilder) -> crate::Result<Option<String>> {
    let mut parts: Vec<String> = Vec::new();
    for w in &builder.wheres {
        crate::validate_field_name(&w.field)?;
        parts.push(format!("{}:={}", w.field, filter_value(&w.value)));
    }
    for (field, values) in &builder.where_ins {
        crate::validate_field_name(field)?;
        let list = values.iter().map(filter_value).collect::<Vec<_>>().join(", ");
        parts.push(format!("{}:=[{}]", field, list));
    }
    for (field, values) in &builder.where_not_ins {
        if values.is_empty() {
            continue; // 空 NOT IN 集合 = 无过滤（Collection 语义，与 Algolia 一致）
        }
        crate::validate_field_name(field)?;
        let list = values.iter().map(filter_value).collect::<Vec<_>>().join(", ");
        parts.push(format!("{}:!=[{}]", field, list));
    }
    match builder.trashed {
        // `:=false` 不匹配缺失字段（Typesense 缺失字段按 null 处理），未软删
        // 文档会被全部隐藏；`:!=true` 对 null/缺失/未定义字段均匹配，
        // 与 Collection 的 as_bool().unwrap_or(false) 语义对齐。
        TrashedFilter::Exclude => parts.push("__soft_deleted:!=true".to_string()),
        TrashedFilter::OnlyTrashed => parts.push("__soft_deleted:=true".to_string()),
        TrashedFilter::WithTrashed => {}
    }
    if parts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(parts.join(" && ")))
    }
}

fn build_sort_by(builder: &SearchBuilder) -> crate::Result<Option<String>> {
    if builder.orders.is_empty() {
        return Ok(None);
    }
    let mut parts: Vec<String> = Vec::new();
    for o in &builder.orders {
        // 同 filter_by：`sort_by` 也是 `field:asc` 文本拼接，字段名要校验。
        crate::validate_field_name(&o.field)?;
        parts.push(format!("{}:{}", o.field, if o.desc { "desc" } else { "asc" }));
    }
    Ok(Some(parts.join(",")))
}

/// `skip`/`take` → `offset`/`limit`。`skip` 精确映射到 `offset`，**不**折算成页：
/// 页只能表达页对齐的起点，`.skip(15).take(10)` 会丢掉 `15 % 10` 的余数，
/// 返回 10–19 而不是 Collection 的 15–24（客户端按偏移翻页会重复/漏行）。
/// `take(0)` 由调用方短路，这里缺省 10。
pub(crate) fn offset_limit(builder: &SearchBuilder) -> (usize, usize) {
    (builder.skip.unwrap_or(0), builder.take.unwrap_or(10))
}

/// `page`/`per_page` → `offset`/`limit`：page N 即 offset `(N-1)*per_page`，
/// 与 `CollectionEngine::paginate` 的页语义一致。
pub(crate) fn page_offset(page: usize, per_page: usize) -> (usize, usize) {
    let per_page = per_page.max(1);
    (
        page.max(1).saturating_sub(1).saturating_mul(per_page),
        per_page,
    )
}

/// GET 搜索 query 参数；q 为空时省略 q/query_by（Typesense 空串匹配全部）。
///
/// 注意：**q 非空时 Typesense 强制要求 `query_by`**，只发 q 会被后端以
/// 400 `Parameter \`query_by\` is required` 拒绝。这里不替调用方兜底补一个字段
/// （猜错字段会返回静默错误的排序），只保证错误信息里带着参数名。见 README。
///
/// 分页用 `offset`/`limit`（`page`/`per_page` 的替代写法，两者不可混用）：
/// 后者只能表达页对齐的起点，会丢掉 `skip` 的余数。
/// `limit` > 250 会报错（`per_page` 上限），由调用方约束。
pub(crate) fn search_params(
    builder: &SearchBuilder,
    offset: usize,
    limit: usize,
) -> crate::Result<Vec<(String, String)>> {
    let mut params = Vec::new();
    if !builder.query.is_empty() {
        params.push(("q".to_string(), builder.query.clone()));
        if let Some(query_by) = builder.options.get("query_by").and_then(Value::as_str) {
            params.push(("query_by".to_string(), query_by.to_string()));
        }
    }
    if let Some(filter) = build_filter_by(builder)? {
        params.push(("filter_by".to_string(), filter));
    }
    params.push(("limit".to_string(), limit.to_string()));
    params.push(("offset".to_string(), offset.to_string()));
    if let Some(sort) = build_sort_by(builder)? {
        params.push(("sort_by".to_string(), sort));
    }
    Ok(params)
}

/// 文档 → NDJSON 行；`id` 键统一注入/覆盖为 doc.id，每行以 `\n` 结尾。
pub(crate) fn ndjson_payload(docs: &[&SearchDocument]) -> crate::Result<String> {
    let mut body = String::new();
    for doc in docs {
        let mut fields = doc.fields.clone();
        fields.insert("id".into(), Value::String(doc.id.clone()));
        body.push_str(&serde_json::to_string(&fields)?);
        body.push('\n');
    }
    Ok(body)
}

/// 响应解析：id 取 `hits[i].document.id`，source 取 document，highlight 取
/// highlight，total 取 `found`（缺失回退 hits.len()）。
pub(crate) fn parse_search_response(raw: &Value) -> SearchResult {
    let hits = raw.get("hits").and_then(Value::as_array);
    let total = raw
        .get("found")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or_else(|| hits.map_or(0, Vec::len));
    let hits = hits
        .map(|arr| arr.iter().filter_map(hit_from_response).collect())
        .unwrap_or_default();
    SearchResult {
        hits,
        total,
        ..Default::default()
    }
}

fn hit_from_response(hit: &Value) -> Option<SearchHit> {
    let document = hit.get("document")?;
    let id = document.get("id")?;
    let id = id.as_str().map(str::to_string).unwrap_or_else(|| id.to_string());
    Some(SearchHit {
        id,
        score: None,
        source: document.clone(),
        highlight: hit.get("highlight").cloned(),
    })
}

/// 非 2xx → `ScoutError::Backend("METHOD path -> status: body")`。
pub(crate) fn check_status(
    method: &str,
    path: &str,
    status: &reqwest::StatusCode,
    text: &str,
) -> crate::Result<()> {
    if status.is_success() {
        Ok(())
    } else {
        Err(crate::ScoutError::Backend(format!(
            "{} {} -> {}: {}",
            method, path, status, text
        )))
    }
}

/// 非 2xx → Backend；否则逐行检查 import 结果，`success:false` 的行报错。
pub(crate) fn check_import(status: &reqwest::StatusCode, path: &str, text: &str) -> crate::Result<()> {
    check_status("POST", path, status, text)?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let row: Value = serde_json::from_str(line)?;
        if row.get("success").and_then(Value::as_bool) == Some(false) {
            let err = row.get("error").and_then(Value::as_str).unwrap_or("unknown error");
            return Err(crate::ScoutError::Backend(format!(
                "{} -> import failed: {}",
                path, err
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_by_clauses() {
        let builder = SearchBuilder::new("").where_field("title", "a").where_field("count", 5)
            .where_field("active", true).where_in("tag", ["x", "y"]).where_not_in("tag", ["z"]);
        assert_eq!(
            build_filter_by(&builder).unwrap().unwrap(),
            r#"title:="a" && count:=5 && active:=true && tag:=["x", "y"] && tag:!=["z"] && __soft_deleted:!=true"#
        );
    }

    #[test]
    fn filter_by_trashed_variants() {
        assert_eq!(build_filter_by(&SearchBuilder::new("")).unwrap().unwrap(), "__soft_deleted:!=true");
        assert_eq!(
            build_filter_by(&SearchBuilder::new("").only_trashed()).unwrap().unwrap(),
            "__soft_deleted:=true"
        );
        assert_eq!(build_filter_by(&SearchBuilder::new("").with_trashed()).unwrap(), None);
    }

    #[test]
    fn operator_chars_in_field_name_are_rejected() {
        // 回归：字段名曾经裸拼进 filter_by/sort_by。`x:=1 || y` 会让 `&&` 优先级
        // 反客为主，把软删除守卫 OR 掉，从而把已软删的文档一起查出来。
        let b = SearchBuilder::new("").where_field("x:=1 || y", "z");
        assert!(matches!(
            build_filter_by(&b),
            Err(crate::ScoutError::InvalidFieldName(_))
        ));
        let b = SearchBuilder::new("").where_in("a && __soft_deleted:=true", ["1"]);
        assert!(build_filter_by(&b).is_err());
        let b = SearchBuilder::new("").where_not_in("a,b", ["1"]);
        assert!(build_filter_by(&b).is_err());
        let b = SearchBuilder::new("").order_by("price:asc,__soft_deleted", true);
        assert!(search_params(&b, 0, 10).is_err());
        // 合法字段名不受影响：点号（嵌套）与中文列名照常。
        let ok = SearchBuilder::new("").where_field("author.name", "x").order_by("价格", false);
        assert_eq!(
            build_filter_by(&ok).unwrap().unwrap(),
            r#"author.name:="x" && __soft_deleted:!=true"#
        );
    }

    #[test]
    fn id_filter_batches_ids_with_escaped_values() {
        // delete-by-query 的过滤器：id 也要走 filter_value 转义（`"`/`\`）。
        assert_eq!(
            id_filter(&["a".to_string(), "b\"c".to_string()]),
            r#"id:=["a", "b\"c"]"#
        );
    }

    #[test]
    fn search_params_build() {
        let builder = SearchBuilder::new("needle").where_field("active", true)
            .order_by("price", true).option("query_by", "title,body");
        let map: std::collections::HashMap<String, String> =
            search_params(&builder, 2, 10).unwrap().into_iter().collect();
        assert_eq!(map["q"], "needle");
        assert_eq!(map["query_by"], "title,body");
        assert_eq!(map["filter_by"], "active:=true && __soft_deleted:!=true");
        assert_eq!(map["offset"], "2");
        assert_eq!(map["limit"], "10");
        // page/per_page 只能表达页对齐的起点（且与 offset 互斥），不再使用。
        assert!(!map.contains_key("page"));
        assert!(!map.contains_key("per_page"));
        assert_eq!(map["sort_by"], "price:desc");
    }

    #[test]
    fn offset_limit_keeps_skip_remainder() {
        // 回归：skip 曾按 skip/per_page+1 折算成页，15 % 10 的余数被丢掉，
        // .skip(15).take(10) 返回 10–19 而不是 Collection 的 15–24。
        assert_eq!(offset_limit(&SearchBuilder::new("").skip(15).take(10)), (15, 10));
        assert_eq!(offset_limit(&SearchBuilder::new("").skip(25).take(7)), (25, 7));
        assert_eq!(offset_limit(&SearchBuilder::new("")), (0, 10));
    }

    #[test]
    fn page_offset_matches_collection_page_semantics() {
        // paginate 的页语义不变：page N == offset (N-1)*per_page，参数钳到合法范围。
        assert_eq!(page_offset(1, 10), (0, 10));
        assert_eq!(page_offset(2, 10), (10, 10));
        assert_eq!(page_offset(0, 0), (0, 1));
        assert_eq!(page_offset(3, 0), (2, 1));
    }

    #[test]
    fn search_params_omit_q_and_filter_when_empty() {
        let map: std::collections::HashMap<String, String> =
            search_params(&SearchBuilder::new("").with_trashed(), 1, 10).unwrap().into_iter().collect();
        assert!(!map.contains_key("q"));
        assert!(!map.contains_key("query_by"));
        assert!(!map.contains_key("filter_by"));
    }

    #[test]
    fn parse_response_extracts_document_and_highlight() {
        let raw = serde_json::json!({
            "found": 3,
            "hits": [
                {"document": {"id": "1", "title": "a"}, "highlight": {"title": "<mark>a</mark>"}},
                {"document": {"id": "2"}}
            ]
        });
        let result = parse_search_response(&raw);
        assert_eq!(result.total, 3);
        assert_eq!(result.hits[0].id, "1");
        assert_eq!(result.hits[0].source["title"], "a");
        assert_eq!(result.hits[0].highlight.as_ref().unwrap()["title"], "<mark>a</mark>");
        assert_eq!(result.hits[1].highlight, None);
    }

    #[test]
    fn parse_response_found_missing_falls_back() {
        let raw = serde_json::json!({"hits": [{"document": {"id": "1"}}]});
        assert_eq!(parse_search_response(&raw).total, 1);
    }

    #[test]
    fn ndjson_payload_injects_id_and_newline() {
        let docs: Vec<SearchDocument> = vec![
            SearchDocument::new("a", serde_json::json!({"title": "x"})).unwrap(),
            SearchDocument::new("b", serde_json::json!({"id": "wrong", "title": "y"})).unwrap(),
        ];
        let refs: Vec<&SearchDocument> = docs.iter().collect();
        let payload = ndjson_payload(&refs).unwrap();
        assert!(payload.ends_with('\n'));
        let lines: Vec<&str> = payload.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first["id"], "a");
        assert_eq!(second["id"], "b"); // 覆盖既有 id
        assert_eq!(second["title"], "y");
    }

    #[test]
    fn check_import_reports_failed_line() {
        let ok_body = "{\"success\":true,\"document\":{\"id\":\"a\"}}\n{\"success\":true,\"document\":{\"id\":\"b\"}}\n";
        assert!(check_import(&reqwest::StatusCode::OK, "/x", ok_body).is_ok());
        let bad_body = "{\"success\":true}\n{\"success\":false,\"error\":\"Document missing required field\"}\n";
        let err = check_import(&reqwest::StatusCode::OK, "/x", bad_body).unwrap_err();
        assert!(err.to_string().contains("Document missing required field"));
    }

    #[test]
    fn encode_path_escapes_special_chars() {
        assert_eq!(crate::config::percent_encode("a/b c"), "a%2Fb%20c");
        assert_eq!(crate::config::percent_encode("simple-1"), "simple-1");
    }

    #[test]
    fn empty_not_in_is_dropped_but_empty_in_matches_nothing() {
        let b = SearchBuilder::new("q").where_not_in("cat", Vec::<&str>::new());
        let f = build_filter_by(&b).unwrap().unwrap_or_default();
        assert!(!f.contains("cat:!="), "空 NOT IN 不该下发: {f:?}");
    }

}
