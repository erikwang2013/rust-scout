    use super::*;

    // 指向必然连不上的地址：任何真的发出网络请求的方法都会失败，因此「返回 Ok」
    // 本身就证明它没有打网络。
    fn engine() -> ElasticsearchEngine {
        ElasticsearchEngine::new("http://127.0.0.1:1".to_string(), None)
    }

    #[tokio::test]
    async fn index_less_soft_delete_is_refused_not_silently_skipped() {
        // 曾经它硬编码 default：对写在别的索引里的文档静默成功却什么都没做。
        let engine = engine();
        let err = engine
            .soft_delete(&["b1".to_string()])
            .await
            .expect_err("index-less soft_delete 必须报错，而不是静默 no-op");
        assert!(
            matches!(err, crate::ScoutError::Unsupported(_)),
            "expected Unsupported, got {err:?}"
        );
        assert!(err.to_string().contains("soft_delete_in"), "错误信息要指出正确用法");
    }

    #[tokio::test]
    async fn driver_is_usable_from_inside_a_tokio_runtime() {
        // 回归：阻塞版 client 在 async 上下文里 send/text 会 enter 一个临时
        // runtime，析构时 tokio 直接 panic（blocking/shutdown.rs:51
        // "Cannot drop a runtime in a context where blocking is not allowed"）。
        // 整个驱动就是从这个上下文被调用的，所以这条必须过。
        let engine = ElasticsearchEngine::new(stub_es(200, r#"{"hits":{"hits":[]}}"#), None);
        let result = engine.search(&SearchBuilder::new("rust")).await;
        assert!(result.is_ok(), "tokio 上下文里调用不得 panic，得到 {result:?}");
    }

    // 读满一个请求（头 + Content-Length 声明的 body）：单次 read 常常只拿到头部，
    // 而断言 _bulk 的 NDJSON 内容需要完整 body。
    fn read_full_request(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;

        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        // 长度未知（头还没读全）时为 usize::MAX。读取必须排在「够不够」之后：
        // 客户端发完请求就不再发了，多读一次会一直阻塞到它自己超时。
        let mut end = usize::MAX;
        loop {
            if end == usize::MAX {
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                    let len: usize = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    end = i + 4 + len;
                }
            }
            if buf.len() >= end {
                break;
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    // 裸 TcpListener 冒充 ES：回一个固定状态码 + body，省掉 HTTP mock 依赖。
    // 只 accept 一个连接 —— 驱动若发了第二个请求，连接会被拒，测试随之报错。
    fn stub_es_capturing(
        status: u16,
        body: &'static str,
    ) -> (String, std::sync::Arc<std::sync::Mutex<String>>) {
        use std::io::Write;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let addr = listener.local_addr().expect("stub addr");
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = captured.clone();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept stub");
            let request = read_full_request(&mut stream);
            *sink.lock().unwrap() = request;
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status} Stub\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        });
        (format!("http://{addr}"), captured)
    }

    fn stub_es(status: u16, body: &'static str) -> String {
        stub_es_capturing(status, body).0
    }

    // 实测抓取：OpenSearch 2.19 对 query_string:"(" 的 400 响应（带 lenient:true 也一样）。
    const PARSE_ERROR_BODY: &str = r#"{"error":{"root_cause":[{"type":"query_shard_exception","reason":"Failed to parse query [(]","index":"verify"}],"type":"search_phase_execution_exception","reason":"all shards failed","phase":"query","failed_shards":[{"shard":0,"reason":{"type":"query_shard_exception","reason":"Failed to parse query [(]","caused_by":{"type":"parse_exception","reason":"Cannot parse '(': Encountered \"<EOF>\" at line 1, column 1."}}}]},"status":400}"#;

    #[tokio::test]
    async fn search_returns_empty_result_on_malformed_query() {
        // lenient 挡不住 JavaCC 语法错误，ES 仍回 400；CollectionEngine 对同样输入
        // 是 Ok，所以这里必须返回空结果，而不是把 400 抛给业务代码。
        let engine = ElasticsearchEngine::new(stub_es(400, PARSE_ERROR_BODY), None);
        let result = engine
            .search(&SearchBuilder::new("("))
            .await
            .expect("语法错误应当返回空结果而不是 Err");
        assert_eq!(result.total, 0);
        assert!(result.hits.is_empty());
    }

    #[tokio::test]
    async fn search_still_fails_on_other_400() {
        // 非语法错的 400（参数非法/映射错误）不能被吞成空结果。
        let engine = ElasticsearchEngine::new(
            stub_es(
                400,
                r#"{"error":{"root_cause":[{"type":"illegal_argument_exception","reason":"bad"}],"type":"illegal_argument_exception"},"status":400}"#,
            ),
            None,
        );
        let result = engine.search(&SearchBuilder::new("x")).await;
        assert!(
            matches!(result, Err(crate::ScoutError::Backend(_))),
            "非语法错的 400 仍须上抛，得到 {result:?}"
        );
    }

    #[tokio::test]
    async fn timed_out_search_is_an_error_not_a_short_result() {
        // ES 默认 allow_partial_search_results=true：超时也回 200，只是 hits 变少、
        // total 缩水。若按 Ok 返回，业务拿到的是静默错误的搜索结果 —— 比报错更糟。
        let engine = ElasticsearchEngine::new(
            stub_es(
                200,
                r#"{"timed_out":true,"hits":{"total":{"value":1},"hits":[{"_id":"a"}]}}"#,
            ),
            None,
        );
        let result = engine.search(&SearchBuilder::new("rust")).await;
        assert!(
            matches!(result, Err(crate::ScoutError::Backend(_))),
            "timed_out 的 200 必须上抛，得到 {result:?}"
        );
    }

    #[tokio::test]
    async fn search_with_failed_shards_is_an_error() {
        // 同上：分片失败也是 200 + 结果不全（total 偏低）。
        let engine = ElasticsearchEngine::new(
            stub_es(
                200,
                r#"{"_shards":{"total":2,"successful":1,"failed":1},"hits":{"total":{"value":0},"hits":[]}}"#,
            ),
            None,
        );
        let result = engine.search(&SearchBuilder::new("rust")).await;
        assert!(
            matches!(result, Err(crate::ScoutError::Backend(_))),
            "分片失败的 200 必须上抛，得到 {result:?}"
        );
    }

    #[tokio::test]
    async fn delete_in_treats_404_as_success() {
        // ES 对「文档不存在」和「索引不存在」都回 404。delete() 委托 delete_in，
        // 重复删除必须像 collection/database/typesense/meilisearch 一样返回 Ok。
        let engine = ElasticsearchEngine::new(stub_es(404, "{}"), None);
        let result = engine.delete_in("default", &["missing".to_string()]).await;
        assert!(result.is_ok(), "404 应当幂等成功，得到 {result:?}");
    }

    #[tokio::test]
    async fn delete_in_still_fails_on_other_non_2xx() {
        let engine = ElasticsearchEngine::new(stub_es(500, "{}"), None);
        let result = engine.delete_in("default", &["x".to_string()]).await;
        assert!(
            matches!(result, Err(crate::ScoutError::Backend(_))),
            "非 404 的错误仍须上抛，得到 {result:?}"
        );
    }

    #[tokio::test]
    async fn reserved_index_names_are_rejected() {
        // delete_index("_all") 在 ES 7.x / OpenSearch 默认配置下会删掉整个集群。
        let engine = engine();
        let err = engine
            .delete_index("_all")
            .await
            .expect_err("_all 必须被拒绝");
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn host_with_userinfo_is_rejected_before_any_request() {
        // reqwest 的错误 Display 会把完整 URL 拼出来，带 userinfo 的 host 只要
        // 失败一次密码就进日志；校验必须在发请求之前。
        let engine =
            ElasticsearchEngine::new("http://elastic:hunter2@127.0.0.1:1".to_string(), None);
        let err = engine
            .search(&SearchBuilder::new("rust"))
            .await
            .expect_err("内嵌 userinfo 的 host 必须被拒绝");
        assert!(matches!(err, crate::ScoutError::InvalidHost(_)), "got {err:?}");
        assert!(!err.to_string().contains("hunter2"), "错误信息不得回显密码");
    }

    #[tokio::test]
    async fn update_sends_one_bulk_request_for_all_documents() {
        // 回归：update 曾逐篇 PUT /{index}/_doc/{id}，N 篇文档 N 次往返。
        // 桩只 accept 一个连接，所以「发一次请求」连带被验证。
        let (host, captured) = stub_es_capturing(
            200,
            r#"{"items":[{"index":{"_id":"a","status":201}},{"index":{"_id":"b","status":201}}]}"#,
        );
        let engine = ElasticsearchEngine::new(host, None);
        let docs = [doc("a"), doc("b")];
        engine.update(&docs).await.expect("bulk update 应当成功");
        let request = captured.lock().unwrap().clone();
        assert!(request.starts_with("POST /default/_bulk "), "不是 _bulk：{request}");
        assert!(request.contains("{\"index\":{\"_id\":\"a\"}}"), "{request}");
        assert!(request.contains("{\"index\":{\"_id\":\"b\"}}"), "{request}");
    }

    #[tokio::test]
    async fn soft_delete_in_sends_one_bulk_request_and_tolerates_missing_docs() {
        // 回归：soft_delete_in 曾逐 id POST _update，N 个 id N 次往返。
        // 逐条版本的幂等来自「404 跳过」，批量版必须同样放行每条的 404。
        let (host, captured) = stub_es_capturing(
            200,
            r#"{"items":[
                {"update":{"_id":"a","status":200,"result":"updated"}},
                {"update":{"_id":"gone","status":404,"error":{"type":"document_missing_exception"}}}
            ]}"#,
        );
        let engine = ElasticsearchEngine::new(host, None);
        let ids = ["a".to_string(), "gone".to_string()];
        engine
            .soft_delete_in("default", &ids)
            .await
            .expect("不存在的文档必须幂等成功");
        let request = captured.lock().unwrap().clone();
        assert!(request.starts_with("POST /default/_bulk "), "不是 _bulk：{request}");
        let expected = "{\"update\":{\"_id\":\"gone\"}}\n{\"doc\":{\"__soft_deleted\":true}}";
        assert!(request.contains(expected), "{request}");
    }

    fn doc(id: &str) -> SearchDocument {
        SearchDocument::new(id, serde_json::json!({"title": id})).unwrap()
    }
