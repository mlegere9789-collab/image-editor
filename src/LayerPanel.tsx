import { useEffect, useState } from "react";

import type { BlendMode, BlendModeInfo, LayerView, MoveDirection } from "./types";

type Props = {
  /** Bottom-to-top, as the model stores them. */
  layers: LayerView[];
  selectedId: number | null;
  blendModes: BlendModeInfo[];
  disabled: boolean;
  onSelect: (id: number) => void;
  onToggleVisible: (id: number, visible: boolean) => void;
  onToggleLocked: (id: number, locked: boolean) => void;
  onOpacity: (id: number, opacity: number) => void;
  /** Called once, when an opacity drag starts, so the whole drag undoes as
   * one step rather than one step per `onOpacity` call it makes. */
  onOpacityDragStart: () => void;
  onBlendMode: (id: number, mode: BlendMode) => void;
  onMove: (id: number, direction: MoveDirection) => void;
  onRemove: (id: number) => void;
  onMergeVisible: () => void;
  onFlattenImage: () => void;
  onMergeDown: (id: number) => void;
  onRasterize: (id: number) => void;
};

export default function LayerPanel({
  layers,
  selectedId,
  blendModes,
  disabled,
  onSelect,
  onToggleVisible,
  onToggleLocked,
  onOpacity,
  onOpacityDragStart,
  onBlendMode,
  onMove,
  onRemove,
  onMergeVisible,
  onFlattenImage,
  onMergeDown,
  onRasterize,
}: Props) {
  // While the opacity slider is being dragged its value has to come from the
  // pointer, not from the last command that happened to land - otherwise the
  // thumb snaps backwards mid-drag. The draft holds the in-flight value until
  // the model catches up.
  const [draftOpacity, setDraftOpacity] = useState<number | null>(null);

  const selected = layers.find((layer) => layer.id === selectedId) ?? null;

  useEffect(() => {
    setDraftOpacity(null);
  }, [selectedId]);

  useEffect(() => {
    if (draftOpacity !== null && selected && Math.abs(selected.opacity - draftOpacity) < 1e-6) {
      setDraftOpacity(null);
    }
  }, [draftOpacity, selected]);

  /** Opacity to display for a layer: the draft wins for the one being dragged. */
  const shownOpacity = (layer: LayerView) =>
    layer.id === selectedId && draftOpacity !== null ? draftOpacity : layer.opacity;

  // The stack is stored bottom-first but reads top-first, like every other
  // layers panel.
  const topFirst = [...layers].reverse();

  return (
    <aside className="panel">
      <h2 className="panel__heading">Layers</h2>

      {layers.length === 0 ? (
        <p className="panel__empty">No layers yet.</p>
      ) : (
        <ul className="layers">
          {topFirst.map((layer) => (
            <li
              key={layer.id}
              className={`layer${layer.id === selectedId ? " layer--selected" : ""}`}
              onClick={() => onSelect(layer.id)}
            >
              <input
                type="checkbox"
                className="layer__eye"
                checked={layer.visible}
                disabled={disabled}
                aria-label={`${layer.visible ? "Hide" : "Show"} ${layer.name}`}
                onClick={(event) => event.stopPropagation()}
                onChange={(event) => onToggleVisible(layer.id, event.target.checked)}
              />
              <input
                type="checkbox"
                className="layer__lock"
                checked={layer.locked}
                disabled={disabled}
                aria-label={`${layer.locked ? "Unlock" : "Lock"} ${layer.name}`}
                title={layer.locked ? "Locked (paint/erase blocked)" : "Not locked"}
                onClick={(event) => event.stopPropagation()}
                onChange={(event) => onToggleLocked(layer.id, event.target.checked)}
              />
              <span className="layer__name" title={layer.name}>
                {layer.name}
              </span>
              <span className="layer__meta">{Math.round(shownOpacity(layer) * 100)}%</span>
            </li>
          ))}
        </ul>
      )}

      {layers.length >= 2 && (
        <button
          className="button button--quiet"
          disabled={disabled || layers.filter((layer) => layer.visible).length < 2}
          onClick={onMergeVisible}
          title="Merge every visible layer into one"
        >
          Merge Visible
        </button>
      )}

      {layers.length >= 1 && (
        <button
          className="button button--quiet"
          disabled={disabled}
          onClick={onFlattenImage}
          title="Combine every layer into one, discarding hidden layers"
        >
          Flatten Image
        </button>
      )}

      {selected && (
        <div className="controls">
          <label className="control">
            <span className="control__label">
              Opacity
              <span className="control__value">{Math.round(shownOpacity(selected) * 100)}%</span>
            </span>
            {/* Deliberately not disabled while busy: a drag fires a command per
                step, and disabling the input mid-drag cancels the drag. Stale
                responses are already discarded by the caller's sequencing.
                onOpacityDragStart only fires for a pointer-driven drag, not a
                keyboard-driven arrow-key nudge — an accepted gap, not a
                deliberate design choice to exclude keyboard users from undo. */}
            <input
              type="range"
              min={0}
              max={100}
              step={1}
              value={Math.round(shownOpacity(selected) * 100)}
              onPointerDown={onOpacityDragStart}
              onChange={(event) => {
                const next = Number(event.target.value) / 100;
                setDraftOpacity(next);
                onOpacity(selected.id, next);
              }}
            />
          </label>

          <label className="control">
            <span className="control__label">Blend mode</span>
            <select
              value={selected.blendMode}
              disabled={disabled}
              onChange={(event) => onBlendMode(selected.id, event.target.value as BlendMode)}
            >
              {blendModes.map(({ mode, label }) => (
                <option key={mode} value={mode}>
                  {label}
                </option>
              ))}
            </select>
          </label>

          <div className="control control--row">
            <button
              className="button button--quiet"
              disabled={disabled || selected.id === layers[layers.length - 1]?.id}
              onClick={() => onMove(selected.id, "up")}
            >
              Move up
            </button>
            <button
              className="button button--quiet"
              disabled={disabled || selected.id === layers[0]?.id}
              onClick={() => onMove(selected.id, "down")}
            >
              Move down
            </button>
          </div>

          <button
            className="button button--quiet"
            disabled={disabled || selected.id === layers[0]?.id}
            onClick={() => onMergeDown(selected.id)}
            title="Merge this layer with the one below it"
          >
            Merge Down
          </button>

          <button
            className="button button--quiet"
            disabled={disabled}
            onClick={() => onRasterize(selected.id)}
            title="Layer > Rasterize — every layer here is already pixels, so this always succeeds as a no-op"
          >
            Rasterize Layer
          </button>

          <button
            className="button button--danger"
            disabled={disabled}
            onClick={() => onRemove(selected.id)}
          >
            Delete layer
          </button>
        </div>
      )}
    </aside>
  );
}
