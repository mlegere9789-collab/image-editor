import { test } from "node:test";
import assert from "node:assert/strict";
import {
  AUTO_CORRECTION_ALGORITHM_LABELS,
  planAutoColorCorrection,
} from "./autoColorCorrection.ts";

const targets = {
  shadows: [10, 20, 30] as [number, number, number],
  midtones: [120, 130, 140] as [number, number, number],
  highlights: [240, 250, 255] as [number, number, number],
  preserveLuminosity: false,
};

test("Enhance Per Channel Contrast runs Auto Tone with just the clip percentages", () => {
  assert.deepEqual(planAutoColorCorrection("perChannel", 3, 10, 20, targets), {
    command: "auto_tone",
    params: { id: 3, shadowClip: 10, highlightClip: 20 },
  });
});

test("Enhance Monochromatic Contrast runs Auto Contrast with just the clip percentages", () => {
  assert.deepEqual(
    planAutoColorCorrection("monochromatic", 3, 10, 20, targets),
    {
      command: "auto_contrast",
      params: { id: 3, shadowClip: 10, highlightClip: 20 },
    },
  );
});

test("Find Dark & Light Colors runs Auto Color with the dialog's own targets", () => {
  assert.deepEqual(
    planAutoColorCorrection("findDarkLight", 3, 10, 20, targets),
    {
      command: "auto_color_with",
      params: {
        id: 3,
        shadowClip: 10,
        highlightClip: 20,
        shadows: [10, 20, 30],
        midtones: [120, 130, 140],
        highlights: [240, 250, 255],
        luminosity: false,
      },
    },
  );
});

test("Find Dark & Light Colors with Snap Neutral Midtones drops the midtone target for the sampled luma instead", () => {
  const plan = planAutoColorCorrection("findDarkLight", 3, 10, 20, {
    ...targets,
    preserveLuminosity: true,
  });
  assert.deepEqual(plan, {
    command: "auto_color_with",
    params: {
      id: 3,
      shadowClip: 10,
      highlightClip: 20,
      shadows: [10, 20, 30],
      midtones: null,
      highlights: [240, 250, 255],
      luminosity: true,
    },
  });
});

test("every algorithm has Photoshop's own label", () => {
  assert.deepEqual(AUTO_CORRECTION_ALGORITHM_LABELS, {
    perChannel: "Enhance Per Channel Contrast",
    monochromatic: "Enhance Monochromatic Contrast",
    findDarkLight: "Find Dark & Light Colors",
  });
});
