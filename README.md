# Chronicle

Chronicle is a Windows-first, local-first computer memory engine. It watches your activity — foreground applications, window titles, filesystem changes, and (opt-in) mouse clicks, keyboard metadata, and screenshots — and persists it as raw evidence on your own machine before any AI processing ever touches it. A local LLM then turns that raw evidence into searchable semantic insights: a Timeline and Search view show only those processed insights, never the raw capture stream, which stays internal to persistence and diagnostics.

Everything runs on-device. There is no cloud processing, no telemetry, and no external service in the loop at any point.

## Architecture

The capture path is intentionally independent from the AI path:

```text
Windows provider -> normalized raw event -> capture_writer -> SQLite -> Processing Queue -> semantic event/search index
```

Raw events are append-only evidence, persisted before any AI processing runs. Semantic events reference their source raw event and can be regenerated when models change; deleting or reprocessing semantic data never touches the raw record it was derived from.

Every capture source (mouse/keyboard hooks, `SetWinEventHook` foreground tracking, `notify`-based filesystem watching) sends normalized events to a single `capture_writer` thread over a channel rather than touching SQLite directly, so a slow database write or a busy AI worker can never stall a low-level input hook — which, if blocked, would visibly lag the whole system, not just Chronicle. Reads (Timeline, Search, Diagnostics) go through a separate `r2d2` read-only connection pool so UI queries never contend with capture writes.

Captured events enqueue local Gemma analysis followed by EmbeddingGemma embedding generation. AI work runs in bounded homogeneous batches of up to eight queue items, with a minimum pause between batches and a brief step-aside while the user is actively clicking or typing, so local inference never pegs the CPU/GPU or competes with active use. Only context-bearing events — window/app focus changes and filesystem activity — are ever queued for analysis; mouse and keyboard activity is recorded for Raw Evidence but never reaches the model. Screenshot analysis is single-image and memory-bounded: the frame is captured the moment a window gains focus (while it is guaranteed to still be on screen) and held in a small in-memory cache until the queue processes it, rather than re-captured later — window-handle events use Windows Graphics Capture with D3D11 texture readback for Gemma vision analysis, with an in-memory GDI/CPU fallback for unsupported or unavailable windows.

### Local AI engine

Local inference runs on a bundled [llama.cpp](https://github.com/ggml-org/llama.cpp) engine (`llama-server`) rather than a separately installed application — there's no Start Menu entry, tray icon, or Windows "installed apps" listing for it. It's a plain executable that Chronicle downloads once into `%LOCALAPPDATA%\Chronicle\llama` and runs itself as an ordinary child process, the same way it manages its own capture threads.

Two local servers run on `127.0.0.1`: one serving **Gemma 3** (chat + vision) via its OpenAI-compatible `/v1/chat/completions` endpoint, one serving **EmbeddingGemma** via `/v1/embeddings` — both models are Gemma-family rather than mixing providers. `CHRONICLE_LLAMA_HOST`, `CHRONICLE_LLAMA_CHAT_PORT`, and `CHRONICLE_LLAMA_EMBED_PORT` override the defaults (`127.0.0.1`, `8090`, `8091`). Gemma text-analysis responses are index-checked in batch requests; any unsupported or malformed batch falls back to individual requests without losing per-event retry/status tracking. Embedding generation is a true server-side batch via `/v1/embeddings`'s array `input`, not a prompting trick.

The **Settings** page drives setup of this engine with a checklist — inference engine downloaded → analysis model → embedding model → engine running — each step a single explicit action with real, byte-accurate progress (from each response's `Content-Length` header) streamed into the UI and mirrored to the backend log, and each downloaded artifact independently removable to free its disk space. The engine binary comes from llama.cpp's own GitHub releases (queried at setup time, so it's always the current release); the analysis model (Gemma 3 4B Instruct + multimodal projector) and embedding model (EmbeddingGemma) come from their Hugging Face GGUF repos. Capture and persistence work with none of this done — the AI queue simply retries until setup is complete.

When Chronicle starts, each server is launched only if its files are present and it isn't already listening, and Chronicle stops only the processes it started when the application closes. The Diagnostics panel shows the configured model names and whether each engine is reachable.

## Privacy

Capture and persistence remain fast and reliable even when local AI inference is slow or unavailable — keyboard and screen capture are opt-in, and privacy exclusions are applied before events are persisted.

- Foreground, mouse, and keyboard metadata capture are independently opt-in.
- Screen capture is independently opt-in and disabled by default; window events use text processing while it is off.
- Keyboard capture stores metadata only; text capture is not enabled by default, and where enabled is restricted to an explicit per-application allowlist.
- Applications and filesystem paths can be excluded before capture events are persisted — application exclusions match on exact executable filename/stem (case-insensitive), and path exclusions match on path-component containment, not raw substring search, so an exclusion of "code" matches `Code.exe` but not `decode.exe`, and an exclusion of "secrets" matches the `\Secrets\` segment but not `\Secretariat\`.
- Watched-folder capture is limited to explicitly selected folders and records file metadata, not file contents.
- Export produces local JSON data; delete-all permanently removes local raw, semantic, embedding, and queue records after confirmation.
- Queue retries are persisted with attempt counts and retry timestamps, so transient failures do not spin continuously after restart.
- The Diagnostics panel reports capture permissions, exclusions, storage counts, queue state, and provider availability.
- Screenshot requests are restricted to explicit meaningful event triggers and remain memory-only through native D3D11/GDI processing.
- Semantic model JSON is size-bounded and schema-validated before persistence; the event inspector exposes raw JSON and source evidence without replacing raw records.

## Performance

- **Mouse capture is click/scroll only.** Movement is never recorded or analyzed — only discrete clicks, double-clicks, right-clicks, and scroll events reach capture, which is what keeps ordinary mouse use from flooding storage or the AI queue.
- **Capture and AI processing are decoupled.** Every capture source sends normalized events to one writer thread (`capture_writer`) over a channel; none of them touch SQLite directly.
- **The database uses WAL with `synchronous=NORMAL`** and indexes on the hot lookup paths (recent events, per-event semantic/queue status), plus a `busy_timeout` so concurrent readers/writers wait briefly instead of erroring.
- **The local engine client reuses keep-alive connections** (via `ureq`) instead of opening a new TCP connection per inference call, and correctly handles HTTP status codes and chunked responses.

## Development

```powershell
npm install
npm run build
npm test
npm run test:frontend
npm run tauri dev
```

`npm test` runs the Rust repository test suite — schema creation, event ordering, FTS search, idempotent first-run seeding, deletion, and more. `npm run test:frontend` runs the TypeScript compiler in no-emit mode.

Search uses the `semantic_events_fts` index across processed categories, summaries, entities, and relationships; raw evidence has no FTS index. The Timeline includes a separate Raw Evidence page for diagnostics; raw records are never mixed into processed insight search results.

Capture workers automatically restart on application launch when capture was previously enabled.

The Tauri CLI and Windows WebView2 are required for the desktop run. Database files are created beside the application process during development and are excluded from version control. Production bundles are enabled in `src-tauri/tauri.conf.json` and use the checked-in platform icon assets.

Run `powershell -ExecutionPolicy Bypass -File scripts/release-smoke.ps1` on Windows to validate frontend checks, Rust tests, production build, NSIS packaging, and a short runtime startup check together. Use `scripts/benchmark.ps1` for repeatable persistence, search, queue, and frontend timing baselines. The runtime check is also available independently at `scripts/windows-runtime-smoke.ps1`. With Capture enabled, `scripts/windows-capture-acceptance.ps1` launches Notepad and verifies that foreground events reach SQLite; this acceptance script requires Python 3 for its SQLite assertion.

### Windows startup troubleshooting

If the Rust build succeeds but the app exits with `0xc0000139 (STATUS_ENTRYPOINT_NOT_FOUND)`, ensure `WebView2Loader.dll` and the generated `chronicle_lib.dll` are available in both `src-tauri/target/debug` and `src-tauri` when launching through Cargo. The WebView2 Runtime must also be installed.
