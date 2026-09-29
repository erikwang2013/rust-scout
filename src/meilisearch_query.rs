#![cfg(feature = "meilisearch")]

//! Meilisearch 的查询/响应层：过滤表达式、排序、分页模式与响应解析。
//! 全部是无 I/O 的纯函数，与 [`MeilisearchEngine`] 的请求逻辑分开（同
//! `typesense_query.rs` 的分层）。

use serde_json::{Map, Value};

use crate::meilisearch_engine::{MeilisearchEngine, PageMode};
use crate::{SearchBuilder, SearchDocument, SearchHit, SearchResult, TrashedFilter};

impl MeilisearchEngine {
    /// filter 值：字符串加双引号（转义 `\` 与 `"`），数字/bool 裸值。
    pub(crate) fn filter_value(v: &Value) -> String {
        match v {
            Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            other => other.to_string(),
        }
    }

    /// 等值/IN/软删除 → Meilisearch filter 表达式；无任何条件时返回 None。
    ///
    /// 表达式是「字段名 + 运算符 + 值」拼出来的文本，值那边转义了，字段名这边
    /// 之前是裸拼：一个带空格或运算符的字段名足以把整条 filter 改写成别的查询
    /// （软删除守卫会被 `OR` 掉）。所以每个插值点都先过校验。
    pub(crate) fn build_filter(builder: &SearchBuilder) -> crate::Result<Option<String>> {
        let mut parts: Vec<String> = Vec::new();
        for w in &builder.wheres {
            crate::validate_field_name(&w.field)?;
            parts.push(format!("{}={}", w.field, Self::filter_value(&w.value)));
        }
        for (field, values) in &builder.where_ins {
            crate::validate_field_name(field)?;
            let list = values.iter().map(Self::filter_value).collect::<Vec<_>>().join(", ");
            parts.push(format!("{} IN [{}]", field, list));
        }
        for (field, values) in &builder.where_not_ins {
            if values.is_empty() {
                continue; // 空 NOT IN 集合 = 无过滤（Collection 语义，与 Algolia 一致）
            }
            // 校验排在空集合之后：不产生表达式就不该报错，与 search 对空
            // where_in 直接短路成空结果（也不校验）的行为一致。
            crate::validate_field_name(field)?;
            let list = values.iter().map(Self::filter_value).collect::<Vec<_>>().join(", ");
            parts.push(format!("{} NOT IN [{}]", field, list));
        }
        // `=`/`IS NULL` 只匹配「存在且相等」/「显式 null」，未软删文档从未写入
        // 该字段，会被全部隐藏；`NOT __soft_deleted = true` 对缺失/false/null
        // 均匹配，与 Collection 的 as_bool().unwrap_or(false) 语义对齐。
        match builder.trashed {
            TrashedFilter::Exclude => parts.push("NOT __soft_deleted = true".to_string()),
            TrashedFilter::OnlyTrashed => parts.push("__soft_deleted=true".to_string()),
            TrashedFilter::WithTrashed => {}
        }
        Ok(if parts.is_empty() {
            None
        } else {
            Some(parts.join(" AND "))
        })
    }

    /// `[{field}:{asc|desc}, ..]`；字段名同上，插值前校验。
    pub(crate) fn sort_array(builder: &SearchBuilder) -> crate::Result<Value> {
        let mut parts: Vec<String> = Vec::with_capacity(builder.orders.len());
        for o in &builder.orders {
            crate::validate_field_name(&o.field)?;
            parts.push(format!("{}:{}", o.field, if o.desc { "desc" } else { "asc" }));
        }
        Ok(Value::Array(parts.into_iter().map(Value::String).collect()))
    }

    pub(crate) fn page_mode(builder: &SearchBuilder) -> PageMode {
        let take = builder.take.unwrap_or(10).max(1);
        let skip = builder.skip.unwrap_or(0);
        if skip.is_multiple_of(take) {
            PageMode::Page(skip / take + 1, take)
        } else {
            PageMode::Offset(skip, take)
        }
    }

    /// `page`/`per_page` → 页模式；page N 与 `(N-1)*per_page` 的偏移等价。
    pub(crate) fn page_mode_of(page: usize, per_page: usize) -> PageMode {
        PageMode::Page(page.max(1), per_page.max(1))
    }

    /// 请求体。按 [`PageMode`] 二选一，理由见那里的注释。
    pub(crate) fn search_body(builder: &SearchBuilder, mode: &PageMode) -> crate::Result<Value> {
        let mut body = Map::new();
        body.insert("q".into(), Value::String(builder.query.clone()));
        if let Some(filter) = Self::build_filter(builder)? {
            body.insert("filter".into(), Value::String(filter));
        }
        match *mode {
            PageMode::Page(page, per_page) => {
                body.insert("page".into(), Value::from(page));
                body.insert("hitsPerPage".into(), Value::from(per_page));
            }
            PageMode::Offset(offset, limit) => {
                body.insert("offset".into(), Value::from(offset));
                body.insert("limit".into(), Value::from(limit));
            }
        }
        if !builder.orders.is_empty() {
            body.insert("sort".into(), Self::sort_array(builder)?);
        }
        Ok(Value::Object(body))
    }

    /// 文档 → Meilisearch 文档对象；`id` 键统一注入/覆盖为 doc.id。
    pub(crate) fn doc_to_document(doc: &SearchDocument) -> Value {
        let mut fields = doc.fields.clone();
        fields.insert("id".into(), Value::String(doc.id.clone()));
        Value::Object(fields)
    }

    /// 响应解析：id 取 `hits[i].id`，source 取整个 hit 对象，total 取 `totalHits`。
    ///
    /// 用 `offset`/`limit` 搜索时 Meilisearch 只回 `estimatedTotalHits`（穷尽计数
    /// `totalHits` 需要 `page`/`hitsPerPage`，见 paginate 分支），故回退读它；
    /// 两者都缺失才回退 hits.len()，避免 total 退化成页大小。
    pub(crate) fn parse_search_response(raw: &Value) -> SearchResult {
        let hits = raw.get("hits").and_then(Value::as_array);
        let total = ["totalHits", "estimatedTotalHits"]
            .iter()
            .find_map(|key| raw.get(*key).and_then(Value::as_u64))
            .map(|n| n as usize)
            .unwrap_or_else(|| hits.map_or(0, Vec::len));
        let hits = hits
            .map(|arr| arr.iter().filter_map(Self::hit_from_response).collect())
            .unwrap_or_default();
        SearchResult {
            hits,
            total,
            ..Default::default()
        }
    }

    pub(crate) fn hit_from_response(hit: &Value) -> Option<SearchHit> {
        let id = hit.get("id")?;
        let id = id.as_str().map(str::to_string).unwrap_or_else(|| id.to_string());
        Some(SearchHit {
            id,
            // `_rankingScore` 需在索引设置中启用；缺失则无分。`_matchesPosition`
            // 是位置信息而非分数，忽略。
            score: hit.get("_rankingScore").and_then(Value::as_f64),
            source: hit.clone(),
            highlight: None,
        })
    }
}
