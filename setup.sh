#!/usr/bin/env bash
set -Eeuo pipefail

# Installerar och startar CraftPilot Runtime Agent på en Linux-värd.
# Körs med sudo första gången: sudo ./setup.sh
# Kan köras igen för att hämta en ny image och starta om agenten.

INSTALL_DIR="/opt/craftpilot-agent"
DATA_DIR="/srv/craftpilot/servers"
NETWORK="craftpilot-servers"
CONTAINER="craftpilot-runtime-agent-rs"
IMAGE="ghcr.io/cozilic/craftpilot-runtime-agent-rs:0.7"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_SRC="$SCRIPT_DIR/compose.yaml"
ENV_FILE="$INSTALL_DIR/.env"

usage() {
  cat <<'EOF'
Användning: sudo ./setup.sh

Skapar och startar en CraftPilot Runtime Agent på den här värden.

Filer:
  compose.yaml   installeras som /opt/craftpilot-agent/compose.yaml
  .env           skapas i /opt/craftpilot-agent/.env om den inte finns

Kör om samma kommando för att hämta senaste imagen och starta om agenten.
EOF
}

log() {
  printf '\n==> %s\n' "$*"
}

fail() {
  printf 'FEL: %s\n' "$*" >&2
  exit 1
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
  '')
    ;;
  *)
    fail "okänt argument: $1 (använd --help)"
    ;;
esac

[ -f "$COMPOSE_SRC" ] || fail "compose.yaml saknas bredvid setup.sh"
command -v docker >/dev/null 2>&1 || fail "Docker saknas. Installera Docker 27 eller senare."
docker info >/dev/null 2>&1 || fail "kan inte ansluta till Docker. Kör med sudo eller kontrollera Docker-tjänsten."
docker compose version >/dev/null 2>&1 || fail "Docker Compose saknas. Installera Compose-plugin."

log "skapar kataloger"
mkdir -p "$INSTALL_DIR" "$DATA_DIR" || fail "kunde inte skapa $INSTALL_DIR eller $DATA_DIR. Kör med sudo."

log "säkerställer Docker-nätverket $NETWORK"
if ! docker network inspect "$NETWORK" >/dev/null 2>&1; then
  docker network create "$NETWORK" >/dev/null
fi

if [ ! -f "$ENV_FILE" ]; then
  log "skapar $ENV_FILE"
  umask 077
  cat > "$ENV_FILE" <<EOF
CRAFTPILOT_MASTER_URL=https://craftpilot.se
CRAFTPILOT_SERVER_DATA_ROOT=$DATA_DIR
CRAFTPILOT_DOCKER_SERVER_DATA_ROOT=$DATA_DIR
CRAFTPILOT_POLL_SECONDS=5
RUST_LOG=info
EOF
  chmod 600 "$ENV_FILE"
else
  log "behåller befintlig $ENV_FILE"
fi

# Om en äldre .env saknar någon av de obligatoriska raderna lägger vi bara
# till den som saknas. Egna värden skrivs aldrig över.
ensure_env() {
  local key="$1"
  local value="$2"

  if ! grep -qE "^${key}=" "$ENV_FILE"; then
    printf '%s=%s\n' "$key" "$value" >> "$ENV_FILE"
  fi
}

ensure_env CRAFTPILOT_MASTER_URL "https://craftpilot.se"
ensure_env CRAFTPILOT_SERVER_DATA_ROOT "$DATA_DIR"
ensure_env CRAFTPILOT_DOCKER_SERVER_DATA_ROOT "$DATA_DIR"
ensure_env CRAFTPILOT_POLL_SECONDS "5"
ensure_env RUST_LOG "info"

if [ ! -f "$INSTALL_DIR/compose.yaml" ]; then
  log "installerar compose.yaml"
  cp "$COMPOSE_SRC" "$INSTALL_DIR/compose.yaml"
  chmod 644 "$INSTALL_DIR/compose.yaml"
else
  log "behåller befintlig compose.yaml"
fi

cd "$INSTALL_DIR"

log "hämtar agentimagen"
if ! docker compose pull; then
  cat >&2 <<'EOF'

Kunde inte hämta agentimagen.
Om GHCR-paketet fortfarande är privat:
  sudo docker login ghcr.io -u DITT_GITHUB_ANVÄNDARNAMN
Använd en classic personal access token med read:packages och kör setup.sh igen.
När paketet är publikt behövs ingen inloggning.
EOF
  exit 1
fi

log "startar agenten"
docker compose up -d

for _ in $(seq 1 20); do
  running="$(docker inspect -f '{{.State.Running}}' "$CONTAINER" 2>/dev/null || true)"
  [ "$running" = "true" ] && break
  sleep 2
done

running="$(docker inspect -f '{{.State.Running}}' "$CONTAINER" 2>/dev/null || true)"
if [ "$running" != "true" ]; then
  docker compose logs --no-color --tail 80 agent >&2 || true
  fail "agenten startade inte. Se loggen ovan."
fi

# Claim-koden skrivs en gång vid första anslutningen. Vänta kort på att
# agenten hinner göra sin första heartbeat och skriva loggraden.
claim=""
for _ in $(seq 1 30); do
  # Rust-agenten skriver ANSI-färgkoder även när Docker fångar loggen;
  # --no-color tar bara bort Compose egna koder. Utan strippning matchar
  # aldrig "claim_code=CP-..." och setupen tror att ingen kod finns.
  logs="$(docker compose logs --no-color --tail 80 agent 2>&1 | sed -E 's/\x1B\[[0-9;]*[A-Za-z]//g' || true)"
  claim="$(printf '%s\n' "$logs" | grep -oE 'claim_code=CP-[A-Z0-9]{5}-[A-Z0-9]{5}' | tail -n 1 | cut -d= -f2 || true)"
  [ -n "$claim" ] && break
  sleep 2
done

printf '\n'
if [ -n "$claim" ]; then
  printf 'Agenten kör.\n\n'
  printf 'Claim-kod: %s\n\n' "$claim"
  printf 'Öppna https://craftpilot.se -> Min nod och klistra in koden.\n'
  printf 'Koden gäller i 30 minuter.\n'
else
  printf 'Agenten kör, men ingen ny claim-kod hittades.\n'
  printf 'Den kan redan vara kopplad. Kontrollera med:\n\n'
  printf '  cd %s && docker compose logs --tail 80 agent\n\n' "$INSTALL_DIR"
fi

printf '\nLoggar: cd %s && docker compose logs -f agent\n' "$INSTALL_DIR"
printf 'Uppdatera: cd %s && docker compose pull && docker compose up -d\n' "$INSTALL_DIR"
