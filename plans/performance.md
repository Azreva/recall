# Recall performance engineering

**Status:** Planned performance architecture for a non-production prototype. Testing requirements are described in [validation and development](docs/validation.md).
**Relationship:** Keep the [architecture](plans/architecture.md) as the semantic and recovery reference. This document extends its execution and storage mechanisms while preserving correctness guarantees. The [roadmap](ROADMAP.md) tracks implementation stages and acceptance gates.

## 1. Engineering goals

**Target:** Small-key mixed workloads, with high successful throughput at a controlled tail-latency and memory budget. Measure memory-only, periodic, and strict durability separately. Linux is the sole Tier 1 development and deployment target; production qualification is future work.

Combine owner-local state, independent scheduling, efficient data movement, and bounded background work. Optimize the complete request path, including admission, queueing, execution, response encoding, and storage synchronization.

| Engineering goal | Mechanism to test | Acceptance evidence |
| --- | --- | --- |
| Multicore efficiency | Owner-local mutable state; move-owned requests; bounded batching; no global read-path lock/counter | Matched-core throughput and CPU per successful operation, including routing/runtime overhead |
| Tail latency under contention | Key-level reservations; fair bounded queues; incremental maintenance | Same offered load and success rate, hot-key/skew tests, queue-wait and end-to-end percentiles |
| Durable mixed workloads | Many nonconflicting prepared writes per owner; ordered group commit | Equivalent flush guarantees; durable write latency, concurrent read latency, recovery fault tests |
| Stable memory behavior | Compact bounded-growth storage; accounted retained buffers; snapshot headroom | Bytes per key, peak RSS/accounted allocations, overload and churn tests |
| Useful online operation | Consistent incremental checkpoints; opt-in cache eviction | Traffic during checkpoints, bounded memory, successful restore, visible abort/disk-pressure behavior |

Keep latency, successful throughput, peak memory, and recovery guarantees in the same acceptance process. Resource rejection, lower durability, or a different workload must not conceal a regression.

## 2. Invariants that optimizations cannot change

- Each shard has exactly one mutable-state owner. Only that owner reads or mutates live table and timer structures; background services receive owned/immutable data.
- Every supported keyspace command remains linearizable. Cross-shard reads and mutations are atomic; conditions, counters, TTL changes, expiration, and eviction participate in the same conflict protocol.
- Preserve per-connection execution and response order. Initially execute one command per connection at a time, with bounded parse-ahead; do not turn pipelining into unadvertised out-of-order semantics.
- No prepared mutation becomes visible before its configured log boundary. Strict responses require the durable watermark; periodic responses require a complete OS write. Memory-only mode has no log dependency.
- After a complete log decision, apply cannot fail recoverably: reserve all resources first. Unexpected failure stops the process and uses recovery. An unreceived response never proves the command did not commit.
- All queues, pending intents, staged effects, backing allocations, snapshot buffers, and cleanup obligations are bounded and charged. Control and completion progress never depends on free capacity in a saturated client-data queue.
- A timeout or disconnected socket cannot strand key reservations, accepted decisions, credits, or completion delivery.

## 3. Fast path and scheduling

Use dedicated single-owner engine threads and mature asynchronous networking. Reserve CPU for networking, logging, and maintenance rather than labeling every hardware thread an independent engine core. Use portable scheduling first; NUMA placement and affinity require Linux evidence.

- Route directly to the owning shard for single-shard commands. The memory-only single-key path has no global coordinator, log sequence, or globally contended metrics update.
- Move request buffers between stages; share immutable payloads only where their lifetime requires it. Zero-copy is not free: retained large input slabs and atomic reference operations must be measured and accounted. Copy small payloads out of disproportionately large retained slabs when beneficial.
- Batch transport and response encoding by request count, bytes, and execution budget. Flush when those limits or a bounded latency budget are reached; never require a full batch or a peer owner to make progress.
- Multiple connections supply concurrency initially. Pipeline parsing and coalesced transport must preserve each connection's dependency chain; later intra-connection execution changes need an explicit semantic proof.
- Fairly interleave ready requests, log completions, reservation controls, snapshot capture, and expiry cleanup. Conflicting queued work cannot be continually bypassed by new requests on the same keys. Independent keys may bypass blocked work.
- Export owner-local counters/histograms and aggregate off the request path. Benchmark allocations, cross-core traffic, batching delay, skew, and queue wait before changing the allocator or introducing platform-specific I/O.

## 4. Concurrent key-level coordination

Retain whole-shard reservations as the reference model. The target uses bounded coordinator contexts and owner-local intent tables, allowing nonconflicting multi-key commands to overlap without a global transaction-execution mutex.

1. Extract every key before execution. Keep original argument order/duplicates for command semantics; separately normalize the participant/key sets for conflict management. Missing keys need intents too.
2. Acquire participant owners in ascending owner order, with only one acquisition request outstanding per operation. At an owner, grant the operation's complete local key set atomically or grant none; never wait while holding only part of that local set.
3. Start with exclusive local intents even for multi-key reads, simplifying lazy expiry. Ordinary single-key reads execute immediately only when their key is available. They otherwise queue, rather than reading through unresolved writes.
4. Owners order overlapping wait groups fairly by admission sequence, permit disjoint progress, and bound registrations/waiters. Do not grant a newer conflicting request ahead of an older waiting group indefinitely. Expiration and eviction use this scheduler, not an out-of-band table mutation.
5. Once all key sets are reserved, evaluate one command-time snapshot, validate, and prepare final effects plus replies using reserved memory. Acquire neither new keys nor dependent transaction results after logging begins.
6. Submit one atomic effect record for a mutating command. Install on all participants after its log boundary. Retain all key intents until every participant confirms installation, then release and complete. Unrelated keys on those same owners keep progressing.
7. Before the log decision, abort preparation cleanly on validation/capacity/cancellation policy. After acceptance, finish or fail-stop; never tell clients an uncertain decision was rolled back.

Ascending participant acquisition plus all-or-none local groups excludes the intended lock-order cycles. This is a proof obligation, not a substitute for model checking: include fairness queues, memory credits, expiry, disconnects, and log completion paths in the liveness model. Bound the number and bytes of coordinators so adversarial multi-key traffic cannot occupy every key indefinitely. No waiting for admission memory, client output, or a new dependent command while holding intents.

## 5. Pipelined durable execution

**Remove owner-wide synchronization stalls before redesigning the log.** One owner can hold multiple independent prepared operations, each with its own intents, effects, reserved resources, response, and completion identity.

- The committed table stays unchanged while a write awaits its boundary. Conflicting commands wait; other keys may read, prepare, or commit. Pending count/bytes and maximum group delay are bounded.
- A single ordered WAL service assigns sequence numbers, coalesces records into bounded vectored writes, and advances written/durable watermarks. Complete records retain per-command framing and checksums; a group need not contain work from all owners.
- Only the correct watermark releases an operation for publication. Owners may publish independent records in different physical order because their keys do not conflict; the intent protocol preserves order for overlapping operations. Checkpoints drain all pre-cut decisions before choosing their cut.
- Completed control delivery is reserved before submission. A disk stall fills bounded prepared capacity and activates admission backpressure, not unbounded speculative state. Never silently downgrade strict durability.
- Keep one log stream initially: simpler cross-shard atomic recovery is valuable. Measure log CPU, append bandwidth, queueing, and synchronization cost. Only a demonstrated remaining bottleneck justifies a separate striped/per-shard log design with explicit cross-log commit and recovery rules.
- A hot key still executes serially. Later, a bounded owner-local sequential mini-batch could amortize synchronization by keeping the entire batch private and logging its committed effects before any response. Do not implement same-key speculation, batch dependencies, or early acknowledgments without a separate model and crash tests.

## 6. Bounded growth and online checkpoints

### Storage layout target

Prototype an owner-local segmented, incrementally split hash table using safe Rust. Compare it to the conventional preallocated reference table before promotion. Keep independently addressable bounded buckets, stable bucket identifiers, immutable payload ownership, paged directory allocation, and explicit occupancy/overflow limits.

Growth migrates bounded entries per owner scheduling turn, not the whole table in one request. During migration, lookups consult the documented old/new locations; keys exist logically once. Charge both storage generations until reclaimed. Seed hashing against collision attacks; a bounded overflow limit may reject admission rather than allow unlimited chains. Test insertion, deletion, replacement, expiration, and concurrent maintenance at every migration step.

This replaces the reference model's fixed owner-capacity ceiling as a usability target, not the overall memory limit. Shard ownership remains fixed for a process lifetime; local bucket growth is not online resharding.

### Online checkpoint algorithm

Use **one active checkpoint epoch**, with a stable bucket layout and copy-before-first-change capture. This avoids a full-dataset clone, unbounded version chains, and fork-based snapshots. Keep the maintenance checkpoint as the reference/fallback.

1. Pre-reserve checkpoint control state, buffers, output disk headroom, and a retention budget. Finish or suspend bounded bucket migrations. Enter an admission fence, drain admitted keyspace work and all pending decisions, synchronize the WAL, and choose the last fully applied sequence as the snapshot cut. Initialize per-owner epoch/cursor/bucket-watermark metadata, then resume admissions. The fence does not scan the dataset, but storage stalls can still delay it; report this honestly.
2. Freeze cross-bucket structural migration for that epoch. Each owner incrementally captures existing buckets. An epoch marker per bucket prevents duplicate capture without clearing every marker at startup. The captured set excludes new post-cut keys and includes stored deadlines/tombstones according to the exact checkpoint format.
3. Before the first post-cut mutation of an uncaptured bucket, capture that bucket's pre-change entries into a bounded owned/immutable batch. This applies to insertions, deletions, value changes, TTL metadata, expiration, and eviction. Because no earlier post-cut change bypassed capture, this batch represents the bucket at the cut. The owner also performs background capture under a work budget.
4. Capture retains immutable payload references rather than copying arbitrary large values on the owner. Charge retained allocations and metadata until the serializer releases them; count and byte limits apply to each capture. A generation/bucket identifier and completion accounting let the writer detect omissions and duplicates. The writer never iterates live tables.
5. If retention/queue capacity is insufficient, structural growth is required before uncaptured state is secured, or storage fails, cancel the generation safely rather than block all writes or exceed memory. Owners terminate capture obligations explicitly; publication requires successful completion from every owner and no failed generation. Cancellation does not return credits until references are actually released, so ordinary overload handling may still apply.
6. Once every bucket is captured and serialized, validate counts/checksums, synchronize the new snapshot, publish and synchronize its manifest with the baseline crash-safe ordering, and only then reclaim covered log segments. Replay applies the complete post-cut log suffix. No incomplete/aborted generation can become authoritative.

Stable bucket membership plus first-change capture preserves exactly the cut state, including deleted/reinserted keys and cross-shard writes. Verify this with a model, not only end-to-end examples. Do not filter keys merely because wall time advanced during serialization; logged expiry and recovery handle deadlines consistently.

Snapshot progress is conditional on reserved resources and healthy storage, not guaranteed under every write pattern. Repeated aborts retain the WAL and surface degraded health; enforce disk headroom and offer the maintenance fallback. Online means concurrent steady-state traffic, not zero overhead or zero setup pause.

## 7. Cache eviction without hidden memory or durability violations

Keep no-eviction as the default. Add an explicit opt-in, owner-local sampled CLOCK-style policy after resource accounting and logging are proven. Specify selection behavior, memory-release accounting, and interaction with durable deletions.

- Track access hints locally with bounded update cost. Prefer already-expired reclaimable entries and bounded victim sampling; skip conflicting intents and pending writes.
- Rebalance unused byte credits among owners at a bounded rate; do not introduce a global per-access LRU list. Report skew and situations where local admission still fails.
- Trigger reclaim before exhaustion. If a user command lacks admission resources, release/retry before a log decision or fail explicitly; do not hold its intents while waiting for victim deletion or memory owned by another operation.
- Delete victims through normal prepare/log/publish rules. Strict-mode eviction is not durable until synchronized. Account for snapshot/reply references: removing a table entry does not necessarily free its allocation.
- Bound eviction work and retries. When no eligible victim releases enough actual memory, reject admission rather than loop or overcommit. Test every policy with TTLs, snapshots, durable recovery, and output-retained values.

## 8. Staged implementation

Build an executable vertical slice, extend it behind correctness gates, and document the capabilities available at each stage.

1. **Contract and oracle:** Maintain the Rust workspace, pinned toolchain/dependencies, Linux CI, RESP2 framing limits, command metadata, deterministic clock-injected engine, and command/property tests. Use synthetic, repeatable protocol vectors.
2. **Runnable memory-only slice:** Implement bounded networking and independent owner workers; initially expose basic session operations, strings, counters, deletion/existence, and expiration. Test negotiation/authentication. Unsupported target commands remain explicitly unsupported. Verify orderly shutdown, malformed/fragmented frames, pipelines, binary values, same-key ordering, and cross-owner independent progress.
3. **Atomicity and resource baseline:** Implement all supported multi-key operations using the simpler reservation reference, then model-check and introduce concurrent key-level coordination. Complete accounted memory, no-eviction admission, expiry, slow-client protection, and queue-liveness tests.
4. **Durable core:** Implement versioned effect records, replay, periodic/strict boundaries, fail-stop behavior, and crash tests; then pipeline nonconflicting prepared writes and group commit. Preserve the simpler execution mode for differential checking.
5. **Storage lifecycle:** Deliver maintenance checkpoints, manifests, reclamation, and restore tests before adding incremental table growth and the online capture algorithm. Promote optimized storage only after semantic/property tests and measured advantages; keep the bounded reference path if the prototype loses.
6. **Cache and production gates:** Add opt-in eviction, security hardening, metrics, operational documentation, and Linux storage qualification. Run representative workload tests and publish command support, operating limits, and measured resource behavior.

**First implementation milestone:** A buildable, tested memory-only multicore server with the initial documented command set and bounded request admission. Promote persistence, online checkpoints, and eviction only after their own gates pass. Record validation results separately from implementation status.

## 9. Performance acceptance protocol

- Use consistent Linux build settings and a repeatable synthetic workload when evaluating a change. Keep private deployment details out of published results.
- Keep CPU allocations, memory budgets, transport/security, dataset, payloads, persistence guarantees, and client behavior fixed when evaluating an optimization. Include all auxiliary threads in the resource budget.
- Test owner/core scaling, one hot key, skewed distributions, disjoint and overlapping multi-key traffic, varied read/write ratios and value sizes, pipeline depth, expiry churn, slow clients, overload, strict disk stalls, and traffic during checkpoints.
- Report sustainable successful throughput at the same latency budget and latency at matched throughput. Include p50/p95/p99/p99.9, offered load, errors/rejections, queue depth, CPU, bytes per key, peak RSS, durable lag, checkpoint pause/abort rate, and recovery results.
- Use open-loop or coordinated-omission-corrected measurements, fixed seeds, independent load-generator capacity checks, warm-up, repeated runs, and distributions rather than one favorable peak.
- Set hardware/workload-specific numeric acceptance limits before tuning. Promote optimizations when correctness/fault gates pass and measured gains justify complexity without hidden regressions.
