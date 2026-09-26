# rust-scout

[![crates.io](https://img.shields.io/crates/v/rust-scout.svg)](https://crates.io/crates/rust-scout)
[![docs.rs](https://img.shields.io/docsrs/rust-scout)](https://docs.rs/rust-scout)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../../LICENSE)

[简体中文](../../../README.md) · [English](../en/README.md) · 日本語 · [한국어](../ko/README.md) · [Bahasa Indonesia](../id/README.md) · [Русский](../ru/README.md) · [Deutsch](../de/README.md) · [Français](../fr/README.md) · [Español](../es/README.md) · [Português](../pt/README.md) · [हिन्दी](../hi/README.md) · [العربية](../ar/README.md) · [বাংলা](../bn/README.md)

**rust-scout 全文検索ライブラリ抽象化** —— Rust 向けの軽量な全文検索インターフェース層。
[Laravel Scout](https://laravel.com/docs/scout) のチェーンクエリの考え方を取り入れ、統一された
`Engine` trait で **8 種のバックエンド**（メモリ、Elasticsearch/OpenSearch、Meilisearch、
Typesense、Algolia、SQLite、XunSearch、Null）を抽象化する：**開発時は依存ゼロのメモリドライバ、
本番では任意のバックエンドへシームレスに切り替え、業務コードは一行も変えない。**

![プロジェクトのペット：嗅探猟犬 Scout](svg/pet.svg)

> プロジェクトのペット **嗅探猟犬 Scout**（Search Hound）—— 書類を嗅ぎ、索引を追う。
> ドキュメントの中だけでなく、ターミナルのバナーやエラー表示にも現れる。詳しくは
> [プロジェクトのペット](#プロジェクトのペット) を参照。

```rust
let result = engine.search(
    SearchBuilder::new("rust 异步")
        .within("articles")
        .where_field("status", "published")
        .order_by("created_at", true)
        .take(10),
).await?;
```

## 機能

| 能力 | 説明 |
|------|------|
| 🔍 全文検索 | メモリドライバは部分文字列一致。HTTP ドライバはバックエンド固有の構文（ES は `query_string`、`フィールド:値`） |
| ⚙️ チェーンクエリ | `SearchBuilder`：query / within / where_field / where_in / where_not_in / order_by / take / skip / option |
| 🎯 完全一致フィルタ | 等値マッチ（ES → `term`）、集合マッチ（ES → `terms` / `must_not`） |
| 📄 複数フィールドソート | asc / desc を重ねがけ可能。JSON 型をまたいでも順序は一定 |
| 📃 ページング | `take`/`skip` のオフセット切り出し + `paginate(page, per_page)` のページ番号方式 |
| 🗂️ マルチインデックス | 文書単位の `index` フィールドでルーティング、既定インデックスは `"default"` |
| 🔄 索引ライフサイクル | `create_index` / `flush` / `reindex` / `delete_index` の全工程 |
| 🗑️ ソフト削除 | `soft_delete` が `__soft_deleted` を立て、`with_trashed()` / `only_trashed()` で三態フィルタ |
| 📦 バッチ操作 | `update_bulk` / `delete_bulk` で往復を削減。`delete_in` は指定索引に限定して削除 |
| 🔌 プラガブルドライバ | 既定は依存ゼロ。8 種のバックエンドはそれぞれ feature ゲートされ、不要なものはコンパイルされない |
| 🔒 安全境界 | 索引名の検証（`validate_index_name`）+ RFC 3986 パーセント符号化でパス注入を防ぐ |
| 🐕 プロジェクトのペット | 嗅探猟犬 Scout：ターミナルバナー + エラーごとの調査ヒント（`rust_scout::pet`） |

## アーキテクチャ設計

![アーキテクチャ](svg/architecture.svg)

5 層構造：アプリケーション層 → データ契約層（serde JSON）→ コア層（`EngineManager` + `Engine` trait）
→ ドライバ層（転送方式で 4 組、計 8 ドライバ）→ ストレージ層。層をまたぐ接ぎ目は `Engine` trait ただ一つ。

## 機能設計

![機能](svg/features.svg)

12 の能力：チェーンクエリ、全文、完全一致／集合フィルタ、ソート、ページング、マルチインデックス、
ソフト削除、索引ライフサイクル、バッチと限定削除、プラガブルドライバ、安全境界。

## 設計思想

![設計思想](svg/design.svg)

## ライフサイクル

![ライフサイクル](svg/lifecycle.svg)

7 つの段階：作成 → 書き込み → 更新 → 検索 → 文書削除 → 再構築 → 破棄。図の下半分は、
4 類のドライバが各段階でどう振る舞うかの対照表。

## プロジェクト構成

```
rust-scout/
├── Cargo.toml              # 依存と feature 宣言（既定 default = []、依存ゼロ）
├── src/
│   ├── lib.rs              # crate ルート：モジュール公開 + feature ゲート付き再エクスポート
│   │
│   ├── engine.rs           # Engine trait：唯一のドライバ契約（必須 8 + 既定実装 5）
│   ├── manager.rs          # EngineManager：ファサード、driver で振り分け Arc<dyn Engine> をキャッシュ
│   ├── config.rs           # ScoutConfig（8 コンストラクタ）+ validate_index_name + percent_encode
│   │
│   ├── builder.rs          # SearchBuilder / Where / Order / TrashedFilter：チェーンクエリ
│   ├── document.rs         # SearchDocument：書き込み文書（serde JSON 契約）
│   ├── result.rs           # SearchResult / SearchHit：検索結果
│   ├── searchable.rs       # Searchable / SearchableStore：業務モデルのブリッジ
│   ├── error.rs            # ScoutError + Result<T> + pet_hint()
│   ├── pet.rs              # プロジェクトのペット：嗅探猟犬 Scout（バナー + エラーヒント）
│   │
│   ├── collection_engine.rs    # メモリドライバ（既定、依存ゼロ）
│   ├── null_engine.rs          # 空ドライバ：書き込みを捨て、常に空結果        [null]
│   ├── elasticsearch_engine.rs # ES / OpenSearch（REST）                    [elasticsearch]
│   │   └── query.rs            #   query_string の構築とレスポンス解析
│   ├── meilisearch_engine.rs   # Meilisearch（REST）                        [meilisearch]
│   ├── typesense_engine.rs     # Typesense（REST）                          [typesense]
│   │   └── typesense_query.rs  #   検索パラメータと filter_by の構築
│   ├── algolia_engine.rs       # Algolia（マネージドクラウド REST）          [algolia]
│   ├── database_engine.rs      # SQLite（sqlx、LIKE 粗ふるい + メモリ精査）    [database]
│   ├── xunsearch_engine.rs     # XunSearch：xunsearchd ネイティブ TCP プロトコル [xunsearch]
│   │   ├── xunsearch_query.rs  #   パケットの符号化と ini フィールド定義
│   │   └── xunsearch_tests.rs  #   mock サーバーを使った E2E テスト
│   │
│   └── (ユニットテストは各モジュール末尾の #[cfg(test)] mod tests に内蔵)
├── tests/                  # 結合テスト（現在は空、テストは src に内蔵）
├── examples/
│   └── pet.rs              # cargo run --example pet：ペットバナー + エラーヒントのデモ
└── docs/
    ├── svg/                # プロジェクトのペット + アーキテクチャ / 機能 / 設計 / ライフサイクル図
    ├── i18n/               # 12 言語の README と対応する SVG
    ├── coin/               # 投げ銭用 QR コード
    └── superpowers/specs/  # 設計ドキュメント
```

> `[feature]` はそのドライバに必要な Cargo feature を示す。有効化されていない場合、
> `EngineManager` は黙って機能を落とすのではなく `ScoutError::Unsupported` を返す。

## クイックスタート

### 1. 依存関係を追加

```toml
[dependencies]
rust-scout = "0.5"
tokio = { version = "1", features = ["macros", "rt"] }   # サンプルでのみ必要
```

### 2. 最小サンプル（既定のメモリドライバ）

```rust
use rust_scout::{Engine, EngineManager, ScoutConfig, SearchBuilder, SearchDocument};

#[tokio::main]
async fn main() -> rust_scout::Result<()> {
    // 既定ドライバ：メモリの CollectionEngine、依存ゼロですぐ使える
    let engine = EngineManager::new(ScoutConfig::collection()).engine()?;

    // 文書を書き込む
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

    // 検索
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

## 使い方

### クエリ構築（SearchBuilder）

すべての検索操作はチェーンで組み立て、最後に `engine.search(&builder)` に渡す：

```rust
let builder = SearchBuilder::new("全文キーワード")  // 全文検索（任意、空文字 = 全件一致）
    .within("articles")                          // 対象インデックス（任意、既定 "default"）
    .where_field("status", "published")          // 等値フィルタ
    .where_in("tags", ["rust", "async"])         // IN 集合
    .where_not_in("category", ["draft"])         // NOT IN 集合
    .order_by("created_at", true)                // 複数フィールドソート（true = desc）
    .order_by("title", false)
    .take(20)                                    // 1 ページの件数
    .skip(40)                                    // オフセット
    .option("highlight", true)                   // ドライバ固有のパススルーオプション
    .with_trashed();                             // ソフト削除の三態：除外 / 含める / のみ
```

> `query` は Lucene の `query_string` 構文をサポートする（ES ドライバでは完全に有効）：
> `"rust"`、`"title:rust AND tags:async"`、`"rust~2"`（あいまい検索）。その他のドライバは
> 独自のネイティブ構文か部分文字列一致で処理する。

### ページング

```rust
// 方法 1：オフセット切り出し
let page2 = SearchBuilder::new("rust").within("books").skip(10).take(10);
// 方法 2：ページ番号方式（page は 1 から）
let page2 = engine.paginate(&SearchBuilder::new("rust").within("books"), 2, 10).await?;
```

### マルチインデックスとライフサイクル

```rust
engine.create_index("books", serde_json::json!({})).await?;    // 索引を作成
engine.update(&docs).await?;                                   // 文書を書き込み
engine.update_bulk(&docs).await?;                              // バッチ書き込み（対応バックエンドは bulk API）
engine.flush("books").await?;                                  // 可視性を更新
engine.search(&builder).await?;                                // 検索
engine.delete_in("books", &["book-1".to_string()]).await?;     // 索引を限定して文書削除
engine.soft_delete(&["book-2".to_string()]).await?;            // ソフト削除（フラグを立てる）
engine.reindex("books", "books_v2").await?;                    // 索引を再構築
engine.delete_index("books").await?;                           // 索引を削除
```

> `delete` は索引情報を持たないため、意味はエンジンごとに異なる（メモリドライバは索引をまたいで
> 削除し、ES は `default` 索引だけを見る）。特定の索引に限定したい場合は `delete_in` を使う。

### Elasticsearch / OpenSearch への切り替え

```bash
cargo add rust-scout --features elasticsearch
```

```rust
use rust_scout::{Engine, EngineManager, ScoutConfig};

let config = ScoutConfig::elasticsearch(
    "http://127.0.0.1:9200",      // または OpenSearch のアドレス
    Some("your-api-key".into()),   // 任意：ApiKey 認証
);
let engine = EngineManager::new(config).engine()?;
// —— 以降のすべての操作はメモリドライバと完全に同じ ——
```

| 項目 | CollectionEngine（既定） | ElasticsearchEngine |
|--------|--------------------------|---------------------|
| 依存 | serde / thiserror のみ | reqwest（feature 有効時） |
| 全文 | シリアライズ後の部分一致 | `query_string` |
| フィルタ | メモリ内 matches() | term / terms / must_not |
| ソート | メモリ内 sort_hits() | sort 配列 |
| flush | no-op | `_refresh` |
| ページング既定 | 全件 | size 10 |
| ソート既定 | id 順 | _score 順 |

### Meilisearch への切り替え

```bash
cargo add rust-scout --features meilisearch
```

```rust
use rust_scout::{Engine, EngineManager, ScoutConfig};

let config = ScoutConfig::meilisearch(
    "http://127.0.0.1:7700",   // Meilisearch のアドレス
    "your-master-key",          // 任意：API キー
);
let engine = EngineManager::new(config).engine()?;
// —— 以降のすべての操作はメモリドライバと完全に同じ ——
```

### エンジン対照表

| エンジン | driver | feature | 転送 | 状態 |
|------|--------|---------|------|------|
| メモリ（既定） | `collection` | 組み込み | プロセス内 | 完全 |
| Elasticsearch / OpenSearch | `elasticsearch` / `opensearch` | `elasticsearch` | HTTP REST | 完全 |
| Meilisearch | `meilisearch` | `meilisearch` | HTTP REST | 完全 |
| Typesense | `typesense` | `typesense` | HTTP REST | 完全 |
| Algolia | `algolia` | `algolia` | HTTP REST | 完全 |
| SQLite | `database` | `database` | ローカルファイル | 完全 |
| XunSearch | `xunsearch` | `xunsearch` | ネイティブ TCP | 完全 |
| Null（テスト / 検索の無効化） | `null` | `null` | — | 完全 |

その他のエンジンの設定コンストラクタは [docs.rs](https://docs.rs/rust-scout) を参照：`ScoutConfig::typesense(host, api_key)`、`ScoutConfig::algolia(app_id, api_key)`、`ScoutConfig::database(url, fields)`、`ScoutConfig::null()`、`ScoutConfig::xunsearch(host, project)`。

> SQLite エンジン（`database`）の `total` は SQL 層の件数（索引 + LIKE 粗ふるい）であり、
> wheres / ソフト削除をメモリで絞り込むと `hits.len() < total` になりうる。ページングは hits を基準とする。

### 予約フィールド

`__soft_deleted` はソフト削除機能（`Engine::soft_delete`、`SearchBuilder::with_trashed()`
/ `only_trashed()`）が使う予約フィールド名で、エンジンはこれを見てソフト削除済みの文書を除外する。
利用者の文書でこのフィールド名を業務フィールドとして使う**べきではない**。

### エラー処理

すべての操作は `crate::Result<T>` を返し、エラーは統一された `ScoutError` に収束する：

| バリアント | 発生条件 | feature |
|------|----------|---------|
| `InvalidIndexName` | 索引名に空白 / `/` / `\` を含む、`.` で始まる、または空（書き込み前に検証） | 組み込み |
| `InvalidResult` | 文書のフィールドが JSON オブジェクトでない | 組み込み |
| `Unsupported` | ドライバに必要な feature が無効、必須設定の欠落、エンジンが未対応の操作 | 組み込み |
| `Json` | serde のシリアライズ / デシリアライズエラー | 組み込み |
| `Http` | HTTP リクエストの失敗（接続、タイムアウト、ステータスコード） | HTTP 4 エンジン |
| `Sqlx` | SQLite のエラー | `database` |
| `Backend` | バックエンドがエラー応答を返した。元の情報をそのまま透過 | HTTP 4 エンジン / `xunsearch` |
| `XunSearch` / `XunSearchIo` | プロトコル解析の失敗 / TCP I/O の失敗 | `xunsearch` |

すべてのバリアントが調査ヒントを伴う。詳しくは [`ScoutError::pet_hint()`](#プロジェクトのペット)。

### 業務モデルのブリッジ（Searchable）

`Searchable` を実装して業務構造をインデックス可能な文書に写像し、`SearchableStore` を実装して
`index_documents` / `remove_documents` / `search` の 3 操作を包む：

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

## プロジェクトのペット

![プロジェクトのペット：嗅探猟犬 Scout](svg/pet.svg)

**Scout · 嗅探猟犬**（Search Hound）—— 書類を嗅ぎ、索引を追う。クエリのあるところに必ずいる。
イラスト版は [`svg/pet.svg`](svg/pet.svg)、ターミナルではこう見える：

```console
$ cargo run --example pet
```

```

      ___              ___
     /   \            /   \
    |     |__________|     |
    |     /          \     |
    |    |   o    o   |    |
    |    |     __     |    |
    |     \   /  \   /     |
     \     \  \__/  /     /
      \     \________/    /
       \_________________/
         \   ~~~~~~   /
          \__________/
             |    |
            _|    |_
           |__|  |__|


   ,^.     ,^.     ,^.     ,^.

  Scout · 嗅探猎犬 · rust-scout
  嗅探文档，追踪索引 —— 哪里有查询，哪里就有它
```

ペットは [`rust_scout::pet`](../../../src/pet.rs) モジュールに住んでおり、**依存を一切増やさない**：

| 項目 | 説明 |
|----|------|
| `pet::NAME` / `pet::SPECIES` / `pet::TAGLINE` | 名札の情報 |
| `pet::ART` | ASCII の姿（意図的に 7 ビット ASCII のみ、CJK ターミナルでも崩れない） |
| `pet::banner()` | ターミナルバナー。プレーンテキストでエスケープシーケンスなし、ログに安全に書ける |
| `pet::hint(&err)` | エラーごとの調査ヒント、`&'static str` を返す |
| `pet::format_error(&err)` | 元のエラー + ヒントを人向けに整形 |
| `ScoutError::pet_hint()` | 同じヒントをエラー型に直接ぶら下げたもの |

```rust
use rust_scout::{pet, ScoutError};

println!("{}", pet::banner());

let err = ScoutError::Unsupported("feature が不足".into());
eprintln!("{}", pet::format_error(&err));
// error: unsupported operation: feature が不足
//
//   [o_o] Scout：这个后端我还没找到路 —— Cargo.toml 里对应的 feature 启用了吗？
```

> **なぜヒントを `Display` に直接入れないのか？** `ScoutError` の `Display` は単一行で
> 機械可読に保たれている —— `?` による伝播、ログ収集、CI でのエラー文字列の grep が
> すべてこれに依存している。ペットのヒント付きの人間向け出力が必要なら
> `pet::format_error()` を使う。

## サポートと投げ銭

このプロジェクトが役に立ったなら、投げ銭で支援していただけると嬉しいです ☕ —— 皆様の支援が継続的なメンテナンスの原動力です！

### WeChat / Alipay

<img src="../../../docs/weixinpay.png" alt="WeChat 投げ銭" width="130" height="130"/>
<img src="../../../docs/alipay.png" alt="Alipay 投げ銭" width="130" height="130"/>

WeChat でスキャン · Alipay でスキャン

### 仮想通貨での投げ銭

| メインネット | ウォレットアドレス | QR コード |
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

### 海外送金（銀行振込）

**受取人情報**

- 受取人名：WANG KEXUN
- 受取口座番号：881015918251

**受取銀行（ZA Bank）**

- SWIFT Code：`AABLHKHHXXX`
- 銀行名：ZA Bank Limited
- 銀行番号：387
- 銀行住所：Core F, Cyberport 3, 100 Cyberport Road, Hong Kong

> 以下は越境送金の代理銀行（中継銀行）の情報であり、受取銀行の情報ではない。必要かどうかは送金元の銀行に確認すること。

- 香港ドル、人民元、米ドルの入金時の代理銀行は **Citibank**：
  - 銀行名：Citibank N.A. Hong Kong
  - SWIFT Code：`CITIHKHXXXX`
  - 銀行番号：006 / 支店番号：391
  - 支店名：Hong Kong Branch
  - 銀行住所：Citibank Tower, Citibank Plaza, 3 Garden Road, Central, Hong Kong
- その他の通貨の入金時の代理銀行は **BNY Mellon**：
  - 銀行名：THE BANK OF NEW YORK MELLON
  - SWIFT Code：`IRVTUS3NXXX`
  - 銀行住所：THE BANK OF NEW YORK MELLON, 240 GREENWICH STREET, NEW YORK, United States

## ライセンス

MIT License。詳しくは [LICENSE](../../../LICENSE) を参照。
