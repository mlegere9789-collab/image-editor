import { test } from "node:test";
import assert from "node:assert/strict";
import { cardPlacement, TOUR_STEPS, tourStep } from "./tourLogic.ts";

test("the tour has a first and last card without a target and every other step points somewhere", () => {
  assert.ok(TOUR_STEPS.length >= 5);
  assert.equal(TOUR_STEPS[0].target, null);
  assert.equal(TOUR_STEPS[TOUR_STEPS.length - 1].target, null);
  for (const step of TOUR_STEPS.slice(1, -1)) {
    assert.equal(typeof step.target, "string");
    assert.ok(step.title.length > 0 && step.body.length > 0);
  }
});

test("tourStep walks forward and back and falls off either end", () => {
  assert.equal(tourStep(0, 1, 7), 1);
  assert.equal(tourStep(6, 1, 7), null);
  assert.equal(tourStep(0, -1, 7), null);
  assert.equal(tourStep(3, -1, 7), 2);
  assert.equal(tourStep(0, 1), 1);
});

test("the card sits below its target, above when there is no room below, centred without one", () => {
  const viewport = { width: 1000, height: 600 };
  const card = { width: 320, height: 160 };
  assert.deepEqual(cardPlacement({ top: 0, left: 0, width: 1000, height: 28 }, viewport, card), {
    top: 40,
    left: 340,
  });
  // A target near the bottom pushes the card above it.
  assert.deepEqual(cardPlacement({ top: 560, left: 0, width: 1000, height: 40 }, viewport, card), {
    top: 388,
    left: 340,
  });
  // A target at the far right keeps the card inside the viewport.
  assert.deepEqual(cardPlacement({ top: 0, left: 900, width: 100, height: 28 }, viewport, card), {
    top: 40,
    left: 668,
  });
  // A target filling the viewport leaves no room either way: centred.
  assert.deepEqual(cardPlacement({ top: 0, left: 0, width: 1000, height: 600 }, viewport, card), {
    top: 220,
    left: 340,
  });
  assert.deepEqual(cardPlacement(null, viewport, card), { top: 220, left: 340 });
});
