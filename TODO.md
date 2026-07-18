# Chronicle implementation roadmap

This file is the working implementation checklist. Update it when a feature is started, verified, or blocked.

Legend: `[x]` complete, `[~]` in progress, `[ ]` pending.

## Completed

- [x] Tauri + React desktop shell
- [x] Rust backend with SQLite initialization
- [x] Raw event schema and typed `RawEvent` model
- [x] SQLite FTS5 search and maintenance triggers
- [x] Timeline and Search views
- [x] Settings/privacy view foundation
- [x] Event recording, listing, searching, counting, deletion, and JSON export commands
- [x] Persistent capture settings table
- [x] Capture provider contracts
- [x] Queue task/status contracts and exponential retry policy
- [x] Foreground-window polling lifecycle and Start/Stop commands
- [x] Rust repository/capture/queue tests
- [x] TypeScript validation and production frontend build
- [x] Windows WebView2 development startup fix
- [x] Descriptive module names and module-level Rustdoc
- [x] Platform capture folders with shared `mod.rs` and Windows extension modules
- [x] Preserve native foreground window handles in raw events

## Current priority: capture engine

- [x] Enrich foreground events with executable name and executable path
- [x] Persist capture enabled state when Start/Stop is used
- [x] Add foreground provider shutdown on application exit
- [x] Add application exclusion matching and tests
- [x] Add process/window handle fields to the public event model
- [x] Add capture status and last-event health to the UI

## Input capture

- [~] Add explicit keyboard permission/on-off flow
- [~] Implement metadata-only keyboard hook
- [x] Add normalized keyboard/mouse event contracts and privacy metadata
- [ ] Implement allowlisted text capture
- [ ] Add protected-field/password/security exclusions
- [ ] Add text batching and 500–1000 ms debounce
- [~] Implement mouse click, double-click, right-click, scroll, and drag events
- [~] Add isolated Windows low-level mouse hook worker
- [ ] Add keyboard/mouse acceptance tests on Windows

## UI Automation and filesystem

- [ ] Read focused UI Automation element
- [ ] Capture control type, name, value, class, framework, and bounds
- [ ] Capture selected text where available
- [ ] Add graceful fallback for inaccessible/elevated applications
- [ ] Add watched-folder selection
- [ ] Implement create/modify/delete/rename notifications
- [ ] Add path exclusions and filesystem tests

## Screenshots and transient assets

- [ ] Integrate Windows Graphics Capture
- [ ] Trigger capture after meaningful events
- [ ] Keep image bytes in memory only by default
- [ ] Associate transient assets with raw events/queue tasks
- [ ] Release assets after processing or failure
- [ ] Add disabled debug-retention mode
- [ ] Add screenshot privacy and failure tests

## Processing Queue

- [ ] Add queue insert/claim/complete/fail repository methods
- [ ] Add bounded worker loop
- [ ] Add crash recovery for `processing` tasks
- [ ] Add retry count and retry timestamp persistence
- [ ] Add queue backlog/progress commands and UI
- [ ] Add cancellation and backpressure
- [ ] Test capture while workers are busy

## Local AI and semantic events

- [ ] Add Gemma provider configuration and model discovery
- [ ] Implement structured text analysis
- [ ] Implement image analysis
- [ ] Validate model JSON output
- [ ] Add model version/latency/error metrics
- [ ] Ensure AI failures never stop capture
- [ ] Add Nomic Embed Text provider
- [ ] Add sqlite-vec storage and similarity search
- [ ] Add hybrid FTS5/vector ranking

## UI completion

- [ ] Event inspector with raw JSON and source evidence
- [ ] Semantic event display and model metadata
- [ ] Queue status page
- [ ] Permission diagnostics page
- [ ] Watched-folder and excluded-application editors
- [ ] Wire export button to file-save dialog
- [ ] Wire delete-all button with confirmation
- [ ] Storage usage and model status indicators

## Hardening and release

- [ ] Add end-to-end Windows capture tests
- [ ] Benchmark raw persistence, screenshot capture, queue latency, and search
- [ ] Test 1,000+ events and memory growth
- [ ] Test forced termination and queue recovery
- [ ] Test elevated apps, UAC, secure desktop, protected windows, and games
- [ ] Add Windows installer icon/resources
- [ ] Test Windows Defender/antivirus interactions
- [ ] Document permissions and privacy behavior
- [ ] Finalize export/delete/data-retention policy

## Verification commands

```powershell
npm run test:frontend
npm test
npm run build
npm run tauri dev
```
