# Chronicle implementation roadmap

Working checklist for Chronicle's implementation. Update it whenever a task moves between pending, in progress, or complete — this file should always reflect the true state of the code, not the intended state.

Legend: `[x]` complete · `[~]` in progress · `[ ]` pending

---

## 1. Desktop shell and persistence

- [x] Tauri + React desktop shell
- [x] Rust backend with SQLite initialization
- [x] Raw event schema and typed `RawEvent` model
- [x] SQLite FTS5 search and maintenance triggers
- [x] Timeline, Search, Raw Evidence, and Settings/privacy views
- [x] Event recording, listing, searching, counting, deletion, and JSON export commands
- [x] Persistent capture settings table
- [x] Seed event for first-run health verification
- [x] Windows WebView2 development startup fix
- [x] Descriptive, responsibility-named modules with module-level Rustdoc

## 2. Data layer and reliability

- [x] `sqlite-vec` ANN vector similarity search, with a durable binary/JSON brute-force fallback when the extension is unavailable or a query vector's dimensionality doesn't match the index
- [x] `r2d2`/`r2d2_sqlite` read-only connection pool, separate from the single writer connection, so UI reads never contend with capture writes
- [x] Async Tauri commands (`spawn_blocking`) for DB, model-status, and export work — the UI thread is never blocked on rusqlite or local-engine HTTP calls
- [x] Recoverable database-init failure handling: a failed on-disk database open falls back to a transient in-memory database and is surfaced to the UI via `startup_diagnostics`, instead of panicking the process
- [x] WAL journal mode, `synchronous=NORMAL`, `busy_timeout`, and indexes on hot lookup paths (recent events, per-event semantic/queue status)
- [x] All capture sources write through one batching writer thread (`capture_writer`) instead of touching SQLite directly

## 3. Capture engine

- [x] Foreground-window tracking via `SetWinEventHook` (`EVENT_SYSTEM_FOREGROUND`), replacing `GetForegroundWindow` polling
- [x] Filesystem watching via the `notify` crate (`ReadDirectoryChangesW` on Windows), replacing periodic recursive rescans
- [x] Enrich foreground events with executable name and executable path; preserve native window handles
- [x] Persist capture enabled state; automatically restart capture workers on launch when previously enabled
- [x] Application exclusion matching, exact executable filename/stem (case-insensitive), not raw substring — an exclusion of "code" matches `Code.exe` but not `decode.exe`
- [x] Path exclusion matching by path-component containment, not raw substring — an exclusion of "secrets" matches the `\Secrets\` segment but not `\Secretariat\`
- [x] Capture status and last-event health surfaced in the UI

## 4. Input capture (mouse/keyboard)

- [x] Explicit keyboard permission/on-off flow; independent keyboard/mouse permission settings
- [x] Metadata-only keyboard hook; low-level Windows keyboard and mouse hook workers
- [x] Mouse click, double-click, right-click, and scroll events — movement is never captured or analyzed
- [x] Windows message pump isolated from the hook callback so a slow database write can never stall system-wide input
- [x] Allowlisted text capture contract, enforced during keyboard event normalization
- [x] Protected-field/password/security exclusions (raw hooks and UI Automation)
- [x] Text batching contract with 500–1000 ms debounce bounds
- [x] Input hook restart bug fixed (`OnceLock` sender is now re-armable after stop/start via a mutex-guarded slot)
- [ ] Keyboard/mouse acceptance tests on Windows

## 5. UI Automation and filesystem

- [x] Read focused UI Automation element: control type, name, class, framework, bounds, selected text
- [x] Bound selected text and control values before persistence; graceful fallback for inaccessible/elevated applications
- [x] Watched-folder selection; create/modify/delete/rename notifications; path exclusions
- [x] Filesystem snapshot and case-insensitive path exclusion tests

## 6. Screenshots and transient assets

- [x] Windows Graphics Capture probe with D3D11 frame-pool readback; CPU/GDI fallback for unsupported windows
- [x] Screenshot captured at event time (foreground focus change) into a bounded in-memory cache, not re-captured at processing time
- [x] Image bytes held in memory only by default; released after processing or failure
- [x] Screenshot privacy, failure, and transient-expiry tests

## 7. Processing queue and local AI

- [x] Queue insert/claim/complete/fail repository methods; bounded worker loop; retry limit and stop handling
- [x] Crash recovery for `processing` tasks; requeue on graceful shutdown; retry count/timestamp persistence
- [x] Cancellation and bounded backpressure for pending tasks
- [x] Gemma 3 (chat + vision) and EmbeddingGemma served locally by a bundled llama.cpp engine (`llama-server`), not a separately installed application — see section 8
- [x] Structured text/image analysis validation boundaries; model JSON output validated before persistence
- [x] Bounded homogeneous batching (up to 8 items) for text analysis, with per-event retry/status tracking preserved across batch fallback; embedding generation batches natively via `/v1/embeddings`' array input
- [x] AI worker paces itself between batches and steps aside while the user is actively clicking/typing
- [x] Local engine client uses a keep-alive `ureq` agent with status-code checks and correct chunked-response handling
- [x] Hybrid FTS5/vector ranking; durable binary embedding fallback with JSON compatibility
- [x] Processing metrics (latency, error counters, snapshot/reset)

## 8. Local AI engine (llama.cpp) and its in-app setup

- [x] Bundled `llama-server` runtime — no separate installer, tray icon, or Start Menu entry; downloaded from llama.cpp's own GitHub releases (CPU-only Windows build, queried at setup time so it's always the current release) and run as a plain Chronicle-managed child process
- [x] Two local servers on `127.0.0.1`: chat/vision (Gemma 3 4B + multimodal projector) and embeddings (EmbeddingGemma), both OpenAI-compatible (`/v1/chat/completions`, `/v1/embeddings`)
- [x] In-app one-time setup: Settings shows an engine-downloaded / analysis-model / embedding-model / engine-running checklist, each step independently downloadable and removable, with real byte-accurate progress (from each response's `Content-Length`, not estimated) streamed to the UI and mirrored to `tracing`
- [x] Removing a downloaded artifact stops the server using it first (a running process keeps its executable/model files locked on Windows)
- [x] Vision analysis via the OpenAI-compatible `image_url` (base64 data URI) content part, gated on the server being started with `--mmproj`
- [ ] **Not yet live-tested end-to-end.** URLs, CLI flags, and request/response shapes were verified against llama.cpp's current documentation and GitHub releases, but the actual multi-gigabyte downloads, extraction, and inference — especially the vision path, which llama.cpp's own docs describe as experimental — have not been exercised in this environment. Run the full setup checklist once on a real Windows machine and fix whatever doesn't match.
- [ ] GPU acceleration (CUDA/Vulkan builds) is not automatic — the engine always downloads the CPU-only build regardless of hardware. Detecting a usable GPU and offering the matching build is unstarted.
- [ ] No model-swap/version-upgrade path yet — replacing a model means removing it and downloading a (possibly future, differently-named) replacement by hand; there's no "update available" signal.

## 9. UI and diagnostics

- [x] Semantic-event FTS search; Timeline/Search show only processed insights, never raw capture
- [x] Separate Raw Evidence page; event inspector with raw JSON and source evidence
- [x] Queue status and permission diagnostics panel; consolidated diagnostics command
- [x] Watched-folder, excluded-application, and excluded-path editors (independent, not conflated)
- [x] Export-to-JSON and delete-all wired to the UI with confirmation
- [x] Storage usage, model/provider status, and processing queue limits commands

## 10. Hardening and release

- [x] Bounded FTS search baseline at 1,000 events; raw persistence and queue latency baselines
- [x] Reproducible Windows release smoke-test workflow; Windows installer icon/resources
- [x] Forced-termination and queue-recovery test; 1,000+ event and memory-growth baseline
- [~] End-to-end Windows capture tests — a reproducible foreground-event acceptance harness exists; restricted/elevated scenarios remain manual
- [~] Benchmarks for persistence, queue latency, search, and frontend build — native screenshot timing remains environment-dependent
- [ ] Elevated apps, UAC, secure desktop, protected windows, and games
- [ ] Windows Defender/antivirus interaction testing

---

## Known issues

Tracked defects that are understood but not yet fixed. Verify current behavior before relying on this list — it reflects the last audit, not a continuously re-checked state.

- [ ] **Raw-event search ignores its query.** `Database::recent_events(limit, query)` never applies `query` — a regression test (`raw_event_listing_does_not_search_private_evidence`) currently encodes this as expected behavior. Either implement filtering on raw evidence or remove the unused parameter.
- [ ] **Input-hook events carry no app identity.** Mouse/keyboard hook events don't set `app_name`/`executable_path`, so `excluded_applications` cannot filter them — exclusions are only enforced for foreground/filesystem events today. This is a privacy-invariant gap worth closing.
- [ ] **Keyboard text allowlist is not wired to the live hook.** `MetadataTextBatcher`, `normalize_allowlisted_keyboard_event`, and `KeyboardMode::AllowlistedText`/`FullText` exist but the keyboard hook always calls the metadata-only normalizer — the Settings allowlist editor currently has no effect on capture.
- [ ] **Queue-full behavior misreports raw-event persistence.** When the queue is at its cap, `enqueue_task` fails and `insert_event_and_enqueue` surfaces that as "failed to persist" even though the raw event was written successfully.
- [ ] **Shutdown can block on a long-running model call.** `CloseRequested` joins capture/worker threads on the main thread; if the AI worker is mid-read on a slow model response, app close can stall until that read completes or times out.
- [ ] **FTS queries can raise a syntax error on ordinary input.** Only `"` is stripped before building the `MATCH` query; characters like `-`/`*` or an unbalanced quote can produce an FTS5 syntax error that the client currently swallows into an empty result list instead of surfacing.
- [ ] **JSON export is raw-events-only.** `export_json` exports raw events (capped at 100k) but not semantic events, embeddings, or settings, while the UI presents "Export JSON" as a full local data export.
- [ ] **`record_event` trusts its caller.** Any webview-side code can insert arbitrary raw events and enqueue AI work through this command; acceptable within the current single-user local-trust model, but worth revisiting if a plugin/extension surface is ever added.
- [ ] **Mixed processing-queue batches only advance the first task when an image task is present**, leaving the rest in `processing` until the stale-task recovery sweep picks them up.
- [ ] **The llama.cpp release-asset lookup assumes a stable naming pattern** (`*bin-win-cpu-x64*.zip`). If upstream ever renames or restructures Windows release assets, `setup_download_runtime` will fail with a clear "no Windows CPU build found" error rather than silently downloading the wrong thing — but it will need a code update to match the new pattern.
- [ ] **No checksum/signature verification on downloaded artifacts.** Downloads go straight from GitHub/Hugging Face over HTTPS with no separate integrity check; acceptable given both are the canonical official sources, but worth revisiting if a mirror or proxy is ever introduced.

## Future scope

Not started. Candidates for the next phase of work, roughly in priority order.

- [ ] Rewrite `list_raw_event_processing_overview` as JOIN queries instead of one query plus N per-event lookups.
- [ ] Push-based UI updates (Tauri events for new insights / queue-count changes) instead of manual refresh and mount-time-only fetches.
- [ ] Virtualize the Timeline list once result sets grow beyond a few hundred rows.
- [ ] Stream `export_data` to a user-chosen file instead of building the full JSON string in memory.
- [ ] Idle-aware processing that pauses the queue worker during sustained active input and resumes on idle, beyond the existing per-batch pacing.
- [ ] macOS capture providers (`mac.rs` modules are currently minimal stubs behind `cfg(not(windows))`) — no timeline commitment; Chronicle remains Windows-first.
- [ ] sqlite-vec index maintenance tooling (rebuild/repair) for recovering from a corrupted or missing vector index without a full data reset.
- [ ] Structured application-level telemetry opt-in for crash/error reporting — explicitly out of scope until there is a documented, privacy-reviewed design (see `AGENTS.md` product invariants).

---

## Verification commands

```powershell
npm run test:frontend
npm test
npm run build
npm run tauri dev
```
