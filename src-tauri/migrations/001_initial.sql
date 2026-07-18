CREATE TABLE IF NOT EXISTS raw_events (
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

CREATE VIRTUAL TABLE IF NOT EXISTS raw_events_fts USING fts5(
    app_name, window_title, element_name, element_value, selected_text, text, file_path,
    content='raw_events', content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS raw_events_ai AFTER INSERT ON raw_events BEGIN
    INSERT INTO raw_events_fts(rowid, app_name, window_title, element_name, element_value, selected_text, text, file_path)
    VALUES (new.rowid, new.app_name, new.window_title, new.element_name, new.element_value, new.selected_text, new.text, new.file_path);
END;

CREATE TRIGGER IF NOT EXISTS raw_events_ad AFTER DELETE ON raw_events BEGIN
    INSERT INTO raw_events_fts(raw_events_fts, rowid, app_name, window_title, element_name, element_value, selected_text, text, file_path)
    VALUES ('delete', old.rowid, old.app_name, old.window_title, old.element_name, old.element_value, old.selected_text, old.text, old.file_path);
END;

CREATE TABLE IF NOT EXISTS semantic_events (
    id TEXT PRIMARY KEY,
    raw_event_id TEXT NOT NULL REFERENCES raw_events(id),
    category TEXT NOT NULL,
    summary TEXT NOT NULL,
    entities_json TEXT NOT NULL,
    relationships_json TEXT NOT NULL,
    confidence REAL NOT NULL,
    model_name TEXT NOT NULL,
    model_version TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS processing_queue (
    id TEXT PRIMARY KEY,
    raw_event_id TEXT NOT NULL REFERENCES raw_events(id),
    task_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    priority INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    model_name TEXT,
    model_version TEXT,
    error TEXT,
    created_at TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT
);

CREATE TABLE IF NOT EXISTS app_settings (
    key TEXT PRIMARY KEY,
    value_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS processing_queue_pending_idx ON processing_queue(status, priority DESC, created_at ASC);
