import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import "./App.css";

type DirEntry = {
  name: string;
  path: string;
  is_dir: boolean;
  managed: boolean;
};

type Listing = {
  path: string;
  parent: string | null;
  entries: DirEntry[];
};

type FilePreview =
  | { kind: "Text"; content: string; truncated: boolean }
  | { kind: "Binary" }
  | { kind: "Image"; url: string }
  | { kind: "Pdf"; url: string };

const MIME_BY_EXT: Record<string, string> = {
  png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif",
  svg: "image/svg+xml", webp: "image/webp", bmp: "image/bmp", ico: "image/x-icon",
  avif: "image/avif", tiff: "image/tiff", tif: "image/tiff", pdf: "application/pdf",
};

function mediaMime(name: string): string | null {
  const dot = name.lastIndexOf(".");
  if (dot === -1) return null;
  return MIME_BY_EXT[name.slice(dot + 1).toLowerCase()] ?? null;
}

async function streamFileToUrl(path: string, mime: string): Promise<string> {
  const bytes = await invoke<ArrayBuffer>("stream_file", { path });
  const blob = new Blob([bytes], { type: mime });
  return URL.createObjectURL(blob);
}

type PreviewState = {
  entry: DirEntry;
  preview: FilePreview | null;
  error: string | null;
};

function HomeIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 14 14" fill="none" xmlns="http://www.w3.org/2000/svg" style={{ display: "block" }}>
      <path d="M1 6.5L7 1.5L13 6.5" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M2.5 5.5V12H5.5V9H8.5V12H11.5V5.5" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

function FolderIcon() {
  return (
    <svg width="80" height="80" viewBox="0 0 80 80" fill="none" xmlns="http://www.w3.org/2000/svg">
      <defs>
        <linearGradient id="folderBack" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#4a6fa5" />
          <stop offset="100%" stopColor="#2a4a80" />
        </linearGradient>
        <linearGradient id="folderFront" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#6b9bde" />
          <stop offset="40%" stopColor="#4f7fc8" />
          <stop offset="100%" stopColor="#2d5aa0" />
        </linearGradient>
        <linearGradient id="folderSheen" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="white" stopOpacity="0.18" />
          <stop offset="100%" stopColor="white" stopOpacity="0" />
        </linearGradient>
        <linearGradient id="tabGrad" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#5a82b8" />
          <stop offset="100%" stopColor="#4a6fa5" />
        </linearGradient>
      </defs>
      {/* Tab */}
      <path d="M4 20 Q4 14 8 14 L30 14 Q34 14 36 20 L38 26 L4 26 Z" fill="url(#tabGrad)" />
      {/* Folder back */}
      <rect x="4" y="24" width="72" height="50" rx="5" fill="url(#folderBack)" />
      {/* Folder front face */}
      <rect x="4" y="30" width="72" height="44" rx="5" fill="url(#folderFront)" />
      {/* Sheen highlight */}
      <rect x="4" y="30" width="72" height="22" rx="5" fill="url(#folderSheen)" />
      {/* Bottom edge shadow */}
      <rect x="4" y="68" width="72" height="6" rx="3" fill="#1a3870" opacity="0.5" />
    </svg>
  );
}

function FileIcon() {
  return (
    <svg width="64" height="80" viewBox="0 0 64 80" fill="none" xmlns="http://www.w3.org/2000/svg">
      <defs>
        <linearGradient id="fileBg" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#e8edf5" />
          <stop offset="100%" stopColor="#c8d0e0" />
        </linearGradient>
      </defs>
      <path d="M8 4 L44 4 L56 16 L56 76 Q56 78 54 78 L10 78 Q8 78 8 76 Z" fill="url(#fileBg)" />
      <path d="M44 4 L44 16 L56 16 Z" fill="#a0aabb" />
      <rect x="16" y="28" width="28" height="3" rx="1.5" fill="#8892a4" opacity="0.7" />
      <rect x="16" y="36" width="22" height="3" rx="1.5" fill="#8892a4" opacity="0.7" />
      <rect x="16" y="44" width="26" height="3" rx="1.5" fill="#8892a4" opacity="0.7" />
    </svg>
  );
}

function ImageIcon() {
  return (
    <svg width="64" height="80" viewBox="0 0 64 80" fill="none" xmlns="http://www.w3.org/2000/svg">
      <defs>
        <linearGradient id="imgBg" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#d4eaff" />
          <stop offset="100%" stopColor="#aacfee" />
        </linearGradient>
      </defs>
      <path d="M8 4 L44 4 L56 16 L56 76 Q56 78 54 78 L10 78 Q8 78 8 76 Z" fill="url(#imgBg)" />
      <path d="M44 4 L44 16 L56 16 Z" fill="#7aaece" />
      {/* sky */}
      <rect x="14" y="30" width="36" height="30" rx="3" fill="#c0dff5" />
      {/* sun */}
      <circle cx="26" cy="38" r="5" fill="#f5c842" />
      {/* mountains */}
      <path d="M14 60 L28 42 L38 54 L44 48 L50 60 Z" fill="#5a9fd4" />
    </svg>
  );
}

function PdfIcon() {
  return (
    <svg width="64" height="80" viewBox="0 0 64 80" fill="none" xmlns="http://www.w3.org/2000/svg">
      <defs>
        <linearGradient id="pdfBg" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#fde0de" />
          <stop offset="100%" stopColor="#f4b8b4" />
        </linearGradient>
      </defs>
      <path d="M8 4 L44 4 L56 16 L56 76 Q56 78 54 78 L10 78 Q8 78 8 76 Z" fill="url(#pdfBg)" />
      <path d="M44 4 L44 16 L56 16 Z" fill="#d47a76" />
      <text x="32" y="58" textAnchor="middle" fontFamily="sans-serif" fontWeight="700" fontSize="16" fill="#c0392b" letterSpacing="0.5">PDF</text>
      <rect x="14" y="34" width="36" height="2.5" rx="1.25" fill="#d47a76" opacity="0.6" />
      <rect x="14" y="40" width="28" height="2.5" rx="1.25" fill="#d47a76" opacity="0.6" />
    </svg>
  );
}

function PreviewPanel({ state, onClose, width }: { state: PreviewState; onClose: () => void; width: number }) {
  return (
    <aside className="fb-preview" style={{ width }}>
      <header className="fb-preview-header">
        <span className="fb-preview-title">{state.entry.name}</span>
        <button className="fb-preview-close" onClick={onClose} aria-label="Close preview">✕</button>
      </header>
      <div className="fb-preview-body">
        {state.error ? (
          <p className="fb-preview-error">{state.error}</p>
        ) : state.preview === null ? (
          <p className="fb-preview-meta">Loading…</p>
        ) : state.preview.kind === "Image" ? (
          <img src={state.preview.url} alt={state.entry.name} className="fb-preview-image" />
        ) : state.preview.kind === "Pdf" ? (
          <iframe src={state.preview.url} className="fb-preview-pdf" title={state.entry.name} />
        ) : state.preview.kind === "Binary" ? (
          <p className="fb-preview-meta">Binary file — no preview available.</p>
        ) : (
          <>
            <pre className="fb-preview-text">{state.preview.content}</pre>
            {state.preview.truncated && (
              <p className="fb-preview-meta">Showing first 64 KB</p>
            )}
          </>
        )}
      </div>
    </aside>
  );
}

function App() {
  const [listing, setListing] = useState<Listing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<PreviewState | null>(null);
  const [previewWidth, setPreviewWidth] = useState(320);
  const [dirty, setDirty] = useState(false);
  const [dragging, setDragging] = useState(false);
  const resizeRef = useRef<{ startX: number; startWidth: number } | null>(null);
  const pathRef = useRef<string | null>(null);

  function onResizeStart(e: React.MouseEvent) {
    resizeRef.current = { startX: e.clientX, startWidth: previewWidth };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";

    function onMouseMove(ev: MouseEvent) {
      if (!resizeRef.current) return;
      const delta = resizeRef.current.startX - ev.clientX;
      setPreviewWidth(Math.max(200, Math.min(800, resizeRef.current.startWidth + delta)));
    }

    function onMouseUp() {
      resizeRef.current = null;
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
    }

    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);
  }

  async function loadDir(path: string | null) {
    try {
      const result = await invoke<Listing>("list_dir", { path });
      setListing(result);
      pathRef.current = result.path;
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }

  async function onPersist() {
    try {
      const d = await invoke<boolean>("persist");
      setDirty(d);
      await loadDir(pathRef.current);
    } catch (e) {
      setError(String(e));
    }
  }

  async function openPreview(entry: DirEntry) {
    if (preview?.preview && ("url" in preview.preview)) {
      URL.revokeObjectURL(preview.preview.url);
    }
    setPreview({ entry, preview: null, error: null });
    try {
      const mime = mediaMime(entry.name);
      if (mime && mime.startsWith("image/")) {
        const url = await streamFileToUrl(entry.path, mime);
        setPreview({ entry, preview: { kind: "Image", url }, error: null });
      } else if (mime === "application/pdf") {
        const url = await streamFileToUrl(entry.path, mime);
        setPreview({ entry, preview: { kind: "Pdf", url }, error: null });
      } else {
        const result = await invoke<FilePreview>("read_file", { path: entry.path });
        setPreview({ entry, preview: result, error: null });
      }
    } catch (e) {
      setPreview({ entry, preview: null, error: String(e) });
    }
  }

  function closePreview() {
    if (preview?.preview && ("url" in preview.preview)) {
      URL.revokeObjectURL(preview.preview.url);
    }
    setPreview(null);
  }

  useEffect(() => {
    loadDir(null);
  }, []);

  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent(async (event) => {
      const p = event.payload;
      if (p.type === "over" || p.type === "enter") {
        setDragging(true);
      } else if (p.type === "leave") {
        setDragging(false);
      } else if (p.type === "drop") {
        setDragging(false);
        try {
          const d = await invoke<boolean>("add_dropped_files", {
            destDir: pathRef.current ?? "/",
            paths: p.paths,
          });
          setDirty(d);
          await loadDir(pathRef.current);
        } catch (e) {
          setError(String(e));
        }
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  return (
    <main className={`fb${dragging ? " fb-dragging" : ""}`}>
      <header className="fb-topbar">
        <nav className="fb-breadcrumbs">
          {listing
            ? listing.path
                .split("/")
                .filter((_, i, arr) => i < arr.length)
                .reduce<{ label: string; path: string }[]>((acc, segment, i, arr) => {
                  if (i === 0 && segment === "") {
                    acc.push({ label: "/", path: "/" });
                  } else if (segment !== "") {
                    const path = arr.slice(0, i + 1).join("/") || "/";
                    acc.push({ label: segment, path });
                  }
                  return acc;
                }, [])
                .map((crumb, i, arr) => (
                  <span key={crumb.path} className="fb-crumb-item">
                    {i > 0 && <span className="fb-crumb-sep">/</span>}
                    {i === arr.length - 1 ? (
                      <span className="fb-crumb fb-crumb-current">{crumb.label === "/" ? <HomeIcon /> : crumb.label}</span>
                    ) : (
                      <button className="fb-crumb" onClick={() => loadDir(crumb.path)}>
                        {crumb.label === "/" ? <HomeIcon /> : crumb.label}
                      </button>
                    )}
                  </span>
                ))
            : null}
        </nav>
        {dirty && (
          <button className="fb-persist" onClick={onPersist}>Persist</button>
        )}
      </header>

      {error && <div className="fb-error">{error}</div>}

      <div className="fb-content">
        {listing && (
          <div className="fb-grid">
            {listing.entries.map((e) => (
              <div
                key={e.path}
                className={`fb-tile ${e.is_dir ? "fb-tile-dir" : "fb-tile-file"}${e.managed ? " fb-tile-managed" : ""}${preview?.entry.path === e.path ? " fb-tile-selected" : ""}`}
                onClick={() => {
                  if (e.is_dir) { closePreview(); loadDir(e.path); }
                  else { openPreview(e); }
                }}
              >
                <div className="fb-tile-icon">
                  {e.managed && <span className="fb-tile-badge" title="Stored in your overlay layers">●</span>}
                  {e.is_dir ? <FolderIcon /> : (() => {
                    const mime = mediaMime(e.name);
                    if (mime?.startsWith("image/")) return <ImageIcon />;
                    if (mime === "application/pdf") return <PdfIcon />;
                    return <FileIcon />;
                  })()}
                </div>
                <span className="fb-tile-name">{e.name}</span>
              </div>
            ))}
          </div>
        )}
        {preview && (
          <>
            <div className="fb-resize-handle" onMouseDown={onResizeStart} />
            <PreviewPanel state={preview} onClose={closePreview} width={previewWidth} />
          </>
        )}
      </div>
    </main>
  );
}

export default App;
