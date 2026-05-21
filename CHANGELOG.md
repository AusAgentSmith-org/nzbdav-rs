# Changelog

All notable changes to nzbdav-rs are documented here.

---

## [Unreleased] — AIOStreams compatibility

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
