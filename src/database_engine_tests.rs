    use super::*;

    async fn engine_with(searchable_fields: Vec<String>) -> DatabaseEngine {
        // 内存库每个连接是独立的，锁死单连接保证共享同一库。
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        DatabaseEngine::from_pool(pool, searchable_fields)
    }

    async fn engine() -> DatabaseEngine {
        engine_with(vec!["title".to_string(), "body".to_string()]).await
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
    async fn searchable_fields_must_not_narrow_what_text_search_covers() {
        // 回归：粗筛只需是内存 matches() 的**超集**，但它必须是超集 —— 漏掉
        // matches() 本会保留的行，就是静默的错误答案。
        //
        // 曾经 `searchable` 列只拼「配置字段的值」，而 matches() 搜的是整份序列化
        // 文档，于是下面这个场景 database 返回 0 条、collection 返回 1 条 ——
        // 同一份文档、同一个查询，两个驱动两个答案。
        let e = engine_with(vec!["title".to_string()]).await;
        let reference = crate::CollectionEngine::new();
        let d = doc(
            "one",
            Some("books"),
            serde_json::json!({"title": "Rust", "tag": "async"}),
        );
        e.update(std::slice::from_ref(&d)).await.unwrap();
        reference.update(std::slice::from_ref(&d)).await.unwrap();

        // "async" 只在未配置字段的值里；"tag" 是字段名。两者 matches() 都命中。
        for q in ["async", "tag", "rust"] {
            let db = e.search(&SearchBuilder::new(q).within("books")).await.unwrap();
            let mem = reference
                .search(&SearchBuilder::new(q).within("books"))
                .await
                .unwrap();
            assert_eq!(
                db.total, mem.total,
                "查询 {q:?} 上 database({}) 与 collection({}) 不一致",
                db.total, mem.total
            );
            assert_eq!(db.total, 1, "查询 {q:?} 本该命中");
        }
    }

    #[tokio::test]
    async fn text_query_works_without_configured_searchable_fields() {
        // `database.fields` 没配时 searchable_fields 是空 Vec（README 里没有任何一处
        // 提到这个键，所以这是默认路径），而 `searchable` 列由 write_all 按
        // searchable_fields 拼出 —— 空配置下整列恒为 ""，`searchable LIKE '%q%'`
        // 一条都匹配不上，于是**每个文本查询都静默返回 0 条**。跳过空预筛退回内存
        // 匹配：慢，但答案是对的。
        let e = engine_with(Vec::new()).await;
        let docs = [
            doc("one", Some("books"), serde_json::json!({"title": "Hello world"})),
            doc("two", Some("books"), serde_json::json!({"title": "Goodbye"})),
        ];
        e.update(&docs).await.unwrap();

        let builder = SearchBuilder::new("hello").within("books");
        let got = e.search(&builder).await.unwrap();
        assert_eq!(got.total, 1, "没配 searchable_fields 不是「搜不到」");
        assert_eq!(got.hits[0].id, "one");

        // 同一个输入，两个进程内驱动必须同答案
        let reference = crate::CollectionEngine::new();
        reference.update(&docs).await.unwrap();
        let want = reference.search(&builder).await.unwrap();
        assert_eq!(got.total, want.total);
        assert_eq!(got.hits[0].id, want.hits[0].id);
    }

    #[tokio::test]
    async fn windowing_matches_full_sort_then_truncate() {
        // 与 CollectionEngine 的同名用例对应：top-K 分区（select_nth_unstable_by）
        // 之后再排序，必须与「全排序后取窗口」逐条一致，total 与窗口无关。
        // 不配 order_by 时全靠 id 升序，是并列项最容易错的地方。
        let e = engine().await;
        let docs: Vec<SearchDocument> = (0..40)
            .map(|i| {
                let mut d = doc(
                    &format!("d{i:02}"),
                    Some("books"),
                    serde_json::json!({"title": "Rust"}),
                );
                // 故意造重复值：并列项要靠 id 兜底，top-K 的边界最容易在这里错
                d.fields
                    .insert("rank".into(), serde_json::json!((i * 7) % 10));
                d
            })
            .collect();
        e.update(&docs).await.unwrap();

        for desc in [false, true] {
            let all = e
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
                let page = e
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
                assert_eq!(page.total, 40, "total 不该被 take 截断");
            }
        }
    }

    #[tokio::test]
    async fn schema_is_created_on_first_operation_only() {
        // 首次操作建表（CREATE ... IF NOT EXISTS），之后走 AtomicBool 短路。
        // 直接问 sqlite_master：表必须真在，不能只验「没报错」。
        let e = engine().await;
        e.update(&[doc("one", Some("books"), serde_json::json!({"title": "Rust"}))])
            .await
            .unwrap();
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'scout_documents'",
        )
        .fetch_all(&e.pool)
        .await
        .unwrap();
        assert_eq!(names, ["scout_documents"]);
        assert_eq!(
            e.search(&SearchBuilder::new("Rust").within("books"))
                .await
                .unwrap()
                .total,
            1
        );

        // 短路的代价：外部绕过驱动删表后不再自愈（有意取舍，见 ensure_schema 注释）。
        // 这条钉住取舍本身 —— 要恢复自愈，就连注释一起改。
        sqlx::query("DROP TABLE scout_documents")
            .execute(&e.pool)
            .await
            .unwrap();
        assert!(e
            .search(&SearchBuilder::new("").within("books"))
            .await
            .is_err());
    }

    /// 保留名在四个入口上的错误，两个进程内驱动各跑一遍。
    async fn reserved_name_errors(engine: &dyn Engine) -> Vec<crate::ScoutError> {
        let bad = doc("one", Some("_all"), serde_json::json!({"title": "Rust"}));
        vec![
            engine
                .search(&SearchBuilder::new("q").within("_all"))
                .await
                .unwrap_err(),
            engine
                .paginate(&SearchBuilder::new("q").within("_all"), 1, 10)
                .await
                .unwrap_err(),
            engine.update(std::slice::from_ref(&bad)).await.unwrap_err(),
            engine
                .update_bulk(std::slice::from_ref(&bad))
                .await
                .unwrap_err(),
            engine.reindex("_all", "books").await.unwrap_err(),
            engine.reindex("books", "_all").await.unwrap_err(),
        ]
    }

    #[tokio::test]
    async fn reserved_index_name_rejected_on_search_paginate_update_reindex() {
        // 契约统一：五个网络驱动在 search / paginate / update(doc.index) / reindex
        // 上都拒保留名，两个进程内驱动不能例外 —— 否则同一输入在 ES 上 Err、
        // 在这里 Ok。两个驱动一起断言，才不会再次各走各的。
        let db = engine().await;
        let collection = crate::CollectionEngine::new();

        let mut errors = reserved_name_errors(&db).await;
        errors.extend(reserved_name_errors(&collection).await);
        assert_eq!(errors.len(), 12, "两个驱动 × 六个入口");
        for err in errors {
            assert!(
                matches!(err, crate::ScoutError::InvalidIndexName(_)),
                "expected InvalidIndexName, got {err:?}"
            );
        }

        // 被拒 = 什么都没发生，且正常索引名照旧可用（别把校验做成一刀切）
        let good = doc("one", Some("books"), serde_json::json!({"title": "Rust"}));
        for e in [&db as &dyn Engine, &collection as &dyn Engine] {
            e.update(std::slice::from_ref(&good)).await.unwrap();
            let result = e
                .search(&SearchBuilder::new("").within("books"))
                .await
                .unwrap();
            assert_eq!(result.total, 1);
            assert_eq!(result.hits[0].id, "one");
        }
    }

    #[tokio::test]
    async fn reserved_index_name_is_rejected_before_any_database_access() {
        // 校验必须排在 ensure_schema / 取连接之前：后端不可用时（这个路径不存在，
        // 连接在第一次查询时才失败）也要给 InvalidIndexName，而不是 I/O 错误。
        let e = DatabaseEngine::new("sqlite:///nonexistent-dir-rust-scout/db.sqlite", Vec::new())
            .expect("connect_lazy 不在构造时建连");
        for err in [
            e.search(&SearchBuilder::new("q").within("_all"))
                .await
                .unwrap_err(),
            e.paginate(&SearchBuilder::new("q").within("_all"), 1, 10)
                .await
                .unwrap_err(),
            e.update_bulk(&[doc("one", Some("_all"), serde_json::json!({"title": "x"}))])
                .await
                .unwrap_err(),
            e.reindex("books", "_all").await.unwrap_err(),
        ] {
            assert!(
                matches!(err, crate::ScoutError::InvalidIndexName(_)),
                "got {err:?}"
            );
        }

        // 反证：同一个坏后端上，正常索引名确实会在 I/O 上失败 —— 否则上面那组断言
        // 可能只是因为「这个后端什么都失败」。
        assert!(!matches!(
            e.search(&SearchBuilder::new("q").within("books"))
                .await
                .unwrap_err(),
            crate::ScoutError::InvalidIndexName(_)
        ));
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
