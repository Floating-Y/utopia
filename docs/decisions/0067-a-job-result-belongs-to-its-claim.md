# 0067 · A job result belongs to its claim

- **Status**: Proposed · 2026-10-09 · [PR #1130](https://github.com/deeplethe/utopia/pull/1130) · open: ADR review and merge before the schema and worker implementation; verification on the current `dev` baseline
- **Written**: 2026-10-09 (conventions in the [README](README.md))
- **Related**: [0051](0051-a-human-phrase-decision-carries-its-materialization-work.md) (durable materialization delivery); [#1106](https://github.com/deeplethe/utopia/issues/1106), including the [maintainer's reply](https://github.com/deeplethe/utopia/issues/1106#issuecomment-6073749092)

## Problem

The worker claims a queued job, runs its handler, then writes the resulting state.
If that final write fails, it currently logs the error and releases the worker slot.
The row stays `running`, while the scheduler claims only `queued` rows. A transient
database failure therefore requires a server restart to recover completed work, and
startup recovery may run the handler and its side effects again.

The discussion in #1106 accepts retrying the result already computed, with backoff,
without rerunning the handler or spending another business attempt. That result must
also belong to the execution that produced it: an old worker must not overwrite a
later claim of the same job.

## Decision proposed

1. **Give each claim a generation independent of its attempt budget.** Add
   `jobs.claim_generation BIGINT NOT NULL DEFAULT 0`. `claim_one` increments it in
   the same update that changes the job to `running`, and returns it with the job.
   Deferred and manual requeue keep this generation; only a new claim increments it.
   Every outcome update performed by the queue worker requires the job ID,
   `status = 'running'`, and the returned generation. Existing rows start at zero;
   startup recovery retains its current role when a server starts with unfinished jobs.

2. **Freeze the handler's outcome and completion time once.** Retain success or the
   complete formatted error, its Terminal/Deferred classification, and the existing
   business retry decision. Derive `run_at` and the first `deferred_since` from that
   completion time on every write attempt, so a database outage cannot keep moving
   the retry or waiting window forward. Success still clears `last_error`; Terminal
   still wins over Deferred; business failure budgets and the bounded deferral
   window keep their existing meanings.

3. **Retry persistence within the original worker task.** Wait 1, 2, 4, 8, 16, then
   30 seconds between failed writes, keeping the cap at 30 seconds. These are writes
   of one outcome, not handler attempts. Retain the original concurrency slot until
   the write succeeds or the claim no longer owns a running row. Release database
   connections before sleeping. Pool closure ends local retry and leaves unfinished
   work for the existing startup recovery.

4. **Treat an already settled or replaced claim as complete for this worker.** A
   guarded update affecting no rows must not rerun the handler or overwrite another
   execution. This also handles a commit whose acknowledgement was lost: the retry
   sees a settled row and stops. A Deferred update can also affect no rows because
   its waiting window expired; its ordinary failure fallback must use the same claim
   guard so a replaced claim cannot spend a later execution's budget.

The implementation should stay in the existing queue, with its existing handler
signature and HTTP `JobStatus` shape. Adding a field to the public Rust `Job` struct
is a source compatibility change: all struct constructors, test fixtures, and
synthetic claim queries must supply the generation. Migration `0107` is the proposed
next number after `0106` on the checked `main` and `dev`; check both branches again
before implementation and submission to avoid a collision.

## Why this and not the alternatives

- **`attempts` as the claim identity.** Deferred returns an attempt and manual requeue
  resets the budget. Different executions can therefore have the same attempt count.
  A separate monotonically increasing generation does not reuse that identity.
- **`locked_at` as the claim identity.** A timestamp serves diagnostics, but uniqueness
  is not its contract. The queue can compare a generation directly without relying
  on clock precision or on two claims having different timestamps.
- **Requeue or rerun the handler after a failed result write.** The handler already
  produced its outcome and may have committed side effects. Repeating it replaces a
  persistence failure with duplicate business work and consumes the wrong budget.
- **Give result writes a finite retry budget.** Exhausting that budget recreates the
  stranded `running` row. Keep capped backoff while the process and pool remain live.
- **Release the slot and create a separate retry queue.** A prolonged outage could
  accumulate unbounded pending outcomes. Keeping the original slot bounds retained
  work by worker concurrency and reuses the current task lifecycle.
- **Persist outcomes separately or add leases.** Either would introduce another
  durable delivery or ownership protocol. This proposal repairs result persistence
  within the current single-process queue; crash durability and multi-instance
  recovery need a separate decision.

## Limits

Pending outcomes remain in process memory. A process exit can still lead startup
recovery to rerun a handler; this is not exactly-once execution or safe multi-instance
ownership. Persistent database failure retains worker slots and can stop new claims.
There is no connection held during the backoff, but progress still depends on a
writable database. A handler that has already settled its own job remains compatible:
the guarded worker acknowledgement sees no running row and stops, including RSS
`complete_hydration` committing `done` before it returns. The generation fences the
worker's outcome writes, not handler business side effects across different claims;
this is not a new ownership protocol for RSS or other handlers.

The current `dev` worker and schema do not implement this proposal.
Under the contribution rules, this ADR must land before the data-model implementation.

## Verification required for implementation

Use a dedicated, otherwise idle database and the existing `test_db::url()` guard with
`UTOPIA_TEST_REQUIRE_DB=1`. Fault injection and real queue workers must not share a
database with unrelated tests.

- Fail outcome writes, restore writes, and verify success, ordinary failure,
  Terminal, and Deferred outcomes. The handler runs once; the original error,
  attempt budget, completion-based schedule, and deferral window remain correct.
- Simulate a committed write whose acknowledgement failed. A retry must not change
  attempts or `run_at`, repeat side effects, or replace the settled outcome.
- Replace a claim before its old success or failure writes back, including equal
  attempt counts. The old generation must not update the newly running job.
- Observe a concurrency slot retained during persistence failure and released after
  recovery or stale-claim detection; cover a handler that already marks its job done
  and pool closure while persistence is waiting.
- Update all `Job` constructors and synthetic claim queries, then run the required
  workspace formatting, Clippy, tests, SQL-backed regressions, and web build against
  the implementing head. Record only checks actually executed on that head.
