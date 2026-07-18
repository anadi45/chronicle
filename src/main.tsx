import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import "./styles.css";

type RawEvent = { id: string; timestamp_ns: number; event_type: string; source: string; app_name?: string; window_title?: string; text?: string; privacy_class: string; confidence: number; created_at: string };
const fallback: RawEvent[] = [{ id: "shell", timestamp_ns: Date.now() * 1_000_000, event_type: "system_ready", source: "chronicle", app_name: "Chronicle", window_title: "Desktop shell initialized", privacy_class: "safe", confidence: 1, created_at: new Date().toISOString() }];

async function loadEvents(query?: string): Promise<RawEvent[]> {
  try { return await invoke<RawEvent[]>("list_events", { limit: 100, query: query || null }); }
  catch { return fallback.filter((event) => !query || JSON.stringify(event).toLowerCase().includes(query.toLowerCase())); }
}

function App() {
  const [events, setEvents] = useState<RawEvent[]>([]);
  const [query, setQuery] = useState("");
  const [section, setSection] = useState<"timeline" | "search" | "settings">("timeline");
  const [captureEnabled, setCaptureEnabled] = useState(false);
  useEffect(() => { loadEvents().then(setEvents); }, []);
  useEffect(() => { if (section === "search") loadEvents(query).then(setEvents); }, [query, section]);
  const refresh = () => loadEvents(section === "search" ? query : undefined).then(setEvents);
  return <main className="app-shell"><aside className="sidebar"><div className="brand"><span className="brand-mark">C</span><span>Chronicle</span></div><nav aria-label="Primary navigation"><button className={`nav-item ${section === "timeline" ? "active" : ""}`} onClick={() => setSection("timeline")}>Timeline</button><button className={`nav-item ${section === "search" ? "active" : ""}`} onClick={() => setSection("search")}>Search</button><button className={`nav-item ${section === "settings" ? "active" : ""}`} onClick={() => setSection("settings")}>Settings</button></nav><div className="capture-card"><div className={`status-dot ${captureEnabled ? "on" : ""}`} /><div><strong>{captureEnabled ? "Capture enabled" : "Capture is off"}</strong><span>Raw events are stored locally.</span></div></div></aside><section className="content"><header className="topbar"><div><p className="eyebrow">LOCAL MEMORY ENGINE</p><h1>{section[0].toUpperCase() + section.slice(1)}</h1></div><button className="quiet-button" onClick={refresh}>Refresh</button></header>{section === "settings" ? <Settings captureEnabled={captureEnabled} setCaptureEnabled={setCaptureEnabled} /> : <><div className="hero"><div><p className="eyebrow">TODAY</p><h2>Your computer, remembered.</h2><p className="muted">Chronicle persists raw activity first. Semantic understanding can follow later.</p></div><div className="metric"><span>Events captured</span><strong>{events.length}</strong></div></div>{section === "search" && <input className="search-input" placeholder="Search raw activity…" value={query} onChange={(event) => setQuery(event.target.value)} />}<section className="timeline"><div className="section-heading"><h3>{section === "search" ? "Search results" : "Recent activity"}</h3><span className="muted">Raw evidence</span></div>{events.length ? events.map((event) => <EventRow key={event.id} event={event} />) : <p className="muted empty">No events found.</p>}</section></>}</section></main>;
}

function EventRow({ event }: { event: RawEvent }) { return <article className="event-row"><time>{new Date(event.timestamp_ns / 1_000_000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time><div className="event-icon">•</div><div className="event-body"><div className="event-top"><strong>{event.app_name || "Unknown application"}</strong><span className="event-status">{event.privacy_class}</span></div><p>{event.window_title || event.text || "Activity recorded"}</p><span className="event-type">{event.event_type} · {event.source}</span></div></article>; }
function Settings({ captureEnabled, setCaptureEnabled }: { captureEnabled: boolean; setCaptureEnabled: (value: boolean) => void }) { return <section className="settings-panel"><h2>Permissions and privacy</h2><p className="muted">Capture capabilities are opt-in. AI processing is independent from raw event persistence.</p><label className="setting-row"><span><strong>Foreground application tracking</strong><small>Records active application and window titles.</small></span><input type="checkbox" checked={captureEnabled} onChange={(event) => setCaptureEnabled(event.target.checked)} /></label><label className="setting-row"><span><strong>Keyboard capture</strong><small>Disabled by default; protected fields are always excluded.</small></span><input type="checkbox" disabled /></label><label className="setting-row"><span><strong>Screen capture</strong><small>Transient screenshots only after meaningful events.</small></span><input type="checkbox" disabled /></label><p className="muted note">Windows providers will be enabled in the next integration pass.</p></section>; }

createRoot(document.getElementById("root")!).render(<StrictMode><App /></StrictMode>);
