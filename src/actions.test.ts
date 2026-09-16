import { test } from "node:test";
import assert from "node:assert/strict";
import {
  batchOutputName,
  describeStep,
  isRecordable,
  playArgs,
  recordArgs,
  SELECTED,
} from "./actions.ts";

test("recording swaps the selected layer's id for the token and drops the progress channel", () => {
  const channel = { onmessage: () => {} };
  assert.deepEqual(recordArgs({ id: 7, radius: 2.5, onProgress: channel }, 7), {
    id: SELECTED,
    radius: 2.5,
  });
  // Another layer's id is kept as it is: the step really was about that layer.
  assert.deepEqual(recordArgs({ id: 3, reference: 7 }, 7), { id: 3, reference: 7 });
  assert.deepEqual(recordArgs({ id: 7 }, null), { id: 7 });
  assert.deepEqual(recordArgs({}, 7), {});
});

test("playback aims the token at the layer selected now, and refuses without one", () => {
  assert.deepEqual(playArgs({ id: SELECTED, radius: 2.5 }, 12), { id: 12, radius: 2.5 });
  assert.deepEqual(playArgs({ id: 3 }, 12), { id: 3 });
  assert.throws(() => playArgs({ id: SELECTED }, null), /needs a selected layer/);
  assert.deepEqual(playArgs({ path: "a.png" }, null), { path: "a.png" });
});

test("commands that change which document is open, and history, are never recorded", () => {
  assert.equal(isRecordable("gaussian_blur"), true);
  assert.equal(isRecordable("generate_image"), true);
  assert.equal(isRecordable("open_document"), false);
  assert.equal(isRecordable("undo"), false);
  assert.equal(isRecordable("checkpoint"), false);
});

test("steps describe themselves without the layer id, and Batch names its outputs after their sources", () => {
  assert.equal(
    describeStep({ kind: "command", command: "gaussian_blur", args: { id: SELECTED, radius: 2 } }),
    "gaussian_blur (radius=2)",
  );
  assert.equal(
    describeStep({ kind: "command", command: "invert", args: { id: SELECTED } }),
    "invert",
  );
  assert.equal(
    describeStep({
      kind: "command",
      command: "fill",
      args: { id: SELECTED, color: [1, 2, 3, 255] },
    }),
    "fill (color=[1,2,3,255])",
  );
  assert.equal(describeStep({ kind: "stop", message: "Check the crop" }), "Stop: Check the crop");
  assert.equal(batchOutputName("/photos/in/IMG_01.PNG"), "IMG_01.png");
  assert.equal(batchOutputName("C:\\photos\\b.png"), "b.png");
});
