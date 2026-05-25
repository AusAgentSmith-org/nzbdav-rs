# nzbdav-rs Test Stacks

Docker Compose stacks for integration and E2E testing.

---

## 1. Usenet-Ultimate / UsenetStreamer Stack (`docker-compose.streaming-clients.yml`)

Validates nzbdav-rs compatibility with the two streaming clients that reported
issue #2 (`addfile` field-name case sensitivity, `nzbname` override, history
category filter). Both services are started and configured to point at nzbdav-rs;
a built-in test-runner fires the exact requests each client makes and prints
pass/fail output.

### Running

```bash
docker compose -f docker-compose.streaming-clients.yml up -d
docker compose -f docker-compose.streaming-clients.yml logs test-runner
```

The `test-runner` container exits after all tests complete. Exit code 0 = all
tests passed; non-zero = at least one failure (check the logs).

### Endpoints

| Service | URL | Purpose |
|---------|-----|---------|
| nzbdav | http://localhost:8080 | SABnzbd API + WebDAV |
| Usenet-Ultimate | http://localhost:1337 | Streaming client UI |
| UsenetStreamer | http://localhost:7000 | Streaming addon server |
| UsenetStreamer admin | http://localhost:7000/testtoken/admin/ | Admin dashboard |

### Manually verifying the streaming clients

Both services are pre-configured to talk to nzbdav-rs via environment variables
(`NZBDAV_URL`, `NZBDAV_API_KEY`, etc.). To confirm the connection from each:

1. Open the Usenet-Ultimate UI at http://localhost:1337 and trigger any NZB
   submission — the queue at http://localhost:8080 should gain an entry.
2. For UsenetStreamer, open the admin dashboard at
   http://localhost:7000/testtoken/admin/ and confirm the NZBDav status shows
   as connected.

---

## 2. Sonarr/Radarr Stack (`docker-compose.test.yml`)

Tests nzbdav-rs as a SABnzbd download client with rclone WebDAV mounting.

### Prerequisites

- Docker + Docker Compose
- A Usenet server (host, port, username, password)
- A Newznab-compatible indexer with API key

### Running

```bash
docker compose -f docker-compose.test.yml up -d
```

### Configuring Sonarr/Radarr

1. **Add Download Client** in Sonarr/Radarr:
   - Type: SABnzbd
   - Host: `nzbdav` (or `localhost` if running natively)
   - Port: `8080`
   - API Key: your configured key (via `--api-key` flag or `NZBDAV_API_KEY` env)
   - Category: `tv` for Sonarr, `movies` for Radarr

2. **Add Indexer** (Newznab type, URL + API key from your indexer)

3. **Remote Path Mapping** (rclone WebDAV mount):
   - Remote path: `/content/`
   - Local path: `/mnt/nzbdav/content/`

### Endpoints

| Service | URL | Purpose |
|---------|-----|---------|
| nzbdav UI | http://localhost:8080 | Web dashboard |
| nzbdav API | http://localhost:8080/api | SABnzbd-compatible API |
| nzbdav WebDAV | http://localhost:8080/dav | Virtual filesystem |
| Sonarr | http://localhost:8989 | TV management |
| Radarr | http://localhost:7878 | Movie management |

---

## 3. Full Streaming-Clients E2E (`docker-compose.streaming-clients-e2e.yml`)

Extends stack #1 with a real indexer (NZBHydra2) and a real Usenet provider so
the test actually exercises **search → download → mount → stream** end-to-end.

```
NZBHydra2  ←  test-runner (search)
   │
   ▼          (addurl)
 nzbdav-rs  ←──────────────  test-runner
   │                              │
   ├─ NNTP → Usenet provider      │ (poll mode=history)
   │                              │
   ▼                              ▼
 /dav/content/{cat}/{nzbname}  ←  rclone FUSE mount inside test-runner
                               ↓
                         range-GET first bytes
```

### Setup

```bash
cp .env.streaming-e2e.sample .env.streaming-e2e
# Fill in USENET_*, INDEXER_*, NZBDAV_* (the file is gitignored)
```

### Running

```bash
docker compose -f docker-compose.streaming-clients-e2e.yml \
               --env-file .env.streaming-e2e up -d --build

docker compose -f docker-compose.streaming-clients-e2e.yml \
               --env-file .env.streaming-e2e logs -f test-runner
```

The test-runner blocks on the two configure jobs (`nzbdav-configure`,
`nzbhydra2-configure`) completing successfully, then runs the five-step script
(`e2e-setup/test-streaming-e2e.sh`). Exit code 0 = all steps green.

### What each step asserts

| Step | Assertion | Failure mode |
|------|-----------|--------------|
| 0. Prereqs | nzbdav version, NZBHydra2 apiKey, ≥1 provider in nzbdav | Misconfigured stack |
| 1. Search | NZBHydra2 returns `SEARCH_LIMIT` recent items (empty query) | Indexer creds wrong |
| 2. Connectivity | Usenet-Ultimate `/` + UsenetStreamer `/$SECRET/manifest.json` reachable | Streaming client crashed |
| 3. Download | `addurl` returns `nzo_id`; history slot reaches `status=Completed` within `DOWNLOAD_TIMEOUT` | NNTP failure or article missing |
| 4. Mount | `rclone mount nzbdav:/` succeeds; `ls /mnt/nzbdav` lists ≥1 entry; downloaded file is locatable via the mount | WebDAV listing broken |
| 5. Stream | WebDAV range-GET returns 206/200; first bytes via WebDAV equal first bytes via the FUSE mount | Streaming path broken |

### Tunables (in `.env.streaming-e2e`)

| Var | Default | Notes |
|-----|---------|-------|
| `SEARCH_LIMIT` | 5 | How many results to pull in step 1 |
| `DOWNLOAD_TIMEOUT` | 900 | Cap (seconds) on step 3. 900 = 15 min "patient" |
| `STREAMER_SHARED_SECRET` | testtoken | UsenetStreamer addon token (matches the existing stack) |

### Endpoints

| Service | URL | Purpose |
|---------|-----|---------|
| nzbdav | http://localhost:8080 | SABnzbd API + WebDAV |
| NZBHydra2 | http://localhost:5076 | Newznab proxy / search |
| Usenet-Ultimate | http://localhost:1337 | Streaming client UI |
| UsenetStreamer | http://localhost:7000 | Streaming addon server |

The test-runner FUSE-mounts WebDAV inside its own container — there is no host
mount point. To inspect the mount manually: `docker compose -f
docker-compose.streaming-clients-e2e.yml exec test-runner ls /mnt/nzbdav`
(only useful while the test-runner is still alive, i.e. paused mid-run).

---

## 4. AIOStreams E2E Stack (`docker-compose.e2e.yml`)

Full end-to-end stack: nzbdav-rs + NZBHydra2 + AIOStreams + Stremio. Proves the
entire streaming pipeline from Stremio through AIOStreams to Usenet and back.

```
Stremio → AIOStreams → NZBHydra2 (indexer) → nzbdav-rs (download + WebDAV)
                                ↓
                    307 redirect to /dav/content/…/file.mkv
                                ↓
                         Stremio streams video
```

### Prerequisites

- Docker + Docker Compose
- Usenet provider credentials
- NZBHydra2 API key (auto-configured from the running container)
- `.env.e2e` credentials file (see sample below)

### Setup

```bash
# Copy and fill in credentials
cp .env.e2e.sample .env.e2e
# edit .env.e2e

# Start the stack (builds nzbdav-rs locally)
docker compose -f docker-compose.e2e.yml --env-file .env.e2e up --build -d
```

The `stremio-setup` service (port 8888) waits for all services to be healthy,
creates an AIOStreams user pre-configured with NzbDAV + NZBHydra2, and serves a
one-click addon install page at **http://localhost:8888**.

### `.env.e2e` variables

```env
# nzbdav-rs auth
NZBDAV_API_KEY=your-api-key
NZBDAV_WEBDAV_USER=user
NZBDAV_WEBDAV_PASS=pass

# AIOStreams secret
AIOSTREAMS_SECRET_KEY=any-random-string

# Optional: reuse an existing AIOStreams user across restarts
# (populated automatically by stremio-setup on first run)
AIOSTREAMS_USER_UUID=
AIOSTREAMS_USER_ENC_PASS=
```

### Verifying the pipeline manually

```bash
source .env.e2e

# 1. Get stream URLs from AIOStreams for any IMDB title
UUID=<your-aiostreams-user-uuid>
ENC=<your-aiostreams-enc-pass>
curl -s "http://localhost:3000/stremio/$UUID/$ENC/stream/movie/tt15239678.json" \
  | python3 -c "import json,sys; [print(s['url'][:80]) for s in json.load(sys.stdin)['streams'][:3]]"

# 2. Follow a stream URL — should redirect to a /dav/content/… URL
curl -sI <stream-url> | grep -i location

# 3. Verify the redirect URL serves video bytes (first 4 bytes = MKV magic)
curl -s --location-trusted --range 0-3 <stream-url> | xxd
# Expected: 1a 45 df a3
```

### Endpoints

| Service | URL | Purpose |
|---------|-----|---------|
| nzbdav | http://localhost:8080 | SABnzbd API + WebDAV |
| NZBHydra2 | http://localhost:5076 | Usenet indexer |
| AIOStreams | http://localhost:3000 | Stremio addon server |
| Stremio | http://localhost:11470 | Streaming backend |
| Setup page | http://localhost:8888 | One-click addon install |

---

## Running nzbdav-rs locally (no Docker)

```bash
cargo build --release -p nzbdav-app
./target/release/nzbdav-app --port 8080 --db-path nzbdav.db --log-level info
```

Open http://localhost:8080 to configure servers and settings.
