import { test } from "node:test";
import assert from "node:assert/strict";
import {
  angleAt,
  handleDragToPercent,
  movedPivotBox,
  referencePivot,
  rotateDragToDegrees,
  scaledBounds,
} from "./freeTransformHandles.ts";

const BOUNDS = { x0: 100, y0: 100, x1: 200, y1: 300 }; // 100x200
const CANVAS = { width: 400, height: 400 };

test("referencePivot: canvas centre ignores the layer, layer points read its bounds", () => {
  assert.deepEqual(referencePivot("canvas", BOUNDS, CANVAS), {
    x: 199.5,
    y: 199.5,
  });
  // bounds are exclusive on x1/y1, so the layer's own last pixel is x1-1/y1-1.
  assert.deepEqual(referencePivot("topLeft", BOUNDS, CANVAS), {
    x: 100,
    y: 100,
  });
  assert.deepEqual(referencePivot("bottomRight", BOUNDS, CANVAS), {
    x: 199,
    y: 299,
  });
  assert.deepEqual(referencePivot("center", BOUNDS, CANVAS), {
    x: 149.5,
    y: 199.5,
  });
});

test("scaledBounds holds the pivot fixed and scales everything else around it", () => {
  const pivot = { x: 149.5, y: 199.5 }; // the layer's own centre
  // 200% about the centre doubles the span each side of the pivot.
  const doubled = scaledBounds(BOUNDS, pivot, 200, 200);
  assert.equal(doubled.x0, 149.5 - 2 * 49.5);
  assert.equal(doubled.x1, 149.5 + 2 * 50.5);
  assert.equal(doubled.y0, 199.5 - 2 * 99.5);
  assert.equal(doubled.y1, 199.5 + 2 * 100.5);
  // 100% is always an exact identity, whatever the pivot.
  assert.deepEqual(scaledBounds(BOUNDS, pivot, 100, 100), BOUNDS);
});

test("handleDragToPercent: dragging the far edge from a near-corner pivot", () => {
  const pivot = { x: 100, y: 100 }; // topLeft: pivot sits on the w/n edges
  const start = { widthPercent: 100, heightPercent: 100 };
  // "e" edge starts at x1=200, 100px from the pivot; +50px -> 150% width.
  assert.deepEqual(
    handleDragToPercent(BOUNDS, pivot, "e", start, 50, 0, false),
    { widthPercent: 150, heightPercent: 100 },
  );
  // "w" edge sits exactly on the pivot (span ~0) -- dragging it is a no-op.
  assert.deepEqual(
    handleDragToPercent(BOUNDS, pivot, "w", start, 50, 0, false),
    start,
  );
  // A corner combines both axes: "se" is x1 (span 100) and y1 (span 200).
  assert.deepEqual(
    handleDragToPercent(BOUNDS, pivot, "se", start, 50, 40, false),
    { widthPercent: 150, heightPercent: 120 },
  );
});

test("handleDragToPercent: maintain aspect applies the larger corner change to both axes", () => {
  const pivot = { x: 100, y: 100 };
  const start = { widthPercent: 100, heightPercent: 100 };
  // Same drag as above (150% width, 120% height) but locked: takes the max.
  assert.deepEqual(
    handleDragToPercent(BOUNDS, pivot, "se", start, 50, 40, true),
    { widthPercent: 150, heightPercent: 150 },
  );
  // A single edge handle is never affected by the aspect lock -- it only
  // ever owned one axis to begin with.
  assert.deepEqual(
    handleDragToPercent(BOUNDS, pivot, "e", start, 50, 999, true),
    { widthPercent: 150, heightPercent: 100 },
  );
});

test("handleDragToPercent floors at 1% and rounds to one decimal place", () => {
  const pivot = { x: 100, y: 100 };
  const start = { widthPercent: 100, heightPercent: 100 };
  // Dragging the far edge almost onto the pivot would go negative -- clamped.
  const shrunk = handleDragToPercent(
    BOUNDS,
    pivot,
    "e",
    start,
    -99.97,
    0,
    false,
  );
  assert.equal(shrunk.widthPercent, 1);
});

test("angleAt matches the same atan2 Show Transform Controls' own rotate handle uses", () => {
  const center = { x: 0, y: 0 };
  assert.equal(angleAt(10, 0, center), 0);
  assert.equal(angleAt(0, 10, center), 90);
  assert.equal(angleAt(-10, 0, center), 180);
  assert.equal(angleAt(0, -10, center), -90);
});

test("rotateDragToDegrees adds the pointer's sweep to whatever rotation the transform already had", () => {
  assert.equal(rotateDragToDegrees(0, 0, 30, false), 30);
  assert.equal(rotateDragToDegrees(45, 10, 25, false), 60);
  // atan2's own range is -180..180 with no wrap-around fixup here -- a
  // sweep that crosses that seam reads as the long way round, exactly as
  // whatever ends up on screen (a real drag never jumps the seam in one
  // pointermove tick, so this matches Show Transform Controls' own rotate
  // handle, which has the same property).
  assert.equal(rotateDragToDegrees(0, 170, -170, false), -340);
});

test("rotateDragToDegrees snaps to the nearest 15 degrees when Shift is held", () => {
  assert.equal(rotateDragToDegrees(0, 0, 22, true), 15);
  assert.equal(rotateDragToDegrees(0, 0, 23, true), 30);
  assert.equal(rotateDragToDegrees(7, 0, 8, true), 15);
});

test("movedPivotBox: no move is exactly scaledBounds, and the origin ratio matches the pivot's own position in the unscaled box", () => {
  const pivot = { x: 149.5, y: 199.5 }; // BOUNDS' own centre
  const { box, originXPercent, originYPercent } = movedPivotBox(
    BOUNDS,
    pivot,
    100,
    100,
    0,
    0,
  );
  assert.deepEqual(box, BOUNDS);
  // (pivot.x - x0) / width * 100 = (149.5 - 100) / 100 * 100
  assert.equal(originXPercent, 49.5);
  // (pivot.y - y0) / height * 100 = (199.5 - 100) / 200 * 100
  assert.equal(originYPercent, 49.75);
});

test("movedPivotBox: scale and move compose -- scaledBounds' own box, then offset by (dx, dy)", () => {
  const pivot = { x: 149.5, y: 199.5 };
  // Hand-computed from scaledBounds(BOUNDS, pivot, 200, 150):
  // x0 = 149.5 + (100 - 149.5)*2 = 50.5, x1 = 149.5 + (200 - 149.5)*2 = 250.5
  // y0 = 199.5 + (100 - 199.5)*1.5 = 50.25, y1 = 199.5 + (300 - 199.5)*1.5 = 350.25
  const scaledOnly = movedPivotBox(BOUNDS, pivot, 200, 150, 0, 0);
  assert.deepEqual(scaledOnly.box, {
    x0: 50.5,
    y0: 50.25,
    x1: 250.5,
    y1: 350.25,
  });
  const moved = movedPivotBox(BOUNDS, pivot, 200, 150, 20, -10);
  assert.deepEqual(moved.box, {
    x0: 70.5,
    y0: 40.25,
    x1: 270.5,
    y1: 340.25,
  });
  // The move offsets the whole box uniformly, so the pivot's own position
  // inside it -- and therefore the CSS transform-origin the rotate handles
  // turn about -- is unchanged by scale, by move, or by the two together.
  assert.equal(moved.originXPercent, scaledOnly.originXPercent);
  assert.equal(moved.originYPercent, scaledOnly.originYPercent);
  assert.equal(moved.originXPercent, 49.5);
  assert.equal(moved.originYPercent, 49.75);
});

test("movedPivotBox falls back to a 50% origin only for the axis that is actually zero-size", () => {
  const degenerate = { x0: 10, y0: 10, x1: 10, y1: 20 }; // zero width, real height
  const { originXPercent, originYPercent } = movedPivotBox(
    degenerate,
    { x: 10, y: 12 }, // not the vertical midpoint, so the two paths read differently
    100,
    100,
    0,
    0,
  );
  // Zero-width axis: the divide-by-zero guard's own 50% default.
  assert.equal(originXPercent, 50);
  // Non-zero-height axis: the real ratio, (12 - 10) / 10 * 100.
  assert.equal(originYPercent, 20);
});
