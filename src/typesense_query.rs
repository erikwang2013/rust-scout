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

/// 等值/IN/软删除 → filter_by 表达式（` && ` 连接）；无任何条件时返回 None。
pub(crate) fn build_filter_by(builder: &SearchBuilder) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for w in &builder.wheres {
        parts.push(format!("{}:={}", w.field, filter_value(&w.value)));
    }
    for (field, values) in &builder.where_ins {
        let list = values.iter().map(filter_value).collect::<Vec<_>>().join(", ");
        parts.push(format!("{}:=[{}]", field, list));
    }
    for (field, values) in &builder.where_not_ins {
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
        None
    } else {
        Some(parts.join(" && "))
    }
}

fn build_sort_by(builder: &SearchBuilder) -> Option<String> {
    if builder.orders.is_empty() {
        return None;
    }
    let parts: Vec<String> = builder
        .orders
        .iter()
        .map(|o| format!("{}:{}", o.field, if o.desc { "desc" } else { "asc" }))
        .collect();
    Some(parts.join(","))
}

/// GET 搜索 query 参数；q 为空时省略 q/query_by（Typesense 空串匹配全部）。
pub(crate) fn search_params(builder: &SearchBuilder, page: usize, per_page: usize) -> Vec<(String, String)> {
    let mut params = Vec::new();
    if !builder.query.is_empty() {
        params.push(("q".to_string(), builder.query.clone()));
        if let Some(query_by) = builder.options.get("query_by").and_then(Value::as_str) {
            params.push(("query_by".to_string(), query_by.to_string()));
        }
    }
    if let Some(filter) = build_filter_by(builder) {
        params.push(("filter_by".to_string(), filter));
    }
    params.push(("per_page".to_string(), per_page.to_string()));
    params.push(("page".to_string(), page.to_string()));
    if let Some(sort) = build_sort_by(builder) {
        params.push(("sort_by".to_string(), sort));
    }
    params
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
            build_filter_by(&builder).unwrap(),
            r#"title:="a" && count:=5 && active:=true && tag:=["x", "y"] && tag:!=["z"] && __soft_deleted:!=true"#
        );
    }

    #[test]
    fn filter_by_trashed_variants() {
        assert_eq!(build_filter_by(&SearchBuilder::new("")).unwrap(), "__soft_deleted:!=true");
        assert_eq!(build_filter_by(&SearchBuilder::new("").only_trashed()).unwrap(), "__soft_deleted:=true");
        assert_eq!(build_filter_by(&SearchBuilder::new("").with_trashed()), None);
    }

    #[test]
    fn search_params_build() {
        let builder = SearchBuilder::new("needle").where_field("active", true)
            .order_by("price", true).option("query_by", "title,body");
        let map: std::collections::HashMap<String, String> =
            search_params(&builder, 2, 10).into_iter().collect();
        assert_eq!(map["q"], "needle");
        assert_eq!(map["query_by"], "title,body");
        assert_eq!(map["filter_by"], "active:=true && __soft_deleted:!=true");
        assert_eq!(map["per_page"], "10");
        assert_eq!(map["page"], "2");
        assert_eq!(map["sort_by"], "price:desc");
    }

    #[test]
    fn search_params_omit_q_and_filter_when_empty() {
        let map: std::collections::HashMap<String, String> =
            search_params(&SearchBuilder::new("").with_trashed(), 1, 10).into_iter().collect();
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
}
