    use super::*;
    use std::time::Duration;

    // 裸 TcpListener 冒充 Typesense：按顺序回 responses（`build` 拿到 base_url 后再
    // 生成，同源跳转要用到自己的端口），并记下每条原始请求文本。
    // 返回 (base_url, 请求记录)，省掉 HTTP mock 依赖。
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

    #[tokio::test]
    async fn api_key_is_not_replayed_to_a_different_port_on_redirect() {
        // reqwest 换 host 只摘 Authorization/Cookie 等 5 个已知头（redirect.rs:239
        // remove_sensitive_headers），自定义的 X-TYPESENSE-API-KEY 不在名单里：
        // 后端回一个 302，密钥就原样落到攻击者主机。限同源后必须停住。
        // 两个 stub 端口不同 —— 只比 host_str() 的实现会在这里放行。
        let (b_url, b_rx) = stub_server(|_| vec![http_response("200 OK", "{}")]);
        let (a_url, a_rx) =
            stub_server(|_| vec![http_redirect(&format!("{b_url}/collections/books/documents"))]);
        let engine = TypesenseEngine::new(a_url, Some("SECRETKEY".to_string()));
        let _ = engine.delete_in("books", &["1".to_string()]).await;

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
                http_redirect(&format!("{base}/collections/books/documents")),
                http_response("200 OK", "{}"),
            ]
        });
        let engine = TypesenseEngine::new(a_url, Some("SECRETKEY".to_string()));
        engine.delete_in("books", &["1".to_string()]).await.unwrap();

        let first = a_rx.recv_timeout(Duration::from_secs(5)).expect("第一次请求");
        let second = a_rx.recv_timeout(Duration::from_secs(5)).expect("同源跳转应被跟随");
        assert!(first.contains("/collections/books/documents"));
        assert!(second.contains("/collections/books/documents"), "{second}");
        assert!(second.to_lowercase().contains("secretkey"), "同源跳转要带着密钥");
    }

    #[tokio::test]
    async fn userinfo_host_is_rejected_without_echoing_the_password() {
        // reqwest 的错误 Display 会拼出完整 URL：`user:pass@` 只要请求失败一次，
        // 密码就跟着错误信息进日志。校验必须先于 I/O，并且不回显 host。
        let engine = TypesenseEngine::new("http://user:s3cret@127.0.0.1:1".to_string(), None);
        let err = engine
            .delete_in("books", &["1".to_string()])
            .await
            .expect_err("内嵌 userinfo 的 host 必须拒绝");
        assert!(matches!(err, crate::ScoutError::InvalidHost(_)), "got {err:?}");
        assert!(!err.to_string().contains("s3cret"), "错误信息回显了密码: {err}");
    }

    #[tokio::test]
    async fn delete_in_issues_one_batched_request() {
        // 曾经每条 id 一次 DELETE（N 次往返）。delete-by-query 一次删完，`id` 默认
        // 有索引；返回 num_deleted，重复删除照样是 Ok（幂等语义不变）。
        let (url, rx) = stub_server(|_| vec![http_response("200 OK", r#"{"num_deleted":2}"#)]);
        let engine = TypesenseEngine::new(url, None);
        engine
            .delete_in("books", &["a".to_string(), "b".to_string()])
            .await
            .unwrap();

        let req = rx.recv_timeout(Duration::from_secs(5)).expect("应收到请求");
        assert!(req.starts_with("DELETE /collections/books/documents?"), "{req}");
        assert!(req.contains("filter_by="), "要靠 delete-by-query 过滤而不是路径: {req}");
        assert!(
            req.contains("%22a%22") && req.contains("%22b%22"),
            "两个 id 都要进过滤器: {req}"
        );
    }

    #[tokio::test]
    async fn delete_in_treats_404_as_success() {
        // 集合不存在仍是 404（不是逐条 404），重复删除必须像其它驱动一样返回 Ok。
        let (url, _rx) = stub_server(|_| vec![http_response("404 Not Found", "{}")]);
        let engine = TypesenseEngine::new(url, None);
        engine.delete_in("books", &["a".to_string()]).await.unwrap();
    }

    #[tokio::test]
    async fn soft_delete_in_batches_read_and_write() {
        // 曾经每条 id 各一次搜索 + 一次 import（2N 次往返）。现在一次批量搜
        // （limit ≤ 250，超了会静默少标）+ 一次 import。
        let (url, rx) = stub_server(|_| {
            vec![
                http_response(
                    "200 OK",
                    r#"{"found":2,"hits":[{"document":{"id":"a","title":"x"}},{"document":{"id":"b","title":"y"}}]}"#,
                ),
                http_response("200 OK", "{\"success\":true}\n{\"success\":true}\n"),
            ]
        });
        let engine = TypesenseEngine::new(url, None);
        engine
            .soft_delete_in("books", &["a".to_string(), "b".to_string()])
            .await
            .unwrap();

        let read = rx.recv_timeout(Duration::from_secs(5)).expect("读应只发一次");
        assert!(read.starts_with("GET /collections/books/documents/search?"), "{read}");
        assert!(read.contains("limit=250"), "一次搜完本块: {read}");
        // 默认 Exclude：已软删的 id 直接跳过（与 meilisearch 的 soft_delete_in 对齐），
        // 省掉一次必然 no-op 的写回。
        assert!(read.contains("__soft_deleted"), "读侧应带默认的软删除守卫: {read}");
        let write = rx.recv_timeout(Duration::from_secs(5)).expect("写应只发一次");
        assert!(
            write.starts_with("POST /collections/books/documents/import?action=upsert"),
            "{write}"
        );
        assert!(write.contains("\"__soft_deleted\":true"), "{write}");
        assert!(write.contains("\"id\":\"a\"") && write.contains("\"id\":\"b\""), "{write}");
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "读+写各一次就该结束"
        );
    }

    #[tokio::test]
    async fn flush_is_a_noop_and_makes_no_request() {
        // 指向必然连不上的地址：真的打网络就会失败，返回 Ok 即证明没有请求。
        // flush 的契约是「刷新可见性」，绝不能是清空索引——README 的生命周期示例
        // 在 update 与 search 之间调用它，而这里曾删掉整个索引并返回 Ok。
        let engine = TypesenseEngine::new("http://127.0.0.1:1".to_string(), None);
        engine.flush("books").await.unwrap();
    }

    #[tokio::test]
    async fn index_less_soft_delete_is_refused_not_silently_skipped() {
        // 曾经它硬编码 default：对写在别的索引里的文档静默跳过却返回 Ok。
        let engine = TypesenseEngine::new("http://127.0.0.1:1".to_string(), None);
        let err = engine
            .soft_delete(&["b1".to_string()])
            .await
            .expect_err("index-less soft_delete 必须报错，而不是静默 no-op");
        assert!(matches!(err, crate::ScoutError::Unsupported(_)), "got {err:?}");
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
    async fn empty_where_in_short_circuits_search_and_paginate() {
        // 回归：空 where_in 集合 = 不匹配任何（Collection 语义），曾被拼成
        // `tag:=[]` 丢给后端（行为由后端决定：报错或匹配全部）。
        let engine = TypesenseEngine::new("http://127.0.0.1:1".to_string(), None);
        let builder = SearchBuilder::new("")
            .within("books")
            .where_in("tag", Vec::<&str>::new());
        let result = engine.search(&builder).await.unwrap();
        assert!(result.hits.is_empty());
        assert_eq!(result.total, 0);
        // paginate 走 search_page 的同一条短路，不能只在 search 上修。
        let paged = engine.paginate(&builder, 2, 10).await.unwrap();
        assert!(paged.hits.is_empty());
        assert_eq!(paged.total, 0);
    }

    #[tokio::test]
    async fn reserved_index_name_is_rejected_even_with_empty_where_in() {
        let engine = TypesenseEngine::new("http://127.0.0.1:1".to_string(), None);
        let builder = SearchBuilder::new("q").within("_all").where_in("t", Vec::<&str>::new());
        let err = engine.search(&builder).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
        let err = engine.paginate(&builder, 2, 10).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn update_bulk_validates_every_index_name_before_writing() {
        // 校验必须覆盖**全部**索引名再发第一条请求：按组边校验边 import 时，一批里
        // 混进一个保留索引名，合法那组已经落库、调用方却拿到 Err —— 半途写入。stub
        // 一条响应都不给：任何一次写入尝试都只会以 Http 错误（连接被拒）收场，
        // 于是「先写后错」在 InvalidIndexName 这条断言下无法蒙混过关。
        let (url, rx) = stub_server(|_| vec![]);
        let engine = TypesenseEngine::new(url, None);
        let mut docs: Vec<SearchDocument> = (0..3)
            .map(|i| {
                let mut doc =
                    SearchDocument::new(format!("ok{i}"), serde_json::json!({"title": "x"}))
                        .unwrap();
                doc.index = Some(format!("books{i}"));
                doc
            })
            .collect();
        let mut reserved = SearchDocument::new("bad", serde_json::json!({"title": "x"})).unwrap();
        reserved.index = Some("_all".to_string());
        docs.push(reserved);

        let err = engine.update_bulk(&docs).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
        assert!(rx.try_recv().is_err(), "一个写入请求都不该发出去");
    }

    #[tokio::test]
    async fn empty_id_list_still_validates_the_index_name() {
        // 钉住顺序：空 id 列表的短路排在 validate_index_name **之后**，与其余
        // 七个驱动的同一输入同行为（`_all` 必须报 InvalidIndexName，不是 Ok）。
        // 地址必然连不上：真发请求会是 Http 错误，返回 InvalidIndexName 才说明
        // 校验先于 I/O 也先于短路。
        let engine = TypesenseEngine::new("http://127.0.0.1:1".to_string(), None);
        let empty: &[String] = &[];
        let err = engine.delete_in("_all", empty).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
        let err = engine.delete_bulk("_all", empty).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
        let err = engine.soft_delete_in("_all", empty).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

