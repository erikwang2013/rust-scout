#![cfg(feature = "typesense")]

use serde_json::{Map, Value};

use crate::config::percent_encode;
use crate::engine::{Engine, EngineFuture};
use crate::typesense_query::{
    check_import, check_status, ndjson_payload, parse_search_response, search_params,
};
use crate::{SearchBuilder, SearchDocument, SearchResult};

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
            client: reqwest::Client::new(),
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
    async fn delete_ok(&self, path: &str) -> crate::Result<()> {
        let (status, text) = self.raw(reqwest::Method::DELETE, path, None, None, None).await?;
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

    /// search 与 paginate 共用的 GET 搜索。
    async fn search_page(&self, builder: &SearchBuilder, page: usize, per_page: usize) -> crate::Result<SearchResult> {
        let index = builder.index.as_deref().unwrap_or("default");
        crate::validate_index_name(index)?;
        let params = search_params(builder, page, per_page);
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
            for id in ids {
                self.delete_ok(&format!(
                    "/collections/{}/documents/{}",
                    percent_encode(index),
                    percent_encode(id)
                ))
                .await?;
            }
            Ok(())
        })
    }
    fn search<'a>(&'a self, builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult> {
        let per_page = builder.take.unwrap_or(10).max(1);
        let page = builder.skip.unwrap_or(0) / per_page + 1;
        Box::pin(async move { self.search_page(builder, page, per_page).await })
    }
    fn paginate<'a>(
        &'a self,
        builder: &'a SearchBuilder,
        page: usize,
        per_page: usize,
    ) -> EngineFuture<'a, SearchResult> {
        let page = page.max(1);
        let per_page = per_page.max(1);
        Box::pin(async move { self.search_page(builder, page, per_page).await })
    }
    fn map_ids(&self, result: &SearchResult) -> Vec<String> {
        result.ids()
    }
    fn flush<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            // Typesense 无 _refresh 等价操作：直接删整集合清空（PHP 版同语义）。
            self.delete_ok(&format!("/collections/{}", percent_encode(index))).await
        })
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
            self.delete_ok(&format!("/collections/{}", percent_encode(index))).await
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
        // 与 delete_in 相同：逐条 DELETE（Typesense 无批量删除端点）。
        self.delete_in(index, ids)
    }
    fn soft_delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            // Typesense 无部分更新：先按 id 搜出原文档，再整体 upsert 打标版本
            // （搜不到则跳过）。
            for id in ids {
                let builder = SearchBuilder::new("").within("default").where_field("id", id.clone());
                let result = self.search(&builder).await?;
                let Some(hit) = result.hits.first() else {
                    continue;
                };
                let mut fields: Map<String, Value> = match &hit.source {
                    Value::Object(map) => map.clone(),
                    _ => Map::new(),
                };
                fields.insert("__soft_deleted".into(), Value::Bool(true));
                let doc = SearchDocument {
                    id: id.clone(),
                    index: None,
                    fields,
                };
                self.import_docs("default", &[&doc]).await?;
            }
            Ok(())
        })
    }
    // reindex：trait 默认 Unsupported（Typesense 无原生端点）。
}
