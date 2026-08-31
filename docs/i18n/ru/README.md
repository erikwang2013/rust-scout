# rust-scout

[![crates.io](https://img.shields.io/crates/v/rust-scout.svg)](https://crates.io/crates/rust-scout)
[![docs.rs](https://img.shields.io/docsrs/rust-scout)](https://docs.rs/rust-scout)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../../LICENSE)

[简体中文](../../../README.md) · [English](../en/README.md) · [日本語](../ja/README.md) · [한국어](../ko/README.md) · Русский · [Deutsch](../de/README.md) · [Français](../fr/README.md) · [Español](../es/README.md) · [Português](../pt/README.md) · [हिन्दी](../hi/README.md) · [العربية](../ar/README.md) · [বাংলা](../bn/README.md) · [Bahasa Indonesia](../id/README.md)

**rust-scout — абстракция полнотекстового поиска** — лёгкий интерфейсный слой полнотекстового поиска для Rust. В духе цепочечных запросов [Laravel Scout](https://laravel.com/docs/scout) через единый trait `Engine` абстрагируются различные бэкенды: память, Elasticsearch/OpenSearch, Meilisearch, Typesense, Algolia, SQLite и другие: **для разработки — драйвер в памяти с нулевыми зависимостями; для продакшена — бесшовное переключение на любой бэкенд без изменения бизнес-кода.**

```rust
let result = engine.search(
    SearchBuilder::new("rust 异步")
        .within("articles")
        .where_field("status", "published")
        .order_by("created_at", true)
        .take(10),
).await?;
```

## Возможности

| Возможность | Описание |
|------|------|
| 🔍 Полнотекстовый поиск | Драйвер в памяти — сопоставление подстрок; драйвер ES — синтаксис `query_string` (`поле:значение`) |
| ⚙️ Цепочечные запросы | `SearchBuilder`: query / within / where_field / where_in / where_not_in / order_by / take / skip |
| 🎯 Точная фильтрация | Сопоставление по равенству (ES → `term`), по множеству (ES → `terms` / `must_not`) |
| 📄 Сортировка по нескольким полям | asc / desc можно комбинировать |
| 📃 Пагинация | Смещение и усечение через `take`/`skip` + пагинация по страницам `paginate(page, per_page)` |
| 🗂️ Несколько индексов | Маршрутизация по полю `index` документа, индекс по умолчанию `"default"` |
| 🔄 Жизненный цикл индекса | Полный цикл `create_index` / `flush` / `delete_index` |
| 🔌 Подключаемые драйверы | По умолчанию — память с нулевыми зависимостями; features `elasticsearch` / `meilisearch` / `typesense` / `algolia` / `database` / `null` подключаются по мере необходимости; `xunsearch` — заглушка-stub |
| 🔒 Границы безопасности | Проверка имени индекса (`validate_index_name`) + процентное кодирование RFC 3986, исключает path-инъекции |

## Архитектура

![Архитектура](svg/architecture.svg)

## Обзор возможностей

![Возможности](svg/features.svg)

## Принципы проектирования

![Проектирование](svg/design.svg)

## Жизненный цикл

![Жизненный цикл](svg/lifecycle.svg)

## Структура проекта

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

## Быстрый старт

### 1. Добавление зависимости

```toml
[dependencies]
rust-scout = "0.1"
tokio = { version = "1", features = ["macros", "rt"] }   # 仅示例需要
```

### 2. Минимальный пример (драйвер в памяти по умолчанию)

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

## Использование

### Построение запросов (SearchBuilder)

Все операции запроса собираются в цепочку и в итоге передаются в `engine.search(&builder)`:

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

> `query` поддерживает синтаксис Lucene `query_string` (полностью работает с драйвером ES):
> `"rust"`, `"title:rust AND tags:async"`, `"rust~2"` (нечёткое). Драйвер в памяти обрабатывает как сопоставление подстрок.

### Пагинация

```rust
// 方式一：偏移截取
let page2 = SearchBuilder::new("rust").within("books").skip(10).take(10);
// 方式二：页码分页（page 从 1 起）
let page2 = engine.paginate(&SearchBuilder::new("rust").within("books"), 2, 10).await?;
```

### Несколько индексов и жизненный цикл

```rust
engine.create_index("books", serde_json::json!({})).await?;   // 建索引
engine.update(&docs).await?;                                  // 写文档
engine.flush("books").await?;                                 // 刷新可见性
engine.delete(&["book-1".to_string()]).await?;                // 删文档
engine.delete_index("books").await?;                          // 删索引
```

### Переход на Elasticsearch / OpenSearch

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

| Критерий | CollectionEngine (по умолчанию) | ElasticsearchEngine |
|--------|--------------------------|---------------------|
| Зависимости | только serde / thiserror | reqwest (при включённом feature) |
| Полнотекстовый поиск | сопоставление подстрок при сериализации | `query_string` |
| Фильтрация | matches() в памяти | term / terms / must_not |
| Сортировка | sort_hits() в памяти | массив sort |
| flush | no-op | `_refresh` |
| Пагинация по умолчанию | все результаты | size 10 |
| Сортировка по умолчанию | по id | по _score |

### Переход на Meilisearch

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

### Сравнение движков

| Движок | driver | feature | Статус |
|------|--------|---------|------|
| Память (по умолчанию) | `collection` | встроенный | Полный |
| Elasticsearch / OpenSearch | `elasticsearch` / `opensearch` | `elasticsearch` | Полный |
| Meilisearch | `meilisearch` | `meilisearch` | Полный |
| Typesense | `typesense` | `typesense` | Полный |
| Algolia | `algolia` | `algolia` | Полный |
| SQLite | `database` | `database` | Полный |
| Null (тесты / отключение поиска) | `null` | `null` | Полный |
| XunSearch | `xunsearch` | `xunsearch` | stub (ожидает реализации) |

Конструкторы конфигурации для остальных движков см. в [docs.rs](https://docs.rs/rust-scout): `ScoutConfig::typesense(host, api_key)`, `ScoutConfig::algolia(app_id, api_key)`, `ScoutConfig::database(url, fields)`, `ScoutConfig::null()`, `ScoutConfig::xunsearch(host, project)`.

> В движке SQLite (`database`) `total` считается на уровне SQL (индекс + грубая фильтрация LIKE);
> после фильтрации wheres / soft-delete в памяти `hits.len()` может быть меньше `total` — пагинация ориентируется на hits.

### Зарезервированные поля

`__soft_deleted` — зарезервированное имя поля для мягкого удаления (`Engine::soft_delete`, `SearchBuilder::with_trashed()` / `only_trashed()`), по которому движок отфильтровывает мягко удалённые документы. В пользовательских документах **не следует** использовать это имя для бизнес-полей.

### Обработка ошибок

Все операции возвращают `crate::Result<T>`, ошибки сводятся к единому `ScoutError`:

- `InvalidIndexName` — имя индекса содержит пробелы / `/` / начинается с `.` и т. п. (проверка перед записью)
- `InvalidResult` — поле документа не является JSON-объектом
- `Unsupported` — feature не включён и т. п.
- `Json` — ошибка serde
- `Http` / `Backend` — сетевые ошибки и ошибки бэкенда драйвера ES (при включённом feature)

### Мост к бизнес-моделям (Searchable)

Реализуйте `Searchable`, чтобы отобразить бизнес-структуру в индексируемые документы, и `SearchableStore`, чтобы обернуть три операции `index_documents` / `remove_documents` / `search`:

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

## Поддержка и пожертвования

Если этот проект оказался вам полезен, поддержите нас чашечкой кофе ☕ — ваша поддержка помогает продолжать его развивать!

### WeChat / Alipay

<img src="../../../docs/weixinpay.png" alt="微信打赏" width="130" height="130"/>
<img src="../../../docs/alipay.png" alt="支付宝打赏" width="130" height="130"/>

Отсканируйте QR-код в WeChat · Отсканируйте QR-код в Alipay

### Пожертвование в криптовалюте

| Сеть | Адрес кошелька | QR-код |
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

### Международные переводы (банковский перевод)

**Информация о получателе**

- Имя получателя: WANG KEXUN
- Номер счёта получателя: 881015918251

**Банк получателя (ZA Bank)**

- SWIFT Code: `AABLHKHHXXX`
- Название банка: ZA Bank Limited
- Код банка: 387
- Адрес банка: Core F, Cyberport 3, 100 Cyberport Road, Hong Kong

> Информация о банке-посреднике (корреспондентском банке) для трансграничных переводов, а не о банке получателя. Уточните в банке, из которого делаете перевод, требуется ли её предоставлять.

- Для переводов в гонконгских долларах, китайских юанях и долларах США банк-посредник — **Citibank**:
  - Название банка: Citibank N.A. Hong Kong
  - SWIFT Code: `CITIHKHXXXX`
  - Код банка: 006 / код отделения: 391
  - Название отделения: Hong Kong Branch
  - Адрес банка: Citibank Tower, Citibank Plaza, 3 Garden Road, Central, Hong Kong
- Для переводов в других валютах банк-посредник — **BNY Mellon**:
  - Название банка: THE BANK OF NEW YORK MELLON
  - SWIFT Code: `IRVTUS3NXXX`
  - Адрес банка: THE BANK OF NEW YORK MELLON, 240 GREENWICH STREET, NEW YORK, United States

## Лицензия

Лицензия MIT. Подробнее см. [LICENSE](../../../LICENSE).
