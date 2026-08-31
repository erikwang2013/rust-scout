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

        // SQL 只负责：索引维度 + LIKE 粗筛 + take/skip 分页。
        // wheres / 软删 / 排序在内存做（直接复用 matches() 与 sort_hits()）。
        let mut count_sql =
            String::from("SELECT COUNT(*) FROM scout_documents WHERE index_name = ?");
        let mut fetch_sql =
            String::from("SELECT id, data FROM scout_documents WHERE index_name = ?");
        if like {
            count_sql.push_str(" AND searchable LIKE ?");
            fetch_sql.push_str(" AND searchable LIKE ?");
        }
        fetch_sql.push_str(" LIMIT ? OFFSET ?");

        let pattern = format!("%{q}%");
        let mut count_query = sqlx::query(&count_sql).bind(index);
        let mut fetch_query = sqlx::query(&fetch_sql).bind(index);
        if like {
            count_query = count_query.bind(&pattern);
            fetch_query = fetch_query.bind(&pattern);
        }
        // total 先数满足 SQL 的行数（索引 + LIKE），再做内存过滤；
        // 因此带 wheres/软删过滤时 total 可能大于 hits 数——SQL 层无法下推这些条件。
        let total: i64 = count_query.fetch_one(&pool).await?.try_get(0)?;
        let take = builder.take.unwrap_or(i64::MAX as usize) as i64;
        let skip = builder.skip.unwrap_or(0) as i64;
        let rows = fetch_query
            .bind(take)
            .bind(skip)
            .fetch_all(&pool)
            .await?;

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
        Ok(SearchResult {
            hits,
            total: total as usize,
            ..SearchResult::default()
        })
    }
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
            base.skip = Some((page - 1) * per_page);
            base.take = Some(per_page);
            self.search_impl(&base).await
        })
    }

    fn map_ids(&self, result: &SearchResult) -> Vec<String> {
        result.ids()
    }

    fn flush<'a>(&'a self, _index: &'a str) -> EngineFuture<'a, ()> {
        // no-op：无独立索引存储，写入即对查询可见（与 PHP Scout 的 database
        // 驱动语义一致；ES 的 flush/refresh 概念在此不适用）。
        Box::pin(async move { Ok(()) })
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
    async fn wheres_filtered_in_memory_total_is_sql_level() {
        let e = engine().await;
        e.update(&[
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
        ])
        .await
        .unwrap();
        let result = e
            .search(
                &SearchBuilder::new("rust")
                    .within("books")
                    .where_field("category", "tech"),
            )
            .await
            .unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].id, "one");
        // total 只含 SQL 层条件（索引 + LIKE），wheres 是内存过滤。
        assert_eq!(result.total, 2);
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
