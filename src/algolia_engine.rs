#![cfg(feature = "algolia")]

use serde_json::{Map, Value};

use crate::config::percent_encode;
use crate::engine::{Engine, EngineFuture};
use crate::{SearchBuilder, SearchDocument, SearchHit, SearchResult, TrashedFilter};

/// Algolia 引擎（标准端点 `https://{app_id}.algolia.net`）。文档主键为
/// `objectID` 保留键，软删除标记 `__soft_deleted` 布尔字段。
pub struct AlgoliaEngine {
    host: String,
    app_id: String,
    api_key: String,
    client: reqwest::Client,
}

impl AlgoliaEngine {
    pub fn new(app_id: String, api_key: String) -> Self {
        Self {
            host: format!("https://{}.algolia.net", app_id),
            app_id,
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
        request = request.header("X-Algolia-Application-Id", &self.app_id);
        request = request.header("X-Algolia-API-Key", &self.api_key);
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

    /// filter 值：字符串加引号（转义 `\` 与 `"`），数字/bool 裸值。
    fn filter_value(v: &Value) -> String {
        match v {
            Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            other => other.to_string(),
        }
    }

    /// 等值/IN/软删除 → Algolia filters 表达式；多条件用逗号（AND 语义）。
    /// 注意与 PHP 版的差异：IN 用括号 `(field: v1 OR v2)`，NOT IN 加 NOT 前缀，
    /// 避免 OR 吞掉逗号连接的其它条件。
    fn build_filters(builder: &SearchBuilder) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        for w in &builder.wheres {
            parts.push(format!("{}={}", w.field, Self::filter_value(&w.value)));
        }
        for (field, values) in &builder.where_ins {
            if values.is_empty() {
                continue; // 空 IN 集合 = 不匹配任何，由 search/paginate 短路
            }
            let ors = values.iter().map(Self::filter_value).collect::<Vec<_>>().join(" OR ");
            parts.push(format!("({}: {})", field, ors));
        }
        for (field, values) in &builder.where_not_ins {
            if values.is_empty() {
                continue; // 空 NOT IN 集合 = 无过滤（Collection 语义）
            }
            let ors = values.iter().map(Self::filter_value).collect::<Vec<_>>().join(" OR ");
            parts.push(format!("NOT ({}: {})", field, ors));
        }
        match builder.trashed {
            TrashedFilter::Exclude => parts.push("NOT __soft_deleted:true".to_string()),
            TrashedFilter::OnlyTrashed => parts.push("__soft_deleted:true".to_string()),
            TrashedFilter::WithTrashed => {}
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(","))
        }
    }

    fn search_body(builder: &SearchBuilder, page: usize, per_page: usize) -> Value {
        let mut body = Map::new();
        body.insert("query".into(), Value::String(builder.query.clone()));
        body.insert("page".into(), Value::from(page)); // Algolia page 是 0 基
        body.insert("hitsPerPage".into(), Value::from(per_page));
        if let Some(filters) = Self::build_filters(builder) {
            body.insert("filters".into(), Value::String(filters));
        }
        // order_by 不生效：Algolia 排序需预建 replica index 并在查询时用
        // `replicas` 参数——Rust 侧未做，避免静默假装支持（注释留档）。
        Value::Object(body)
    }

    /// 文档 → Algolia record；`objectID` 保留键统一注入/覆盖为 doc.id。
    fn doc_to_record(doc: &SearchDocument) -> Value {
        let mut fields = doc.fields.clone();
        fields.insert("objectID".into(), Value::String(doc.id.clone()));
        Value::Object(fields)
    }

    /// 响应解析：id 取 `hits[i].objectID`，source 取整个 hit（含 _highlightResult
    /// 等 Algolia 元数据），total 取 `nbHits`。
    fn parse_search_response(raw: &Value) -> SearchResult {
        let hits = raw.get("hits").and_then(Value::as_array);
        let total = raw
            .get("nbHits")
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
        let object_id = hit.get("objectID")?;
        let id = object_id
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| object_id.to_string());
        Some(SearchHit {
            id,
            score: None,
            source: hit.clone(),
            highlight: None,
        })
    }
}

impl AlgoliaEngine {
    /// batch 端点；requests 为空则跳过。
    async fn batch(&self, index: &str, requests: Vec<Value>) -> crate::Result<()> {
        if requests.is_empty() {
            return Ok(());
        }
        crate::validate_index_name(index)?;
        let path = format!("/1/indexes/{}/batch", percent_encode(index));
        let body = serde_json::json!({"requests": requests});
        let _ = self.request(reqwest::Method::POST, &path, Some(body)).await?;
        Ok(())
    }
}

impl Engine for AlgoliaEngine {
    fn update<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        // 与 update_bulk 相同：batch addObject。
        self.update_bulk(docs)
    }

    fn delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        // 无索引信息：仅作用于 default 索引（同 ElasticsearchEngine 语义）。
        self.delete_in("default", ids)
    }

    fn delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            let requests: Vec<Value> = ids
                .iter()
                .map(|id| {
                    serde_json::json!({"action": "deleteObject", "body": {"objectID": id}})
                })
                .collect();
            self.batch(index, requests).await
        })
    }

    fn search<'a>(&'a self, builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult> {
        let per_page = builder.take.unwrap_or(10).max(1);
        let page = builder.skip.unwrap_or(0) / per_page;
        // 空 where_in 集合 = 不匹配任何（Collection 语义）：短路空结果。
        if builder.where_ins.iter().any(|(_, v)| v.is_empty()) {
            return Box::pin(async move { Ok(SearchResult::default()) });
        }
        Box::pin(async move {
            let index = builder.index.as_deref().unwrap_or("default");
            crate::validate_index_name(index)?;
            let body = Self::search_body(builder, page, per_page);
            let path = format!("/1/indexes/{}/query", percent_encode(index));
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
        let page = page.max(1) - 1; // Algolia page 是 0 基
        let per_page = per_page.max(1);
        // 空 where_in 集合 = 不匹配任何（Collection 语义）：短路空结果。
        if builder.where_ins.iter().any(|(_, v)| v.is_empty()) {
            return Box::pin(async move { Ok(SearchResult::default()) });
        }
        Box::pin(async move {
            let index = builder.index.as_deref().unwrap_or("default");
            crate::validate_index_name(index)?;
            let body = Self::search_body(builder, page, per_page);
            let path = format!("/1/indexes/{}/query", percent_encode(index));
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
            let path = format!("/1/indexes/{}/clear", percent_encode(index));
            let _ = self.request(reqwest::Method::POST, &path, None).await?;
            Ok(())
        })
    }

    fn create_index<'a>(
        &'a self,
        index: &'a str,
        _settings: serde_json::Value,
    ) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let settings_path = format!("/1/indexes/{}/settings", percent_encode(index));
            let (status, text) = self
                .raw(
                    reqwest::Method::PUT,
                    &settings_path,
                    Some("{}".to_string()),
                    Some("application/json"),
                )
                .await?;
            if status == reqwest::StatusCode::NOT_FOUND {
                // Algolia 索引首次写入时自动创建：先 PUT 空索引再重设 settings。
                let create_path = format!("/1/indexes/{}", percent_encode(index));
                let (status, text) =
                    self.raw(reqwest::Method::PUT, &create_path, None, None).await?;
                if !status.is_success() {
                    return Err(crate::ScoutError::Backend(format!(
                        "{} {} -> {}: {}",
                        reqwest::Method::PUT, create_path, status, text
                    )));
                }
                let (status, text) = self
                    .raw(
                        reqwest::Method::PUT,
                        &settings_path,
                        Some("{}".to_string()),
                        Some("application/json"),
                    )
                    .await?;
                if !status.is_success() {
                    return Err(crate::ScoutError::Backend(format!(
                        "{} {} -> {}: {}",
                        reqwest::Method::PUT, settings_path, status, text
                    )));
                }
            } else if !status.is_success() {
                return Err(crate::ScoutError::Backend(format!(
                    "{} {} -> {}: {}",
                    reqwest::Method::PUT, settings_path, status, text
                )));
            }
            Ok(())
        })
    }

    fn delete_index<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let path = format!("/1/indexes/{}", percent_encode(index));
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
                    .push(Self::doc_to_record(doc));
            }
            for (index, records) in groups {
                let requests: Vec<Value> = records
                    .into_iter()
                    .map(|body| serde_json::json!({"action": "addObject", "body": body}))
                    .collect();
                self.batch(index, requests).await?;
            }
            Ok(())
        })
    }

    fn delete_bulk<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        // 与 delete_in 相同：batch deleteObject。
        self.delete_in(index, ids)
    }

    fn soft_delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            // partialUpdateObject 单请求原子部分更新，无需先读原文档。
            let requests: Vec<Value> = ids
                .iter()
                .map(|id| {
                    serde_json::json!({
                        "action": "partialUpdateObject",
                        "body": {"objectID": id, "__soft_deleted": true}
                    })
                })
                .collect();
            self.batch("default", requests).await
        })
    }

    fn reindex<'a>(&'a self, from: &'a str, to: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(from)?;
            crate::validate_index_name(to)?;
            let body = serde_json::json!({
                "operation": "copy",
                "destination": {"index": to}
            });
            let path = format!("/1/indexes/{}/operation", percent_encode(from));
            let _ = self.request(reqwest::Method::POST, &path, Some(body)).await?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SearchBuilder;

    #[test]
    fn filters_join_with_comma_and_or_groups() {
        let builder = SearchBuilder::new("")
            .where_field("active", true)
            .where_field("price", 10)
            .where_in("tag", ["a", "b"])
            .where_not_in("tag", ["x"]);
        let filters = AlgoliaEngine::build_filters(&builder).unwrap();
        assert_eq!(
            filters,
            r#"active=true,price=10,(tag: "a" OR "b"),NOT (tag: "x"),NOT __soft_deleted:true"#
        );
    }

    #[test]
    fn filters_trashed_variants() {
        assert_eq!(
            AlgoliaEngine::build_filters(&SearchBuilder::new("")).unwrap(),
            "NOT __soft_deleted:true"
        );
        assert_eq!(
            AlgoliaEngine::build_filters(&SearchBuilder::new("").only_trashed()).unwrap(),
            "__soft_deleted:true"
        );
        assert_eq!(
            AlgoliaEngine::build_filters(&SearchBuilder::new("").with_trashed()),
            None
        );
    }

    #[test]
    fn doc_uses_object_id_key() {
        let doc =
            SearchDocument::new("a", serde_json::json!({"objectID": "wrong", "title": "x"}))
                .unwrap();
        let record = AlgoliaEngine::doc_to_record(&doc);
        assert_eq!(record["objectID"], "a"); // 覆盖保留键
        assert_eq!(record["title"], "x");
    }

    #[test]
    fn search_body_zero_based_page_and_filters() {
        let builder = SearchBuilder::new("q").where_field("cat", 1);
        let body = AlgoliaEngine::search_body(&builder, 2, 10);
        assert_eq!(body["query"], "q");
        assert_eq!(body["page"], 2); // 第 3 页 → page=2
        assert_eq!(body["hitsPerPage"], 10);
        assert_eq!(body["filters"], "cat=1,NOT __soft_deleted:true");
    }

    #[test]
    fn parse_response_uses_object_id_and_nb_hits() {
        let raw = serde_json::json!({
            "hits": [{
                "objectID": "1",
                "title": "a",
                "_highlightResult": {"title": {"value": "<em>a</em>"}}
            }],
            "nbHits": 7
        });
        let result = AlgoliaEngine::parse_search_response(&raw);
        assert_eq!(result.total, 7);
        assert_eq!(result.hits[0].id, "1");
        assert_eq!(result.hits[0].score, None);
        // Algolia 元数据原样保留在 source 中
        assert_eq!(
            result.hits[0].source["_highlightResult"]["title"]["value"],
            "<em>a</em>"
        );
    }

    #[test]
    fn parse_response_nb_hits_missing_falls_back() {
        let raw = serde_json::json!({"hits": [{"objectID": "1"}]});
        assert_eq!(AlgoliaEngine::parse_search_response(&raw).total, 1);
    }
}
