import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";

type Activity = {
  time: string;
  app: string;
  title: string;
  event: string;
  status: string;
};

const activities: Activity[] = [
  { time: "Now", app: "Chronicle", title: "Desktop shell initialized", event: "app_activated", status: "Raw evidence" },
  { time: "—", app: "Waiting for capture", title: "Capture providers will appear here", event: "system_ready", status: "Ready" },
];

function App() {
  return (
    <main className="app-shell">
      <aside className="sidebar">
        <div className="brand"><span className="brand-mark">C</span><span>Chronicle</span></div>
        <nav aria-label="Primary navigation">
          <a className="nav-item active" href="#timeline">Timeline</a>
          <a className="nav-item" href="#search">Search</a>
          <a className="nav-item" href="#settings">Settings</a>
        </nav>
        <div className="capture-card">
          <div className="status-dot" />
          <div><strong>Capture is ready</strong><span>Providers connect in Phase 2</span></div>
        </div>
      </aside>
      <section className="content">
        <header className="topbar"><div><p className="eyebrow">LOCAL MEMORY ENGINE</p><h1>Timeline</h1></div><button className="quiet-button">Capture settings</button></header>
        <div className="hero"><div><p className="eyebrow">TODAY</p><h2>Your computer, remembered.</h2><p className="muted">Chronicle persists raw activity first. Semantic understanding can follow later.</p></div><div className="metric"><span>Events captured</span><strong>0</strong></div></div>
        <section className="timeline" id="timeline"><div className="section-heading"><h3>Recent activity</h3><span className="muted">Raw evidence</span></div>{activities.map((activity) => <article className="event-row" key={activity.event}><time>{activity.time}</time><div className="event-icon">•</div><div className="event-body"><div className="event-top"><strong>{activity.app}</strong><span className="event-status">{activity.status}</span></div><p>{activity.title}</p><span className="event-type">{activity.event}</span></div></article>)}</section>
      </section>
    </main>
  );
}

createRoot(document.getElementById("root")!).render(<StrictMode><App /></StrictMode>);

