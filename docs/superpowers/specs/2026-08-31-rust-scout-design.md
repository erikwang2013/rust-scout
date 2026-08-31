# rust-scout design

Date: 2026-08-31

## Context

`erikwang2013/webman-scout` is a Laravel Scout-style full-text search package for PHP hosts: it maps searchable models to search engines, exposes a `Searchable` API, queues or synchronously syncs model changes, and routes searches through an engine manager plus query builder. The source also supports OpenSearch/Elasticsearch-oriented advanced queries such as filters, ranges, aggregations, facets, geo, highlight, and vector-style params.

This crate ports the business shape into Rust, not the PHP framework wiring. Target delivery is a reusable Cargo crate, not a Webman plugin layout.

## Goals

- Provide a Scout-like Rust API for searchable entities: serialize to JSON documents, sync/index/remove, and search through an engine.
- Support the minimum useful engines now:
  - `Collection` engine for in-memory/test workflows.
  - `Elasticsearch/OpenSearch` REST-compatible engine via `reqwest`.
- Preserve the Scout concepts that matter: default driver selection, index prefix, chunk size, soft-delete metadata, builder filters, pagination, raw results, model remapping.
- Keep the dependency footprint small and the API async-friendly for real search backends.

## Non-goals for v1

- Do not implement PHP framework bridges, Eloquent observers, Webman Redis Queue, or Symfony commands.
- Do not port Algolia, Meilisearch, Typesense, XunSearch, DB, and all advanced OpenSearch operators exhaustively.
- Do not provide ORM-specific model integration. Rust callers pass documents or implement a small `Searchable` trait.

## Proposed package shape

```text
Cargo.toml
src/lib.rs
src/config.rs
src/error.rs
src/document.rs
src/builder.rs
src/engine.rs
src/manager.rs
src/searchable.rs
src/collection_engine.rs
src/elasticsearch_engine.rs
src/lib.rs
tests/scout_test.rs
examples/collection_search.rs
```

## Core types

- `ScoutConfig`: driver, prefix, queue flag (configuration only for now), after_commit flag, chunk sizes, soft_delete, identify, backend host/key settings.
- `SearchDocument`: id plus JSON fields; implements `serde::Serialize`/`Deserialize`.
- `SearchResult`: raw hits, total, optional aggregations/facets, engine-specific metadata.
- `Hit`: id, score, source JSON, optional highlight.
- `Engine` async trait:
  - `update(&[SearchDocument])`
  - `delete(&[String])`
  - `search(&SearchBuilder) -> SearchResult`
  - `paginate(&SearchBuilder, page, per_page) -> SearchResult`
  - `map_ids(&SearchResult) -> Vec<String>`
  - `flush(index)`
  - `create_index(index, settings)`
  - `delete_index(index)`
- `SearchBuilder`: query, index, where/where_in/where_not_in, take/skip/offset, orders, options, after_raw callback hook if cheap; advanced clauses may be stored as `serde_json::Value` for engine pass-through.
- `EngineManager`: owns config and lazy cached engines by driver name; custom driver registration stays optional.

## Searchable flow

Rust has no Eloquent model observer, so v1 uses an explicit `Searchable` trait or helper functions:

- `Searchable::to_searchable_json(self) -> serde_json::Value`
- `Searchable::searchable_id(self) -> String`
- `Scout::searchable(&engine, docs)` indexes documents.
- `Scout::unsearchable(&engine, ids)` removes documents.
- `Scout::search::<T>(&engine, T::IndexName)` returns ids or raw hits; callers hydrate domain models themselves.

This keeps Rust ownership and lifetimes clean while preserving Scout's separation between serialization and engine execution.

## Collection engine behavior

`CollectionEngine` stores documents in a mutex-backed in-memory map. `update` upserts, `delete` removes ids, `flush` clears an index, `search` filters scalar JSON fields and performs case-insensitive substring search over the document JSON. It supports basic `where`, `where_in`, `where_not_in`, `take`, `skip`, and orders by JSON field values when present. It does not attempt full Elasticsearch relevance; tests should assert deterministic id/source ordering.

## Elasticsearch/OpenSearch engine behavior

Use one REST-compatible engine behind a feature flag named `elasticsearch` to avoid pulling `reqwest` for collection-only use. Request mapping:

- index: `{prefix}{index_name}`
- update: bulk index operations when multiple docs are present, single index for one doc.
- delete: bulk delete by id.
- search: `bool` query with `multi_match` for text query, `term`/`terms` filters for where clauses, `must_not` for not-in, `from`/`size` for pagination.
- create/delete index: REST `_index` endpoints.

This is intentionally a practical Scout bridge, not a complete Elasticsearch query DSL. Users needing full geo/vector/aggregation/facet behavior can pass raw query fragments through `options` or add a later feature.

## Error handling

Use a small `ScoutError` enum wrapping config errors, backend request errors, serialization errors, unsupported operations, and invalid index names. Validate index names at the system boundary: non-empty, no whitespace, no `/`, and no leading dot by default.

## Testing

Unit tests use only the collection engine and cover:

- config defaults and index name validation.
- builder where/where_in/where_not_in/take/skip/order behavior.
- update/delete/search round trip.
- result id mapping and total count.
- soft-delete metadata flag serialization.

Elasticsearch/OpenSearch tests are compile-only or mocked behind feature gates unless a live cluster is available. One live integration test can be added later, but v1 must pass `cargo test` without external services.

## Success criteria

- `cargo fmt`, `cargo test`, and `cargo build --features elasticsearch` pass.
- A sample or test demonstrates searchable docs and search result ordering.
- API remains engine-agnostic: adding a new engine should not require changing `SearchBuilder` callers.
