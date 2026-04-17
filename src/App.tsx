import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type DirEntry = {
  name: string;
  path: string;
  is_dir: boolean;
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

function PreviewPanel({ state, onClose }: { state: PreviewState; onClose: () => void }) {
  return (
    <aside className="fb-preview">
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

  async function loadDir(path: string | null) {
    try {
      const result = await invoke<Listing>("list_dir", { path });
      setListing(result);
      setError(null);
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

  return (
    <main className="fb">
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
                      <span className="fb-crumb fb-crumb-current">{crumb.label}</span>
                    ) : (
                      <button className="fb-crumb" onClick={() => loadDir(crumb.path)}>
                        {crumb.label}
                      </button>
                    )}
                  </span>
                ))
            : null}
        </nav>
      </header>

      {error && <div className="fb-error">{error}</div>}

      <div className="fb-content">
        {listing && (
          <div className="fb-grid">
            {listing.entries.map((e) => (
              <div
                key={e.path}
                className={`fb-tile ${e.is_dir ? "fb-tile-dir" : "fb-tile-file"}${preview?.entry.path === e.path ? " fb-tile-selected" : ""}`}
                onClick={() => {
                  if (e.is_dir) { closePreview(); loadDir(e.path); }
                  else { openPreview(e); }
                }}
              >
                <div className="fb-tile-icon">
                  {e.is_dir ? <FolderIcon /> : <FileIcon />}
                </div>
                <span className="fb-tile-name">{e.name}</span>
              </div>
            ))}
          </div>
        )}
        {preview && (
          <PreviewPanel state={preview} onClose={closePreview} />
        )}
      </div>
    </main>
  );
}

export default App;
