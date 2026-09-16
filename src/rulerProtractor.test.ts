import { test } from "node:test";
import assert from "node:assert/strict";
import { protractorAngle } from "./rulerProtractor.ts";

test("is zero when both legs share the same angle", () => {
  assert.equal(protractorAngle(30, 30), 0);
});

test("is the plain difference for two legs less than 180 degrees apart", () => {
  // A right angle: a horizontal first leg (0 degrees) and a vertical
  // second leg (90 degrees) read out as exactly 90.
  assert.equal(protractorAngle(0, 90), 90);
});

test("takes the shorter arc when the raw difference exceeds 180", () => {
  // 10 degrees and 350 degrees are 340 apart the long way around, but
  // only 20 degrees apart the short way -- the interior angle a
  // protractor actually shows.
  assert.equal(protractorAngle(10, 350), 20);
});

test("is symmetric in its two arguments", () => {
  assert.equal(protractorAngle(200, 340), protractorAngle(340, 200));
  assert.equal(protractorAngle(200, 340), 140);
});

test("reads a straight line as 180 degrees", () => {
  assert.equal(protractorAngle(45, 225), 180);
});
