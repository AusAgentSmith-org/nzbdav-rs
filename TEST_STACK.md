# nzbdav-rs Test Stacks

Docker Compose stacks for integration and E2E testing.

---

## 1. Sonarr/Radarr Stack (`docker-compose.test.yml`)

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

## 2. AIOStreams E2E Stack (`docker-compose.e2e.yml`)

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
