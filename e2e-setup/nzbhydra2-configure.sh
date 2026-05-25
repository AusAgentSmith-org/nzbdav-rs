#!/usr/bin/env bash
# Configure NZBHydra2 with a Newznab indexer via its internal API.
#
# Approach:
#   1. GET /internalapi/config — fetch the current config (NZBHydra generates
#      a default apiKey on first boot)
#   2. Inject the indexer into config['indexers'] (if not already present)
#   3. PUT /internalapi/config to save
#
# Then print the resolved Hydra API key so the test-runner can use it.

set -euo pipefail

HYDRA="${HYDRA_URL:-http://nzbhydra2:5076}"

require() { [ -n "${!1:-}" ] || { echo "ERROR: env $1 required" >&2; exit 1; }; }
require INDEXER_NAME
require INDEXER_URL
require INDEXER_API_KEY

echo "[hydra-configure] Waiting for NZBHydra2 at $HYDRA..."
for _ in $(seq 1 60); do
    if curl -sf "$HYDRA/internalapi/config" >/dev/null 2>&1; then break; fi
    sleep 2
done

CFG=$(curl -sf "$HYDRA/internalapi/config")
if [ -z "$CFG" ]; then
    echo "[hydra-configure] FAILED to fetch /internalapi/config" >&2
    exit 1
fi

HYDRA_KEY=$(echo "$CFG" | python3 -c "import sys,json; print(json.load(sys.stdin)['main']['apiKey'])")
echo "[hydra-configure] Hydra apiKey=$HYDRA_KEY"

CFG_TMP=$(mktemp); printf '%s' "$CFG" > "$CFG_TMP"
NEW_CFG=$(CFG_PATH="$CFG_TMP" python3 - <<'PY'
import json, os, sys
with open(os.environ['CFG_PATH']) as f:
    cfg = json.load(f)
name = os.environ['INDEXER_NAME']
url = os.environ['INDEXER_URL']
api_key = os.environ['INDEXER_API_KEY']

indexers = cfg.setdefault('indexers', [])
existing = [i for i in indexers if i.get('name') == name]
if existing:
    sys.stderr.write(f"[hydra-configure] indexer {name} already present, leaving alone\n")
else:
    indexers.append({
        'name': name,
        'enabled': True,
        'host': url,
        'apikey': api_key,
        'searchModuleType': 'NEWZNAB',
        'type': 'NEWZNAB',
        'state': 'ENABLED',
        'categoryMapping': 'Newznab',
        'enabledForSearchSource': 'BOTH',
        'configComplete': True,
        'allCapsChecked': True,
        'supportedSearchIds': ['IMDB', 'TVDB', 'TMDB'],
        'supportedSearchTypes': ['SEARCH', 'MOVIE', 'TVSEARCH'],
        'timeout': None,
        'downloadLimit': None,
        'hitLimit': None,
        'loadLimitOnRandom': None,
        'preselect': True,
        'score': 0,
        'showOnSearch': True,
        'userAgent': None,
        'username': None,
        'password': None,
        'vipExpirationDate': None,
        'backend': 'NEWZNAB',
    })
    sys.stderr.write(f"[hydra-configure] added indexer {name}\n")

# Suppress NZBHydra's first-run wizard prompt
cfg.setdefault('main', {})['startupBrowser'] = False
cfg['main']['isFirstStart'] = False
cfg['main']['firstStart'] = False

print(json.dumps(cfg))
PY
)
rm -f "$CFG_TMP"

echo "[hydra-configure] Saving updated config..."
SAVE=$(curl -sf -X PUT -H 'Content-Type: application/json' \
    -d "$NEW_CFG" \
    "$HYDRA/internalapi/config" 2>&1 || true)
echo "[hydra-configure] save response (truncated): ${SAVE:0:200}"

# Quick verification: hit the Newznab API ourselves to confirm the indexer is live.
echo "[hydra-configure] Verifying indexer via search probe..."
PROBE=$(curl -sf "$HYDRA/api?t=search&q=&apikey=$HYDRA_KEY&o=json&limit=1" 2>&1 || echo "")
HITS=$(echo "$PROBE" | python3 -c "
import sys,json
try:
    d=json.load(sys.stdin)
    ch=d.get('channel',{})
    items=ch.get('item',[])
    print(len(items) if isinstance(items,list) else (1 if items else 0))
except Exception as e:
    print('parse-err:', e, file=sys.stderr)
    print(0)
" 2>/dev/null)
echo "[hydra-configure] probe returned $HITS item(s)"

echo "[hydra-configure] Done. (test-runner will refetch apiKey from /internalapi/config)"
