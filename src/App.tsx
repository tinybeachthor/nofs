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
    <svg width="80" height="64" viewBox="0 0 80 64" fill="none" xmlns="http://www.w3.org/2000/svg">
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
      <path d="M4 18 Q4 14 8 14 L28 14 Q32 14 34 18 L36 22 L4 22 Z" fill="url(#tabGrad)" />
      {/* Folder back */}
      <rect x="4" y="20" width="72" height="40" rx="5" fill="url(#folderBack)" />
      {/* Folder front face */}
      <rect x="4" y="26" width="72" height="34" rx="5" fill="url(#folderFront)" />
      {/* Sheen highlight */}
      <rect x="4" y="26" width="72" height="18" rx="5" fill="url(#folderSheen)" />
      {/* Bottom edge shadow */}
      <rect x="4" y="54" width="72" height="6" rx="3" fill="#1a3870" opacity="0.5" />
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
        <button
          className="fb-up"
          onClick={() => listing?.parent && loadDir(listing.parent)}
          disabled={!listing?.parent}
        >
          ↑ Up
        </button>
        <span className="fb-path">{listing?.path ?? ""}</span>
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
