import { useState } from "react";
import type { ChannelView, ColorMode, MoveDirection, SpotChannelView } from "./types";

/** Channel Thumbnail Options: the size of the thumbnail beside each row. */
export type ChannelThumbs = "none" | "small" | "medium" | "large";

type Props = {
  /** Bumped whenever the composite changes; cache-busts the thumbnails. */
  generation: number | null;
  /** Alpha channel names, in panel order. */
  channels: string[];
  /** Spot colour channels, in overprinting order. */
  spots: SpotChannelView[];
  /** What the canvas is showing. */
  view: ChannelView;
  /** Image > Mode, which decides the fixed rows. */
  mode: ColorMode;
  thumbs: ChannelThumbs;
  disabled: boolean;
  onSelect: (view: ChannelView) => void;
  onThumbs: (thumbs: ChannelThumbs) => void;
  onAdd: () => void;
  onRename: (old: string, next: string) => void;
  onMove: (name: string, direction: MoveDirection) => void;
  onDelete: (name: string) => void;
  onLoad: (name: string) => void;
  onNewSpot: () => void;
  onEditSpot: (name: string) => void;
  onMoveSpot: (name: string, direction: MoveDirection) => void;
  onDeleteSpot: (name: string) => void;
  onMergeSpot: (name: string) => void;
  onConvertToSpot: (name: string) => void;
};

/** The query value the `composite://` protocol reads for a view. */
export function channelQuery(view: ChannelView): string {
  if (view.kind === "alpha") return `alpha:${encodeURIComponent(view.name)}`;
  if (view.kind === "spot") return `spot:${encodeURIComponent(view.name)}`;
  return view.kind;
}

function sameView(a: ChannelView, b: ChannelView): boolean {
  return a.kind === b.kind && (!("name" in a) || !("name" in b) || a.name === b.name);
}

/** The fixed rows for each mode — Photoshop's own Channels panel layout. */
function fixedRows(mode: ColorMode): { view: ChannelView; label: string }[] {
  switch (mode) {
    case "cmyk":
      return [
        { view: { kind: "composite" }, label: "CMYK" },
        { view: { kind: "cyan" }, label: "Cyan" },
        { view: { kind: "magenta" }, label: "Magenta" },
        { view: { kind: "yellow" }, label: "Yellow" },
        { view: { kind: "black" }, label: "Black" },
      ];
    case "lab":
      return [
        { view: { kind: "composite" }, label: "Lab" },
        { view: { kind: "lightness" }, label: "Lightness" },
        { view: { kind: "aStar" }, label: "a" },
        { view: { kind: "bStar" }, label: "b" },
      ];
    case "multichannel":
      return [];
    case "grayscale":
    case "bitmap":
    case "duotone":
      return [{ view: { kind: "composite" }, label: mode === "duotone" ? "Duotone" : "Gray" }];
    case "indexed":
      return [{ view: { kind: "composite" }, label: "Index" }];
    default:
      return [
        { view: { kind: "composite" }, label: "RGB" },
        { view: { kind: "red" }, label: "Red" },
        { view: { kind: "green" }, label: "Green" },
        { view: { kind: "blue" }, label: "Blue" },
      ];
  }
}

/** Photoshop's Channels panel: the composite, its three colour channels,
 * and every alpha channel, each selectable as the canvas view, with
 * thumbnails at a chosen size and rename / reorder / delete / load for
 * the alpha channels. */
export default function ChannelPanel({
  generation,
  channels,
  spots,
  view,
  mode,
  thumbs,
  disabled,
  onSelect,
  onThumbs,
  onAdd,
  onRename,
  onMove,
  onDelete,
  onLoad,
  onNewSpot,
  onEditSpot,
  onMoveSpot,
  onDeleteSpot,
  onMergeSpot,
  onConvertToSpot,
}: Props) {
  const [renaming, setRenaming] = useState<{ name: string; draft: string } | null>(null);

  const rows: { view: ChannelView; label: string }[] = [
    ...fixedRows(mode),
    ...channels.map((name) => ({ view: { kind: "alpha", name } as ChannelView, label: name })),
  ];

  return (
    <section className="channels">
      <div className="channels__header">
        <h2 className="panel__heading">Channels</h2>
        <label className="channels__thumbs">
          Thumbnails
          <select
            value={thumbs}
            onChange={(event) => onThumbs(event.target.value as ChannelThumbs)}
            title="Channel Thumbnail Options"
          >
            <option value="none">None</option>
            <option value="small">Small</option>
            <option value="medium">Medium</option>
            <option value="large">Large</option>
          </select>
        </label>
      </div>
      <ul className="layers">
        {rows.map(({ view: rowView, label }) => {
          const active = sameView(rowView, view);
          const isAlpha = rowView.kind === "alpha";
          return (
            <li
              key={label + rowView.kind}
              className={`layer channel${active ? " layer--selected" : ""}`}
              onClick={() => onSelect(rowView)}
              title={
                isAlpha
                  ? "Show and edit this alpha channel on the canvas; brush strokes paint its grey"
                  : `Show the ${label} channel on the canvas`
              }
            >
              {thumbs !== "none" && generation !== null && (
                <img
                  className={`channel__thumb channel__thumb--${thumbs}`}
                  src={`composite://composite.png?g=${generation}&channel=${channelQuery(rowView)}`}
                  alt=""
                  draggable={false}
                />
              )}
              {isAlpha && renaming?.name === label ? (
                <input
                  className="channel__rename"
                  value={renaming.draft}
                  autoFocus
                  onClick={(event) => event.stopPropagation()}
                  onChange={(event) => setRenaming({ name: label, draft: event.target.value })}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      onRename(label, renaming.draft);
                      setRenaming(null);
                    } else if (event.key === "Escape") {
                      setRenaming(null);
                    }
                  }}
                  onBlur={() => setRenaming(null)}
                />
              ) : (
                <span
                  className="layer__name"
                  onDoubleClick={(event) => {
                    if (!isAlpha) return;
                    event.stopPropagation();
                    setRenaming({ name: label, draft: label });
                  }}
                  title={isAlpha ? "Double-click to rename" : undefined}
                >
                  {label}
                </span>
              )}
              {isAlpha && (
                <span className="channel__actions" onClick={(event) => event.stopPropagation()}>
                  <button
                    className="button button--quiet"
                    onClick={() => onMove(label, "up")}
                    disabled={disabled || channels[0] === label}
                    title="Move the channel up"
                  >
                    ↑
                  </button>
                  <button
                    className="button button--quiet"
                    onClick={() => onMove(label, "down")}
                    disabled={disabled || channels[channels.length - 1] === label}
                    title="Move the channel down"
                  >
                    ↓
                  </button>
                  <button
                    className="button button--quiet"
                    onClick={() => onLoad(label)}
                    disabled={disabled}
                    title="Load the channel as a selection (grey 128 and up)"
                  >
                    Load
                  </button>
                  <button
                    className="button button--quiet"
                    onClick={() => onConvertToSpot(label)}
                    disabled={disabled}
                    title="Convert Alpha Channel to Spot Channel: its white areas become ink"
                  >
                    Spot
                  </button>
                  <button
                    className="button button--quiet"
                    onClick={() => onDelete(label)}
                    disabled={disabled}
                    title="Delete the channel"
                  >
                    ×
                  </button>
                </span>
              )}
            </li>
          );
        })}
      </ul>
      {spots.length > 0 && (
        <ul className="layers">
          {spots.map((spot) => {
            const rowView: ChannelView = { kind: "spot", name: spot.name };
            const active = sameView(rowView, view);
            return (
              <li
                key={`spot:${spot.name}`}
                className={`layer channel${active ? " layer--selected" : ""}`}
                onClick={() => onSelect(rowView)}
                title="Show and edit this spot channel on the canvas; black brush strokes lay down ink"
              >
                {thumbs !== "none" && generation !== null && (
                  <img
                    className={`channel__thumb channel__thumb--${thumbs}`}
                    src={`composite://composite.png?g=${generation}&channel=${channelQuery(rowView)}`}
                    alt=""
                    draggable={false}
                  />
                )}
                <span
                  className="channel__swatch"
                  style={{ background: `rgb(${spot.color[0]}, ${spot.color[1]}, ${spot.color[2]})` }}
                  title={`Solidity ${spot.solidity}%`}
                />
                <span className="layer__name" onDoubleClick={() => onEditSpot(spot.name)} title="Double-click for Spot Channel Options">
                  {spot.name}
                </span>
                <span className="channel__actions" onClick={(event) => event.stopPropagation()}>
                  <button className="button button--quiet" onClick={() => onEditSpot(spot.name)} disabled={disabled} title="Spot Channel Options">
                    …
                  </button>
                  <button
                    className="button button--quiet"
                    onClick={() => onMoveSpot(spot.name, "up")}
                    disabled={disabled || spots[0].name === spot.name}
                    title="Overprint earlier (move up)"
                  >
                    ↑
                  </button>
                  <button
                    className="button button--quiet"
                    onClick={() => onMoveSpot(spot.name, "down")}
                    disabled={disabled || spots[spots.length - 1].name === spot.name}
                    title="Overprint later (move down)"
                  >
                    ↓
                  </button>
                  <button className="button button--quiet" onClick={() => onMergeSpot(spot.name)} disabled={disabled} title="Merge Spot Channel: flatten and print the ink into the image">
                    Merge
                  </button>
                  <button className="button button--quiet" onClick={() => onDeleteSpot(spot.name)} disabled={disabled} title="Delete the spot channel">
                    ×
                  </button>
                </span>
              </li>
            );
          })}
        </ul>
      )}
      <button className="button button--quiet" onClick={onAdd} disabled={disabled} title="New Channel: a black alpha channel">
        New Channel
      </button>
      <button className="button button--quiet" onClick={onNewSpot} disabled={disabled} title="New Spot Channel: an ink filled from the selection">
        New Spot Channel…
      </button>
    </section>
  );
}
