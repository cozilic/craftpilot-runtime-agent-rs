# syntax=docker/dockerfile:1

# Static musl build, then a runtime image with the Docker CLI the agent drives.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
# Taggens namn kan ges som --build-arg; annars används git describe (om .git finns).
ARG CRAFTPILOT_AGENT_VERSION
RUN cargo build --release --locked

FROM docker:27-cli
RUN apk add --no-cache ca-certificates curl

# frp 0.72.0 — samma version som masterns tunnel-hub. Arkiven verifieras mot
# release-checksummorna innan frpc kopieras in; ingen version löses vid bygget.
ARG FRP_VERSION=0.72.0
ARG TARGETARCH
RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) frp_sha256=daf719b32ca2778770bb92955e231a07414d754378b9af328aa2642862479804 ;; \
      arm64) frp_sha256=02180b857480302fd934e9ddbd50fe2f73229f99062f88756abc6de9df5e95cf ;; \
      *) echo "unsupported architecture for frpc: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/frp.tar.gz \
      "https://github.com/fatedier/frp/releases/download/v${FRP_VERSION}/frp_${FRP_VERSION}_linux_${TARGETARCH}.tar.gz"; \
    echo "${frp_sha256}  /tmp/frp.tar.gz" | sha256sum -c -; \
    tar -xzf /tmp/frp.tar.gz -C /tmp; \
    install -m 0755 "/tmp/frp_${FRP_VERSION}_linux_${TARGETARCH}/frpc" /usr/local/bin/frpc; \
    rm -rf /tmp/frp.tar.gz "/tmp/frp_${FRP_VERSION}_linux_${TARGETARCH}"
COPY --from=build /src/target/release/craftpilot-runtime-agent /usr/local/bin/craftpilot-runtime-agent

ENV CRAFTPILOT_AGENT_STATE_DIR=/var/lib/craftpilot-agent
VOLUME ["/var/lib/craftpilot-agent"]
ENTRYPOINT ["craftpilot-runtime-agent"]
