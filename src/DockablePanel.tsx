import { useCallback, useRef, useState } from "react";

import type React from "react";

/** Window > Panel Docking's own placement for one panel: pinned to a
 * dock zone beside the canvas, or floating at an absolute position --
 * plus Window > Collapsed Icon Panels' own flag, orthogonal to where
 * the panel sits. */
export type PanelPlacement = (
  | { zone: "left" | "right" }
  | { zone: "float"; x: number; y: number }
) & {
  /** Window > Collapsed Icon Panels: shrunk to a narrow labelled strip,
   * its own content hidden, until expanded again. */
  collapsed?: boolean;
};

type Props = {
  id: string;
  /** Shown on the collapsed strip, and as the grip's own title text. */
  title: string;
  placement: PanelPlacement;
  onPlacementChange: (id: string, placement: PanelPlacement) => void;
  children: React.ReactNode;
};

/** Window > Panel Docking / Floating Panels / Collapsed Icon Panels: a
 * real drag handle that lets any panel it wraps be dragged to the left or
 * right edge of the window to dock there, or dropped anywhere else to
 * float at that position -- the same left/right/float model this app's
 * own Workspaces (Phase 288) could one day save alongside
 * `hiddenTools`/`keyBindings`. Docking is decided purely by where the
 * pointer releases, an 80px zone from either edge of the window;
 * everything else floats. A second, independent toggle collapses the
 * panel to a narrow labelled strip, its own content hidden until
 * expanded again -- Photoshop's own Collapse to Icons, applying to
 * either a docked or a floating panel alike. */
export default function DockablePanel({ id, title, placement, onPlacementChange, children }: Props) {
  // `livePos` is the source of truth read by pointerup -- a ref, not
  // state, so it is always current the instant the drag ends even if a
  // pointerup lands before React has re-rendered the last pointermove's
  // own state update (state is still kept, in `live`, purely to repaint
  // the floating panel's position while the drag is in progress).
  const drag = useRef<{ startX: number; startY: number; originX: number; originY: number } | null>(null);
  const livePos = useRef<{ x: number; y: number } | null>(null);
  const [live, setLive] = useState<{ x: number; y: number } | null>(null);

  const onGripPointerDown = useCallback(
    (event: React.PointerEvent<HTMLButtonElement>) => {
      event.currentTarget.setPointerCapture(event.pointerId);
      const rect = event.currentTarget.parentElement?.getBoundingClientRect();
      const originX = placement.zone === "float" ? placement.x : (rect?.left ?? event.clientX);
      const originY = placement.zone === "float" ? placement.y : (rect?.top ?? event.clientY);
      drag.current = { startX: event.clientX, startY: event.clientY, originX, originY };
      livePos.current = { x: originX, y: originY };
      setLive({ x: originX, y: originY });
    },
    [placement],
  );

  const onGripPointerMove = useCallback((event: React.PointerEvent<HTMLButtonElement>) => {
    if (!drag.current) return;
    const next = {
      x: drag.current.originX + (event.clientX - drag.current.startX),
      y: drag.current.originY + (event.clientY - drag.current.startY),
    };
    livePos.current = next;
    setLive(next);
  }, []);

  const onGripPointerUp = useCallback(
    (event: React.PointerEvent<HTMLButtonElement>) => {
      if (!drag.current) return;
      const finalX = livePos.current?.x ?? drag.current.originX;
      const finalY = livePos.current?.y ?? drag.current.originY;
      drag.current = null;
      livePos.current = null;
      setLive(null);
      const edgeZone = 80;
      const width = event.currentTarget.closest(".dockable-panel")?.clientWidth ?? 260;
      const collapsed = placement.collapsed;
      if (finalX < edgeZone) {
        onPlacementChange(id, { zone: "left", collapsed });
      } else if (finalX + width > window.innerWidth - edgeZone) {
        onPlacementChange(id, { zone: "right", collapsed });
      } else {
        onPlacementChange(id, { zone: "float", x: Math.max(0, finalX), y: Math.max(0, finalY), collapsed });
      }
    },
    [id, onPlacementChange, placement.collapsed],
  );

  const toggleCollapsed = useCallback(() => {
    onPlacementChange(id, { ...placement, collapsed: !placement.collapsed });
  }, [id, placement, onPlacementChange]);

  const floating = placement.zone === "float";
  const collapsed = placement.collapsed ?? false;
  const style: React.CSSProperties | undefined = floating
    ? { left: live?.x ?? placement.x, top: live?.y ?? placement.y }
    : undefined;

  return (
    <div
      className={`dockable-panel${floating ? " dockable-panel--floating" : ""}${collapsed ? " dockable-panel--collapsed" : ""}`}
      style={style}
    >
      <button
        type="button"
        className="dockable-panel__grip"
        title="Drag to the left or right edge of the window to dock, or drop anywhere else to float"
        onPointerDown={onGripPointerDown}
        onPointerMove={onGripPointerMove}
        onPointerUp={onGripPointerUp}
        onPointerCancel={onGripPointerUp}
      >
        ⠿
      </button>
      <button
        type="button"
        className="dockable-panel__collapse"
        title={collapsed ? `Expand ${title}` : `Collapse ${title} to an icon`}
        onClick={toggleCollapsed}
      >
        {collapsed ? "»" : "«"}
      </button>
      {collapsed ? <span className="dockable-panel__label">{title}</span> : children}
    </div>
  );
}
