import type { BlendMode, BlendModeInfo, LayerView, MoveDirection } from "./types";

type Props = {
  /** Bottom-to-top, as the model stores them. */
  layers: LayerView[];
  selectedId: number | null;
  blendModes: BlendModeInfo[];
  disabled: boolean;
  onSelect: (id: number) => void;
  onToggleVisible: (id: number, visible: boolean) => void;
  onOpacity: (id: number, opacity: number) => void;
  onBlendMode: (id: number, mode: BlendMode) => void;
  onMove: (id: number, direction: MoveDirection) => void;
  onRemove: (id: number) => void;
};

export default function LayerPanel({
  layers,
  selectedId,
  blendModes,
  disabled,
  onSelect,
  onToggleVisible,
  onOpacity,
  onBlendMode,
  onMove,
  onRemove,
}: Props) {
  // The stack is stored bottom-first but reads top-first, like every other
  // layers panel.
  const topFirst = [...layers].reverse();
  const selected = layers.find((layer) => layer.id === selectedId) ?? null;

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
              <span className="layer__name" title={layer.name}>
                {layer.name}
              </span>
              <span className="layer__meta">{Math.round(layer.opacity * 100)}%</span>
            </li>
          ))}
        </ul>
      )}

      {selected && (
        <div className="controls">
          <label className="control">
            <span className="control__label">
              Opacity<span className="control__value">{Math.round(selected.opacity * 100)}%</span>
            </span>
            <input
              type="range"
              min={0}
              max={100}
              step={1}
              value={Math.round(selected.opacity * 100)}
              disabled={disabled}
              onChange={(event) => onOpacity(selected.id, Number(event.target.value) / 100)}
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
