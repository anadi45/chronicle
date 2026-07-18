# Chronicle

Chronicle is a Windows-first, local-first computer memory engine. It persists raw activity locally before any optional AI processing.

## Phase 1 status

- Tauri + React desktop shell
- Rust backend entry point with structured logging
- SQLite database with raw events, FTS5, semantic events, and Processing Queue tables
- Timeline placeholder ready for Phase 2 capture providers

## Development

```powershell
npm install
npm run build
npm run tauri dev
```

The Tauri CLI and Windows WebView2 are required for the desktop run. Database files are created beside the application process during development and are excluded from version control.

## Privacy invariant

Capture and persistence must remain fast and reliable even when local AI inference is slow or unavailable. Keyboard capture will be opt-in and privacy exclusions will be implemented before enabling global hooks.

