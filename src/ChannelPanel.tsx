import { useState } from "react";
import type { ChannelView, MoveDirection } from "./types";

/** Channel Thumbnail Options: the size of the thumbnail beside each row. */
export type ChannelThumbs = "none" | "small" | "medium" | "large";

type Props = {
  /** Bumped whenever the composite changes; cache-busts the thumbnails. */
  generation: number | null;
  /** Alpha channel names, in panel order. */
  channels: string[];
  /** What the canvas is showing. */
  view: ChannelView;
  thumbs: ChannelThumbs;
  disabled: boolean;
  onSelect: (view: ChannelView) => void;
  onThumbs: (thumbs: ChannelThumbs) => void;
  onAdd: () => void;
  onRename: (old: string, next: string) => void;
  onMove: (name: string, direction: MoveDirection) => void;
  onDelete: (name: string) => void;
  onLoad: (name: string) => void;
};

/** The query value the `composite://` protocol reads for a view. */
export function channelQuery(view: ChannelView): string {
  return view.kind === "alpha" ? `alpha:${encodeURIComponent(view.name)}` : view.kind;
}

function sameView(a: ChannelView, b: ChannelView): boolean {
  return a.kind === b.kind && (a.kind !== "alpha" || b.kind !== "alpha" || a.name === b.name);
}

const FIXED: { view: ChannelView; label: string }[] = [
  { view: { kind: "composite" }, label: "RGB" },
  { view: { kind: "red" }, label: "Red" },
  { view: { kind: "green" }, label: "Green" },
  { view: { kind: "blue" }, label: "Blue" },
];

/** Photoshop's Channels panel: the composite, its three colour channels,
 * and every alpha channel, each selectable as the canvas view, with
 * thumbnails at a chosen size and rename / reorder / delete / load for
 * the alpha channels. */
export default function ChannelPanel({
  generation,
  channels,
  view,
  thumbs,
  disabled,
  onSelect,
  onThumbs,
  onAdd,
  onRename,
  onMove,
  onDelete,
  onLoad,
}: Props) {
  const [renaming, setRenaming] = useState<{ name: string; draft: string } | null>(null);

  const rows: { view: ChannelView; label: string }[] = [
    ...FIXED,
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
      <button className="button button--quiet" onClick={onAdd} disabled={disabled} title="New Channel: a black alpha channel">
        New Channel
      </button>
    </section>
  );
}
