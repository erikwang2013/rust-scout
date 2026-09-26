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
            if values.is_empty() {
                continue; // 空 NOT IN 集合 = 无过滤（Collection 语义，与 Algolia 一致）
            }
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

    /// `skip`/`take` → `offset`/`limit`。`skip` 精确映射到 `offset`，**不**折算成页：
    /// 页只能表达页对齐的起点，`.skip(15).take(10)` 会丢掉 `15 % 10` 的余数，
    /// 返回 10–19 而不是 Collection 的 15–24（客户端按偏移翻页会重复/漏行）。
    /// `take(0)` 由调用方短路，这里缺省 10。
    fn offset_limit(builder: &SearchBuilder) -> (usize, usize) {
        (builder.skip.unwrap_or(0), builder.take.unwrap_or(10))
    }

    /// `page`/`per_page` → `offset`/`limit`：page N 即 offset `(N-1)*per_page`，
    /// 与 `CollectionEngine::paginate` 的页语义一致。
    fn page_offset(page: usize, per_page: usize) -> (usize, usize) {
        let per_page = per_page.max(1);
        (
            page.max(1).saturating_sub(1).saturating_mul(per_page),
            per_page,
        )
    }

    /// 请求体。用 `offset`/`limit` 而非 `page`/`hitsPerPage`：后者只能表达页对齐的
    /// 起点，且 Meilisearch 拒绝 `offset` 与 `page` 同时出现。
    fn search_body(builder: &SearchBuilder, offset: usize, limit: usize) -> Value {
        let mut body = Map::new();
        body.insert("q".into(), Value::String(builder.query.clone()));
        if let Some(filter) = Self::build_filter(builder) {
            body.insert("filter".into(), Value::String(filter));
        }
        body.insert("limit".into(), Value::from(limit));
        body.insert("offset".into(), Value::from(offset));
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
    ///
    /// 用 `offset`/`limit` 搜索时 Meilisearch 只回 `estimatedTotalHits`（穷尽计数
    /// `totalHits` 需要 `page`/`hitsPerPage`，见 paginate 分支），故回退读它；
    /// 两者都缺失才回退 hits.len()，避免 total 退化成页大小。
    fn parse_search_response(raw: &Value) -> SearchResult {
        let hits = raw.get("hits").and_then(Value::as_array);
        let total = ["totalHits", "estimatedTotalHits"]
            .iter()
            .find_map(|key| raw.get(*key).and_then(Value::as_u64))
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
        // 空 where_in 集合 = 不匹配任何（Collection 语义）：短路空结果。
        if builder.where_ins.iter().any(|(_, v)| v.is_empty()) {
            return Box::pin(async move { Ok(SearchResult::default()) });
        }
        // take(0)：Collection/ES 的 total 是「取之前」的全量匹配数，take(0) 于是
        // 返回「命中总数 + 空 hits」。total 只能从后端拿，所以请求照发，但只取 1 条
        // （limit=0 在 Meilisearch 上语义不明），拿到 total 后把 hits 清空。
        let (offset, limit) = Self::offset_limit(builder);
        let want_none = builder.take == Some(0);
        Box::pin(async move {
            let index = builder.index.as_deref().unwrap_or("default");
            crate::validate_index_name(index)?;
            let body = Self::search_body(builder, offset, limit.max(1));
            let path = format!("/indexes/{}/search", percent_encode(index));
            let raw = self.request(reqwest::Method::POST, &path, Some(body)).await?;
            let result = Self::parse_search_response(&raw);
            Ok(if want_none { result.without_hits() } else { result })
        })
    }

    fn paginate<'a>(
        &'a self,
        builder: &'a SearchBuilder,
        page: usize,
        per_page: usize,
    ) -> EngineFuture<'a, SearchResult> {
        // 空 where_in 集合 = 不匹配任何（Collection 语义）：短路空结果。
        if builder.where_ins.iter().any(|(_, v)| v.is_empty()) {
            return Box::pin(async move { Ok(SearchResult::default()) });
        }
        // 页语义在本地折算成 offset：page N 与 Collection 的 (N-1)*per_page 对齐，
        // 所以 paginate 的结果与改 offset 之前逐条相同。
        let (offset, per_page) = Self::page_offset(page, per_page);
        Box::pin(async move {
            let index = builder.index.as_deref().unwrap_or("default");
            crate::validate_index_name(index)?;
            let body = Self::search_body(builder, offset, per_page);
            let path = format!("/indexes/{}/search", percent_encode(index));
            let raw = self.request(reqwest::Method::POST, &path, Some(body)).await?;
            Ok(Self::parse_search_response(&raw))
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

    /// 仅作用于 `index`。Meilisearch 的 add-or-replace 是整体替换，无法只更新
    /// 单字段：先按 id 搜出原文档再整体写回打标版本（搜不到则跳过）。
    ///
    /// 不带索引的 [`Engine::soft_delete`] 在本驱动上不可用（见 `engine.rs` 说明）：
    /// 原先它硬编码 `default`，对写在其它索引里的文档会静默跳过却返回 Ok。
    fn soft_delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            for id in ids {
                let builder =
                    SearchBuilder::new("").within(index).where_field("id", id.clone());
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
        assert_eq!(body["offset"], 3);
        assert_eq!(body["limit"], 10);
        // page/hitsPerPage 只能表达页对齐的起点（且 Meilisearch 禁止与 offset
        // 同用），改用 offset/limit 后不再出现。
        assert!(body.get("page").is_none());
        assert!(body.get("hitsPerPage").is_none());
        assert_eq!(body["sort"], serde_json::json!(["price:desc", "name:asc"]));
        assert!(body.get("filter").is_none());
    }

    #[test]
    fn offset_limit_keeps_skip_remainder() {
        // 回归：skip 曾按 skip/per_page+1 折算成页，15 % 10 的余数被丢掉，
        // .skip(15).take(10) 返回 10–19 而不是 Collection 的 15–24。
        assert_eq!(
            MeilisearchEngine::offset_limit(&SearchBuilder::new("").skip(15).take(10)),
            (15, 10)
        );
        assert_eq!(
            MeilisearchEngine::offset_limit(&SearchBuilder::new("").skip(25).take(7)),
            (25, 7)
        );
        assert_eq!(MeilisearchEngine::offset_limit(&SearchBuilder::new("")), (0, 10));
    }

    #[test]
    fn page_offset_matches_collection_page_semantics() {
        // paginate 的页语义不变：page N == offset (N-1)*per_page，参数钳到合法范围。
        assert_eq!(MeilisearchEngine::page_offset(1, 10), (0, 10));
        assert_eq!(MeilisearchEngine::page_offset(2, 10), (10, 10));
        assert_eq!(MeilisearchEngine::page_offset(0, 0), (0, 1));
        assert_eq!(MeilisearchEngine::page_offset(3, 0), (2, 1));
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
    fn parse_response_reads_estimated_total_hits() {
        // 回归：offset/limit 搜索只回 estimatedTotalHits，读到它之前 total 会
        // 退化成 hits.len()（页大小），分页 UI 的「共 N 条」就错了。
        let raw = serde_json::json!({"hits": [{"id": "1"}], "estimatedTotalHits": 471});
        let result = MeilisearchEngine::parse_search_response(&raw);
        assert_eq!(result.total, 471);
        assert_eq!(result.hits.len(), 1);
        // page/hitsPerPage 模式仍回 totalHits（穷尽计数），优先读它。
        let page_mode = serde_json::json!({
            "hits": [{"id": "1"}],
            "totalHits": 42,
            "estimatedTotalHits": 471
        });
        assert_eq!(MeilisearchEngine::parse_search_response(&page_mode).total, 42);
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

    #[tokio::test]
    async fn flush_is_a_noop_and_makes_no_request() {
        // 指向必然连不上的地址：真的打网络就会失败，返回 Ok 即证明没有请求。
        // flush 的契约是「刷新可见性」，绝不能是清空索引——README 的生命周期示例
        // 在 update 与 search 之间调用它，而这里曾打 delete-all 把索引删空。
        let engine = MeilisearchEngine::new("http://127.0.0.1:1".to_string(), None);
        engine.flush("books").await.unwrap();
    }

    #[tokio::test]
    async fn index_less_soft_delete_is_refused_not_silently_skipped() {
        // 曾经它硬编码 default：对写在别的索引里的文档静默跳过却返回 Ok。
        let engine = MeilisearchEngine::new("http://127.0.0.1:1".to_string(), None);
        let err = engine
            .soft_delete(&["b1".to_string()])
            .await
            .expect_err("index-less soft_delete 必须报错，而不是静默 no-op");
        assert!(matches!(err, crate::ScoutError::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn take_zero_truncates_but_keeps_total() {
        // take(0) 的契约（Collection/ES 基准）是「命中总数 + 空 hits」：total 在分页
        // 之前算出，不能因为不要 hits 就报 0。截断逻辑在 result.rs::without_hits，
        // 那里有独立单测；这里确认本驱动用它、且 limit 被钳到 1（limit=0 语义不明）。
        assert_eq!(MeilisearchEngine::offset_limit(&SearchBuilder::new("").take(0)), (0, 0));
        let r = SearchResult { total: 7, ..SearchResult::default() };
        assert_eq!(r.without_hits().total, 7);
    }

    #[tokio::test]
    async fn empty_where_in_short_circuits_search_and_paginate() {
        // 回归：空 where_in 集合 = 不匹配任何（Collection 语义），曾被拼成
        // `tag IN []` 丢给后端（行为由后端决定：报错或匹配全部）。
        let engine = MeilisearchEngine::new("http://127.0.0.1:1".to_string(), None);
        let builder = SearchBuilder::new("")
            .within("books")
            .where_in("tag", Vec::<&str>::new());
        let result = engine.search(&builder).await.unwrap();
        assert!(result.hits.is_empty());
        assert_eq!(result.total, 0);
        // paginate 走同一条短路，不能只在 search 上修。
        let paged = engine.paginate(&builder, 2, 10).await.unwrap();
        assert!(paged.hits.is_empty());
        assert_eq!(paged.total, 0);
    }
}
