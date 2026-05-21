# Changelog

All notable changes to nzbdav-rs are documented here.

---

## [Unreleased] — Usenet-Ultimate / UsenetStreamer compatibility

Resolves all known incompatibilities with [Usenet-Ultimate](https://github.com/DSmart33/Usenet-Ultimate)
and [UsenetStreamer](https://github.com/Sanket9225/UsenetStreamer) (issue #2).

### Fixed

#### `addfile` rejects `nzbFile` field name (critical — NZB upload silently ignored)

Usenet-Ultimate posts multipart uploads with field name `nzbFile` (capital F).
nzbdav-rs only accepted `nzbfile` and `name`, so the NZB data was never found.
The handler returned `{"status":false,"error":"no NZB file found in upload"}` with
HTTP 200 — which both clients treated as success, leaving the queue empty.

**Fix:** field name matching is now case-insensitive (`eq_ignore_ascii_case`).

#### `addfile` ignores `nzbname` parameter (major — deduplication broken)

Both clients send `nzbname=<FolderName>` on every `addfile` call for the same reason
AIOStreams does: the folder name in the WebDAV tree must match for deduplication.
nzbdav-rs used the multipart filename instead, so the WebDAV folder never matched
and every play request re-added the NZB.

**Fix:** `handle_addfile` now applies the same `nzbname`-over-filename priority
as `handle_addurl`.

#### History `category` filter not recognised (minor — category filter ignored)

Usenet-Ultimate polls `mode=history&category=movies` (not `cat=movies`). The
`category` query parameter was undeclared and silently dropped, so every history
poll returned all categories regardless.

**Fix:** `ApiParams` now includes `category: Option<String>`. `handle_history`
merges `category` and `cat` into a single case-insensitive filter applied in Rust
after fetching the paginated results.

### Added

- **4 regression tests** in `sab_api/handler.rs`:
  - `test_addfile_nzbFile_field_name_accepted`
  - `test_addfile_lowercase_nzbfile_field_accepted`
  - `test_addfile_nzbname_overrides_multipart_filename`
  - `test_history_category_filter`
- **Debug Docker Compose** (`docker-compose.debug.yml`) — runs nzbdav-rs with
  `NZBDAV_LOG=debug` plus an auto-running `test-runner` container
- **Issue #2 regression test script** (`e2e-setup/test-issue2.sh`) — fires the
  exact curl requests Usenet-Ultimate and UsenetStreamer make, with pass/fail output

---

## [0.5.2] — AIOStreams compatibility

This release resolves all known incompatibilities with
[AIOStreams](https://github.com/Viren070/AIOStreams), making nzbdav-rs a
fully working NzbDAV backend for Stremio streaming via AIOStreams.

### Fixed

#### History item UUID mismatch (critical — streaming never completed)

`move_to_history()` was generating a fresh `Uuid::new_v4()` for the history
entry instead of reusing the queue item's UUID. AIOStreams polls
`mode=history&nzo_ids=<id>` using the UUID it received from `addurl` — because
the history UUID was different from the queue UUID, polling never found the
completed entry and the request timed out every time.

**Fix:** `HistoryItem.id` now inherits `item.id` rather than a new UUID.

#### WebDAV PROPFIND hrefs missing mount prefix (critical — video redirects to 404)

nzbdav-rs mounts its WebDAV handler under `/dav` via Axum's `.nest()`. Axum
strips the `/dav` prefix before the handler runs, so `req.uri().path()` inside
the handler is `/content/…` rather than `/dav/content/…`. The PROPFIND
response was returning child hrefs like `/content/Movies/Film/film.mkv` without
the `/dav` prefix.

The webdav-client library used by AIOStreams computed the file path as
`/../content/Movies/Film/film.mkv` (a relative traversal from `/dav`). When
appended to the public WebDAV URL, the resulting redirect resolved to the
frontend fallback handler and returned HTML instead of video bytes.

**Fix:** The PROPFIND handler reads `OriginalUri` from the Axum request
extensions (set by the nesting middleware and falling back to `req.uri()` when
not nested) and passes it to `multistatus_xml()` as `base_href`. Child hrefs
are now built as `url_prefix + node.item.path`, where `url_prefix` is the
difference between `base_href` and the first node's VFS path.

#### `nzbname` parameter ignored (major — deduplication broken)

AIOStreams sends `nzbname=<ExpectedFolderName>` with every `addurl` call. The
parameter encodes the folder name AIOStreams will look for in the WebDAV tree
when checking whether content already exists. nzbdav-rs was ignoring it and
deriving the job name from the URL basename instead, so the content folder name
never matched and every play request re-added the NZB.

**Fix:** `ApiParams` now includes `nzbname: Option<String>`. `handle_addurl`
uses this value as the stored filename (and therefore the job/folder name) when
it is present and non-empty, falling back to the URL basename otherwise.

#### `nzo_ids` history filter not implemented (minor — O(N) polling)

AIOStreams passes `nzo_ids=<id>` when calling `mode=history` to narrow the
result to a single item. The parameter was undeclared in `ApiParams` and
ignored, causing nzbdav-rs to return the full paginated history. AIOStreams
located the item client-side via `.find()`, which was correct but O(N).

**Fix:** `ApiParams` now includes `nzo_ids: Option<String>`. When present,
`handle_history` calls `history_items::get_by_id` for each requested UUID
directly rather than scanning the full history table. The result set is limited
to the requested IDs so pagination cannot hide an item.

#### SABnzbd API not reachable at `/dav/api` (critical — AIOStreams config impossible)

AIOStreams uses a single `nzbdavUrl` field for both the WebDAV root and the
SABnzbd API: `webdavUrl = {nzbdavUrl}/` and `apiUrl = {nzbdavUrl}/api`. With
nzbdav-rs serving WebDAV at `/dav` and the API at `/api`, no single URL value
could satisfy both constraints.

**Fix:** The SABnzbd router is now also mounted at `/dav/api`. Users can set
`nzbdavUrl = http://host:8080/dav` and both paths resolve correctly:
WebDAV at `/dav/…` and the API at `/dav/api`.

### Added

- **AIOStreams integration documentation** in README and TEST_STACK.md
- **AIOStreams E2E Docker Compose stack** (`docker-compose.e2e.yml`) — wires
  nzbdav-rs, NZBHydra2, AIOStreams, and Stremio for full pipeline testing
- **Regression tests** for all three bug fixes:
  - `test_enqueue_nzb_stores_job_name_from_filename` — verifies the nzo_id in
    the addurl response matches the UUID stored in the queue (Bug #1 regression)
  - `test_history_nzo_ids_filter_returns_only_matching` — verifies `nzo_ids`
    filters history to exactly the requested item (Bug #3 regression)
  - `test_history_item_id_matches_queue_item_id` — verifies that a history item
    inserted with a known UUID is findable by that UUID via the API (Bug #1
    API-level regression)
  - `test_propfind_hrefs_include_mount_prefix` — injects an `OriginalUri` of
    `/dav/` onto a PROPFIND request to `/` and asserts all response hrefs carry
    the `/dav` prefix (Bug #2 regression)

---

## Previous releases

No prior CHANGELOG entries. See git log for historical changes.
