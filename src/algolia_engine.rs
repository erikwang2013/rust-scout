#![cfg(feature = "algolia")]

use std::time::Duration;

use serde_json::{Map, Value};

use crate::config::percent_encode;
use crate::engine::{Engine, EngineFuture};
use crate::{SearchBuilder, SearchDocument, SearchHit, SearchResult, TrashedFilter};

/// 写入任务（`GET /1/indexes/{index}/task/{taskID}`）的轮询间隔与总超时。超时即
/// 报错，不静默接受「结果未知」——官方 JS 客户端重试用尽时同样抛错。
/// 间隔取官方客户端的退避下界（`min(retry * 200, 5000)` 起步 200ms），总时长按
/// 驱动侧的请求超时（30s）对齐。
const TASK_POLL_INTERVAL: Duration = Duration::from_millis(200);
const TASK_POLL_TIMEOUT: Duration = Duration::from_secs(30);

/// 客户端：同源重定向 + 超时。
///
/// - 超时：reqwest 的 async 默认 `connect_timeout`/`timeout` 都是 `None`，
///   接得上 TCP 却不回包的 peer 会让请求永久挂起。
/// - 重定向：reqwest 换 host 时只摘 `Authorization`/`Cookie` 等 5 个已知头，
///   **自定义头不动**，`X-Algolia-API-Key`/`-Application-Id` 会被 302 原样送到
///   别的 host。不用 `Policy::none()`：那连反向代理的同源跳转一起掐掉。
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .redirect(crate::config::same_origin_redirect_policy())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

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
            client: client(),
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
        let base = self.base_url()?;
        // 校验先于任何 I/O：带 userinfo 的 host 一旦请求失败，reqwest 的错误
        // Display 会把完整 URL（含密码）拼进日志。
        crate::validate_host(base)?;
        let mut request = self.client.request(method.clone(), format!("{}{}", base, path));
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
    ///
    /// 字段名先过 [`crate::validate_field_name`]：表达式是文本拼接，值转了义而字段名
    /// 曾经裸拼，`where_field("x OR NOT x", ..)` 之类的值能把整条表达式改写。
    fn build_filters(builder: &SearchBuilder) -> crate::Result<Option<String>> {
        let mut parts: Vec<String> = Vec::new();
        for w in &builder.wheres {
            crate::validate_field_name(&w.field)?;
            parts.push(format!("{}={}", w.field, Self::filter_value(&w.value)));
        }
        for (field, values) in &builder.where_ins {
            if values.is_empty() {
                continue; // 空 IN 集合 = 不匹配任何，由 search/paginate 短路
            }
            crate::validate_field_name(field)?;
            let ors = values.iter().map(Self::filter_value).collect::<Vec<_>>().join(" OR ");
            parts.push(format!("({}: {})", field, ors));
        }
        for (field, values) in &builder.where_not_ins {
            if values.is_empty() {
                continue; // 空 NOT IN 集合 = 无过滤（Collection 语义）
            }
            crate::validate_field_name(field)?;
            let ors = values.iter().map(Self::filter_value).collect::<Vec<_>>().join(" OR ");
            parts.push(format!("NOT ({}: {})", field, ors));
        }
        match builder.trashed {
            TrashedFilter::Exclude => parts.push("NOT __soft_deleted:true".to_string()),
            TrashedFilter::OnlyTrashed => parts.push("__soft_deleted:true".to_string()),
            TrashedFilter::WithTrashed => {}
        }
        if parts.is_empty() {
            Ok(None)
        } else {
            Ok(Some(parts.join(",")))
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
    fn search_body(builder: &SearchBuilder, offset: usize, limit: usize) -> crate::Result<Value> {
        let mut body = Map::new();
        body.insert("query".into(), Value::String(builder.query.clone()));
        body.insert("offset".into(), Value::from(offset));
        body.insert("length".into(), Value::from(limit));
        if let Some(filters) = Self::build_filters(builder)? {
            body.insert("filters".into(), Value::String(filters));
        }
        // order_by 不生效：Algolia 排序需预建 replica index 并在查询时用
        // `replicas` 参数——Rust 侧未做，避免静默假装支持（注释留档）。
        Ok(Value::Object(body))
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
    /// `copy` 操作体。`destination` 是**索引名字符串**（官方 JS 客户端
    /// `operationIndex` 的形状）：写成 `{"index": to}` 对象每次都是 400/422，
    /// README 却把 `reindex` 宣传成所有驱动通用。
    fn reindex_body(to: &str) -> Value {
        serde_json::json!({"operation": "copy", "destination": to})
    }

    /// 等待写入任务发布。
    ///
    /// `batch`（写入与删除）与 `operation`（reindex）都只回 `{taskID, ...}`：
    /// **请求被受理不等于记录已可搜索**，任务发布前索引里看不到它，任务级失败也
    /// 只能在任务终态上看到。不读 taskID 就等于把「已入队」当「已写入」，而且驱动
    /// 里再没有别的地方能看到它（`flush` 是 no-op）。官方 JS 客户端
    /// （`searchClient.waitForTask`）同样轮询到终态，`chunkedBatch`/`deleteObjects`
    /// 默认带 `waitForTasks`。
    ///
    /// 状态枚举见官方 OpenAPI（`common/responses/common.yml#/taskStatus`）：**只有
    /// `published` 与 `notPublished`**，后者是「尚未发布」而不是失败，所以它落在
    /// 继续轮询的一侧而不是错误分支；任务真出不来只会表现为迟迟到不了 `published`，
    /// 由超时报错兜住。响应里没有 `taskID`（旧接口或中间代理）时无从确认，按成功处理。
    async fn await_task(&self, index: &str, response: &Value) -> crate::Result<()> {
        let Some(task_id) = response.get("taskID").and_then(Value::as_u64) else {
            return Ok(());
        };
        let path = format!("/1/indexes/{}/task/{}", percent_encode(index), task_id);
        let deadline = std::time::Instant::now() + TASK_POLL_TIMEOUT;
        loop {
            let task = self.request(reqwest::Method::GET, &path, None).await?;
            if task.get("status").and_then(Value::as_str) == Some("published") {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                // 超时不静默放行：写入结果未知，报错比谎报成功安全
                return Err(crate::ScoutError::Backend(format!(
                    "写入任务 {task_id} 在 {}s 内未 published，写入结果未知",
                    TASK_POLL_TIMEOUT.as_secs()
                )));
            }
            // 用 tokio 的定时器：async 里 `thread::sleep` 会占死一个运行时工作线程
            tokio::time::sleep(TASK_POLL_INTERVAL).await;
        }
    }

    /// batch 端点；requests 为空则跳过。响应带 `taskID`，等它发布才算写完。
    ///
    /// 校验排在空列表短路之前：`delete_in`/`delete_bulk`/`soft_delete_in` 都从这里
    /// 下发，短路在前时 `delete_in("_all", &[])` 返回 Ok，而其余驱动返回
    /// InvalidIndexName —— 同一个输入两种答案。
    async fn batch(&self, index: &str, requests: Vec<Value>) -> crate::Result<()> {
        crate::validate_index_name(index)?;
        if requests.is_empty() {
            return Ok(());
        }
        let path = format!("/1/indexes/{}/batch", percent_encode(index));
        let body = serde_json::json!({"requests": requests});
        let task = self.request(reqwest::Method::POST, &path, Some(body)).await?;
        self.await_task(index, &task).await
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
        // 索引名校验先于短路，理由同 meilisearch
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
        // （Algolia 的 length 最小 1），拿到 total 后把 hits 清空。
        let offset = builder.skip.unwrap_or(0);
        let limit = builder.take.unwrap_or(10);
        let want_none = builder.take == Some(0);
        Box::pin(async move {
            let body = Self::search_body(builder, offset, limit.max(1))?;
            let path = format!("/1/indexes/{}/query", percent_encode(index));
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
        let per_page = per_page.max(1);
        let offset = Self::page_offset(page, per_page); // 页 N → 第 (N-1)*per_page 条
        // 索引名校验先于短路，理由同 search：否则 within("_all") 配空 where_in 会
        // 返回 Ok(空结果) 而不是 InvalidIndexName —— 两个入口必须同行为。
        let index = builder.index.as_deref().unwrap_or("default");
        if let Err(e) = crate::validate_index_name(index) {
            return Box::pin(async move { Err(e) });
        }
        // 空 where_in 集合 = 不匹配任何（Collection 语义）：短路空结果。
        if builder.where_ins.iter().any(|(_, v)| v.is_empty()) {
            return Box::pin(async move { Ok(SearchResult::default()) });
        }
        Box::pin(async move {
            let body = Self::search_body(builder, offset, per_page)?;
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
            // 校验全部索引名先于第一条请求：按组边校验边 batch 时，{合法索引,
            // 保留索引} 的一批会先把合法那组写进去再报错，调用方拿到 Err 时数据
            // 已经落了一半。`batch` 自己也校验（delete_in/soft_delete_in 依赖它），
            // 这里只为保证「任何一组非法 => 一个写入请求都不发」。
            for index in groups.keys() {
                crate::validate_index_name(index)?;
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
            let path = format!("/1/indexes/{}/operation", percent_encode(from));
            let task = self.request(reqwest::Method::POST, &path, Some(Self::reindex_body(to))).await?;
            // 复制同样是任务制，等它发布完目标索引才搜得到。任务挂在**目标**索引上
            // ——官方客户端 `replaceAllObjects` 里 copy 就是拿 destination 的名字
            // 去 `waitForTask` 的（`/1/indexes/{index}/task/{taskID}` 认索引名）。
            self.await_task(to, &task).await
        })
    }
}

#[cfg(test)]
#[path = "algolia_engine_tests.rs"]
mod tests;
