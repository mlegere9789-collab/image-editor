import { test } from "node:test";
import assert from "node:assert/strict";
import { inlineFontSizePx, shouldCommitTypeEdit } from "./typeInlineEdit.ts";

test("shouldCommitTypeEdit keeps anything but whitespace-only text", () => {
  assert.equal(shouldCommitTypeEdit("Hello"), true);
  assert.equal(shouldCommitTypeEdit("  Hello  "), true);
  assert.equal(shouldCommitTypeEdit(""), false);
  assert.equal(shouldCommitTypeEdit("   "), false);
  assert.equal(shouldCommitTypeEdit("\n\t "), false);
});

test("inlineFontSizePx scales by the canvas's own displayed size", () => {
  // A 800px document shown at 400px on screen is half size -- a 32px
  // document font previews at 16 real screen pixels.
  assert.equal(inlineFontSizePx(32, 400, 800), 16);
  // Shown at its native size, the preview is 1:1.
  assert.equal(inlineFontSizePx(32, 800, 800), 32);
  // Zoomed in 2x.
  assert.equal(inlineFontSizePx(32, 1600, 800), 64);
});

test("inlineFontSizePx falls back to the unscaled size before layout exists", () => {
  assert.equal(inlineFontSizePx(32, 0, 800), 32);
  assert.equal(inlineFontSizePx(32, 400, 0), 32);
});
