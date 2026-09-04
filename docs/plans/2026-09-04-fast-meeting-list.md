# Fast meeting list

## Evidence and choice

The homepage fetches 5,155 metadata entries (1.7 MB) before rendering. Finding
one active meeting reads 4,627 `meta.json` files on every request: about 300 ms
versus 12 ms without that scan. Rendering the 426 KTalk cards takes 25 ms.

Three options were considered:

- A writer-maintained active index makes reads cheap but introduces another
  persistent record to synchronize across end/reopen operations and crashes.
- SQLite would provide transactional indexing, but migrating recording and
  external imports exceeds this bottleneck's scope.
- A disposable frontend catalog preserves the existing storage format and
  removes filesystem work from HTTP list requests. This is the chosen design.

## Ownership and consistency

`frontend/meeting_catalog.py` owns the web server's read model. Recording still
owns `meta.json` and `index.json`; external directories remain read-only.

- Build once at startup in a worker thread. Reuse the existing watchdog observer
  for targeted metadata/index invalidations, including atomic rename destinations.
- Coalesce events; serialize refreshes and publish a complete snapshot only after
  a refresh finishes. An event arriving during a refresh remains pending.
- Reconcile every 30 seconds even during continuous events. Cache parsed entries
  by inode, size, mtime and ctime so reconciliation only reopens changed files.
- Retain the last valid entry across malformed or unstable reads and retry in
  one second. A confirmed deletion removes the cached file. Failed directory
  scans retain their previous entries until recovery.
- Preserve index-only historical rows. Existing local metadata takes precedence
  over the completed index, covering the multi-file end/reopen transition without
  duplicate rows. External metadata changes do not depend on directory mtime.
- List requests read the published snapshot without filesystem calls. Compute
  current active duration when serving it. Successful UI deletion/title updates
  refresh the catalog before returning to preserve read-after-write behavior.
- Active scans during Live connection and watcher updates run off the event loop.
  A shared selection lock orders these scans; callers apply the result before
  their next await. Initial transcript loading and watermark seeding remain an
  uninterrupted section. Read-only status/anchor scans also run off-loop.

The browser displays its last successful list while fetching an update. A
versioned, five-minute sessionStorage cache also covers reloads in the same tab.
Refreshing and failed refreshes are visible; a failure preserves saved cards.
Storage failures are optional-cache failures, never page failures. Only the
latest load may replace the visible list or persisted cache. Filtering remains
client-side and the API remains a JSON array; pagination is deferred because
rendering and transfer are not the measured bottleneck.

## Implementation and validation

- [x] Add catalog and HTTP regression tests, including no I/O on warm list reads,
  end/reopen, external edits, atomic replacement, deletion, malformed writes,
  lost events, and an event arriving during a refresh.
- [x] Implement the catalog and its server lifespan/mutation integration.
- [x] Add browser-cache tests and implement immediate saved-list rendering,
  freshness/error status, and overlapping-load protection.
- [x] Run the relevant Python and Node tests, independently review the change,
  restart the service, and measure the actual homepage/API again.
- [x] Commit the server and browser improvements as independently valid changes,
  with their tests and evidence.

## Validation on 2026-09-04

- 136 focused Python tests passed (catalog, HTTP, storage, copilot proxy, live and
  transcript watchers, audio retention, title generation).
- `node --test tests/frontend_meetings.test.cjs`: 12 passed.
- Independent review reproduced and helped fix invalid-index cache poisoning,
  cancellation releasing a refresh lock before its thread exits, and external
  watch failure preventing startup. Final review found no important issues.
- Actual HTTP list requests: previous median 311 ms, final median 14.4 ms over
  10 requests. With a concurrent Live connection: previously 260–320 ms, finally
  12.1 / 18.8 / 15.3 ms. These are local observations, not latency guarantees.
- Browser: first cards in 124 ms with the session cache empty; saved cards in
  58 ms on reload even while the update request was deliberately held. Failed
  refresh preserved all 427 visible KTalk cards and displayed an error status.
- Catalog warm-up took about 0.9 s once at startup; stat-only reconciliation took
  40–46 ms in the background. Snapshot copying plus JSON encoding took 9 ms.
  Missing filesystem events can delay updates until the 30-second reconciliation.
- Compared with the former listing, index-only historical entries survived and
  one completed metadata-only meeting became visible. Metadata also corrects
  existing stale index fields; strict payload equality was intentionally avoided.
- Restarted Recog and verified all four services running and the frontend's
  bridge connection restored. Browser console contained no warnings or errors.
