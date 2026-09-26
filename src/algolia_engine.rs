#![cfg(feature = "algolia")]

use serde_json::{Map, Value};

use crate::config::percent_encode;
use crate::engine::{Engine, EngineFuture};
use crate::{SearchBuilder, SearchDocument, SearchHit, SearchResult, TrashedFilter};

/// Algolia 引擎（标准端点 `https://{app_id}.algolia.net`）。文档主键为
/// `objectID` 保留键，软删除标记 `__soft_deleted` 布尔字段。
pub struct AlgoliaEngine {
    /// 校验通过的主机前缀；`app_id` 非法时为 `None`（见 [`Self::new`]，首次
    /// 请求由 [`Self::base_url`] 报错）。
    host: Option<String>,
    app_id: String,
    api_key: String,
    client: reqwest::Client,
}

impl AlgoliaEngine {
    /// `app_id` 会被直接拼进主机名，因此**只接受非空 ASCII 字母数字**：含
    /// `@`/`/`/`:` 等字符的值能把请求（连同 `X-Algolia-API-Key` 头）改道到别的
    /// 主机。签名是公共 API（`EngineManager` 依赖）不能返回 `Result`，非法值
    /// 存成 `None`，首次请求时返回 `ScoutError::Unsupported`，绝不发出去。
    pub fn new(app_id: String, api_key: String) -> Self {
        let host = Self::valid_app_id(&app_id).then(|| format!("https://{app_id}.algolia.net"));
        Self {
            host,
            app_id,
            api_key,
            client: reqwest::Client::new(),
        }
    }

    fn valid_app_id(app_id: &str) -> bool {
        !app_id.is_empty() && app_id.bytes().all(|b| b.is_ascii_alphanumeric())
    }

    /// 请求主机前缀；`app_id` 未通过校验时在此短路（所有请求都经过它）。
    fn base_url(&self) -> crate::Result<&str> {
        self.host.as_deref().ok_or_else(|| {
            crate::ScoutError::Unsupported(format!(
                "algolia: invalid app_id `{}`: 只允许非空 ASCII 字母数字",
                self.app_id
            ))
        })
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
            .request(method.clone(), format!("{}{}", self.base_url()?, path));
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

    /// 页 N（1 基，`paginate` 语义）→ Algolia 的 offset。
    fn page_offset(page: usize, per_page: usize) -> usize {
        page.max(1).saturating_sub(1).saturating_mul(per_page)
    }

    /// 请求体。用 `offset` + `length` 而不是 `page` + `hitsPerPage`：page 是页号，
    /// 会把 skip 向下取整到页边界（`skip(15).take(10)` 变成第 10..20 条），
    /// offset 才能精确命中第 15..25 条。两者互斥，只发 offset 一组；Algolia 的
    /// offset 与 length 成对出现才走偏移分页（只给 offset 可能退回页语义），
    /// 所以条数用 `length` 而不是 `hitsPerPage`。`nbHits`（总匹配数）不受影响。
    fn search_body(builder: &SearchBuilder, offset: usize, limit: usize) -> Value {
        let mut body = Map::new();
        body.insert("query".into(), Value::String(builder.query.clone()));
        body.insert("offset".into(), Value::from(offset));
        body.insert("length".into(), Value::from(limit));
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
        // 空 where_in 集合 = 不匹配任何（Collection 语义）：短路空结果。
        if builder.where_ins.iter().any(|(_, v)| v.is_empty()) {
            return Box::pin(async move { Ok(SearchResult::default()) });
        }
        // take(0) = 不要结果（Collection 语义）：Algolia 的 hitsPerPage 最小 1，
        // 原先把 take 钳到 1 会多返回一条。短路后 total 也是 0（CollectionEngine
        // 那里是「取之前」的全量匹配数）——take(0) 下没有结果可报，不值得为 total
        // 单发一次查询。
        if builder.take == Some(0) {
            return Box::pin(async move { Ok(SearchResult::default()) });
        }
        let offset = builder.skip.unwrap_or(0);
        let limit = builder.take.unwrap_or(10);
        Box::pin(async move {
            let index = builder.index.as_deref().unwrap_or("default");
            crate::validate_index_name(index)?;
            let body = Self::search_body(builder, offset, limit);
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
        let per_page = per_page.max(1);
        let offset = Self::page_offset(page, per_page); // 页 N → 第 (N-1)*per_page 条
        // 空 where_in 集合 = 不匹配任何（Collection 语义）：短路空结果。
        if builder.where_ins.iter().any(|(_, v)| v.is_empty()) {
            return Box::pin(async move { Ok(SearchResult::default()) });
        }
        Box::pin(async move {
            let index = builder.index.as_deref().unwrap_or("default");
            crate::validate_index_name(index)?;
            let body = Self::search_body(builder, offset, per_page);
            let path = format!("/1/indexes/{}/query", percent_encode(index));
            let raw = self.request(reqwest::Method::POST, &path, Some(body)).await?;
            Ok(Self::parse_search_response(&raw))
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

    /// 仅作用于 `index`。`partialUpdateObject` 单请求原子部分更新，无需先读原文档。
    ///
    /// 不带索引的 [`Engine::soft_delete`] 在本驱动上不可用（见 `engine.rs` 说明）：
    /// 原先它硬编码 `default`，而且 `partialUpdateObject` 默认
    /// `createIfNotExists=true`——对写在其它索引里的文档，它会在 `default` 里
    /// **凭空造一条** `{objectID, __soft_deleted:true}` 幽灵记录。
    fn soft_delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let requests: Vec<Value> = ids
                .iter()
                .map(|id| {
                    serde_json::json!({
                        // NoCreate：目标不存在时不要凭空建记录。用
                        // partialUpdateObject 会在索引里留下幽灵行。
                        "action": "partialUpdateObjectNoCreate",
                        "body": {"objectID": id, "__soft_deleted": true}
                    })
                })
                .collect();
            self.batch(index, requests).await
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
    fn search_body_sends_offset_and_filters() {
        let builder = SearchBuilder::new("q").where_field("cat", 1);
        let body = AlgoliaEngine::search_body(&builder, 20, 10);
        assert_eq!(body["query"], "q");
        assert_eq!(body["offset"], 20);
        assert_eq!(body["length"], 10);
        // offset/length 与 page/hitsPerPage 互斥，只发偏移那一组
        assert!(body.get("page").is_none(), "page 会把 skip 取整到页边界");
        assert!(body.get("hitsPerPage").is_none());
        assert_eq!(body["filters"], "cat=1,NOT __soft_deleted:true");
    }

    #[test]
    fn search_body_honours_exact_skip_and_paginate_pages() {
        // `skip(15).take(10)` 必须命中第 15..25 条：曾经发的是 page(=skip/per_page=1)，
        // 服务端按页返回第 10..20 条。
        let body = AlgoliaEngine::search_body(&SearchBuilder::new("q"), 15, 10);
        assert_eq!(body["offset"], 15);
        assert_eq!(body["length"], 10);
        // paginate 的页语义按 (N-1)*per_page 换算，仍然返回第 N 页
        assert_eq!(AlgoliaEngine::page_offset(1, 10), 0);
        assert_eq!(AlgoliaEngine::page_offset(2, 10), 10);
        assert_eq!(AlgoliaEngine::page_offset(3, 7), 14);
        assert_eq!(AlgoliaEngine::page_offset(0, 10), 0); // 第 0 页按第 1 页
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

    #[tokio::test]
    async fn flush_is_a_noop_and_makes_no_request() {
        // 指向必然连不上的 app_id：真的打网络就会失败，返回 Ok 即证明没有请求。
        // flush 的契约是「刷新可见性」，绝不能是清空索引——README 的生命周期示例
        // 在 update 与 search 之间调用它，而这里曾打 /clear 把索引清空。
        let engine = AlgoliaEngine::new("testappid".to_string(), "k".to_string());
        engine.flush("books").await.unwrap();
    }

    #[tokio::test]
    async fn take_zero_returns_nothing_without_a_request() {
        // take(0) 的契约是「不要结果」（Collection 语义）。指向不存在的主机：
        // 真打网络必然失败，返回 Ok 即证明短路发生在请求之前（原先钳到 1 → 多一条）。
        let engine = AlgoliaEngine::new("testappid".to_string(), "k".to_string());
        let result = engine
            .search(&SearchBuilder::new("q").take(0))
            .await
            .unwrap();
        assert!(result.hits.is_empty());
        assert_eq!(result.total, 0);
    }

    #[tokio::test]
    async fn invalid_app_id_fails_before_sending_the_api_key() {
        // app_id 直接拼进主机名：`evil@attacker.com` 会把请求（连同
        // X-Algolia-API-Key 头）送到 attacker.com.algolia.net。校验必须先于任何
        // 网络调用失败，且错误要说清楚原因。
        let bad = AlgoliaEngine::new("evil@attacker.com".to_string(), "secret".to_string());
        let err = bad
            .search(&SearchBuilder::new("q"))
            .await
            .expect_err("非法 app_id 必须报错，而不是把 API key 发到别的主机");
        assert!(matches!(err, crate::ScoutError::Unsupported(_)), "got {err:?}");

        // 合法 app_id 照常拼主机，别把正常配置一起禁掉
        let ok = AlgoliaEngine::new("TESTAPPID".to_string(), "k".to_string());
        assert_eq!(ok.base_url().unwrap(), "https://TESTAPPID.algolia.net");
    }

    #[tokio::test]
    async fn index_less_soft_delete_is_refused_not_silently_skipped() {
        // 曾经它硬编码 default，且 partialUpdateObject 默认 createIfNotExists=true，
        // 会在 default 里凭空造一条幽灵记录。
        let engine = AlgoliaEngine::new("testappid".to_string(), "k".to_string());
        let err = engine
            .soft_delete(&["b1".to_string()])
            .await
            .expect_err("index-less soft_delete 必须报错，而不是静默 no-op");
        assert!(matches!(err, crate::ScoutError::Unsupported(_)), "got {err:?}");
    }
}
