import { useEffect, useMemo, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import {
  Archive, ArrowUpRight, Check, ChevronDown, ChevronRight,
  HardDrive, LayoutGrid, List, Search, ShieldCheck,
} from "lucide-react";

type Decision = "Keep" | "Review" | "Remove";
type Zone = "build" | "cache" | "git" | "personal" | "installer";
type FixtureItem = { id: string; title: string; folder: string; size: number; zone: Zone; note: string; decision: Decision; x: number; y: number; w: number; h: number };

const initialItems: FixtureItem[] = [
  { id: "build-atlas", title: "Build outputs", folder: "Atlas", size: 14.2, zone: "build", note: "Generated output in a recognized project. Source and local configuration are excluded.", decision: "Review", x: 1, y: 1, w: 30, h: 37 },
  { id: "deps", title: "Dependencies", folder: "Atlas", size: 10.6, zone: "personal", note: "Project dependencies. Kept in this example because rebuilding takes time.", decision: "Keep", x: 1, y: 39, w: 30, h: 28 },
  { id: "dataset", title: "Local dataset", folder: "Research", size: 6.7, zone: "personal", note: "Personal research material. No cleanup action is available.", decision: "Keep", x: 32, y: 44, w: 19, h: 23 },
  { id: "git", title: "Git history", folder: "Research", size: 6.8, zone: "git", note: "Repository state has not been investigated. Local work may be present.", decision: "Review", x: 32, y: 1, w: 19, h: 42 },
  { id: "packages", title: "Package downloads", folder: "Caches", size: 18.6, zone: "cache", note: "Downloaded packages. Availability and ownership need to be checked.", decision: "Review", x: 52, y: 1, w: 29, h: 43 },
  { id: "appcache", title: "App caches", folder: "Caches", size: 10, zone: "cache", note: "Cache contents are mixed in this example. The folder name alone proves nothing.", decision: "Keep", x: 82, y: 1, w: 17, h: 43 },
  { id: "installer", title: "Installers", folder: "Downloads", size: 4.1, zone: "installer", note: "Installer-like files need a person to check ownership and current use.", decision: "Review", x: 52, y: 45, w: 15, h: 17 },
  { id: "archive", title: "Archives", folder: "Downloads", size: 9.3, zone: "installer", note: "Archive contents are unknown. Nothing is selected for removal.", decision: "Keep", x: 52, y: 63, w: 19, h: 23 },
  { id: "tools", title: "Build outputs", folder: "Tools", size: 8.6, zone: "build", note: "Generated output candidate. Evidence is illustrative only.", decision: "Review", x: 33, y: 68, w: 18, h: 18 },
  { id: "originals", title: "Originals", folder: "Pictures", size: 7.4, zone: "personal", note: "Personal photos. Kept and outside the example plan.", decision: "Keep", x: 72, y: 64, w: 27, h: 22 },
];

const fmt = (n: number) => `${Number.isInteger(n) ? n.toFixed(0) : n.toFixed(1)} GiB`;
const fixtureTotal = initialItems.reduce((sum, item) => sum + item.size, 0);

export default function App() {
  const [bridgeStatus, setBridgeStatus] = useState("Browser fixture preview · native bridge unavailable");
  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    invoke<[string, boolean][]>("foundation_status")
      .then((reports) => {
        if (active) setBridgeStatus(`Native core connected · ${reports.filter(([, available]) => available).length}/${reports.length} production adapters available`);
      })
      .catch(() => { if (active) setBridgeStatus("Native capability query failed · cleanup remains unavailable"); });
    return () => { active = false; };
  }, []);
  const [folderCheckStatus, setFolderCheckStatus] = useState("");
  const [checkingFolder, setCheckingFolder] = useState(false);
  const checkFolderAccess = async () => {
    setCheckingFolder(true);
    try {
      setFolderCheckStatus(await invoke<string>("check_selected_folder_access"));
    } catch (error) {
      setFolderCheckStatus(typeof error === "string" ? error : "Folder check failed; access remains unavailable.");
    } finally {
      setCheckingFolder(false);
    }
  };
  const previewNativeReview = async () => {
    setCheckingFolder(true);
    try {
      setFolderCheckStatus(await invoke<string>("preview_native_review"));
    } catch (error) {
      setFolderCheckStatus(typeof error === "string" ? error : "Review preview could not complete.");
    } finally {
      setCheckingFolder(false);
    }
  };
  const [items, setItems] = useState(initialItems);
  const [selectedId, setSelectedId] = useState("build-atlas");
  const [view, setView] = useState<"map" | "list">("map");
  const [page, setPage] = useState<"space" | "plan">("space");
  const [query, setQuery] = useState("");
  const selected = items.find((item) => item.id === selectedId) ?? items[0];
  const included = items.filter((item) => item.decision === "Remove");
  const selectedBytes = included.reduce((total, item) => total + item.size, 0);
  const visibleItems = useMemo(() => items.filter((item) => `${item.title} ${item.folder}`.toLowerCase().includes(query.toLowerCase())), [items, query]);
  const setDecision = (id: string, decision: Decision) => setItems((current) => current.map((item) => item.id === id ? { ...item, decision } : item));

  return (
    <div className="app-shell">
      <header className="topbar">
        <div className="brand-mark"><HardDrive size={17} strokeWidth={1.8} /></div>
        <span className="brand-name">Disk Steward</span>
        <span className="top-divider" />
        <span className="eyebrow">Storage review</span>
        <div className="top-spacer" />
        <span className="demo-chip"><span /> Example data</span>
      </header>

      <aside className="sidebar">
        <div className="nav-label">WORKSPACE</div>
        <button className={`nav-item ${page === "space" ? "active" : ""}`} onClick={() => setPage("space")}><LayoutGrid size={16} /> Space map</button>
        <button className={`nav-item ${page === "plan" ? "active" : ""}`} onClick={() => setPage("plan")}><Archive size={16} /> Cleanup plan{included.length > 0 && <span className="nav-count">{included.length}</span>}</button>
        <div className="sidebar-rule" />
        <div className="sidebar-stat"><span className="stat-label">Selected folders</span><strong>{fmt(fixtureTotal)}</strong><span>10 example items</span></div>
        <div className="coverage"><span className="coverage-dot" /><span>Example coverage<br /><b>Complete fixture</b></span></div>
        <div className="sidebar-foot"><ShieldCheck size={14} /> Decisions stay in this window</div>
      </aside>

      <main className="main-content">
        <div className="main-head">
          <div>
            <div className="crumb">EXAMPLE VOLUME <ChevronRight size={12} /> {page === "space" ? "OVERVIEW" : "PLAN"}</div>
            <h1>{page === "space" ? "See what’s taking space." : "Review your plan."}</h1>
            <p className="subtitle">{page === "space" ? "Explore the example folders. Inspect the notes. Decide what stays." : "Change example decisions and see how the plan responds."}</p>
            <p className="subtitle" role="status">{bridgeStatus}</p>
            <button className="text-button" disabled={!isTauri() || checkingFolder} onClick={checkFolderAccess}>
              {checkingFolder ? "Native check in progress…" : "Check folder access"}
            </button>
            <button className="text-button" disabled={!isTauri() || checkingFolder} onClick={previewNativeReview}>Preview native review</button>
            {folderCheckStatus && <p className="subtitle" role="status">{folderCheckStatus}</p>}
          </div>
          <button className="quiet-button" onClick={() => setPage(page === "space" ? "plan" : "space")}>{page === "space" ? "Review your plan" : "Back to space map"}<ArrowUpRight size={15} /></button>
        </div>

        {page === "space" ? <>
          <section className="workspace-panel">
            <div className="map-column">
              <div className="section-heading"><div><h2>Selected folders</h2><span>{fmt(fixtureTotal)} in this illustrative example</span></div>
                <div className="view-toggle"><button className={view === "map" ? "chosen" : ""} onClick={() => setView("map")} aria-label="Map view"><LayoutGrid size={15} /></button><button className={view === "list" ? "chosen" : ""} onClick={() => setView("list")} aria-label="List view"><List size={16} /></button></div>
              </div>
              {view === "list" && <label className="search-box"><Search size={14} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Find an example item" /></label>}
              {view === "map" ? <div className="treemap" aria-label="Example storage map">
                {items.map((item) => <button key={item.id} className={`tile zone-${item.zone} ${selectedId === item.id ? "selected" : ""} decision-${item.decision.toLowerCase()}`} style={{ left: `${item.x}%`, top: `${item.y}%`, width: `${item.w}%`, height: `${item.h}%` }} onClick={() => setSelectedId(item.id)}>
                  <span className="tile-folder">{item.folder}</span><strong>{item.title}</strong><span className="tile-size">{fmt(item.size)}</span><span className="tile-status">{item.decision === "Remove" ? "Selected" : item.decision === "Keep" ? "Kept" : "Needs review"}</span>
                </button>)}
                <span className="map-label label-projects">Atlas · 24.8 GiB</span><span className="map-label label-caches">Caches · 28.6 GiB</span><span className="map-label label-downloads">Downloads · 13.4 GiB</span><span className="map-label label-pictures">Pictures · 7.4 GiB</span>
              </div> : <div className="item-list">{visibleItems.map((item) => <button key={item.id} className={`list-row ${selectedId === item.id ? "selected" : ""}`} style={{ "--share": `${item.size / fixtureTotal * 100}%` } as React.CSSProperties} onClick={() => setSelectedId(item.id)}><span className={`list-swatch zone-${item.zone}`} /><span className="row-title"><b>{item.title}</b><small>{item.folder}</small></span><span className="row-size">{fmt(item.size)}</span><span className={`decision-pill ${item.decision.toLowerCase()}`}>{item.decision}</span><ChevronRight size={15} /></button>)}</div>}
              <div className="legend"><span><i className="swatch build" />Build output</span><span><i className="swatch cache" />Caches</span><span><i className="swatch git" />Git</span><span><i className="swatch personal" />Personal</span><span><i className="swatch installer" />Downloads</span></div>
              <div className="map-caption">Illustrative layout · area is not proportional <span>·</span> Color = category <span>·</span> Outline = selected item</div>
            </div>
            <aside className="inspector">
              <div className="inspector-label">SELECTED ITEM</div>
              <div className="inspector-context">{selected.folder}</div>
              <h2>{selected.title}</h2>
              <div className="inspector-size">{fmt(selected.size)}</div>
              <span className={`decision-pill ${selected.decision.toLowerCase()}`}>{selected.decision === "Remove" ? "Selected for review" : selected.decision === "Keep" ? "Kept" : "Needs review"}</span>
              <p className="evidence-note">{selected.note}</p>
              <div className="evidence-list"><div><Check size={13} /> Example evidence attached</div><div><Check size={13} /> No live folders scanned</div><div><Check size={13} /> Nothing has been changed</div></div>
              <label className="decision-label" htmlFor="decision">Your decision</label>
              <div className="select-wrap"><select id="decision" value={selected.decision} onChange={(event) => setDecision(selected.id, event.target.value as Decision)}><option>Keep</option><option>Review</option><option>Remove</option></select><ChevronDown size={15} /></div>
              <div className="decision-help">{selected.decision === "Remove" ? "Included in the example plan below." : selected.decision === "Keep" ? "This item stays out of the example plan." : "It needs more evidence before an action."}</div>
            </aside>
          </section>
          <section className="plan-strip">
            <div className="plan-mark"><Archive size={17} /></div>
            <div className="plan-summary"><strong>{fmt(selectedBytes)} selected <span>·</span> {included.length} {included.length === 1 ? "item" : "items"}</strong><span>Example only. Staging keeps data recoverable; it does not free space.</span></div>
            <button className="primary-button" onClick={() => setPage("plan")}>Review example plan <ArrowUpRight size={15} /></button>
          </section>
          <div className="footnote">Synthetic example · no scanning or cleanup · no AI requests</div>
        </> : <>
          <section className="plan-page-card">
            <div className="plan-card-head"><div><div className="inspector-label">EXAMPLE PLAN · REVISION 1</div><h2>Items you’ve selected</h2><p>These choices are temporary and reset when the app reloads.</p></div><div className="plan-total">{fmt(selectedBytes)}<small>selected size</small></div></div>
            {included.length ? included.map((item) => <div className="plan-row" key={item.id}><span className={`list-swatch zone-${item.zone}`} /><div className="row-title"><b>{item.title}</b><small>{item.folder} · example evidence only</small></div><span className="row-size">{fmt(item.size)}</span><button className="text-button" onClick={() => setDecision(item.id, "Review")}>Remove from plan</button></div>) : <div className="empty-plan"><Archive size={22} /><b>No items selected</b><span>Choose an item and set its decision to “Remove” to add it here.</span></div>}
            <div className="plan-callout"><ShieldCheck size={17} /><div><b>No action can run in this prototype.</b><span>This review is made from fixed example data. The optional folder check is read-only. Scanning, approval, and cleanup are not implemented.</span></div></div>
            <div className="plan-actions"><button className="quiet-button" onClick={() => setPage("space")}>Back to example</button><button className="primary-button disabled" disabled>Approve and stage <ArrowUpRight size={15} /></button></div>
          </section>
          <div className="footnote">Synthetic example · optional read-only access check · approval and execution are not implemented</div>
        </>}
      </main>
    </div>
  );
}
