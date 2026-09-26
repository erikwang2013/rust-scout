//! SQLite 存储引擎：把文档存进单表，SQL LIKE 做粗筛，wheres/软删/排序在内存完成。
//!
//! 表设计：单表 `scout_documents`，`id` 全局唯一（同一 id 在多个索引写入时按
//! upsert 覆盖，与 [`crate::CollectionEngine`] 的按索引存副本不同——见 delete 注释）。
//! `searchable` 列 = 所有 searchable_fields 的值小写空格连接，供 LIKE 粗筛；
//! `data` 列 = 完整字段 JSON，内存过滤时还原。

use sqlx::Row;

use crate::engine::{Engine, EngineFuture};
use crate::{Result, SearchBuilder, SearchDocument, SearchHit, SearchResult};

pub struct DatabaseEngine {
    pool: sqlx::SqlitePool,
    searchable_fields: Vec<String>,
}

impl DatabaseEngine {
    /// 同步构造（`connect_lazy`：连接池延迟建连），保持 [`crate::EngineManager::engine`]
    /// 的同步签名。表结构在每次操作前用 `CREATE ... IF NOT EXISTS` 确保（幂等）。
    pub fn new(database_url: &str, searchable_fields: Vec<String>) -> Result<Self> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new().connect_lazy(database_url)?;
        Ok(Self {
            pool,
            searchable_fields,
        })
    }

    #[cfg(test)]
    fn from_pool(pool: sqlx::SqlitePool, searchable_fields: Vec<String>) -> Self {
        Self {
            pool,
            searchable_fields,
        }
    }

    async fn ensure_schema(pool: &sqlx::SqlitePool) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS scout_documents (\
             id TEXT PRIMARY KEY, index_name TEXT NOT NULL, \
             searchable TEXT NOT NULL, data TEXT NOT NULL)",
        )
        .execute(pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_scout_index ON scout_documents (index_name)")
            .execute(pool)
            .await?;
        Ok(())
    }

    async fn write_all(&self, docs: &[SearchDocument]) -> Result<()> {
        let pool = self.pool.clone();
        Self::ensure_schema(&pool).await?;
        let mut tx = pool.begin().await?;
        for doc in docs {
            let index = doc.index.clone().unwrap_or_else(|| "default".to_string());
            let searchable = self
                .searchable_fields
                .iter()
                .filter_map(|f| doc.fields.get(f))
                .map(|v| match v {
                    serde_json::Value::String(s) => s.to_lowercase(),
                    other => other.to_string().to_lowercase(),
                })
                .collect::<Vec<_>>()
                .join(" ");
            sqlx::query(
                "INSERT INTO scout_documents (id, index_name, searchable, data) \
                 VALUES (?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET \
                 index_name = excluded.index_name, searchable = excluded.searchable, \
                 data = excluded.data",
            )
            .bind(&doc.id)
            .bind(&index)
            .bind(&searchable)
            .bind(serde_json::to_string(&doc.fields)?)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn delete_by_ids(&self, ids: &[String]) -> Result<()> {
        let pool = self.pool.clone();
        Self::ensure_schema(&pool).await?;
        for id in ids {
            sqlx::query("DELETE FROM scout_documents WHERE id = ?")
                .bind(id)
                .execute(&pool)
                .await?;
        }
        Ok(())
    }

    async fn delete_in_impl(&self, index: &str, ids: &[String]) -> Result<()> {
        let pool = self.pool.clone();
        Self::ensure_schema(&pool).await?;
        for id in ids {
            sqlx::query("DELETE FROM scout_documents WHERE index_name = ? AND id = ?")
                .bind(index)
                .bind(id)
                .execute(&pool)
                .await?;
        }
        Ok(())
    }

    async fn soft_delete_impl(&self, ids: &[String]) -> Result<()> {
        let pool = self.pool.clone();
        Self::ensure_schema(&pool).await?;
        for id in ids {
            let row = sqlx::query("SELECT data FROM scout_documents WHERE id = ?")
                .bind(id)
                .fetch_optional(&pool)
                .await?;
            let Some(row) = row else { continue };
            let data: String = row.try_get("data")?;
            let mut fields: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&data)?;
            fields.insert("__soft_deleted".to_string(), serde_json::Value::Bool(true));
            sqlx::query("UPDATE scout_documents SET data = ? WHERE id = ?")
                .bind(serde_json::to_string(&fields)?)
                .bind(id)
                .execute(&pool)
                .await?;
        }
        Ok(())
    }

    async fn reindex_impl(&self, from: &str, to: &str) -> Result<()> {
        let pool = self.pool.clone();
        Self::ensure_schema(&pool).await?;
        // 一行 SQL 移动索引归属；to 中已存在相同 id 的行会触发主键冲突
        // （id 全局唯一，重索引重叠时整批中止）。
        sqlx::query("UPDATE scout_documents SET index_name = ? WHERE index_name = ?")
            .bind(to)
            .bind(from)
            .execute(&pool)
            .await?;
        Ok(())
    }

    async fn search_impl(&self, builder: &SearchBuilder) -> Result<SearchResult> {
        let pool = self.pool.clone();
        Self::ensure_schema(&pool).await?;
        let index = builder.index.as_deref().unwrap_or("default");
        let q = builder.query.trim().to_lowercase();
        let like = !q.is_empty();

        // SQL 只负责：索引维度 + LIKE 粗筛。
        //
        // 分页**不能**下推到 SQL 的 LIMIT/OFFSET：wheres / 软删是内存过滤，SQL 先
        // 截断会让窗口外的匹配行永远取不回来——3 条文档、where_field 只命中第 3 条、
        // take(2) 时 SQL 取回前 2 条再被内存滤掉，结果是 0 条，而 CollectionEngine
        // 返回 1 条。改成与 CollectionEngine::selected 同序：先粗筛取回 → 内存过滤
        // → 排序 → 取 total → 最后才切 skip/take。
        let mut fetch_sql =
            String::from("SELECT id, data FROM scout_documents WHERE index_name = ?");
        if like {
            fetch_sql.push_str(" AND searchable LIKE ? ESCAPE '\\'");
        }

        // LIKE 的通配符必须转义成字面量：不转义时查 "50%" 会变成 "50 后跟任意"，
        // 粗筛放过额外行，而这些行不计入 hits（内存 matches() 会滤掉）。转义后
        // 与 CollectionEngine 的 contains() 字面匹配语义才对得上。
        let pattern = format!("%{}%", escape_like(&q));
        let mut fetch_query = sqlx::query(&fetch_sql).bind(index);
        if like {
            fetch_query = fetch_query.bind(&pattern);
        }
        let rows = fetch_query.fetch_all(&pool).await?;

        let docs: Vec<SearchDocument> = rows
            .into_iter()
            .map(|row| {
                Ok(SearchDocument {
                    id: row.try_get::<String, _>("id")?,
                    index: None,
                    fields: serde_json::from_str(&row.try_get::<String, _>("data")?)?,
                })
            })
            .collect::<Result<_>>()?;

        let mut hits: Vec<SearchHit> = docs
            .iter()
            .filter(|doc| trashed_allows(builder, doc))
            .filter(|doc| builder.matches(doc))
            .map(SearchHit::from)
            .collect();
        builder.sort_hits(&mut hits);

        // 与 CollectionEngine::selected 一致：total 是**过滤后**的命中总数，
        // 分页在最后一步切。
        let total = hits.len();
        let offset = builder.skip.unwrap_or(0);
        let take = builder.take.unwrap_or(total);
        let hits = hits.into_iter().skip(offset).take(take).collect();

        Ok(SearchResult {
            hits,
            total,
            ..SearchResult::default()
        })
    }
}

/// 把 LIKE 元字符转义成字面量（配合 `ESCAPE '\'`）。反斜杠必须先处理，
/// 否则会把后面刚插入的转义符再转一次。
fn escape_like(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    for c in q.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn trashed_allows(builder: &SearchBuilder, doc: &SearchDocument) -> bool {
    let soft = doc
        .get("__soft_deleted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    match builder.trashed {
        crate::TrashedFilter::Exclude => !soft,
        crate::TrashedFilter::OnlyTrashed => soft,
        crate::TrashedFilter::WithTrashed => true,
    }
}

impl Engine for DatabaseEngine {
    fn update<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(self.write_all(docs))
    }

    fn delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        // 按 id 删除，忽略索引维度：文档 id 全局唯一（表主键），
        // 与 CollectionEngine 的跨索引删除语义一致。
        Box::pin(self.delete_by_ids(ids))
    }

    fn delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(self.delete_in_impl(index, ids))
    }

    fn search<'a>(&'a self, builder: &'a SearchBuilder) -> EngineFuture<'a, SearchResult> {
        Box::pin(self.search_impl(builder))
    }

    fn paginate<'a>(
        &'a self,
        builder: &'a SearchBuilder,
        page: usize,
        per_page: usize,
    ) -> EngineFuture<'a, SearchResult> {
        let page = page.max(1);
        let per_page = per_page.max(1);
        Box::pin(async move {
            let mut base = builder.clone();
            base.skip = Some((page - 1).saturating_mul(per_page));
            base.take = Some(per_page);
            self.search_impl(&base).await
        })
    }

    fn create_index<'a>(
        &'a self,
        _index: &'a str,
        _settings: serde_json::Value,
    ) -> EngineFuture<'a, ()> {
        // no-op：单表结构，索引维度只是 index_name 列。
        Box::pin(async move { Ok(()) })
    }

    fn delete_index<'a>(&'a self, _index: &'a str) -> EngineFuture<'a, ()> {
        // no-op：无独立索引存储，索引维度只是 index_name 列。
        Box::pin(async move { Ok(()) })
    }

    fn update_bulk<'a>(&'a self, docs: &'a [SearchDocument]) -> EngineFuture<'a, ()> {
        Box::pin(self.write_all(docs))
    }

    fn delete_bulk<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        // 与 delete_in 一致：index + id 双条件（trait 契约按 index 删除，
        // 与 CollectionEngine 对齐，避免误删其它索引的同 id 文档）。
        Box::pin(self.delete_in_impl(index, ids))
    }

    fn soft_delete_in<'a>(&'a self, index: &'a str, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let pool = self.pool.clone();
            Self::ensure_schema(&pool).await?;
            for id in ids {
                let row = sqlx::query(
                    "SELECT data FROM scout_documents WHERE index_name = ? AND id = ?",
                )
                .bind(index)
                .bind(id)
                .fetch_optional(&pool)
                .await?;
                let Some(row) = row else { continue };
                let data: String = row.try_get("data")?;
                let mut fields: serde_json::Map<String, serde_json::Value> =
                    serde_json::from_str(&data)?;
                fields.insert("__soft_deleted".to_string(), serde_json::Value::Bool(true));
                sqlx::query("UPDATE scout_documents SET data = ? WHERE index_name = ? AND id = ?")
                    .bind(serde_json::to_string(&fields)?)
                    .bind(index)
                    .bind(id)
                    .execute(&pool)
                    .await?;
            }
            Ok(())
        })
    }

    fn soft_delete<'a>(&'a self, ids: &'a [String]) -> EngineFuture<'a, ()> {
        Box::pin(self.soft_delete_impl(ids))
    }

    fn reindex<'a>(&'a self, from: &'a str, to: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(self.reindex_impl(from, to))
    }
}

#[cfg(all(test, feature = "database"))]
mod tests {
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
    async fn reindex_moves_documents() {
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
        assert_eq!(books.total, 0);
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
}
