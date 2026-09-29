#![cfg(feature = "meilisearch")]

use std::time::Duration;

use serde_json::{Map, Value};

use crate::config::percent_encode;
use crate::engine::{Engine, EngineFuture};
use crate::{SearchBuilder, SearchDocument, SearchResult};

/// 分页模式。**两种模式的总数语义不同**，这是选它的唯一理由：
///
/// - `Page`：`page`/`hitsPerPage`。Meilisearch 只在这个模式下返回**穷尽**的
///   `totalHits`，与 Collection/ES/Typesense/Algolia 的 `total` 一致。
/// - `Offset`：`offset`/`limit`。能表达任意起点（页对齐表达不了 `skip % take`
///   的余数），但 Meilisearch 此时只回 `estimatedTotalHits` —— 官方文档明确说它
///   不适合算精确页数，且受索引 `maxTotalHits`（默认 1000）封顶。
///
/// 所以：窗口页对齐时走 `Page`（`paginate` 永远对齐，普通 `search` 在
/// `skip` 是 `take` 整数倍时也对齐），只有真正页对不齐时才退回 `Offset`。
/// 两者互斥，Meilisearch 拒绝同时出现。
pub(crate) enum PageMode {
    Page(usize, usize),
    Offset(usize, usize),
}

/// 写入任务（`/tasks/{uid}`）的轮询间隔与总超时。超时即报错，不静默接受
/// 「结果未知」——官方 SDK 同样在超时时抛错。
const TASK_POLL_INTERVAL: Duration = Duration::from_millis(100);
const TASK_POLL_TIMEOUT: Duration = Duration::from_secs(30);

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
            // 必须给超时：reqwest 的默认是 `timeout: None` + `connect_timeout: None`，
            // 一个「接了 TCP 但不回包」的服务端会让调用永久挂住且无法取消。
            // redirect 限同源：Meili 用头传凭据，后端回 302 到别的 host 就把 key
            // 带出去了。builder 出错时退回默认 client —— `new()` 保持不会失败。
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .connect_timeout(Duration::from_secs(10))
                .redirect(crate::config::same_origin_redirect_policy())
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    /// 发送请求，返回状态码 + body 文本；网络错误经 `?` 转 `ScoutError::Http`。
    ///
    /// host 在这里校验：带着 `user:pass@` 的 host 一旦请求失败，reqwest 的错误
    /// `Display` 会把完整 URL（含密码）拼进日志。
    async fn raw(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<String>,
        content_type: Option<&str>,
    ) -> crate::Result<(reqwest::StatusCode, String)> {
        crate::validate_host(&self.host)?;
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

    /// 等待写入类请求的异步任务落地。
    ///
    /// `POST /documents`、`delete-batch` 都只回 `202 {taskUid, status:"enqueued"}`：
    /// 请求本身成功了，「一条都没写进去」这类失败（缺主键、字段类型不符）只出现在
    /// `GET /tasks/{uid}` 的 `"status":"failed"` 上。不读 `taskUid` 就等于把写入
    /// 失败当成功返回，而且永远没有别的地方能再看到它（`flush` 是 no-op）。
    /// 官方 SDK 同样是轮询任务直到终态。
    async fn await_task(&self, response: &Value) -> crate::Result<()> {
        let Some(uid) = response.get("taskUid").and_then(Value::as_u64) else {
            return Ok(()); // 旧版 Meilisearch 没有 taskUid：无从确认，按成功处理
        };
        let deadline = std::time::Instant::now() + TASK_POLL_TIMEOUT;
        loop {
            let task = self
                .request(reqwest::Method::GET, &format!("/tasks/{uid}"), None)
                .await?;
            match task.get("status").and_then(Value::as_str) {
                Some("succeeded") => return Ok(()),
                // canceled 同样是「没写进去」，不能算成功
                Some(status @ ("failed" | "canceled")) => {
                    let error = task.get("error").map(Value::to_string).unwrap_or_default();
                    return Err(crate::ScoutError::Backend(format!(
                        "写入任务 {uid} {status}: {error}"
                    )));
                }
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                // 超时不静默放行：此时写入结果未知，报错比谎报成功安全
                return Err(crate::ScoutError::Backend(format!(
                    "写入任务 {uid} 在 {}s 内未结束，写入结果未知",
                    TASK_POLL_TIMEOUT.as_secs()
                )));
            }
            tokio::time::sleep(TASK_POLL_INTERVAL).await;
        }
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
            let task = self
                .request(reqwest::Method::POST, &path, Some(Value::Array(ids)))
                .await?;
            self.await_task(&task).await?;
            Ok(())
        })
    }

    fn search<'a>(&'a self, builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult> {
        // 索引名校验必须在短路之前：否则 within("_all") 配一个空 where_in 会返回
        // Ok(空结果)，而其它驱动返回 InvalidIndexName —— 边界行为必须一致。
        let index = builder.index.as_deref().unwrap_or("default");
        if let Err(e) = crate::validate_index_name(index) {
            return Box::pin(async move { Err(e) });
        }
        // 空 where_in 集合 = 不匹配任何（Collection 语义）：短路空结果。
        if builder.where_ins.iter().any(|(_, v)| v.is_empty()) {
            return Box::pin(async move { Ok(SearchResult::default()) });
        }
        // take(0)：Collection/ES 的 total 是「取之前」的全量匹配数，take(0) 于是
        // 返回「命中总数 + 空 hits」。total 只能从后端拿，所以请求照发，但只取 1 条
        // （limit=0 在 Meilisearch 上语义不明），拿到 total 后把 hits 清空。
        let mode = Self::page_mode(builder);
        let want_none = builder.take == Some(0);
        Box::pin(async move {
            let body = Self::search_body(builder, &mode)?;
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
        // 索引名校验同样必须在短路之前：否则 within("_all") 配空 where_in 会返回
        // Ok(空结果)，与 search 的 InvalidIndexName 不一致——两个入口必须同行为。
        let index = builder.index.as_deref().unwrap_or("default");
        if let Err(e) = crate::validate_index_name(index) {
            return Box::pin(async move { Err(e) });
        }
        // 空 where_in 集合 = 不匹配任何（Collection 语义）：短路空结果。
        if builder.where_ins.iter().any(|(_, v)| v.is_empty()) {
            return Box::pin(async move { Ok(SearchResult::default()) });
        }
        // 页语义在本地折算成 offset：page N 与 Collection 的 (N-1)*per_page 对齐，
        // 所以 paginate 的结果与改 offset 之前逐条相同。
        // paginate 天然页对齐：走 Page 模式，total 是穷尽的 totalHits
        let mode = Self::page_mode_of(page, per_page);
        Box::pin(async move {
            let body = Self::search_body(builder, &mode)?;
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
                let task = self
                    .request(reqwest::Method::POST, &path, Some(Value::Array(docs)))
                    .await?;
                self.await_task(&task).await?;
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
                    // 必须显式带上索引：update_bulk 按 doc.index 分组，缺省是
                    // `default`。之前这里写 None —— 搜的是 `index`，写回却落在
                    // default：目标索引里的文档根本没被标软删（依旧可搜到），
                    // 同时给 default 塞了个幽灵副本、覆盖掉那儿的同 id 文档。
                    index: Some(index.to_string()),
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
#[path = "meilisearch_engine_tests.rs"]
mod tests;
