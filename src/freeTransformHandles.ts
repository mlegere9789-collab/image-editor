// Edit > Free Transform's on-canvas handles: Photoshop lets you drag the
// bounding box's eight handles and the area just outside its corners
// instead of typing Width/Height/Rotate into the options bar. The geometry
// is pure so it is unit-testable without the component: where the pivot
// (the Reference Point Locator) sits in document pixels, where the
// currently-scaled box's edges are, and what dragging a handle by some
// document-pixel delta does to `widthPercent`/`heightPercent`/`degrees`.

export type Bounds = { x0: number; y0: number; x1: number; y1: number };
export type Point = { x: number; y: number };
export type Size = { width: number; height: number };
export type ReferencePoint =
  | "topLeft"
  | "top"
  | "topRight"
  | "left"
  | "center"
  | "right"
  | "bottomLeft"
  | "bottom"
  | "bottomRight";

/** The transform's pivot, in document pixels -- the exact same point
 * `Document::free_transform` itself resolves `transform.reference` to
 * (see its own match over `ReferencePoint`), so a handle drag's preview
 * lines up with what Apply will actually do. "canvas" (this dialog's own
 * default, sent to the backend as `reference: null`) is the canvas
 * centre; every other option is a point on the layer's own opaque bounds. */
export function referencePivot(
  reference: "canvas" | ReferencePoint,
  bounds: Bounds,
  canvas: Size,
): Point {
  if (reference === "canvas") {
    return { x: (canvas.width - 1) / 2, y: (canvas.height - 1) / 2 };
  }
  const x0 = bounds.x0;
  const x1 = bounds.x1 - 1;
  const y0 = bounds.y0;
  const y1 = bounds.y1 - 1;
  const mx = (x0 + x1) / 2;
  const my = (y0 + y1) / 2;
  switch (reference) {
    case "topLeft":
      return { x: x0, y: y0 };
    case "top":
      return { x: mx, y: y0 };
    case "topRight":
      return { x: x1, y: y0 };
    case "left":
      return { x: x0, y: my };
    case "center":
      return { x: mx, y: my };
    case "right":
      return { x: x1, y: my };
    case "bottomLeft":
      return { x: x0, y: y1 };
    case "bottom":
      return { x: mx, y: y1 };
    case "bottomRight":
      return { x: x1, y: y1 };
  }
}

/** Where `bounds` sits once scaled `widthPercent`/`heightPercent` about
 * `pivot` -- the same inverse mapping `Document::scale_about` applies to
 * every pixel, applied here to just the four corners, since the pivot
 * itself is a fixed point under that mapping. This is the box the on-canvas
 * handles are drawn on. */
export function scaledBounds(
  bounds: Bounds,
  pivot: Point,
  widthPercent: number,
  heightPercent: number,
): Bounds {
  const fx = widthPercent / 100;
  const fy = heightPercent / 100;
  return {
    x0: pivot.x + (bounds.x0 - pivot.x) * fx,
    y0: pivot.y + (bounds.y0 - pivot.y) * fy,
    x1: pivot.x + (bounds.x1 - pivot.x) * fx,
    y1: pivot.y + (bounds.y1 - pivot.y) * fy,
  };
}

/** Solve for the new percent that puts one already-scaled edge exactly
 * `delta` document pixels from where it started, holding `pivot` fixed --
 * the inverse of the one line inside `scaledBounds` that moves this edge.
 * A pivot sitting on the edge itself (dragging "e" while the reference
 * point is Right, say) has no lever to pull, so the drag is a no-op there
 * rather than a divide-by-near-zero blowup. */
function edgePercent(
  originalEdge: number,
  pivot: number,
  startPercent: number,
  delta: number,
): number {
  const span = originalEdge - pivot;
  if (Math.abs(span) < 0.5) return startPercent;
  const startTransformed = pivot + span * (startPercent / 100);
  const newTransformed = startTransformed + delta;
  return (100 * (newTransformed - pivot)) / span;
}

export type HandlePercent = { widthPercent: number; heightPercent: number };

/** Dragging on-canvas handle `handle` ("n"/"s"/"e"/"w" edges, "nw"/"ne"/
 * "sw"/"se" corners) by `(dx, dy)` document pixels from where the drag
 * started: the new Width %/Height % that keeps the pivot fixed and moves
 * only the edge(s) that handle owns by that amount. A locked aspect ratio
 * (the options bar's own checkbox) makes a corner drag apply the larger of
 * the two axis changes to both -- Photoshop's own linked-corner behaviour --
 * and is not consulted for a single edge handle, which only ever owns one
 * axis to begin with. */
export function handleDragToPercent(
  bounds: Bounds,
  pivot: Point,
  handle: string,
  start: HandlePercent,
  dx: number,
  dy: number,
  maintainAspect: boolean,
): HandlePercent {
  let widthPercent = start.widthPercent;
  let heightPercent = start.heightPercent;
  if (handle.includes("w")) {
    widthPercent = edgePercent(bounds.x0, pivot.x, start.widthPercent, dx);
  } else if (handle.includes("e")) {
    widthPercent = edgePercent(bounds.x1, pivot.x, start.widthPercent, dx);
  }
  if (handle.includes("n")) {
    heightPercent = edgePercent(bounds.y0, pivot.y, start.heightPercent, dy);
  } else if (handle.includes("s")) {
    heightPercent = edgePercent(bounds.y1, pivot.y, start.heightPercent, dy);
  }
  widthPercent = Math.max(1, Math.round(widthPercent * 10) / 10);
  heightPercent = Math.max(1, Math.round(heightPercent * 10) / 10);
  if (maintainAspect && handle.length === 2) {
    const scale = Math.max(widthPercent, heightPercent);
    return { widthPercent: scale, heightPercent: scale };
  }
  return { widthPercent, heightPercent };
}

/** The pointer's angle about a screen-space centre, in degrees -- the same
 * arithmetic Show Transform Controls' own rotate handle already uses,
 * pulled out here so Free Transform's rotate handle is testable without
 * the component. */
export function angleAt(
  clientX: number,
  clientY: number,
  center: Point,
): number {
  return (Math.atan2(clientY - center.y, clientX - center.x) * 180) / Math.PI;
}

/** A rotate-handle drag's new `degrees`: whatever rotation the transform
 * already had, plus how far the pointer has swept since the drag started.
 * Shift snaps the result to the nearest 15°. */
export function rotateDragToDegrees(
  baseDegrees: number,
  startAngle: number,
  currentAngle: number,
  snap: boolean,
): number {
  let degrees = baseDegrees + (currentAngle - startAngle);
  if (snap) degrees = Math.round(degrees / 15) * 15;
  return Math.round(degrees * 10) / 10;
}
