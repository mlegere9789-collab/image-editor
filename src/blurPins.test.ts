import { test } from "node:test";
import assert from "node:assert/strict";
import { documentPoint, percentOf, pixelDistance, ringStyle } from "./blurPins.ts";

test("a pointer over the canvas maps to the document pixel under it, clamped to the canvas", () => {
  const canvas = { left: 100, top: 50, width: 400, height: 300 };
  const doc = { width: 800, height: 600 };
  assert.deepEqual(documentPoint(canvas, 100, 50, doc), { x: 0, y: 0 });
  assert.deepEqual(documentPoint(canvas, 300, 200, doc), { x: 400, y: 300 });
  assert.deepEqual(documentPoint(canvas, 500, 350, doc), { x: 800, y: 600 });
  assert.deepEqual(documentPoint(canvas, 0, 0, doc), { x: 0, y: 0 });
  assert.deepEqual(documentPoint(canvas, 900, 900, doc), { x: 800, y: 600 });
  assert.deepEqual(documentPoint({ left: 0, top: 0, width: 0, height: 0 }, 5, 5, doc), {
    x: 0,
    y: 0,
  });
});

test("document points and radii become canvas percentages", () => {
  assert.equal(percentOf(200, 800), "25%");
  assert.equal(percentOf(3, 0), "0%");
  assert.deepEqual(ringStyle({ x: 400, y: 300 }, 100, { width: 800, height: 600 }), {
    left: "37.5%",
    top: "33.33333333333333%",
    width: "25%",
    height: "33.33333333333333%",
  });
  assert.equal(pixelDistance({ x: 0, y: 0 }, { x: 3, y: 4 }), 5);
  assert.equal(pixelDistance({ x: 10, y: 10 }, { x: 10, y: 10 }), 0);
});
