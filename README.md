# Chronicle

Chronicle is a Windows-first, local-first computer memory engine. It persists raw activity locally before any optional AI processing.

## Current implementation status

- Tauri + React desktop shell
- Rust backend entry point with structured logging
- SQLite raw-event repository with FTS5 search triggers
- Tauri commands for listing, searching, recording, counting, and deleting events
- Live Timeline, Search, and privacy/settings views
- Seed event for first-run health verification
- Capture provider contracts and privacy-safe defaults
- Processing Queue task/status contracts with exponential retry policy
- Persistent capture settings in SQLite
- JSON export command for local event data

The concrete Windows hooks, Processing Queue workers, screenshots, local model runtimes, embeddings, and installer hardening are still in progress. Settings persistence and export are now available through the backend commands.

## Development

```powershell
npm install
npm run build
npm test
npm run test:frontend
npm run tauri dev
```

### Windows startup troubleshooting

If the Rust build succeeds but the app exits with `0xc0000139 (STATUS_ENTRYPOINT_NOT_FOUND)`, ensure `WebView2Loader.dll` and the generated `chronicle_lib.dll` are available in both `src-tauri/target/debug` and `src-tauri` when launching through Cargo. The WebView2 Runtime must also be installed. The current development copies are present on this machine.

`npm test` runs the Rust repository test suite. It currently covers schema creation, event ordering, FTS search, idempotent first-run seeding, and deletion. `npm run test:frontend` runs the TypeScript compiler in no-emit mode.

## Development milestones

1. Raw event persistence and UI — implemented
2. Windows foreground/window capture — provider contract ready; native hook next
3. Keyboard, mouse, UI Automation, and filesystem providers
4. Screenshot lifecycle and Processing Queue
5. Gemma analysis, Nomic embeddings, and hybrid search
6. Permissions, export/delete controls, benchmarks, and installer hardening

The Tauri CLI and Windows WebView2 are required for the desktop run. Database files are created beside the application process during development and are excluded from version control.

## Privacy invariant

Capture and persistence must remain fast and reliable even when local AI inference is slow or unavailable. Keyboard capture will be opt-in and privacy exclusions will be implemented before enabling global hooks.

## Current privacy controls

- Foreground, mouse, and keyboard metadata capture are independently opt-in.
- Keyboard capture stores metadata only; text capture is not enabled.
- Applications and filesystem paths can be excluded before capture events are persisted.
- Watched-folder capture is limited to explicitly selected folders and records file metadata, not file contents.
- Export produces local JSON data; delete-all permanently removes local raw, semantic, embedding, and queue records after confirmation.
- Queue retries are persisted with attempt counts and retry timestamps, so transient failures do not spin continuously after restart.
