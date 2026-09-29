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

![Hewan peliharaan proyek: Scout si Robot Pencari](svg/pet.svg)

> Hewan peliharaan proyek **Scout si Robot Pencari** — delapan slot modul di dadanya, pasang yang mana pun.
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
| 🗑️ Penghapusan lunak | `soft_delete_in(index, ids)` memberi tanda `__soft_deleted`; penyaringan tiga mode `with_trashed()` / `only_trashed()` |
| 📦 Operasi massal | `update_bulk` / `delete_bulk` mengurangi pulang-pergi; `delete_in` menghapus tepat pada indeks tertentu |
| 🔌 Driver plug-and-play | Bawaan in-memory tanpa dependensi; 8 backend masing-masing di balik feature — yang tak dipakai tidak dikompilasi |
| 🔒 Batas keamanan | Validasi nama indeks, nama field, dan host (`validate_index_name` / `validate_field_name` / `validate_host`) + pengodean persen RFC 3986 untuk mencegah injeksi path |
| 🤖 Hewan peliharaan | Scout si Robot Pencari: banner terminal + petunjuk penelusuran per kesalahan (`rust_scout::pet`) |

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
│   ├── engine.rs           # trait Engine: satu-satunya kontrak driver (6 wajib + 8 bawaan)
│   ├── manager.rs          # EngineManager: fasad, mengarahkan per driver dan menyimpan Arc<dyn Engine>
│   ├── config.rs           # ScoutConfig (9 konstruktor) + validate_index_name + percent_encode
│   │
│   ├── builder.rs          # SearchBuilder / Where / Order / TrashedFilter: kueri berantai
│   ├── document.rs         # SearchDocument: dokumen yang ditulis (kontrak serde JSON)
│   ├── result.rs           # SearchResult / SearchHit: hasil kueri
│   ├── error.rs            # ScoutError + Result<T> + pet_hint()
│   ├── pet.rs              # hewan peliharaan: Scout si Robot Pencari (banner + petunjuk kesalahan)
│   │
│   ├── collection_engine.rs    # driver in-memory (bawaan, tanpa dependensi)
│   ├── null_engine.rs          # driver kosong: buang tulisan, selalu kosong          [null]
│   ├── elasticsearch_engine.rs # ES / OpenSearch (REST)                              [elasticsearch]
│   ├── query.rs                #   pembuatan query_string dan penguraian respons
│   ├── meilisearch_engine.rs   # Meilisearch (REST)                                  [meilisearch]
│   ├── typesense_engine.rs     # Typesense (REST)                                    [typesense]
│   ├── typesense_query.rs      #   parameter pencarian dan pembuatan filter_by
│   ├── algolia_engine.rs       # Algolia (REST cloud terkelola)                      [algolia]
│   ├── database_engine.rs      # SQLite (sqlx, saring kasar LIKE + saring halus memori) [database]
│   ├── xunsearch_engine.rs     # XunSearch: protokol TCP asli xunsearchd             [xunsearch]
│   ├── xunsearch_query.rs      #   kodek paket + skema field ini
│   ├── xunsearch_tests.rs      #   uji end-to-end dengan mock server
│   │
│   └── (uji unit disisipkan di akhir tiap modul: #[cfg(test)] mod tests)
├── tests/                  # uji integrasi (masih kosong, uji ada di dalam src)
├── examples/
│   ├── collection_search.rs # cargo run --example collection_search: alur pencarian utuh yang bisa dijalankan
│   └── pet.rs              # cargo run --example pet: banner hewan + demo petunjuk kesalahan
└── docs/
    ├── svg/                # hewan peliharaan + diagram arsitektur / fitur / desain / siklus hidup
    ├── i18n/               # README dan SVG terkait untuk 12 bahasa
    ├── coin/               # kode QR donasi
    └── superpowers/specs/  # dokumen desain
```

> Label `[feature]` menandai Cargo feature yang dibutuhkan driver. Bila tidak aktif,
> `EngineManager` mengembalikan `ScoutError::Unsupported`, bukan menurunkan kemampuan diam-diam.
> String `driver` yang tak dikenal (salah ketik, spasi di akhir, atau huruf besar-kecil keliru
> seperti `OpenSearch`) juga error — dulu ia diam-diam jatuh ke driver in-memory.

### Perbedaan Kemampuan Driver

Driver in-memory bawaan adalah acuan semantik. Bila sebuah backend tidak bisa melakukan sesuatu,
ia **mengatakannya secara eksplisit** alih-alih diam-diam mengembalikan hasil yang salah:

| Driver | Keterbatasan | Perilaku |
|--------|-----------|-----------|
| Algolia | Pengurutan memerlukan indeks replika yang dibuat lebih dulu; tidak bisa dipilih per kueri | `order_by` **diabaikan** (hasil tetap dikembalikan, hanya urutannya tak tentu) |
| XunSearch | Tidak ada perintah protokol untuk `where_in` / `where_not_in` | mengembalikan `Unsupported`; gunakan `where_field` |
| XunSearch | Server hanya mendukung satu field pengurutan | beberapa `order_by` mengembalikan `Unsupported` |
| XunSearch | Soft delete belum diimplementasikan | `soft_delete` / `soft_delete_in` / `only_trashed` mengembalikan `Unsupported` |
| XunSearch | Membuat indeks memerlukan ini skema field | `create_index` mengembalikan `Unsupported` (berikan ini ke `XunSearchEngine::new`) |
| Typesense | `q` yang tidak kosong mewajibkan `query_by` | tanpa `.option("query_by", "field1,field2")` backend menjawab 400 `Parameter \`query_by\` is required`; driver tidak menebak field sendiri (tebakan yang salah akan diam-diam mengubah urutan) |
| Algolia | Penulisan berjalan sebagai task: POST hanya mengembalikan `taskID`, dan diterima ≠ dapat dicari | `update` / `update_bulk` / `delete` / `delete_in` / `delete_bulk` / `soft_delete_in` / `reindex` memantau `/1/indexes/{index}/task/{taskID}` sampai `published` sebelum kembali; tidak terbit dalam 30 detik berarti error (hasil yang tidak diketahui tidak dilaporkan sebagai sukses) |
| Meilisearch | Penulisan berjalan sebagai task: POST hanya mengembalikan `taskUid` | bentuk sama, memantau `/tasks/{uid}` sampai status akhir; task `failed` / `canceled` kini mengembalikan error `Backend` (dulu penulisan yang gagal dibuang diam-diam sebagai sukses), dan 30 detik tanpa status akhir berarti error. Penulisan massal jadi berbiaya «selama task backend berjalan» |
| database | `reindex` **memindahkan**, bukan menyalin | indeks sumber dikosongkan (`id` adalah kunci utama global, satu id tidak bisa ada di dua indeks); bila sumber harus tetap ada, jangan pakai driver database |
| XunSearch | `index: None` kini berarti indeks bernama `default` | sama seperti tujuh driver lainnya; sebelumnya ia jatuh ke basis data bawaan server xunsearchd (`db`) — data yang ditulis lewat `index: None` hanya terjangkau dengan `index("db")` |
| Jumlah bawaan | Tanpa `take`, collection / database mengembalikan **semua** hasil | Enam driver lainnya mengembalikan **10** secara bawaan (batas kebiasaan backend masing-masing) |

Ada dua penyelarasan semantik yang disengaja:

- **Saat ES menerima sintaks kueri yang cacat** (`"("`, `"foo AND"`), ia mengembalikan hasil kosong,
  bukan kesalahan —— driver in-memory melakukan pencocokan substring untuk masukan yang sama, dan
  400 akan merusak "tukar backend, kode tetap".
- **`delete` dan `soft_delete` tidak membawa informasi indeks**, jadi apakah keduanya mencakup
  beberapa indeks bergantung pada backend. Untuk menargetkan satu indeks, selalu gunakan
  `delete_in` / `soft_delete_in`.

## Mulai Cepat

### 1. Tambahkan dependensi

```toml
[dependencies]
rust-scout = "0.8"
tokio = { version = "1", features = ["macros", "rt"] }   # hanya untuk contoh
```

### 2. Contoh minimal (driver in-memory bawaan)

> Daripada salin-tempel, jalankan saja langsung: `cargo run --example collection_search` — tulis → kueri (where + pengurutan) → paginasi → hapus lunak → hapus indeks, seluruh alurnya, tanpa feature, tanpa layanan eksternal.

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
engine.soft_delete_in("books", &["book-2".to_string()]).await?;            // hapus lunak (memberi tanda; XunSearch: Unsupported)
engine.reindex("books", "books_v2").await?;                    // bangun ulang indeks
engine.delete_index("books").await?;                           // hapus indeks
```

> `delete` tidak membawa informasi indeks, jadi semantiknya berbeda antar mesin
> (driver in-memory menghapus lintas indeks, ES hanya menyentuh `default`).
> Untuk menargetkan satu indeks secara tepat, gunakan `delete_in`.
>
> Hal yang sama berlaku untuk hapus lunak: `soft_delete_in(index, ids)` adalah jalur yang andal
> pada tujuh dari delapan mesin — **XunSearch tidak mengimplementasikan `soft_delete` maupun
> `soft_delete_in`**, keduanya mengembalikan `ScoutError::Unsupported`. `soft_delete` tanpa indeks
> hanya dapat menandai lintas indeks pada backend sinkron (`collection` / `database`); backend HTTP
> tidak bisa dan mengembalikan `ScoutError::Unsupported` (bukan diam-diam tidak melakukan apa pun).
>
> Kontrak `flush` adalah "menyegarkan visibilitas tulisan", **tidak ada driver yang
> mengosongkan indeks**: ES memakai `_refresh`, XunSearch mengirim `CMD_INDEX_COMMIT`, dan
> driver lainnya membuat tulisan langsung terlihat sehingga menjadi no-op. Untuk mengosongkan
> indeks, gunakan `delete_index`.

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

> Pada mesin SQLite (`database`), `total` adalah jumlah hit **setelah penyaringan** (sama dengan
> `CollectionEngine`): SQL hanya menjalankan indeks + saring kasar LIKE untuk mengambil kandidat,
> sedangkan wheres / hapus lunak / pengurutan / paginasi semuanya di memori. Paginasi tidak bisa
> diturunkan ke `LIMIT/OFFSET` SQL — baris yang cocok di luar jendela akan selamanya tak terjangkau.

### Kunci Konfigurasi Driver

Kunci di `ScoutConfig::insert` (atau yang ditulis langsung ke `options`) dibaca `EngineManager` saat membangun driver:

| Kunci | Driver | Keterangan |
|---------|--------------|---------|
| `elasticsearch.host` / `opensearch.host` | Elasticsearch / OpenSearch | bawaan `http://127.0.0.1:9200` |
| `elasticsearch.api_key` / `opensearch.api_key` | Elasticsearch / OpenSearch | opsional |
| `meilisearch.host` / `meilisearch.api_key` | Meilisearch | host bawaan `http://127.0.0.1:7700` |
| `typesense.host` / `typesense.api_key` | Typesense | host bawaan `http://127.0.0.1:8108` |
| `algolia.app_id` / `algolia.api_key` | Algolia | keduanya wajib |
| `database.url` | SQLite | wajib |
| `database.fields` | SQLite | **tidak lagi ikut serta dalam pencarian**, disimpan hanya demi kompatibilitas (dan dijadwalkan dihapus pada rilis perusak berikutnya). Dulu ia membatasi pencarian teks pada field yang didaftarkan, sehingga `database` berbeda dari basis in-memory (`collection`) untuk masukan yang sama (`fields=["title"]` + dokumen `{"title":"Rust","tag":"async"}` + kueri `"async"` → database 0 hit, collection 1); kini seluruh dokumen ikut dicocokkan. Array kosong dan menghilangkan kunci ini berperilaku sama |
| `xunsearch.host` | XunSearch | bawaan `127.0.0.1:8383` |
| `xunsearch.project` | XunSearch | bawaan `default` |
| `xunsearch.ini` | XunSearch | jalur ini skema field. Tanpanya `create_index` mengembalikan `Unsupported` dan vno field hanya bisa ditebak secara dinamis, jadi sebaiknya diisi |

> **Timeout ditulis mati di kode**, untuk sekarang tidak bisa dikonfigurasi: empat mesin HTTP 30s per permintaan dan 10s untuk koneksi; polling tugas Meilisearch / Algolia dibatasi 30s (selang polling 100ms / 200ms); XunSearch 5s per I/O dan 20s untuk seluruh aliran hasil.
> Perhatikan: `update_bulk` Elasticsearch memotong di sekitar 5MB menjadi beberapa permintaan `_bulk`, jadi satu panggilan bisa memakan waktu lebih dari 30s — angka 30s adalah batas **per permintaan**, bukan batas seluruh operasi bulk.

> **`options` hanya dibaca dua driver**: Elasticsearch (seluruh objek diteruskan ke body permintaan; tiga kunci cadangan `query` / `from` / `size` **menghasilkan error** — kunci itu akan ditimpa oleh setelan builder yang bernama sama, dan membuangnya diam-diam akan memberi Anda hasil tanpa penyaringan; pakai `.query()` / `.skip()` / `.take()` sebagai gantinya. `sort` adalah pengecualian: hanya ditolak bila Anda juga memakai `.order_by()`, jadi `option("sort", …)` tunggal tetap berlaku apa adanya) dan Typesense (hanya `query_by`, yang wajib selama `q` tidak kosong).
> Enam driver lainnya **tidak membaca** `options`; apa pun yang dikirim ke sana tidak akan berpengaruh. Justru kesenyapan inilah yang selalu ditolak crate ini untuk dibiarkan tanpa catatan — karena itu ditulis tegas di sini, bukan supaya Anda baru sadar setelah memasangnya.

> **Hanya jumlah, tanpa hit**: `.take(0)` sah di semua driver — `hits` kembali kosong tetapi `total` tetap jumlah sebenarnya setelah penyaringan, jadi inilah cara bertanya «berapa yang cocok» tanpa mengambil barisnya.

### Field yang Dicadangkan

`__soft_deleted` adalah nama field yang dicadangkan untuk fitur hapus lunak
(`Engine::soft_delete_in`, `SearchBuilder::with_trashed()` / `only_trashed()`); mesin memakainya
untuk menyaring dokumen yang dihapus lunak. Dokumen pengguna **tidak boleh** memakai nama field
ini sebagai field bisnis.

### Penanganan Kesalahan

Semua operasi mengembalikan `crate::Result<T>`, dengan kesalahan menyatu ke `ScoutError` tunggal:

| Varian | Pemicu | feature |
|---------|--------------|---------|
| `InvalidIndexName` | nama indeks memuat spasi / `/` / `\` / `"` / `'` / `;` / `` ` ``, kosong, diawali `.` / `-` / `_`, atau memuat karakter wildcard / multi-indeks (`*` `?` `,` `+`) — divalidasi sebelum menulis | bawaan |
| `InvalidHost` | host memuat kredensial (`http://user:pass@host`); host tidak diulang di pesan error | bawaan |
| `InvalidFieldName` | field filter atau urut memuat spasi atau karakter operator; hanya huruf, angka, `_`, `-`, dan `.` yang diizinkan | bawaan |
| `InvalidResult` | field dokumen bukan objek JSON | bawaan |
| `Unsupported` | feature driver tidak aktif, konfigurasi wajib kurang, atau mesin tidak mendukung operasi itu | bawaan |
| `Json` | kesalahan serialisasi / deserialisasi serde | bawaan |
| `Http` | kegagalan **transmisi** HTTP: tidak bisa terhubung, timeout, TLS, pengalihan ditolak | empat mesin HTTP |
| `Sqlx` | kesalahan SQLite | `database` |
| `Backend` | backend mengembalikan respons non-2xx, atau melaporkan kegagalannya sendiri (ES `timed_out` / shard gagal, tugas yang tak pernah mencapai keadaan terminal). **Kode status HTTP ada di sini** — baik `429` maupun `400` masuk ke varian ini, jadi membedakan yang layak dicoba ulang dari yang final berarti harus membaca teks pesannya (`Backend` sengaja meneruskannya apa adanya) | empat mesin HTTP / `xunsearch` |
| `XunSearch` / `XunSearchIo` | gagal mengurai protokol / gagal I/O TCP | `xunsearch` |

Setiap varian membawa petunjuk penelusuran — lihat [`ScoutError::pet_hint()`](#hewan-peliharaan-proyek).

> **Batas keamanan.** Host berisi kredensial (`http://user:pass@host`) ditolak (`InvalidHost`) —
> `Display` error reqwest menambahkan URL lengkap, jadi satu kegagalan saja sudah membawa kata
> sandi ke log. Pengalihan hanya diikuti **same-origin** (scheme, host, dan port sama), karena
> reqwest hanya melepas header autentikasi standar saat host berganti dan `X-TYPESENSE-API-KEY` /
> `X-Algolia-API-Key` akan ikut ke host asing. Akibat lazimnya: POST yang dialihkan bisa tiba
> sebagai GET tanpa body (RFC 7231), jadi reverse proxy yang mengalihkan jalur tulis tidak
> transparan. Nama field filter dan urut melewati `validate_field_name` (hanya huruf, angka, `_`,
> `-`, `.`; `author.name` dan nama non-ASCII tetap boleh). Dan `Debug` pada `ScoutConfig`
> menyamarkan rahasia (`*.api_key` / `*secret*` / `*password*` / `*token` menjadi `"<redacted>"`),
> sedangkan `Serialize` tetap menuliskannya apa adanya — untuk log gunakan `{:?}`, jangan
> `serde_json::to_string`.

## Hewan Peliharaan Proyek

![Hewan peliharaan proyek: Scout si Robot Pencari](svg/pet.svg)

**Scout · Robot Pencari** — delapan slot modul di dadanya, pasang yang mana pun: kembangkan
dengan driver in-memory tanpa dependensi, tukar ke backend apa pun di produksi, kode bisnis
tak berubah.
Versi gambarnya di [`svg/pet.svg`](svg/pet.svg); di terminal ia tampak seperti ini:

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

<img src="../../../docs/weixinpay.png" alt="Donasi via WeChat" width="130"/>
<img src="../../../docs/alipay.png" alt="Donasi via Alipay" width="130"/>

Pindai dengan WeChat · Pindai dengan Alipay

### Donasi Kripto

| Jaringan | Alamat Dompet | Kode QR |
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
