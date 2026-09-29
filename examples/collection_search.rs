//! 最小可运行的搜索示例：零 feature、零外部服务。
//!
//! ```console
//! $ cargo run --example collection_search
//! ```
//!
//! 这个文件只依赖默认的内存驱动，所以 `cargo add rust-scout` 之后就能直接跑。
//! 换成真实后端只需改 Cargo feature 与 `ScoutConfig` —— 下面的调用一行都不用动，
//! 这正是 rust-scout 存在的理由。
//!
//! 用的是 `current_thread` 运行时：示例没有并发需求，而 tokio 的
//! `rt-multi-thread` 不在本 crate 的 dev-dependencies 里。

use rust_scout::{CollectionEngine, Engine, SearchBuilder, SearchDocument};

#[tokio::main(flavor = "current_thread")]
async fn main() -> rust_scout::Result<()> {
    let engine = CollectionEngine::new();

    // 1. 写入。`index` 决定文档归属哪个索引；不设则是 "default"。
    let mut docs = Vec::new();
    for (id, title, status, year) in [
        ("a1", "Rust 异步编程", "published", 2024),
        ("a2", "Rust 所有权入门", "published", 2023),
        ("a3", "Go 并发模型", "draft", 2024),
        ("a4", "Rust 错误处理", "published", 2025),
    ] {
        let mut doc = SearchDocument::new(
            id,
            serde_json::json!({ "title": title, "status": status, "year": year }),
        )?;
        doc.index = Some("articles".to_string());
        docs.push(doc);
    }
    // 批量写入：一次提交，各驱动会映射到后端的原生批量端点。
    engine.update_bulk(&docs).await?;

    // 2. 查询。`total` 是**过滤后**的命中总数，与 `take` 无关。
    let found = engine
        .search(
            &SearchBuilder::new("rust")
                .within("articles")
                .where_field("status", "published")
                .order_by("year", true),
        )
        .await?;
    println!("命中 {} 条：", found.total);
    for hit in &found.hits {
        println!("  {}  {}", hit.id, hit.source["title"]);
    }

    // 3. 分页：第 2 页、每页 2 条。等价于 skip((page-1)*per_page).take(per_page)。
    let page2 = engine
        .paginate(
            &SearchBuilder::new("").within("articles").order_by("year", true),
            2,
            2,
        )
        .await?;
    let ids: Vec<&str> = page2.hits.iter().map(|h| h.id.as_str()).collect();
    println!("\n第 2 页（每页 2 条）：{ids:?}");

    // 4. 软删除：只打 `__soft_deleted` 标记，默认查询看不见，with_trashed 才看得见。
    engine
        .soft_delete_in("articles", &["a2".to_string()])
        .await?;
    let visible = engine
        .search(&SearchBuilder::new("").within("articles"))
        .await?;
    let all = engine
        .search(&SearchBuilder::new("").within("articles").with_trashed())
        .await?;
    println!(
        "\n软删除 a2 后：默认可见 {} 条，with_trashed {} 条",
        visible.total, all.total
    );

    // 5. 清空索引。
    engine.delete_index("articles").await?;
    let gone = engine
        .search(&SearchBuilder::new("").within("articles"))
        .await?;
    println!("\ndelete_index 后：{} 条", gone.total);

    Ok(())
}
