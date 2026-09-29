    use super::*;
    use crate::SearchBuilder;
    use std::time::Duration;

    // 裸 TcpListener 冒充 Algolia：按顺序回 responses（`build` 拿到 base_url 后再生成，
    // 同源跳转要用到自己的端口），并记下每条原始请求文本。返回 (base_url, 请求记录)。
    fn stub_server(
        build: impl FnOnce(&str) -> Vec<String>,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let addr = listener.local_addr().expect("stub addr");
        let base = format!("http://{addr}");
        let responses = build(&base);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                    let text = String::from_utf8_lossy(&buf);
                    if let Some(head_end) = text.find("\r\n\r\n").map(|i| i + 4) {
                        // body 要按 content-length 读全再回包，否则客户端看到连接被截断
                        if buf.len() >= head_end + content_length(&text[..head_end]) {
                            break;
                        }
                    }
                }
                let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (base, rx)
    }

    fn content_length(head: &str) -> usize {
        head.lines()
            .find_map(|l| match l.split_once(':') {
                Some((k, v)) if k.eq_ignore_ascii_case("content-length") => v.trim().parse().ok(),
                _ => None,
            })
            .unwrap_or(0)
    }

    fn http_response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn http_redirect(location: &str) -> String {
        format!(
            "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        )
    }

    /// 指向任意 base_url 的引擎。`new()` 只会生成 `*.algolia.net`，而重定向 /
    /// userinfo 这两条路径要用本地 stub 才能测；host 是模块私有字段，测试模块能直接构造。
    fn engine_at(base: &str, key: &str) -> AlgoliaEngine {
        AlgoliaEngine {
            host: Some(base.to_string()),
            app_id: "TESTAPPID".to_string(),
            api_key: key.to_string(),
            client: client(),
        }
    }

    #[test]
    fn filters_join_with_comma_and_or_groups() {
        let builder = SearchBuilder::new("")
            .where_field("active", true)
            .where_field("price", 10)
            .where_in("tag", ["a", "b"])
            .where_not_in("tag", ["x"]);
        let filters = AlgoliaEngine::build_filters(&builder).unwrap().unwrap();
        assert_eq!(
            filters,
            r#"active=true,price=10,(tag: "a" OR "b"),NOT (tag: "x"),NOT __soft_deleted:true"#
        );
    }

    #[test]
    fn filters_trashed_variants() {
        assert_eq!(
            AlgoliaEngine::build_filters(&SearchBuilder::new("")).unwrap().unwrap(),
            "NOT __soft_deleted:true"
        );
        assert_eq!(
            AlgoliaEngine::build_filters(&SearchBuilder::new("").only_trashed())
                .unwrap()
                .unwrap(),
            "__soft_deleted:true"
        );
        assert_eq!(
            AlgoliaEngine::build_filters(&SearchBuilder::new("").with_trashed()).unwrap(),
            None
        );
    }

    #[test]
    fn operator_chars_in_field_name_are_rejected() {
        // 回归：字段名曾经裸拼进 filters。`NOT (x)` 之类的字段名能把逗号连接的
        // AND 语义改写掉（逗号在 Algolia 是 AND，字段名里塞 `,` 就多出一个条件）。
        let b = SearchBuilder::new("").where_field("a,NOT __soft_deleted:true", "1");
        assert!(matches!(
            AlgoliaEngine::build_filters(&b),
            Err(crate::ScoutError::InvalidFieldName(_))
        ));
        let b = SearchBuilder::new("").where_in("a, b", ["1"]);
        assert!(AlgoliaEngine::build_filters(&b).is_err());
        let b = SearchBuilder::new("").where_not_in("a OR b", ["1"]);
        assert!(AlgoliaEngine::build_filters(&b).is_err());
        // 合法字段名不受影响：点号（嵌套属性）与中文列名照常。
        let ok = SearchBuilder::new("").where_field("author.name", "x");
        assert_eq!(
            AlgoliaEngine::build_filters(&ok).unwrap().unwrap(),
            r#"author.name="x",NOT __soft_deleted:true"#
        );
    }

    #[test]
    fn reindex_body_destination_is_an_index_name_string() {
        // 回归：曾经发 `{"operation":"copy","destination":{"index":"to"}}`，官方
        // operationIndex 要的是**字符串**，对象形式每次都是 400/422。
        let body = AlgoliaEngine::reindex_body("products_backup");
        assert_eq!(body["operation"], "copy");
        assert!(body["destination"].is_string(), "destination 是字符串，不是对象: {body}");
        assert_eq!(body["destination"], "products_backup");
    }

    #[tokio::test]
    async fn reindex_posts_the_copy_operation_to_the_source_index() {
        let (url, rx) = stub_server(|_| {
            vec![
                http_response("200 OK", r#"{"taskID":1,"updatedAt":"2026-01-01T00:00:00Z"}"#),
                http_response("200 OK", r#"{"status":"published"}"#),
            ]
        });
        let engine = engine_at(&url, "SECRETKEY");
        engine.reindex("products", "products_backup").await.unwrap();

        let req = rx.recv_timeout(Duration::from_secs(5)).expect("应收到请求");
        assert!(req.starts_with("POST /1/indexes/products/operation "), "{req}");
        assert!(
            req.contains(r#""destination":"products_backup""#),
            "destination 必须是索引名字符串: {req}"
        );
        // 复制同样是任务制（`reindex_body` 硬编码 copy）：任务挂在**目标**索引上，
        // 官方客户端 replaceAllObjects 对 copy 就是拿 destination 的名字等任务的。
        let poll = rx.recv_timeout(Duration::from_secs(5)).expect("应轮询任务");
        assert!(poll.starts_with("GET /1/indexes/products_backup/task/1 "), "{poll}");
    }

    #[tokio::test]
    async fn write_waits_until_the_task_is_published() {
        // 回归：曾经丢弃 batch 的响应体，「已入队」被当成「已写入」返回 Ok。
        // 记录在任务发布前不可搜索，所以必须轮询到 published。
        let (url, rx) = stub_server(|_| {
            vec![
                http_response("200 OK", r#"{"taskID":7,"objectIDs":["a"]}"#),
                http_response("200 OK", r#"{"status":"notPublished"}"#),
                http_response("200 OK", r#"{"status":"published"}"#),
            ]
        });
        let engine = engine_at(&url, "SECRETKEY");
        let doc = SearchDocument::new("a", serde_json::json!({"title": "x"})).unwrap();
        engine.update(&[doc]).await.unwrap();

        let write = rx.recv_timeout(Duration::from_secs(5)).expect("写入请求");
        assert!(write.starts_with("POST /1/indexes/default/batch "), "{write}");
        // notPublished 是「还没发布完」而不是失败：要继续轮，不能当错误也不能当成功
        let first = rx.recv_timeout(Duration::from_secs(5)).expect("第一次轮询");
        assert!(first.starts_with("GET /1/indexes/default/task/7 "), "{first}");
        let second = rx.recv_timeout(Duration::from_secs(5)).expect("notPublished 之后要接着轮");
        assert!(second.starts_with("GET /1/indexes/default/task/7 "), "{second}");
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "published 之后不该再轮");
    }

    #[tokio::test]
    async fn write_without_task_id_is_accepted() {
        // 代理或旧接口不回 taskID：无从确认，按成功处理，也不能再发 /task 请求。
        let (url, rx) = stub_server(|_| vec![http_response("200 OK", r#"{"objectIDs":["a"]}"#)]);
        let engine = engine_at(&url, "SECRETKEY");
        let doc = SearchDocument::new("a", serde_json::json!({"title": "x"})).unwrap();
        engine.update(&[doc]).await.unwrap();

        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "没有 taskID 就不该再多发 /task 请求"
        );
    }

    #[tokio::test]
    async fn api_key_is_not_replayed_to_a_different_port_on_redirect() {
        // reqwest 换 host 只摘 Authorization/Cookie 等 5 个已知头
        // （redirect.rs 的 remove_sensitive_headers），自定义的 X-Algolia-API-Key
        // 不在名单里：后端回一个 302，密钥就原样落到攻击者主机。
        // 两个 stub 端口不同 —— 只比 host_str() 的实现会在这里放行。
        let (b_url, b_rx) = stub_server(|_| vec![http_response("200 OK", "{}")]);
        let (a_url, a_rx) =
            stub_server(|_| vec![http_redirect(&format!("{b_url}/1/indexes/books/query"))]);
        let engine = engine_at(&a_url, "SECRETKEY");
        let _ = engine.search(&SearchBuilder::new("q").within("books")).await;

        let a_req = a_rx.recv_timeout(Duration::from_secs(5)).expect("A 应收到请求");
        assert!(a_req.to_lowercase().contains("secretkey"), "密钥本该发给 A: {a_req}");
        assert!(
            b_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "302 把 API key 送到了另一个端口"
        );
    }

    #[tokio::test]
    async fn same_origin_redirect_is_still_followed() {
        // 反向代理补尾斜杠这类同源跳转必须照常跟随——这正是没用 Policy::none() 的原因。
        let (a_url, a_rx) = stub_server(|base| {
            vec![
                http_redirect(&format!("{base}/1/indexes/books/query")),
                http_response("200 OK", r#"{"hits":[],"nbHits":0}"#),
            ]
        });
        let engine = engine_at(&a_url, "SECRETKEY");
        engine.search(&SearchBuilder::new("q").within("books")).await.unwrap();

        let first = a_rx.recv_timeout(Duration::from_secs(5)).expect("第一次请求");
        let second = a_rx.recv_timeout(Duration::from_secs(5)).expect("同源跳转应被跟随");
        assert!(first.starts_with("POST /1/indexes/books/query"));
        // 302 之后的第二次请求由 tower-http 按 RFC 7231 降级成 GET（原方法只保 HEAD），
        // 所以这里断言路径与密钥，不断言方法。
        assert!(second.contains("/1/indexes/books/query"), "{second}");
        assert!(second.to_lowercase().contains("secretkey"), "同源跳转要带着密钥");
    }

    #[tokio::test]
    async fn userinfo_host_is_rejected_without_echoing_the_password() {
        // reqwest 的错误 Display 会拼出完整 URL：`user:pass@` 只要请求失败一次，
        // 密码就跟着错误信息进日志。校验必须先于 I/O，并且不回显 host。
        let engine = engine_at("http://user:s3cret@127.0.0.1:1", "SECRETKEY");
        let err = engine
            .search(&SearchBuilder::new("q").within("books"))
            .await
            .expect_err("内嵌 userinfo 的 host 必须拒绝");
        assert!(matches!(err, crate::ScoutError::InvalidHost(_)), "got {err:?}");
        assert!(!err.to_string().contains("s3cret"), "错误信息回显了密码: {err}");
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
        let body = AlgoliaEngine::search_body(&builder, 20, 10).unwrap();
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
        let body = AlgoliaEngine::search_body(&SearchBuilder::new("q"), 15, 10).unwrap();
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
    async fn take_zero_keeps_total_and_drops_hits() {
        // take(0) 的契约（Collection/ES 基准）是「命中总数 + 空 hits」：total 在分页
        // 之前算出。实现改为照发请求（只取 1 条）再清空 hits，见 result.rs::without_hits。
        let r = SearchResult {
            hits: vec![],
            total: 7,
            ..SearchResult::default()
        };
        let trimmed = r.without_hits();
        assert!(trimmed.hits.is_empty());
        assert_eq!(trimmed.total, 7);
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

    #[test]
    fn empty_where_in_and_not_in_produce_no_filter_clause() {
        // 空 IN 集合 = 不匹配任何（由 search/paginate 短路，不进 filter）；
        // 空 NOT IN 集合 = 无过滤。两者都不该出现在 filters 里。
        let b = SearchBuilder::new("q")
            .where_in("tag", Vec::<&str>::new())
            .where_not_in("cat", Vec::<&str>::new());
        let f = AlgoliaEngine::build_filters(&b).unwrap().unwrap_or_default();
        assert!(!f.contains("tag:") && !f.contains("cat:"), "got {f:?}");
    }


    #[tokio::test]
    async fn reserved_index_name_is_rejected_even_with_empty_where_in() {
        let engine = AlgoliaEngine::new("testappid".to_string(), "k".to_string());
        let err = engine
            .search(&SearchBuilder::new("q").within("_all").where_in("t", Vec::<&str>::new()))
            .await
            .unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

