import { useEffect, useState } from "react";

import type React from "react";
import type {
  BlendMode,
  BlendModeInfo,
  LayerGroup,
  LayerView,
  MoveDirection,
} from "./types";

type Props = {
  /** Extra panels rendered under the layer list (the Channels panel). */
  children?: React.ReactNode;
  /** Bottom-to-top, as the model stores them. */
  layers: LayerView[];
  /** Layer groups; a header row is drawn above each group's top member. */
  groups: LayerGroup[];
  selectedId: number | null;
  blendModes: BlendModeInfo[];
  disabled: boolean;
  onSelect: (id: number) => void;
  onToggleVisible: (id: number, visible: boolean) => void;
  onToggleLocked: (id: number, locked: boolean) => void;
  onToggleLinked: (id: number, linked: boolean) => void;
  onToggleClipped: (id: number, clipped: boolean) => void;
  onGroupVisible: (index: number, visible: boolean) => void;
  onUngroup: (index: number) => void;
  onOpacity: (id: number, opacity: number) => void;
  /** Called once, when an opacity drag starts, so the whole drag undoes as
   * one step rather than one step per `onOpacity` call it makes. */
  onOpacityDragStart: () => void;
  /** Properties panel's Mask Density, 0..=1; only called when `hasMask`. */
  onMaskDensity: (id: number, density: number) => void;
  /** Same one-step-undo purpose as `onOpacityDragStart`, for the Mask
   * Density slider. */
  onMaskDensityDragStart: () => void;
  /** Properties panel's Mask Feather radius in pixels; only called when
   * `hasMask`. */
  onMaskFeather: (id: number, radius: number) => void;
  /** Same one-step-undo purpose as `onOpacityDragStart`, for the Mask
   * Feather slider. */
  onMaskFeatherDragStart: () => void;
  onBlendMode: (id: number, mode: BlendMode) => void;
  onMove: (id: number, direction: MoveDirection) => void;
  onRemove: (id: number) => void;
  onDuplicate: (id: number) => void;
  onMergeVisible: () => void;
  onFlattenImage: () => void;
  onMergeDown: (id: number) => void;
  onRasterize: (id: number) => void;
  onFlipHorizontal: (id: number) => void;
  onFlipVertical: (id: number) => void;
  onRotate180: (id: number) => void;
};

export default function LayerPanel({
  children,
  layers,
  selectedId,
  blendModes,
  disabled,
  onSelect,
  onToggleVisible,
  onToggleLocked,
  onToggleLinked,
  onToggleClipped,
  groups,
  onGroupVisible,
  onUngroup,
  onOpacity,
  onOpacityDragStart,
  onMaskDensity,
  onMaskDensityDragStart,
  onMaskFeather,
  onMaskFeatherDragStart,
  onBlendMode,
  onMove,
  onRemove,
  onDuplicate,
  onMergeVisible,
  onFlattenImage,
  onMergeDown,
  onRasterize,
  onFlipHorizontal,
  onFlipVertical,
  onRotate180,
}: Props) {
  // While the opacity slider is being dragged its value has to come from the
  // pointer, not from the last command that happened to land - otherwise the
  // thumb snaps backwards mid-drag. The draft holds the in-flight value until
  // the model catches up.
  const [draftOpacity, setDraftOpacity] = useState<number | null>(null);
  const [draftMaskDensity, setDraftMaskDensity] = useState<number | null>(null);
  const [draftMaskFeather, setDraftMaskFeather] = useState<number | null>(null);

  const selected = layers.find((layer) => layer.id === selectedId) ?? null;

  useEffect(() => {
    setDraftOpacity(null);
    setDraftMaskDensity(null);
    setDraftMaskFeather(null);
  }, [selectedId]);

  useEffect(() => {
    if (
      draftOpacity !== null &&
      selected &&
      Math.abs(selected.opacity - draftOpacity) < 1e-6
    ) {
      setDraftOpacity(null);
    }
  }, [draftOpacity, selected]);

  useEffect(() => {
    if (
      draftMaskDensity !== null &&
      selected &&
      Math.abs(selected.maskDensity - draftMaskDensity) < 1e-6
    ) {
      setDraftMaskDensity(null);
    }
  }, [draftMaskDensity, selected]);

  useEffect(() => {
    if (
      draftMaskFeather !== null &&
      selected &&
      selected.maskFeather === draftMaskFeather
    ) {
      setDraftMaskFeather(null);
    }
  }, [draftMaskFeather, selected]);

  /** Opacity to display for a layer: the draft wins for the one being dragged. */
  const shownOpacity = (layer: LayerView) =>
    layer.id === selectedId && draftOpacity !== null
      ? draftOpacity
      : layer.opacity;

  /** Same draft-wins-during-a-drag rule as `shownOpacity`, for Mask Density. */
  const shownMaskDensity = (layer: LayerView) =>
    layer.id === selectedId && draftMaskDensity !== null
      ? draftMaskDensity
      : layer.maskDensity;

  /** Same draft-wins-during-a-drag rule as `shownOpacity`, for Mask Feather. */
  const shownMaskFeather = (layer: LayerView) =>
    layer.id === selectedId && draftMaskFeather !== null
      ? draftMaskFeather
      : layer.maskFeather;

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
              className={`layer${layer.id === selectedId ? " layer--selected" : ""}${
                groups.some((group) => group.members.includes(layer.id))
                  ? " layer--grouped"
                  : ""
              }`}
              onClick={() => onSelect(layer.id)}
            >
              {groups.map((group, index) =>
                group.members[group.members.length - 1] === layer.id ? (
                  <div
                    className="layer__group"
                    key={group.name}
                    onClick={(event) => event.stopPropagation()}
                  >
                    <input
                      type="checkbox"
                      className="layer__eye"
                      checked={group.members.every(
                        (id) =>
                          layers.find((l) => l.id === id)?.visible ?? true,
                      )}
                      disabled={disabled}
                      aria-label={`Show or hide group ${group.name}`}
                      onChange={(event) =>
                        onGroupVisible(index, event.target.checked)
                      }
                    />
                    <span className="layer__name">📁 {group.name}</span>
                    <button
                      className="button button--quiet"
                      disabled={disabled}
                      onClick={() => onUngroup(index)}
                      title="Ungroup these layers"
                    >
                      Ungroup
                    </button>
                  </div>
                ) : null,
              )}
              <input
                type="checkbox"
                className="layer__eye"
                checked={layer.visible}
                disabled={disabled}
                aria-label={`${layer.visible ? "Hide" : "Show"} ${layer.name}`}
                onClick={(event) => event.stopPropagation()}
                onChange={(event) =>
                  onToggleVisible(layer.id, event.target.checked)
                }
              />
              <input
                type="checkbox"
                className="layer__lock"
                checked={layer.locked}
                disabled={disabled}
                aria-label={`${layer.locked ? "Unlock" : "Lock"} ${layer.name}`}
                title={
                  layer.locked ? "Locked (paint/erase blocked)" : "Not locked"
                }
                onClick={(event) => event.stopPropagation()}
                onChange={(event) =>
                  onToggleLocked(layer.id, event.target.checked)
                }
              />
              <input
                type="checkbox"
                className="layer__link"
                checked={layer.linked}
                disabled={disabled}
                aria-label={`${layer.linked ? "Unlink" : "Link"} ${layer.name}`}
                title={
                  layer.linked
                    ? "Linked: moves with the other linked layers"
                    : "Not linked"
                }
                onClick={(event) => event.stopPropagation()}
                onChange={(event) =>
                  onToggleLinked(layer.id, event.target.checked)
                }
              />
              <input
                type="checkbox"
                className="layer__clip"
                checked={layer.clipped}
                disabled={disabled}
                aria-label={`${layer.clipped ? "Release" : "Create"} clipping mask for ${layer.name}`}
                title={
                  layer.clipped
                    ? "Clipped: shows only where the layer below has pixels"
                    : "Not clipped to the layer below"
                }
                onClick={(event) => event.stopPropagation()}
                onChange={(event) =>
                  onToggleClipped(layer.id, event.target.checked)
                }
              />
              <span className="layer__name" title={layer.name}>
                {layer.name}
                {layer.hasMask && (
                  <span className="layer__meta" title="Has a layer mask">
                    {" "}
                    ▣
                  </span>
                )}
                {layer.adjustment && (
                  <span
                    className="layer__meta"
                    title={`Adjustment layer: ${layer.adjustment.kind}`}
                  >
                    {" "}
                    ◐
                  </span>
                )}
                {layer.fill && (
                  <span
                    className="layer__meta"
                    title={`Fill layer: ${layer.fill.kind}`}
                  >
                    {" "}
                    ▨
                  </span>
                )}
                {layer.text && (
                  <span
                    className="layer__meta"
                    title={`Text layer: ${layer.text.text}`}
                  >
                    {" "}
                    T
                  </span>
                )}
                {layer.shape && (
                  <span
                    className="layer__meta"
                    title={`Shape layer: ${layer.shape.spec.kind}`}
                  >
                    {" "}
                    ◇
                  </span>
                )}
                {layer.smart && (
                  <span
                    className="layer__meta"
                    title="Smart object: transforms re-render from its embedded source"
                  >
                    {" "}
                    ▣
                  </span>
                )}
              </span>
              <span className="layer__meta">
                {Math.round(shownOpacity(layer) * 100)}%
              </span>
            </li>
          ))}
        </ul>
      )}

      {layers.length >= 2 && (
        <button
          className="button button--quiet"
          disabled={
            disabled || layers.filter((layer) => layer.visible).length < 2
          }
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
              <span className="control__value">
                {Math.round(shownOpacity(selected) * 100)}%
              </span>
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

          {selected.hasMask && (
            <>
              <label className="control">
                <span className="control__label">
                  Mask Density
                  <span className="control__value">
                    {Math.round(shownMaskDensity(selected) * 100)}%
                  </span>
                </span>
                <input
                  type="range"
                  min={0}
                  max={100}
                  step={1}
                  value={Math.round(shownMaskDensity(selected) * 100)}
                  onPointerDown={onMaskDensityDragStart}
                  onChange={(event) => {
                    const next = Number(event.target.value) / 100;
                    setDraftMaskDensity(next);
                    onMaskDensity(selected.id, next);
                  }}
                />
              </label>

              <label className="control">
                <span className="control__label">
                  Mask Feather
                  <span className="control__value">
                    {shownMaskFeather(selected)}px
                  </span>
                </span>
                <input
                  type="range"
                  min={0}
                  max={250}
                  step={1}
                  value={shownMaskFeather(selected)}
                  onPointerDown={onMaskFeatherDragStart}
                  onChange={(event) => {
                    const next = Number(event.target.value);
                    setDraftMaskFeather(next);
                    onMaskFeather(selected.id, next);
                  }}
                />
              </label>
            </>
          )}

          <label className="control">
            <span className="control__label">Blend mode</span>
            <select
              value={selected.blendMode}
              disabled={disabled}
              onChange={(event) =>
                onBlendMode(selected.id, event.target.value as BlendMode)
              }
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
              disabled={
                disabled || selected.id === layers[layers.length - 1]?.id
              }
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
            className="button button--quiet"
            disabled={disabled}
            onClick={() => onDuplicate(selected.id)}
            title="Layer > Duplicate Layer"
          >
            Duplicate Layer
          </button>

          <div className="control control--row">
            <button
              className="button button--quiet"
              disabled={disabled}
              onClick={() => onFlipHorizontal(selected.id)}
              title="Edit > Transform > Flip Horizontal"
            >
              Flip H
            </button>
            <button
              className="button button--quiet"
              disabled={disabled}
              onClick={() => onFlipVertical(selected.id)}
              title="Edit > Transform > Flip Vertical"
            >
              Flip V
            </button>
            <button
              className="button button--quiet"
              disabled={disabled}
              onClick={() => onRotate180(selected.id)}
              title="Edit > Transform > Rotate 180°"
            >
              Rotate 180°
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
      {children}
    </aside>
  );
}
