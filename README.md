# CraftPilot Runtime Agent (Rust)

A Rust rewrite of the [CraftPilot runtime agent](https://github.com/cozilic/craftpilot-runtime-agent),
built as a side project. The agent connects a trusted host to a CraftPilot master, manages
Minecraft containers through the host's Docker daemon, and speaks the **same HTTP contract** as
the Python agent — no master changes are required to run it side by side.

> **Installera agenten:** [INSTALL.sv.md](INSTALL.sv.md) – Linux med `setup.sh` eller Windows med
> en fristående EXE.
>
> **Svensk driftsättningsguide för nodägare:** [GUIDE.sv.md](GUIDE.sv.md)
>
> **Windows-panel (EXE):** [desktop/README.md](desktop/README.md) – ladda ner, dubbelklicka och
> följ statusen för Docker, master och agent.

> **Status: phase 7.** Enrollment, heartbeat, command claiming, acknowledgement, START, STOP over
> RCON, the one-time initial SNAPSHOT, console commands, the file manager, encrypted runtime-node
> migrations and server telemetry work, file downloads stream to the master, and the remote file
> tree is indexed and synced in bounded batches.

## Implemented

- Self-enrollment: files a request, polls until a sysadmin approves it under
  **Ops & log → Runtime nodes**, stores the credential and completes the request.
- Replacement enrollment when the master rejects the credential (node deleted/disabled), and a
  fresh request when a pending/expired enrollment is no longer known to the master.
- Private state file (`state.json`, `0700` directory / `0600` file) with the node ID, credential,
  pending enrollment and a strictly increasing heartbeat sequence, written atomically.
- Heartbeat on startup, then `commands/claim` every poll interval (the master counts a claim as a
  heartbeat, so each request consumes a new sequence number).- Acknowledgement with lease token, including the "lease expired" path.
- **START** through the Docker CLI: derives `craftpilot-<server-id[0..8]>`, verifies the
  `craftpilot.managed=1` / `craftpilot.server_id` labels before touching an existing container,
  refuses images outside the allowlist and networks other than its own, mounts
  `<data root>/<server uuid>` at the spec's mount path, and runs `docker run` with a fixed argv
  (`--memory`, `--memory-swap`, `--cpus`, `--restart no`, `--user`, `--publish <port>:25565/tcp`).
  START is idempotent and answers `ALREADY_RUNNING` when the container already runs.
- **STOP** asks Minecraft itself to shut down: START enables RCON in `server.properties` with a
  random per-server password persisted in the agent state (never published, reachable only on the
  isolated Docker network), and STOP sends `stop` over RCON, then waits for the container to exit.
  Servers without RCON (Pumpkin, a missing properties file, a rejected password or a timeout) fall
  back to `docker stop --time <grace>` (SIGTERM) and `force` sends SIGKILL immediately, answering
  `STOPPED` / `ALREADY_STOPPED`; a container that loses its labels is never touched.
- **TELEMETRY** (`servers` + `servers/report`): every 30 s, and immediately after a start/stop,
  the agent reports its assigned servers: status from `docker inspect` (missing → `OFFLINE`,
  running → `ONLINE`, exited 0 → `OFFLINE`, non-zero → `CRASHED`), CPU/memory from
  `docker stats --no-stream`, `players_max` from `server.properties` (`max-players`, default 20),
  `players_online` from join/leave console lines, `disk_used_mb` from a recursive walk of the
  server directory (cached for five minutes, symlinks are never followed), `artifact_present`
  from the jar name and minimum size the master lists in `artifacts`, and new console lines from
  `docker logs --timestamps` with a cursor persisted in the state volume (the first report sends a
  400-line backlog; after a restart only new lines are sent).
  Each report consumes a new sequence number, so the master never sees it as a replay.
- **MIGRATIONS** (`migration_prepare`, `migration_stop`, `migration_export`, `migration_import`,
  `migration_commit`, `migration_delete_source`): the destination creates an X25519 keypair and
  keeps the private half in the agent state volume (`0600`); the source archives the server root,
  derives an AES-256-GCM key with HKDF-SHA256 over the X25519 shared secret (protocol
  `craftpilot-server-migration:v1`, salt and AAD bound to migration/source/destination), encrypts
  in 1 MiB chunks with a `nonce_prefix || index` nonce and relays them through the master. The
  destination downloads, decrypts and authenticates every chunk, verifies the archive SHA-256 and
  byte count against the manifest, extracts with the snapshot's ZIP guards into a private staging
  directory and only installs it into the server root on `migration_commit`. Source cleanup
  requires a recorded export with the same digest; the server root is never touched otherwise.
- **FILE MANAGER** (`file_list`, `file_read`, `file_mutate`): every path is normalised and then
  resolved against the server's sandbox root — no `..`, no absolute or drive-letter paths, no
  control characters, depth/segment/length limits, and the nearest existing ancestor is
  canonicalised so a symlink can never lead outside the root. Listing is capped at 1000 entries;
  reads are UTF-8 only and bounded by the edit limit. Mutations: `write` (atomic temp+rename),
  `mkdir`, `touch`, `rename`, `delete` (a symlink is unlinked, never followed), `zip`, `unzip`
  (validated, extracted to a temporary directory and only then moved into place, with ZIP-slip,
  symlink, ratio and size guards) and `upload` (streamed from the master with the lease, checked
  against the declared byte count and SHA-256). Results are posted to the master's
  `commands/{id}/result` endpoint **before** the ack, which is where the master reads them.
  A persistent directory index (`file-tree-index.json`, `0600` in the state volume) keeps the
  master's remote file-tree cache fresh: agent mutations invalidate the affected directory, the
  index is rescanned every 30 seconds for changes made directly on the host, and deltas are sent
  in batches of 20 to `/file-tree` with progress reported to `/file-tree/progress`.
- **CONSOLE**: writes one attributed line to the game's console FIFO with the same argv shape as
  the master's own Docker adapter (`docker exec <name> sh -c 'printf … > /tmp/.craftpilot-console'
  sh <command>`), so the command is never interpolated into a shell script. Requires the container
  to exist, carry this server's labels and be running; the line is trimmed, must be non-empty,
  free of control characters and at most 1000 characters. Answers `CONSOLE_EXECUTED` with
  `sha256(payload.command)`, the digest the master verifies. Console text is never logged.
- **Initial SNAPSHOT** (the one-time master→node copy that makes a remote server startable):
  downloads the bounded archive from this node's command URL with its bearer credential,
  rejects any other origin, host, path, query or redirect, enforces the declared byte count and
  SHA-256 while streaming, rejects unsafe ZIP entries (absolute or traversing paths, symlinks,
  special files, control characters, per-file/size/ratio/count limits) and extracts into a
  private staging directory. The result is installed with one atomic rename into a missing or
  empty `<data root>/<server uuid>`; a non-empty destination is refused, never overwritten.
  Answers `SNAPSHOT_STAGED` with the verified digest and byte count the master checks.
- Signed-off configuration validation: HTTPS-only master origin with no path/query/credentials,
  poll interval 5–120 s, absolute data root, Docker network name, stop grace 0–45 s.

## Not implemented yet

A download larger than `CRAFTPILOT_MAX_UPLOAD_BYTES` is refused before anything is streamed.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `CRAFTPILOT_MASTER_URL` | `https://craftpilot.se` | Master origin (HTTPS, no path). |
| `CRAFTPILOT_AGENT_STATE_DIR` | `/var/lib/craftpilot-agent` | Private state directory. |
| `CRAFTPILOT_SERVER_DATA_ROOT` | `/srv/craftpilot/servers` | Server data root (used once operations land). |
| `CRAFTPILOT_NODE_ID` | – | Only to attach an existing node. |
| `CRAFTPILOT_REGISTRATION_TOKEN` | – | One-time token for a re-enabled node. |
| `CRAFTPILOT_POLL_SECONDS` | `5` | Command poll interval, 5–120. |
| `CRAFTPILOT_DOCKER_NETWORK` | `craftpilot-servers` | Network the game containers join; a spec for another network is refused. |
| `CRAFTPILOT_ALLOWED_IMAGES` | master Java images + `craftpilot-pumpkin:latest` | Exact image allowlist. |
| `CRAFTPILOT_STOP_GRACE_SECONDS` | `30` | `docker stop` timeout before the daemon sends SIGKILL, 0–45. |
| `CRAFTPILOT_DOCKER_SERVER_DATA_ROOT` | – | Host-side data path for the volume when it differs from the in-container mount (Docker Desktop). |
| `CRAFTPILOT_DATA_GID` | `33` | Group that staged server files are shared with (Minecraft's GID in the panel images). |
| `CRAFTPILOT_MAX_EDIT_BYTES` | `4194304` | Largest file the editor may read or write (same as the master's `server.files.max_edit_bytes`). |
| `CRAFTPILOT_MAX_UPLOAD_BYTES` | `2147483648` | Largest upload accepted from the master (same as `server.files.max_upload_bytes`). |
| `RUST_LOG` | `info` | Log filter. Console text, credentials and RCON passwords are never logged. |

## Run locally

The agent uses the same local TLS bridge as the Python agent (see
`CraftPilot/runtime-agent/create-local-certs.sh`). Inside the Compose network the master is
`https://runtime-tls`; from the host it is `https://localhost:8443`.

```sh
CRAFTPILOT_MASTER_URL=https://localhost:8443 \
SSL_CERT_FILE=<CraftPilot>/runtime-agent/local-certs/ca.crt \
CRAFTPILOT_AGENT_STATE_DIR=$PWD/.agent-state \
cargo run
```

Approve the logged request ID in **Ops & log → Runtime nodes**. Delete `.agent-state` to enroll
again (it holds the credential).

## Build and test

Rust 1.88 or newer is required by the locked dependency set.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

The Docker image is a multi-stage build: `rust:1-alpine` (musl) produces a static binary that is
copied into `docker:27-cli`, because the agent needs the Docker CLI and must stay small.

## Container image

### Install on a node

Copy `setup.sh` and `compose.yaml` to the node and run `sudo ./setup.sh`. The script creates the
required directories and Docker network, writes `.env`, pulls the image and prints the claim code.
For Docker Desktop on macOS or Windows, use `compose.desktop.yaml`; see
[GUIDE.sv.md](GUIDE.sv.md) for the full walkthrough.

`.github/workflows/publish-image.yml` builds the Docker image on every pull request and
smoke-tests it without pushing. Pushes to `main` publish `main` and `sha-<commit>` tags, and a
stable semantic-version tag such as `v0.2.0` publishes `0.2.0`, `0.2`, `0`, `sha-<commit>` and
`latest` to `ghcr.io/cozilic/craftpilot-runtime-agent-rs`. A manual run requires an explicit
version and publishes that tag plus the commit tag.

START/STOP require the Docker socket, the game network and the server data root:

```sh
docker run -d --name craftpilot-runtime-agent-rs \
  --network craftpilot_default \
  -e CRAFTPILOT_MASTER_URL=https://runtime-tls \
  -e CRAFTPILOT_DOCKER_NETWORK=craftpilot-servers \
  -e CRAFTPILOT_DOCKER_SERVER_DATA_ROOT=/host/path/to/server-data \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v /host/path/to/server-data:/srv/craftpilot/servers \
  -v craftpilot_rs_agent_state:/var/lib/craftpilot-agent \
  craftpilot-runtime-agent-rs:dev

# The agent must also see the game network it starts containers on.
docker network connect craftpilot-servers craftpilot-runtime-agent-rs
```

## Security

The agent will mount the Docker socket, which is effectively host-root. It is a privileged
service: dedicated host, private state volume, no exposed Docker socket, and no host-shell
execution — Docker is only ever invoked with fixed argv. Keep credentials out of logs, images and
git. This repository is the rewrite; until it reaches parity, the Python agent remains the
reference implementation and the one to deploy.
