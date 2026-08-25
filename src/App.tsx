import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";

/** Mirrors the `LoadedImage` struct returned by the `load_image` Rust command. */
type LoadedImage = {
  path: string;
  fileName: string;
  width: number;
  height: number;
  byteLength: number;
  dataUrl: string;
};

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export default function App() {
  const [image, setImage] = useState<LoadedImage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [dropping, setDropping] = useState(false);

  const loadPath = useCallback(async (path: string) => {
    setLoading(true);
    setError(null);
    try {
      setImage(await invoke<LoadedImage>("load_image", { path }));
    } catch (err) {
      setImage(null);
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  const openDialog = useCallback(async () => {
    const selected = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "PNG image", extensions: ["png"] }],
    });
    if (typeof selected === "string") await loadPath(selected);
  }, [loadPath]);

  // Dropping a file anywhere on the window opens it, same as the Open button.
  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "over") {
        setDropping(true);
      } else if (event.payload.type === "drop") {
        setDropping(false);
        const [first] = event.payload.paths;
        if (first) void loadPath(first);
      } else {
        setDropping(false);
      }
    });
    return () => {
      void unlisten.then((off) => off());
    };
  }, [loadPath]);

  return (
    <div className={`app${dropping ? " app--dropping" : ""}`}>
      <header className="toolbar">
        <h1 className="toolbar__title">Image Editor</h1>
        <button className="button" onClick={openDialog} disabled={loading}>
          {loading ? "Opening…" : "Open PNG…"}
        </button>
      </header>

      <main className="stage">
        {error && (
          <div className="notice notice--error" role="alert">
            {error}
          </div>
        )}
        {!error && !image && (
          <div className="notice">
            <p className="notice__lead">No image open</p>
            <p>Click <strong>Open PNG…</strong> or drop a .png file onto this window.</p>
          </div>
        )}
        {image && (
          <img className="canvas" src={image.dataUrl} alt={image.fileName} />
        )}
      </main>

      <footer className="statusbar">
        {image ? (
          <>
            <span className="statusbar__name" title={image.path}>
              {image.fileName}
            </span>
            <span>
              {image.width} × {image.height}
            </span>
            <span>{formatBytes(image.byteLength)}</span>
          </>
        ) : (
          <span>Ready</span>
        )}
      </footer>
    </div>
  );
}
