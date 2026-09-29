    use super::*;

    #[test]
    fn split_addrs_handles_boundary_ports() {
        assert_eq!(
            XunSearchEngine::split_addrs("127.0.0.1:8383"),
            ("127.0.0.1:8383".to_string(), "127.0.0.1:8384".to_string())
        );
        // 65535 没有下一个端口：曾经是 p + 1，debug 下溢出 panic、release 下得 0。
        assert_eq!(
            XunSearchEngine::split_addrs("127.0.0.1:65535"),
            ("127.0.0.1:8383".to_string(), "127.0.0.1:8384".to_string())
        );
        // 非法/缺失端口退回默认对
        assert_eq!(
            XunSearchEngine::split_addrs("127.0.0.1:not-a-port"),
            ("127.0.0.1:8383".to_string(), "127.0.0.1:8384".to_string())
        );
        assert_eq!(
            XunSearchEngine::split_addrs("127.0.0.1"),
            ("127.0.0.1:8383".to_string(), "127.0.0.1:8384".to_string())
        );
    }

    #[test]
    fn doc_commands_skip_empty_values_and_dont_burn_vnos() {
        // 数组/null/空串经 value_bytes 都是空字节：xapian 拒绝空词，这些字段也不该
        // 占用动态 vno。引擎里曾有一份漏掉该 guard 的副本，且只有它真正在跑。
        let mut scheme = FieldScheme::default();
        let doc = SearchDocument::new(
            "1",
            serde_json::json!({"tags": ["a"], "meta": null, "title": ""}),
        )
        .unwrap();
        let cmds = doc_commands(&mut scheme, &doc, false).unwrap();
        assert_eq!(
            cmds,
            vec![
                163, 0, 0, 0, 0, 0, 0, 0, // INDEX_REQUEST(INIT)
                162, 0x81, 0, 0, 1, 0, 0, 0, b'1', // DOC_INDEX(id: weight1|SAVEVALUE)
            ]
        );
        for name in ["tags", "meta", "title"] {
            assert!(!scheme.has_field(name), "{name} 不该占 vno");
        }
    }

    #[test]
    fn project_name_validation_is_deferred_to_first_use() {
        // 项目名与库名同进 CMD_USE 包：`../../other_project` 曾原样发出（跳出项目 home）。
        let bad = XunSearchEngine::new("127.0.0.1:8383", "../../other_project", None);
        let err = bad.project_name().expect_err("非法项目名必须被拒");
        assert!(
            matches!(err, crate::ScoutError::InvalidIndexName(ref n) if n == "../../other_project"),
            "got {err:?}"
        );
        // 正常项目名照旧，别把合规配置一起禁掉
        let ok = XunSearchEngine::new("127.0.0.1:8383", "books", None);
        assert_eq!(ok.project_name().unwrap(), "books");
    }

    #[tokio::test]
    async fn invalid_project_fails_before_any_connection() {
        // 校验先于 I/O：非法项目名不但不该发 CMD_USE，连 connect 都不该建立 —— 校验
        // 原先挂在握手之后，后端挂着时先撞上的是 XunSearchIo，不是 InvalidIndexName。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // new() 把给定端口当 index 端口、search 取 port+1；监听器开在 search 端口上。
        let engine = XunSearchEngine::new(&format!("127.0.0.1:{}", addr.port() - 1), "../../other_project", None);
        let err = engine.search(&SearchBuilder::new("q")).await.expect_err("非法项目名必须报错");
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept())
                .await
                .is_err(),
            "非法项目名不得建立连接"
        );
    }

    #[tokio::test]
    async fn search_round_trip_with_mock_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (cmd, _, _, _) = read_packet(&mut sock).await.unwrap();
            assert_eq!(cmd, CMD_USE);
            sock.write_all(&pack_cmd(CMD_OK, 0, OK_PROJECT as u8, &[], &[])).await.unwrap();
            // index=None ≡ "default"：搜索连接也显式选库（与其余七个驱动一致）
            let (cmd, _, buf, _) = read_packet(&mut sock).await.unwrap();
            assert_eq!(cmd, CMD_INDEX_SET_DB);
            assert_eq!(buf, b"default");
            sock.write_all(&pack_cmd(CMD_OK, 0, OK_DB_CHANGED as u8, &[], &[])).await.unwrap();
            let (cmd, _, _, _) = read_packet(&mut sock).await.unwrap();
            assert_eq!(cmd, CMD_QUERY_INIT);
            let (cmd, _, buf, _) = read_packet(&mut sock).await.unwrap();
            assert_eq!(cmd, CMD_QUERY_PARSE);
            assert_eq!(String::from_utf8_lossy(&buf), "hello");
            let (cmd, _, _, buf1) = read_packet(&mut sock).await.unwrap();
            assert_eq!(cmd, CMD_SEARCH_GET_RESULT);
            assert_eq!(buf1, [0, 0, 0, 0, 10, 0, 0, 0]);
            sock.write_all(&pack_cmd(CMD_OK, 0, OK_RESULT_BEGIN as u8, &1u32.to_le_bytes(), &[])).await.unwrap();
            let mut doc = vec![1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]; // docid/rank/ccount
            doc.extend_from_slice(&100i32.to_le_bytes()); // percent
            doc.extend_from_slice(&3.5f32.to_le_bytes()); // weight
            sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_DOC, 0, 0, &doc, &[])).await.unwrap();
            sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_FIELD, 0, 0, b"one", &[])).await.unwrap();
            sock.write_all(&pack_cmd(CMD_SEARCH_RESULT_FIELD, 0, 255, b"hello world", &[])).await.unwrap();
            sock.write_all(&pack_cmd(CMD_OK, 0, OK_RESULT_END as u8, &[], &[])).await.unwrap();
        });
        // new() 把给定端口当 index 端口、search 取 port+1；监听器开在 search 端口上。
        let engine = XunSearchEngine::new(&format!("127.0.0.1:{}", addr.port() - 1), "books", None);
        let result = engine.search(&SearchBuilder::new("hello")).await.unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].id, "one");
        assert_eq!(result.hits[0].score, Some(3.5));
        assert_eq!(result.hits[0].source, serde_json::json!({"body": "hello world"}));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn empty_where_in_matches_nothing_instead_of_erroring() {
        // 回归：守卫原先数「子句」而不是「值」，于是空集合被当成「本引擎不支持
        // where_in」报 Unsupported，而基准语义是「空 IN = 不匹配任何」→ Ok(空)。
        // 指向不可达地址：真去连就会失败，返回 Ok 说明在连接之前就短路了。
        let engine = XunSearchEngine::new("127.0.0.1:1", "proj", None);
        let r = engine
            .search(&SearchBuilder::new("q").where_in("tag", Vec::<&str>::new()))
            .await
            .unwrap();
        assert!(r.hits.is_empty() && r.total == 0);
    }

    #[tokio::test]
    async fn empty_where_not_in_is_not_an_unsupported_error() {
        // 空 NOT IN = 无过滤，不该报「不支持 where_in/where_not_in」。
        // 这里只断言「不是 Unsupported」——真发查询会连不上，那是 Http/IO 类错误。
        let engine = XunSearchEngine::new("127.0.0.1:1", "proj", None);
        let err = engine
            .search(&SearchBuilder::new("q").where_not_in("tag", Vec::<&str>::new()))
            .await
            .unwrap_err();
        assert!(
            !matches!(err, crate::ScoutError::Unsupported(_)),
            "空 NOT IN 不该报 Unsupported，得到 {err:?}"
        );
    }

    /// 后端不可达（127.0.0.1:1 拒绝连接）：真去连就必然失败。库名校验必须排在
    /// connect 之前，否则这里拿到的是 XunSearchIo ——「名字非法」不该要看后端脸色，
    /// 其余七个驱动对同一个输入一律 InvalidIndexName。
    fn dead_backend() -> XunSearchEngine {
        XunSearchEngine::new("127.0.0.1:1", "proj", None)
    }

    fn doc_in(id: &str, index: &str) -> SearchDocument {
        let mut doc = SearchDocument::new(id, serde_json::json!({"title": "x"})).unwrap();
        doc.index = Some(index.to_string());
        doc
    }

    #[tokio::test]
    async fn flush_validates_the_index_name_before_connecting() {
        // 回归：flush 原先忽略 _index（既不过滤也不校验），flush("_all") 会静默成功。
        // 其余驱动都会先过 validate_index_name，这里必须一致。
        let err = dead_backend().flush("_all").await.unwrap_err();
        assert!(
            matches!(err, crate::ScoutError::InvalidIndexName(_)),
            "_all 必须在连接之前被拒，得到 {err:?}"
        );
    }

    #[tokio::test]
    async fn search_rejects_reserved_index_before_connecting() {
        let err = dead_backend().search(&SearchBuilder::new("q").within("_all")).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
        // 空 where_in 的短路排在 I/O 之前，但不能排在库名校验之前
        let err = dead_backend()
            .search(&SearchBuilder::new("q").within("_all").where_in("tag", Vec::<&str>::new()))
            .await
            .unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn paginate_rejects_reserved_index_before_connecting() {
        let err = dead_backend()
            .paginate(&SearchBuilder::new("q").within("_all"), 1, 10)
            .await
            .unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn delete_in_rejects_reserved_index_before_connecting() {
        let ids = vec!["one".to_string()];
        let err = dead_backend().delete_in("_all", &ids).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn delete_in_rejects_reserved_index_with_empty_ids() {
        // 空 id 短路原先排在库名校验之前，`delete_in("_all", &[])` 直接返回 Ok
        let err = dead_backend().delete_in("_all", &[]).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn delete_bulk_rejects_reserved_index_before_connecting() {
        let ids = vec!["one".to_string()];
        let err = dead_backend().delete_bulk("_all", &ids).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn delete_bulk_rejects_reserved_index_with_empty_ids() {
        let err = dead_backend().delete_bulk("_all", &[]).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn delete_index_rejects_reserved_index_before_connecting() {
        let err = dead_backend().delete_index("_all").await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn update_rejects_reserved_doc_index_before_connecting() {
        // update/update_bulk 的库名来自 doc.index：分组是纯计算，校验跟着一起前置
        let docs = [doc_in("one", "_all")];
        let err = dead_backend().update(&docs).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
        let err = dead_backend().update_bulk(&docs).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn create_index_rejects_reserved_index_against_both_branches() {
        // 无 ini 时是 Unsupported；保留名必须排在它前面（各驱动一致）
        let err = dead_backend().create_index("_all", serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");

        // 有 ini 时它本是 no-op（Ok）—— 保留名不能因此静默通过
        let ini = std::env::temp_dir().join(format!("rust_scout_xunsearch_{}.ini", std::process::id()));
        std::fs::write(&ini, "[pid]\ntype = id\n\n[title]\ntype = string\nindex = both\n").unwrap();
        let engine = XunSearchEngine::new("127.0.0.1:1", "proj", Some(ini.to_str().unwrap()));
        let err = engine.create_index("_all", serde_json::json!({})).await.unwrap_err();
        let _ = std::fs::remove_file(&ini);
        assert!(engine.has_ini, "临时 ini 未被解析，这条用例没测到 no-op 分支");
        assert!(matches!(err, crate::ScoutError::InvalidIndexName(_)), "got {err:?}");
    }

