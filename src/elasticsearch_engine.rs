use crate::config::percent_encode;
use crate::engine::{Engine, EngineFuture};
use crate::query::{build_body, check_bulk_items, parse_search_response};
use crate::{SearchBuilder, SearchDocument, SearchResult};

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

    /// 发送请求并返回状态码 + body 文本；失败时 body 尽力读取（读不到则为空串）。
    fn raw_request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<String>,
        content_type: Option<&str>,
    ) -> crate::Result<(reqwest::StatusCode, String)> {
        let mut request = self
            .client
            .request(method, &format!("{}{}", self.host, path));
        if let Some(api_key) = &self.api_key {
            request = request.header("Authorization", format!("ApiKey {}", api_key));
        }
        if let Some(body) = body {
            request = request.body(body);
            if let Some(content_type) = content_type {
                request = request.header(reqwest::header::CONTENT_TYPE, content_type);
            }
        }
        let response = request.send()?;
        let status = response.status();
        let body = response.text().unwrap_or_default();
        Ok((status, body))
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> crate::Result<serde_json::Value> {
        let content_type = body.as_ref().map(|_| "application/json");
        let (status, body) = self.raw_request(
            method.clone(),
            path,
            body.map(|b| b.to_string()),
            content_type,
        )?;
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
                let (status, body) = self.raw_request(
                    reqwest::Method::POST,
                    &path,
                    Some(body),
                    Some("application/x-ndjson"),
                )?;
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
            let (status, body) = self.raw_request(
                reqwest::Method::POST,
                &path,
                Some(body),
                Some("application/x-ndjson"),
            )?;
            if !status.is_success() {
                return Err(crate::ScoutError::Backend(format!(
                    "{} {} -> {}: {}",
                    reqwest::Method::POST, path, status, body
                )));
            }
            check_bulk_items(&serde_json::from_str(&body)?)
        })
    }

    fn soft_delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            // 与 delete() 一致：仅作用于 default 索引。POST _update 单请求原子
            // 部分更新（不再读改写）；404（文档不存在，found:false）跳过。
            let index = "default";
            for id in ids {
                let path = format!("/{}/_update/{}", percent_encode(index), percent_encode(id));
                let (status, body) = self.raw_request(
                    reqwest::Method::POST,
                    &path,
                    Some(serde_json::json!({"doc": {"__soft_deleted": true}}).to_string()),
                    Some("application/json"),
                )?;
                if status == reqwest::StatusCode::NOT_FOUND {
                    continue;
                }
                if !status.is_success() {
                    return Err(crate::ScoutError::Backend(format!(
                        "{} {} -> {}: {}",
                        reqwest::Method::POST, path, status, body
                    )));
                }
            }
            Ok(())
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
            let _ = self.request(reqwest::Method::POST, "/_reindex", Some(body))?;
            Ok(())
        })
    }
}
