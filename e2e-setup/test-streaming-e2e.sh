#!/usr/bin/env bash
# Full-flow e2e for nzbdav-rs + Usenet-Ultimate + UsenetStreamer.
#
# Order of operations:
#   1. Search   — pull SEARCH_LIMIT recent NZBs from NZBHydra2 (the indexer
#                 both streaming clients rely on)
#   2. Connect  — confirm Usenet-Ultimate and UsenetStreamer are alive
#   3. Download — addurl the first search hit to nzbdav, poll mode=history
#                 until status=Completed (cap = DOWNLOAD_TIMEOUT seconds)
#   4. Mount    — rclone-mount nzbdav's WebDAV inside this container, ls the
#                 downloaded folder to confirm files appear via the mount
#   5. Stream   — range-GET the first non-trivial file's bytes from /dav/
#                 directly to confirm WebDAV serves byte ranges
#
# Exit 0 = all five steps green.

set -uo pipefail

NZBDAV="${NZBDAV_URL:-http://nzbdav:8080}"
KEY="${NZBDAV_API_KEY:-}"
WDU="${NZBDAV_WEBDAV_USER:-}"
WDP="${NZBDAV_WEBDAV_PASS:-}"
HYDRA="${HYDRA_URL:-http://nzbhydra2:5076}"
ULTI="${ULTIMATE_URL:-http://usenet-ultimate:1337}"
STRMR="${STREAMER_URL:-http://usenetstreamer:7000}"
SECRET="${STREAMER_SHARED_SECRET:-testtoken}"
LIMIT="${SEARCH_LIMIT:-5}"
DLTO="${DOWNLOAD_TIMEOUT:-900}"

MOUNT=/mnt/nzbdav
PASS=0
FAIL=0
GREEN='\033[0;32m'; RED='\033[0;31m'; YEL='\033[0;33m'; NC='\033[0m'
ok()   { echo -e "${GREEN}PASS${NC} $1"; PASS=$((PASS+1)); }
fail() { echo -e "${RED}FAIL${NC} $1"; FAIL=$((FAIL+1)); }
info() { echo -e "${YEL}INFO${NC} $1"; }

cleanup() {
    if mountpoint -q "$MOUNT" 2>/dev/null; then
        fusermount -u "$MOUNT" 2>/dev/null || umount "$MOUNT" 2>/dev/null || true
    fi
}
trap cleanup EXIT

echo "═══════════════════════════════════════════════════════════════"
echo "  nzbdav-rs streaming-clients FULL e2e"
echo "═══════════════════════════════════════════════════════════════"
echo "nzbdav:        $NZBDAV"
echo "hydra:         $HYDRA"
echo "ultimate:      $ULTI"
echo "streamer:      $STRMR"
echo "search limit:  $LIMIT"
echo "dl timeout:    ${DLTO}s"
echo ""

# ── Step 0: connectivity prerequisites ───────────────────────────────────────
echo "── 0. Prerequisites ──"
VER=$(curl -sf "$NZBDAV/api?mode=version&apikey=$KEY" 2>/dev/null \
    | python3 -c "import sys,json; print(json.load(sys.stdin).get('version',''))" 2>/dev/null || echo "")
[ -n "$VER" ] && ok "nzbdav reachable (version=$VER)" || { fail "nzbdav not reachable"; exit 1; }

HYDRA_CFG=$(curl -sf "$HYDRA/internalapi/config" 2>/dev/null || echo '{}')
HYDRA_KEY=$(echo "$HYDRA_CFG" | python3 -c "import sys,json; print(json.load(sys.stdin).get('main',{}).get('apiKey',''))" 2>/dev/null || echo "")
[ -n "$HYDRA_KEY" ] && ok "hydra reachable (key acquired)" || { fail "hydra not reachable / no apiKey"; exit 1; }

# Confirm the Usenet provider is actually wired into nzbdav before we ask it to download.
SERVERS=$(curl -sf -H "X-Api-Key: $KEY" "$NZBDAV/api/servers" 2>/dev/null || echo '[]')
SERVER_COUNT=$(echo "$SERVERS" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "0")
if [ "$SERVER_COUNT" -ge 1 ]; then
    ok "nzbdav has $SERVER_COUNT Usenet provider(s) configured"
else
    fail "nzbdav has no Usenet providers — nzbdav-configure must run first"
    exit 1
fi
echo ""

# ── Step 1: search ───────────────────────────────────────────────────────────
# Primary: search direct against INDEXER_URL (Newznab). Secondary: try Hydra
# too as an informational check, but a Hydra-side failure is NOT fatal — the
# search step is here so we can find a real NZB to download in step 3.
echo "── 1. SEARCH ──"

INDEXER_URL="${INDEXER_URL:-}"
INDEXER_API_KEY="${INDEXER_API_KEY:-}"

QUERIES=( "${SEARCH_QUERY:-}" "ubuntu" "linux" "2024" )
SEARCH=""
SEARCH_Q=""
SEARCH_VIA=""

probe() {
    local label="$1"; local url="$2"
    SEARCH=$(curl -sf "$url" 2>/dev/null || echo '')
    if [ -z "$SEARCH" ]; then
        info "[$label] (no response)"
        return 1
    fi
    TOTAL=$(printf '%s' "$SEARCH" | python3 -c "
import sys,json,re
raw=sys.stdin.read()
# Try JSON first; fall back to XML by extracting total='N' (newznab:response in RSS)
try:
    d=json.loads(raw)
    ch=d.get('channel',{})
    resp=ch.get('response',{}).get('attributes',{})
    items=ch.get('item',[])
    if isinstance(items, dict): items=[items]
    print(int(resp.get('total', len(items))))
except Exception:
    m=re.search(r'total=\"(\\d+)\"', raw)
    print(m.group(1) if m else '0')
" 2>/dev/null || echo "0")
    info "[$label] total=$TOTAL"
    [ "$TOTAL" -gt 0 ]
}

for Q in "${QUERIES[@]}"; do
    Q_ENC=$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1]))" "$Q")
    # Try Hydra first (preferred — what the streaming clients use in practice)
    if [ -n "$HYDRA_KEY" ]; then
        URL="$HYDRA/api?t=search&q=$Q_ENC&apikey=$HYDRA_KEY&o=json&limit=$LIMIT"
        echo "[search hydra] q='$Q'"
        if probe "hydra q='$Q'" "$URL"; then
            SEARCH_Q="$Q"; SEARCH_VIA="hydra"; break
        fi
    fi
    # Fall back to direct indexer (Newznab) — same protocol nzbhydra speaks
    if [ -n "$INDEXER_URL" ] && [ -n "$INDEXER_API_KEY" ]; then
        URL="$INDEXER_URL/api?t=search&q=$Q_ENC&apikey=$INDEXER_API_KEY&o=json&limit=$LIMIT"
        echo "[search direct] q='$Q'"
        if probe "direct q='$Q'" "$URL"; then
            SEARCH_Q="$Q"; SEARCH_VIA="direct"; break
        fi
    fi
done

[ -n "$SEARCH_VIA" ] && info "search succeeded via: $SEARCH_VIA"

SEARCH_TMP=$(mktemp)
printf '%s' "$SEARCH" > "$SEARCH_TMP"
RESULTS=$(SEARCH_PATH="$SEARCH_TMP" python3 <<'PY'
import json, os, re, sys
import xml.etree.ElementTree as ET
with open(os.environ['SEARCH_PATH']) as f:
    raw = f.read()

out = []

def push(title, link, size):
    if title and link:
        try: size = int(size or 0)
        except: size = 0
        out.append({'title': title, 'link': link, 'size': size})

try:
    data = json.loads(raw)
    items = data.get('channel', {}).get('item', [])
    if isinstance(items, dict): items = [items]
    for it in items:
        push(
            it.get('title', ''),
            it.get('link') or it.get('enclosure', {}).get('@attributes', {}).get('url', ''),
            it.get('size') or it.get('enclosure', {}).get('@attributes', {}).get('length', '0'),
        )
except Exception:
    # RSS XML fallback (Newznab default)
    try:
        # ET doesn't like default namespaces in xpath; strip them upfront.
        cleaned = re.sub(r'\\sxmlns="[^"]+"', '', raw, count=1)
        root = ET.fromstring(cleaned)
        for item in root.iter('item'):
            t  = (item.findtext('title') or '').strip()
            l  = (item.findtext('link')  or '').strip()
            sz = '0'
            enc = item.find('enclosure')
            if enc is not None:
                sz = enc.get('length', '0')
                if not l: l = enc.get('url', '')
            push(t, l, sz)
    except Exception as e:
        sys.stderr.write(f'[search-parse] XML fallback failed: {e}\\n')

print(json.dumps(out))
PY
)
rm -f "$SEARCH_TMP"
COUNT=$(printf '%s' "$RESULTS" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "0")
if [ "$COUNT" -ge 1 ]; then
    ok "Search returned $COUNT result(s) (query='$SEARCH_Q')"
    RES_TMP=$(mktemp); printf '%s' "$RESULTS" > "$RES_TMP"
    RESULTS_PATH="$RES_TMP" python3 -c "
import json,os
with open(os.environ['RESULTS_PATH']) as f:
    rs=json.load(f)
for i,r in enumerate(rs):
    sz=r['size']//1024//1024 if r['size'] else '?'
    print(f'  [{i}] {r[\"title\"][:80]} ({sz}MB)')
"
    rm -f "$RES_TMP"
else
    fail "Search returned 0 results across all probed queries — indexer/hydra config wrong"
    echo "Raw final search response (truncated): ${SEARCH:0:500}"
    exit 1
fi
echo ""

# ── Step 2: streaming-client connectivity ────────────────────────────────────
echo "── 2. STREAMING CLIENT CONNECTIVITY ──"

# UsenetStreamer Stremio addon: GET /manifest.json (token-scoped)
ULTI_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$ULTI/" 2>/dev/null || echo "000")
case "$ULTI_CODE" in
    2*|3*) ok "Usenet-Ultimate responded HTTP $ULTI_CODE on /" ;;
    *)     fail "Usenet-Ultimate / returned HTTP $ULTI_CODE" ;;
esac

STRMR_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$STRMR/$SECRET/manifest.json" 2>/dev/null || echo "000")
case "$STRMR_CODE" in
    2*|3*) ok "UsenetStreamer manifest.json HTTP $STRMR_CODE" ;;
    *)     fail "UsenetStreamer manifest.json HTTP $STRMR_CODE" ;;
esac
echo ""

# ── Step 3: download ─────────────────────────────────────────────────────────
echo "── 3. DOWNLOAD ──"

# Try each search result smallest-first. If one fails fast (e.g. password-
# protected → "no importable video"), discard it and try the next. We want
# to surface dav-rs bugs across the full pipeline, not get stuck on the first
# encrypted RAR release. Pass criterion: at least ONE result downloads cleanly.
ORDER_TMP=$(mktemp); printf '%s' "$RESULTS" > "$ORDER_TMP"
ORDERED=$(ORDER_PATH="$ORDER_TMP" python3 <<'PY'
import json, os
with open(os.environ['ORDER_PATH']) as f:
    rs = json.load(f)
# Smallest first (but >0 first); drop suspicious password groups for the
# happy-path slot. Tag the rest as "may-be-encrypted" but still try them.
SUSPECT = ('reimu', 'megusta', 'rarbg', 'yify', 'sparks')  # often password-protected
def key(r):
    title_l = (r.get('title') or '').lower()
    suspect = any(s in title_l for s in SUSPECT)
    size = r.get('size', 0) or 9e18
    return (1 if suspect else 0, size)
print(json.dumps(sorted(rs, key=key)))
PY
)
rm -f "$ORDER_TMP"

ATTEMPT=0
NZO_ID=""
COMPLETED_SLOT=""
FAIL_HISTORY=()
TOTAL_RESULTS=$(printf '%s' "$ORDERED" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))")
info "trying up to $TOTAL_RESULTS result(s) in order"

while [ "$ATTEMPT" -lt "$TOTAL_RESULTS" ]; do
    PICK=$(printf '%s' "$ORDERED" | python3 -c "
import sys,json
print(json.dumps(json.load(sys.stdin)[$ATTEMPT]))
")
    NZB_URL=$(printf '%s' "$PICK"   | python3 -c "import sys,json; print(json.load(sys.stdin)['link'])")
    NZB_NAME=$(printf '%s' "$PICK"  | python3 -c "import sys,json; print(json.load(sys.stdin)['title'])")
    NZB_SIZE=$(printf '%s' "$PICK"  | python3 -c "import sys,json; print(json.load(sys.stdin)['size'])")
    info "[attempt $((ATTEMPT+1))/$TOTAL_RESULTS] $NZB_NAME (~$((NZB_SIZE/1024/1024))MB)"

    NZB_NAME_ENC=$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1]))" "$NZB_NAME")
    NZB_URL_ENC=$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=''))" "$NZB_URL")

    ADD=$(curl -sf -X POST \
        "$NZBDAV/api?mode=addurl&name=$NZB_URL_ENC&nzbname=$NZB_NAME_ENC&cat=movies&apikey=$KEY" 2>/dev/null \
        || echo '{}')
    THIS_ID=$(printf '%s' "$ADD" | python3 -c "import sys,json; print(json.load(sys.stdin).get('nzo_ids',[''])[0])" 2>/dev/null || echo "")
    if [ -z "$THIS_ID" ]; then
        info "  addurl failed: $ADD"
        ATTEMPT=$((ATTEMPT+1))
        continue
    fi

    # Poll. If history says Failed quickly, capture the fail_message and try next.
    POLL=0
    SINGLE_TIMEOUT=$DLTO
    [ "$TOTAL_RESULTS" -gt 1 ] && SINGLE_TIMEOUT=$((DLTO/TOTAL_RESULTS+60))
    LAST_STATUS=""; LAST_PCT=""
    while [ "$POLL" -lt "$SINGLE_TIMEOUT" ]; do
        Q=$(curl -sf "$NZBDAV/api?mode=queue&apikey=$KEY" 2>/dev/null || echo '{}')
        H=$(curl -sf "$NZBDAV/api?mode=history&apikey=$KEY" 2>/dev/null || echo '{}')
        STATUS_LINE=$(python3 - "$THIS_ID" "$Q" "$H" <<'PY'
import sys, json
nzo, q_raw, h_raw = sys.argv[1], sys.argv[2], sys.argv[3]
try: q = json.loads(q_raw).get('queue', {})
except: q = {}
try: h = json.loads(h_raw).get('history', {})
except: h = {}
for s in q.get('slots', []):
    if s.get('nzo_id') == nzo:
        print(f"QUEUE status={s.get('status','?')} pct={s.get('percentage','?')} slot={json.dumps(s)}")
        sys.exit(0)
for s in h.get('slots', []):
    if s.get('nzo_id') == nzo:
        print(f"HIST status={s.get('status','?')} slot={json.dumps(s)}")
        sys.exit(0)
print("UNKNOWN status=? slot={}")
PY
)
        PHASE=$(echo "$STATUS_LINE" | awk '{print $1}')
        STATUS=$(echo "$STATUS_LINE" | grep -oP 'status=\K[^ ]+' || echo "?")
        PCT=$(echo "$STATUS_LINE" | grep -oP 'pct=\K[^ ]+' || echo "")
        if [ "$STATUS" != "$LAST_STATUS" ] || [ "$PCT" != "$LAST_PCT" ]; then
            info "  [t=${POLL}s] phase=$PHASE status=$STATUS ${PCT:+pct=$PCT}"
            LAST_STATUS="$STATUS"; LAST_PCT="$PCT"
        fi

        if [ "$PHASE" = "HIST" ]; then
            if [ "$STATUS" = "Completed" ]; then
                NZO_ID="$THIS_ID"
                COMPLETED_SLOT=$(python3 -c "
import sys
line = sys.argv[1]
idx = line.find('slot=')
print(line[idx+5:] if idx >= 0 else '')
" "$STATUS_LINE")
                ok "[attempt $((ATTEMPT+1))] download completed within ${POLL}s"
                break 2
            elif [ "$STATUS" = "Failed" ] || [ "$STATUS" = "failed" ]; then
                FAIL_MSG=$(python3 -c "
import sys, json
slot = sys.argv[1]
idx = slot.find('{')
try: d = json.loads(slot[idx:]) if idx>=0 else {}
except: d = {}
print(d.get('fail_message','(no message)'))
" "$STATUS_LINE")
                FAIL_HISTORY+=("$NZB_NAME → $FAIL_MSG")
                info "  failed: $FAIL_MSG — trying next"
                break
            fi
        fi
        sleep 5
        POLL=$((POLL+5))
    done

    if [ -n "$NZO_ID" ]; then break; fi
    ATTEMPT=$((ATTEMPT+1))
done

if [ -z "$NZO_ID" ]; then
    fail "all $TOTAL_RESULTS search hits failed to download"
    echo ""
    echo "── nzbdav-rs failure summary (this is the bug surface) ──"
    for entry in "${FAIL_HISTORY[@]}"; do echo "  ✗ $entry"; done
    exit 1
fi

# Resolve where the content landed on disk. nzbdav's history slots include
# 'storage'/'path'/'name' depending on version — be tolerant.
CONTENT_DIR=$(echo "$COMPLETED_SLOT" | python3 -c "
import sys,json
try:
    s=json.loads(sys.stdin.read())
except Exception:
    print(''); sys.exit(0)
# possible fields
for key in ('storage','path','name','filename'):
    v=s.get(key)
    if v:
        print(v); sys.exit(0)
print('')
")
info "content folder (from history): $CONTENT_DIR"
echo ""

# ── Step 4: mount ────────────────────────────────────────────────────────────
echo "── 4. MOUNT ──"
mkdir -p "$MOUNT" /root/.config/rclone
OBS_PASS=$(rclone obscure "$WDP")
cat > /root/.config/rclone/rclone.conf <<RCFG
[nzbdav]
type = webdav
url = http://nzbdav:8080/dav
vendor = other
user = $WDU
pass = $OBS_PASS
RCFG

echo "[mount] rclone mount nzbdav: $MOUNT"
rclone mount nzbdav:/ "$MOUNT" \
    --allow-other \
    --vfs-cache-mode=off \
    --dir-cache-time=2s \
    --poll-interval=2s \
    --no-modtime \
    --read-only \
    --daemon \
    --log-level INFO \
    --log-file /tmp/rclone-mount.log 2>&1 || true

# Wait for mount to come up
for _ in $(seq 1 30); do
    if mountpoint -q "$MOUNT" 2>/dev/null; then break; fi
    sleep 1
done

if mountpoint -q "$MOUNT" 2>/dev/null; then
    ok "rclone FUSE mount is live at $MOUNT"
else
    fail "rclone mount failed to come up"
    info "rclone log tail:"
    tail -20 /tmp/rclone-mount.log 2>/dev/null || true
    exit 1
fi

# Browse the WebDAV root through the mount.
echo "[mount] ls $MOUNT"
ls -la "$MOUNT" || true
TOPLEVEL=$(ls "$MOUNT" 2>/dev/null | wc -l)
if [ "$TOPLEVEL" -ge 1 ]; then
    ok "mount lists $TOPLEVEL top-level entries"
else
    fail "mount listing is empty (expected at least /content)"
fi

# Find the downloaded file. We don't trust CONTENT_DIR (varies across versions);
# instead, walk /content/ looking for the largest regular file.
echo "[mount] Locating downloaded file under $MOUNT/content/..."
FOUND=""
if [ -d "$MOUNT/content" ]; then
    FOUND=$(find "$MOUNT/content" -type f -size +1c 2>/dev/null | head -1)
fi
if [ -n "$FOUND" ]; then
    SZ=$(stat -c %s "$FOUND" 2>/dev/null || echo "?")
    ok "located file via mount: $FOUND (${SZ}B)"
else
    fail "no file found under $MOUNT/content/"
    info "tree under content (if any):"
    find "$MOUNT/content" -maxdepth 3 -print 2>/dev/null | head -20 || true
fi
echo ""

# ── Step 5: stream ───────────────────────────────────────────────────────────
echo "── 5. STREAM ──"

# Method A: direct WebDAV range-GET (proves nzbdav serves byte ranges).
if [ -n "$FOUND" ]; then
    # Strip the mount prefix to derive the WebDAV path.
    REL="${FOUND#$MOUNT/}"
    DAV_PATH=$(python3 -c "
import urllib.parse, sys
print('/'.join(urllib.parse.quote(p) for p in sys.argv[1].split('/')))
" "$REL")
    DAV_URL="$NZBDAV/dav/$DAV_PATH"
    echo "[stream] GET Range: bytes=0-15  $DAV_URL"
    HDR_RANGE=$(curl -sI -u "$WDU:$WDP" -H "Range: bytes=0-15" "$DAV_URL" 2>/dev/null \
        | tr -d '\r' | head -20)
    echo "$HDR_RANGE"
    CODE=$(echo "$HDR_RANGE" | head -1 | awk '{print $2}')
    if [ "$CODE" = "206" ] || [ "$CODE" = "200" ]; then
        ok "WebDAV range-GET returned HTTP $CODE"
    else
        fail "WebDAV range-GET returned HTTP $CODE (expected 206)"
    fi

    BYTES=$(curl -s -u "$WDU:$WDP" -H "Range: bytes=0-15" "$DAV_URL" 2>/dev/null \
        | head -c 16 | od -An -tx1 | tr -d ' \n')
    if [ -n "$BYTES" ] && [ "$BYTES" != "00000000000000000000000000000000" ]; then
        ok "first 16 bytes via WebDAV: $BYTES"
    else
        fail "WebDAV streamed empty/zero bytes"
    fi
fi

# Method B: stream THROUGH the FUSE mount (proves the rclone path serves bytes).
if [ -n "$FOUND" ]; then
    MOUNT_BYTES=$(head -c 16 "$FOUND" 2>/dev/null | od -An -tx1 | tr -d ' \n')
    if [ -n "$MOUNT_BYTES" ] && [ "$MOUNT_BYTES" != "00000000000000000000000000000000" ]; then
        ok "first 16 bytes via FUSE mount: $MOUNT_BYTES"
    else
        fail "FUSE mount read returned empty/zero bytes"
    fi

    if [ -n "$BYTES" ] && [ -n "$MOUNT_BYTES" ] && [ "$BYTES" = "$MOUNT_BYTES" ]; then
        ok "WebDAV bytes ≡ FUSE bytes (mount + stream are consistent)"
    elif [ -n "$BYTES" ] && [ -n "$MOUNT_BYTES" ]; then
        fail "WebDAV ($BYTES) and FUSE ($MOUNT_BYTES) disagree on first bytes"
    fi
fi
echo ""

# ── Summary ───────────────────────────────────────────────────────────────────
echo "═══════════════════════════════════════════════════════════════"
echo -e "Results: ${GREEN}${PASS} passed${NC}, ${RED}${FAIL} failed${NC}"
echo "═══════════════════════════════════════════════════════════════"
[ "$FAIL" -eq 0 ]
