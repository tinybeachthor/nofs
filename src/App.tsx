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
        <ul className="fb-list">
          {listing.entries.map((e) => (
            <li
              key={e.path}
              className={`fb-row ${e.is_dir ? "fb-row-dir" : "fb-row-file"}`}
              onClick={() => e.is_dir && loadDir(e.path)}
            >
              <span className="fb-glyph">{e.is_dir ? "▸" : "·"}</span>
              <span className="fb-name">{e.name}</span>
            </li>
          ))}
        </ul>
      )}
    </main>
  );
}

export default App;
