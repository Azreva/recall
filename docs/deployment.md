# Deploying Recall

Recall's prototype runs on Linux as a standalone executable or in a container. **Linux is the sole Tier 1 development and deployment target. These instructions are for evaluation, not production use.** Both methods run the Rust server directly; Python is only an optional development checker.

**Current boundary:** The server is memory-only and permits loopback listeners only. Stopping, restarting, crashing, or exceeding a process/container memory limit loses the dataset. Deployment artifacts do not add persistence, TLS, or public-network access.

## Configuration

Copy [the environment template](.env.example) to [a local environment file](.env), then edit the local file. It is ignored by version control and excluded from Docker build inputs. There is no bundled password.

On Linux, use [`cp .env.example .env`](.env.example:1), then [`chmod 600 .env`](.env.example:1).

Do not overwrite existing configuration. Use a private, nonempty password when authentication is required; leave the password setting absent to disable local-development authentication. An explicitly empty password is rejected.

### Precedence and syntax

**Command line → process environment → environment file → built-in defaults.**

Recall automatically loads [the local environment file](.env) from its working directory if present. Use [`--env-file PATH`](crates/recall-server/src/settings.rs:1) for a file that must exist, or [`--no-env-file`](crates/recall-server/src/settings.rs:1) to disable file loading. These options do not disable process-environment settings. Configuration is read once at startup; edits require a restart or container recreation.

| Environment setting | Command-line override | Meaning / built-in default |
| --- | --- | --- |
| [`RECALL_BIND`](crates/recall-server/src/settings.rs:1) | [`--bind`](crates/recall-server/src/main.rs:1) | Loopback IP and port; 127.0.0.1:6379 |
| [`RECALL_WORKERS`](crates/recall-server/src/settings.rs:1) | [`--workers`](crates/recall-server/src/main.rs:1) | Owner threads; CPU-based default, range 1–64 |
| [`RECALL_IO_THREADS`](crates/recall-server/src/settings.rs:1) | [`--io-threads`](crates/recall-server/src/main.rs:1) | Network runtime threads; 2 |
| [`RECALL_MAX_MEMORY_MIB`](crates/recall-server/src/settings.rs:1) | [`--max-memory-mib`](crates/recall-server/src/main.rs:1) | Live key/value payload budget; 512 MiB |
| [`RECALL_KEYS_PER_WORKER`](crates/recall-server/src/settings.rs:1) | [`--keys-per-worker`](crates/recall-server/src/main.rs:1) | Per-owner key ceiling; 16,384 |
| [`RECALL_MAX_CONNECTIONS`](crates/recall-server/src/settings.rs:1) | [`--max-connections`](crates/recall-server/src/main.rs:1) | Simultaneous connections; 128 |
| [`RECALL_QUEUE_BYTES_MIB`](crates/recall-server/src/settings.rs:1) | [`--queue-bytes-mib`](crates/recall-server/src/main.rs:1) | Byte budget per owner/coordinator admission queue; 4 MiB |
| [`RECALL_PASSWORD`](crates/recall-server/src/settings.rs:1) | None | Optional default-user password, 1–1024 UTF-8 bytes; absent by default |

The example uses a smaller 256 MiB payload budget and 32 connections. These are example settings, not different built-in defaults. Leave the worker setting commented to retain automatic selection.

Environment files accept one literal assignment per line, blank lines, full-line comments, and matching single/double quotes around values. Whitespace around assignments is trimmed; quoted interior whitespace is preserved. There is **no shell execution, variable interpolation, escape processing, multiline value, or inline-comment syntax**. A hash inside a value is literal. Duplicate/unknown settings, malformed quotes, NUL bytes, files over 16 KiB, and lines over 2 KiB are rejected. Process-environment settings in Recall's namespace are also validated. Diagnostics identify a setting or line without printing its value.

## Manual executable

In an authorized Linux workspace, follow [the contribution checks](CONTRIBUTING.md). Locked builds require the pinned Rust toolchain and [a generated dependency lock](Cargo.lock). Generate the lock with Cargo if absent; do not manufacture it or silently change resolution in a locked build.

1. Build with [`cargo build --release -p recall-server --locked`](Cargo.toml:1).
2. Prepare the local configuration above.
3. Start [`./target/release/recall-server --env-file .env`](crates/recall-server/src/main.rs:1).

Use the executable only on Linux hosts with a matching CPU architecture and compatible runtime libraries.

Applications on the same host connect directly to the configured loopback port using RESP2. Verify the listen address in startup output and a client health request before routing application traffic. A live process is not proof of successful command execution. Interruption or termination signals stop new connections, complete accepted work, and join owners. Forced termination immediately loses cached data.

### Optional Linux service

The provided [service unit](deploy/recall.service) uses a dedicated unprivileged account, restricted filesystem access, and a 1 GiB process memory ceiling. On a native Linux systemd host, after reviewing the binary and settings:

1. Create the account **if absent**: [`sudo useradd --system --user-group --no-create-home --shell /usr/sbin/nologin recall`](deploy/recall.service:1). Adapt account creation to your distribution.
2. Create the binary directory: [`sudo install -d -m 0755 /opt/recall/bin`](deploy/recall.service:1).
3. Install the binary: [`sudo install -m 0755 target/release/recall-server /opt/recall/bin/recall-server`](deploy/recall.service:1).
4. Create the configuration directory: [`sudo install -d -o root -g recall -m 0750 /etc/recall`](deploy/recall.service:1).
5. Install private configuration: [`sudo install -o root -g recall -m 0640 .env /etc/recall/recall.env`](deploy/recall.service:1).
6. Install the unit: [`sudo install -m 0644 deploy/recall.service /etc/systemd/system/recall.service`](deploy/recall.service:1).
7. Run [`sudo systemctl daemon-reload`](deploy/recall.service:1), then [`sudo systemctl enable --now recall`](deploy/recall.service:1).
8. Inspect [`sudo systemctl status recall`](deploy/recall.service:1) and [`sudo journalctl -u recall -f`](deploy/recall.service:1).

The service restarts on failure, each time with an empty cache. Normal shutdown has a 30-second service-manager deadline. Review memory, task, and file-descriptor limits for the chosen worker/connection counts. Do not remove hardening to conceal a permissions error.

## Docker on native Linux

The [Dockerfile](Dockerfile) uses a Rust build stage and a slim runtime stage containing the server and system libraries. Runtime user/group is 10001. The executable receives termination signals directly. Local environment files are not copied into either stage.

### Prerequisites

- Native Linux Docker Engine and Compose v2. This host-network configuration is **not a Docker Desktop or rootless-networking deployment recipe**.
- A generated [dependency lock](Cargo.lock). Use the Docker-only first-checkout step below if absent. The runtime image build intentionally requires it. Docker supplies the build toolchain; Rust/Python are unnecessary on the runtime host.
- An existing regular [local environment file](.env) readable by container user 10001. On an appropriate native Linux filesystem, [`sudo chown 10001:10001 .env`](.env.example:1), then [`sudo chmod 0600 .env`](.env.example:1), grants access without making secrets world-readable. Use a dedicated copy or managed ACLs where ownership changes are unsuitable.

[Compose](compose.yaml) mounts the private file read-only, rather than injecting its contents into build arguments or image metadata. Docker/host administrators can still access mounted secrets; a container is not a secret boundary against its host administrator.

### First checkout: generate the dependency lock with Docker

If [the dependency lock](Cargo.lock) is missing, run [`docker build --target lockfile --output type=local,dest=. .`](Dockerfile:1) from the repository root **before** building or starting the Compose service.

This explicit target uses the pinned Rust build container to resolve dependencies, then exports only the generated lock into the current directory. It needs Docker BuildKit and registry access but no host Rust installation. It does not compile Recall, start a service, mount the host source tree into a running container, or include private environment files in build inputs.

Do not rerun the bootstrap on an existing lock unless deliberately updating dependencies: the export can replace that file. Ordinary server-image builds use locked resolution and do not silently generate or refresh the lock.

### Run the Rust tests using Docker

After generating the lock if needed, run [`docker buildx build --target test --no-cache-filter test --progress=plain --load -t recall:test .`](Dockerfile:1). This reuses the build/dependency layers but executes the test stage freshly in the pinned environment. Tests create isolated local Recall instances; they do not connect to the deployed cache or receive its private environment file.

The test target is explicit: building the runtime image or seeing a startup banner does not imply these tests ran. Resolve test failures before relying on changed behavior. A cached test layer represents a prior execution, not a fresh run. If Buildx lacks stage-specific cache controls, use [`docker build --target test --no-cache --progress=plain .`](Dockerfile:1), which rebuilds all relevant layers as well.

### Build and run

Validate configuration with [`docker compose --env-file .env.example config --quiet`](compose.yaml:1), then run [`docker compose --env-file .env.example up --build -d`](compose.yaml:1).

The explicit Compose environment-file argument selects the nonsecret template for **Compose interpolation**. Recall reads the actual private file mounted by the service. This avoids Compose interpreting dollar signs or quoting inside the private password file. Host-shell settings are not automatically forwarded to the container; edit the mounted file or explicitly configure the container environment.

- Status: [`docker compose --env-file .env.example ps`](compose.yaml:1).
- Startup logs: [`docker compose --env-file .env.example logs --tail=100 recall`](compose.yaml:1).
- Reload changed configuration reliably: [`docker compose --env-file .env.example up -d --force-recreate recall`](compose.yaml:1).
- Graceful stop: [`docker compose --env-file .env.example down`](compose.yaml:1).

Container recreation remounts the current configuration file. This also handles editors that save by atomically replacing the file: a restart of the same container can retain a single-file bind mount of the old inode. Recreating the container discards the memory-only dataset. Removing volumes is neither needed for configuration changes nor a safe general substitute for recreation.

Direct image build: [`docker build -t recall:local .`](Dockerfile:1). The Compose service supplies runtime restrictions and the file mount; building an image alone does not validate them.

### Networking and resources

Recall remains bound to the host's loopback address through host networking. **No container port is published and no wildcard bind is enabled.** Same-host applications can connect; external hosts cannot connect directly. One listener occupies each host address/port. A bridge-networked application container has a different loopback interface and cannot treat this address as its own host.

Host networking reduces network isolation. Use this recipe only for trusted local deployment. Remote/TLS and isolated bridge networking need their own security design; do not solve connectivity by removing loopback validation.

The container has a read-only root filesystem, drops capabilities, forbids privilege escalation, limits processes/memory, and mounts only the explicit read-only configuration. Restart is manual. Its 1 GiB limit is separate from the live payload budget: allow for runtime, metadata, queues, retained input/output, and allocator overhead. Exceeding it can terminate Recall and lose the entire cache.

## Validation boundary

[Static checks](static-checks.md) inspect repository and deployment policy, not Rust compilation, complete Docker/Compose semantics, host permissions, or service readiness. Complete native Rust checks, Docker build/start/stop tests, and real command round trips on the deployment host before relying on these artifacts.
