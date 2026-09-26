# rust-scout

[![crates.io](https://img.shields.io/crates/v/rust-scout.svg)](https://crates.io/crates/rust-scout)
[![docs.rs](https://img.shields.io/docsrs/rust-scout)](https://docs.rs/rust-scout)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../../LICENSE)

[简体中文](../../../README.md) · [English](../en/README.md) · [日本語](../ja/README.md) · [한국어](../ko/README.md) · [Bahasa Indonesia](../id/README.md) · [Русский](../ru/README.md) · [Deutsch](../de/README.md) · [Français](../fr/README.md) · [Español](../es/README.md) · Português · [हिन्दी](../hi/README.md) · [العربية](../ar/README.md) · [বাংলা](../bn/README.md)

**rust-scout: abstração de biblioteca de pesquisa em texto integral** — uma camada leve de interface de pesquisa em texto integral para Rust. Inspirada no modelo mental de consultas encadeadas do [Laravel Scout](https://laravel.com/docs/scout), abstrai **8 backends** (memória, Elasticsearch/OpenSearch, Meilisearch, Typesense, Algolia, SQLite, XunSearch, Null) através de um `Engine` trait unificado: **driver em memória sem dependências para desenvolvimento, troca transparente para qualquer backend em produção, sem alterar uma linha do código de negócio.**

![Mascote do projeto: Scout, o Cão de Busca](svg/pet.svg)

> Mascote do projeto: **Scout, o Cão de Busca** (Search Hound) — fareja documentos, segue o rasto dos índices.
> Vive na documentação *e* no código: faixa de terminal e sugestões de erro.
> Ver [Mascote do Projeto](#mascote-do-projeto).

```rust
let result = engine.search(
    SearchBuilder::new("rust async")
        .within("articles")
        .where_field("status", "published")
        .order_by("created_at", true)
        .take(10),
).await?;
```

## Funcionalidades

| Capacidade | Descrição |
|------|------|
| 🔍 Pesquisa em texto integral | Driver em memória faz correspondência por substring; drivers HTTP usam a sintaxe nativa do backend (ES: `query_string`, `campo:valor`) |
| ⚙️ Consultas encadeadas | `SearchBuilder`: query / within / where_field / where_in / where_not_in / order_by / take / skip / option |
| 🎯 Filtragem exata | Igualdade (ES → `term`), conjuntos (ES → `terms` / `must_not`) |
| 📄 Ordenação por vários campos | asc / desc acumuláveis, ordem determinística entre tipos JSON |
| 📃 Paginação | Corte por offset `take`/`skip` + paginação por página `paginate(page, per_page)` |
| 🗂️ Vários índices | Encaminhamento pelo campo `index` de cada documento, índice padrão `"default"` |
| 🔄 Ciclo de vida do índice | Fluxo completo `create_index` / `flush` / `reindex` / `delete_index` |
| 🗑️ Eliminação lógica | `soft_delete_in(index, ids)` marca `__soft_deleted`; filtro tri-estado `with_trashed()` / `only_trashed()` |
| 📦 Operações em lote | `update_bulk` / `delete_bulk` reduzem idas e voltas; `delete_in` aponta a um índice exato |
| 🔌 Drivers plugáveis | Em memória por padrão, zero dependências; 8 backends com feature própria — o que não se usa não compila |
| 🔒 Limite de segurança | Validação do nome do índice (`validate_index_name`) + codificação percentual RFC 3986 contra injeção de caminho |
| 🐕 Mascote do projeto | Scout, o Cão de Busca: faixa de terminal + sugestões de diagnóstico erro a erro (`rust_scout::pet`) |

## Arquitetura

![Arquitetura](svg/architecture.svg)

Cinco camadas: aplicação → contrato de dados (serde JSON) → núcleo (`EngineManager` + `Engine` trait)
→ drivers (agrupados por transporte, 8 no total) → armazenamento. O `Engine` trait é a única costura entre camadas.

## Design de Funcionalidades

![Funcionalidades](svg/features.svg)

12 capacidades: consultas encadeadas, texto integral, filtragem exata/por conjuntos, ordenação,
paginação, vários índices, eliminação lógica, ciclo de vida do índice, eliminação em lote e
direcionada, drivers plugáveis, limite de segurança.

## Filosofia de Design

![Design](svg/design.svg)

## Ciclo de Vida

![Ciclo de vida](svg/lifecycle.svg)

Sete fases: criar → escrever → flush → pesquisar → eliminar documentos → reindexar → destruir.
A metade inferior compara o comportamento das quatro famílias de drivers em cada fase.

## Estrutura do Projeto

```
rust-scout/
├── Cargo.toml              # dependências e features (default = [], zero dependências)
├── src/
│   ├── lib.rs              # raiz do crate: módulos + re-exports protegidos por feature
│   │
│   ├── engine.rs           # Engine trait: o único contrato de driver (8 obrigatórios + 5 com default)
│   ├── manager.rs          # EngineManager: fachada, distribui por driver e cacheia Arc<dyn Engine>
│   ├── config.rs           # ScoutConfig (8 construtores) + validate_index_name + percent_encode
│   │
│   ├── builder.rs          # SearchBuilder / Where / Order / TrashedFilter: consultas encadeadas
│   ├── document.rs         # SearchDocument: o documento escrito (contrato serde JSON)
│   ├── result.rs           # SearchResult / SearchHit: resultados da consulta
│   ├── searchable.rs       # Searchable / SearchableStore: ponte para modelos de negócio
│   ├── error.rs            # ScoutError + Result<T> + pet_hint()
│   ├── pet.rs              # Mascote do projeto: Scout, o Cão de Busca (faixa + sugestões)
│   │
│   ├── collection_engine.rs    # driver em memória (padrão, zero dependências)
│   ├── null_engine.rs          # driver nulo: descarta escritas, sempre vazio       [null]
│   ├── elasticsearch_engine.rs # ES / OpenSearch (REST)                             [elasticsearch]
│   │   └── query.rs            #   construção de query_string e leitura da resposta
│   ├── meilisearch_engine.rs   # Meilisearch (REST)                                 [meilisearch]
│   ├── typesense_engine.rs     # Typesense (REST)                                   [typesense]
│   │   └── typesense_query.rs  #   parâmetros de pesquisa e construção do filter_by
│   ├── algolia_engine.rs       # Algolia (REST de nuvem gerida)                     [algolia]
│   ├── database_engine.rs      # SQLite (sqlx, pré-filtro LIKE + refino em memória) [database]
│   ├── xunsearch_engine.rs     # XunSearch: protocolo TCP nativo do xunsearchd      [xunsearch]
│   │   ├── xunsearch_query.rs  #   codec de pacotes + esquema de campos ini
│   │   └── xunsearch_tests.rs  #   testes ponta a ponta contra um servidor mock
│   │
│   └── (testes unitários inline em cada módulo, em #[cfg(test)] mod tests)
├── tests/                  # testes de integração (vazios por agora; os testes vivem em src)
├── examples/
│   └── pet.rs              # cargo run --example pet: faixa da mascote + demo das sugestões
└── docs/
    ├── svg/                # mascote + diagramas de arquitetura / funcionalidades / design / ciclo
    ├── i18n/               # READMEs e SVGs correspondentes, em 12 línguas
    ├── coin/               # códigos QR de doação
    └── superpowers/specs/  # documentos de design
```

> A etiqueta `[feature]` indica a feature de Cargo de que o driver precisa. Quando está
> desligada, o `EngineManager` devolve `ScoutError::Unsupported` em vez de degradar em silêncio.

### Diferenças de capacidades entre drivers

O driver em memória padrão é a referência semântica. Quando um backend não consegue
fazer algo, ele **diz isso explicitamente** em vez de devolver em silêncio resultados errados:

| Driver | Limitação | Comportamento |
|--------|-----------|----------------|
| Algolia | A ordenação exige índices réplica pré-construídos; não dá para escolher por consulta | `order_by` é **ignorado** (os resultados até vêm, só a ordem fica indefinida) |
| XunSearch | Sem comando de protocolo para `where_in` / `where_not_in` | devolve `Unsupported`; use `where_field` |
| XunSearch | O servidor aceita apenas um campo de ordenação | vários `order_by` devolvem `Unsupported` |
| XunSearch | Eliminação lógica não implementada | `soft_delete` / `only_trashed` devolvem `Unsupported` |
| XunSearch | Criar um índice exige um ini de esquema de campos | `create_index` devolve `Unsupported` (passe um ini a `XunSearchEngine::new`) |

Dois alinhamentos semânticos deliberados:

- **Quando o ES recebe sintaxe de consulta malformada** (`"("`, `"foo AND"`), devolve um
  resultado vazio em vez de erro — o driver em memória faz correspondência por substring
  com a mesma entrada, e um 400 quebraria o «troque de backend, mantenha o seu código».
- **`delete` e `soft_delete` não trazem informação de índice**, por isso abrangerem ou não
  vários índices depende do backend. Para apontar a um só índice, use sempre
  `delete_in` / `soft_delete_in`.

## Início Rápido

### 1. Adicionar a dependência

```toml
[dependencies]
rust-scout = "0.6"
tokio = { version = "1", features = ["macros", "rt"] }   # apenas para o exemplo
```

### 2. Exemplo mínimo (driver em memória, o padrão)

```rust
use rust_scout::{Engine, EngineManager, ScoutConfig, SearchBuilder, SearchDocument};

#[tokio::main]
async fn main() -> rust_scout::Result<()> {
    // driver padrão: CollectionEngine em memória, zero dependências
    let engine = EngineManager::new(ScoutConfig::collection()).engine()?;

    // escrever um documento
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

    // consultar
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

## Utilização

### Construção de Consultas (SearchBuilder)

Todas as operações de consulta são encadeadas e no fim entregues a `engine.search(&builder)`:

```rust
let builder = SearchBuilder::new("texto integral")  // pesquisa integral (opcional, vazio = todos)
    .within("articles")                          // índice alvo (opcional, padrão "default")
    .where_field("status", "published")          // filtro de igualdade
    .where_in("tags", ["rust", "async"])         // conjunto IN
    .where_not_in("category", ["draft"])         // conjunto NOT IN
    .order_by("created_at", true)                // ordenação multi-campo (true = desc)
    .order_by("title", false)
    .take(20)                                    // tamanho da página
    .skip(40)                                    // deslocamento
    .option("highlight", true)                   // opções repassadas ao driver
    .with_trashed();                             // tri-estado: excluir / incluir / só eliminados
```

> `query` suporta a sintaxe Lucene `query_string` (totalmente efetiva no driver ES):
> `"rust"`, `"title:rust AND tags:async"`, `"rust~2"` (difusa). Os restantes drivers usam a sua
> sintaxe nativa ou correspondência por substring.

### Paginação

```rust
// Opção 1: corte por offset
let page2 = SearchBuilder::new("rust").within("books").skip(10).take(10);
// Opção 2: por número de página (a página começa em 1)
let page2 = engine.paginate(&SearchBuilder::new("rust").within("books"), 2, 10).await?;
```

### Vários Índices e Ciclo de Vida

```rust
engine.create_index("books", serde_json::json!({})).await?;    // criar índice
engine.update(&docs).await?;                                   // escrever documentos
engine.update_bulk(&docs).await?;                              // escrita em lote (endpoint nativo, se houver)
engine.flush("books").await?;                                  // atualizar visibilidade
engine.search(&builder).await?;                                // consultar
engine.delete_in("books", &["book-1".to_string()]).await?;     // eliminar docs de um índice
engine.soft_delete_in("books", &["book-2".to_string()]).await?;            // eliminação lógica (marca o doc)
engine.reindex("books", "books_v2").await?;                    // reconstruir um índice
engine.delete_index("books").await?;                           // apagar o índice
```

> O `delete` não transporta informação de índice, por isso a semântica varia com o engine (o
> driver em memória elimina em todos os índices; o ES só toca no `default`). Para apontar a um
> índice exato use `delete_in`.
>
> O mesmo se aplica à eliminação lógica: **`soft_delete_in(index, ids)` é a variante
> fiável, seja qual for o engine**. O `soft_delete` sem índice só consegue marcar em todos
> os índices nos drivers síncronos (`collection` / `database`); os drivers HTTP não
> conseguem e devolvem `ScoutError::Unsupported` (em vez de não fazer nada em silêncio).
>
> O contrato de `flush` é atualizar a visibilidade das escritas: **nenhum driver esvazia
> um índice** — o ES passa por `_refresh`, nos restantes drivers as escritas ficam
> imediatamente visíveis, logo é um no-op. Para esvaziar um índice use `delete_index`.

### Mudar para Elasticsearch / OpenSearch

```bash
cargo add rust-scout --features elasticsearch
```

```rust
use rust_scout::{Engine, EngineManager, ScoutConfig};

let config = ScoutConfig::elasticsearch(
    "http://127.0.0.1:9200",      // ou o endereço do OpenSearch
    Some("your-api-key".into()),   // opcional: autenticação ApiKey
);
let engine = EngineManager::new(config).engine()?;
// —— daqui em diante tudo é idêntico ao driver em memória ——
```

| Item | CollectionEngine (padrão) | ElasticsearchEngine |
|--------|--------------------------|---------------------|
| Dependências | só serde / thiserror | reqwest (com feature) |
| Texto integral | Correspondência por substring serializada | `query_string` |
| Filtragem | matches() em memória | term / terms / must_not |
| Ordenação | sort_hits() em memória | array sort |
| flush | no-op | `_refresh` |
| Paginação padrão | Todos os resultados | size 10 |
| Ordenação padrão | Por id | Por _score |

### Mudar para Meilisearch

```bash
cargo add rust-scout --features meilisearch
```

```rust
use rust_scout::{Engine, EngineManager, ScoutConfig};

let config = ScoutConfig::meilisearch(
    "http://127.0.0.1:7700",   // endereço do Meilisearch
    "your-master-key",          // opcional: chave de API
);
let engine = EngineManager::new(config).engine()?;
// —— daqui em diante tudo é idêntico ao driver em memória ——
```

### Comparação de Engines

| Engine | driver | feature | Transporte | Estado |
|------|--------|---------|-----------|--------|
| Em memória (padrão) | `collection` | integrada | No processo | Completo |
| Elasticsearch / OpenSearch | `elasticsearch` / `opensearch` | `elasticsearch` | HTTP REST | Completo |
| Meilisearch | `meilisearch` | `meilisearch` | HTTP REST | Completo |
| Typesense | `typesense` | `typesense` | HTTP REST | Completo |
| Algolia | `algolia` | `algolia` | HTTP REST | Completo |
| SQLite | `database` | `database` | Ficheiro local | Completo |
| XunSearch | `xunsearch` | `xunsearch` | TCP nativo | Completo |
| Null (testes / desligar pesquisa) | `null` | `null` | — | Completo |

Os construtores de configuração dos restantes engines estão em [docs.rs](https://docs.rs/rust-scout): `ScoutConfig::typesense(host, api_key)`, `ScoutConfig::algolia(app_id, api_key)`, `ScoutConfig::database(url, fields)`, `ScoutConfig::null()`, `ScoutConfig::xunsearch(host, project)`.

> No engine SQLite (`database`), o `total` é contado ao nível do SQL (índice + pré-filtro
> LIKE); os wheres / eliminações lógicas podem fazer `hits.len() < total` depois da filtragem
> em memória, e a paginação guia-se pelos hits.

### Campos Reservados

`__soft_deleted` é o nome de campo reservado usado pela eliminação lógica (`Engine::soft_delete_in`,
`SearchBuilder::with_trashed()` / `only_trashed()`), e é por ele que os engines filtram os
documentos eliminados. Os documentos do utilizador **não devem** usar este nome como campo de negócio.

### Tratamento de Erros

Todas as operações devolvem `crate::Result<T>`, com os erros a convergir no `ScoutError` unificado:

| Variante | Despoletado por | feature |
|------|----------|---------|
| `InvalidIndexName` | nome do índice com espaço / `/` / `\`, começado por `.`, ou vazio (validado antes de escrever) | integrada |
| `InvalidResult` | campo do documento que não é um objeto JSON | integrada |
| `Unsupported` | feature do driver desligada, configuração obrigatória em falta, ou engine que não suporta a operação | integrada |
| `Json` | erro de serialização / desserialização serde | integrada |
| `Http` | pedido HTTP falhado (ligação, tempo limite, estado) | quatro drivers HTTP |
| `Sqlx` | erro do SQLite | `database` |
| `Backend` | o backend devolveu uma resposta de erro; mensagem original repassada | quatro drivers HTTP / `xunsearch` |
| `XunSearch` / `XunSearchIo` | falha ao interpretar o protocolo / falha de I/O TCP | `xunsearch` |

Cada variante traz uma sugestão de diagnóstico — ver [`ScoutError::pet_hint()`](#mascote-do-projeto).

### Ligar Modelos de Negócio (Searchable)

Implemente `Searchable` para mapear uma estrutura de negócio num documento indexável, e
`SearchableStore` para encapsular as três operações `index_documents` / `remove_documents` / `search`:

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

## Mascote do Projeto

![Mascote do projeto: Scout, o Cão de Busca](svg/pet.svg)

**Scout · o Cão de Busca** (Search Hound) — fareja documentos, segue o rasto dos índices.
Onde há uma consulta, ele já lá está. A versão gráfica está em [`svg/pet.svg`](svg/pet.svg);
no terminal é assim:

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

A mascote vive no módulo [`rust_scout::pet`](../../../src/pet.rs) e **não introduz qualquer dependência**:

| Item | Descrição |
|------|-------------|
| `pet::NAME` / `pet::SPECIES` / `pet::TAGLINE` | informação da placa de identificação |
| `pet::ART` | o retrato ASCII (deliberadamente só 7 bits, para nunca ficar torto em terminais CJK) |
| `pet::banner()` | faixa de terminal; texto simples, sem sequências de escape, seguro para registos |
| `pet::hint(&err)` | sugestão de diagnóstico por erro, devolve `&'static str` |
| `pet::format_error(&err)` | erro original + sugestão, compostos para leitura humana |
| `ScoutError::pet_hint()` | a mesma sugestão, pendurada no próprio tipo de erro |

```rust
use rust_scout::{pet, ScoutError};

println!("{}", pet::banner());

let err = ScoutError::Unsupported("feature em falta".into());
eprintln!("{}", pet::format_error(&err));
// error: unsupported operation: feature em falta
//
//   [o_o] Scout：这个后端我还没找到路 —— Cargo.toml 里对应的 feature 启用了吗？
```

> **Porque é que a sugestão não vai direto para o `Display`?** O `Display` do `ScoutError`
> mantém-se numa linha e legível por máquina — a propagação com `?`, a recolha de registos e
> o grep de erros em CI dependem disso. Para saída legível por humanos, com a sugestão,
> chame `pet::format_error()`.

## Apoio e Doações

Se este projeto lhe for útil, considere apoiá-lo com uma doação ☕ — o seu apoio é o que mantém a manutenção a andar!

### WeChat / Alipay

<img src="../../../docs/weixinpay.png" alt="Doação por WeChat" width="130" height="130"/>
<img src="../../../docs/alipay.png" alt="Doação por Alipay" width="130" height="130"/>

Ler o QR com o WeChat · Ler o QR com o Alipay

### Doações em Criptomoeda

| Rede | Endereço da carteira | Código QR |
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

### Transferências Internacionais (Bancárias)

**Informação do beneficiário**

- Nome do beneficiário: WANG KEXUN
- Número da conta: 881015918251

**Banco recetor (ZA Bank)**

- SWIFT Code: `AABLHKHHXXX`
- Nome do banco: ZA Bank Limited
- Código do banco: 387
- Endereço do banco: Core F, Cyberport 3, 100 Cyberport Road, Hong Kong

> A informação abaixo é do banco correspondente (banco intermediário) para transferências
> transfronteiriças, não do banco recetor. Confirme com o banco emissor se é necessária.

- O banco correspondente para remessas em dólares de Hong Kong, renminbi e dólares dos Estados Unidos é o **Citibank**:
  - Nome do banco: Citibank N.A. Hong Kong
  - SWIFT Code: `CITIHKHXXXX`
  - Código do banco / agência: 006 / 391
  - Nome da agência: Hong Kong Branch
  - Endereço do banco: Citibank Tower, Citibank Plaza, 3 Garden Road, Central, Hong Kong
- O banco correspondente para remessas noutras moedas é o **BNY Mellon**:
  - Nome do banco: THE BANK OF NEW YORK MELLON
  - SWIFT Code: `IRVTUS3NXXX`
  - Endereço do banco: THE BANK OF NEW YORK MELLON, 240 GREENWICH STREET, NEW YORK, United States

## Licença

Licença MIT. Ver [LICENSE](../../../LICENSE) para mais detalhes.
