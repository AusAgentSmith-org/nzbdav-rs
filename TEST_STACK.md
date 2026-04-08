# nzbdav-rs Test Stack

Docker Compose stack for end-to-end integration testing with Sonarr and Radarr.

## Prerequisites

- Docker + Docker Compose
- A Usenet server (host, port, username, password)
- An NZB indexer (Newznab-compatible) with API key

## Running Locally (without Docker)

```bash
cargo build --release -p nzbdav-app
./target/release/nzbdav-app --port 8080 --db-path nzbdav.db --log-level info
```

Then open http://localhost:8080/ui to configure servers and settings.

## Docker Compose (local binary)

Build locally, then run with Sonarr/Radarr in Docker:

```bash
cargo build --release -p nzbdav-app
docker compose -f docker-compose.local.yml up -d
```

## Configuring Sonarr/Radarr

1. **Add Download Client** in Sonarr/Radarr:
   - Type: SABnzbd
   - Host: `nzbdav` (or `localhost` if running natively)
   - Port: `8080`
   - API Key: your configured key (via `--api-key` flag)
   - Category: `tv` for Sonarr, `movies` for Radarr

2. **Add Indexer**:
   - Type: Newznab
   - URL + API key from your indexer

3. **Remote Path Mapping** (if using rclone mount):
   - Remote: `/content/`
   - Local: `/mnt/nzbdav/content/`

## Endpoints

| Service | URL | Purpose |
|---------|-----|---------|
| nzbdav UI | http://localhost:8080/ui | Web dashboard |
| nzbdav API | http://localhost:8080/api | SABnzbd-compatible API |
| nzbdav WebDAV | http://localhost:8080/dav | Virtual filesystem |
| nzbdav Docs | http://localhost:8080/ui/docs.html | Documentation |
| nzbdav API Spec | http://localhost:8080/ui/swagger.html | OpenAPI/Swagger |
| Sonarr | http://localhost:8989 | TV management |
| Radarr | http://localhost:7878 | Movie management |
