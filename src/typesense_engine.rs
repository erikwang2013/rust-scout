#![cfg(feature = "typesense")]

use serde_json::{Map, Value};

use crate::config::percent_encode;
use crate::engine::{Engine, EngineFuture};
use crate::typesense_query::{
    check_import, check_status, id_filter, ndjson_payload, offset_limit, page_offset,
    parse_search_response, search_params,
};
use crate::{SearchBuilder, SearchDocument, SearchResult};

/// `delete-by-query` 的分块：`limit` 上限 250，一次超过就会被后端拒绝。
const SOFT_DELETE_CHUNK: usize = 250;

/// 客户端：同源重定向 + 超时。
///
/// - 超时：reqwest 的 async 默认 `connect_timeout`/`timeout` 都是 `None`，
///   接得上 TCP 却不回包的 peer 会让请求永久挂起。
/// - 重定向：reqwest 换 host 时只摘 `Authorization`/`Cookie` 等 5 个已知头，
///   **自定义头不动**，`X-TYPESENSE-API-KEY` 会被 302 原样送到别的 host。
///   不用 `Policy::none()`：那连反向代理的同源跳转一起掐掉。
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .redirect(crate::config::same_origin_redirect_policy())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Typesense 引擎。文档主键为 `id` 字符串字段；写入走 NDJSON import
/// （upsert 语义），软删除标记 `__soft_deleted` 布尔字段。
/// filter_by 语法 / 响应解析等纯函数见 `typesense_query`。
pub struct TypesenseEngine {
    host: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl TypesenseEngine {
    pub fn new(host: String, api_key: Option<String>) -> Self {
        Self {
            host: host.trim_end_matches('/').to_string(),
            api_key,
            client: client(),
        }
    }

    /// 发送请求，返回状态码 + body 文本；网络错误经 `?` 转 `ScoutError::Http`。
    async fn raw(
        &self,
        method: reqwest::Method,
        path: &str,
        query: Option<&[(String, String)]>,
        body: Option<String>,
        content_type: Option<&str>,
    ) -> crate::Result<(reqwest::StatusCode, String)> {
        // 校验先于任何 I/O：带 userinfo 的 host 一旦请求失败，reqwest 的错误
        // Display 会把完整 URL（含密码）拼进日志。
        crate::validate_host(&self.host)?;
        let mut request = self.client.request(method.clone(), format!("{}{}", self.host, path));
        if let Some(api_key) = &self.api_key {
            request = request.header("X-TYPESENSE-API-KEY", api_key);
        }
        if let Some(query) = query {
            request = request.query(query);
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

    /// DELETE 请求；404 视为成功（文档/集合不存在）。
    async fn delete_ok(&self, path: &str, query: Option<&[(String, String)]>) -> crate::Result<()> {
        let (status, text) = self.raw(reqwest::Method::DELETE, path, query, None, None).await?;
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        check_status("DELETE", path, &status, &text)
    }

    /// 建集合（幂等，409 已存在视为成功）。`__soft_deleted` 需在 schema 中
    /// 声明为 optional bool，否则 filter_by 对未定义字段直接报错。
    async fn create_collection(&self, index: &str) -> crate::Result<()> {
        let path = format!("/collections/{}", percent_encode(index));
        let body = serde_json::json!({
            "name": index,
            "fields": [
                {"name": "id", "type": "string"},
                {"name": "__soft_deleted", "type": "bool", "optional": true}
            ]
        });
        let (status, text) = self
            .raw(reqwest::Method::PUT, &path, None, Some(body.to_string()), Some("application/json"))
            .await?;
        if status == reqwest::StatusCode::CONFLICT {
            return Ok(()); // 已存在
        }
        check_status("PUT", &path, &status, &text)
    }

    /// NDJSON import（upsert）；集合不存在（404）时先建集合再重试一次。
    async fn import_docs(&self, index: &str, docs: &[&SearchDocument]) -> crate::Result<()> {
        let payload = ndjson_payload(docs)?;
        let path = format!("/collections/{}/documents/import?action=upsert", percent_encode(index));
        let (status, text) = self
            .raw(reqwest::Method::POST, &path, None, Some(payload.clone()), Some("application/x-ndjson"))
            .await?;
        if status == reqwest::StatusCode::NOT_FOUND {
            self.create_collection(index).await?;
            let (status, text) = self
                .raw(reqwest::Method::POST, &path, None, Some(payload), Some("application/x-ndjson"))
                .await?;
            return check_import(&status, &path, &text);
        }
        check_import(&status, &path, &text)
    }

    /// search 与 paginate 共用的 GET 搜索（`offset`/`limit` 分页）。
    ///
    /// 空 where_in 集合 = 不匹配任何（Collection 语义）：放在这里短路，两个入口
    /// 都覆盖到，不会只在 search 上修好而 paginate 继续把 `field:=[]` 丢给后端。
    async fn search_page(&self, builder: &SearchBuilder, offset: usize, limit: usize) -> crate::Result<SearchResult> {
        let index = builder.index.as_deref().unwrap_or("default");
        // 校验先于短路：空 where_in 不该让非法索引名蒙混过关
        crate::validate_index_name(index)?;
        if builder.where_ins.iter().any(|(_, v)| v.is_empty()) {
            return Ok(SearchResult::default());
        }
        let params = search_params(builder, offset, limit)?;
        let path = format!("/collections/{}/documents/search", percent_encode(index));
        let (status, text) = self.raw(reqwest::Method::GET, &path, Some(&params), None, None).await?;
        check_status("GET", &path, &status, &text)?;
        Ok(parse_search_response(&serde_json::from_str::<Value>(&text)?))
    }
}

impl Engine for TypesenseEngine {
    fn update<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        // 与 update_bulk 相同：按索引分组一次 NDJSON import。
        self.update_bulk(docs)
    }
    fn delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        // 无索引信息：仅作用于 default 索引（同 ElasticsearchEngine 语义）。
        self.delete_in("default", ids)
    }
    fn delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            if ids.is_empty() {
                // 一次 DELETE 都不发：`id:=[]` 的语义由后端定，别赌它。
                return Ok(());
            }
            // delete-by-query 一次删完（`id` 默认索引）。返回 `num_deleted` 而不是
            // 逐条 404，所以「重复删除返回 Ok」的幂等语义不变；集合不存在仍是 404，
            // 由 delete_ok 吃掉。
            let path = format!("/collections/{}/documents", percent_encode(index));
            let query = vec![("filter_by".to_string(), id_filter(ids))];
            self.delete_ok(&path, Some(&query)).await
        })
    }
    fn search<'a>(&'a self, builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult> {
        // take(0)：Collection/ES 的 total 是「取之前」的全量匹配数，take(0) 于是
        // 返回「命中总数 + 空 hits」。total 只能从后端拿，所以请求照发，但只取 1 条
        // （limit=0 的语义后端不一），拿到 total 后把 hits 清空。
        // 只在 search 上判：paginate 的 take 一律由 per_page 覆盖（同 Collection）。
        let (offset, limit) = offset_limit(builder);
        let want_none = builder.take == Some(0);
        Box::pin(async move {
            let result = self.search_page(builder, offset, limit.max(1)).await?;
            Ok(if want_none { result.without_hits() } else { result })
        })
    }
    fn paginate<'a>(
        &'a self,
        builder: &'a SearchBuilder,
        page: usize,
        per_page: usize,
    ) -> EngineFuture<'a, SearchResult> {
        // 页语义在本地折算成 offset：page N 与 Collection 的 (N-1)*per_page 对齐。
        let (offset, per_page) = page_offset(page, per_page);
        Box::pin(async move { self.search_page(builder, offset, per_page).await })
    }
    fn create_index<'a>(
        &'a self,
        index: &'a str,
        _settings: serde_json::Value,
    ) -> EngineFuture<'a, ()> {
        // Typesense 集合随首次写入自动创建；这里做幂等建集合。
        Box::pin(async move {
            crate::validate_index_name(index)?;
            self.create_collection(index).await
        })
    }
    fn delete_index<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            self.delete_ok(&format!("/collections/{}", percent_encode(index)), None).await
        })
    }
    fn update_bulk<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            let mut groups: std::collections::HashMap<&str, Vec<&SearchDocument>> = Default::default();
            for doc in docs {
                groups.entry(doc.index.as_deref().unwrap_or("default")).or_default().push(doc);
            }
            for (index, docs) in groups {
                crate::validate_index_name(index)?;
                self.import_docs(index, &docs).await?;
            }
            Ok(())
        })
    }
    fn delete_bulk<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        // 与 delete_in 相同：delete-by-query 一次删完。
        self.delete_in(index, ids)
    }
    /// 仅作用于 `index`。Typesense 无部分更新：先按 id 搜出原文档，再整体
    /// upsert 打标版本（搜不到则跳过）。
    ///
    /// 读侧按 id 集合批量搜、写侧一次 import（原先每条 id 一次搜索 + 一次
    /// import = 2N 次往返）。批量搜的 `limit` 上限是 250，**超了会静默少标**，
    /// 所以按 `SOFT_DELETE_CHUNK` 分块；写侧本来就是批量。
    ///
    /// 不带索引的 [`Engine::soft_delete`] 在本驱动上不可用（见 `engine.rs` 说明）：
    /// 原先它硬编码 `default`，对写在其它索引里的文档会静默跳过却返回 Ok。
    fn soft_delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let mut docs: Vec<SearchDocument> = Vec::new();
            for chunk in ids.chunks(SOFT_DELETE_CHUNK) {
                // 默认的 Exclude 过滤（跳过已软删的 id），与 meilisearch 的
                // soft_delete_in 一致。读-改-写会把搜到的整份文档重新 import 一遍，
                // 搜索响应没返回的字段就此丢失；已软删的文档本就处于目标状态，
                // 再写一遍只是白白承担这份字段丢失风险，末尾状态却完全一样。
                let builder = SearchBuilder::new("")
                    .within(index)
                    .take(SOFT_DELETE_CHUNK)
                    .where_in("id", chunk.iter().map(String::as_str).collect::<Vec<_>>());
                let result = self.search(&builder).await?;
                for hit in result.hits {
                    let mut fields: Map<String, Value> = match &hit.source {
                        Value::Object(map) => map.clone(),
                        _ => Map::new(),
                    };
                    fields.insert("__soft_deleted".into(), Value::Bool(true));
                    docs.push(SearchDocument {
                        id: hit.id,
                        index: None,
                        fields,
                    });
                }
            }
            if docs.is_empty() {
                return Ok(()); // 全都没搜到：别发一次空的 import
            }
            let refs: Vec<&SearchDocument> = docs.iter().collect();
            self.import_docs(index, &refs).await
        })
    }
    // reindex：trait 默认 Unsupported（Typesense 无原生端点）。
}

#[cfg(test)]
#[path = "typesense_engine_tests.rs"]
mod tests;
