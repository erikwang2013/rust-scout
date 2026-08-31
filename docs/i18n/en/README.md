# rust-scout

[![crates.io](https://img.shields.io/crates/v/rust-scout.svg)](https://crates.io/crates/rust-scout)
[![docs.rs](https://img.shields.io/docsrs/rust-scout)](https://docs.rs/rust-scout)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../../LICENSE)

[简体中文](../../../README.md) · English · [日本語](../ja/README.md) · [한국어](../ko/README.md) · [Bahasa Indonesia](../id/README.md) · [Русский](../ru/README.md) · [Deutsch](../de/README.md) · [Français](../fr/README.md) · [Español](../es/README.md) · [Português](../pt/README.md) · [हिन्दी](../hi/README.md) · [العربية](../ar/README.md) · [বাংলা](../bn/README.md)

**rust-scout full-text search library abstraction** — a lightweight full-text search interface layer for Rust. Borrowing the chained-query mental model of [Laravel Scout](https://laravel.com/docs/scout), it abstracts in-memory, Elasticsearch/OpenSearch, Meilisearch, Typesense, Algolia, SQLite and other backends through a unified `Engine` trait: **zero-dependency in-memory driver for development, seamless switch to any backend in production, without changing a single line of business code.**

```rust
let result = engine.search(
    SearchBuilder::new("rust 异步")
        .within("articles")
        .where_field("status", "published")
        .order_by("created_at", true)
        .take(10),
).await?;
```

## Features

| Capability | Description |
|------|------|
| 🔍 Full-text search | In-memory driver substring matching; ES driver `query_string` syntax (`field:value`) |
| ⚙️ Chained queries | `SearchBuilder`: query / within / where_field / where_in / where_not_in / order_by / take / skip |
| 🎯 Exact filtering | Equality matching (ES → `term`), set matching (ES → `terms` / `must_not`) |
| 📄 Multi-field sorting | Stackable asc / desc |
| 📃 Pagination | `take`/`skip` offset truncation + `paginate(page, per_page)` page-based pagination |
| 🗂️ Multiple indexes | Document-level `index` field routing, default index `"default"` |
| 🔄 Index lifecycle | `create_index` / `flush` / `delete_index` full workflow |
| 🔌 Pluggable drivers | Default in-memory zero-dependency; `elasticsearch` / `meilisearch` / `typesense` / `algolia` / `database` / `null` features opt-in; `xunsearch` is a placeholder stub |
| 🔒 Safety boundary | Index name validation (`validate_index_name`) + RFC 3986 percent-encoding to prevent path injection |

## Architecture

![Architecture](svg/architecture.svg)

## Feature Overview

![Features](svg/features.svg)

## Design Philosophy

![Design](svg/design.svg)

## Lifecycle

![Lifecycle](svg/lifecycle.svg)

## Project Structure

```
rust-scout/
├── Cargo.toml            # 依赖与 feature 声明（elasticsearch 可选）
├── src/
│   ├── lib.rs            # crate 根：模块导出 + 公开类型再导出
│   ├── engine.rs         # Engine trait：驱动统一接口（8 个操作）
│   ├── manager.rs        # EngineManager：门面，按配置分发驱动
│   ├── config.rs         # ScoutConfig + validate_index_name
│   ├── builder.rs        # SearchBuilder：链式查询构建与匹配/排序逻辑
│   ├── document.rs       # SearchDocument：写入文档（serde JSON 契约）
│   ├── result.rs         # SearchResult / SearchHit：查询结果
│   ├── searchable.rs     # Searchable / SearchableStore：业务模型桥接
│   ├── error.rs          # ScoutError + Result<T>
│   ├── collection_engine.rs  # 内存驱动（默认）
│   └── elasticsearch_engine.rs # ES/OpenSearch 驱动（feature 可选）
├── tests/                # 集成测试（当前为空）
├── examples/             # 示例（当前为空）
└── docs/
    ├── svg/              # 本 README 引用的架构/功能/设计/生命周期图
    └── superpowers/specs/ # 设计文档
```

## Quick Start

### 1. Add the dependency

```toml
[dependencies]
rust-scout = "0.1"
tokio = { version = "1", features = ["macros", "rt"] }   # 仅示例需要
```

### 2. Minimal example (default in-memory driver)

```rust
use rust_scout::{Engine, EngineManager, ScoutConfig, SearchBuilder, SearchDocument};

#[tokio::main]
async fn main() -> rust_scout::Result<()> {
    // 默认驱动：内存 CollectionEngine，零依赖开箱即用
    let engine = EngineManager::new(ScoutConfig::collection()).engine()?;

    // 写入文档
    let mut book = SearchDocument::new(
        "book-1",
        serde_json::json!({
            "title": "Programming Rust",
            "author": "Blandy & Orendorff",
            "tags": ["rust", "systems"],
            "status": "published",
        }),
    )?;
    book.index = Some("books".to_string());
    engine.update(&[book]).await?;

    // 查询
    let result = engine
        .search(
            SearchBuilder::new("rust")
                .within("books")
                .where_field("status", "published")
                .order_by("title", false)
                .take(10),
        )
        .await?;

    println!("total = {}", result.total);
    for hit in &result.hits {
        println!("  [{}] {:?}", hit.id, hit.source);
    }
    Ok(())
}
```

## Usage

### Query Building (SearchBuilder)

All query operations are chained together and finally handed to `engine.search(&builder)`:

```rust
let builder = SearchBuilder::new("全文关键词")   // 全文搜索（可选，空串 = 匹配全部）
    .within("articles")                          // 指定索引（可选，默认 "default"）
    .where_field("status", "published")          // 等值过滤
    .where_in("tags", ["rust", "async"])         // IN 集合
    .where_not_in("category", ["draft"])         // NOT IN 集合
    .order_by("created_at", true)                // 多字段排序（true = desc）
    .order_by("title", false)
    .take(20)                                    // 每页条数
    .skip(40);                                   // 偏移
```

> `query` supports Lucene `query_string` syntax (fully effective under the ES driver): `"rust"`, `"title:rust AND tags:async"`, `"rust~2"` (fuzzy). The in-memory driver treats it as substring matching.

### Pagination

```rust
// 方式一：偏移截取
let page2 = SearchBuilder::new("rust").within("books").skip(10).take(10);
// 方式二：页码分页（page 从 1 起）
let page2 = engine.paginate(&SearchBuilder::new("rust").within("books"), 2, 10).await?;
```

### Multiple Indexes and Lifecycle

```rust
engine.create_index("books", serde_json::json!({})).await?;   // 建索引
engine.update(&docs).await?;                                  // 写文档
engine.flush("books").await?;                                 // 刷新可见性
engine.delete(&["book-1".to_string()]).await?;                // 删文档
engine.delete_index("books").await?;                          // 删索引
```

### Switching to Elasticsearch / OpenSearch

```bash
cargo add rust-scout --features elasticsearch
```

```rust
use rust_scout::{Engine, EngineManager, ScoutConfig};

let config = ScoutConfig::elasticsearch(
    "http://127.0.0.1:9200",      // 或 OpenSearch 地址
    Some("your-api-key".into()),   // 可选：ApiKey 认证
);
let engine = EngineManager::new(config).engine()?;
// —— 之后所有操作与内存驱动完全一致 ——
```

| Item | CollectionEngine (default) | ElasticsearchEngine |
|--------|--------------------------|---------------------|
| Dependencies | serde / thiserror only | reqwest (feature-enabled) |
| Full-text | Serialized substring matching | `query_string` |
| Filtering | In-memory matches() | term / terms / must_not |
| Sorting | In-memory sort_hits() | sort array |
| flush | no-op | `_refresh` |
| Default pagination | All results | size 10 |
| Default sorting | By id | By _score |

### Switching to Meilisearch

```bash
cargo add rust-scout --features meilisearch
```

```rust
use rust_scout::{Engine, EngineManager, ScoutConfig};

let config = ScoutConfig::meilisearch(
    "http://127.0.0.1:7700",   // Meilisearch 服务地址
    "your-master-key",          // 可选：API 密钥
);
let engine = EngineManager::new(config).engine()?;
// —— 之后所有操作与内存驱动完全一致 ——
```

### Engine Comparison

| Engine | driver | feature | Status |
|------|--------|---------|------|
| In-memory (default) | `collection` | built-in | Complete |
| Elasticsearch / OpenSearch | `elasticsearch` / `opensearch` | `elasticsearch` | Complete |
| Meilisearch | `meilisearch` | `meilisearch` | Complete |
| Typesense | `typesense` | `typesense` | Complete |
| Algolia | `algolia` | `algolia` | Complete |
| SQLite | `database` | `database` | Complete |
| Null (testing/disabled search) | `null` | `null` | Complete |
| XunSearch | `xunsearch` | `xunsearch` | stub (to be implemented) |

Config constructors for the remaining engines are documented on [docs.rs](https://docs.rs/rust-scout): `ScoutConfig::typesense(host, api_key)`, `ScoutConfig::algolia(app_id, api_key)`, `ScoutConfig::database(url, fields)`, `ScoutConfig::null()`, `ScoutConfig::xunsearch(host, project)`.

> For the SQLite engine (`database`), `total` is counted at the SQL layer (index + LIKE coarse filter); wheres / soft deletes may make `hits.len() < total` after in-memory filtering, and pagination is based on hits.

### Reserved Fields

`__soft_deleted` is the reserved field name used by the soft-delete feature (`Engine::soft_delete`, `SearchBuilder::with_trashed()` / `only_trashed()`), which engines use to filter out soft-deleted documents. User documents **should not** use this field name as a business field.

### Error Handling

All operations return `crate::Result<T>`, with errors converging into the unified `ScoutError`:

- `InvalidIndexName` — index name contains whitespace / `/` / starts with `.`, etc. (validated before writing)
- `InvalidResult` — document field is not a JSON object
- `Unsupported` — feature not enabled, etc.
- `Json` — serde errors
- `Http` / `Backend` — ES driver network and backend errors (when the feature is enabled)

### Bridging Business Models (Searchable)

Implement `Searchable` to map a business structure into an indexable document, and implement `SearchableStore` to encapsulate the three operations `index_documents` / `remove_documents` / `search`:

```rust
use rust_scout::{Searchable, SearchableStore, SearchDocument, SearchResult};

struct Article { id: String, title: String, body: String }

impl Searchable for Article {
    fn searchable_id(&self) -> String { self.id.clone() }
    fn to_searchable_json(&self) -> serde_json::Value {
        serde_json::json!({ "title": self.title, "body": self.body })
    }
}
```

## Support & Donations

If this project helps you, feel free to support it with a donation ☕ — your support is the motivation for continued maintenance!

### WeChat / Alipay

<img src="../../../docs/weixinpay.png" alt="微信打赏" width="130" height="130"/>
<img src="../../../docs/alipay.png" alt="支付宝打赏" width="130" height="130"/>

Scan with WeChat · Scan with Alipay

### Crypto Donations

| Network | Wallet Address | QR Code |
|------|----------|--------|
| BNB Smart Chain (BEP20) | `0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../../docs/coin/1.jpg" width="130" height="130"/> |
| Tron (TRC20) | `TEdDHWLajt1XvqtPDWmQctdrJaC3pzZZzz` | <img src="../../../docs/coin/2.jpg" width="130" height="130"/> |
| Ethereum (ERC20) | `0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../../docs/coin/3.jpg" width="130" height="130"/> |
| Aptos | `0x836e3780edfc3f7b2372b39e2a1a3a5d7adfaccd96c726f21cfde1b50dd68030` | <img src="../../../docs/coin/4.jpg" width="130" height="130"/> |
| Plasma | `0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../../docs/coin/5.jpg" width="130" height="130"/> |
| Polygon POS | `0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../../docs/coin/6.jpg" width="130" height="130"/> |
| Solana | `2hfhboHdmdrYsY25XfQSsEWxq5ip4EQsR7f4AzSRMUyr` | <img src="../../../docs/coin/7.jpg" width="130" height="130"/> |
| The Open Network (TON) | `UQB9kFQohzmXUir9QSSZq01iwl9aQZIDdBpNmDklljRtCoGK` | <img src="../../../docs/coin/8.jpg" width="130" height="130"/> |
| Arbitrum One | `0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../../docs/coin/9.jpg" width="130" height="130"/> |
| AVAX C-Chain | `0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../../docs/coin/10.jpg" width="130" height="130"/> |

### Global Transfers (Bank Transfer)

**Payee Information**

- Payee Name: WANG KEXUN
- Account Number: 881015918251

**Receiving Bank (ZA Bank)**

- SWIFT Code: `AABLHKHHXXX`
- Bank Name: ZA Bank Limited
- Bank Code: 387
- Bank Address: Core F, Cyberport 3, 100 Cyberport Road, Hong Kong

> The correspondent bank (intermediary bank) information below is for cross-border remittances, not the receiving bank's information. Please ask the remitting bank whether it is required.

- The correspondent bank for remittances in HKD, CNY and USD is **Citibank**:
  - Bank Name: Citibank N.A. Hong Kong
  - SWIFT Code: `CITIHKHXXXX`
  - Bank Code: 006 / Branch Code: 391
  - Branch Name: Hong Kong Branch
  - Bank Address: Citibank Tower, Citibank Plaza, 3 Garden Road, Central, Hong Kong
- The correspondent bank for remittances in other currencies is **BNY Mellon**:
  - Bank Name: THE BANK OF NEW YORK MELLON
  - SWIFT Code: `IRVTUS3NXXX`
  - Bank Address: THE BANK OF NEW YORK MELLON, 240 GREENWICH STREET, NEW YORK, United States

## License

MIT License. See [LICENSE](../../../LICENSE) for details.
