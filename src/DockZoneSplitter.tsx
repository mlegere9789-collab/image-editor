import { useCallback, useRef } from "react";

import type React from "react";

type Props = {
  onResize: (height: number) => void;
};

/** Window > Panel Stacking: the draggable divider between two panels that
 * share one dock zone, pinning the panel immediately above it to an
 * explicit height and letting the last panel in the zone absorb whatever
 * space is left (see the `flex` this drives in `App.tsx`'s own dock-zone
 * assembly). Height is read fresh from that panel's own current rendered
 * height at the start of *each* drag, rather than trusting whatever
 * `stackHeights` last held for it -- the very first drag on a zone that
 * has never been resized has nothing there yet (the panel is still sized
 * by its own content), and a stale starting point would make the panel
 * jump the instant the drag begins instead of growing/shrinking smoothly
 * from wherever it visually already is. */
export default function DockZoneSplitter({ onResize }: Props) {
  const drag = useRef<{ startY: number; startHeight: number } | null>(null);

  const onPointerDown = useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    event.currentTarget.setPointerCapture(event.pointerId);
    const above = event.currentTarget.previousElementSibling;
    const startHeight = above instanceof HTMLElement ? above.getBoundingClientRect().height : 200;
    drag.current = { startY: event.clientY, startHeight };
  }, []);

  const onPointerMove = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      if (!drag.current) return;
      const next = Math.max(80, drag.current.startHeight + (event.clientY - drag.current.startY));
      onResize(next);
    },
    [onResize],
  );

  const onPointerUp = useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    event.currentTarget.releasePointerCapture(event.pointerId);
    drag.current = null;
  }, []);

  return (
    <div
      className="dock-zone__splitter"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      role="separator"
      aria-orientation="horizontal"
      aria-label="Resize panel"
    />
  );
}
