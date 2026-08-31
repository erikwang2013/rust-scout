#![cfg(feature = "meilisearch")]

use serde_json::{Map, Value};

use crate::config::percent_encode;
use crate::engine::{Engine, EngineFuture};
use crate::{SearchBuilder, SearchDocument, SearchHit, SearchResult, TrashedFilter};

/// Meilisearch 引擎。文档主键固定为 `id` 字段（add-or-replace 语义）；
/// 软删除标记为 `__soft_deleted` 布尔字段。
pub struct MeilisearchEngine {
    host: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl MeilisearchEngine {
    pub fn new(host: String, api_key: Option<String>) -> Self {
        Self {
            host: host.trim_end_matches('/').to_string(),
            api_key,
            client: reqwest::Client::new(),
        }
    }

    /// 发送请求，返回状态码 + body 文本；网络错误经 `?` 转 `ScoutError::Http`。
    async fn raw(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<String>,
        content_type: Option<&str>,
    ) -> crate::Result<(reqwest::StatusCode, String)> {
        let mut request = self
            .client
            .request(method.clone(), format!("{}{}", self.host, path));
        if let Some(api_key) = &self.api_key {
            request = request.header("Authorization", format!("Bearer {}", api_key));
        }
        if let Some(body) = body {
            request = request.body(body);
            if let Some(content_type) = content_type {
                request = request.header(reqwest::header::CONTENT_TYPE, content_type);
            }
        }
        let response = request.send().await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Ok((status, body))
    }

    /// JSON 请求；非 2xx 返回 `ScoutError::Backend("METHOD path -> status: body")`。
    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> crate::Result<Value> {
        let (status, body) = self
            .raw(
                method.clone(),
                path,
                body.map(|b| b.to_string()),
                Some("application/json"),
            )
            .await?;
        if !status.is_success() {
            return Err(crate::ScoutError::Backend(format!(
                "{} {} -> {}: {}",
                method, path, status, body
            )));
        }
        Ok(serde_json::from_str(&body)?)
    }

    /// filter 值：字符串加双引号（转义 `\` 与 `"`），数字/bool 裸值。
    fn filter_value(v: &Value) -> String {
        match v {
            Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            other => other.to_string(),
        }
    }

    /// 等值/IN/软删除 → Meilisearch filter 表达式；无任何条件时返回 None。
    fn build_filter(builder: &SearchBuilder) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        for w in &builder.wheres {
            parts.push(format!("{}={}", w.field, Self::filter_value(&w.value)));
        }
        for (field, values) in &builder.where_ins {
            let list = values.iter().map(Self::filter_value).collect::<Vec<_>>().join(", ");
            parts.push(format!("{} IN [{}]", field, list));
        }
        for (field, values) in &builder.where_not_ins {
            let list = values.iter().map(Self::filter_value).collect::<Vec<_>>().join(", ");
            parts.push(format!("{} NOT IN [{}]", field, list));
        }
        // `=`/`IS NULL` 只匹配「存在且相等」/「显式 null」，未软删文档从未写入
        // 该字段，会被全部隐藏；`NOT __soft_deleted = true` 对缺失/false/null
        // 均匹配，与 Collection 的 as_bool().unwrap_or(false) 语义对齐。
        match builder.trashed {
            TrashedFilter::Exclude => parts.push("NOT __soft_deleted = true".to_string()),
            TrashedFilter::OnlyTrashed => parts.push("__soft_deleted=true".to_string()),
            TrashedFilter::WithTrashed => {}
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" AND "))
        }
    }

    fn sort_array(builder: &SearchBuilder) -> Value {
        let parts: Vec<String> = builder
            .orders
            .iter()
            .map(|o| format!("{}:{}", o.field, if o.desc { "desc" } else { "asc" }))
            .collect();
        Value::Array(parts.into_iter().map(Value::String).collect())
    }

    fn search_body(builder: &SearchBuilder, page: usize, per_page: usize) -> Value {
        let mut body = Map::new();
        body.insert("q".into(), Value::String(builder.query.clone()));
        if let Some(filter) = Self::build_filter(builder) {
            body.insert("filter".into(), Value::String(filter));
        }
        body.insert("hitsPerPage".into(), Value::from(per_page));
        body.insert("page".into(), Value::from(page));
        if !builder.orders.is_empty() {
            body.insert("sort".into(), Self::sort_array(builder));
        }
        Value::Object(body)
    }

    /// 文档 → Meilisearch 文档对象；`id` 键统一注入/覆盖为 doc.id。
    fn doc_to_document(doc: &SearchDocument) -> Value {
        let mut fields = doc.fields.clone();
        fields.insert("id".into(), Value::String(doc.id.clone()));
        Value::Object(fields)
    }

    /// 响应解析：id 取 `hits[i].id`，source 取整个 hit 对象，total 取 `totalHits`。
    fn parse_search_response(raw: &Value) -> SearchResult {
        let hits = raw.get("hits").and_then(Value::as_array);
        let total = raw
            .get("totalHits")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or_else(|| hits.map_or(0, Vec::len));
        let hits = hits
            .map(|arr| arr.iter().filter_map(Self::hit_from_response).collect())
            .unwrap_or_default();
        SearchResult {
            hits,
            total,
            ..Default::default()
        }
    }

    fn hit_from_response(hit: &Value) -> Option<SearchHit> {
        let id = hit.get("id")?;
        let id = id.as_str().map(str::to_string).unwrap_or_else(|| id.to_string());
        Some(SearchHit {
            id,
            // `_rankingScore` 需在索引设置中启用；缺失则无分。`_matchesPosition`
            // 是位置信息而非分数，忽略。
            score: hit.get("_rankingScore").and_then(Value::as_f64),
            source: hit.clone(),
            highlight: None,
        })
    }
}

impl Engine for MeilisearchEngine {
    fn update<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        // 与 update_bulk 相同：按索引分组一次 POST documents。
        self.update_bulk(docs)
    }

    fn delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        // 无索引信息：仅作用于 default 索引（同 ElasticsearchEngine 语义）。
        self.delete_in("default", ids)
    }

    fn delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            if ids.is_empty() {
                return Ok(());
            }
            crate::validate_index_name(index)?;
            let ids: Vec<Value> = ids.iter().map(|id| Value::String(id.clone())).collect();
            let path = format!("/indexes/{}/documents/delete-batch", percent_encode(index));
            let _ = self
                .request(reqwest::Method::POST, &path, Some(Value::Array(ids)))
                .await?;
            Ok(())
        })
    }

    fn search<'a>(&'a self, builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult> {
        let per_page = builder.take.unwrap_or(10).max(1);
        // skip 只换算到页（offset 余数丢失），与 PHP 版同源限制；
        // hitsPerPage > 1000 会 400（Meilisearch 上限），由调用方约束。
        let page = builder.skip.unwrap_or(0) / per_page + 1;
        Box::pin(async move {
            let index = builder.index.as_deref().unwrap_or("default");
            crate::validate_index_name(index)?;
            let body = Self::search_body(builder, page, per_page);
            let path = format!("/indexes/{}/search", percent_encode(index));
            let raw = self.request(reqwest::Method::POST, &path, Some(body)).await?;
            Ok(Self::parse_search_response(&raw))
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
            let index = builder.index.as_deref().unwrap_or("default");
            crate::validate_index_name(index)?;
            let body = Self::search_body(builder, page, per_page);
            let path = format!("/indexes/{}/search", percent_encode(index));
            let raw = self.request(reqwest::Method::POST, &path, Some(body)).await?;
            Ok(Self::parse_search_response(&raw))
        })
    }

    fn map_ids(&self, result: &SearchResult) -> Vec<String> {
        result.ids()
    }

    fn flush<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let path = format!("/indexes/{}/documents/delete-all", percent_encode(index));
            let _ = self.request(reqwest::Method::POST, &path, None).await?;
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
            let body = serde_json::json!({"uid": index, "primaryKey": "id"});
            let path = "/indexes";
            let (status, text) = self
                .raw(
                    reqwest::Method::POST,
                    path,
                    Some(body.to_string()),
                    Some("application/json"),
                )
                .await?;
            if status == reqwest::StatusCode::CONFLICT {
                return Ok(()); // 已存在
            }
            if !status.is_success() {
                return Err(crate::ScoutError::Backend(format!(
                    "{} {} -> {}: {}",
                    reqwest::Method::POST, path, status, text
                )));
            }
            // 过滤/排序/软删请求要求字段已配置 filterable/sortable，否则一律
            // 400（attribute not filterable）。settings 可提供这两个数组，
            // 强制并入 id 与 __soft_deleted（软删内部过滤依赖）。
            let mut filterable: Vec<&str> = vec!["id", "__soft_deleted"];
            let mut sortable: Vec<&str> = Vec::new();
            for (key, list) in [("filterableAttributes", &mut filterable), ("sortableAttributes", &mut sortable)] {
                if let Some(values) = settings.get(key).and_then(Value::as_array) {
                    for v in values {
                        if let Some(s) = v.as_str() {
                            if !list.contains(&s) {
                                list.push(s);
                            }
                        }
                    }
                }
            }
            let settings_body = serde_json::json!({
                "filterableAttributes": filterable,
                "sortableAttributes": sortable,
            });
            let _ = self
                .request(
                    reqwest::Method::PATCH,
                    &format!("/indexes/{}/settings", percent_encode(index)),
                    Some(settings_body),
                )
                .await?;
            Ok(())
        })
    }

    fn delete_index<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let path = format!("/indexes/{}", percent_encode(index));
            let (status, text) = self.raw(reqwest::Method::DELETE, &path, None, None).await?;
            if status == reqwest::StatusCode::NOT_FOUND {
                return Ok(()); // 不存在视为成功
            }
            if !status.is_success() {
                return Err(crate::ScoutError::Backend(format!(
                    "{} {} -> {}: {}",
                    reqwest::Method::DELETE, path, status, text
                )));
            }
            Ok(())
        })
    }

    fn update_bulk<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            let mut groups: std::collections::HashMap<&str, Vec<Value>> = Default::default();
            for doc in docs {
                groups
                    .entry(doc.index.as_deref().unwrap_or("default"))
                    .or_default()
                    .push(Self::doc_to_document(doc));
            }
            for (index, docs) in groups {
                crate::validate_index_name(index)?;
                let path = format!("/indexes/{}/documents?primaryKey=id", percent_encode(index));
                let _ = self
                    .request(reqwest::Method::POST, &path, Some(Value::Array(docs)))
                    .await?;
            }
            Ok(())
        })
    }

    fn delete_bulk<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        // 与 delete_in 相同的 delete-batch 端点。
        self.delete_in(index, ids)
    }

    fn soft_delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            // Meilisearch 的 add-or-replace 是整体替换，无法只更新单字段：
            // 先按 id 搜出原文档再整体写回打标版本（搜不到则跳过）。
            for id in ids {
                let builder =
                    SearchBuilder::new("").within("default").where_field("id", id.clone());
                let result = self.search(&builder).await?;
                let Some(hit) = result.hits.first() else {
                    continue;
                };
                let mut fields: Map<String, Value> = match &hit.source {
                    Value::Object(map) => map
                        .iter()
                        .filter(|(k, _)| !k.starts_with('_')) // 丢弃 _rankingScore 等元数据
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    _ => Map::new(),
                };
                fields.insert("__soft_deleted".into(), Value::Bool(true));
                let doc = SearchDocument {
                    id: id.clone(),
                    index: None,
                    fields,
                };
                self.update(std::slice::from_ref(&doc)).await?;
            }
            Ok(())
        })
    }
    // reindex：trait 默认 Unsupported（Meilisearch 无原生端点）。
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SearchBuilder;

    #[test]
    fn filter_equality_and_in_clauses() {
        let builder = SearchBuilder::new("")
            .where_field("title", "a\"b\\c")
            .where_field("count", 5)
            .where_field("active", true)
            .where_in("tag", ["x", "y"])
            .where_not_in("tag", ["z"]);
        let filter = MeilisearchEngine::build_filter(&builder).unwrap();
        assert_eq!(
            filter,
            r#"title="a\"b\\c" AND count=5 AND active=true AND tag IN ["x", "y"] AND tag NOT IN ["z"] AND NOT __soft_deleted = true"#
        );
    }

    #[test]
    fn filter_trashed_variants() {
        assert_eq!(
            MeilisearchEngine::build_filter(&SearchBuilder::new("")).unwrap(),
            "NOT __soft_deleted = true"
        );
        assert_eq!(
            MeilisearchEngine::build_filter(&SearchBuilder::new("").only_trashed()).unwrap(),
            "__soft_deleted=true"
        );
        assert_eq!(
            MeilisearchEngine::build_filter(&SearchBuilder::new("").with_trashed()),
            None
        );
    }

    #[test]
    fn search_body_params() {
        let builder = SearchBuilder::new("hello")
            .order_by("price", true)
            .order_by("name", false)
            .with_trashed();
        let body = MeilisearchEngine::search_body(&builder, 3, 10);
        assert_eq!(body["q"], "hello");
        assert_eq!(body["hitsPerPage"], 10);
        assert_eq!(body["page"], 3);
        assert_eq!(body["sort"], serde_json::json!(["price:desc", "name:asc"]));
        assert!(body.get("filter").is_none());
    }

    #[test]
    fn search_body_includes_filter_when_trashed_excludes() {
        let builder = SearchBuilder::new("x").where_field("cat", 1);
        let body = MeilisearchEngine::search_body(&builder, 1, 10);
        assert_eq!(
            body["filter"],
            "cat=1 AND NOT __soft_deleted = true"
        );
    }

    #[test]
    fn parse_response_extracts_hits_total_score() {
        let raw = serde_json::json!({
            "hits": [
                {"id": "1", "title": "a", "_rankingScore": 0.9},
                {"id": 2, "title": "b"}
            ],
            "totalHits": 42
        });
        let result = MeilisearchEngine::parse_search_response(&raw);
        assert_eq!(result.total, 42);
        assert_eq!(result.hits.len(), 2);
        assert_eq!(result.hits[0].id, "1");
        assert_eq!(result.hits[0].score, Some(0.9));
        assert_eq!(result.hits[0].source["title"], "a");
        assert_eq!(result.hits[1].id, "2"); // 数字 id 也转字符串
        assert_eq!(result.hits[1].score, None);
    }

    #[test]
    fn parse_response_total_falls_back_to_hits_len() {
        let raw = serde_json::json!({"hits": [{"id": "1"}]});
        assert_eq!(MeilisearchEngine::parse_search_response(&raw).total, 1);
        let empty = MeilisearchEngine::parse_search_response(&serde_json::json!({}));
        assert_eq!(empty.total, 0);
        assert!(empty.hits.is_empty());
    }
}
