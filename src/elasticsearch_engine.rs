use std::time::Duration;

use crate::config::{percent_encode, same_origin_redirect_policy};
use crate::engine::{Engine, EngineFuture};
use crate::query::{
    build_body, check_bulk_delete_items, check_bulk_items, is_query_parse_error,
    parse_search_response,
};
use crate::{SearchBuilder, SearchDocument, SearchResult};

pub struct ElasticsearchEngine {
    host: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl ElasticsearchEngine {
    pub fn new(host: String, api_key: Option<String>) -> Self {
        // 异步 client 没有 blocking 版那种隐式超时，不显式设就可能是无限等待。
        // 重定向限同源：跨源跳转等于把请求（含 ApiKey）送到第三方地址；同源跳转
        // （补尾斜杠这类）照常跟随。builder 失败（TLS 后端异常）退回默认 client，
        // 构造函数保持不会失败。
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .redirect(same_origin_redirect_policy())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            host: host.trim_end_matches('/').to_string(),
            api_key,
            client,
        }
    }

    /// 发送请求并返回状态码 + body 文本；失败时 body 尽力读取（读不到则为空串）。
    async fn raw_request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<String>,
        content_type: Option<&str>,
    ) -> crate::Result<(reqwest::StatusCode, String)> {
        // 所有 I/O 的唯一入口，校验放这里才拦得住：带 userinfo 的 host 一旦进了
        // reqwest，请求失败时它的错误 Display 会把密码原样拼进日志。
        crate::validate_host(&self.host)?;
        let mut request = self
            .client
            .request(method, format!("{}{}", self.host, path));
        if let Some(api_key) = &self.api_key {
            request = request.header("Authorization", format!("ApiKey {}", api_key));
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

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> crate::Result<serde_json::Value> {
        let content_type = body.as_ref().map(|_| "application/json");
        let (status, body) = self
            .raw_request(
                method.clone(),
                path,
                body.map(|b| b.to_string()),
                content_type,
            )
            .await?;
        if !status.is_success() {
            return Err(crate::ScoutError::Backend(format!(
                "{} {} -> {}: {}",
                method, path, status, body
            )));
        }
        // 成功路径解析 JSON；非 JSON 成功体（不应发生）走 Json 错误路径。
        Ok(serde_json::from_str(&body)?)
    }
}

impl ElasticsearchEngine {
    /// `_search` 请求。查询语法错误时 ES 会 400，而 CollectionEngine 对同样输入是 Ok
    /// （`lenient` 挡不住语法错误，见 [`crate::query::is_query_parse_error`]），
    /// 这里按「无命中」处理以对齐契约；其余非 2xx 照旧报错。
    async fn search_hits(
        &self,
        builder: &SearchBuilder,
        from: usize,
        size: usize,
    ) -> crate::Result<SearchResult> {
        let index = builder.index.as_deref().unwrap_or("default");
        crate::validate_index_name(index)?;
        let path = format!("/{}/_search", percent_encode(index));
        let (status, body) = self
            .raw_request(
                reqwest::Method::POST,
                &path,
                Some(build_body(builder, from, size).to_string()),
                Some("application/json"),
            )
            .await?;
        if status == reqwest::StatusCode::BAD_REQUEST && is_query_parse_error(&body) {
            return Ok(SearchResult::default());
        }
        if !status.is_success() {
            return Err(crate::ScoutError::Backend(format!(
                "{} {} -> {}: {}",
                reqwest::Method::POST, path, status, body
            )));
        }
        let parsed: serde_json::Value = serde_json::from_str(&body)?;
        // ES 默认 allow_partial_search_results=true：超时或分片失败时照样回 200，
        // 只是 hits 变少、total 缩水。这种「短成功」会静默给出错误的搜索结果，
        // 宁可报错也不能让调用方以为拿到的是全量。
        if parsed.get("timed_out").and_then(|v| v.as_bool()) == Some(true) {
            return Err(crate::ScoutError::Backend(format!(
                "POST {} -> 200: search timed out: {}",
                path, body
            )));
        }
        let failed_shards = parsed
            .pointer("/_shards/failed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if failed_shards > 0 {
            return Err(crate::ScoutError::Backend(format!(
                "POST {} -> 200: {} shard(s) failed: {}",
                path, failed_shards, body
            )));
        }
        Ok(parse_search_response(&parsed))
    }
}

impl Engine for ElasticsearchEngine {
    fn update<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        // 与 update_bulk 同语义（`index` 动作就是整篇重建），委托它按索引分组走 _bulk。
        self.update_bulk(docs)
    }

    fn delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        // 无索引信息：仅作用于 default 索引（与 v0.1.0 语义一致）；精确语义用 delete_in。
        self.delete_in("default", ids)
    }

    fn delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            for id in ids {
                let path = format!(
                    "/{}/_doc/{}",
                    percent_encode(index),
                    percent_encode(id)
                );
                let (status, body) = self
                    .raw_request(reqwest::Method::DELETE, &path, None, None)
                    .await?;
                // ES 对「文档不存在」和「索引不存在」都回 404；删除必须幂等，
                // 与 collection/database/typesense/meilisearch 保持一致。
                if status == reqwest::StatusCode::NOT_FOUND {
                    continue;
                }
                if !status.is_success() {
                    return Err(crate::ScoutError::Backend(format!(
                        "{} {} -> {}: {}",
                        reqwest::Method::DELETE, path, status, body
                    )));
                }
            }
            Ok(())
        })
    }

    fn search<'a>(&'a self, builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult> {
        Box::pin(async move {
            let from = builder.skip.unwrap_or(0);
            let size = builder.take.unwrap_or(10);
            self.search_hits(builder, from, size).await
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
            self.search_hits(&base, base.skip.unwrap_or(0), base.take.unwrap_or(10))
                .await
        })
    }

    fn flush<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let path = format!("/{}/_refresh", percent_encode(index));
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
            let path = format!("/{}", percent_encode(index));
            let _ = self
                .request(reqwest::Method::PUT, &path, Some(settings))
                .await?;
            Ok(())
        })
    }

    fn delete_index<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let path = format!("/{}", percent_encode(index));
            let _ = self.request(reqwest::Method::DELETE, &path, None).await?;
            Ok(())
        })
    }

    fn update_bulk<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            // 按 index 分组，每组一次 _bulk 请求（NDJSON）。
            let mut groups: std::collections::HashMap<&str, Vec<&SearchDocument>> =
                Default::default();
            for doc in docs {
                let index = doc.index.as_deref().unwrap_or("default");
                groups.entry(index).or_default().push(doc);
            }
            for (index, docs) in groups {
                crate::validate_index_name(index)?;
                let mut body = String::new();
                for doc in docs {
                    body.push_str(&format!(
                        "{{\"index\":{{\"_id\":{}}}}}\n{}\n",
                        serde_json::to_string(&doc.id)?,
                        serde_json::to_string(&doc.fields)?
                    ));
                }
                let path = format!("/{}/_bulk", percent_encode(index));
                let (status, body) = self
                    .raw_request(
                        reqwest::Method::POST,
                        &path,
                        Some(body),
                        Some("application/x-ndjson"),
                    )
                    .await?;
                if !status.is_success() {
                    return Err(crate::ScoutError::Backend(format!(
                        "{} {} -> {}: {}",
                        reqwest::Method::POST, path, status, body
                    )));
                }
                check_bulk_items(&serde_json::from_str(&body)?)?;
            }
            Ok(())
        })
    }

    fn delete_bulk<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            if ids.is_empty() {
                return Ok(());
            }
            let mut body = String::new();
            for id in ids {
                body.push_str(&format!(
                    "{{\"delete\":{{\"_id\":{}}}}}\n",
                    serde_json::to_string(id)?
                ));
            }
            let path = format!("/{}/_bulk", percent_encode(index));
            let (status, body) = self
                .raw_request(
                    reqwest::Method::POST,
                    &path,
                    Some(body),
                    Some("application/x-ndjson"),
                )
                .await?;
            if !status.is_success() {
                return Err(crate::ScoutError::Backend(format!(
                    "{} {} -> {}: {}",
                    reqwest::Method::POST, path, status, body
                )));
            }
            // 删除语义：404（文档或索引不存在）视为成功，保持幂等。
            check_bulk_delete_items(&serde_json::from_str(&body)?)
        })
    }

    /// 仅作用于 `index`。_bulk 的 `update` 动作是同样的原子部分更新（不读改写），
    /// 所有 id 一次请求打完；文档/索引不存在的 404 由
    /// 底层的 `check_bulk_delete_items` 逐条放行，幂等语义与逐条版本一致。
    ///
    /// 不带索引的 [`Engine::soft_delete`] 在本驱动上不可用（见 `engine.rs` 说明）：
    /// 原先它硬编码 `default`，对写在其它索引里的文档会静默什么都不做却返回 Ok。
    fn soft_delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            if ids.is_empty() {
                return Ok(());
            }
            let mut body = String::new();
            for id in ids {
                body.push_str(&format!(
                    "{{\"update\":{{\"_id\":{}}}}}\n{{\"doc\":{{\"__soft_deleted\":true}}}}\n",
                    serde_json::to_string(id)?
                ));
            }
            let path = format!("/{}/_bulk", percent_encode(index));
            let (status, body) = self
                .raw_request(
                    reqwest::Method::POST,
                    &path,
                    Some(body),
                    Some("application/x-ndjson"),
                )
                .await?;
            if !status.is_success() {
                return Err(crate::ScoutError::Backend(format!(
                    "{} {} -> {}: {}",
                    reqwest::Method::POST, path, status, body
                )));
            }
            check_bulk_delete_items(&serde_json::from_str(&body)?)
        })
    }

    fn reindex<'a>(&'a self, from: &'a str, to: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(from)?;
            crate::validate_index_name(to)?;
            let body = serde_json::json!({
                "source": {"index": from},
                "dest": {"index": to}
            });
            let _ = self
                .request(reqwest::Method::POST, "/_reindex", Some(body))
                .await?;
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "elasticsearch_engine_tests.rs"]
mod tests;
