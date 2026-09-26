use crate::config::percent_encode;
use crate::engine::{Engine, EngineFuture};
use crate::query::{
    build_body, check_bulk_delete_items, check_bulk_items, is_query_parse_error,
    parse_search_response,
};
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

impl ElasticsearchEngine {
    /// `_search` 请求。查询语法错误时 ES 会 400，而 CollectionEngine 对同样输入是 Ok
    /// （`lenient` 挡不住语法错误，见 [`crate::query::is_query_parse_error`]），
    /// 这里按「无命中」处理以对齐契约；其余非 2xx 照旧报错。
    fn search_hits(
        &self,
        builder: &SearchBuilder,
        from: usize,
        size: usize,
    ) -> crate::Result<SearchResult> {
        let index = builder.index.as_deref().unwrap_or("default");
        crate::validate_index_name(index)?;
        let path = format!("/{}/_search", percent_encode(index));
        let (status, body) = self.raw_request(
            reqwest::Method::POST,
            &path,
            Some(build_body(builder, from, size).to_string()),
            Some("application/json"),
        )?;
        if status == reqwest::StatusCode::BAD_REQUEST && is_query_parse_error(&body) {
            return Ok(SearchResult::default());
        }
        if !status.is_success() {
            return Err(crate::ScoutError::Backend(format!(
                "{} {} -> {}: {}",
                reqwest::Method::POST, path, status, body
            )));
        }
        Ok(parse_search_response(&serde_json::from_str(&body)?))
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
                let (status, body) =
                    self.raw_request(reqwest::Method::DELETE, &path, None, None)?;
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
            self.search_hits(builder, from, size)
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
        })
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
            // 删除语义：404（文档或索引不存在）视为成功，保持幂等。
            check_bulk_delete_items(&serde_json::from_str(&body)?)
        })
    }

    /// 仅作用于 `index`。POST `_update` 单请求原子部分更新（不再读改写）；
    /// 404（文档不存在，found:false）跳过。
    ///
    /// 不带索引的 [`Engine::soft_delete`] 在本驱动上不可用（见 `engine.rs` 说明）：
    /// 原先它硬编码 `default`，对写在其它索引里的文档会静默什么都不做却返回 Ok。
    fn soft_delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    // 指向必然连不上的地址：任何真的发出网络请求的方法都会失败，因此「返回 Ok」
    // 本身就证明它没有打网络。
    //
    // 用普通 `#[test]` 而非 `#[tokio::test]`：`reqwest::blocking::Client` 自带一个
    // runtime，在 async 上下文里构造或析构都会 panic（tokio blocking/shutdown.rs）。
    // 所以引擎的构造与释放都放在我们自己建的 runtime 之外。
    fn engine() -> ElasticsearchEngine {
        ElasticsearchEngine::new("http://127.0.0.1:1".to_string(), None)
    }

    // new_current_thread 而非 Runtime::new()：后者要 rt-multi-thread，而 dev-deps
    // 只开了 rt，默认 feature 构建下会编译不过。
    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().build().unwrap()
    }

    #[test]
    fn index_less_soft_delete_is_refused_not_silently_skipped() {
        // 曾经它硬编码 default：对写在别的索引里的文档静默成功却什么都没做。
        let engine = engine();
        let rt = rt();
        let err = rt
            .block_on(engine.soft_delete(&["b1".to_string()]))
            .expect_err("index-less soft_delete 必须报错，而不是静默 no-op");
        drop(engine);
        drop(rt);
        assert!(
            matches!(err, crate::ScoutError::Unsupported(_)),
            "expected Unsupported, got {err:?}"
        );
        assert!(err.to_string().contains("soft_delete_in"), "错误信息要指出正确用法");
    }

    // 手工 poll 而不借 tokio runtime：reqwest::blocking 在 debug 构建里会
    // `enter()` 一个临时 runtime（blocking/wait.rs），在已有 runtime 上下文里再
    // enter 会 panic（tokio runtime/context/runtime.rs）。引擎的 future 全是阻塞
    // 调用、首次 poll 就 Ready，所以不需要真正的调度器。
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        let mut fut = std::pin::pin!(fut);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        loop {
            if let std::task::Poll::Ready(output) = fut.as_mut().poll(&mut cx) {
                return output;
            }
            std::thread::yield_now();
        }
    }

    // 裸 TcpListener 冒充 ES：回一个固定状态码 + body，省掉 HTTP mock 依赖。
    fn stub_es(status: u16, body: &'static str) -> String {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let addr = listener.local_addr().expect("stub addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept stub");
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status} Stub\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        });
        format!("http://{addr}")
    }

    // 实测抓取：OpenSearch 2.19 对 query_string:"(" 的 400 响应（带 lenient:true 也一样）。
    const PARSE_ERROR_BODY: &str = r#"{"error":{"root_cause":[{"type":"query_shard_exception","reason":"Failed to parse query [(]","index":"verify"}],"type":"search_phase_execution_exception","reason":"all shards failed","phase":"query","failed_shards":[{"shard":0,"reason":{"type":"query_shard_exception","reason":"Failed to parse query [(]","caused_by":{"type":"parse_exception","reason":"Cannot parse '(': Encountered \"<EOF>\" at line 1, column 1."}}}]},"status":400}"#;

    #[test]
    fn search_returns_empty_result_on_malformed_query() {
        // lenient 挡不住 JavaCC 语法错误，ES 仍回 400；CollectionEngine 对同样输入
        // 是 Ok，所以这里必须返回空结果，而不是把 400 抛给业务代码。
        let engine = ElasticsearchEngine::new(stub_es(400, PARSE_ERROR_BODY), None);
        let result = block_on(engine.search(&SearchBuilder::new("(")));
        drop(engine);
        let result = result.expect("语法错误应当返回空结果而不是 Err");
        assert_eq!(result.total, 0);
        assert!(result.hits.is_empty());
    }

    #[test]
    fn search_still_fails_on_other_400() {
        // 非语法错的 400（参数非法/映射错误）不能被吞成空结果。
        let engine = ElasticsearchEngine::new(
            stub_es(
                400,
                r#"{"error":{"root_cause":[{"type":"illegal_argument_exception","reason":"bad"}],"type":"illegal_argument_exception"},"status":400}"#,
            ),
            None,
        );
        let result = block_on(engine.search(&SearchBuilder::new("x")));
        drop(engine);
        assert!(
            matches!(result, Err(crate::ScoutError::Backend(_))),
            "非语法错的 400 仍须上抛，得到 {result:?}"
        );
    }

    #[test]
    fn delete_in_treats_404_as_success() {
        // ES 对「文档不存在」和「索引不存在」都回 404。delete() 委托 delete_in，
        // 重复删除必须像 collection/database/typesense/meilisearch 一样返回 Ok。
        let engine = ElasticsearchEngine::new(stub_es(404, "{}"), None);
        let result = block_on(engine.delete_in("default", &["missing".to_string()]));
        drop(engine);
        assert!(result.is_ok(), "404 应当幂等成功，得到 {result:?}");
    }

    #[test]
    fn delete_in_still_fails_on_other_non_2xx() {
        let engine = ElasticsearchEngine::new(stub_es(500, "{}"), None);
        let result = block_on(engine.delete_in("default", &["x".to_string()]));
        drop(engine);
        assert!(
            matches!(result, Err(crate::ScoutError::Backend(_))),
            "非 404 的错误仍须上抛，得到 {result:?}"
        );
    }

    #[test]
    fn reserved_index_names_are_rejected() {
        // delete_index("_all") 在 ES 7.x / OpenSearch 默认配置下会删掉整个集群。
        let engine = engine();
        let rt = rt();
        let err = rt
            .block_on(engine.delete_index("_all"))
            .expect_err("_all 必须被拒绝");
        drop(engine);
        drop(rt);
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }
}
