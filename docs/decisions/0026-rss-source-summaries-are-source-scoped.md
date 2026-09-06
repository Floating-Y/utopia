# 0026 · RSS summaries are scoped to the source being listed

- **Status**: implemented in the #417 implementation branch (`0ddc8b9`)
- **Written**: 2026-09-06 (conventions in the [README](README.md))
- **Related**: [0023](0023-rss-observations-are-not-documents.md) established RSS observations as a separate responsibility from documents; #417 changes the public `SourceView` contract while fixing the scope of its RSS summary.

> A Library page lists ten sources. The database holds several thousand RSS observations, most belonging to other sources or older activations. The list query should count the rows belonging to each source and its current generation, not project the whole RSS observation table before it starts returning sources.

## What the source list does today

`sources::list` wraps the complete `ENTRY_SELECT` projection in one global CTE and then runs a separate correlated count over that projection for every RSS summary field. `ENTRY_SELECT` is also the canonical place where observation and job state becomes `pending`, `queued`, `hydrating`, `retry_wait`, `complete`, `terminal`, `deleted` or `superseded`.

The result is correct in its state classification, but the global CTE makes every Library load a candidate to project all RSS observations in the deployment before the source-specific counts are applied. The projection also exposes implementation state — `generation` and `baseline_count` — as part of the public source-list response.

## Decisions

### 1. Aggregate only the source and generation being listed

The source list will use a `LEFT JOIN LATERAL` aggregate per source. The aggregate is parameterized by the outer source's `id` and current `rss_generation`, and its filters are pushed into `rss_full_content_entries` through the existing source/generation index. The `s.kind = 'rss'` predicate appears both inside the lateral subquery and on the join. The inner predicate gives PostgreSQL a one-time false/filter opportunity for non-RSS rows; the join predicate preserves the fact that a non-RSS source has no RSS summary.

The aggregate counts `pending`, `queued`, `retrying`, `complete` and `terminal` from the canonical `ENTRY_SELECT` projection. It does not duplicate the projection's state `CASE` in `sources.rs`. `queued` remains the union of `queued` and `hydrating`; `terminal` remains the union of `terminal`, `deleted` and `superseded`. Only rows whose `activation_generation` equals the source's current `rss_generation` are counted.

The query continues to preserve source ordering, document and missing counts, credential removal, the `SOURCE_SECRET_KEYS` bind, and `config - $2::text[]`. Observation, job and document responsibilities remain unchanged, as does `rss_full_content::counts()` and the diagnostic list.

### 2. Make the public summary an explicit nested type

`SourceView` will replace its flat RSS fields with a nullable `rss_full_content` object:

```json
{
  "rss_full_content": {
    "state": "active",
    "pending": 1,
    "queued": 2,
    "retrying": 3,
    "complete": 4,
    "terminal": 5
  }
}
```

The Rust API will expose a strongly typed `RssFullContentSummary`, not an arbitrary JSON value. A non-RSS source returns `"rss_full_content": null`. An RSS source always returns an object, including when full-content hydration is disabled. `queued` and `terminal` retain the state unions described above. `generation` and `baseline_count` remain internal state and are removed from the source-list API.

The store will deserialize a private flat `SourceListRow`, then explicitly convert it to `SourceView`. Missing SQL fields or a row that cannot form the required RSS object are storage errors; the conversion will not use `unwrap`, `expect` or silent defaults to hide them.

The TypeScript contract and Library consumer will use the nested object and first check it for null. No compatibility double-write of the old fields is planned. No UI redesign, new component, visual-style change or new copy is part of this decision.

### 3. Do not add schema or runtime dependencies

This change adds no migration and no new Rust or npm dependency. It changes the read query, the API model, the TypeScript contract and their tests only. SQL values continue to use binds; any dynamic SQL is assembled only from repository-owned constants.

## Alternatives rejected

- **Keep the global CTE.** It is simple to reuse, but its work grows with the entire deployment rather than with the source being listed. A larger observation table makes every Library load more expensive.
- **Run one query per count.** Five round trips and repeated scans add latency and work for the same source summary.
- **Fetch every entry and count in Rust.** It transfers and retains data that the endpoint only needs as five numbers, making network and memory costs unacceptable.
- **Add a cache table for the summary.** A cache introduces consistency and invalidation problems. The current read path is sufficient once its scope is source- and generation-bound.

## Performance evidence required by the implementation

The implementation PR will include before/after `EXPLAIN (ANALYZE, BUFFERS, VERBOSE)` results from a dedicated PostgreSQL database with one knowledge base, at least one full-content RSS source, several thousand current-generation observations, and observations for other sources or knowledge bases. The comparison will record actual entry scan rows, use of the `source_id`/`activation_generation` index, lateral loops, shared buffer hits/reads, execution time, and whether non-RSS sources take a one-time false/filter path without scanning entries. Temporary SQL, plans and generated data will stay out of the repository.

## Open questions

- **A transition period for the flat fields.** Not included. The nested object is the public contract approved by this record; a compatibility period would need a separate API decision.
- **A summary cache.** Deferred until measured source-scoped aggregation is insufficient for a real workload; the invalidation boundary would need to be designed with it.
