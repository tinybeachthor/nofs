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

function App() {
  const [listing, setListing] = useState<Listing | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function loadDir(path: string | null) {
    try {
      const result = await invoke<Listing>("list_dir", { path });
      setListing(result);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
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
                .filter((_, i, arr) => i < arr.length) // keep all including empty root
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

      {listing && (
        <div className="fb-grid">
          {listing.entries.map((e) => (
            <div
              key={e.path}
              className={`fb-tile ${e.is_dir ? "fb-tile-dir" : "fb-tile-file"}`}
              onClick={() => e.is_dir && loadDir(e.path)}
            >
              <div className="fb-tile-icon">
                {e.is_dir ? <FolderIcon /> : <FileIcon />}
              </div>
              <span className="fb-tile-name">{e.name}</span>
            </div>
          ))}
        </div>
      )}
    </main>
  );
}

export default App;
