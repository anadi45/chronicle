# Chronicle

Chronicle is a Windows-first, local-first computer memory engine. It persists raw activity locally before any optional AI processing.

## Current implementation status

- Tauri + React desktop shell
- Rust backend entry point with structured logging
- SQLite raw-event repository with FTS5 search triggers
- Tauri commands for listing, searching, recording, counting, and deleting events
- Live Timeline, Search, and privacy/settings views
- Timeline and Search display only processed semantic insights; raw capture evidence remains internal to persistence and processing
- Seed event for first-run health verification
- Capture provider contracts and privacy-safe defaults
- Processing Queue task/status contracts with exponential retry policy
- Persistent capture settings in SQLite
- JSON export command for local event data
- `sqlite-vec` ANN vector similarity search for embeddings, with a durable binary/JSON brute-force fallback when the extension is unavailable or a query vector's dimensionality does not match the index
- Separate `r2d2`/`r2d2_sqlite` read-only connection pool for UI query commands, kept apart from the single writer connection used by capture/processing so reads never block writes (or each other) on the database mutex
- Async Tauri commands (`tauri::async_runtime::spawn_blocking`) for DB, model-status, and export work, so the UI thread is never blocked on rusqlite or local-engine HTTP calls
- Event-driven filesystem watching via the `notify` crate (`ReadDirectoryChangesW` on Windows) instead of periodic recursive rescans
- Event-driven foreground-window tracking via `SetWinEventHook` (`EVENT_SYSTEM_FOREGROUND`) instead of polling `GetForegroundWindow`
- Recoverable database-init failure handling: a failed on-disk database open falls back to a transient in-memory database and is surfaced to the UI via `startup_diagnostics` instead of panicking the process

The concrete Windows hooks, Processing Queue workers, screenshots, local model runtimes, embeddings, and installer hardening are implemented. Settings persistence and export are available through the backend commands.

Local inference runs on a bundled [llama.cpp](https://github.com/ggml-org/llama.cpp) engine (`llama-server`), not a separately installed application — see [Local model setup](#local-model-setup). Two local servers run on `127.0.0.1`: one serving Gemma 3 (chat + vision) via its OpenAI-compatible `/v1/chat/completions` endpoint, one serving EmbeddingGemma via `/v1/embeddings`. Set `CHRONICLE_LLAMA_HOST`, `CHRONICLE_LLAMA_CHAT_PORT`, and `CHRONICLE_LLAMA_EMBED_PORT` to override the defaults (`127.0.0.1`, `8090`, `8091`).
Captured events enqueue local Gemma analysis followed by EmbeddingGemma embedding generation. If the engine or either model is unavailable, queue retries are used and capture continues.
AI work is performed in bounded homogeneous batches of up to eight queue items, with a minimum pause between batches and a brief step-aside while the user is actively clicking or typing, so local inference never pegs the CPU/GPU or competes with active use. Only context-bearing events — window/app focus changes and filesystem activity — are ever queued for analysis; mouse and keyboard activity is recorded for Raw Evidence but never reaches the model. Gemma text-analysis responses are index-checked; any unsupported or malformed batch falls back to individual requests without losing per-event retry/status tracking. Embedding generation is a true server-side batch via `/v1/embeddings`' array `input`, not a prompting trick. Screenshot analysis remains single-image and memory-bounded: the frame is captured the moment a window gains focus (while it is guaranteed to still be on screen) and held in a small in-memory cache until the queue processes it, rather than re-captured later.

## Performance

- **Mouse capture is click/scroll only.** Movement is never recorded or analyzed — only discrete clicks, double-clicks, right-clicks, and scroll events reach capture, which is what keeps ordinary mouse use from flooding storage or the AI queue.
- **Capture and AI processing are decoupled.** Every capture source (mouse/keyboard hooks, foreground `SetWinEventHook`, filesystem watching) sends normalized events to one writer thread (`capture_writer`) over a channel; none of them touch SQLite directly, so a slow database write or a busy AI worker can never stall the low-level input hooks (which, if blocked, would visibly lag the whole system, not just Chronicle).
- **The database uses WAL with `synchronous=NORMAL`** and indexes on the hot lookup paths (recent events, per-event semantic/queue status), plus a `busy_timeout` so concurrent readers/writers wait briefly instead of erroring.
- **The local model client reuses keep-alive connections** (via `ureq`) instead of opening a new TCP connection per inference call, and correctly handles HTTP status codes and chunked responses.

## Architecture

The capture path is intentionally independent from the AI path:

```text
Windows provider -> normalized raw event -> capture_writer -> SQLite -> Processing Queue -> semantic event/search index
```

Raw events are append-only evidence, persisted before any AI processing runs. Semantic events reference their source raw event and can be regenerated when models change; deleting or reprocessing semantic data never touches the raw record it was derived from.

Every capture source (mouse/keyboard hooks, `SetWinEventHook` foreground tracking, `notify`-based filesystem watching) sends normalized events to the single `capture_writer` thread over a channel rather than touching SQLite directly, so a slow database write or a busy AI worker can never stall a low-level input hook. Reads (Timeline, Search, Diagnostics) go through a separate `r2d2` read-only connection pool so UI queries never contend with capture writes.

## Local model setup

Chronicle does not depend on a separately installed AI application. There's no Start Menu entry, tray icon, or Windows "installed apps" listing for the inference engine — it's a plain `llama-server.exe` (from [llama.cpp](https://github.com/ggml-org/llama.cpp)) that Chronicle downloads once into `%LOCALAPPDATA%\Chronicle\llama` and runs itself as an ordinary child process, the same way it manages its own capture threads.

The **Settings** page shows a "Local AI setup" checklist — inference engine downloaded → analysis model → embedding model → engine running — with a button for each unmet step:

1. **Inference engine**: downloads the latest Windows CPU build of `llama-server` from [llama.cpp's GitHub releases](https://github.com/ggml-org/llama.cpp/releases) (queried at setup time, so it always fetches the current release rather than one pinned into Chronicle's code) and extracts it.
2. **Analysis model**: downloads Gemma 3 4B Instruct (chat + vision, Q4_K_M, ~2.5 GB) plus its multimodal projector (~850 MB), from [`bartowski/google_gemma-3-4b-it-GGUF`](https://huggingface.co/bartowski/google_gemma-3-4b-it-GGUF) — a community re-upload of Google's weights. Google's own [`gemma-3-4b-it-qat-q4_0-gguf`](https://huggingface.co/google/gemma-3-4b-it-qat-q4_0-gguf) repo is access-gated (requires a Hugging Face login and accepting a license agreement, returning HTTP 401 for an anonymous download), so this setup flow uses the openly downloadable mirror instead.
3. **Embedding model**: downloads [`EmbeddingGemma`](https://huggingface.co/ggml-org/embeddinggemma-300M-GGUF) (~330 MB) — a small Gemma-family embedding model, so both the analysis and embedding paths are Gemma models rather than mixing model families.
4. **Engine running**: starts two `llama-server` processes on `127.0.0.1` (chat/vision on `8090`, embeddings on `8091` by default).

Every download streams real, byte-accurate progress into the UI (`llama-setup-progress` Tauri events, computed from each response's `Content-Length` header — not estimated from log text) and into the same `tracing` log the `npm run dev` terminal shows. Each step is a single explicit action; nothing downloads, installs, or starts silently in the background. Every downloaded artifact — the engine binary, the analysis model, the embedding model — can be removed independently from the same checklist, freeing its disk space; removing a running engine's files stops that server first (Windows locks a running executable's file, so deleting out from under a live process would fail anyway).

Capture and persistence work with none of this done; the AI queue simply retries until setup is complete, per Chronicle's product invariant that capture must stay useful when AI is disabled, slow, or unavailable.

When Chronicle starts, each server is launched only if its files are present and it isn't already listening; Chronicle stops only the processes it started when the application closes. The Diagnostics panel shows the configured model names and whether each engine is reachable. `CHRONICLE_LLAMA_HOST`, `CHRONICLE_LLAMA_CHAT_PORT`, and `CHRONICLE_LLAMA_EMBED_PORT` can override the defaults.

CPU-only inference is the current default — it's the one build that works on every Windows machine without detecting hardware first. GPU acceleration (CUDA/Vulkan builds, also published by llama.cpp) is tracked as follow-up work in `TODO.md`, not yet automatic.

**Not yet live-tested.** This flow was built and verified to compile/typecheck/pass its test suite, and its URLs and API shapes were verified against llama.cpp's current documentation and GitHub releases, but the actual multi-gigabyte downloads, extraction, and inference (especially the vision path — llama.cpp's own docs describe multimodal HTTP support as experimental) have not been exercised end-to-end in this environment. Exercise the full checklist once and report back if any step doesn't behave as documented here.

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

Search uses the `semantic_events_fts` index across processed categories, summaries, entities, and relationships; raw evidence has no FTS index.
The Timeline includes a separate Raw Evidence page for diagnostics; raw records are never mixed into processed insight search results.

Capture workers automatically restart on application launch when capture was previously enabled.

Window-handle events use Windows Graphics Capture with D3D11 texture readback for Gemma vision analysis, with an in-memory GDI/CPU fallback for unsupported or unavailable windows.

## Development milestones

1. Raw event persistence and UI — implemented
2. Windows foreground/window capture — event-driven via `SetWinEventHook`
3. Keyboard, mouse, UI Automation, and filesystem providers
4. Screenshot lifecycle and Processing Queue
5. Gemma analysis, EmbeddingGemma embeddings, and hybrid search — on a bundled llama.cpp engine, not a separately installed application
6. Permissions, export/delete controls, benchmarks, and installer hardening

The Tauri CLI and Windows WebView2 are required for the desktop run. Database files are created beside the application process during development and are excluded from version control.

Production bundles are enabled in `src-tauri/tauri.conf.json` and use the checked-in platform icon assets.

Run `powershell -ExecutionPolicy Bypass -File scripts/release-smoke.ps1` on Windows to validate frontend checks, Rust tests, production build, NSIS packaging, and a short runtime startup check together. Use `scripts/benchmark.ps1` for repeatable persistence, search, queue, and frontend timing baselines. The runtime check is also available independently at `scripts/windows-runtime-smoke.ps1`. With Capture enabled, `scripts/windows-capture-acceptance.ps1` launches Notepad and verifies that foreground events reach SQLite; this acceptance script requires Python 3 for its SQLite assertion.

## Privacy invariant

Capture and persistence remain fast and reliable even when local AI inference is slow or unavailable. Keyboard and screen capture are opt-in, and privacy exclusions are applied before events are persisted.

## Current privacy controls

- Foreground, mouse, and keyboard metadata capture are independently opt-in.
- Screen capture is independently opt-in and disabled by default; window events use text processing while it is off.
- Keyboard capture stores metadata only; text capture is not enabled.
- Applications and filesystem paths can be excluded before capture events are persisted.
- Watched-folder capture is limited to explicitly selected folders and records file metadata, not file contents.
- Export produces local JSON data; delete-all permanently removes local raw, semantic, embedding, and queue records after confirmation.
- Queue retries are persisted with attempt counts and retry timestamps, so transient failures do not spin continuously after restart.
- The Diagnostics action in the desktop shell reports capture permissions, exclusions, storage counts, queue state, and provider availability.
- Queue status is available from Diagnostics, including pending, processing, completed, failed, and cancelled task counts.
- Screenshot requests are restricted to explicit meaningful event triggers and remain memory-only through native D3D11/GDI processing.
- Semantic model JSON is size-bounded and schema-validated before persistence; the event inspector exposes raw JSON and source evidence without replacing raw records.
