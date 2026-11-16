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
RUN apk add --no-cache ca-certificates
COPY --from=build /src/target/release/craftpilot-runtime-agent /usr/local/bin/craftpilot-runtime-agent

ENV CRAFTPILOT_AGENT_STATE_DIR=/var/lib/craftpilot-agent
VOLUME ["/var/lib/craftpilot-agent"]
ENTRYPOINT ["craftpilot-runtime-agent"]
