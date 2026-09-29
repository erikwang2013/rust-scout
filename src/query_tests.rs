    use super::*;
    use serde_json::json;

    #[test]
    fn build_query_translates_all_conditions() {
        let builder = SearchBuilder::new("rust")
            .where_field("status", "active")
            .where_in("tags", ["a", "b"])
            .where_not_in("deleted", [true]);
        assert_eq!(
            build_query(&builder),
            json!({
                "bool": {
                    "must": [{"query_string": {"query": "rust", "lenient": true}}],
                    "filter": [
                        {"term": {"status": "active"}},
                        {"terms": {"tags": ["a", "b"]}},
                        {"bool": {"must_not": [{"term": {"__soft_deleted": true}}]}}
                    ],
                    "must_not": [{"terms": {"deleted": [true]}}]
                }
            })
        );
    }

    #[test]
    fn build_query_empty_builder_matches_all() {
        assert_eq!(
            build_query(&SearchBuilder::default().with_trashed()),
            json!({"match_all": {}})
        );
    }

    #[test]
    fn build_body_adds_sort_orders() {
        let builder = SearchBuilder::default()
            .with_trashed()
            .order_by("created_at", true)
            .order_by("title", false);
        assert_eq!(
            build_body(&builder, 5, 20).expect("无保留键不该报错"),
            json!({
                "query": {"match_all": {}},
                "from": 5,
                "size": 20,
                "track_total_hits": true,
                "sort": [
                    {"created_at": {"order": "desc"}},
                    {"title": {"order": "asc"}}
                ]
            })
        );
    }

    #[test]
    fn build_body_defaults_track_total_hits_but_leaves_it_overridable() {
        // 不设 track_total_hits 时 ES 只精确统计前 10000 条，超出恒报 10000；
        // 其余驱动都返回真实 total。
        let body = build_body(&SearchBuilder::default(), 0, 10).expect("无保留键不该报错");
        assert_eq!(body["track_total_hits"], json!(true));
        // track_total_hits 不在保留键内，options 仍能覆盖默认值。
        let builder = SearchBuilder::default().option("track_total_hits", 500);
        assert_eq!(
            build_body(&builder, 0, 10).expect("无保留键不该报错")["track_total_hits"],
            json!(500)
        );
    }

    #[test]
    fn build_query_marks_query_string_lenient() {
        // lenient 覆盖「文本查数值字段」这类错误；查询串原样透传，语法由调用方负责
        // （语法错误在引擎层按无命中处理，见 is_query_parse_error 的测试）。
        let query = build_query(&SearchBuilder::new("("));
        let clause = &query["bool"]["must"][0]["query_string"];
        assert_eq!(clause["query"], json!("("));
        assert_eq!(clause["lenient"], json!(true));
    }

    #[test]
    fn parse_search_response_extracts_total_aggregations_and_highlight() {
        let raw = json!({
            "hits": {
                "total": {"value": 42},
                "hits": [{
                    "_id": "a",
                    "_score": 1.5,
                    "_source": {"title": "x"},
                    "highlight": {"title": ["<em>x</em>"]}
                }]
            },
            "aggregations": {"by_tag": {"buckets": []}}
        });
        let result = parse_search_response(&raw);
        assert_eq!(result.total, 42);
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].id, "a");
        assert_eq!(result.hits[0].score, Some(1.5));
        assert_eq!(result.hits[0].source, json!({"title": "x"}));
        assert_eq!(result.hits[0].highlight, Some(json!({"title": ["<em>x</em>"]})));
        assert_eq!(result.aggregations, Some(json!({"by_tag": {"buckets": []}})));
        assert!(result.facets.is_none());
    }

    #[test]
    fn parse_search_response_falls_back_to_hit_count() {
        // total 缺失 → 回退 hits 长度；_source 缺失 → Null。
        let raw = json!({"hits": {"hits": [{"_id": "a"}, {"_id": "b"}]}});
        let result = parse_search_response(&raw);
        assert_eq!(result.total, 2);
        assert_eq!(result.hits[0].source, serde_json::Value::Null);
        assert!(result.aggregations.is_none());
        assert!(result.facets.is_none());
    }

    #[test]
    fn parse_search_response_extracts_facets() {
        let raw = json!({"hits": {"hits": []}, "facets": {"tags": {}}});
        let result = parse_search_response(&raw);
        assert_eq!(result.facets, Some(json!({"tags": {}})));
    }

    #[test]
    fn build_body_merges_non_reserved_options() {
        // options 是透传口：非保留键原样进请求体，query/from/size/sort 由 builder 决定。
        let builder = SearchBuilder::default()
            .with_trashed()
            .option("track_total_hits", true)
            .option("custom", json!({"a": 1}))
            .take(3)
            .order_by("title", false);
        let body = build_body(&builder, 5, 20).expect("非保留键不该报错");
        assert_eq!(body["size"], 20);
        assert_eq!(body["from"], 5);
        assert_eq!(body["query"], json!({"match_all": {}}));
        assert_eq!(body["sort"], json!([{"title": {"order": "asc"}}]));
        assert_eq!(body["track_total_hits"], true);
        assert_eq!(body["custom"], json!({"a": 1}));
    }

    #[test]
    fn build_body_errors_on_reserved_options_instead_of_silently_dropping_them() {
        // 曾经 option("query", …) 被 build_query 的结果覆盖成 match_all：调用方以为加了
        // 过滤条件，拿到的是全量未过滤结果，且没有任何报错。现在必须报错并点名替代方法。
        for (key, hint) in [("query", "query body"), ("from", "skip()"), ("size", "take()")] {
            let builder = SearchBuilder::default().option(key, json!(1));
            let err = build_body(&builder, 0, 10).expect_err(key);
            assert!(
                matches!(err, crate::ScoutError::Unsupported(_)),
                "{key}: got {err:?}"
            );
            let message = err.to_string();
            assert!(message.contains(key), "{key} 的错误信息要点名键：{message}");
            assert!(
                message.contains(hint),
                "{key} 的错误信息要指出替代方法：{message}"
            );
        }
    }

    #[test]
    fn build_body_rejects_sort_only_when_order_by_overrides_it() {
        // 单独用 option 传 sort、没配 .order_by() 时它原样生效 —— 没被丢弃就不拦。
        let passed_through =
            SearchBuilder::default().option("sort", json!([{"a": {"order": "desc"}}]));
        assert_eq!(
            build_body(&passed_through, 0, 10).expect("单独的 sort 仍然生效")["sort"],
            json!([{"a": {"order": "desc"}}])
        );
        // 同时有 .order_by() 时 options 里的 sort 被覆盖，属于静默丢弃，必须报错。
        let overridden = SearchBuilder::default()
            .order_by("title", false)
            .option("sort", json!([{"a": {"order": "desc"}}]));
        let err = build_body(&overridden, 0, 10).expect_err("被覆盖的 sort 必须报错");
        assert!(err.to_string().contains("order_by"), "got {err}");
    }

    #[test]
    fn build_body_activates_highlight_from_options() {
        let builder = SearchBuilder::default().option("highlight", true);
        let body = build_body(&builder, 0, 10).expect("无保留键不该报错");
        assert_eq!(body["highlight"], json!({"fields": {"*": {}}}));
    }

    #[test]
    fn build_body_passes_through_highlight_object() {
        let builder = SearchBuilder::default()
            .option("highlight", json!({"fields": {"title": {}}, "pre_tags": ["<b>"]}));
        let body = build_body(&builder, 0, 10).expect("无保留键不该报错");
        assert_eq!(
            body["highlight"],
            json!({"fields": {"title": {}}, "pre_tags": ["<b>"]})
        );
    }

    #[test]
    fn build_body_drops_non_highlight_value() {
        let body = build_body(&SearchBuilder::default().option("highlight", false), 0, 10)
            .expect("无保留键不该报错");
        assert!(body.get("highlight").is_none());
    }

    #[test]
    fn build_query_trashed_default_excludes_soft_deleted() {
        let query = build_query(&SearchBuilder::default());
        assert_eq!(
            query,
            json!({
                "bool": {
                    "must": [],
                    "filter": [{"bool": {"must_not": [{"term": {"__soft_deleted": true}}]}}],
                    "must_not": []
                }
            })
        );
    }

    #[test]
    fn build_query_only_trashed_uses_term() {
        let query = build_query(&SearchBuilder::default().only_trashed());
        assert_eq!(
            query,
            json!({
                "bool": {
                    "must": [],
                    "filter": [{"term": {"__soft_deleted": true}}],
                    "must_not": []
                }
            })
        );
    }

    #[test]
    fn build_query_with_trashed_no_filter() {
        assert_eq!(
            build_query(&SearchBuilder::default().with_trashed()),
            json!({"match_all": {}})
        );
    }

    #[test]
    fn is_query_parse_error_matches_real_400_body() {
        // 实测抓取：OpenSearch 2.19 对 query_string:"(" 的响应（lenient:true 也一样 400）。
        let raw = json!({
            "error": {
                "root_cause": [{
                    "type": "query_shard_exception",
                    "reason": "Failed to parse query [(]",
                    "index": "verify"
                }],
                "type": "search_phase_execution_exception",
                "reason": "all shards failed",
                "phase": "query",
                "failed_shards": [{
                    "shard": 0,
                    "reason": {
                        "type": "query_shard_exception",
                        "reason": "Failed to parse query [(]",
                        "caused_by": {
                            "type": "parse_exception",
                            "reason": "Cannot parse '(': Encountered \"<EOF>\" at line 1, column 1."
                        }
                    }
                }]
            },
            "status": 400
        });
        assert!(is_query_parse_error(&raw.to_string()));
    }

    #[test]
    fn is_query_parse_error_ignores_other_failures() {
        // 同为 shard 级失败但语义是「建不出查询」，不是语法错，不得吞掉。
        let other_shard_error = json!({
            "error": {"root_cause": [{"type": "query_shard_exception", "reason": "failed to create query: x"}]}
        });
        assert!(!is_query_parse_error(&other_shard_error.to_string()));
        // 请求级错误（映射、参数非法等）同样不吞。
        let mapping_error = json!({
            "error": {"root_cause": [{"type": "illegal_argument_exception", "reason": "bad"}]}
        });
        assert!(!is_query_parse_error(&mapping_error.to_string()));
        assert!(!is_query_parse_error("not json"));
        assert!(!is_query_parse_error("{}"));
    }

    #[test]
    fn bulk_delete_tolerates_404_but_regular_bulk_does_not() {
        // ES 对「索引不存在」的删除项回 index_not_found_exception（带 error 键）。
        // delete_in / 其余驱动的 delete_bulk 都是幂等的，删除侧必须同样放行 404，
        // 否则同一个「重复删除」在别的驱动返回 Ok、在 ES 上返回 Err。
        let response = serde_json::json!({
            "items": [
                {"delete": {"_id": "gone", "status": 404,
                            "error": {"type": "index_not_found_exception"}}},
                {"delete": {"_id": "ok", "status": 200, "result": "deleted"}}
            ]
        });
        assert!(check_bulk_delete_items(&response).is_ok());
        // 写入侧语义不变：同样的 404 项仍然报错
        assert!(check_bulk_items(&response).is_err());
    }

    #[test]
    fn check_bulk_items_reports_failed_item_id() {
        let response = json!({
            "items": [
                {"index": {"_id": "ok", "status": 201}},
                {"index": {"_id": "bad", "status": 400, "error": {"type": "mapper_parsing_exception", "reason": "boom"}}}
            ]
        });
        let err = check_bulk_items(&response).unwrap_err();
        assert!(
            err.to_string().contains("bad") && err.to_string().contains("boom"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn check_bulk_items_ok_on_all_success() {
        let response = json!({"items": [{"index": {"_id": "a", "status": 201}}]});
        assert!(check_bulk_items(&response).is_ok());
    }
