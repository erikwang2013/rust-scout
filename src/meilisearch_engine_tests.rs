    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::SearchBuilder;

    /// 记录到的请求：(路径, 请求体)。
    type Seen = Arc<Mutex<Vec<(String, String)>>>;

    /// 裸 TcpListener 冒充 Meilisearch：按顺序回预置响应，并记录 (路径, 请求体)。
    /// 省掉 HTTP mock 依赖（同 elasticsearch_engine.rs 的做法）。
    fn stub(responses: Vec<(u16, String)>) -> (String, Seen) {
        use std::io::{BufRead, BufReader, Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let addr = listener.local_addr().expect("stub addr");
        let seen: Seen = Arc::default();
        let recorded = Arc::clone(&seen);
        std::thread::spawn(move || {
            for (status, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                    return;
                }
                let path = request_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                // 必须先读干请求体再回包：带着未读数据的 socket 直接 close 会给
                // 对端发 RST，reqwest 就读不到响应了。
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut raw = vec![0u8; len];
                let _ = reader.read_exact(&mut raw);
                recorded
                    .lock()
                    .unwrap()
                    .push((path, String::from_utf8_lossy(&raw).into_owned()));
                let response = format!(
                    "HTTP/1.1 {status} Stub\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), seen)
    }

    #[test]
    fn filter_equality_and_in_clauses() {
        let builder = SearchBuilder::new("")
            .where_field("title", "a\"b\\c")
            .where_field("count", 5)
            .where_field("active", true)
            .where_in("tag", ["x", "y"])
            .where_not_in("tag", ["z"]);
        let filter = MeilisearchEngine::build_filter(&builder).unwrap().unwrap();
        assert_eq!(
            filter,
            r#"title="a\"b\\c" AND count=5 AND active=true AND tag IN ["x", "y"] AND tag NOT IN ["z"] AND NOT __soft_deleted = true"#
        );
    }

    #[test]
    fn filter_trashed_variants() {
        assert_eq!(
            MeilisearchEngine::build_filter(&SearchBuilder::new(""))
                .unwrap()
                .unwrap(),
            "NOT __soft_deleted = true"
        );
        assert_eq!(
            MeilisearchEngine::build_filter(&SearchBuilder::new("").only_trashed())
                .unwrap()
                .unwrap(),
            "__soft_deleted=true"
        );
        assert_eq!(
            MeilisearchEngine::build_filter(&SearchBuilder::new("").with_trashed()).unwrap(),
            None
        );
    }

    #[test]
    fn field_name_with_operators_is_rejected_not_interpolated() {
        // 字段名之前是裸拼进表达式：一个带空格或 `||` 的字段名能把整条 filter
        // 改写成别的查询（软删除守卫会被 OR 掉），值那边转义了但字段名没有。
        for builder in [
            SearchBuilder::new("").where_field("x=1 || y", "v"),
            SearchBuilder::new("").where_in("a IN [] || b", ["v"]),
            SearchBuilder::new("").where_not_in("a AND b", ["v"]),
        ] {
            let err = MeilisearchEngine::build_filter(&builder)
                .expect_err("非法字段名必须拒绝");
            assert!(
                matches!(err, crate::ScoutError::InvalidFieldName(_)),
                "got {err:?}"
            );
        }
        let sorted = SearchBuilder::new("").order_by("a:desc, b", false);
        assert!(matches!(
            MeilisearchEngine::sort_array(&sorted),
            Err(crate::ScoutError::InvalidFieldName(_))
        ));
        // 正常字段名（含嵌套点号与中文）不能误伤
        assert!(MeilisearchEngine::build_filter(
            &SearchBuilder::new("").where_field("author.name", 1).where_field("标题", 2)
        )
        .is_ok());
    }

    #[test]
    fn empty_not_in_skips_validation_like_empty_in_skips_the_query() {
        // 不产生表达式就不该报错：与 search 对空 where_in 直接短路（也不校验）
        // 的行为保持对称。
        assert_eq!(
            MeilisearchEngine::build_filter(
                &SearchBuilder::new("")
                    .with_trashed()
                    .where_not_in("bad field", Vec::<&str>::new())
            )
            .unwrap(),
            None
        );
    }




    #[test]
    fn page_mode_prefers_page_when_offset_is_page_aligned() {
        // 关键回归：Meilisearch 只在 page/hitsPerPage 模式下返回**穷尽**的 totalHits；
        // offset/limit 模式只给 estimatedTotalHits（受 maxTotalHits 封顶，默认 1000）。
        // 全用 offset 会让 total 变成估算值，与其余七个驱动不一致。
        let aligned = SearchBuilder::new("q").take(10).skip(20);
        assert!(
            matches!(
                MeilisearchEngine::page_mode(&aligned),
                PageMode::Page(3, 10)
            ),
            "skip 是 take 的整数倍时必须走页模式（穷尽计数）"
        );
        // 页对不齐才退回 offset —— 页模式会丢掉 15 % 10 的余数
        let unaligned = SearchBuilder::new("q").take(10).skip(15);
        assert!(matches!(
            MeilisearchEngine::page_mode(&unaligned),
            PageMode::Offset(15, 10)
        ));
        // 无 skip 时页对齐（offset 0）
        assert!(matches!(
            MeilisearchEngine::page_mode(&SearchBuilder::new("q")),
            PageMode::Page(1, 10)
        ));
    }

    #[test]
    fn search_body_emits_exactly_one_paging_mode() {
        // 两种模式互斥：Meilisearch 拒绝 offset 与 page 同时出现
        let b = SearchBuilder::new("q");
        let paged = MeilisearchEngine::search_body(&b, &PageMode::Page(2, 10)).unwrap();
        assert!(paged.get("page").is_some() && paged.get("offset").is_none());
        let offset = MeilisearchEngine::search_body(&b, &PageMode::Offset(15, 10)).unwrap();
        assert!(offset.get("offset").is_some() && offset.get("page").is_none());
    }

    #[test]
    fn search_body_includes_filter_when_trashed_excludes() {
        let builder = SearchBuilder::new("x").where_field("cat", 1);
        let body = MeilisearchEngine::search_body(&builder, &MeilisearchEngine::page_mode_of(1, 10))
            .unwrap();
        assert_eq!(
            body["filter"],
            "cat=1 AND NOT __soft_deleted = true"
        );
    }

    #[test]
    fn parse_response_reads_estimated_total_hits() {
        // 回归：offset/limit 搜索只回 estimatedTotalHits，读到它之前 total 会
        // 退化成 hits.len()（页大小），分页 UI 的「共 N 条」就错了。
        let raw = serde_json::json!({"hits": [{"id": "1"}], "estimatedTotalHits": 471});
        let result = MeilisearchEngine::parse_search_response(&raw);
        assert_eq!(result.total, 471);
        assert_eq!(result.hits.len(), 1);
        // page/hitsPerPage 模式仍回 totalHits（穷尽计数），优先读它。
        let page_mode = serde_json::json!({
            "hits": [{"id": "1"}],
            "totalHits": 42,
            "estimatedTotalHits": 471
        });
        assert_eq!(MeilisearchEngine::parse_search_response(&page_mode).total, 42);
    }

    #[test]
    fn parse_response_extracts_hits_total_score() {
        let raw = serde_json::json!({
            "hits": [
                {"id": "1", "title": "a", "_rankingScore": 0.9},
                {"id": 2, "title": "b"}
            ],
            "totalHits": 42
        });
        let result = MeilisearchEngine::parse_search_response(&raw);
        assert_eq!(result.total, 42);
        assert_eq!(result.hits.len(), 2);
        assert_eq!(result.hits[0].id, "1");
        assert_eq!(result.hits[0].score, Some(0.9));
        assert_eq!(result.hits[0].source["title"], "a");
        assert_eq!(result.hits[1].id, "2"); // 数字 id 也转字符串
        assert_eq!(result.hits[1].score, None);
    }

    #[test]
    fn parse_response_total_falls_back_to_hits_len() {
        let raw = serde_json::json!({"hits": [{"id": "1"}]});
        assert_eq!(MeilisearchEngine::parse_search_response(&raw).total, 1);
        let empty = MeilisearchEngine::parse_search_response(&serde_json::json!({}));
        assert_eq!(empty.total, 0);
        assert!(empty.hits.is_empty());
    }

    #[tokio::test]
    async fn flush_is_a_noop_and_makes_no_request() {
        // 指向必然连不上的地址：真的打网络就会失败，返回 Ok 即证明没有请求。
        // flush 的契约是「刷新可见性」，绝不能是清空索引——README 的生命周期示例
        // 在 update 与 search 之间调用它，而这里曾打 delete-all 把索引删空。
        let engine = MeilisearchEngine::new("http://127.0.0.1:1".to_string(), None);
        engine.flush("books").await.unwrap();
    }

    #[tokio::test]
    async fn index_less_soft_delete_is_refused_not_silently_skipped() {
        // 曾经它硬编码 default：对写在别的索引里的文档静默跳过却返回 Ok。
        let engine = MeilisearchEngine::new("http://127.0.0.1:1".to_string(), None);
        let err = engine
            .soft_delete(&["b1".to_string()])
            .await
            .expect_err("index-less soft_delete 必须报错，而不是静默 no-op");
        assert!(matches!(err, crate::ScoutError::Unsupported(_)), "got {err:?}");
    }


    #[tokio::test]
    async fn empty_where_in_short_circuits_search_and_paginate() {
        // 回归：空 where_in 集合 = 不匹配任何（Collection 语义），曾被拼成
        // `tag IN []` 丢给后端（行为由后端决定：报错或匹配全部）。
        let engine = MeilisearchEngine::new("http://127.0.0.1:1".to_string(), None);
        let builder = SearchBuilder::new("")
            .within("books")
            .where_in("tag", Vec::<&str>::new());
        let result = engine.search(&builder).await.unwrap();
        assert!(result.hits.is_empty());
        assert_eq!(result.total, 0);
        // paginate 走同一条短路，不能只在 search 上修。
        let paged = engine.paginate(&builder, 2, 10).await.unwrap();
        assert!(paged.hits.is_empty());
        assert_eq!(paged.total, 0);
    }

    #[tokio::test]
    async fn reserved_index_name_is_rejected_even_with_empty_where_in() {
        // 回归：空 where_in 的短路原先排在 validate_index_name 之前，
        // within("_all") 会因此返回 Ok(空结果) 而不是 InvalidIndexName。
        // paginate 有同一个短路，两个入口必须同行为。
        let engine = MeilisearchEngine::new("http://127.0.0.1:1".to_string(), None);
        let builder = SearchBuilder::new("q").within("_all").where_in("t", Vec::<&str>::new());
        let err = engine.search(&builder).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
        let err = engine.paginate(&builder, 2, 10).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn soft_delete_in_writes_back_to_the_index_it_searched() {
        // 回归：曾把 `index: None` 的文档交给 update_bulk，而它按 doc.index 分组、
        // 缺省落 default —— 目标索引里的文档根本没被标记（照样搜得到），default
        // 里反而多出一个幽灵副本，覆盖掉那儿的同 id 文档。
        let (host, seen) = stub(vec![
            (
                200,
                r#"{"hits":[{"id":"b1","title":"t"}],"totalHits":1}"#.to_string(),
            ),
            (202, r#"{"taskUid":1,"status":"enqueued"}"#.to_string()),
            (200, r#"{"uid":1,"status":"succeeded"}"#.to_string()),
        ]);
        let engine = MeilisearchEngine::new(host, None);
        engine
            .soft_delete_in("books", &["b1".to_string()])
            .await
            .unwrap();

        let seen = seen.lock().unwrap();
        let paths: Vec<&str> = seen.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "/indexes/books/search",
                "/indexes/books/documents?primaryKey=id",
                "/tasks/1",
            ],
            "读、写、任务轮询都要落在 books 上"
        );
        assert!(
            !seen.iter().any(|(path, _)| path.contains("/indexes/default/")),
            "不许碰 default，实际路径：{paths:?}"
        );
        let write = &seen[1].1;
        assert!(
            write.contains(r#""__soft_deleted":true"#),
            "写回的是打标版本：{write}"
        );
        assert!(write.contains(r#""id":"b1""#), "写回的是原文档：{write}");
    }

    #[tokio::test]
    async fn failed_write_task_surfaces_as_error() {
        // 回归：POST /documents 只回 202 enqueued，真正的失败（缺主键、字段类型
        // 不符）出现在 /tasks/{uid} 的 status:"failed" 上。不轮询 => update()
        // 谎报 Ok，而 flush 是 no-op，这次失败再没有别的地方能看到。
        let (host, _seen) = stub(vec![
            (202, r#"{"taskUid":7,"status":"enqueued"}"#.to_string()),
            (
                200,
                r#"{"uid":7,"status":"failed","error":{"message":"missing document id"}}"#
                    .to_string(),
            ),
        ]);
        let engine = MeilisearchEngine::new(host, None);
        let doc = SearchDocument::new("1", serde_json::json!({"title": "a"})).unwrap();
        let err = engine.update(&[doc]).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::Backend(_)), "got {err:?}");
        assert!(
            err.to_string().contains("missing document id"),
            "错误里要带上后端给出的原因：{err}"
        );
    }

    #[tokio::test]
    async fn pending_write_task_is_polled_until_terminal() {
        // 任务不会一次查询就到终态：轮询循环必须带间隔重试。这也是唯一会走到
        // 定时器的路径 —— 用 thread::sleep 会占死运行时工作线程，把同运行时的
        // 其它任务一起卡住（#[tokio::test] 就是 current_thread 运行时）。
        let (host, seen) = stub(vec![
            (202, r#"{"taskUid":9,"status":"enqueued"}"#.to_string()),
            (200, r#"{"uid":9,"status":"processing"}"#.to_string()),
            (200, r#"{"uid":9,"status":"succeeded"}"#.to_string()),
        ]);
        let engine = MeilisearchEngine::new(host, None);
        let doc = SearchDocument::new("1", serde_json::json!({"title": "a"})).unwrap();
        engine.update(&[doc]).await.unwrap();
        let seen = seen.lock().unwrap();
        let paths: Vec<&str> = seen.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "/indexes/default/documents?primaryKey=id",
                "/tasks/9",
                "/tasks/9",
            ],
            "非终态要再查一次，直到 succeeded"
        );
    }

    #[tokio::test]
    async fn write_without_task_uid_is_accepted() {
        // 旧版 Meilisearch（或中间的代理）不回 taskUid：无从确认，按成功处理，
        // 不要因此把正常写入变成错误。
        let (host, seen) = stub(vec![(200, "{}".to_string())]);
        let engine = MeilisearchEngine::new(host, None);
        let doc = SearchDocument::new("1", serde_json::json!({"title": "a"})).unwrap();
        engine.update(&[doc]).await.unwrap();
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "没有 taskUid 就不该再多发 /tasks 请求"
        );
    }

    #[tokio::test]
    async fn userinfo_host_is_rejected_before_any_request() {
        // host 里内嵌凭据时，reqwest 的错误 Display 会把密码拼进日志；这里要求
        // 请求根本没发出去（地址不可达，真发出去会是 Http 错误而不是 InvalidHost）。
        let engine = MeilisearchEngine::new("http://meili:hunter2@127.0.0.1:1".to_string(), None);
        let err = engine.search(&SearchBuilder::new("q")).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidHost(_)), "got {err:?}");
        assert!(
            !err.to_string().contains("hunter2"),
            "错误信息不能回显凭据：{err}"
        );
    }
