import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open, save } from "@tauri-apps/plugin-dialog";

import LayerPanel from "./LayerPanel";
import type {
  BlendMode,
  BlendModeInfo,
  DocumentView,
  HistoryState,
  MoveDirection,
  Snapshot,
  Tool,
} from "./types";

const PNG_FILTER = [{ name: "PNG image", extensions: ["png"] }];
const PROJECT_FILTER = [{ name: "Image Editor Project", extensions: ["iep"] }];

/** `#rrggbb` to `[r, g, b]`, each `0..=255`. */
function hexToRgb(hex: string): [number, number, number] {
  const value = Number.parseInt(hex.slice(1), 16);
  return [(value >> 16) & 0xff, (value >> 8) & 0xff, value & 0xff];
}

/** A pointer event's position, in document pixel coordinates. */
function toDocPoint(
  event: React.PointerEvent<HTMLImageElement>,
  doc: DocumentView,
): [number, number] {
  const rect = event.currentTarget.getBoundingClientRect();
  return [
    ((event.clientX - rect.left) / rect.width) * doc.width,
    ((event.clientY - rect.top) / rect.height) * doc.height,
  ];
}

/** A doc-pixel rectangle as a percentage-based overlay style, positioned
 * relative to the canvas image it's drawn over. */
function overlayStyle(
  bounds: { x0: number; y0: number; x1: number; y1: number },
  doc: DocumentView,
): React.CSSProperties {
  return {
    left: `${(bounds.x0 / doc.width) * 100}%`,
    top: `${(bounds.y0 / doc.height) * 100}%`,
    width: `${((bounds.x1 - bounds.x0) / doc.width) * 100}%`,
    height: `${((bounds.y1 - bounds.y0) / doc.height) * 100}%`,
  };
}

/** The two arbitrary drag corners of an in-progress marquee, normalized into
 * a bounds rectangle and clamped to the canvas — purely for the live
 * preview outline; the backend does its own authoritative clamping once the
 * drag ends. */
function marqueeBounds(
  start: [number, number],
  current: [number, number],
  doc: DocumentView,
): { x0: number; y0: number; x1: number; y1: number } {
  const [sx, sy] = start;
  const [cx, cy] = current;
  return {
    x0: Math.max(0, Math.min(sx, cx)),
    y0: Math.max(0, Math.min(sy, cy)),
    x1: Math.min(doc.width, Math.max(sx, cx)),
    y1: Math.min(doc.height, Math.max(sy, cy)),
  };
}

export default function App() {
  const [document, setDocument] = useState<DocumentView | null>(null);
  // `null` until the first snapshot lands. The composite's actual bytes never
  // cross IPC: this only tells the `<img>` below which `composite://`
  // generation to fetch.
  const [generation, setGeneration] = useState<number | null>(null);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [blendModes, setBlendModes] = useState<BlendModeInfo[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [dropping, setDropping] = useState(false);
  const [canUndo, setCanUndo] = useState(false);
  const [canRedo, setCanRedo] = useState(false);

  const [showNewDialog, setShowNewDialog] = useState(false);
  const [newWidth, setNewWidth] = useState(800);
  const [newHeight, setNewHeight] = useState(600);

  const [tool, setTool] = useState<Tool>("brush");
  const [brushColor, setBrushColor] = useState("#ffffff");
  const [brushSize, setBrushSize] = useState(16);
  const [brushOpacity, setBrushOpacity] = useState(1);

  // A marquee drag's live start point (a ref, not state — read directly at
  // pointerup rather than through a closure that could be stale by then, the
  // same reasoning `lastPoint` below uses for brush strokes). `marqueePreview`
  // exists only to re-render the live outline as the drag moves; the actual
  // select_rectangle/select_ellipse call at drag-end recomputes its corners
  // from the ref and the pointerup event directly.
  const marqueeStart = useRef<[number, number] | null>(null);
  const [marqueePreview, setMarqueePreview] = useState<{
    start: [number, number];
    current: [number, number];
  } | null>(null);

  // Dragging the opacity slider fires many overlapping commands. Each one is
  // tagged, and only the newest response is allowed to land, so a slow render
  // can never overwrite a newer one.
  const requestId = useRef(0);

  // A stroke is a sequence of pointer-move events, not one command: each move
  // sends just the segment since the last point, so a call's own bounding box
  // (and the coverage work behind it) stays small regardless of how long the
  // drag has run. `lastPoint` is `null` between strokes.
  const lastPoint = useRef<[number, number] | null>(null);

  const runCommand = useCallback(
    async (command: string, args: Record<string, unknown> = {}, selectAfter?: "top") => {
      const ticket = ++requestId.current;
      setBusy(true);
      try {
        const snapshot = await invoke<Snapshot>(command, args);
        if (ticket !== requestId.current) return;

        setError(null);
        setDocument(snapshot.document);
        setGeneration(snapshot.generation);
        setCanUndo(snapshot.canUndo);
        setCanRedo(snapshot.canRedo);

        const { layers } = snapshot.document;
        setSelectedId((current) => {
          if (selectAfter === "top") return layers[layers.length - 1]?.id ?? null;
          // Keep the selection unless that layer is gone.
          if (current !== null && layers.some((layer) => layer.id === current)) return current;
          return layers[layers.length - 1]?.id ?? null;
        });
      } catch (err) {
        if (ticket !== requestId.current) return;
        setError(String(err));
      } finally {
        if (ticket === requestId.current) setBusy(false);
      }
    },
    [],
  );

  // Snapshots the document onto the undo stack, for a multi-step gesture
  // (a stroke, an opacity drag) to call once, at the start — the whole
  // gesture then undoes as one step rather than one step per command it
  // happens to have sent. Unlike runCommand, this doesn't touch document,
  // generation, or selection: only the undo/redo button states change.
  //
  // Returns the promise so a caller that needs the checkpoint to actually
  // land before its first edit — not just be issued first, which two
  // invoke() calls fired back to back in the same tick do not guarantee —
  // can await it. paint/erase strokes do; the opacity slider doesn't need
  // to, since its own onChange only fires later, on real pointer movement.
  const checkpoint = useCallback(() => {
    return invoke<HistoryState>("checkpoint")
      .then((history) => {
        setCanUndo(history.canUndo);
        setCanRedo(history.canRedo);
      })
      .catch(() => {
        // A failed checkpoint just costs this gesture its undo step; the
        // edit that follows still happens normally.
      });
  }, []);

  const undo = useCallback(() => void runCommand("undo"), [runCommand]);
  const redo = useCallback(() => void runCommand("redo"), [runCommand]);
  const deselect = useCallback(() => void runCommand("deselect"), [runCommand]);
  const selectAll = useCallback(() => void runCommand("select_all"), [runCommand]);
  const invertSelection = useCallback(
    () => void runCommand("invert_selection"),
    [runCommand],
  );
  const hasSelection = document?.selection != null;

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return;
      const key = event.key.toLowerCase();
      if (key === "z" && !event.shiftKey) {
        event.preventDefault();
        if (canUndo && !busy) undo();
      } else if ((key === "z" && event.shiftKey) || key === "y") {
        event.preventDefault();
        if (canRedo && !busy) redo();
      } else if (key === "d") {
        event.preventDefault();
        if (hasSelection && !busy) deselect();
      } else if (key === "a") {
        event.preventDefault();
        if (document !== null && !busy) selectAll();
      } else if (key === "i" && event.shiftKey) {
        event.preventDefault();
        if (hasSelection && !busy) invertSelection();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [
    canUndo,
    canRedo,
    busy,
    undo,
    redo,
    hasSelection,
    deselect,
    document,
    selectAll,
    invertSelection,
  ]);

  useEffect(() => {
    invoke<BlendModeInfo[]>("blend_modes").then(setBlendModes).catch(() => {
      // A failure here only costs the picker its labels; the canvas still works.
      setBlendModes([]);
    });
  }, []);

  const openDocument = useCallback(async () => {
    const selected = await open({ multiple: false, directory: false, filters: PNG_FILTER });
    if (typeof selected === "string") await runCommand("open_document", { path: selected }, "top");
  }, [runCommand]);

  const addLayer = useCallback(async () => {
    const selected = await open({ multiple: false, directory: false, filters: PNG_FILTER });
    if (typeof selected === "string") await runCommand("add_layer", { path: selected }, "top");
  }, [runCommand]);

  // Unlike runCommand, exporting reads the open document but never mutates
  // it — there is no new Snapshot to apply, only success or an error to show.
  const exportDocument = useCallback(async () => {
    const destination = await save({ filters: PNG_FILTER, defaultPath: "untitled.png" });
    if (typeof destination !== "string") return;
    setBusy(true);
    try {
      await invoke("export_png", { path: destination });
      setError(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, []);

  // Unlike Export PNG…, this writes the full editable layer stack (order,
  // visibility, opacity, blend mode, and each layer's own pixels) to a
  // project file, not just the flattened composite — the counterpart to
  // openProject below. Reads the open document but never mutates it.
  const saveProject = useCallback(async () => {
    const destination = await save({ filters: PROJECT_FILTER, defaultPath: "untitled.iep" });
    if (typeof destination !== "string") return;
    setBusy(true);
    try {
      await invoke("save_project", { path: destination });
      setError(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, []);

  const openProject = useCallback(async () => {
    const selected = await open({ multiple: false, directory: false, filters: PROJECT_FILTER });
    if (typeof selected === "string") await runCommand("open_project", { path: selected }, "top");
  }, [runCommand]);

  const createNewDocument = useCallback(async () => {
    await runCommand("new_document", { width: newWidth, height: newHeight }, "top");
    setShowNewDialog(false);
  }, [runCommand, newWidth, newHeight]);

  // A drop opens the file when nothing is open, and stacks it as a layer when
  // something is.
  const hasDocument = document !== null;
  const hasDocumentRef = useRef(hasDocument);
  hasDocumentRef.current = hasDocument;

  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "over") {
        setDropping(true);
        return;
      }
      setDropping(false);
      if (event.payload.type !== "drop") return;
      const [first] = event.payload.paths;
      if (!first) return;
      void runCommand(
        hasDocumentRef.current ? "add_layer" : "open_document",
        { path: first },
        "top",
      );
    });
    return () => {
      void unlisten.then((off) => off());
    };
  }, [runCommand]);

  // Sends the segment `points` (1 or 2 document-space coordinates) to the
  // active tool's command. Not gated on `busy`, for the same reason the
  // opacity slider isn't: each pointer move is its own command, and stale
  // responses are already discarded by `runCommand`'s ticket.
  const applyStroke = useCallback(
    (points: [number, number][]) => {
      if (selectedId === null) return;
      if (tool === "eraser") {
        void runCommand("erase_stroke", { id: selectedId, points, radius: brushSize });
      } else {
        const [r, g, b] = hexToRgb(brushColor);
        const alpha = Math.round(brushOpacity * 255);
        void runCommand("paint_stroke", {
          id: selectedId,
          points,
          radius: brushSize,
          color: [r, g, b, alpha],
        });
      }
    },
    [runCommand, selectedId, tool, brushColor, brushOpacity, brushSize],
  );

  const canPaint = document !== null && selectedId !== null;
  const isMarqueeTool = tool === "selectRect" || tool === "selectEllipse";

  const handlePointerDown = useCallback(
    (event: React.PointerEvent<HTMLImageElement>) => {
      if (!document) return;
      if (isMarqueeTool) {
        event.currentTarget.setPointerCapture(event.pointerId);
        const point = toDocPoint(event, document);
        marqueeStart.current = point;
        setMarqueePreview({ start: point, current: point });
        return;
      }
      if (!canPaint) return;
      event.currentTarget.setPointerCapture(event.pointerId);
      const point = toDocPoint(event, document);
      lastPoint.current = point;
      // Two invoke() calls fired back to back in the same tick race for the
      // document lock on the Rust side with no guaranteed order — awaiting
      // the checkpoint's own promise is what actually guarantees it lands
      // before the stroke's first segment does.
      void checkpoint().then(() => applyStroke([point]));
    },
    [document, isMarqueeTool, canPaint, checkpoint, applyStroke],
  );

  const handlePointerMove = useCallback(
    (event: React.PointerEvent<HTMLImageElement>) => {
      if (!document) return;
      if (isMarqueeTool) {
        if (marqueeStart.current === null) return;
        setMarqueePreview({ start: marqueeStart.current, current: toDocPoint(event, document) });
        return;
      }
      if (lastPoint.current === null) return;
      const point = toDocPoint(event, document);
      const previous = lastPoint.current;
      lastPoint.current = point;
      applyStroke([previous, point]);
    },
    [document, isMarqueeTool, applyStroke],
  );

  const endStroke = useCallback(
    (event: React.PointerEvent<HTMLImageElement>) => {
      if (event.currentTarget.hasPointerCapture(event.pointerId)) {
        event.currentTarget.releasePointerCapture(event.pointerId);
      }
      if (isMarqueeTool) {
        const start = marqueeStart.current;
        marqueeStart.current = null;
        setMarqueePreview(null);
        if (start && document) {
          const [x0, y0] = start;
          const [x1, y1] = toDocPoint(event, document);
          // A click with no drag has no area to select — silently a no-op,
          // rather than round-tripping to the backend just to show its
          // "must cover at least one pixel" error for an everyday click.
          if (x0 !== x1 || y0 !== y1) {
            const command = tool === "selectRect" ? "select_rectangle" : "select_ellipse";
            void runCommand(command, { x0, y0, x1, y1 });
          }
        }
        return;
      }
      lastPoint.current = null;
    },
    [isMarqueeTool, document, tool, runCommand],
  );

  const layers = document?.layers ?? [];
  const compositeSrc = generation !== null ? `composite://composite.png?g=${generation}` : null;

  return (
    <div className={`app${dropping ? " app--dropping" : ""}`}>
      <header className="toolbar">
        <h1 className="toolbar__title">Image Editor</h1>
        <button
          className="button"
          onClick={() => setShowNewDialog(true)}
          disabled={busy}
        >
          New…
        </button>
        <button className="button" onClick={openDocument} disabled={busy}>
          Open PNG…
        </button>
        <button className="button button--quiet" onClick={addLayer} disabled={busy || !hasDocument}>
          Add layer…
        </button>
        <button
          className="button button--quiet"
          onClick={exportDocument}
          disabled={busy || !hasDocument}
        >
          Export PNG…
        </button>
        <button className="button button--quiet" onClick={openProject} disabled={busy}>
          Open Project…
        </button>
        <button
          className="button button--quiet"
          onClick={saveProject}
          disabled={busy || !hasDocument}
        >
          Save Project…
        </button>

        <div className="tools" role="group" aria-label="Undo history">
          <button
            className="button button--quiet"
            onClick={undo}
            disabled={busy || !canUndo}
            title="Undo (Ctrl/Cmd+Z)"
          >
            Undo
          </button>
          <button
            className="button button--quiet"
            onClick={redo}
            disabled={busy || !canRedo}
            title="Redo (Ctrl/Cmd+Shift+Z)"
          >
            Redo
          </button>
        </div>

        <div className="tools" role="group" aria-label="Selection tool">
          <button
            className={`button button--quiet${tool === "selectRect" ? " button--active" : ""}`}
            disabled={!hasDocument}
            aria-pressed={tool === "selectRect"}
            onClick={() => setTool("selectRect")}
          >
            Rect Select
          </button>
          <button
            className={`button button--quiet${tool === "selectEllipse" ? " button--active" : ""}`}
            disabled={!hasDocument}
            aria-pressed={tool === "selectEllipse"}
            onClick={() => setTool("selectEllipse")}
          >
            Ellipse Select
          </button>
          <button
            className="button button--quiet"
            onClick={selectAll}
            disabled={busy || !hasDocument}
            title="Select All (Ctrl/Cmd+A)"
          >
            Select All
          </button>
          <button
            className="button button--quiet"
            onClick={invertSelection}
            disabled={busy || !hasSelection}
            title="Invert Selection (Ctrl/Cmd+Shift+I)"
          >
            Invert
          </button>
          <button
            className="button button--quiet"
            onClick={deselect}
            disabled={busy || !hasSelection}
            title="Deselect (Ctrl/Cmd+D)"
          >
            Deselect
          </button>
        </div>

        <div className="tools" role="group" aria-label="Paint tool">
          <button
            className={`button button--quiet${tool === "brush" ? " button--active" : ""}`}
            disabled={!canPaint}
            aria-pressed={tool === "brush"}
            onClick={() => setTool("brush")}
          >
            Brush
          </button>
          <button
            className={`button button--quiet${tool === "eraser" ? " button--active" : ""}`}
            disabled={!canPaint}
            aria-pressed={tool === "eraser"}
            onClick={() => setTool("eraser")}
          >
            Eraser
          </button>
          <input
            type="color"
            className="tools__color"
            value={brushColor}
            disabled={!canPaint || tool === "eraser"}
            aria-label="Brush color"
            onChange={(event) => setBrushColor(event.target.value)}
          />
          <label className="tools__slider">
            Size
            <input
              type="range"
              min={1}
              max={150}
              value={brushSize}
              disabled={!canPaint}
              onChange={(event) => setBrushSize(Number(event.target.value))}
            />
          </label>
          <label className="tools__slider">
            Flow
            <input
              type="range"
              min={1}
              max={100}
              value={Math.round(brushOpacity * 100)}
              disabled={!canPaint || tool === "eraser"}
              onChange={(event) => setBrushOpacity(Number(event.target.value) / 100)}
            />
          </label>
        </div>
      </header>

      {showNewDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowNewDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="New document"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">New document</h2>
            <label className="control">
              <span className="control__label">Width</span>
              <input
                type="number"
                min={1}
                max={8000}
                value={newWidth}
                onChange={(event) => setNewWidth(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">Height</span>
              <input
                type="number"
                min={1}
                max={8000}
                value={newHeight}
                onChange={(event) => setNewHeight(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowNewDialog(false)}>
                Cancel
              </button>
              <button
                className="button"
                onClick={createNewDocument}
                disabled={busy || newWidth < 1 || newHeight < 1}
              >
                Create
              </button>
            </div>
          </div>
        </div>
      )}

      <div className="workspace">
        <main className="stage">
          {error && (
            <div className="notice notice--error" role="alert">
              {error}
            </div>
          )}
          {!error && !compositeSrc && (
            <div className="notice">
              <p className="notice__lead">No image open</p>
              <p>
                Click <strong>Open PNG…</strong> or drop a .png file onto this window. Drop another
                to stack it as a layer.
              </p>
            </div>
          )}
          {compositeSrc && document && (
            <div className="canvas-wrap">
              <img
                className={`canvas${(isMarqueeTool ? hasDocument : canPaint) ? ` canvas--${tool}` : ""}`}
                src={compositeSrc}
                alt="Flattened composite"
                draggable={false}
                onDragStart={(event) => event.preventDefault()}
                onPointerDown={handlePointerDown}
                onPointerMove={handlePointerMove}
                onPointerUp={endStroke}
                onPointerCancel={endStroke}
                onPointerLeave={(event) => {
                  // Pointer capture keeps delivering move/up here even once the
                  // cursor leaves the element, but a mouse that was never
                  // pressed on the canvas has no capture to keep the stroke
                  // alive — treat leaving as the end of the stroke either way.
                  if (!event.currentTarget.hasPointerCapture(event.pointerId)) endStroke(event);
                }}
              />
              {marqueePreview && (
                <div
                  className={`selection-outline${tool === "selectEllipse" ? " selection-outline--ellipse" : ""}`}
                  style={overlayStyle(
                    marqueeBounds(marqueePreview.start, marqueePreview.current, document),
                    document,
                  )}
                />
              )}
              {!marqueePreview && document.selection && (
                <>
                  <div
                    className={`selection-outline${
                      document.selection.shape === "ellipse" ? " selection-outline--ellipse" : ""
                    }`}
                    style={overlayStyle(document.selection.bounds, document)}
                  />
                  {document.selection.inverted && (
                    // Select > Inverse selects everywhere *outside* the shape above —
                    // a second outline around the whole canvas marks that outer edge.
                    <div
                      className="selection-outline"
                      style={overlayStyle(
                        { x0: 0, y0: 0, x1: document.width, y1: document.height },
                        document,
                      )}
                    />
                  )}
                </>
              )}
            </div>
          )}
        </main>

        <LayerPanel
          layers={layers}
          selectedId={selectedId}
          blendModes={blendModes}
          disabled={busy}
          onSelect={setSelectedId}
          onToggleVisible={(id, visible) =>
            void runCommand("set_layer_visible", { id, visible })
          }
          onOpacity={(id, opacity) => void runCommand("set_layer_opacity", { id, opacity })}
          onOpacityDragStart={checkpoint}
          onBlendMode={(id, blendMode: BlendMode) =>
            void runCommand("set_layer_blend_mode", { id, blendMode })
          }
          onMove={(id, direction: MoveDirection) =>
            void runCommand("move_layer", { id, direction })
          }
          onRemove={(id) => void runCommand("remove_layer", { id })}
        />
      </div>

      <footer className="statusbar">
        {document ? (
          <>
            <span className="statusbar__name">
              {document.width} × {document.height}
            </span>
            <span>
              {layers.length} layer{layers.length === 1 ? "" : "s"}
            </span>
          </>
        ) : (
          <span className="statusbar__name">Ready</span>
        )}
      </footer>
    </div>
  );
}
