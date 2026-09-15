import { test } from "node:test";
import assert from "node:assert/strict";
import { nextCloneOffset } from "./cloneStamp.ts";

test("aligned keeps the first stroke's own offset across later strokes", () => {
  // Source (100, 40); the first stroke begins at (10, 10) -> offset
  // (90, 30). A later, separate stroke starting anywhere else still
  // reuses that same offset rather than recomputing from its own start.
  const first = nextCloneOffset(null, true, [100, 40], [10, 10]);
  assert.deepEqual(first, [90, 30]);
  const second = nextCloneOffset(first, true, [100, 40], [500, 500]);
  assert.deepEqual(second, first);
});

test("not aligned recomputes the offset fresh at the start of every stroke", () => {
  // Same source and first stroke as above, so the first offset agrees --
  // but a later stroke starting somewhere else gets its own fresh offset
  // from the same fixed source point instead of inheriting the first.
  const first = nextCloneOffset(null, false, [100, 40], [10, 10]);
  assert.deepEqual(first, [90, 30]);
  const second = nextCloneOffset(null, false, [100, 40], [70, 25]);
  assert.deepEqual(second, [30, 15]);
});

test("a fractional document point rounds to the nearest whole pixel", () => {
  // 100.6 - 10.2 = 90.4 -> 90; 40.4 - 10.7 = 29.7 -> 30.
  assert.deepEqual(
    nextCloneOffset(null, true, [100.6, 40.4], [10.2, 10.7]),
    [90, 30],
  );
});
