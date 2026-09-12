# 0039 · An upload may carry a document's date

- **Status**: Proposed; not implemented.
- **Written**: 2026-09-12 (conventions in the [README](README.md)).
- **Related**: [#610](https://github.com/deeplethe/utopia/issues/610), [0022](0022-an-unknown-date-is-not-an-open-one.md). This proposal does not change extraction or fact validity.

## Why a decision is needed

Dated announcements and filings uploaded as files currently receive upload time, so their evidence anchors say when they entered the system rather than when they were written. JSON ingest already accepts a document date. The multipart endpoint ignores non-file fields and processes each file immediately; accepting a request-wide date also requires deciding when files may become visible to workers. ADR 0022 explains how document dates anchor evidence; this record proposes the upload contract that supplies them.

## Decisions proposed for acceptance

### 1. One optional field for newly created documents

POST /kbs/{id}/documents accepts one optional text field named `doc_time`. When present, its value applies to every newly created document in that request, whether the field precedes, follows, or appears between files. Different dates require separate requests in this cut.

Use the same chrono parsing semantics as JSON ingest's `DateTime<Utc>`. With the current dependency, parsing the raw field as `DateTime<Utc>` follows the same fixed-offset parser and UTC conversion as serde; `parse_from_rfc3339` would be stricter and is not an exact substitute. Do not add preprocessing or a separate date parser. Recommended client spelling is `2024-03-01T08:00:00+08:00`, stored as the instant `2024-03-01T00:00:00Z`. Date-only and zone-less timestamps are rejected. Fractional seconds retain the existing database precision, without inventing a new precision model.

Omission is the only way to request the current default. Empty strings, whitespace-only text, invalid dates and the literal `null` are not omission. A second field is rejected even if both values are equal. A field named `doc_time` carrying a filename is rejected as an invalid field type; it cannot bypass validation by being treated as a document. Other non-file fields retain the current ignored behavior.

Proposed new errors use the existing `AppError::invalid` / `ApiErr` response shape and HTTP 422:

| Input | Code | Message |
|---|---|---|
| Invalid value or field type | `invalid_doc_time` | `doc_time must be a text timestamp with a timezone` |
| Second occurrence | `duplicate_doc_time` | `doc_time must appear at most once` |

Reject at the first observed error; a request with multiple independent errors does not promise error aggregation. Existing malformed-upload and read-failure error mappings remain unchanged.

### 2. Finish validation before publishing documents

Keep permission and target-folder checks before upload processing. Read the complete multipart stream through the existing 100 MiB total-body limit, temporarily retaining file bytes and metadata. Do not write blobs, insert or restore documents, or enqueue jobs until parsing and date validation finish successfully. Then use the existing per-file blob/create/enqueue flow in file order.

The route already applies `DefaultBodyLimit::max(MAX_UPLOAD_BYTES)`, and the current axum multipart extractor consumes `with_limited_body()`. The limit applies to the whole body, including fields and multipart framing, rather than independently granting each file 100 MiB. Regression coverage must exercise the real route and cross the limit cumulatively with several files, including when Content-Length is absent. Do not disable that limiter or collect an unrestricted raw body.

The request limit bounds retained file payload, not exact heap usage: metadata, parser allocations and concurrent requests add overhead. This is a deliberate ceiling for this cut. A future increase to the limit or measured memory pressure would justify temporary-file spooling; neither a new storage layer nor a new dependency is needed now.

This changes one observable failure boundary: a late parse, date-validation or size-limit failure leaves no upload-created blobs, documents, restoration changes or jobs. Previously earlier files could already have been processed. Maintainers must accept this change explicitly. It does not promise a transaction for the whole upload: once validated, a later blob/database/enqueue failure still returns the existing error and can leave earlier files committed. An enqueue failure may leave its document created, as today. Do not introduce rollback or blob cleanup as part of this feature.

### 3. Persist the timestamp and its provenance together

| Entry and outcome | Date | Source |
|---|---|---|
| New multipart document, field present | Parsed instant | `upload` |
| New multipart document, field omitted | Existing database `now()` fallback | `upload_time` |
| JSON ingest, explicit date | Existing supplied instant | `source` |
| Source synchronization | Existing creation/update rules | Existing provenance rules |
| Live duplicate or restored duplicate | Existing document date | Existing document source |

Keep `documents::create` and its existing callers compatible. Add the smallest upload-specific entry needed to select provenance, sharing the existing insertion/restoration implementation instead of copying SQL or changing every caller. Date and source are written in the same insertion statement and committed before queueing. Do not insert with a temporary date or source and patch the row afterward. No database migration is needed.

Without an explicit date, each newly inserted row keeps its own upload-time default; this is not a promise that all default timestamps are identical. With an explicit date, all new documents share that instant. Duplicate/restored documents are deliberately excluded from this assignment.

The meaning also stays separate from a fact's `valid_from` / `valid_to`: a document's date can anchor evidence under ADR 0022; it is not a statement that every fact began or ended on that date. `doc_time_source` records how the date was obtained, not how trustworthy the document is.

### 4. Retransmission does not correct history

A live duplicate in the same knowledge base is skipped without changing its date or enqueueing new work, including repeated content within the request. Identical bytes in a different knowledge base remain independent.

A soft-deleted duplicate retains its stable identity, date and provenance when restored. Preserve the current upload behavior of enqueueing `process_document` after restoration; do not add an extra date-triggered extraction or change the existing processing policy. Purged-content handling remains unchanged.

Keep the existing success response (`created` and `skipped`), empty-file reporting, no-files error, knowledge-base access checks and folder validation. A date-only request still has no files; an empty file remains a skipped file. An invalid date rejects even a request whose files would all have been skipped.

## Alternatives and why they are deferred

| Alternative | Reason not chosen for this cut |
|---|---|
| Require the date before files | Makes field order part of the contract; the proposed request-wide field must work in any position. |
| First or last repeated value wins | Hides client errors and makes intent ambiguous. |
| Process files immediately, update dates later | A worker can consume the wrong date; earlier facts are not repaired by changing a document row. |
| Write each blob early and retain only metadata | Reduces multi-file payload memory, but a late invalid date can leave unreferenced blobs; deleting by hash can remove content reused by other requests. Bounded buffering gives simpler rejection behavior under the existing limit. |
| Temporary files or a new staging subsystem | Adds cleanup and failure paths before measured memory demand requires them. |
| Overwrite dates on duplicate or restore | Silently rewrites evidence history and raises a separate re-extraction policy question. |

Revision, 2026-09-12: the first local sketch left the buffering choice open. Review of axum's aggregate body limit supports bounded buffering as the smallest proposal with no persistent effects before validation. Early blob writes remain a viable alternative if maintainers prefer lower multi-file memory usage and explicitly accept orphaned blobs on rejection.

## Questions for the issue discussion

1. Accept the request-wide, single-field contract and rejection rules above?
2. Accept JSON ingest timestamp compatibility rather than adding a separate date-only convention?
3. Accept bounded buffering under the existing 100 MiB aggregate limit and no persistent upload effects on a late validation failure, while preserving partial success after validation when storage or queueing fails?
4. Confirm duplicate/restoration dates remain unchanged and existing restoration processing stays as-is?

## Acceptance after the record lands

| Regression group | Required observation |
|---|---|
| Defaults and parsing | Omitted date retains `upload_time`; valid and offset timestamps yield the correct UTC instant with `upload`; compare accepted syntax with JSON ingest's parser. |
| Invalid fields | Invalid, empty, whitespace-only, `null`, date-only, zone-less, equal/unequal repeated values and a filename-bearing date field reject with the defined error shape. |
| Ordering and late failure | Date first/middle/last applies equally to multiple new files; a later malformed part, bad date, duplicate field or cumulative size excess leaves no new blobs, documents, restoration changes or jobs. |
| Identity | Existing and within-request duplicates keep dates and add no duplicate jobs; restore keeps the date/source and existing processing behavior; cross-KB same content and purged paths remain compatible. |
| Permissions and shape | Authentication, editor permission, cross-KB/non-folder targets, empty files and no-files responses remain intact. |
| Queue visibility and other ingress | Claim an uploaded document's processing job and read its correct date/source through an independent DB connection; JSON ingest and source synchronization retain their provenance and update behavior. |

Use existing server test facilities (including the patterns in `api/jobs_routes_tests.rs`) and store tests. Database tests must obtain their URL through `test_db::url()` and actually run in a dedicated isolated PostgreSQL/pgvector database with `UTOPIA_TEST_REQUIRE_DB=1`. The visibility assertion must observe committed state from another connection, not only inspect the HTTP response.

Run CONTRIBUTING's fmt, workspace clippy, workspace tests, and frozen-lockfile web build. Report actual failures and blocked checks separately; a skipped database test is not a pass.

## Out of scope

Content/title date detection, per-file JSON sidecars, frontend date controls, historical backfill, schema migrations, extraction algorithm changes, and #592.
