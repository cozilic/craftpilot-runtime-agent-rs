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
> RCON, CANCEL_START, DELETE_SERVER, the one-time initial SNAPSHOT, console commands, the file
> manager, encrypted runtime-node migrations and server telemetry work, file downloads stream to
> the master, and the remote file tree is indexed and synced in bounded batches.

## Implemented

- Self-enrollment: files a request, polls until a sysadmin approves it under
  **Ops & log → Runtime nodes**, stores the credential and completes the request.
  The claim code lives 30 minutes on the master, so a request that keeps waiting
  renews it every 25 minutes (terminal and `identity.conf` show the fresh code).
- Replacement enrollment when the master rejects the credential (node deleted/disabled) — also on
  startup, so a restart after the node was removed always ends with a fresh claim code — and a fresh
  request when a pending/expired enrollment is no longer known to the master. A request a sysadmin
  rejected is never replaced: the agent keeps the rejection in its state and waits 24 hours (the
  lifetime of a request) before asking again, so the inbox does not fill up with duplicates.
- Private state file (`state.json`, `0700` directory / `0600` file) with the node ID, credential,
  pending enrollment and a strictly increasing heartbeat sequence, written atomically.
- Heartbeat on startup, then `commands/claim` every poll interval (the master counts a claim as a
  heartbeat, so each request consumes a new sequence number).
- The build version is baked in by `build.rs` — `CRAFTPILOT_AGENT_VERSION` (CI tag or Docker
  `--build-arg`) or `git describe --tags --always`, normalized to the master's allowed format —
  and is sent on enrollment, registration, heartbeat and every claim, so **Ops & log → Runtime
  nodes** shows which agent a node actually runs (not a shared placeholder).
- Acknowledgement with lease token, including the "lease expired" path.
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
- **CANCEL_START** (`cancel_start`): aborts a start that hangs in `STARTING`. The master sends no
  `force` for this command, so the agent always SIGKILLs the container and never tries RCON — a
  start that has hung cannot answer a graceful stop — and answers `START_CANCELLED`, or
  `ALREADY_STOPPED` when the container is already gone or stopped. The master moves the server to
  `CANCELLED` only after that ack.
- **DELETE_SERVER** (`delete_server`): removes the container (`docker rm --force`, only after the
  ownership label check; a missing container is a no-op) and then `<data root>/<server uuid>` and
  the stored RCON password, answering `SERVER_DELETED`. A symlinked or escaping data directory is
  refused and a missing one is a successful no-op, so a re-delivered command never fails.
- **TELEMETRY** (`servers` + `servers/report`): every 30 s, and immediately after a start/stop,
  the agent reports its assigned servers: status from `docker inspect` (missing → `OFFLINE`,
  running → `ONLINE`, exited 0 → `OFFLINE`, non-zero → `CRASHED`), CPU/memory from
  `docker stats --no-stream`, raw network counters (`network_rx_bytes`/`network_tx_bytes`) from the
  Docker Engine API `/containers/{id}/stats` through bollard, summed over every interface (the
  CLI's `NetIO` is rounded, so it cannot be used; a stopped container sends no counters at all),
  `players_max` from `server.properties` (`max-players`, default 20),
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
- **ARTIFACT INSTALL** (`install_artifact`): the master stages and verifies the server artifact
  (the native `pumpkin` binary or the jar the startup command runs) and queues the install. The
  agent refuses a running container, validates the payload filename/size/digest, streams the blob
  from its leased `commands/{id}/artifact` URL (bounded by `CRAFTPILOT_MAX_UPLOAD_BYTES`), checks
  the byte count and SHA-256 while streaming, writes `.cp-artifact-*` in the server root,
  `fsync`s it, sets `0755` for the native binary (`0644` for a jar) and replaces the target with
  one atomic rename. Answers `ARTIFACT_INSTALLED` with `"<filename> <bytes> <sha256>"` (the exact
  triple the master validates), or `ARTIFACT_DIGEST_MISMATCH`, `ARTIFACT_TOO_LARGE`,
  `ARTIFACT_TARGET_BUSY` / `ARTIFACT_INVALID`. The panel's "installed" flag remains the master's
  next server report, not the ack.
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
- **NODE SFTP** (`features: ["sftp"]`): the agent runs its own SFTPGo container on the
  node's Docker network and applies the full state from `GET /api/runtime/nodes/{node}/sftp`
  — one key-only account per `u<id>`, one virtual folder per server mounted at `/<slug>`,
  and every folder is validated against the server data root (UUID, directory, no symlink
  escape) before it is mapped. Accounts and folders that leave the master's state are
  removed. Only when the SFTPGo admin API answers does the agent report `sftp` and the SSH
  host-key fingerprint (Ed25519) in heartbeat/claim; a stopped or broken SFTPGo stops being
  advertised instead of leaving a dead endpoint in the panel. SFTPGo serves **only** that
  Ed25519 host key, so the fingerprint in the panel is the same one every client sees — a
  client that would otherwise negotiate RSA cannot show a different value. The admin API is
  never published — only the SFTP port is.
- **Bounded container logs**: every game container runs with the Docker `json-file` driver
  capped at 10 MiB × 5 files, so a chatty server can no longer fill the node's disk. Console
  delivery still uses a timestamp cursor and keeps its 400-line window.
- **Verified starts**: after `docker run` the agent waits up to five seconds and reports the
  exit code instead of acknowledging STARTED for a JVM that died on a bad jar, the wrong Java
  version or the EULA.
- **Player counts from the server-list ping**: telemetry asks the game container for its status
  and reports `players.online`, with console join/leave lines as a fallback while the port is
  still starting.
- **Event-driven file tree**: a recursive watcher (inotify, ReadDirectoryChangesW or kqueue)
  rescans after host changes with a five-second debounce; a 30-second scan remains as a safety
  net for filesystems without watcher support.
- Signed-off configuration validation: a required HTTPS-only master origin with no
  path/query/credentials, poll interval 5–120 s, absolute data root, Docker network name, stop
  grace 0–45 s.

## Not implemented yet

A download larger than `CRAFTPILOT_MAX_UPLOAD_BYTES` is refused before anything is streamed.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `CRAFTPILOT_MASTER_URL` | – (required) | Master origin (HTTPS, no path). The agent refuses to start without it. |
| `CRAFTPILOT_RS_AGENT_IMAGE_TAG` | `0.10` | Image tag the compose files run. Both are read with `--env-file`, so a new image needs no new Desktop build. |
| `CRAFTPILOT_AGENT_STATE_DIR` | `/var/lib/craftpilot-agent` | Private state directory. |
| `CRAFTPILOT_SERVER_DATA_ROOT` | `/srv/craftpilot/servers` | Server data root (used once operations land). |
| `CRAFTPILOT_NODE_ID` | – | Only to attach an existing node. |
| `CRAFTPILOT_REGISTRATION_TOKEN` | – | One-time token for a re-enabled node. |
| `CRAFTPILOT_POLL_SECONDS` | `5` | Command poll interval, 5–120. |
| `CRAFTPILOT_HOST_ADDRESS` | auto-detected | Address the agent reports at enrollment (the panel pre-fills it in the claim form). Detected as the machine's LAN IP when the agent runs directly on the host; never auto-detected inside a container (the bridge IP is useless), so set it explicitly there — or for a public IP or a host with several interfaces. |
| `CRAFTPILOT_DOCKER_NETWORK` | `craftpilot-servers` | Network the game containers join; a spec for another network is refused. |
| `CRAFTPILOT_ALLOWED_IMAGES` | master Java images + `craftpilot-pumpkin:latest` | Exact image allowlist. |
| `CRAFTPILOT_STOP_GRACE_SECONDS` | `30` | `docker stop` timeout before the daemon sends SIGKILL, 0–45. |
| `CRAFTPILOT_DOCKER_SERVER_DATA_ROOT` | – | Host-side data path for the volume when it differs from the in-container mount (Docker Desktop). |
| `CRAFTPILOT_DATA_GID` | `33` | Group that staged server files are shared with (Minecraft's GID in the panel images). |
| `CRAFTPILOT_MAX_EDIT_BYTES` | `4194304` | Largest file the editor may read or write (same as the master's `server.files.max_edit_bytes`). |
| `CRAFTPILOT_MAX_UPLOAD_BYTES` | `2147483648` | Largest upload accepted from the master (same as `server.files.max_upload_bytes`). |
| `CRAFTPILOT_SFTP_ENABLED` | `true` | Runs the node's own SFTPGo (see [Node SFTP](#node-sftp)). `false` never reports `sftp`, so the panel hides node endpoints. |
| `CRAFTPILOT_SFTP_IMAGE` | `drakkan/sftpgo:v2.6.6` | Exact SFTPGo image; an upgrade recreates the container but keeps its volumes. |
| `CRAFTPILOT_SFTP_PORT` | `2222` | Preferred host port for SFTP (the container port is always 2022). When it is taken on the host, the agent publishes on the next free one and reports it to the master as `sftp_bound_port`. The admin API is never published. |
| `CRAFTPILOT_SFTP_ADMIN_URL` | `http://craftpilot-sftpgo:8080/` | Internal admin API origin. Must be reachable from the agent; the Docker network name is used by default. |
| `CRAFTPILOT_SFTP_CONTAINER` | `craftpilot-sftpgo` | Container name. Config and home/data live in the volumes `<name>-config` and `<name>-data`. |
| `CRAFTPILOT_SFTP_FILE_UID` | owner of `<data root>/<server uuid>` | Owner of files SFTPGo creates. Derived from the server directory so the game container can write them on any node; set it only to override a node with unusual ownership. |
| `CRAFTPILOT_SFTP_FILE_GID` | owner of `<data root>/<server uuid>` | Group of files SFTPGo creates (same derivation and override). Fallback is `33` when no unix owner can be read. |
| `RUST_LOG` | `info` | Log filter. Console text, credentials and RCON passwords are never logged. |

There is no built-in master default: a bare `docker run` without
`CRAFTPILOT_MASTER_URL` exits with a configuration error instead of enrolling against
production.

## Node SFTP

The master owns permission; the agent owns location. A server that runs on a node can be
reached on the node's own SFTP endpoint instead of the master's, so the files never have to
be mirrored (`docs/RUNTIME-NODE-SFTP.md` in the CraftPilot repository).

The agent runs one SFTPGo container per node (not per server), joined to
`CRAFTPILOT_DOCKER_NETWORK`, with root inside the container because the server directories
belong to the game containers. Its SQLite database and SSH host keys live in the Docker
volume `<CRAFTPILOT_SFTP_CONTAINER>-config`, the (empty) user homes in
`<CRAFTPILOT_SFTP_CONTAINER>-data`, and the server data root is mounted read-write at
`/srv/craftpilot/servers`. Only the SFTP port is published; the admin API — and the admin
password, which is generated once and stored in the agent's `0600` state file — never leaves
the Docker network.

Every poll:

1. the agent ensures the container runs (creating or recreating it when the image or port
   changed, with volumes kept),
2. it reads `GET /api/runtime/nodes/{node}/sftp` and applies the full state when the revision
   changed — folders first, then accounts, then removal of accounts/folders that left the
   state,
3. it reports `features: ["sftp"]` and the Ed25519 host-key fingerprint only when the admin
   API answers, so a stopped SFTPGo is never advertised as a working endpoint.

SFTPGo is started with
`SFTPGO_SFTPD__HOST_KEYS=/var/lib/sftpgo/id_ed25519`, so RSA/ECDSA host keys are neither
served nor reported. A container created by an older agent is recreated automatically (the
`craftpilot.sftpgo.config` label), while the volumes and the Ed25519 key are kept.

New files get the uid/gid of the server directory they are created in, read from the mounted data
root, so an SFTP upload stays writable by the game container even when the node's runtime user
differs from the panel images. `CRAFTPILOT_SFTP_FILE_UID`/`_GID` override the derivation. SFTPGo
has one uid/gid per account, so when a user's servers have different owners the first server's
owner is used and the node logs how many servers differ.

Accounts are key-only: the master's password channel is disabled and not implemented here, so
a user without a public key gets no account (never an account without an authentication
method). Folder paths are built from the master's `server_id` only after the directory is
confirmed to be a real directory under the data root — a symlink or a path outside the root
is skipped, never mounted.

If the SFTPGo admin password and its config volume get out of sync (for example after the
volume was deleted but the agent state kept), the admin API answers `401` and the agent logs
`sftpgo token request failed` without advertising `sftp`. Remove both volumes and restart the
agent to re-provision SFTPGo:

```sh
docker rm -f craftpilot-sftpgo
docker volume rm craftpilot-sftpgo-config craftpilot-sftpgo-data
```

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

The Docker build takes the version from the tag (the image has no `.git`, so pass it in):

```sh
docker build --build-arg CRAFTPILOT_AGENT_VERSION="$(git describe --tags --always)" \n  -t craftpilot-runtime-agent-rs:dev .
```

The Docker image is a multi-stage build: `rust:1-alpine` (musl) produces a static binary that is
copied into `docker:27-cli`, because the agent needs the Docker CLI and must stay small.

## Container image

### Install on a node

Copy `setup.sh` and `compose.yaml` to the node and run `sudo ./setup.sh`. The script creates the
required directories and Docker network, writes `.env`, pulls the image and prints the claim code.
For Docker Desktop on macOS or Windows, use `compose.desktop.yaml`; see
[GUIDE.sv.md](GUIDE.sv.md) for the full walkthrough.

### Update a node

`compose.yaml` and `compose.desktop.yaml` pin the image to the stable `0.10` tag, so a node follows
the 0.10.x releases instead of every push to `main`. `scripts/check-version.sh`, which CI runs,
fails when Cargo, the panel, the compose files and these docs disagree.

```sh
cd /opt/craftpilot-agent
sudo docker compose pull && sudo docker compose up -d
```

An existing install keeps its compose file, so change `:main` to `:0.10` there once
(`/opt/craftpilot-agent/compose.yaml`, or the panel's install directory on Windows) before pulling.

Re-running `sudo ./setup.sh` does the same. On Windows the panel's **Uppdatera** button
pulls the new image and restarts the container. The tag comes from
`CRAFTPILOT_RS_AGENT_IMAGE_TAG` (default `0.10`) in the compose files, so a new minor release is one
line in `.env` or `.env.desktop` instead of a new Desktop build.

`.github/workflows/publish-image.yml` builds the Docker image on every pull request and
smoke-tests it without pushing. Pushes to `main` publish `main` and `sha-<commit>` tags, and a
stable semantic-version tag such as `v0.2.0` publishes `0.2.0`, `0.2`, `0`, `sha-<commit>` and
`latest` to `ghcr.io/cozilic/craftpilot-runtime-agent-rs`. A manual run requires an explicit
version and publishes that tag plus the commit tag.

START/STOP require the Docker socket, the game network and the server data root. Telemetry also
reads the socket, for the container's network counters:

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
