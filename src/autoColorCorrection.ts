// The Levels and Curves dialogs' own "Auto" button, matching Photoshop's
// "Auto Color Correction Options" dialog: one Auto button, backed by a
// choice of algorithm rather than a single fixed behaviour.
//
// Photoshop offers exactly three: Enhance Per Channel Contrast (this app's
// existing Auto Tone command — the plain "Auto" default), Enhance
// Monochromatic Contrast (Auto Contrast), and Find Dark & Light Colors
// (Auto Color, with its own Snap Neutral Midtones option). All three
// commands already exist; this module is just the dispatch from "which
// algorithm is selected" to "which command, with which arguments" runs.

export type AutoCorrectionAlgorithm =
  | "perChannel"
  | "monochromatic"
  | "findDarkLight";

/** The Black/Gray/White point targets and Preserve Luminosity toggle the
 * Levels and Curves dialogs already keep, as Find Dark & Light Colors needs them. */
export interface AutoCorrectionTargets {
  shadows: [number, number, number];
  midtones: [number, number, number];
  highlights: [number, number, number];
  preserveLuminosity: boolean;
}

export interface AutoCorrectionPlan {
  command: string;
  params: Record<string, unknown>;
}

/** What running the Auto button sends to the backend for the chosen algorithm. */
export function planAutoColorCorrection(
  algorithm: AutoCorrectionAlgorithm,
  id: number,
  shadowClip: number,
  highlightClip: number,
  targets: AutoCorrectionTargets,
): AutoCorrectionPlan {
  switch (algorithm) {
    case "perChannel":
      return {
        command: "auto_tone",
        params: { id, shadowClip, highlightClip },
      };
    case "monochromatic":
      return {
        command: "auto_contrast",
        params: { id, shadowClip, highlightClip },
      };
    case "findDarkLight":
      return {
        command: "auto_color_with",
        params: {
          id,
          shadowClip,
          highlightClip,
          shadows: targets.shadows,
          midtones: targets.preserveLuminosity ? null : targets.midtones,
          highlights: targets.highlights,
          luminosity: targets.preserveLuminosity,
        },
      };
  }
}

/** The label Photoshop itself uses for each algorithm, for the dialogs' own select. */
export const AUTO_CORRECTION_ALGORITHM_LABELS: Record<
  AutoCorrectionAlgorithm,
  string
> = {
  perChannel: "Enhance Per Channel Contrast",
  monochromatic: "Enhance Monochromatic Contrast",
  findDarkLight: "Find Dark & Light Colors",
};
