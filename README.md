# nzbdav-rs

Usenet virtual filesystem with WebDAV serving. Stream files directly from Usenet without downloading — no local storage required.

Written entirely in **Rust** — pure async I/O, zero-copy streaming, and a single statically-linked binary.

## What It Does

nzbdav-rs accepts NZB files, processes them to discover contained files (RAR archives, plain files), and serves the content as a virtual WebDAV filesystem. Every file read streams directly from Usenet on demand.

```
NZB → Pipeline → Virtual Filesystem → WebDAV → Browser/VLC/rclone
```

## Why Rust?

- **Pure Rust RAR4/RAR5 parser** — header-only, no decompression, no external libraries
- **Native async NNTP client** — TLS via rustls, connection pooling, pipelining
- **Single binary** — ~17MB statically linked, no runtime dependencies
- **Memory safe** — no buffer overflows, no null pointer dereferences, no data races
- **Fast** — async I/O throughout, zero-copy where possible, SIMD yEnc decoding

## Features

- **SABnzbd-compatible API** — drop-in download client for Sonarr, Radarr, Lidarr
- **On-demand streaming** — no local storage; articles fetched from Usenet in real time
- **RAR5 encrypted archives** — PBKDF2-HMAC-SHA256 key derivation, AES-256-CBC header decryption
- **Multi-server failover** — priority-ordered connection pools with automatic failover
- **WebDAV server** — PROPFIND, GET (with Range), PUT, DELETE, MOVE, COPY, MKCOL, OPTIONS
- **Browser playback** — stream video directly in Chrome/VLC via WebDAV
- **Web UI** — queue, history, file browser, upload, server management, settings, real-time logs
- **Concurrent processing** — configurable parallel NZB pipeline
- **Password support** — extracts from NZB filename `{{password}}` or XML metadata
- **WebSocket** — real-time status updates
- **OpenAPI/Swagger** — interactive API documentation

## Quick Start

```bash
# Build
cargo build --release -p nzbdav-app

# Run
./target/release/nzbdav-app --port 8080

# Open the web UI
open http://localhost:8080/ui
```

1. Add a Usenet server in the **Servers** tab
2. Upload an NZB in the **Upload** tab (or configure Sonarr)
3. Watch processing in the **Logs** tab
4. Browse and stream files in the **Files** tab

## Architecture

Rust workspace with 7 crates:

```
nzbdav-core      Models, DB, config, blob store
nzbdav-rar       RAR4/RAR5 header parser (pure Rust, no decompression)
nzbdav-stream    On-demand Usenet streaming (MultiSegmentStream, SeekableSegmentStream)
nzbdav-pipeline  NZB processing (deobfuscation, RAR/plain processors, aggregators)
nzbdav-dav       WebDAV server (custom axum handlers)
nzbdav-arr       Sonarr/Radarr client, health check, monitoring
nzbdav-app       Binary: CLI, SABnzbd API, server management, WebSocket, frontend
```

### Shared Libraries (on crates.io)

| Crate | Version | Description |
|-------|---------|-------------|
| [nzb-nntp](https://crates.io/crates/nzb-nntp) | 0.2.3 | Async NNTP client with TLS, pipelining, connection pooling |
| [nzb-core](https://crates.io/crates/nzb-core) | 0.2.2 | NZB parser, models, SQLite database |
| [nzb-decode](https://crates.io/crates/nzb-decode) | 0.1.1 | yEnc decoding and file assembly |
| [rust-par2](https://crates.io/crates/rust-par2) | 0.1.2 | PAR2 verify and repair |

## Configuration

All settings configurable via CLI flags, environment variables, or the web UI:

```
--port          HTTP listen port (default: 8080, env: NZBDAV_PORT)
--host          Listen address (default: 0.0.0.0, env: NZBDAV_HOST)
--db-path       SQLite database path (default: nzbdav.db, env: NZBDAV_DB)
--log-level     Log level (default: info, env: NZBDAV_LOG)
--api-key       API key for SABnzbd API (env: NZBDAV_API_KEY)
--webdav-user   WebDAV basic auth username (env: NZBDAV_WEBDAV_USER)
--webdav-pass   WebDAV basic auth password (env: NZBDAV_WEBDAV_PASS)
```

## Sonarr/Radarr Integration

Add nzbdav-rs as a SABnzbd download client:

1. **Settings → Download Clients → Add → SABnzbd**
2. Host: your nzbdav host, Port: 8080, API Key: your key
3. Category: `tv` (Sonarr) or `movies` (Radarr)

nzbdav-rs speaks the SABnzbd API protocol — Sonarr/Radarr don't know the difference.

For file access, mount the WebDAV with rclone and configure a remote path mapping. See [TEST_STACK.md](TEST_STACK.md) for details.

## API

- **SABnzbd API**: `/api?mode=addfile|queue|history|version|status|get_cats`
- **Server management**: `GET/POST /api/servers`, `PUT/DELETE /api/servers/{id}`
- **Settings**: `GET/PUT /api/settings`
- **Database backup**: `GET /api/backup`
- **WebSocket**: `ws://host:port/ws` (real-time queue/history/log events)
- **Interactive docs**: `/ui/swagger.html`

## License

MIT
