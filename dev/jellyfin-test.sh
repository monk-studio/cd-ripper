#!/usr/bin/env bash
# Throwaway Jellyfin for testing library import (CDR-11).
#
#   dev/jellyfin-test.sh up     start it, run the setup wizard, create a music
#                               library over rips/lib-test and an API key
#   dev/jellyfin-test.sh down   remove the container and its data
#
# Credentials land in rips/jellyfin-test/credentials.env (git-ignored); the
# admin password is generated, never fixed in this script.
set -euo pipefail

cd "$(dirname "$0")/.."
NAME=cdr-jellyfin-test
IMAGE=jellyfin/jellyfin:12.0
PORT=8099
URL="http://127.0.0.1:$PORT"
DATA=rips/jellyfin-test
LIBRARY=rips/lib-test
CREDS="$DATA/credentials.env"
AUTH='MediaBrowser Client="CDR", Device="dev", DeviceId="cdr-dev", Version="0.1.0"'

api() { # method path [json]
  curl -sf -X "$1" "$URL$2" -H "Authorization: $AUTH${TOKEN:+, Token=\"$TOKEN\"}" \
    -H 'Content-Type: application/json' ${3:+-d "$3"}
}

up() {
  mkdir -p "$DATA/config" "$DATA/cache" "$LIBRARY"
  if ! docker ps -a --format '{{.Names}}' | grep -qx "$NAME"; then
    docker run -d --name "$NAME" -p "127.0.0.1:$PORT:8096" \
      -v "$PWD/$DATA/config:/config" -v "$PWD/$DATA/cache:/cache" \
      -v "$PWD/$LIBRARY:/music:ro" "$IMAGE" >/dev/null
  fi
  docker start "$NAME" >/dev/null
  for _ in $(seq 60); do curl -sf "$URL/System/Info/Public" >/dev/null && break; sleep 1; done

  if [ -f "$CREDS" ]; then
    echo "already set up: $CREDS"
    return
  fi
  local user=cdr-admin password
  password=$(openssl rand -hex 16)
  api POST /Startup/Configuration '{"UICulture":"en-US","MetadataCountryCode":"US","PreferredMetadataLanguage":"en"}'
  api GET /Startup/User >/dev/null
  api POST /Startup/User "{\"Name\":\"$user\",\"Password\":\"$password\"}"
  api POST /Startup/RemoteAccess '{"EnableRemoteAccess":true,"EnableAutomaticPortMapping":false}'
  api POST /Startup/Complete

  TOKEN=$(api POST /Users/AuthenticateByName "{\"Username\":\"$user\",\"Pw\":\"$password\"}" |
    python3 -I -c 'import json,sys; print(json.load(sys.stdin)["AccessToken"])')
  api POST "/Library/VirtualFolders?name=Music&collectionType=music&paths=%2Fmusic&refreshLibrary=false" '{"LibraryOptions":{}}'
  api POST "/Auth/Keys?app=CDR"
  local key
  key=$(api GET /Auth/Keys |
    python3 -I -c 'import json,sys; print(next(k["AccessToken"] for k in json.load(sys.stdin)["Items"] if k["AppName"]=="CDR"))')

  umask 077
  printf 'JELLYFIN_URL=%s\nJELLYFIN_ADMIN_USER=%s\nJELLYFIN_ADMIN_PASSWORD=%s\nCDR_JELLYFIN_API_KEY=%s\n' \
    "$URL" "$user" "$password" "$key" >"$CREDS"
  echo "Jellyfin test server ready at $URL; credentials in $CREDS"
}

down() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  rm -rf "$DATA"
  echo "removed $NAME and $DATA"
}

"${1:-up}"
