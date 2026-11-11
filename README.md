# CraftPilot Runtime Agent (Rust)

A Rust rewrite of the [CraftPilot runtime agent](https://github.com/cozilic/craftpilot-runtime-agent),
built as a side project. The agent connects a trusted host to a CraftPilot master, manages
Minecraft containers through the host's Docker daemon, and speaks the **same HTTP contract** as
the Python agent — no master changes are required to run it side by side.

> **Status: skeleton (phase 1).** Enrollment, heartbeat, command claiming and acknowledgement
> work. No Minecraft operation is implemented yet: every claimed command is acknowledged with
> `success=false` / `RUNTIME_ERROR`, so the panel shows a failure instead of a silent lease.

## Implemented

- Self-enrollment: files a request, polls until a sysadmin approves it under
  **Ops & log → Runtime nodes**, stores the credential and completes the request.
- Replacement enrollment when the master rejects the credential (node deleted/disabled), and a
  fresh request when a pending/expired enrollment is no longer known to the master.
- Private state file (`state.json`, `0700` directory / `0600` file) with the node ID, credential,
  pending enrollment and a strictly increasing heartbeat sequence, written atomically.
- Heartbeat on startup, then `commands/claim` every poll interval (the master counts a claim as a
  heartbeat, so each request consumes a new sequence number).
- Acknowledgement with lease token, including the "lease expired" path.
- Signed-off configuration validation: HTTPS-only master origin with no path/query/credentials,
  poll interval 5–120 s, absolute data root.

## Not implemented yet

`snapshot`, `start`, `stop`, `console`, file operations and migrations. Telemetry/`servers/report`
is not sent either, so a node running this agent appears online but reports no server data.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `CRAFTPILOT_MASTER_URL` | `https://craftpilot.se` | Master origin (HTTPS, no path). |
| `CRAFTPILOT_AGENT_STATE_DIR` | `/var/lib/craftpilot-agent` | Private state directory. |
| `CRAFTPILOT_SERVER_DATA_ROOT` | `/srv/craftpilot/servers` | Server data root (used once operations land). |
| `CRAFTPILOT_NODE_ID` | – | Only to attach an existing node. |
| `CRAFTPILOT_REGISTRATION_TOKEN` | – | One-time token for a re-enabled node. |
| `CRAFTPILOT_POLL_SECONDS` | `5` | Command poll interval, 5–120. |
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

## Security

The agent will mount the Docker socket, which is effectively host-root. It is a privileged
service: dedicated host, private state volume, no exposed Docker socket, and no host-shell
execution — Docker is only ever invoked with fixed argv. Keep credentials out of logs, images and
git. This repository is the rewrite; until it reaches parity, the Python agent remains the
reference implementation and the one to deploy.
