#!/usr/bin/env bash
# Reproduces the exact API calls Usenet-Ultimate and UsenetStreamer make against nzbdav-rs.
# Validates the issue #2 fix: addfile field-name case sensitivity + nzbname param.
#
# Usage:
#   ./e2e-setup/test-issue2.sh [BASE_URL] [API_KEY]
#
# Defaults:
#   BASE_URL = http://localhost:8080
#   API_KEY  = testkey
#
# Run nzbdav-rs first:
#   docker compose -f docker-compose.debug.yml up -d
#   # or locally: ./target/release/nzbdav-app --port 8080 --api-key testkey

set -euo pipefail

BASE="${1:-http://localhost:8080}"
KEY="${2:-testkey}"
PASS=0
FAIL=0

GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m'

ok()   { echo -e "${GREEN}PASS${NC} $1"; PASS=$((PASS+1)); }
fail() { echo -e "${RED}FAIL${NC} $1"; FAIL=$((FAIL+1)); }

# Minimal valid NZB (single-file, no real content — enough for parse+enqueue)
NZB='<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <file poster="test@test.com" date="1700000000" subject="Test.Movie.2024.mkv (1/1)">
    <groups><group>alt.binaries.test</group></groups>
    <segments><segment bytes="1024" number="1">test-message-id@test</segment></segments>
  </file>
</nzb>'

# Write NZB to temp file
NZB_FILE=$(mktemp /tmp/test-XXXXXXXX.nzb)
echo "$NZB" > "$NZB_FILE"
trap 'rm -f "$NZB_FILE"' EXIT

echo "=== nzbdav-rs issue #2 regression tests ==="
echo "Base URL: $BASE"
echo ""

# ── 1. Version check (sanity) ────────────────────────────────────────────────
echo "── Connectivity ──"
RESP=$(curl -sf "$BASE/api?mode=version&apikey=$KEY" 2>/dev/null || echo '{}')
VER=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('version',''))" 2>/dev/null || echo "")
if [ -n "$VER" ]; then
    ok "GET /api?mode=version → version=$VER"
else
    fail "GET /api?mode=version → no response (is nzbdav-rs running at $BASE?)"
    echo "Aborting — cannot reach nzbdav-rs."
    exit 1
fi

# ── 2. addfile with lowercase 'nzbfile' (UsenetStreamer) ─────────────────────
echo ""
echo "── UsenetStreamer: addfile with field name 'nzbfile' (lowercase) ──"
RESP=$(curl -sf -X POST \
    "$BASE/api?mode=addfile&cat=movies&nzbname=UsenetStreamer-Test&apikey=$KEY" \
    -F "nzbfile=@$NZB_FILE;type=application/x-nzb+xml" 2>/dev/null || echo '{}')
STATUS=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('status',False))" 2>/dev/null || echo "False")
NZO_ID=$(echo "$RESP" | python3 -c "import sys,json; r=json.load(sys.stdin); print(r.get('nzo_ids',[''])[0])" 2>/dev/null || echo "")
if [ "$STATUS" = "True" ] && [ -n "$NZO_ID" ]; then
    ok "addfile nzbfile (lowercase) → nzo_id=$NZO_ID"
else
    fail "addfile nzbfile (lowercase) → status=$STATUS response=$RESP"
fi

# ── 3. addfile with 'nzbFile' (Usenet-Ultimate — capital F) ──────────────────
echo ""
echo "── Usenet-Ultimate: addfile with field name 'nzbFile' (capital F) ──"
RESP=$(curl -sf -X POST \
    "$BASE/api?mode=addfile&cat=movies&nzbname=UsenetUltimate-Test&apikey=$KEY" \
    -F "nzbFile=@$NZB_FILE;type=application/x-nzb+xml" 2>/dev/null || echo '{}')
STATUS=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('status',False))" 2>/dev/null || echo "False")
NZO_ID2=$(echo "$RESP" | python3 -c "import sys,json; r=json.load(sys.stdin); print(r.get('nzo_ids',[''])[0])" 2>/dev/null || echo "")
if [ "$STATUS" = "True" ] && [ -n "$NZO_ID2" ]; then
    ok "addfile nzbFile (capital F) → nzo_id=$NZO_ID2"
else
    fail "addfile nzbFile (capital F) → status=$STATUS response=$RESP"
    echo "  >>> This is the Usenet-Ultimate bug: 'nzbFile' field not recognised"
fi

# ── 4. Verify nzbname is used as job name ─────────────────────────────────────
echo ""
echo "── nzbname → job name mapping ──"
RESP=$(curl -sf "$BASE/api?mode=queue&apikey=$KEY" 2>/dev/null || echo '{}')
SLOTS=$(echo "$RESP" | python3 -c "import sys,json; print(json.dumps(json.load(sys.stdin).get('queue',{}).get('slots',[])))" 2>/dev/null || echo "[]")
NAMES=$(echo "$SLOTS" | python3 -c "import sys,json; [print(s.get('filename','')) for s in json.load(sys.stdin)]" 2>/dev/null || echo "")
if echo "$NAMES" | grep -q "UsenetUltimate-Test"; then
    ok "Queue slot filename = 'UsenetUltimate-Test' (nzbname used correctly)"
else
    fail "nzbname not used as job name — queue slots: $NAMES"
fi
if echo "$NAMES" | grep -q "UsenetStreamer-Test"; then
    ok "Queue slot filename = 'UsenetStreamer-Test' (nzbname used correctly)"
else
    fail "nzbname not used as job name — queue slots: $NAMES"
fi

# ── 5. history category filter (Usenet-Ultimate polls with category=) ─────────
echo ""
echo "── History category filter ──"
RESP=$(curl -sf "$BASE/api?mode=history&category=movies&apikey=$KEY" 2>/dev/null || echo '{}')
SLOTS_LEN=$(echo "$RESP" | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('history',{}).get('slots',[])))" 2>/dev/null || echo "0")
if [ "$SLOTS_LEN" -ge 1 ]; then
    ok "history?category=movies → $SLOTS_LEN slot(s) returned"
else
    fail "history?category=movies → 0 slots (category filter broken or nothing in history yet)"
fi

# ── 6. Usenet-Ultimate queue polling ─────────────────────────────────────────
echo ""
echo "── Usenet-Ultimate queue polling ──"
RESP=$(curl -sf "$BASE/api?mode=queue&output=json&apikey=$KEY" 2>/dev/null || echo '{}')
QUEUE_OK=$(echo "$RESP" | python3 -c "import sys,json; q=json.load(sys.stdin).get('queue',{}); print('ok' if 'slots' in q and 'noofslots' in q else 'missing')" 2>/dev/null || echo "missing")
if [ "$QUEUE_OK" = "ok" ]; then
    ok "queue response has slots + noofslots fields"
else
    fail "queue response missing expected fields: $RESP"
fi

# ── 7. Queue delete via nzo_id ────────────────────────────────────────────────
echo ""
echo "── Queue delete ──"
if [ -n "$NZO_ID" ]; then
    RESP=$(curl -sf "$BASE/api?mode=queue&name=delete&value=$NZO_ID&apikey=$KEY" 2>/dev/null || echo '{}')
    STATUS=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('status',False))" 2>/dev/null || echo "False")
    if [ "$STATUS" = "True" ]; then
        ok "queue delete nzo_id=$NZO_ID → ok"
    else
        fail "queue delete → status=$STATUS response=$RESP"
    fi
else
    echo "  (skipped — no nzo_id from step 2)"
fi

# ── 8. API key auth regression (issue #3 — dashboard 401 when key is set) ────
# Verifies the backend auth contract that the UI now relies on:
#   - request without key  → 401 (not 200 with JSON error)
#   - request with key     → 200 with well-formed JSON
# This catches any regression where auth middleware stops enforcing the key,
# which would silently break the localStorage-based key flow in the UI.
echo ""
echo "── API key auth (dashboard regression, issue #3) ──"
if [ -n "$KEY" ]; then
    HTTP_NO_KEY=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/api?mode=queue" 2>/dev/null)
    if [ "$HTTP_NO_KEY" = "401" ]; then
        ok "queue without apikey → 401 (auth enforced)"
    else
        fail "queue without apikey → HTTP $HTTP_NO_KEY (expected 401 — auth not enforced)"
    fi

    HTTP_WRONG_KEY=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/api?mode=queue&apikey=wrongkey" 2>/dev/null)
    if [ "$HTTP_WRONG_KEY" = "401" ]; then
        ok "queue with wrong apikey → 401"
    else
        fail "queue with wrong apikey → HTTP $HTTP_WRONG_KEY (expected 401)"
    fi

    HTTP_WITH_KEY=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/api?mode=queue&apikey=$KEY" 2>/dev/null)
    if [ "$HTTP_WITH_KEY" = "200" ]; then
        ok "queue with correct apikey → 200"
    else
        fail "queue with correct apikey → HTTP $HTTP_WITH_KEY (expected 200)"
    fi
else
    echo "  (skipped — no API key configured, auth is open)"
fi

# ── Summary ───────────────────────────────────────────────────────────────────
echo ""
echo "═══════════════════════════════"
echo "Results: ${GREEN}${PASS} passed${NC}, ${RED}${FAIL} failed${NC}"
echo "═══════════════════════════════"
[ "$FAIL" -eq 0 ]
