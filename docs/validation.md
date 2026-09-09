# Validation and development

Recall is a non-production prototype. **Linux is the sole Tier 1 development, testing, and deployment target.** Passing individual tests does not establish production performance, security, or reliability.

This guide defines repeatable checks and coverage expectations, not a record of private development sessions.

## 1. Formatting, diagnostics, and baseline tests

Use [the pinned toolchain](rust-toolchain.toml) in an authorized Linux workspace. Locked builds require [a generated dependency lock](Cargo.lock); if absent, use Cargo or [the Docker bootstrap](deployment.md#first-checkout-generate-the-dependency-lock-with-docker). No separate deployment-provenance archive is required.

**Without a host Rust installation (recommended on the server):**

1. If you intentionally want to normalize formatting first, run [`cargo fmt --all`](Cargo.toml:1) in a local toolchain workspace; otherwise skip to the Docker gate.
2. Run the Docker lint gate: [`docker buildx build --target lint --no-cache-filter lint --progress=plain .`](Dockerfile:1). This runs rustfmt verification and Clippy with warnings as errors inside the pinned container.
3. Run the Docker test gate: [`docker buildx build --target test --no-cache-filter test --progress=plain --load -t recall:test .`](Dockerfile:1). This executes the full Rust unit/integration suite fresh, including the negative authorization tests.
4. The runtime image build remains [`docker build -t recall:local .`](Dockerfile:1).

**With a host toolchain instead:** run [`cargo fmt --all -- --check`](Cargo.toml:1), [`cargo clippy --workspace --all-targets --locked -- -D warnings`](Cargo.toml:1), [`cargo test --workspace --locked`](Cargo.toml:1), and [`cargo build --workspace --release --locked`](Cargo.toml:1) from the repository root.

Resolve diagnostics rather than disabling them for a passing result. Rerun relevant tests after behavior-changing fixes. [Linux CI](.github/workflows/ci.yml) applies the same checks.

An optional offline pass is [`python -B tools/static_check.py`](tools/static_check.py:1). Checker regressions use [`python -B -m unittest discover -s tools -p test_static_check.py -v`](tools/test_static_check.py:1). [The checker guide](static-checks.md) explains its limits.

## 2. Automated coverage

| Component | Coverage expectations |
| --- | --- |
| [Protocol](crates/recall-protocol/src/lib.rs) | Fragment boundaries, binary data, pipelines, malformed lengths, aggregate limits, reply encoding |
| [Commands](crates/recall-core/src/command.rs) | Integer boundaries, argument counts, option combinations, case handling, unsupported operations |
| [Storage](crates/recall-core/src/store.rs) | Conditional writes, counters, duplicate keys, preparation rollback, capacity failure, expiry |
| [Expiration](crates/recall-core/src/expiry.rs) | Heap order, arbitrary removal, repeated deadlines without timer growth |
| [Configuration](crates/recall-server/src/config.rs) | Loopback enforcement, limits, owner-budget apportionment |
| [Coordination](crates/recall-server/src/engine.rs) | Conflicting/disjoint operations, atomic publication, cancellation, saturated queues, resource release |
| [Networking](crates/recall-server/tests/network.rs) | Framing, authentication, negotiation, order, I/O deadlines, shutdown |
| [Environment loading](crates/recall-server/src/settings.rs) | Precedence, literal parsing, byte limits, invalid values, file selection, credentials |

Use isolated state, synthetic data, and injected clocks where appropriate. Integration tests must not target an unrelated running instance. Automated coverage is not an exhaustive concurrency proof or long-duration stress test.

## 3. Negative authorization cases

Verify these cases on a dedicated Linux test instance with authentication enabled. Add regression tests wherever current coverage is incomplete:

- New connections cannot read or mutate keys before authentication; rejected writes have no side effects.
- Wrong passwords, empty supplied passwords, invalid usernames, and malformed authentication arguments do not grant access.
- Failed authentication followed by a command remains unauthorized. An authenticated connection does not authorize other clients.
- Negotiation without valid credentials cannot bypass authentication. Unsupported protocol requests must not authenticate through partially processed options.
- Pipelines containing failed authentication followed by protected commands remain ordered and do not execute those operations.
- Valid authentication permits intended commands; subsequent new connections start unauthenticated.
- Credential changes follow the documented startup configuration path. Invalid configured credentials fail startup rather than silently disabling authentication.
- Responses and diagnostics do not expose passwords or protected values.

These checks cover the current default-user model, not multi-user ACL support. Use synthetic credentials and keep private environment files out of reports.

## 4. Deployed-instance smoke tests

On a dedicated loopback test instance, exercise request/response behavior, binary values, conditions, counters, overflow, expiration, and multi-key operations. Include ordered pipelines, fragmented input, large valid values, malformed requests, connection churn, and valid/invalid authentication.

Use a unique test-key namespace, bound the generated work, and clean up only those keys. Do not clear an entire keyspace as routine cleanup. [The deployment guide](deployment.md) describes configuration and memory-only behavior.

## 5. Stress, resource pressure, and shutdown

- Sustain traffic long enough to observe queueing, retained allocations, expiration cleanup, and latency drift.
- Exercise hot keys, skewed ownership, overlapping multi-key operations, varied values, and slow clients.
- Saturate connection, request, output, and memory limits. Verify bounded rejection and recovery rather than hangs or uncontrolled allocation.
- Check that disconnects release resources while accepted commands reach their required terminal state.
- Stop with work in flight. Verify draining, bounded shutdown, and handling of clients that do not read responses.
- Validate Linux service/container settings separately from library tests. Forced termination does not substitute for orderly-shutdown testing.

## 6. Performance and future guarantees

Keep workloads and resource budgets consistent when evaluating changes. Distinguish command latency from whole-pipeline latency, include rejection rates, and account for load-generator overhead. A short closed-loop test does not establish maximum throughput or sustained capacity.

The prototype needs substantial work on scheduling, allocation accounting, reclamation, fine-grained coordination, storage growth, and security. Persistence, snapshots, eviction, and remote transport security need their own correctness/fault gates before being advertised as supported. See [the architecture](plans/architecture.md), [performance design](plans/performance.md), and [roadmap](ROADMAP.md).