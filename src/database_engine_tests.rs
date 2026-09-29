    use super::*;

    async fn engine() -> DatabaseEngine {
        // 内存库每个连接是独立的，锁死单连接保证共享同一库。
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        DatabaseEngine::from_pool(pool, vec!["title".to_string(), "body".to_string()])
    }

    fn doc(id: &str, index: Option<&str>, fields: serde_json::Value) -> SearchDocument {
        let mut d = SearchDocument::new(id, fields).unwrap();
        d.index = index.map(str::to_string);
        d
    }

    #[test]
    fn escape_like_neutralises_wildcards() {
        assert_eq!(escape_like("plain"), "plain");
        assert_eq!(escape_like("50%"), "50\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        // 反斜杠自身必须转义，否则会吃掉后插入的转义符
        assert_eq!(escape_like("a\\b"), "a\\\\b");
    }

    #[tokio::test]
    async fn pagination_is_applied_after_wheres_not_before() {
        // SQL 先 LIMIT 再内存过滤时：take(2) 取回前 2 条，两条都被 where 滤掉，
        // 结果是 0 条——而真正命中的第 3 条永远取不回来。
        let engine = engine().await;
        engine
            .update(&[
                doc("a", Some("books"), serde_json::json!({"title": "x", "cat": "news"})),
                doc("b", Some("books"), serde_json::json!({"title": "y", "cat": "news"})),
                doc("c", Some("books"), serde_json::json!({"title": "z", "cat": "tech"})),
            ])
            .await
            .unwrap();

        let r = engine
            .search(
                &SearchBuilder::new("")
                    .within("books")
                    .where_field("cat", "tech")
                    .take(2),
            )
            .await
            .unwrap();
        assert_eq!(r.hits.len(), 1, "窗口外的匹配行被 SQL LIMIT 丢掉了");
        assert_eq!(r.hits[0].id, "c");
        assert_eq!(r.total, 1, "total 应是过滤后的命中数，与 CollectionEngine 一致");

        // skip 在过滤之后生效
        let r = engine
            .search(
                &SearchBuilder::new("")
                    .within("books")
                    .order_by("title", false)
                    .skip(1)
                    .take(2),
            )
            .await
            .unwrap();
        assert_eq!(r.hits.len(), 2);
        assert_eq!(r.total, 3);
    }

    #[tokio::test]
    async fn like_wildcards_in_query_are_treated_literally() {
        let engine = engine().await;
        engine
            .update(&[
                doc("pct", None, serde_json::json!({"title": "50% off"})),
                doc("num", None, serde_json::json!({"title": "50123 items"})),
            ])
            .await
            .unwrap();

        // "%" 是字面量：只应命中真正含 "50%" 的那条，total 不该把 "50123" 算进去
        let r = engine.search(&SearchBuilder::new("50%")).await.unwrap();
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].id, "pct");
        assert_eq!(r.total, 1, "SQL 层 total 不应被 LIKE 通配符放大");
    }

    #[tokio::test]
    async fn update_then_search_finds_matches() {
        let e = engine().await;
        e.update(&[doc(
            "one",
            Some("books"),
            serde_json::json!({"title": "Hello world", "body": "intro"}),
        )])
        .await
        .unwrap();
        let result = e
            .search(&SearchBuilder::new("hello").within("books"))
            .await
            .unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.hits[0].id, "one");
        assert_eq!(result.hits[0].source["title"], "Hello world");
    }

    #[tokio::test]
    async fn like_filter_is_scoped_to_index() {
        let e = engine().await;
        e.update(&[
            doc("one", Some("books"), serde_json::json!({"title": "Rust"})),
            doc("two", Some("movies"), serde_json::json!({"title": "Rust"})),
        ])
        .await
        .unwrap();
        let result = e
            .search(&SearchBuilder::new("rust").within("books"))
            .await
            .unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.hits[0].id, "one");
    }

    #[tokio::test]
    async fn wheres_filtered_total_matches_collection_engine() {
        // 这个用例原先断言 total == 2（SQL 层计数），把「SQL 先截断」的 bug 当成
        // 预期行为锁住了。现在两个驱动必须给出同样的结论。
        let docs = [
            doc(
                "one",
                Some("books"),
                serde_json::json!({"title": "Rust", "category": "tech"}),
            ),
            doc(
                "two",
                Some("books"),
                serde_json::json!({"title": "Rust", "category": "fiction"}),
            ),
        ];
        let e = engine().await;
        e.update(&docs).await.unwrap();

        let builder = SearchBuilder::new("rust")
            .within("books")
            .where_field("category", "tech");
        let db_result = e.search(&builder).await.unwrap();

        let reference = crate::CollectionEngine::new();
        reference.update(&docs).await.unwrap();
        let ref_result = reference.search(&builder).await.unwrap();

        assert_eq!(db_result.hits.len(), 1);
        assert_eq!(db_result.hits[0].id, "one");
        assert_eq!(
            db_result.total, ref_result.total,
            "database 与 collection 的 total 必须一致（都是过滤后的命中数）"
        );
        assert_eq!(db_result.total, 1);
    }

    #[tokio::test]
    async fn soft_delete_three_states() {
        let e = engine().await;
        e.update(&[
            doc("one", Some("books"), serde_json::json!({"title": "Alpha"})),
            doc("two", Some("books"), serde_json::json!({"title": "Beta"})),
        ])
        .await
        .unwrap();
        e.soft_delete(&["one".to_string()]).await.unwrap();

        let excluded = e
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        assert_eq!(excluded.hits.len(), 1);
        assert_eq!(excluded.hits[0].id, "two");

        let only = e
            .search(&SearchBuilder::new("").within("books").only_trashed())
            .await
            .unwrap();
        assert_eq!(only.hits.len(), 1);
        assert_eq!(only.hits[0].id, "one");

        let all = e
            .search(&SearchBuilder::new("").within("books").with_trashed())
            .await
            .unwrap();
        assert_eq!(all.hits.len(), 2);
    }

    #[tokio::test]
    async fn reindex_moves_documents_and_empties_source() {
        // 钉住**移动**语义（不是复制）：本驱动 id 全局主键，复制做不到，见
        // reindex_impl 注释。这个测试存在的意义就是让「from 被清空」是写下来的
        // 契约，而不是将来读代码时才发现的事故。
        let e = engine().await;
        e.update(&[doc("one", Some("books"), serde_json::json!({"title": "Rust"}))])
            .await
            .unwrap();
        e.reindex("books", "archive").await.unwrap();
        let archive = e
            .search(&SearchBuilder::new("").within("archive"))
            .await
            .unwrap();
        assert_eq!(archive.total, 1);
        assert_eq!(archive.hits[0].id, "one");
        let books = e
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        assert_eq!(books.total, 0, "database reindex 是移动：from 会清空");

        // 重叠重索引不会撞主键：id 全局唯一，同一 id 在 to 里不可能另有一行。
        // （原注释声称会「整批中止」，这个冲突在本 schema 下构造不出来。）
        e.update(&[doc("one", Some("books"), serde_json::json!({"title": "Rust"}))])
            .await
            .unwrap();
        e.reindex("books", "archive").await.unwrap();
        let archive = e
            .search(&SearchBuilder::new("").within("archive"))
            .await
            .unwrap();
        assert_eq!(archive.total, 1);
        assert_eq!(
            e.search(&SearchBuilder::new("").within("books"))
                .await
                .unwrap()
                .total,
            0
        );
    }

    #[tokio::test]
    async fn delete_index_purges_only_that_index() {
        // README 的索引生命周期是 update → delete_index → search。此前 delete_index
        // 是 no-op，照文档跑一遍旧文档全都还在。
        let e = engine().await;
        e.update(&[
            doc("one", Some("books"), serde_json::json!({"title": "Rust"})),
            doc("two", Some("movies"), serde_json::json!({"title": "Rust"})),
        ])
        .await
        .unwrap();

        e.delete_index("books").await.unwrap();
        let books = e
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        assert_eq!(books.total, 0);
        let movies = e
            .search(&SearchBuilder::new("").within("movies"))
            .await
            .unwrap();
        assert_eq!(movies.total, 1, "delete_index 只清自己的索引");

        // 幂等：再删一次仍是 Ok（删 0 行），生命周期示例可以重复跑
        e.delete_index("books").await.unwrap();
    }

    #[tokio::test]
    async fn delete_paths_reject_reserved_index_name_in_both_in_process_drivers() {
        // 同一个保留名必须在两个进程内驱动上给出同一个答案，且与
        // ES/Meilisearch/Typesense/Algolia/XunSearch 一致。
        let e = engine().await;
        let fields = serde_json::json!({"title": "Rust"});
        e.update(&[doc("one", Some("books"), fields.clone())])
            .await
            .unwrap();

        let ids = vec!["one".to_string()];
        let db_errors = [
            e.delete_in("_all", &ids).await.unwrap_err(),
            e.delete_index("_all").await.unwrap_err(),
            e.soft_delete_in("_all", &ids).await.unwrap_err(),
            e.create_index("_all", serde_json::json!({}))
                .await
                .unwrap_err(),
        ];

        let reference = crate::CollectionEngine::new();
        reference
            .update(&[doc("one", Some("books"), fields)])
            .await
            .unwrap();
        let collection_errors = [
            reference.delete_in("_all", &ids).await.unwrap_err(),
            reference.delete_index("_all").await.unwrap_err(),
            reference.soft_delete_in("_all", &ids).await.unwrap_err(),
            reference
                .create_index("_all", serde_json::json!({}))
                .await
                .unwrap_err(),
        ];

        for err in db_errors.into_iter().chain(collection_errors) {
            assert!(
                matches!(err, crate::ScoutError::InvalidIndexName(_)),
                "expected InvalidIndexName, got {err:?}"
            );
        }

        // 被拒 = 什么都没发生
        assert_eq!(
            e.search(&SearchBuilder::new("").within("books"))
                .await
                .unwrap()
                .total,
            1
        );
    }

    #[tokio::test]
    async fn bulk_delete_and_soft_delete_skip_unknown_ids() {
        // 事务化批量写不能顺手改语义：不存在的 id 静默跳过（不是 Err），已有的照删，
        // 重复 id 也不炸——这条在改成事务前后必须完全一样。
        let e = engine().await;
        e.update(&[
            doc("one", Some("books"), serde_json::json!({"title": "Rust"})),
            doc("two", Some("books"), serde_json::json!({"title": "Rust"})),
        ])
        .await
        .unwrap();

        e.delete(&["ghost".to_string()]).await.unwrap();
        e.delete_in("books", &["one".to_string(), "ghost".to_string()])
            .await
            .unwrap();
        let remaining = e
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        assert_eq!(remaining.total, 1);
        assert_eq!(remaining.hits[0].id, "two");

        e.soft_delete(&["two".to_string(), "ghost".to_string(), "two".to_string()])
            .await
            .unwrap();
        e.soft_delete_in("books", &["ghost".to_string()])
            .await
            .unwrap();
        let trashed = e
            .search(&SearchBuilder::new("").within("books").only_trashed())
            .await
            .unwrap();
        assert_eq!(trashed.total, 1);
        assert_eq!(trashed.hits[0].id, "two");
        assert_eq!(
            e.search(&SearchBuilder::new("").within("books"))
                .await
                .unwrap()
                .total,
            0
        );
    }

    #[tokio::test]
    async fn delete_removes_by_id_across_indexes() {
        let e = engine().await;
        e.update(&[
            doc("one", Some("books"), serde_json::json!({"title": "Rust"})),
            doc("two", Some("movies"), serde_json::json!({"title": "Rust"})),
        ])
        .await
        .unwrap();
        e.delete(&["one".to_string()]).await.unwrap();
        let books = e
            .search(&SearchBuilder::new("").within("books"))
            .await
            .unwrap();
        assert_eq!(books.total, 0);
        let movies = e
            .search(&SearchBuilder::new("").within("movies"))
            .await
            .unwrap();
        assert_eq!(movies.total, 1);
    }
