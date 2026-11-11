# CraftPilot Runtime Agent (Rust)

A Rust rewrite of the [CraftPilot runtime agent](https://github.com/cozilic/craftpilot-runtime-agent),
built as a side project. The agent connects a trusted host to a CraftPilot master, manages
Minecraft containers through the host's Docker daemon, and speaks the **same HTTP contract** as
the Python agent — no master changes are required to run it side by side.

> **Status: phase 4.** Enrollment, heartbeat, command claiming, acknowledgement, START, STOP, the
> one-time initial SNAPSHOT and console commands work. File operations and migrations are still
> acknowledged as `RUNTIME_ERROR`, so the panel shows a failure instead of a silent lease.

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
- **STOP** with the `force` flag: graceful `docker stop --time <grace>` otherwise
  `docker kill --signal=KILL`, answering `STOPPED` / `ALREADY_STOPPED`; a container that loses its
  labels is never touched.
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

File operations and migrations. Telemetry/`servers/report` is not sent
  either, so a node running this agent appears online but reports no server data — the master
  therefore keeps a started server at `STARTING` until something else reports it. STOP sends
  SIGTERM to the JVM rather than Minecraft's own `stop` command over RCON; a server that needs an
  explicit save-on-stop gets the JVM shutdown hook only.

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

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

The Docker image is a multi-stage build: `rust:1-alpine` (musl) produces a static binary that is
copied into `docker:27-cli`, because the agent needs the Docker CLI and must stay small.

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
