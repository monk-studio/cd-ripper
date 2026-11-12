#!/usr/bin/env bash
# Throwaway Navidrome for testing library import (CDR-11).
#
#   dev/navidrome-test.sh up     start it over rips/lib-test and create an admin
#   dev/navidrome-test.sh down   remove the container and its data
#
# Startup, scheduled and file-watcher scans are all off, so an album only shows
# up when cdr asks for a scan. Credentials land in
# rips/navidrome-test/credentials.env (git-ignored); the password is generated.
set -euo pipefail

cd "$(dirname "$0")/.."
NAME=cdr-navidrome-test
IMAGE=deluan/navidrome:0.64.2
PORT=4599
URL="http://127.0.0.1:$PORT"
DATA=rips/navidrome-test
LIBRARY=rips/lib-test
CREDS="$DATA/credentials.env"

up() {
  mkdir -p "$DATA/data" "$LIBRARY"
  if ! docker ps -a --format '{{.Names}}' | grep -qx "$NAME"; then
    docker run -d --name "$NAME" -p "127.0.0.1:$PORT:4533" \
      -e ND_SCANNER_SCANONSTARTUP=false -e ND_SCANNER_SCHEDULE=0 -e ND_SCANNER_WATCHERWAIT=0 \
      -v "$PWD/$DATA/data:/data" -v "$PWD/$LIBRARY:/music:ro" "$IMAGE" >/dev/null
  fi
  docker start "$NAME" >/dev/null
  for _ in $(seq 60); do curl -sf "$URL/ping" >/dev/null && break; sleep 1; done

  if [ -f "$CREDS" ]; then
    echo "already set up: $CREDS"
    return
  fi
  local user=cdr-admin password
  password=$(openssl rand -hex 16)
  curl -sf -X POST "$URL/auth/createAdmin" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$user\",\"password\":\"$password\"}" >/dev/null

  umask 077
  printf 'CDR_NAVIDROME_URL=%s\nCDR_NAVIDROME_USER=%s\nCDR_NAVIDROME_PASSWORD=%s\n' \
    "$URL" "$user" "$password" >"$CREDS"
  echo "Navidrome test server ready at $URL; credentials in $CREDS"
}

down() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  rm -rf "$DATA"
  echo "removed $NAME and $DATA"
}

"${1:-up}"
