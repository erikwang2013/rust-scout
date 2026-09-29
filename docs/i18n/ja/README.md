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

![プロジェクトのペット：検索ロボット Scout](svg/pet.svg)

> プロジェクトのペット **検索ロボット Scout** —— 胸に八つのモジュールスロット、挿したものを使う。
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
| 🗑️ ソフト削除 | `soft_delete_in(index, ids)` が `__soft_deleted` を立て、`with_trashed()` / `only_trashed()` で三態フィルタ |
| 📦 バッチ操作 | `update_bulk` / `delete_bulk` で往復を削減。`delete_in` は指定索引に限定して削除 |
| 🔌 プラガブルドライバ | 既定は依存ゼロ。8 種のバックエンドはそれぞれ feature ゲートされ、不要なものはコンパイルされない |
| 🔒 安全境界 | 索引名・フィールド名・ホストの検証（`validate_index_name` / `validate_field_name` / `validate_host`）+ RFC 3986 パーセント符号化でパス注入を防ぐ |
| 🤖 プロジェクトのペット | 検索ロボット Scout：ターミナルバナー + エラーごとの調査ヒント（`rust_scout::pet`） |

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
│   ├── engine.rs           # Engine trait：唯一のドライバ契約（必須 6 + 既定実装 8）
│   ├── manager.rs          # EngineManager：ファサード、driver で振り分け Arc<dyn Engine> をキャッシュ
│   ├── config.rs           # ScoutConfig（9 コンストラクタ）+ validate_index_name + percent_encode
│   │
│   ├── builder.rs          # SearchBuilder / Where / Order / TrashedFilter：チェーンクエリ
│   ├── document.rs         # SearchDocument：書き込み文書（serde JSON 契約）
│   ├── result.rs           # SearchResult / SearchHit：検索結果
│   ├── error.rs            # ScoutError + Result<T> + pet_hint()
│   ├── pet.rs              # プロジェクトのペット：検索ロボット Scout（バナー + エラーヒント）
│   │
│   ├── collection_engine.rs    # メモリドライバ（既定、依存ゼロ）
│   ├── null_engine.rs          # 空ドライバ：書き込みを捨て、常に空結果        [null]
│   ├── elasticsearch_engine.rs # ES / OpenSearch（REST）                    [elasticsearch]
│   ├── query.rs                #   query_string の構築とレスポンス解析
│   ├── meilisearch_engine.rs   # Meilisearch（REST）                        [meilisearch]
│   ├── typesense_engine.rs     # Typesense（REST）                          [typesense]
│   ├── typesense_query.rs      #   検索パラメータと filter_by の構築
│   ├── algolia_engine.rs       # Algolia（マネージドクラウド REST）          [algolia]
│   ├── database_engine.rs      # SQLite（sqlx、LIKE 粗ふるい + メモリ精査）    [database]
│   ├── xunsearch_engine.rs     # XunSearch：xunsearchd ネイティブ TCP プロトコル [xunsearch]
│   ├── xunsearch_query.rs      #   パケットの符号化と ini フィールド定義
│   ├── xunsearch_tests.rs      #   mock サーバーを使った E2E テスト
│   │
│   └── (ユニットテストは各モジュール末尾の #[cfg(test)] mod tests に内蔵)
├── tests/                  # 結合テスト（現在は空、テストは src に内蔵）
├── examples/
│   ├── collection_search.rs # cargo run --example collection_search：実行できる検索フロー全体
│   └── pet.rs              # cargo run --example pet：ペットバナー + エラーヒントのデモ
└── docs/
    ├── svg/                # プロジェクトのペット + アーキテクチャ / 機能 / 設計 / ライフサイクル図
    ├── i18n/               # 12 言語の README と対応する SVG
    ├── coin/               # 投げ銭用 QR コード
    └── superpowers/specs/  # 設計ドキュメント
```

> `[feature]` はそのドライバに必要な Cargo feature を示す。有効化されていない場合、
> `EngineManager` は黙って機能を落とすのではなく `ScoutError::Unsupported` を返す。未知の
> `driver` 文字列（打ち間違い、末尾の空白、`OpenSearch` のような大文字小文字の誤り）もエラーに
> なる —— 以前は黙ってメモリドライバに落ちていた。

### ドライバ能力の差異

既定のメモリドライバが意味論の基準になる。バックエンドにできないことがあれば、黙って誤った
結果を返すのではなく **明示的にそう告げる**：

| ドライバ | 制約 | 挙動 |
|--------|-----------|-----------|
| Algolia | ソートには事前に構築したレプリカ索引が必要で、クエリごとに選ぶことはできない | `order_by` は **無視される**（結果自体は返り、順序が不定になるだけ） |
| XunSearch | `where_in` / `where_not_in` に対応するプロトコルコマンドがない | `Unsupported` を返す；`where_field` を使う |
| XunSearch | サーバーが扱えるソートフィールドは 1 つだけ | `order_by` を複数指定すると `Unsupported` を返す |
| XunSearch | ソフト削除は未実装 | `soft_delete` / `soft_delete_in` / `only_trashed` は `Unsupported` を返す |
| XunSearch | 索引の作成にはフィールド定義の ini が必要 | `create_index` は `Unsupported` を返す（`XunSearchEngine::new` に ini を渡す） |
| Typesense | 空でない `q` には `query_by` が必須 | `.option("query_by", "field1,field2")` を渡さないとバックエンドは 400 `Parameter \`query_by\` is required` を返す；ドライバはフィールドを推測しない（推測を誤ると順位が黙って変わる） |
| Algolia | 書き込みはタスク制：POST が返すのは `taskID` だけで、受理された ≠ 検索可能になった | `update` / `update_bulk` / `delete` / `delete_in` / `delete_bulk` / `soft_delete_in` / `reindex` は `/1/indexes/{index}/task/{taskID}` を `published` までポーリングしてから戻る；30 秒で published にならなければエラー（結果不明を成功とは報告しない） |
| Meilisearch | 書き込みはタスク制：POST が返すのは `taskUid` だけ | 同じ形で `/tasks/{uid}` を終端状態までポーリング；`failed` / `canceled` のタスクは `Backend` エラーとして返る（以前は失敗した書き込みが成功として黙って捨てられていた）、30 秒で終端に達しなければエラー。バッチ書き込みの所要時間は「バックエンドのタスクが終わるまで」になる |
| database | `reindex` はコピーではなく**移動** | 元の索引は空になる（`id` がグローバル主キーで、同じ id は二つの索引に存在できない）；元を残したいなら database ドライバを使わないこと |
| XunSearch | `index: None` は `default` という名前の索引を指すようになった | 他の七つのドライバと同じ；以前は xunsearchd 側の既定データベース `db` に落ちていた —— `index: None` で書いたデータは `index("db")` を渡さないと引けない |
| 既定件数 | `take` を渡さない場合、collection / database は**すべて**の命中を返す | 残り六つのドライバは既定で **10** 件だけ返す（各バックエンドの慣例的な上限） |

意図的に意味論をそろえている点が 2 つある：

- **ES が壊れたクエリ構文**（`"("`、`"foo AND"`）を受け取ったときは、エラーではなく空の結果を
  返す —— メモリドライバは同じ入力に対して部分文字列一致を行い、400 を返すと
  「バックエンドを差し替えてもコードはそのまま」が成り立たなくなるから。
- **`delete` と `soft_delete` は索引の情報を持たない**ため、複数の索引にまたがるかどうかは
  バックエンド次第。1 つの索引を対象にするには、常に `delete_in` / `soft_delete_in` を使う。

## クイックスタート

### 1. 依存関係を追加

```toml
[dependencies]
rust-scout = "0.8"
tokio = { version = "1", features = ["macros", "rt"] }   # サンプルでのみ必要
```

### 2. 最小サンプル（既定のメモリドライバ）

> コピー＆ペーストではなくそのまま実行するなら：`cargo run --example collection_search` —— 書き込み → クエリ（where + ソート）→ ページング → ソフト削除 → 索引の削除、一連の流れをまるごと、feature も外部サービスも不要。

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
engine.soft_delete_in("books", &["book-2".to_string()]).await?;            // ソフト削除（フラグを立てる；XunSearch は Unsupported）
engine.reindex("books", "books_v2").await?;                    // 索引を再構築
engine.delete_index("books").await?;                           // 索引を削除
```

> `delete` は索引情報を持たないため、意味はエンジンごとに異なる（メモリドライバは索引をまたいで
> 削除し、ES は `default` 索引だけを見る）。特定の索引に限定したい場合は `delete_in` を使う。
>
> ソフト削除も同様：`soft_delete_in(index, ids)` が八つのうち七つのエンジンで信頼できる経路。
> **XunSearch は `soft_delete` も `soft_delete_in` も実装しておらず**、どちらも
> `ScoutError::Unsupported` を返す。索引を指定しない `soft_delete` は同期バックエンド
> （`collection` / `database`）だけが索引をまたいで印を付けられる；HTTP バックエンドにはできず、
> `ScoutError::Unsupported` を返す（黙って何もしないのではなく）。
>
> `flush` の契約は「書き込みの可視性を更新する」ことで、**どのドライバも索引を空にしない**：
> ES は `_refresh`、XunSearch は `CMD_INDEX_COMMIT` を送り、他のドライバは書き込みが即時可視
> なので no-op。索引を空にするには `delete_index` を使う。

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

> SQLite エンジン（`database`）の `total` は**フィルタ後**のヒット数（`CollectionEngine`
> と一致）：SQL は索引 + LIKE の粗ふるいで候補を取るだけで、wheres / ソフト削除 / ソート /
> ページングはすべてメモリで行う。ページングは SQL の `LIMIT/OFFSET` に押し下げられない——
> そうするとウィンドウ外の一致行を永久に取れなくなる。

### ドライバ設定キー

`ScoutConfig::insert`（または直接 `options` に書いた）のキーを、`EngineManager` がドライバ構築時に読む：

| キー | ドライバ | 説明 |
|------|----------|---------|
| `elasticsearch.host` / `opensearch.host` | Elasticsearch / OpenSearch | 既定 `http://127.0.0.1:9200` |
| `elasticsearch.api_key` / `opensearch.api_key` | Elasticsearch / OpenSearch | 任意 |
| `meilisearch.host` / `meilisearch.api_key` | Meilisearch | host の既定は `http://127.0.0.1:7700` |
| `typesense.host` / `typesense.api_key` | Typesense | host の既定は `http://127.0.0.1:8108` |
| `algolia.app_id` / `algolia.api_key` | Algolia | どちらも必須 |
| `database.url` | SQLite | 必須 |
| `database.fields` | SQLite | **検索にはもう関与しない**。互換性のためだけに残しており、次の破壊的リリースで削除予定。以前はテキスト検索を列挙したフィールドだけに限定していたため、同じ入力でも `database` がメモリ側の基準（`collection`）と食い違っていた（`fields=["title"]` + 文書 `{"title":"Rust","tag":"async"}` + クエリ `"async"` → database 0 件、collection 1 件）。現在は文書全体が照合対象になる。空配列を渡すのとキー自体を省くのは同じ |
| `xunsearch.host` | XunSearch | 既定 `127.0.0.1:8383` |
| `xunsearch.project` | XunSearch | 既定 `default` |
| `xunsearch.ini` | XunSearch | フィールドスキーマ ini のパス。設定しないと `create_index` が `Unsupported` を返し、フィールドの vno は動的な推測に頼るしかないので、設定しておくのが望ましい |

> **タイムアウトはコードに固定**されており、現状は設定できない：HTTP 4 エンジンはリクエスト 30s・接続 10s、Meilisearch / Algolia のタスクポーリングは上限 30s（ポーリング間隔 100ms / 200ms）、XunSearch は 1 回の I/O に 5s、結果ストリーム全体に 20s。
> なお Elasticsearch の `update_bulk` は約 5MB ごとに切って複数の `_bulk` リクエストを送るため、1 回の呼び出しの総時間は 30s を超えうる —— 30s は**1 リクエスト**の上限であって、バルク操作全体の上限ではない。

> **`options` を読むドライバは 2 つだけ**：Elasticsearch（オブジェクト全体をリクエストボディにそのまま渡す。予約キー `query` / `from` / `size` の 3 つは**エラーになる**——builder の同名設定に上書きされるためで、黙って捨てると未フィルタの結果を渡すことになる。代わりに `.query()` / `.skip()` / `.take()` を使う。`sort` は例外で、`.order_by()` を併用したときだけ拒否され、単独の `option("sort", …)` はそのまま効く）と Typesense（`query_by` のみを読み、`q` が空でないときは必須）。
> 残り 6 つのドライバは `options` を**読まない**ため、何を渡しても効果はない。こうした「沈黙」を本 crate は記録せずに残さない —— インストール後に気づかせるのではなく、ここに明記しておく。

> **件数だけ、ヒットは不要**：`.take(0)` はどのドライバでも有効で、`hits` は空のまま `total` は絞り込み後の真の件数を返す。「全部で何件か」を行を取らずに問い合わせる方法がこれ。

### 予約フィールド

`__soft_deleted` はソフト削除機能（`Engine::soft_delete_in`、`SearchBuilder::with_trashed()`
/ `only_trashed()`）が使う予約フィールド名で、エンジンはこれを見てソフト削除済みの文書を除外する。
利用者の文書でこのフィールド名を業務フィールドとして使う**べきではない**。

### エラー処理

すべての操作は `crate::Result<T>` を返し、エラーは統一された `ScoutError` に収束する：

| バリアント | 発生条件 | feature |
|------|----------|---------|
| `InvalidIndexName` | 索引名に空白 / `/` / `\` / `"` / `'` / `;` / `` ` `` を含む、空である、`.` / `-` / `_` で始まる、またはワイルドカード / 複数索引の文字（`*` `?` `,` `+`）を含む（書き込み前に検証） | 組み込み |
| `InvalidHost` | ホストに認証情報が埋め込まれている（`http://user:pass@host`）；エラーにホストをそのまま出さない | 組み込み |
| `InvalidFieldName` | フィルタ / ソートのフィールド名に空白か演算子文字が含まれる；使えるのは英数字、`_`、`-`、`.` のみ | 組み込み |
| `InvalidResult` | 文書のフィールドが JSON オブジェクトでない | 組み込み |
| `Unsupported` | ドライバに必要な feature が無効、必須設定の欠落、エンジンが未対応の操作 | 組み込み |
| `Json` | serde のシリアライズ / デシリアライズエラー | 組み込み |
| `Http` | HTTP の**転送**失敗：接続不可、タイムアウト、TLS、リダイレクト拒否 | HTTP 4 エンジン |
| `Sqlx` | SQLite のエラー | `database` |
| `Backend` | バックエンドが非 2xx を返した、または自ら失敗を報告した（ES `timed_out` / シャード失敗、期限までに終端状態に達しなかったタスク）。**HTTP ステータスコードはここ** —— `429` も `400` もこのバリアントに入るので、再試行すべきか決定的かを区別するにはメッセージ本文を解析する必要がある（`Backend` は意図的にそのまま透過させる） | HTTP 4 エンジン / `xunsearch` |
| `XunSearch` / `XunSearchIo` | プロトコル解析の失敗 / TCP I/O の失敗 | `xunsearch` |

すべてのバリアントが調査ヒントを伴う。詳しくは [`ScoutError::pet_hint()`](#プロジェクトのペット)。

> **セキュリティ境界。** 認証情報を埋め込んだホスト（`http://user:pass@host`）は拒否される
> （`InvalidHost`）—— reqwest のエラー `Display` は URL 全体を連結するため、一度失敗すれば
> パスワードがログに載る。リダイレクトは**同一オリジン**（scheme・host・port がすべて同じ）
> のときだけ追従する。reqwest はホストが変わっても標準の認証ヘッダーしか外さず、
> `X-TYPESENSE-API-KEY` / `X-Algolia-API-Key` はそのまま別ホストへ送られてしまうからだ。
> よくある帰結として、リダイレクトされた POST は本文なしの GET として届くことがある（RFC 7231）。
> 書き込みパスをリダイレクトするリバースプロキシは透過的ではない。フィルタ / ソートの
> フィールド名は `validate_field_name` を通る（英数字、`_`、`-`、`.` のみ許可；`author.name` と
> 非 ASCII はそのまま使える）。また `ScoutConfig` の `Debug` は秘密を伏せ
> （`*.api_key` / `*secret*` / `*password*` / `*token` は `"<redacted>"` になる）、`Serialize` は
> そのまま出力する —— ログには `{:?}` を使い、`serde_json::to_string` は使わないこと。

## プロジェクトのペット

![プロジェクトのペット：検索ロボット Scout](svg/pet.svg)

**Scout · 検索ロボット** —— 胸に八つのモジュールスロット、挿したものを使う：開発は依存ゼロの
メモリドライバ、本番は任意のバックエンドへ、業務コードは一行も変えない。
イラスト版は [`svg/pet.svg`](svg/pet.svg)、ターミナルではこう見える：

```console
$ cargo run --example pet
```

```

                      (*)
                       |
         ______________|______________
        /                             \
        |    [o]               [o]    |
        |_____________________________|
                      | |
     _________________| |_________________
    /                                     \
    |      +-----+-----+-----+-----+      |     [##]
    |      |  ## |  ## |  ## |     |      |<- - - -'
   \|      +-----+-----+-----+-----+      |/
   o|      |  ## |  ## |  ## |  ## |      |o
    |      +-----+-----+-----+-----+      |
    \_____________________________________/
            ||                   ||
           _||_                 _||_
          (____)               (____)

  Scout · 检索机器人 · rust-scout
  八个插槽，插哪个用哪个 —— 业务代码一行不改
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

<img src="../../../docs/weixinpay.png" alt="WeChat 投げ銭" width="130"/>
<img src="../../../docs/alipay.png" alt="Alipay 投げ銭" width="130"/>

WeChat でスキャン · Alipay でスキャン

### 仮想通貨での投げ銭

| メインネット | ウォレットアドレス | QR コード |
|------|----------|--------|
| BNB Smart Chain (BEP20) | `0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../../docs/coin/1.jpg" width="130"/> |
| Tron (TRC20) | `TEdDHWLajt1XvqtPDWmQctdrJaC3pzZZzz` | <img src="../../../docs/coin/2.jpg" width="130"/> |
| Ethereum (ERC20) | `0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../../docs/coin/3.jpg" width="130"/> |
| Aptos | `0x836e3780edfc3f7b2372b39e2a1a3a5d7adfaccd96c726f21cfde1b50dd68030` | <img src="../../../docs/coin/4.jpg" width="130"/> |
| Plasma | `0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../../docs/coin/5.jpg" width="130"/> |
| Polygon POS | `0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../../docs/coin/6.jpg" width="130"/> |
| Solana | `2hfhboHdmdrYsY25XfQSsEWxq5ip4EQsR7f4AzSRMUyr` | <img src="../../../docs/coin/7.jpg" width="130"/> |
| The Open Network (TON) | `UQB9kFQohzmXUir9QSSZq01iwl9aQZIDdBpNmDklljRtCoGK` | <img src="../../../docs/coin/8.jpg" width="130"/> |
| Arbitrum One | `0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../../docs/coin/9.jpg" width="130"/> |
| AVAX C-Chain | `0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../../docs/coin/10.jpg" width="130"/> |

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
