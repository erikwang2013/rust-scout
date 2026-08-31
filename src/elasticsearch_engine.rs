use crate::engine::{Engine, EngineFuture};
use crate::{SearchBuilder, SearchDocument, SearchHit, SearchResult};

pub struct ElasticsearchEngine {
    host: String,
    api_key: Option<String>,
    client: reqwest::blocking::Client,
}

impl ElasticsearchEngine {
    pub fn new(host: String, api_key: Option<String>) -> Self {
        Self {
            host: host.trim_end_matches('/').to_string(),
            api_key,
            client: reqwest::blocking::Client::new(),
        }
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> crate::Result<serde_json::Value> {
        let mut request = self
            .client
            .request(method, &format!("{}{}", self.host, path));
        if let Some(api_key) = &self.api_key {
            request = request.header("Authorization", format!("ApiKey {}", api_key));
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send()?;
        let status = response.status();
        let body = response.json::<serde_json::Value>()?;
        if !status.is_success() {
            return Err(crate::ScoutError::Backend(body.to_string()));
        }
        Ok(body)
    }
}

impl Engine for ElasticsearchEngine {
    fn update<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            for doc in docs {
                let index = doc.index.as_deref().unwrap_or("default");
                crate::validate_index_name(index)?;
                let path = format!(
                    "/{}/_doc/{}",
                    percent_encode(index),
                    percent_encode(&doc.id)
                );
                let _ = self.request(
                    reqwest::Method::PUT,
                    &path,
                    Some(serde_json::to_value(doc.fields.clone())?),
                )?;
            }
            Ok(())
        })
    }

    fn delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            for id in ids {
                let path = format!(
                    "/{}/_doc/{}",
                    percent_encode("default"),
                    percent_encode(id)
                );
                let _ = self.request(reqwest::Method::DELETE, &path, None)?;
            }
            Ok(())
        })
    }

    fn search<'a>(&'a self, builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult> {
        Box::pin(async move {
            let index = builder.index.as_deref().unwrap_or("default");
            crate::validate_index_name(index)?;
            let path = format!("/{}/_search", percent_encode(index));
            let body = build_body(builder, builder.skip.unwrap_or(0), builder.take.unwrap_or(10));
            let raw = self.request(reqwest::Method::POST, &path, Some(body))?;
            Ok(parse_search_response(&raw))
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
            let index = base.index.as_deref().unwrap_or("default");
            crate::validate_index_name(index)?;
            let path = format!("/{}/_search", percent_encode(index));
            let body = build_body(&base, base.skip.unwrap_or(0), base.take.unwrap_or(10));
            let raw = self.request(reqwest::Method::POST, &path, Some(body))?;
            Ok(parse_search_response(&raw))
        })
    }

    fn map_ids(&self, result: &SearchResult) -> Vec<String> {
        result.ids()
    }

    fn flush<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let path = format!("/{}/_refresh", percent_encode(index));
            let _ = self.request(reqwest::Method::POST, &path, None)?;
            Ok(())
        })
    }

    fn create_index<'a>(
        &'a self,
        index: &'a str,
        settings: serde_json::Value,
    ) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let path = format!("/{}", percent_encode(index));
            let _ = self.request(reqwest::Method::PUT, &path, Some(settings))?;
            Ok(())
        })
    }

    fn delete_index<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let path = format!("/{}", percent_encode(index));
            let _ = self.request(reqwest::Method::DELETE, &path, None)?;
            Ok(())
        })
    }
}

fn build_query(builder: &SearchBuilder) -> serde_json::Value {
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
    if must.is_empty() && filter.is_empty() && must_not.is_empty() {
        return serde_json::json!({"match_all": {}});
    }
    serde_json::json!({"bool": {"must": must, "filter": filter, "must_not": must_not}})
}

fn build_body(builder: &SearchBuilder, from: usize, size: usize) -> serde_json::Value {
    let mut body = serde_json::json!({
        "query": build_query(builder),
        "from": from,
        "size": size
    });
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
    body
}

fn percent_encode(s: &str) -> String {
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

fn parse_search_response(raw: &serde_json::Value) -> SearchResult {
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
        ..SearchResult::default()
    }
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
                        {"terms": {"tags": ["a", "b"]}}
                    ],
                    "must_not": [{"terms": {"deleted": [true]}}]
                }
            })
        );
    }

    #[test]
    fn build_query_empty_builder_matches_all() {
        assert_eq!(build_query(&SearchBuilder::default()), json!({"match_all": {}}));
    }

    #[test]
    fn build_body_adds_sort_orders() {
        let builder = SearchBuilder::default()
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
}
