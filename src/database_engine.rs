//! SQLite 存储引擎：把文档存进单表，SQL LIKE 做粗筛，wheres/软删/排序在内存完成。
//!
//! 表设计：单表 `scout_documents`，`id` 全局唯一（同一 id 在多个索引写入时按
//! upsert 覆盖，与 [`crate::CollectionEngine`] 的按索引存副本不同——见 delete 注释）。
//! `searchable` 列 = 整份字段 JSON 的小写，供 LIKE 粗筛（必须是内存
//! `matches()` 的超集，否则会静默丢结果）；`data` 列 = 同样的 JSON 原样，
//! 内存过滤时还原。

use std::sync::atomic::{AtomicBool, Ordering};

use sqlx::Row;

use crate::engine::{Engine, EngineFuture};
use crate::{Result, SearchBuilder, SearchDocument, SearchHit, SearchResult};

pub struct DatabaseEngine {
    pool: sqlx::SqlitePool,
    /// **不再影响搜索范围**，仅保留以维持 [`Self::new`] 的公开签名。
    ///
    /// 早期用它拼 `searchable` 列，于是文本查询只覆盖这些字段，而基准驱动
    /// （[`crate::CollectionEngine`]）搜整份文档 —— 同一输入两个驱动给出不同结果。
    /// 现在粗筛对齐基准，这个字段成了摆设，待下个 breaking 版本连同构造参数一起删。
    #[allow(dead_code)]
    searchable_fields: Vec<String>,
    /// 表结构是否已建（见 [`Self::ensure_schema`]）。
    schema_ready: AtomicBool,
}

impl DatabaseEngine {
    /// 同步构造（`connect_lazy`：连接池延迟建连），保持 [`crate::EngineManager::engine`]
    /// 的同步签名。表结构在**首次操作**时建好（`CREATE ... IF NOT EXISTS`，幂等）。
    ///
    /// `searchable_fields` 已不参与搜索（见字段注释），保留只为不破坏签名。
    pub fn new(database_url: &str, searchable_fields: Vec<String>) -> Result<Self> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new().connect_lazy(database_url)?;
        Ok(Self {
            pool,
            searchable_fields,
            schema_ready: AtomicBool::new(false),
        })
    }

    #[cfg(test)]
    fn from_pool(pool: sqlx::SqlitePool, searchable_fields: Vec<String>) -> Self {
        Self {
            pool,
            searchable_fields,
            schema_ready: AtomicBool::new(false),
        }
    }

    /// 建表建索引（幂等）。真正的语句只在**首次**操作时发一次：两条
    /// `CREATE ... IF NOT EXISTS` 实测 145µs/次，200 行搜索的 8% 都花在这上面，
    /// 10k 次小操作就是 1.45s 的纯开销。`AtomicBool` 而不是 `bool`：Engine 要求
    /// `Send + Sync` 且方法是 `&self`。
    ///
    /// 取舍：外部绕过驱动把表删掉后不再自愈（旧行为每次都补一遍）。进程内驱动
    /// 独占这张表，为「别人在背后删表」付每次操作的钱是错的。
    async fn ensure_schema(&self) -> Result<()> {
        if self.schema_ready.load(Ordering::Acquire) {
            return Ok(());
        }
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS scout_documents (\
             id TEXT PRIMARY KEY, index_name TEXT NOT NULL, \
             searchable TEXT NOT NULL, data TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_scout_index ON scout_documents (index_name)")
            .execute(&self.pool)
            .await?;
        // 两条都成功才置位：中途失败下次仍会重试。
        self.schema_ready.store(true, Ordering::Release);
        Ok(())
    }

    async fn write_all(&self, docs: &[SearchDocument]) -> Result<()> {
        // 写入的索引名来自 doc.index（None ≡ "default"）：与五个网络驱动同契约，
        // 且校验排在 ensure_schema 之前 —— 库不可用时也该是 InvalidIndexName。
        // 先整批校验再开事务，避免「写进去几条才报错」的半截状态。
        for doc in docs {
            crate::validate_index_name(doc.index.as_deref().unwrap_or("default"))?;
        }
        let pool = self.pool.clone();
        self.ensure_schema().await?;
        let mut tx = pool.begin().await?;
        for doc in docs {
            let index = doc.index.clone().unwrap_or_else(|| "default".to_string());
            // searchable 存**整份字段 JSON 的小写**，而不是配置字段白名单的值。
            //
            // 粗筛（SQL LIKE）只需是内存 matches() 的**超集**，但它必须是超集：
            // 漏掉 matches() 本会保留的行就是静默的错误答案。而 matches() 搜的是
            // 整份序列化 JSON，所以粗筛也必须是整份 JSON。曾经只拼白名单字段的值，
            // 于是 `fields=["title"]` + 文档 {"title":"Rust","tag":"async"} + 查
            // "async" 会得到 database 0 条、collection 1 条 —— 八个驱动对同一输入
            // 给出不同答案。`searchable_fields` 因此不再影响搜索范围。
            let data = serde_json::to_string(&doc.fields)?;
            let searchable = data.to_lowercase();
            sqlx::query(
                "INSERT INTO scout_documents (id, index_name, searchable, data) \
                 VALUES (?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET \
                 index_name = excluded.index_name, searchable = excluded.searchable, \
                 data = excluded.data",
            )
            .bind(&doc.id)
            .bind(&index)
            .bind(&searchable)
            .bind(&data)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn delete_by_ids(&self, ids: &[String]) -> Result<()> {
        let pool = self.pool.clone();
        self.ensure_schema().await?;
        // 整批一个事务（与 write_all 同款）：逐条 `execute(&pool)` 时每条 DELETE 都是
        // 独立隐式事务，SQLite 默认 rollback journal + synchronous=FULL 每条都要
        // fsync，1000 个 id 就是 1000 次同步。语义不变：不存在的 id 静默跳过。
        let mut tx = pool.begin().await?;
        for id in ids {
            sqlx::query("DELETE FROM scout_documents WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn delete_in_impl(&self, index: &str, ids: &[String]) -> Result<()> {
        // 校验先于任何数据库操作（与其余驱动、与 soft_delete_in 同序）：进程内驱动
        // 没有被 ES 展开成多索引的风险，但契约要统一——否则 delete_in("_all")
        // 在这里 Ok、在 ES 上 Err。
        crate::validate_index_name(index)?;
        let pool = self.pool.clone();
        self.ensure_schema().await?;
        let mut tx = pool.begin().await?;
        for id in ids {
            sqlx::query("DELETE FROM scout_documents WHERE index_name = ? AND id = ?")
                .bind(index)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn soft_delete_impl(&self, ids: &[String]) -> Result<()> {
        let pool = self.pool.clone();
        self.ensure_schema().await?;
        // 读-改-写整体进同一事务：除了省掉每 id 一次的 fsync，UPDATE 拿到的写锁
        // 持有到 commit，中间不会有别的连接插进来改同一份 data。
        let mut tx = pool.begin().await?;
        for id in ids {
            let row = sqlx::query("SELECT data FROM scout_documents WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
            let Some(row) = row else { continue };
            let data: String = row.try_get("data")?;
            let mut fields: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&data)?;
            fields.insert("__soft_deleted".to_string(), serde_json::Value::Bool(true));
            sqlx::query("UPDATE scout_documents SET data = ? WHERE id = ?")
                .bind(serde_json::to_string(&fields)?)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn reindex_impl(&self, from: &str, to: &str) -> Result<()> {
        // 两端都校验，且先于建表/取连接（与 ES/Algolia 同序）
        crate::validate_index_name(from)?;
        crate::validate_index_name(to)?;
        let pool = self.pool.clone();
        self.ensure_schema().await?;
        // 这是**移动**不是复制：from 的文档改归属到 to，from 随即为空。
        //
        // 为什么不做真复制：id 是全局主键（见模块头），复制到 to 时同 id 行会撞
        // 主键，除非把 id 改成 (index_name, id) 复合主键——那是 schema 变更，本驱动
        // 不做。所以保留移动语义并在此显式标注；trait 文档与 README 的驱动表按
        // 移动描述（CollectionEngine 才是复制：to 的既有内容被整体替换）。
        //
        // 一行 SQL 即完成。原注释说「to 中已有同 id 的行会撞主键、整批中止」——
        // id 是全局主键，同一 id 全表只有一行，to 里不可能另有一行与 from 的文档
        // 同 id，这个冲突在本 schema 下构造不出来；旧说法已作废（见
        // reindex_moves_documents_and_empties_source 的移动语义测试）。
        sqlx::query("UPDATE scout_documents SET index_name = ? WHERE index_name = ?")
            .bind(to)
            .bind(from)
            .execute(&pool)
            .await?;
        Ok(())
    }

    async fn search_impl(&self, builder: &SearchBuilder) -> Result<SearchResult> {
        let index = builder.index.as_deref().unwrap_or("default");
        // 校验必须排在建表/取连接之前（与 ES `search_hits` 同序）：库不可用时
        // 保留名要报 InvalidIndexName，而不是 I/O 错误。
        crate::validate_index_name(index)?;
        let pool = self.pool.clone();
        self.ensure_schema().await?;
        let q = builder.query.trim().to_lowercase();
        // `searchable` 列现在存整份字段 JSON 的小写（见 `write_all`），所以只要
        // 查询非空，粗筛就一定能覆盖 `matches()` 的结果 —— 不再需要「没配字段就
        // 跳过粗筛」那种兜底：那时候 `database.fields` 缺省会让整列恒为 ""，
        // `searchable LIKE '%q%'` 一条都匹配不上，**每个文本查询都静默返回 0 条**。
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

        // 先排引用、取完窗口再物化：命中上万条时不必为落选的行深拷贝 fields。
        let mut matched: Vec<&SearchDocument> = docs
            .iter()
            .filter(|doc| trashed_allows(builder, doc))
            .filter(|doc| builder.matches(doc))
            .collect();
        let cmp = |a: &&SearchDocument, b: &&SearchDocument| {
            builder.sort_cmp((Some(&a.fields), a.id.as_str()), (Some(&b.fields), b.id.as_str()))
        };

        // 与 CollectionEngine::selected 一致：total 是**过滤后**的命中总数，
        // 分页在最后一步切。
        let total = matched.len();
        let offset = builder.skip.unwrap_or(0);
        let take = builder.take.unwrap_or(total);
        // 同 CollectionEngine：先 top-K 分区再排序，window 外的元素直接 truncate。
        // sort_cmp 是按 id 兜底的全序，结果与「全排序再取窗口」逐条相同。
        let k = offset.saturating_add(take).min(total);
        if k < total {
            matched.select_nth_unstable_by(k, cmp);
            matched.truncate(k);
        }
        matched.sort_by(cmp);
        let hits = matched
            .into_iter()
            .skip(offset)
            .take(take)
            .map(SearchHit::from)
            .collect();

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
        index: &'a str,
        _settings: serde_json::Value,
    ) -> EngineFuture<'a, ()> {
        // no-op：单表结构，索引维度只是 index_name 列。但名字还是要校验 ——
        // 其余驱动（含 ES）都拒保留名，这里放行会让 `_all` 的行为随驱动漂移。
        Box::pin(async move {
            crate::validate_index_name(index)?;
            Ok(())
        })
    }

    fn delete_index<'a>(&'a self, index: &'a str) -> EngineFuture<'a, ()> {
        Box::pin(async move {
            crate::validate_index_name(index)?;
            let pool = self.pool.clone();
            self.ensure_schema().await?;
            // 真删该索引的行。此前这里是 no-op：而 README（及 12 份译文）的索引
            // 生命周期把 delete_index 当作「清空索引」的入口——update → delete_index
            // → search 仍返回全部旧文档，文档与行为相反。只按 index_name 删，
            // 其它索引的行（哪怕 id 相同）不在 WHERE 范围内。
            sqlx::query("DELETE FROM scout_documents WHERE index_name = ?")
                .bind(index)
                .execute(&pool)
                .await?;
            Ok(())
        })
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
            self.ensure_schema().await?;
            // 一个事务包住 SELECT+UPDATE 循环（理由同 soft_delete_impl）。
            let mut tx = pool.begin().await?;
            for id in ids {
                let row = sqlx::query(
                    "SELECT data FROM scout_documents WHERE index_name = ? AND id = ?",
                )
                .bind(index)
                .bind(id)
                .fetch_optional(&mut *tx)
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
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
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
#[path = "database_engine_tests.rs"]
mod tests;
