# Chronicle — Windows-First Complete Technical Plan

## 1. Product definition

Chronicle is a local-first computer memory engine. Version 0.1 captures detailed user activity on Windows, persists raw evidence locally, and asynchronously converts that evidence into semantic events.

The system must remain useful even when AI processing is disabled, slow, unavailable, or incorrect.

### Core statement

> Chronicle records what happened on the computer first. AI later explains what it meant.

### Version 0.1 scope

- Windows only
- Tauri desktop application
- Rust-first implementation
- Local SQLite database
- SQLite FTS5 for text search
- sqlite-vec for vector search
- Gemma 3 4B IT for text and image analysis
- Nomic Embed Text v1.5 for embeddings
- Local-only processing
- No browser extension
- No cloud LLM
- No screenshots or video retained permanently by default
- No multi-device synchronization

## 2. System architecture

```text
Windows capture providers
        ↓
Rust event normalizer
        ↓
Raw event persistence
        ↓
Processing Queue
        ├── Gemma text analysis
        ├── Gemma image analysis
        └── Nomic embedding generation
                    ↓
       semantic_events + sqlite-vec
                    ↓
             Timeline / Search UI
```

The capture path and AI path are independent.

```text
Capture event → SQLite immediately → return
                         ↓
                  Process later
```

AI inference must never block keyboard, mouse, screen, or application capture.

## 3. Recommended repository structure

```text
chronicle/
├── apps/
│   └── desktop/
│       ├── src/
│       │   ├── pages/
│       │   ├── components/
│       │   ├── stores/
│       │   ├── hooks/
│       │   └── types/
│       └── src-tauri/
│           ├── src/
│           │   ├── main.rs
│           │   ├── commands.rs
│           │   ├── capture/
│           │   │   ├── mod.rs
│           │   │   ├── foreground_window.rs
│           │   │   ├── input.rs
│           │   │   ├── ui_automation.rs
│           │   │   ├── filesystem.rs
│           │   │   └── screen.rs
│           │   ├── db/
│           │   │   ├── mod.rs
│           │   │   ├── migrations.rs
│           │   │   └── repositories.rs
│           │   ├── queue/
│           │   │   ├── mod.rs
│           │   │   ├── worker.rs
│           │   │   └── retry.rs
│           │   ├── inference/
│           │   │   ├── gemma.rs
│           │   │   ├── embeddings.rs
│           │   │   └── schemas.rs
│           │   └── assets/
│           └── migrations/
├── models/
├── scripts/
├── docs/
│   ├── architecture.md
│   ├── privacy.md
│   ├── permissions.md
│   └── benchmarks.md
└── README.md
```

Use Rust for the backend and Windows integration. Do not add a separate Swift-style helper; Windows APIs can be accessed through Rust crates and Windows FFI bindings.

## 4. Windows capture providers

### 4.1 Foreground application and window provider

Capture the currently focused application and window.

Required fields:

- Process ID
- Executable path
- Executable name
- Window handle
- Window title
- Process start time
- Foreground start time
- Monitor identifier
- Window bounds

Event types:

```text
app_activated
window_focused
window_title_changed
app_closed
```

Use a foreground-window listener where possible. Use a low-frequency poll only as a recovery mechanism.

### 4.2 Windows UI Automation provider

Use Windows UI Automation to inspect the focused element and nearby control hierarchy.

Capture where available:

- Automation ID
- Control type
- Element name
- Class name
- Framework ID
- Bounding rectangle
- Value pattern
- Text pattern
- Selection pattern
- Toggle state
- Parent window

This may identify a button, editor, tab, list item, document, browser control, or text field without requiring a screenshot.

Event types:

```text
element_focused
element_value_changed
text_selected
button_invoked
document_changed
```

Applications expose different levels of UI Automation data. Treat unavailable data as normal rather than as an error.

### 4.3 Keyboard provider

Capture system-wide keyboard events only after explicit user opt-in.

Capture:

- Key down/up
- Virtual key code
- Scan code
- Modifier state
- Unicode representation when available
- Foreground application
- Foreground window
- Timestamp

Default modes:

```text
metadata_only
text_capture_for_allowlisted_apps
full_text_capture
```

Never persist raw input from:

- Password fields
- Credential managers
- Banking/payment fields
- Windows security dialogs
- UAC/secure desktop
- User-configured excluded applications

The low-level event listener must not be used to inject input or control other applications.

Event types:

```text
key_down
key_up
text_inserted
text_deleted
shortcut_detected
```

Debounce text updates. Persist an editing batch after the user pauses for approximately 500–1000 ms instead of writing one database row per keystroke.

### 4.4 Mouse provider

Capture:

- Left/right/middle button down/up
- Click count
- Screen coordinates
- Window-relative coordinates
- Drag start/end
- Scroll direction and amount
- Modifier state
- Active application/window

Do not persist every mouse-movement event. Use mouse movement only to detect drags or in an explicit diagnostics mode.

Event types:

```text
mouse_click
mouse_right_click
mouse_double_click
mouse_drag_started
mouse_drag_ended
mouse_scroll
```

### 4.5 File-system provider

Use the Windows file notification APIs for user-selected directories.

Capture:

- Created
- Modified
- Deleted
- Renamed
- Path
- Extension
- Size
- Timestamp

Do not claim that a file was edited by the user merely because it changed. Store it as filesystem evidence.

Event types:

```text
file_created
file_modified
file_deleted
file_renamed
```

### 4.6 Screen provider

Use Windows Graphics Capture for screenshots and optional short clips.

Default capture policy:

- Capture active window after meaningful events
- Capture after selection
- Capture after right-click or double-click
- Capture after drag completion
- Capture after window-title changes
- Hold assets in memory only
- Send assets to inference
- Delete assets after processing

Do not record continuous video in the first release.

## 5. Event normalization

Every provider emits the same normalized event envelope.

```rust
struct RawEvent {
    id: String,
    timestamp_ns: i64,
    event_type: String,
    source: String,

    app_name: Option<String>,
    executable_path: Option<String>,
    process_id: Option<u32>,
    window_handle: Option<u64>,
    window_title: Option<String>,
    window_bounds_json: Option<String>,

    automation_id: Option<String>,
    control_type: Option<String>,
    element_name: Option<String>,
    element_value: Option<String>,
    selected_text: Option<String>,

    key_code: Option<u32>,
    text: Option<String>,
    mouse_x: Option<f64>,
    mouse_y: Option<f64>,
    mouse_button: Option<String>,

    file_path: Option<String>,
    metadata_json: String,
    privacy_class: String,
    confidence: f32,
}
```

Raw events are append-only. Do not overwrite them with AI-generated interpretations.

## 6. SQLite schema

### Raw events

```sql
CREATE TABLE raw_events (
    id TEXT PRIMARY KEY,
    timestamp_ns INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    source TEXT NOT NULL,

    app_name TEXT,
    executable_path TEXT,
    process_id INTEGER,
    window_handle INTEGER,
    window_title TEXT,
    window_bounds_json TEXT,

    automation_id TEXT,
    control_type TEXT,
    element_name TEXT,
    element_value TEXT,
    selected_text TEXT,

    key_code INTEGER,
    text TEXT,
    mouse_x REAL,
    mouse_y REAL,
    mouse_button TEXT,
    file_path TEXT,

    metadata_json TEXT NOT NULL,
    privacy_class TEXT NOT NULL,
    confidence REAL NOT NULL,
    created_at TEXT NOT NULL
);
```

### Full-text search

```sql
CREATE VIRTUAL TABLE raw_events_fts USING fts5(
    app_name,
    window_title,
    element_name,
    element_value,
    selected_text,
    text,
    file_path,
    content='raw_events',
    content_rowid='rowid'
);
```

### Semantic events

```sql
CREATE TABLE semantic_events (
    id TEXT PRIMARY KEY,
    raw_event_id TEXT NOT NULL,
    category TEXT NOT NULL,
    summary TEXT NOT NULL,
    entities_json TEXT NOT NULL,
    relationships_json TEXT NOT NULL,
    confidence REAL NOT NULL,
    model_name TEXT NOT NULL,
    model_version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY(raw_event_id) REFERENCES raw_events(id)
);
```

### sqlite-vec

sqlite-vec stores and searches vectors; it does not generate them. Nomic Embed Text v1.5 remains the embedding generator.

```sql
CREATE VIRTUAL TABLE semantic_event_vectors USING vec0(
    semantic_event_id TEXT PRIMARY KEY,
    embedding float[768]
);
```

Use the actual dimension returned by the selected Nomic runtime.

## 7. Processing Queue

Use “Processing Queue” instead of “Analysis Jobs.” It represents every asynchronous post-capture task.

```sql
CREATE TABLE processing_queue (
    id TEXT PRIMARY KEY,
    raw_event_id TEXT NOT NULL,
    task_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    priority INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    model_name TEXT,
    model_version TEXT,
    error TEXT,
    created_at TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,
    FOREIGN KEY(raw_event_id) REFERENCES raw_events(id)
);
```

Task types:

```text
semantic_text_analysis
semantic_image_analysis
embedding_generation
```

Statuses:

```text
pending
processing
complete
failed
cancelled
```

The queue provides:

- Non-blocking capture
- Retry handling
- Crash recovery
- Progress visibility
- Model version tracking
- Backpressure
- Separate text/image/vector workloads

Initial worker policy:

- One Gemma inference worker
- One embedding worker
- Bounded concurrency
- Retry up to three times
- Exponential retry delay
- Never drop persisted raw events

## 8. Local AI runtime

### Gemma 3 4B IT

Use a 4-bit quantized version for the old GTX 1650 machine.

Expected configuration:

```text
Model: Gemma 3 4B IT
Quantization: Q4_K_M or equivalent
Images: one image per request initially
Context: short, bounded context
Concurrency: one request
Output: structured JSON
```

Gemma 3 4B supports text and image input and is positioned for desktop computers and small servers. Its model card specifies image understanding and a 128K maximum context, but Chronicle should use a much smaller operational context to protect performance. [Gemma 3 model card](https://ai.google.dev/gemma/docs/core/model_card_3)

The GTX 1650 is likely usable if it has 4 GB VRAM, but image inference may require partial CPU offload. If the machine has 2 GB VRAM, use CPU-heavy execution or switch to a smaller model.

### Nomic Embed Text v1.5

Use Nomic only after semantic event creation.

```text
semantic event summary
  ↓
Nomic embedding
  ↓
sqlite-vec insertion
```

Run embeddings on CPU initially so Gemma retains GPU resources.

### Model abstraction

Create a provider interface:

```rust
trait MultimodalAnalyzer {
    fn analyze_text(&self, input: TextInput) -> Result<SemanticOutput>;
    fn analyze_image(&self, input: ImageInput) -> Result<SemanticOutput>;
}

trait Embedder {
    fn embed(&self, input: &str) -> Result<Vec<f32>>;
}
```

This allows future model replacement without changing capture or database code.

## 9. In-memory asset lifecycle

Assets are transient by default.

```text
Capture screenshot/frame
  ↓
Keep bytes in memory
  ↓
Submit to Processing Queue
  ↓
Gemma analyzes asset
  ↓
Semantic event saved
  ↓
Memory released
```

Do not store screenshot/video blobs in SQLite.

For development, provide a disabled-by-default debug setting that temporarily retains assets. This is useful for diagnosing model errors but should not be the normal privacy mode.

If the app crashes before processing, the raw event remains but the transient visual asset is intentionally lost.

## 10. Screenshot and video policy

### Screenshots

Capture only after meaningful events:

```text
app_activated
window_title_changed
mouse_double_click
mouse_right_click
text_selected
mouse_drag_ended
element_focused
```

Wait approximately 500 ms after the event before capturing so the resulting UI state is visible.

### Video

Do not continuously record video for v0.1.

If temporal evidence is required later:

- Maintain a 5–10 second in-memory rolling buffer
- Capture at 2–5 FPS
- Save no video by default
- Trigger a temporary clip around a meaningful event
- Send it to Gemma
- Delete it after analysis

Video is more expensive than screenshots because it involves continuous frame delivery, memory pressure, encoding, and model processing. Use it only for dragging, scrolling, menus, animations, or other state transitions.

## 11. Required Windows permissions and runtime constraints

Windows is easier for the first release, but it does not eliminate security boundaries.

### Input monitoring

Required for system-wide keyboard and mouse hooks. The user must explicitly opt in.

### UI Automation access

Required for detailed focused-element and control metadata. Some elevated applications may expose less information to a non-elevated Chronicle process.

### Screen capture

Required for Windows Graphics Capture screenshots and video. Protected surfaces may appear blank or unavailable.

### File access

The user must select directories Chronicle is allowed to watch. Avoid unrestricted disk scanning.

### Administrator/UAC limitations

Do not run Chronicle as administrator by default. Document that elevated applications, UAC prompts, secure desktops, games, and some protected processes may not expose events.

### Antivirus and privacy software

Global keyboard/mouse monitoring may trigger security tools. The application must clearly explain why input monitoring is requested and provide a visible capture-on/off state.

## 12. UI screens

### Timeline

Show:

- Time
- Application
- Window title
- Event type
- Semantic status
- Confidence
- Screenshot availability

### Event inspector

Show:

- Raw event JSON
- Application metadata
- UI Automation metadata
- Keyboard/mouse metadata
- Semantic interpretation
- Model/version
- Processing status

### Search

Support:

- FTS5 keyword search
- Semantic vector search
- Time filters
- Application filters
- Event-type filters

Retrieval flow:

```text
query
  ↓
FTS5 candidates + sqlite-vec candidates
  ↓
merge and rank
  ↓
display semantic events and raw evidence
```

### Settings and permissions

Display:

- Input monitoring status
- UI Automation status
- Screen capture status
- Watched folders
- Excluded applications
- Keyboard capture mode
- Screenshot mode
- Video mode
- Model status
- Queue backlog
- Storage usage
- Delete all data
- Export database

## 13. Performance targets

### Capture

```text
Raw event persistence:       <10 ms target
Event loss under load:       0 in normal operation
Idle CPU:                    <2%
Input capture CPU:           <5%
Screenshot capture:          <150 ms target
```

### AI

```text
Gemma concurrency:           1 request
Image input:                 1 image initially
Embedding concurrency:      1 CPU worker
Capture blocked by AI:       never
Queue memory:                bounded
```

### Storage

Raw events are compact. Temporary assets must not accumulate. Add a queue watchdog that reports memory growth and deletes failed in-memory work.

## 14. Development phases

### Phase 1 — Bootstrap

- Create Tauri + React application
- Add Rust workspace structure
- Add SQLite migrations
- Add logging
- Add Windows build scripts
- Add model download documentation

### Phase 2 — Raw event engine

- Foreground application provider
- Window title provider
- Raw event schema
- SQLite repository
- Timeline of raw events

### Phase 3 — Keyboard and mouse

- Keyboard hook
- Mouse hook
- Click/double-click/right-click detection
- Drag detection
- Debouncing and event coalescing
- Application exclusion list

### Phase 4 — UI Automation

- Focused element lookup
- Element role/name/value
- Selected text
- Control hierarchy metadata
- Graceful fallback when metadata is unavailable

### Phase 5 — Filesystem activity

- Watched-folder selection
- File create/modify/delete/rename events
- Path exclusions
- Event correlation with active application

### Phase 6 — Screenshots

- Windows Graphics Capture integration
- Active-window capture
- In-memory image representation
- Meaningful-event trigger policy
- Screenshot-to-event association

### Phase 7 — Processing Queue

- Queue table
- Worker loop
- Retry handling
- Status UI
- Crash recovery
- Queue throttling

### Phase 8 — Gemma integration

- Load quantized Gemma 3 4B
- Text analysis
- Structured JSON schema
- Image analysis
- Output validation
- Model metrics

### Phase 9 — Embeddings and semantic search

- Integrate Nomic Embed Text v1.5
- Generate vectors from semantic summaries
- Add sqlite-vec
- Implement hybrid FTS5/vector retrieval
- Add result ranking

### Phase 10 — Hardening

- Permission diagnostics
- Export/delete controls
- Crash recovery
- Performance benchmarks
- Memory limits
- Model fallback behavior
- Windows Defender testing
- Documentation

## 15. Three-day hackathon cut

### Day 1

- Tauri shell
- Rust SQLite layer
- Foreground application tracking
- Window title tracking
- Raw event timeline
- Basic mouse and keyboard capture

### Day 2

- UI Automation metadata
- Click/selection event enrichment
- Screenshot capture
- In-memory asset pipeline
- Processing Queue
- Event inspector

### Day 3

- Gemma 3 4B text/image analysis
- Nomic embeddings
- sqlite-vec search
- Permission/status page
- Export/delete controls
- Demo polish and benchmarks

Do not attempt continuous video, automatic updates, or broad application-specific integrations during the hackathon.

## 16. Acceptance tests

### Capture

- Switching applications creates events.
- Window title changes are recorded.
- Left/right/double-clicks are differentiated.
- Text input is grouped into editing batches.
- Drag start/end is captured.
- Raw events continue saving while Gemma is busy.

### Visual evidence

- Meaningful clicks can trigger a screenshot.
- Screenshot is available in memory for analysis.
- Screenshot is deleted after processing.
- Capture failure does not lose the raw event.

### AI

- Gemma produces schema-valid output.
- Failed outputs are retried.
- A model crash does not crash capture.
- Semantic events reference their source raw events.

### Search

- Keyword search finds exact terms.
- Vector search finds semantically related events.
- Hybrid search returns both semantic and raw evidence.

### Privacy

- User can disable keyboard capture.
- User can exclude applications.
- User can select watched folders.
- User can delete all events.
- User can export all local data.
- The UI visibly shows which capture capabilities are active.

## 17. Exact machine validation checklist

Before committing to Gemma 3 4B on the old PC, record:

- Exact CPU model
- CPU core/thread count
- Exact GTX 1650 variant
- Dedicated VRAM amount
- NVIDIA driver version
- Windows version/build
- Total RAM
- RAM available at idle
- SSD or HDD
- Free disk space

Run these benchmarks:

1. Gemma text-only inference.
2. Gemma one-image inference.
3. Nomic embedding latency.
4. sqlite-vec insertion/search latency.
5. Capture while Gemma is processing.
6. Memory usage after 1000 raw events.
7. Queue recovery after forced termination.

## 18. Final architecture decision

Proceed Windows-first.

Use:

```text
Rust + Tauri
SQLite + FTS5 + sqlite-vec
Gemma 3 4B IT, quantized
Nomic Embed Text v1.5
Windows UI Automation
Windows input hooks
Windows Graphics Capture
Processing Queue
Transient visual assets
```

The product should be able to demonstrate:

1. The user works across applications.
2. Chronicle records detailed raw activity locally.
3. Chronicle captures visual context for meaningful events.
4. Gemma converts raw evidence into semantic events.
5. Nomic and sqlite-vec make those events searchable.
6. The original raw event remains available as evidence.

The non-negotiable invariant is:

> Capture and persistence must remain fast and reliable even when local AI inference is slow or unavailable.

