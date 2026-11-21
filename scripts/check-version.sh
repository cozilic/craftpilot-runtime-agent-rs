#!/usr/bin/env bash
# Kontrollerar att Cargo, panelen, compose-filerna och dokumentationen pekar på
# samma release. Körs av CI och kan köras lokalt: bash scripts/check-version.sh
set -Eeuo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)
panel_version=$(sed -n 's:.*<Version>\([^<]*\)</Version>.*:\1:p' \
  desktop/CraftPilot.AgentSetup/CraftPilot.AgentSetup.csproj | head -n1)
minor="${version%.*}"
failures=0

fail() {
  printf 'FEL: %s\n' "$*" >&2
  failures=$((failures + 1))
}

[ -n "$version" ] || fail "Cargo.toml saknar version"
[ -n "$panel_version" ] || fail "panelens csproj saknar <Version>"
[ "$panel_version" = "$version" ] ||
  fail "panelen är $panel_version men agenten är $version"

# Endast utgivningstaggar granskas; :dev, :main och :sha-<commit> är avsiktliga
# utvecklings- eller digesttaggar.
check_tag() {
  local where="$1"
  local tag="$2"
  case "$tag" in
    '' )
      fail "$where saknar en agentimage"
      return
      ;;
  esac
  case "$tag" in
    [0-9]*.[0-9]*) ;;
    *) return ;;
  esac
  [ "$tag" = "$minor" ] || fail "$where pekar på :$tag, väntade :$minor"
}

for file in compose.yaml compose.desktop.yaml; do
  tag=$(grep -oE 'craftpilot-runtime-agent-rs:[A-Za-z0-9._-]+' "$file" |
    head -n1 | cut -d: -f2 || true)
  check_tag "$file" "$tag"
done

for file in README.md INSTALL.sv.md GUIDE.sv.md desktop/README.md; do
  while IFS= read -r tag; do
    check_tag "$file" "$tag"
  done < <(grep -oE 'craftpilot-runtime-agent-rs:[A-Za-z0-9._-]+' "$file" |
    cut -d: -f2 || true)
done

if [ "$failures" -gt 0 ]; then
  printf 'Versionskontrollen hittade %s fel.\n' "$failures" >&2
  exit 1
fi

printf 'Versionerna stämmer: agent %s, panel %s, image %s.\n' \
  "$version" "$panel_version" "$minor"
