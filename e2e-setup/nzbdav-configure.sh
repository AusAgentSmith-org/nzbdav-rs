#!/usr/bin/env bash
# Wire a Usenet provider into nzbdav via POST /api/servers.
# Idempotent: if the host already exists, skip.

set -euo pipefail

BASE="${BASE_URL:-http://nzbdav:8080}"
KEY="${API_KEY:-}"

require() {
    if [ -z "${!1:-}" ]; then
        echo "ERROR: env $1 is required" >&2
        exit 1
    fi
}
require USENET_HOST
require USENET_USER
require USENET_PASS

PORT="${USENET_PORT:-563}"
SSL="${USENET_SSL:-true}"
CONNS="${USENET_CONNECTIONS:-20}"

echo "[configure] Waiting for nzbdav API at $BASE..."
for _ in $(seq 1 60); do
    if curl -sf "$BASE/api?mode=version&apikey=$KEY" >/dev/null 2>&1; then break; fi
    sleep 1
done

# Auth: POST /api/servers is NOT key-protected today, but pass the key anyway
# in case that changes.
EXISTING=$(curl -sf -H "X-Api-Key: $KEY" "$BASE/api/servers" 2>/dev/null || echo '[]')
if echo "$EXISTING" | python3 -c "
import sys,json
data=json.load(sys.stdin)
host='$USENET_HOST'
for s in data:
    if s.get('host')==host:
        print('found'); sys.exit(0)
print('missing')
" | grep -q found; then
    echo "[configure] Usenet host $USENET_HOST already configured — skipping."
    exit 0
fi

BODY=$(python3 -c "
import json,os
print(json.dumps({
    'id': '',
    'name': 'e2e-provider',
    'host': os.environ['USENET_HOST'],
    'port': int('$PORT'),
    'ssl': '$SSL'.lower() in ('true','1','yes'),
    'ssl_verify': True,
    'username': os.environ['USENET_USER'],
    'password': os.environ['USENET_PASS'],
    'connections': int('$CONNS'),
    'priority': 0,
    'enabled': True,
}))
")

echo "[configure] Adding Usenet provider $USENET_HOST..."
RESP=$(curl -sf -X POST -H 'Content-Type: application/json' \
    -H "X-Api-Key: $KEY" \
    -d "$BODY" \
    "$BASE/api/servers" 2>&1 || true)
SID=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('id',''))" 2>/dev/null || echo "")
if [ -z "$SID" ]; then
    echo "[configure] FAILED to add server. Response: $RESP" >&2
    exit 1
fi
echo "[configure] Server added: id=$SID"

# Optional sanity test — POST /api/servers/:id/test
echo "[configure] Testing connection..."
TEST_RESP=$(curl -s -X POST -H "X-Api-Key: $KEY" \
    "$BASE/api/servers/$SID/test" 2>&1 || true)
echo "[configure] test response: $TEST_RESP"
echo "[configure] Done."
