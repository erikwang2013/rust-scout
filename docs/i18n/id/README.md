# rust-scout

[![crates.io](https://img.shields.io/crates/v/rust-scout.svg)](https://crates.io/crates/rust-scout)
[![docs.rs](https://img.shields.io/docsrs/rust-scout)](https://docs.rs/rust-scout)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../../LICENSE)

[简体中文](../../../README.md) · [English](../en/README.md) · [日本語](../ja/README.md) · [한국어](../ko/README.md) · Bahasa Indonesia · [Русский](../ru/README.md) · [Deutsch](../de/README.md) · [Français](../fr/README.md) · [Español](../es/README.md) · [Português](../pt/README.md) · [हिन्दी](../hi/README.md) · [العربية](../ar/README.md) · [বাংলা](../bn/README.md)

**Abstraksi pustaka pencarian teks lengkap rust-scout** — lapisan antarmuka pencarian teks
lengkap yang ringan untuk Rust. Mengadopsi model mental kueri berantai dari
[Laravel Scout](https://laravel.com/docs/scout), pustaka ini mengabstraksi **8 backend**
(in-memory, Elasticsearch/OpenSearch, Meilisearch, Typesense, Algolia, SQLite, XunSearch,
Null) melalui satu trait `Engine`: **driver in-memory tanpa dependensi untuk pengembangan,
beralih mulus ke backend apa pun di produksi, tanpa mengubah satu baris pun kode bisnis.**

![Hewan peliharaan proyek: Scout si Search Hound](svg/pet.svg)

> Hewan peliharaan proyek **Scout si Search Hound** — mengendus dokumen, melacak indeks.
> Ia bukan hanya ada di dokumentasi: ia hadir di banner terminal dan di petunjuk kesalahan,
> lihat [Hewan Peliharaan Proyek](#hewan-peliharaan-proyek).

```rust
let result = engine.search(
    SearchBuilder::new("rust 异步")
        .within("articles")
        .where_field("status", "published")
        .order_by("created_at", true)
        .take(10),
).await?;
```

## Fitur

| Kemampuan | Keterangan |
|------|------|
| 🔍 Pencarian teks lengkap | Driver in-memory mencocokkan substring; driver HTTP memakai sintaks asli backend (ES: `query_string`, `field:value`) |
| ⚙️ Kueri berantai | `SearchBuilder`: query / within / where_field / where_in / where_not_in / order_by / take / skip / option |
| 🎯 Pemfilteran tepat | Pencocokan kesamaan (ES → `term`), pencocokan himpunan (ES → `terms` / `must_not`) |
| 📄 Pengurutan multi-field | asc / desc dapat ditumpuk, urutan pasti saat membandingkan tipe JSON berbeda |
| 📃 Paginasi | Pemotongan offset `take`/`skip` + paginasi nomor halaman `paginate(page, per_page)` |
| 🗂️ Banyak indeks | Perutean lewat field `index` tingkat dokumen, indeks bawaan `"default"` |
| 🔄 Siklus hidup indeks | Alur lengkap `create_index` / `flush` / `reindex` / `delete_index` |
| 🗑️ Penghapusan lunak | `soft_delete` memberi tanda `__soft_deleted`; penyaringan tiga mode `with_trashed()` / `only_trashed()` |
| 📦 Operasi massal | `update_bulk` / `delete_bulk` mengurangi pulang-pergi; `delete_in` menghapus tepat pada indeks tertentu |
| 🔌 Driver plug-and-play | Bawaan in-memory tanpa dependensi; 8 backend masing-masing di balik feature — yang tak dipakai tidak dikompilasi |
| 🔒 Batas keamanan | Validasi nama indeks (`validate_index_name`) + pengodean persen RFC 3986 untuk mencegah injeksi path |
| 🐕 Hewan peliharaan | Scout si Search Hound: banner terminal + petunjuk penelusuran per kesalahan (`rust_scout::pet`) |

## Arsitektur

![Arsitektur](svg/architecture.svg)

Lima lapisan: aplikasi → kontrak data (serde JSON) → inti (`EngineManager` + trait `Engine`)
→ driver (menurut cara transmisi, empat kelompok, total 8 driver) → penyimpanan. Satu-satunya
sambungan antar lapisan adalah trait `Engine`.

## Desain Fitur

![Fitur](svg/features.svg)

12 kemampuan: kueri berantai, teks lengkap, filter tepat/himpunan, pengurutan, paginasi,
banyak indeks, penghapusan lunak, siklus hidup indeks, penghapusan massal dan tepat,
driver plug-and-play, batas keamanan.

## Filosofi Desain

![Desain](svg/design.svg)

## Siklus Hidup

![Siklus hidup](svg/lifecycle.svg)

Tujuh tahap: buat → tulis → flush → cari → hapus dokumen → indeks ulang → musnahkan.
Bagian bawah diagram membandingkan perilaku empat keluarga driver di setiap tahap.

## Struktur Proyek

```
rust-scout/
├── Cargo.toml              # dependensi dan feature (default = [], tanpa dependensi)
├── src/
│   ├── lib.rs              # akar crate: ekspor modul + re-ekspor tipe publik yang dibatasi feature
│   │
│   ├── engine.rs           # trait Engine: satu-satunya kontrak driver (8 wajib + 5 bawaan)
│   ├── manager.rs          # EngineManager: fasad, mengarahkan per driver dan menyimpan Arc<dyn Engine>
│   ├── config.rs           # ScoutConfig (8 konstruktor) + validate_index_name + percent_encode
│   │
│   ├── builder.rs          # SearchBuilder / Where / Order / TrashedFilter: kueri berantai
│   ├── document.rs         # SearchDocument: dokumen yang ditulis (kontrak serde JSON)
│   ├── result.rs           # SearchResult / SearchHit: hasil kueri
│   ├── searchable.rs       # Searchable / SearchableStore: jembatan ke model bisnis
│   ├── error.rs            # ScoutError + Result<T> + pet_hint()
│   ├── pet.rs              # hewan peliharaan: Scout si Search Hound (banner + petunjuk kesalahan)
│   │
│   ├── collection_engine.rs    # driver in-memory (bawaan, tanpa dependensi)
│   ├── null_engine.rs          # driver kosong: buang tulisan, selalu kosong          [null]
│   ├── elasticsearch_engine.rs # ES / OpenSearch (REST)                              [elasticsearch]
│   │   └── query.rs            #   pembuatan query_string dan penguraian respons
│   ├── meilisearch_engine.rs   # Meilisearch (REST)                                  [meilisearch]
│   ├── typesense_engine.rs     # Typesense (REST)                                    [typesense]
│   │   └── typesense_query.rs  #   parameter pencarian dan pembuatan filter_by
│   ├── algolia_engine.rs       # Algolia (REST cloud terkelola)                      [algolia]
│   ├── database_engine.rs      # SQLite (sqlx, saring kasar LIKE + saring halus memori) [database]
│   ├── xunsearch_engine.rs     # XunSearch: protokol TCP asli xunsearchd             [xunsearch]
│   │   ├── xunsearch_query.rs  #   kodek paket + skema field ini
│   │   └── xunsearch_tests.rs  #   uji end-to-end dengan mock server
│   │
│   └── (uji unit disisipkan di akhir tiap modul: #[cfg(test)] mod tests)
├── tests/                  # uji integrasi (masih kosong, uji ada di dalam src)
├── examples/
│   └── pet.rs              # cargo run --example pet: banner hewan + demo petunjuk kesalahan
└── docs/
    ├── svg/                # hewan peliharaan + diagram arsitektur / fitur / desain / siklus hidup
    ├── i18n/               # README dan SVG terkait untuk 12 bahasa
    ├── coin/               # kode QR donasi
    └── superpowers/specs/  # dokumen desain
```

> Label `[feature]` menandai Cargo feature yang dibutuhkan driver. Bila tidak aktif,
> `EngineManager` mengembalikan `ScoutError::Unsupported`, bukan menurunkan kemampuan diam-diam.

## Mulai Cepat

### 1. Tambahkan dependensi

```toml
[dependencies]
rust-scout = "0.5"
tokio = { version = "1", features = ["macros", "rt"] }   # hanya untuk contoh
```

### 2. Contoh minimal (driver in-memory bawaan)

```rust
use rust_scout::{Engine, EngineManager, ScoutConfig, SearchBuilder, SearchDocument};

#[tokio::main]
async fn main() -> rust_scout::Result<()> {
    // driver bawaan: CollectionEngine in-memory, tanpa dependensi
    let engine = EngineManager::new(ScoutConfig::collection()).engine()?;

    // tulis dokumen
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

    // kueri
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

## Penggunaan

### Membangun Kueri (SearchBuilder)

Semua operasi kueri dirangkai berantai lalu diserahkan ke `engine.search(&builder)`:

```rust
let builder = SearchBuilder::new("kata kunci")  // teks lengkap (opsional, kosong = semua)
    .within("articles")                          // indeks tujuan (opsional, bawaan "default")
    .where_field("status", "published")          // filter kesamaan
    .where_in("tags", ["rust", "async"])         // himpunan IN
    .where_not_in("category", ["draft"])         // himpunan NOT IN
    .order_by("created_at", true)                // urut multi-field (true = desc)
    .order_by("title", false)
    .take(20)                                    // jumlah per halaman
    .skip(40)                                    // offset
    .option("highlight", true)                   // opsi diteruskan khusus driver
    .with_trashed();                             // tiga mode hapus lunak: sembunyikan / sertakan / hanya itu
```

> `query` mendukung sintaks Lucene `query_string` (berlaku penuh di driver ES):
> `"rust"`, `"title:rust AND tags:async"`, `"rust~2"` (fuzzy). Driver lain memakai
> sintaks aslinya atau pencocokan substring.

### Paginasi

```rust
// Cara 1: pemotongan offset
let page2 = SearchBuilder::new("rust").within("books").skip(10).take(10);
// Cara 2: per nomor halaman (page mulai dari 1)
let page2 = engine.paginate(&SearchBuilder::new("rust").within("books"), 2, 10).await?;
```

### Banyak Indeks dan Siklus Hidup

```rust
engine.create_index("books", serde_json::json!({})).await?;    // buat indeks
engine.update(&docs).await?;                                   // tulis dokumen
engine.update_bulk(&docs).await?;                              // tulis massal (endpoint bulk asli bila didukung)
engine.flush("books").await?;                                  // segarkan visibilitas
engine.search(&builder).await?;                                // kueri
engine.delete_in("books", &["book-1".to_string()]).await?;     // hapus dokumen dari satu indeks
engine.soft_delete(&["book-2".to_string()]).await?;            // hapus lunak (memberi tanda)
engine.reindex("books", "books_v2").await?;                    // bangun ulang indeks
engine.delete_index("books").await?;                           // hapus indeks
```

> `delete` tidak membawa informasi indeks, jadi semantiknya berbeda antar mesin
> (driver in-memory menghapus lintas indeks, ES hanya menyentuh `default`).
> Untuk menargetkan satu indeks secara tepat, gunakan `delete_in`.

### Beralih ke Elasticsearch / OpenSearch

```bash
cargo add rust-scout --features elasticsearch
```

```rust
use rust_scout::{Engine, EngineManager, ScoutConfig};

let config = ScoutConfig::elasticsearch(
    "http://127.0.0.1:9200",      // atau alamat OpenSearch
    Some("your-api-key".into()),  // opsional: autentikasi ApiKey
);
let engine = EngineManager::new(config).engine()?;
// —— selanjutnya semua operasi sama persis dengan driver in-memory ——
```

| Item | CollectionEngine (bawaan) | ElasticsearchEngine |
|--------|--------------------------|---------------------|
| Dependensi | hanya serde / thiserror | reqwest (saat feature aktif) |
| Teks lengkap | Pencocokan substring hasil serialisasi | `query_string` |
| Pemfilteran | matches() di memori | term / terms / must_not |
| Pengurutan | sort_hits() di memori | array sort |
| flush | no-op | `_refresh` |
| Paginasi bawaan | Semua hasil | size 10 |
| Urutan bawaan | Berdasarkan id | Berdasarkan _score |

### Beralih ke Meilisearch

```bash
cargo add rust-scout --features meilisearch
```

```rust
use rust_scout::{Engine, EngineManager, ScoutConfig};

let config = ScoutConfig::meilisearch(
    "http://127.0.0.1:7700",   // alamat layanan Meilisearch
    "your-master-key",          // opsional: kunci API
);
let engine = EngineManager::new(config).engine()?;
// —— selanjutnya semua operasi sama persis dengan driver in-memory ——
```

### Perbandingan Mesin

| Mesin | driver | feature | Transmisi | Status |
|------|--------|---------|-----------|--------|
| In-memory (bawaan) | `collection` | bawaan | Dalam proses | Lengkap |
| Elasticsearch / OpenSearch | `elasticsearch` / `opensearch` | `elasticsearch` | HTTP REST | Lengkap |
| Meilisearch | `meilisearch` | `meilisearch` | HTTP REST | Lengkap |
| Typesense | `typesense` | `typesense` | HTTP REST | Lengkap |
| Algolia | `algolia` | `algolia` | HTTP REST | Lengkap |
| SQLite | `database` | `database` | Berkas lokal | Lengkap |
| XunSearch | `xunsearch` | `xunsearch` | TCP asli | Lengkap |
| Null (pengujian / menonaktifkan pencarian) | `null` | `null` | — | Lengkap |

Konstruktor konfigurasi mesin lainnya ada di [docs.rs](https://docs.rs/rust-scout): `ScoutConfig::typesense(host, api_key)`, `ScoutConfig::algolia(app_id, api_key)`, `ScoutConfig::database(url, fields)`, `ScoutConfig::null()`, `ScoutConfig::xunsearch(host, project)`.

> Pada mesin SQLite (`database`), `total` dihitung di lapisan SQL (indeks + saring kasar LIKE);
> setelah penyaringan wheres / hapus lunak di memori bisa terjadi `hits.len() < total`,
> dan paginasi mengacu pada hits.

### Field yang Dicadangkan

`__soft_deleted` adalah nama field yang dicadangkan untuk fitur hapus lunak
(`Engine::soft_delete`, `SearchBuilder::with_trashed()` / `only_trashed()`); mesin memakainya
untuk menyaring dokumen yang dihapus lunak. Dokumen pengguna **tidak boleh** memakai nama field
ini sebagai field bisnis.

### Penanganan Kesalahan

Semua operasi mengembalikan `crate::Result<T>`, dengan kesalahan menyatu ke `ScoutError` tunggal:

| Varian | Pemicu | feature |
|---------|--------------|---------|
| `InvalidIndexName` | nama indeks memuat spasi / `/` / `\`, diawali `.`, atau kosong (divalidasi sebelum menulis) | bawaan |
| `InvalidResult` | field dokumen bukan objek JSON | bawaan |
| `Unsupported` | feature driver tidak aktif, konfigurasi wajib kurang, atau mesin tidak mendukung operasi itu | bawaan |
| `Json` | kesalahan serialisasi / deserialisasi serde | bawaan |
| `Http` | permintaan HTTP gagal (koneksi, timeout, kode status) | empat mesin HTTP |
| `Sqlx` | kesalahan SQLite | `database` |
| `Backend` | backend mengembalikan respons kesalahan, pesan asli diteruskan apa adanya | empat mesin HTTP / `xunsearch` |
| `XunSearch` / `XunSearchIo` | gagal mengurai protokol / gagal I/O TCP | `xunsearch` |

Setiap varian membawa petunjuk penelusuran — lihat [`ScoutError::pet_hint()`](#hewan-peliharaan-proyek).

### Menjembatani Model Bisnis (Searchable)

Implementasikan `Searchable` untuk memetakan struktur bisnis menjadi dokumen yang dapat
diindeks, dan `SearchableStore` untuk membungkus tiga operasi `index_documents` /
`remove_documents` / `search`:

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

## Hewan Peliharaan Proyek

![Hewan peliharaan proyek: Scout si Search Hound](svg/pet.svg)

**Scout · si Search Hound** — mengendus dokumen, melacak indeks: di mana ada kueri, di situ ada dia.
Versi gambarnya di [`svg/pet.svg`](svg/pet.svg); di terminal ia tampak seperti ini:

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

Hewan ini tinggal di modul [`rust_scout::pet`](../../../src/pet.rs) dan **tidak menambah dependensi apa pun**:

| Item | Keterangan |
|----|------|
| `pet::NAME` / `pet::SPECIES` / `pet::TAGLINE` | info papan nama |
| `pet::ART` | potret ASCII (sengaja hanya ASCII 7-bit, agar tidak miring di terminal CJK) |
| `pet::banner()` | banner terminal; teks polos tanpa escape sequence, aman ditulis ke log |
| `pet::hint(&err)` | petunjuk penelusuran per kesalahan, mengembalikan `&'static str` |
| `pet::format_error(&err)` | kesalahan asli + petunjuk, dirender untuk manusia |
| `ScoutError::pet_hint()` | petunjuk yang sama, menempel langsung pada tipe kesalahan |

```rust
use rust_scout::{pet, ScoutError};

println!("{}", pet::banner());

let err = ScoutError::Unsupported("feature tidak ada".into());
eprintln!("{}", pet::format_error(&err));
// error: unsupported operation: feature tidak ada
//
//   [o_o] Scout：这个后端我还没找到路 —— Cargo.toml 里对应的 feature 启用了吗？
```

> **Mengapa petunjuknya tidak langsung dimasukkan ke `Display`?** `Display` pada `ScoutError`
> tetap satu baris dan dapat dibaca mesin — perambatan `?`, pengumpulan log, dan grep string
> kesalahan di CI bergantung padanya. Untuk keluaran yang mudah dibaca manusia beserta petunjuk
> hewan peliharaan, panggil `pet::format_error()`.

## Dukungan dan Donasi

Jika proyek ini bermanfaat bagi Anda, dukunglah dengan donasi ☕ — dukungan Anda adalah dorongan untuk terus merawatnya!

### WeChat / Alipay

<img src="../../../docs/weixinpay.png" alt="Donasi via WeChat" width="130" height="130"/>
<img src="../../../docs/alipay.png" alt="Donasi via Alipay" width="130" height="130"/>

Pindai dengan WeChat · Pindai dengan Alipay

### Donasi Kripto

| Jaringan | Alamat Dompet | Kode QR |
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

### Transfer Global (Transfer Bank)

**Informasi Penerima**

- Nama penerima: WANG KEXUN
- Nomor rekening penerima: 881015918251

**Bank Penerima (ZA Bank)**

- SWIFT Code: `AABLHKHHXXX`
- Nama bank: ZA Bank Limited
- Kode bank: 387
- Alamat bank: Core F, Cyberport 3, 100 Cyberport Road, Hong Kong

> Informasi bank koresponden (bank perantara) di bawah ini adalah untuk transfer lintas negara, bukan informasi bank penerima. Silakan tanyakan kepada bank pengirim apakah perlu disertakan.

- Bank koresponden untuk transfer dalam HKD, CNY, dan USD adalah **Citibank**:
  - Nama bank: Citibank N.A. Hong Kong
  - SWIFT Code: `CITIHKHXXXX`
  - Kode bank: 006 / Kode cabang: 391
  - Nama cabang: Hong Kong Branch
  - Alamat bank: Citibank Tower, Citibank Plaza, 3 Garden Road, Central, Hong Kong
- Bank koresponden untuk transfer dalam mata uang lain adalah **BNY Mellon**:
  - Nama bank: THE BANK OF NEW YORK MELLON
  - SWIFT Code: `IRVTUS3NXXX`
  - Alamat bank: THE BANK OF NEW YORK MELLON, 240 GREENWICH STREET, NEW YORK, United States

## Lisensi

MIT License. Lihat [LICENSE](../../../LICENSE) untuk detailnya.
