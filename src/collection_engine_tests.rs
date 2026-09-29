    use super::*;

    fn doc(id: &str, index: Option<&str>) -> SearchDocument {
        let mut d = SearchDocument::new(id, serde_json::json!({"title": id})).unwrap();
        d.index = index.map(str::to_string);
        d
    }

    #[tokio::test]
    async fn update_respects_doc_index() {
        let engine = CollectionEngine::new();
        engine
            .update(&[doc("one", Some("books")), doc("two", None)])
            .await
            .unwrap();
        let result = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        let ids: Vec<&str> = result.hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["one"]);
    }

    #[tokio::test]
    async fn flush_keeps_data() {
        let engine = CollectionEngine::new();
        engine.update(&[doc("one", Some("books"))]).await.unwrap();
        engine.flush("books").await.unwrap();
        let result = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        assert_eq!(result.total, 1);
    }

    #[tokio::test]
    async fn delete_in_only_removes_from_given_index() {
        let engine = CollectionEngine::new();
        engine
            .update(&[doc("one", Some("books")), doc("one", Some("movies"))])
            .await
            .unwrap();
        engine.delete_in("books", &["one".to_string()]).await.unwrap();
        let books = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        let movies = engine
            .search(&SearchBuilder::new("").within("movies"))
            .await
            .unwrap();
        assert_eq!(books.total, 0);
        assert_eq!(movies.total, 1);
    }

    #[tokio::test]
    async fn windowing_matches_full_sort_then_truncate() {
        // 「先排引用、再物化窗口」这个优化不能改变结果的顺序或 total：
        // take(k) 必须等于「全排序后取前 k 条」，skip 同理，total 与窗口无关。
        let engine = CollectionEngine::new();
        let docs: Vec<SearchDocument> = (0..40)
            .map(|i| {
                let mut d = doc(&format!("d{i:02}"), Some("books"));
                // 故意造重复值：排序的并列项要靠 id 兜底，最容易在这里错
                d.fields
                    .insert("rank".into(), serde_json::json!((i * 7) % 10));
                d
            })
            .collect();
        engine.update(&docs).await.unwrap();

        for desc in [false, true] {
            let all = engine
                .search(
                    &SearchBuilder::new("")
                        .within("books")
                        .order_by("rank", desc),
                )
                .await
                .unwrap();
            assert_eq!(all.total, 40);
            assert_eq!(all.hits.len(), 40);

            for (offset, take) in [(0, 3), (0, 10), (5, 7), (37, 10), (39, 5)] {
                let page = engine
                    .search(
                        &SearchBuilder::new("")
                            .within("books")
                            .order_by("rank", desc)
                            .skip(offset)
                            .take(take),
                    )
                    .await
                    .unwrap();
                let expected: Vec<&str> = all
                    .hits
                    .iter()
                    .skip(offset)
                    .take(take)
                    .map(|h| h.id.as_str())
                    .collect();
                let got: Vec<&str> = page.hits.iter().map(|h| h.id.as_str()).collect();
                assert_eq!(
                    got, expected,
                    "desc={desc} skip={offset} take={take}: 窗口与全排序不一致"
                );
                // total 始终是过滤后的总数，不随分页变小
                assert_eq!(page.total, 40, "total 不该被 take 截断");
            }
        }
    }

    #[tokio::test]
    async fn delete_paths_reject_reserved_index_name() {
        // 契约统一：保留名（`_all` 等）在进程内驱动上也要报 InvalidIndexName，
        // 不能静默 Ok（ES 上 delete_index("_all") 会删掉整个集群）。
        let engine = CollectionEngine::new();
        engine.update(&[doc("one", Some("books"))]).await.unwrap();
        let ids = vec!["one".to_string()];

        for err in [
            engine.delete_in("_all", &ids).await.unwrap_err(),
            engine.soft_delete_in("_all", &ids).await.unwrap_err(),
            engine.delete_index("_all").await.unwrap_err(),
            engine
                .create_index("_all", serde_json::json!({}))
                .await
                .unwrap_err(),
        ] {
            assert!(
                matches!(err, crate::ScoutError::InvalidIndexName(_)),
                "expected InvalidIndexName, got {err:?}"
            );
        }

        // 被拒 = 什么都没发生：文档仍在，且没被顺手软删
        let result = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        assert_eq!(result.total, 1);
    }

    #[tokio::test]
    async fn read_and_write_paths_reject_reserved_index_name() {
        // 与五个网络驱动同契约：保留名不只 delete 那几条路径要拒，
        // search / paginate / update(doc.index) / reindex 也一样
        // （此前这些路径全都静默 Ok）。database 侧的同名断言在
        // database_engine_tests.rs，两个驱动一起钉住。
        let engine = CollectionEngine::new();
        engine.update(&[doc("keep", Some("books"))]).await.unwrap();
        let bad = doc("one", Some("_all"));

        let errors = [
            engine
                .search(&SearchBuilder::new("q").within("_all"))
                .await
                .unwrap_err(),
            engine
                .paginate(&SearchBuilder::new("q").within("_all"), 1, 10)
                .await
                .unwrap_err(),
            engine.update(std::slice::from_ref(&bad)).await.unwrap_err(),
            engine.reindex("_all", "archive").await.unwrap_err(),
            engine.reindex("books", "_all").await.unwrap_err(),
        ];
        for err in errors {
            assert!(
                matches!(err, crate::ScoutError::InvalidIndexName(_)),
                "expected InvalidIndexName, got {err:?}"
            );
        }

        // 被拒 = 什么都没发生：books 里还是只有那条合法文档，_all 的写入没落进来
        let result = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.hits[0].id, "keep");
    }

    #[tokio::test]
    async fn reindex_copies_docs_keeps_source() {
        let engine = CollectionEngine::new();
        engine.update(&[doc("one", Some("books"))]).await.unwrap();
        engine.reindex("books", "archive").await.unwrap();
        let source = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        let copy = engine
            .search(&SearchBuilder::new("").within("archive"))
            .await
            .unwrap();
        assert_eq!(source.total, 1);
        assert_eq!(copy.total, 1);
    }

    #[tokio::test]
    async fn reindex_missing_source_creates_empty_target() {
        let engine = CollectionEngine::new();
        engine.reindex("nope", "target").await.unwrap();
        let result = engine
            .search(&SearchBuilder::new("").within("target"))
            .await
            .unwrap();
        assert_eq!(result.total, 0);
    }

    #[tokio::test]
    async fn soft_delete_filters_per_trashed() {
        let engine = CollectionEngine::new();
        engine
            .update(&[doc("one", Some("books")), doc("two", Some("books"))])
            .await
            .unwrap();
        engine.soft_delete(&["one".to_string()]).await.unwrap();

        let excluded = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        assert_eq!(excluded.total, 1);
        assert_eq!(excluded.hits[0].id, "two");

        let only = engine
            .search(&SearchBuilder::new("").within("books").only_trashed())
            .await
            .unwrap();
        assert_eq!(only.total, 1);
        assert_eq!(only.hits[0].id, "one");

        let all = engine
            .search(&SearchBuilder::new("").within("books").with_trashed())
            .await
            .unwrap();
        assert_eq!(all.total, 2);
    }

    #[tokio::test]
    async fn soft_delete_filter_ignores_non_true_markers() {
        // 只有 `__soft_deleted == true` 才算软删除：false/字符串/缺失值在
        // Exclude 下可见、OnlyTrashed 下不可见（与 ES term 语义对齐）。
        let engine = CollectionEngine::new();
        let mut marked_false = doc("false", Some("books"));
        marked_false.set("__soft_deleted", false);
        let mut marked_str = doc("str", Some("books"));
        marked_str.set("__soft_deleted", "x");
        let mut marked_true = doc("gone", Some("books"));
        marked_true.set("__soft_deleted", true);
        engine.update(&[marked_false, marked_str, marked_true]).await.unwrap();

        let excluded = engine
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        let ids: Vec<&str> = excluded.hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["false", "str"]);

        let only = engine
            .search(&SearchBuilder::new("").within("books").only_trashed())
            .await
            .unwrap();
        let ids: Vec<&str> = only.hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["gone"]);
    }
