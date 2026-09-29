    use super::*;

    #[test]
    fn pack_cmd_matches_php_format() {
        // 对照 PHP SDK pack('CCCCIN', ...)：cmd/arg1/arg2/blen1 各 1 字节，blen u32le
        assert_eq!(
            pack_cmd(CMD_QUERY_INIT, 0, 0, &[], &[]),
            vec![CMD_QUERY_INIT, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            pack_cmd(1, 2, 3, b"hi", b"x"),
            vec![1, 2, 3, 1, 2, 0, 0, 0, b'h', b'i', b'x']
        );
        assert_eq!(
            pack_cmd(CMD_QUERY_RANGE, 0, 7, &[1, 2, 3], &[1, 2, 3]),
            vec![228, 0, 7, 3, 3, 0, 0, 0, 1, 2, 3, 1, 2, 3]
        );
    }

    #[test]
    fn parse_packet_round_trips_and_rejects_short_data() {
        let data = pack_cmd(CMD_USE, 0, 0, b"books", &[]);
        let (cmd, arg, buf, buf1) = parse_packet(&data).unwrap();
        assert_eq!(cmd, CMD_USE);
        assert_eq!(arg, 0);
        assert_eq!(buf, b"books");
        assert!(buf1.is_empty());
        assert_eq!(parse_packet(&data[..7]), None); // 头不足 8 字节
        let bad = pack_cmd(CMD_OK, 0, 0, b"abc", &[]);
        assert_eq!(parse_packet(&bad[..9]), None); // 长度字段超出实际数据
    }

    #[test]
    fn ini_scheme_parses_vnos_and_flags() {
        let ini = r#"
project.name = demo

[id]
type = id

[title]
type = title
index = both
weight = 5

[content]
type = body

[status]
type = numeric
index = none

[tags]
type = string
tokenizer = none
"#;
        let scheme = FieldScheme::from_ini(ini).unwrap();
        assert_eq!(scheme.id_vno(), 0);
        assert_eq!(scheme.id_name(), "id");
        let title = scheme.field("title").unwrap();
        assert_eq!(title.vno, 1);
        assert!(title.index_self && title.index_mixed && title.with_pos);
        assert_eq!(title.weight, 5);
        let body = scheme.field("content").unwrap();
        assert_eq!(body.vno, MIXED_VNO);
        let status = scheme.field("status").unwrap();
        assert_eq!(status.vno, 2);
        assert!(!status.index_self && !status.index_mixed);
        assert_eq!(scheme.numeric_vnos(), vec![2]);
        let tags = scheme.field("tags").unwrap();
        assert!(tags.custom_tokenizer()); // tokenizer=none 非 full → 只存值
        assert!(!tags.index_self);
        assert_eq!(scheme.name_for_vno(1), Some("title"));
        assert_eq!(scheme.name_for_vno(255), Some("content"));
        assert_eq!(scheme.name_for_vno(9), None);
    }

    #[test]
    fn ini_without_id_field_is_rejected() {
        assert!(FieldScheme::from_ini("[title]\ntype = title\n").is_none());
        assert!(FieldScheme::from_ini("").is_none());
    }

    #[test]
    fn doc_commands_emit_expected_bytes() {
        let mut scheme = FieldScheme::default();
        let doc = SearchDocument::new("one", serde_json::json!({"title": "rust"})).unwrap();
        let cmds = doc_commands(&mut scheme, &doc, true).unwrap();
        assert_eq!(
            cmds,
            vec![
                163, 1, 0, 0, 3, 0, 0, 0, b'o', b'n', b'e', // INDEX_REQUEST(UPDATE, vno=0, "one")
                162, 0x81, 0, 0, 3, 0, 0, 0, b'o', b'n', b'e', // DOC_INDEX(id: weight1|SAVEVALUE)
                162, 1, 255, 0, 4, 0, 0, 0, b'r', b'u', b's', b't', // DOC_INDEX(mixed, vno=255)
                162, 0x81, 1, 0, 4, 0, 0, 0, b'r', b'u', b's', b't', // DOC_INDEX(self+SAVEVALUE, vno=1)
            ]
        );
        assert_eq!(scheme.field("title").map(|f| f.vno), Some(1)); // 动态方案已记录新字段
    }

    #[test]
    fn doc_commands_keep_id_casing_in_the_value_slot() {
        // 「id 被小写化」只发生在查找 term 上（xapian term 一律小写）；值槽
        // （SAVEVALUE，结果 FIELD vno=0 读的就是它）存的是 doc.id 原始字节 ——
        // 客户端在写入/读回两侧都不改写它。这里把两点都钉住。
        let mut scheme = FieldScheme::default();
        let doc = SearchDocument::new("Book-1", serde_json::json!({})).unwrap();
        let cmds = doc_commands(&mut scheme, &doc, true).unwrap();
        assert_eq!(
            cmds,
            vec![
                163, 1, 0, 0, 6, 0, 0, 0, b'b', b'o', b'o', b'k', b'-', b'1', // UPDATE 的查找 term
                162, 0x81, 0, 0, 6, 0, 0, 0, b'B', b'o', b'o', b'k', b'-', b'1', // 值槽：原始大小写
            ]
        );
    }

    #[test]
    fn dynamic_vno_exhaustion_errors_instead_of_aliasing_vno_1() {
        let mut scheme = FieldScheme::default();
        for i in 1..=254 {
            scheme.add_dynamic(&format!("f{i}")).unwrap();
        }
        // 第 255 个字段：报错，绝不回退到已占用的 vno 1（否则读回时值挂错名字）
        let err = scheme.add_dynamic("overflow").unwrap_err();
        assert!(matches!(err, crate::ScoutError::XunSearch(_)), "got {err:?}");
        assert_eq!(scheme.field("f1").map(|f| f.vno), Some(1));
        assert!(!scheme.has_field("overflow"));
        assert_eq!(scheme.name_for_vno(1), Some("f1")); // 没被后来的字段顶掉

        // 走真正的触发路径：一个键数超过 vno 空间的文档
        let fields: serde_json::Map<String, serde_json::Value> =
            (0..300).map(|i| (format!("k{i}"), serde_json::json!("v"))).collect();
        let doc = SearchDocument::new("x", serde_json::Value::Object(fields)).unwrap();
        let err = doc_commands(&mut scheme, &doc, false).unwrap_err();
        assert!(matches!(err, crate::ScoutError::XunSearch(_)), "got {err:?}");
    }

    #[test]
    fn ini_with_more_fields_than_vnos_is_rejected() {
        // 1..=254 只有 254 个槽：第 255 个非 id/body 字段就该让整份 ini 作废
        // （None），而不是让两个字段共用 vno 1
        let mut ini = String::from("[id]\ntype = id\n[body]\ntype = body\n");
        for i in 0..255 {
            ini.push_str(&format!("[f{i}]\ntype = string\n"));
        }
        assert!(FieldScheme::from_ini(&ini).is_none());
        // 边界内（254 个）照样可用
        let mut ok = String::from("[id]\ntype = id\n");
        for i in 0..254 {
            ok.push_str(&format!("[f{i}]\ntype = string\n"));
        }
        let scheme = FieldScheme::from_ini(&ok).unwrap();
        assert_eq!(scheme.field("f253").map(|f| f.vno), Some(254));
    }
