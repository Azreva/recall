# Static repository checks

[The Python checker](tools/static_check.py) provides offline feedback without a Rust toolchain. It uses **Python 3.11+ and only the standard library**. It is a development tool, not part of Recall's runtime.

Linux is the supported development and CI target. The checker's use of portable Python does not establish support for other Recall platforms.

## Run

- Text report: [`python -B tools/static_check.py`](tools/static_check.py:1).
- JSON report: [`python -B tools/static_check.py --format json`](tools/static_check.py:1).
- Deployment preparation: [`python -B tools/static_check.py --require-lock`](tools/static_check.py:1).
- Fail on warnings too: [`python -B tools/static_check.py --strict`](tools/static_check.py:1).
- Checker tests: [`python -B -m unittest discover -s tools -p test_static_check.py -v`](tools/test_static_check.py:1).

Use your installed Python 3.11+ executable; no packages are needed. The default repository root derives from the script location, not the terminal directory. [`--root`](tools/static_check.py:1) can select another tree.

Exit status is zero with no errors, one for findings failing the selected policy, and two for usage errors. Reports go to standard output. A missing generated [dependency lock](Cargo.lock) is a warning by default and an error with the deployment flag; the checker never creates it.

## Checks performed

- Required files, workspace targets, TOML syntax, direct dependency/toolchain pins, lint inheritance, local dependency paths, and basic lock/direct-pin consistency.
- Local Markdown targets, heading anchors, editor-style line references, and the shared Claude guide import. Existing repository-root-relative editor links and explicit document-relative links are recognized.
- Python syntax without importing or executing scanned modules.
- Rust lexical/delimiter hazards, executable unfinished placeholders, unsafe constructs, unbounded-channel calls, and selected deterministic-core boundary violations.
- Merge markers, NUL bytes, and bounded UTF-8 inputs.
- Tracked environment-template names/ranges, loopback bind, absence of default credentials, and loader/schema consistency.
- Environment ignore rules and supplied Docker/Compose/service policy: locked builds, non-root execution, excluded secrets, preserved listener scope, and basic hardening.

These are targeted repository rules, not general language/deployment parsers. When a deliberate design change alters a rule, review the policy and extend its tests rather than suppressing a finding for a passing report.

## Data handling

The checker does not run subprocesses, use the network, read process secrets, compile code, mutate project files, or import project modules. It skips private environment files, generated directories, and external/symlink inputs. Only the tracked environment example is inspected. Diagnostics do not print environment values.

Checker tests create temporary synthetic fixtures and clean them up; they do not use local passwords or production data.

## Limits

A passing result does not prove Rust syntax/type correctness, borrowing, dependency resolution, formatting, successful compilation, runtime behavior, atomicity, liveness, allocation accounting, durability, or recovery.

It also does not validate YAML/Docker syntax comprehensively, build containers, check runtime libraries/permissions, inspect real network exposure, confirm readiness, check external URLs, detect every secret, audit security, or measure performance.

Run [Rust contribution checks](CONTRIBUTING.md) and [deployment validation](deployment.md) separately and report their results independently.
