import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open, save } from "@tauri-apps/plugin-dialog";

import LayerPanel from "./LayerPanel";
import type {
  BlendMode,
  BlendModeInfo,
  DocumentView,
  HistoryState,
  MoveDirection,
  SelectionShape,
  Snapshot,
  Tool,
} from "./types";

const PNG_FILTER = [{ name: "PNG image", extensions: ["png"] }];
const PROJECT_FILTER = [{ name: "Image Editor Project", extensions: ["iep"] }];

/** One row per output channel (R, G, B); each row is
 * [rCoeff, gCoeff, bCoeff, constant]. This is the no-op matrix. */
const IDENTITY_KERNEL = Array.from({ length: 25 }, (_, i) => (i === 12 ? "1" : "0"));

// The Custom kernel fields are held as strings: a controlled numeric value
// would snap the invalid intermediate "-" back to 0 and eat the sign.
function toInteger(text: string): number {
  const n = Math.trunc(Number(text));
  return Number.isFinite(n) ? n : 0;
}

type DiffuseMode = "normal" | "darkenOnly" | "lightenOnly" | "anisotropic";

const DIFFUSE_MODES: readonly (readonly [DiffuseMode, string])[] = [
  ["normal", "Normal"],
  ["darkenOnly", "Darken Only"],
  ["lightenOnly", "Lighten Only"],
  ["anisotropic", "Anisotropic"],
];

type RippleSize = "small" | "medium" | "large";

// Photoshop's Small / Medium / Large ripple sizes, as wavelengths in pixels.
const RIPPLE_SIZES: readonly (readonly [RippleSize, string, number])[] = [
  ["small", "Small", 8],
  ["medium", "Medium", 16],
  ["large", "Large", 32],
];

type ZigZagStyle = "aroundCenter" | "outFromCenter" | "pondRipples";

const ZIGZAG_STYLES: readonly (readonly [ZigZagStyle, string])[] = [
  ["aroundCenter", "Around Center"],
  ["outFromCenter", "Out From Center"],
  ["pondRipples", "Pond Ripples"],
];

const TEXT_INPUT_TYPES = new Set(["text", "number", "search", "email", "url", "password"]);

// Keyboard shortcuts must not steal Ctrl+A / Ctrl+C / Ctrl+V / Ctrl+Z from a
// field the user is typing in; sliders and colour pickers keep them.
function isTypingTarget(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLTextAreaElement ||
    (target instanceof HTMLInputElement && TEXT_INPUT_TYPES.has(target.type))
  );
}

const IDENTITY_CHANNEL_MIXER = [
  [100, 0, 0, 0],
  [0, 100, 0, 0],
  [0, 0, 100, 0],
];

/** Output values for the five fixed Curves control points at input
 * positions 0, 64, 128, 192, 255. This is the no-op curve. */
const IDENTITY_CURVE = [0, 64, 128, 192, 255];

/** Heading/label text for the Expand/Contract/Smooth/Border shared dialog. */
const MODIFY_SELECTION_LABELS: Record<
  "expand" | "contract" | "smooth" | "border",
  { heading: string; control: string }
> = {
  expand: { heading: "Expand selection", control: "Expand By (px)" },
  contract: { heading: "Contract selection", control: "Contract By (px)" },
  smooth: { heading: "Smooth selection", control: "Smooth Radius (px)" },
  border: { heading: "Border selection", control: "Border Width (px)" },
};

/** `#rrggbb` to `[r, g, b]`, each `0..=255`. */
function hexToRgb(hex: string): [number, number, number] {
  const value = Number.parseInt(hex.slice(1), 16);
  return [(value >> 16) & 0xff, (value >> 8) & 0xff, value & 0xff];
}

/** `[r, g, b]`, each `0..=255`, to `#rrggbb`. */
function rgbToHex(r: number, g: number, b: number): string {
  return `#${[r, g, b].map((c) => c.toString(16).padStart(2, "0")).join("")}`;
}

/** A pointer event's position, in document pixel coordinates. */
function toDocPoint(
  event: React.PointerEvent<HTMLImageElement>,
  doc: DocumentView,
): [number, number] {
  const rect = event.currentTarget.getBoundingClientRect();
  return [
    ((event.clientX - rect.left) / rect.width) * doc.width,
    ((event.clientY - rect.top) / rect.height) * doc.height,
  ];
}

/** A doc-pixel rectangle as a percentage-based overlay style, positioned
 * relative to the canvas image it's drawn over. */
function overlayStyle(
  bounds: { x0: number; y0: number; x1: number; y1: number },
  doc: DocumentView,
): React.CSSProperties {
  return {
    left: `${(bounds.x0 / doc.width) * 100}%`,
    top: `${(bounds.y0 / doc.height) * 100}%`,
    width: `${((bounds.x1 - bounds.x0) / doc.width) * 100}%`,
    height: `${((bounds.y1 - bounds.y0) / doc.height) * 100}%`,
  };
}

/** Border radius for a `roundedRectangle` selection outline. Expressed as
 * independent horizontal/vertical percentages (CSS's `x% / y%` border-radius
 * syntax) of the outline element's own width/height, so the displayed
 * corners track the true pixel radius even though the element itself is
 * laid out in percentages, not pixels. */
function selectionRadiusStyle(
  shape: SelectionShape,
  bounds: { x0: number; y0: number; x1: number; y1: number },
): React.CSSProperties {
  if (typeof shape !== "object") return {};
  const { radius } = shape.roundedRectangle;
  const width = bounds.x1 - bounds.x0;
  const height = bounds.y1 - bounds.y0;
  if (radius <= 0 || width <= 0 || height <= 0) return {};
  const horizontal = (radius / width) * 100;
  const vertical = (radius / height) * 100;
  return { borderRadius: `${horizontal}% / ${vertical}%`, overflow: "hidden" };
}

/** `bounds` shrunk by `width` pixels on every side, or `null` if that would
 * collapse it to zero or negative area. Mirrors `shrink_rect` in
 * src-tauri/src/document.rs, used here to draw the inner edge of a
 * Select > Modify > Border selection's outline ring. */
function shrinkBounds(
  bounds: { x0: number; y0: number; x1: number; y1: number },
  width: number,
): { x0: number; y0: number; x1: number; y1: number } | null {
  const x0 = bounds.x0 + width;
  const y0 = bounds.y0 + width;
  const x1 = bounds.x1 - width;
  const y1 = bounds.y1 - width;
  if (x0 >= x1 || y0 >= y1) return null;
  return { x0, y0, x1, y1 };
}

/** The two arbitrary drag corners of an in-progress marquee, normalized into
 * a bounds rectangle and clamped to the canvas — purely for the live
 * preview outline; the backend does its own authoritative clamping once the
 * drag ends. */
function marqueeBounds(
  start: [number, number],
  current: [number, number],
  doc: DocumentView,
): { x0: number; y0: number; x1: number; y1: number } {
  const [sx, sy] = start;
  const [cx, cy] = current;
  return {
    x0: Math.max(0, Math.min(sx, cx)),
    y0: Math.max(0, Math.min(sy, cy)),
    x1: Math.min(doc.width, Math.max(sx, cx)),
    y1: Math.min(doc.height, Math.max(sy, cy)),
  };
}

export default function App() {
  const [document, setDocument] = useState<DocumentView | null>(null);
  // `null` until the first snapshot lands. The composite's actual bytes never
  // cross IPC: this only tells the `<img>` below which `composite://`
  // generation to fetch.
  const [generation, setGeneration] = useState<number | null>(null);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [blendModes, setBlendModes] = useState<BlendModeInfo[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [dropping, setDropping] = useState(false);
  const [canUndo, setCanUndo] = useState(false);
  const [canRedo, setCanRedo] = useState(false);

  const [showNewDialog, setShowNewDialog] = useState(false);
  const [newWidth, setNewWidth] = useState(800);
  const [newHeight, setNewHeight] = useState(600);

  // Select > Modify > Expand/Contract/Smooth share one dialog: `null` means
  // closed, otherwise which of the three backend commands Apply should send.
  const [modifyMode, setModifyMode] = useState<"expand" | "contract" | "smooth" | "border" | null>(
    null,
  );
  const [modifyAmount, setModifyAmount] = useState(4);

  const [showThresholdDialog, setShowThresholdDialog] = useState(false);
  const [thresholdLevel, setThresholdLevel] = useState(128);

  const [showPosterizeDialog, setShowPosterizeDialog] = useState(false);
  const [posterizeLevels, setPosterizeLevels] = useState(4);

  const [showBrightnessContrastDialog, setShowBrightnessContrastDialog] = useState(false);
  const [brightness, setBrightness] = useState(0);
  const [contrast, setContrast] = useState(0);

  const [showHueSaturationDialog, setShowHueSaturationDialog] = useState(false);
  const [hue, setHue] = useState(0);
  const [saturation, setSaturation] = useState(0);
  const [lightness, setLightness] = useState(0);

  const [showVibranceDialog, setShowVibranceDialog] = useState(false);
  const [vibrance, setVibrance] = useState(0);
  const [vibranceSaturation, setVibranceSaturation] = useState(0);

  const [showPhotoFilterDialog, setShowPhotoFilterDialog] = useState(false);
  const [photoFilterColor, setPhotoFilterColor] = useState("#ff9933");
  const [photoFilterDensity, setPhotoFilterDensity] = useState(25);

  const [showExposureDialog, setShowExposureDialog] = useState(false);
  const [exposureStops, setExposureStops] = useState(0);
  const [exposureOffset, setExposureOffset] = useState(0);
  const [exposureGamma, setExposureGamma] = useState(100);

  const [showGradientMapDialog, setShowGradientMapDialog] = useState(false);
  const [gradientMapShadow, setGradientMapShadow] = useState("#000000");
  const [gradientMapHighlight, setGradientMapHighlight] = useState("#ffffff");

  const [showChannelMixerDialog, setShowChannelMixerDialog] = useState(false);
  const [channelMixerMatrix, setChannelMixerMatrix] = useState<number[][]>(
    IDENTITY_CHANNEL_MIXER,
  );

  const [showLevelsDialog, setShowLevelsDialog] = useState(false);
  const [levelsInputBlack, setLevelsInputBlack] = useState(0);
  const [levelsInputWhite, setLevelsInputWhite] = useState(255);
  const [levelsGamma, setLevelsGamma] = useState(100);
  const [levelsOutputBlack, setLevelsOutputBlack] = useState(0);
  const [levelsOutputWhite, setLevelsOutputWhite] = useState(255);

  const [showCurvesDialog, setShowCurvesDialog] = useState(false);
  const [curvePoints, setCurvePoints] = useState<number[]>(IDENTITY_CURVE);

  const [showColorBalanceDialog, setShowColorBalanceDialog] = useState(false);
  const [colorBalanceShadows, setColorBalanceShadows] = useState<number[]>([0, 0, 0]);
  const [colorBalanceMidtones, setColorBalanceMidtones] = useState<number[]>([0, 0, 0]);
  const [colorBalanceHighlights, setColorBalanceHighlights] = useState<number[]>([0, 0, 0]);

  const [showSolidColorFillDialog, setShowSolidColorFillDialog] = useState(false);
  const [solidColorFill, setSolidColorFill] = useState("#ffffff");

  const [showGradientFillDialog, setShowGradientFillDialog] = useState(false);
  const [gradientFillStart, setGradientFillStart] = useState("#000000");
  const [gradientFillEnd, setGradientFillEnd] = useState("#ffffff");

  const [showFillDialog, setShowFillDialog] = useState(false);
  const [fillColor, setFillColor] = useState("#ffffff");

  const [showBoxBlurDialog, setShowBoxBlurDialog] = useState(false);
  const [boxBlurRadius, setBoxBlurRadius] = useState(4);
  const [showGaussianBlurDialog, setShowGaussianBlurDialog] = useState(false);
  const [gaussianBlurRadius, setGaussianBlurRadius] = useState(2);
  const [showSurfaceBlurDialog, setShowSurfaceBlurDialog] = useState(false);
  const [surfaceBlurRadius, setSurfaceBlurRadius] = useState(5);
  const [surfaceBlurThreshold, setSurfaceBlurThreshold] = useState(15);
  const [showGlowingEdgesDialog, setShowGlowingEdgesDialog] = useState(false);
  const [glowEdgeWidth, setGlowEdgeWidth] = useState(2);
  const [glowEdgeBrightness, setGlowEdgeBrightness] = useState(6);
  const [glowSmoothness, setGlowSmoothness] = useState(5);
  const [showMosaicDialog, setShowMosaicDialog] = useState(false);
  const [mosaicCellSize, setMosaicCellSize] = useState(8);
  const [showRippleDialog, setShowRippleDialog] = useState(false);
  const [rippleAmount, setRippleAmount] = useState(100);
  const [rippleSize, setRippleSize] = useState<RippleSize>("medium");
  const [showTwirlDialog, setShowTwirlDialog] = useState(false);
  const [twirlAngle, setTwirlAngle] = useState(50);
  const [showPinchDialog, setShowPinchDialog] = useState(false);
  const [pinchAmount, setPinchAmount] = useState(50);
  const [showSpherizeDialog, setShowSpherizeDialog] = useState(false);
  const [spherizeAmount, setSpherizeAmount] = useState(50);
  const [showZigZagDialog, setShowZigZagDialog] = useState(false);
  const [zigZagAmount, setZigZagAmount] = useState(10);
  const [zigZagRidges, setZigZagRidges] = useState(5);
  const [zigZagStyle, setZigZagStyle] = useState<ZigZagStyle>("pondRipples");
  const [showPolarDialog, setShowPolarDialog] = useState(false);
  const [polarToPolar, setPolarToPolar] = useState(true);
  const [showWaveDialog, setShowWaveDialog] = useState(false);
  const [waveGenerators, setWaveGenerators] = useState(5);
  const [waveWavelengthMin, setWaveWavelengthMin] = useState(10);
  const [waveWavelengthMax, setWaveWavelengthMax] = useState(40);
  const [waveAmplitudeMin, setWaveAmplitudeMin] = useState(5);
  const [waveAmplitudeMax, setWaveAmplitudeMax] = useState(20);
  const [waveHorizontalScale, setWaveHorizontalScale] = useState(100);
  const [waveVerticalScale, setWaveVerticalScale] = useState(100);
  const [showColorHalftoneDialog, setShowColorHalftoneDialog] = useState(false);
  const [colorHalftoneRadius, setColorHalftoneRadius] = useState(8);
  const [showCrystallizeDialog, setShowCrystallizeDialog] = useState(false);
  const [crystallizeCellSize, setCrystallizeCellSize] = useState(16);
  const [showPointillizeDialog, setShowPointillizeDialog] = useState(false);
  const [pointillizeCellSize, setPointillizeCellSize] = useState(16);
  const [pointillizeBackground, setPointillizeBackground] = useState("#ffffff");
  const [showDiffuseDialog, setShowDiffuseDialog] = useState(false);
  const [diffuseMode, setDiffuseMode] = useState<DiffuseMode>("normal");

  const [showUnsharpMaskDialog, setShowUnsharpMaskDialog] = useState(false);
  const [unsharpMaskRadius, setUnsharpMaskRadius] = useState(2);
  const [unsharpMaskAmount, setUnsharpMaskAmount] = useState(100);
  const [unsharpMaskThreshold, setUnsharpMaskThreshold] = useState(4);

  const [showMotionBlurDialog, setShowMotionBlurDialog] = useState(false);
  const [motionBlurAngle, setMotionBlurAngle] = useState(0);
  const [motionBlurDistance, setMotionBlurDistance] = useState(10);

  const [showMedianDialog, setShowMedianDialog] = useState(false);
  const [medianRadius, setMedianRadius] = useState(1);

  const [showDustAndScratchesDialog, setShowDustAndScratchesDialog] = useState(false);
  const [dustRadius, setDustRadius] = useState(1);
  const [dustThreshold, setDustThreshold] = useState(0);

  const [showAddNoiseDialog, setShowAddNoiseDialog] = useState(false);
  const [noiseAmount, setNoiseAmount] = useState(10);
  const [noiseGaussian, setNoiseGaussian] = useState(false);
  const [noiseMonochromatic, setNoiseMonochromatic] = useState(false);

  const [showMaximumDialog, setShowMaximumDialog] = useState(false);
  const [maximumRadius, setMaximumRadius] = useState(1);
  const [showMinimumDialog, setShowMinimumDialog] = useState(false);
  const [minimumRadius, setMinimumRadius] = useState(1);
  const [showHighPassDialog, setShowHighPassDialog] = useState(false);
  const [highPassRadius, setHighPassRadius] = useState(3);
  const [showOffsetDialog, setShowOffsetDialog] = useState(false);
  const [showCustomDialog, setShowCustomDialog] = useState(false);
  const [customKernel, setCustomKernel] = useState<string[]>(IDENTITY_KERNEL);
  const [customScale, setCustomScale] = useState("1");
  const [customOffset, setCustomOffset] = useState("0");
  const [showEmbossDialog, setShowEmbossDialog] = useState(false);
  const [embossAngle, setEmbossAngle] = useState(135);
  const [embossHeight, setEmbossHeight] = useState(3);
  const [embossAmount, setEmbossAmount] = useState(100);
  const [showTraceContourDialog, setShowTraceContourDialog] = useState(false);
  const [traceLevel, setTraceLevel] = useState(128);
  const [traceUpper, setTraceUpper] = useState(false);
  const [offsetX, setOffsetX] = useState(0);
  const [offsetY, setOffsetY] = useState(0);

  const [tool, setTool] = useState<Tool>("brush");
  const [brushColor, setBrushColor] = useState("#ffffff");
  const [brushSize, setBrushSize] = useState(16);
  const [brushOpacity, setBrushOpacity] = useState(1);
  const [gradientEndColor, setGradientEndColor] = useState("#000000");

  // The gradient drag's live start point — a ref, not state, read directly
  // at pointerup the same way `marqueeStart` below is; the gradient itself
  // has no live preview while dragging (a deliberate scope cut, unlike the
  // marquee tools' outline).
  const gradientStart = useRef<[number, number] | null>(null);

  // A marquee drag's live start point (a ref, not state — read directly at
  // pointerup rather than through a closure that could be stale by then, the
  // same reasoning `lastPoint` below uses for brush strokes). `marqueePreview`
  // exists only to re-render the live outline as the drag moves; the actual
  // select_rectangle/select_ellipse call at drag-end recomputes its corners
  // from the ref and the pointerup event directly.
  const marqueeStart = useRef<[number, number] | null>(null);
  const [marqueePreview, setMarqueePreview] = useState<{
    start: [number, number];
    current: [number, number];
  } | null>(null);

  // Dragging the opacity slider fires many overlapping commands. Each one is
  // tagged, and only the newest response is allowed to land, so a slow render
  // can never overwrite a newer one.
  const requestId = useRef(0);

  // A stroke is a sequence of pointer-move events, not one command: each move
  // sends just the segment since the last point, so a call's own bounding box
  // (and the coverage work behind it) stays small regardless of how long the
  // drag has run. `lastPoint` is `null` between strokes.
  const lastPoint = useRef<[number, number] | null>(null);

  const runCommand = useCallback(
    async (
      command: string,
      args: Record<string, unknown> = {},
      selectAfter?: "top" | { above: number },
    ) => {
      const ticket = ++requestId.current;
      setBusy(true);
      try {
        const snapshot = await invoke<Snapshot>(command, args);
        if (ticket !== requestId.current) return;

        setError(null);
        setDocument(snapshot.document);
        setGeneration(snapshot.generation);
        setCanUndo(snapshot.canUndo);
        setCanRedo(snapshot.canRedo);

        const { layers } = snapshot.document;
        setSelectedId((current) => {
          if (selectAfter === "top") return layers[layers.length - 1]?.id ?? null;
          if (selectAfter && typeof selectAfter === "object") {
            // A layer that was just inserted directly above `above` (e.g.
            // Duplicate Layer) rather than at the very top of the stack.
            const index = layers.findIndex((layer) => layer.id === selectAfter.above);
            if (index !== -1) return layers[index + 1]?.id ?? layers[index]?.id ?? null;
          }
          // Keep the selection unless that layer is gone.
          if (current !== null && layers.some((layer) => layer.id === current)) return current;
          return layers[layers.length - 1]?.id ?? null;
        });
      } catch (err) {
        if (ticket !== requestId.current) return;
        setError(String(err));
      } finally {
        if (ticket === requestId.current) setBusy(false);
      }
    },
    [],
  );

  // Snapshots the document onto the undo stack, for a multi-step gesture
  // (a stroke, an opacity drag) to call once, at the start — the whole
  // gesture then undoes as one step rather than one step per command it
  // happens to have sent. Unlike runCommand, this doesn't touch document,
  // generation, or selection: only the undo/redo button states change.
  //
  // Returns the promise so a caller that needs the checkpoint to actually
  // land before its first edit — not just be issued first, which two
  // invoke() calls fired back to back in the same tick do not guarantee —
  // can await it. paint/erase strokes do; the opacity slider doesn't need
  // to, since its own onChange only fires later, on real pointer movement.
  const checkpoint = useCallback(() => {
    return invoke<HistoryState>("checkpoint")
      .then((history) => {
        setCanUndo(history.canUndo);
        setCanRedo(history.canRedo);
      })
      .catch(() => {
        // A failed checkpoint just costs this gesture its undo step; the
        // edit that follows still happens normally.
      });
  }, []);

  const undo = useCallback(() => void runCommand("undo"), [runCommand]);
  const redo = useCallback(() => void runCommand("redo"), [runCommand]);
  const deselect = useCallback(() => void runCommand("deselect"), [runCommand]);
  const reselect = useCallback(() => void runCommand("reselect"), [runCommand]);
  const selectAll = useCallback(() => void runCommand("select_all"), [runCommand]);
  const invertSelection = useCallback(
    () => void runCommand("invert_selection"),
    [runCommand],
  );
  const hasSelection = document?.selection != null;
  const canReselect = document?.canReselect ?? false;

  const invertColors = useCallback(() => {
    if (selectedId === null) return;
    void runCommand("invert_colors", { id: selectedId });
  }, [runCommand, selectedId]);

  const blackAndWhite = useCallback(() => {
    if (selectedId === null) return;
    void runCommand("black_and_white", { id: selectedId });
  }, [runCommand, selectedId]);

  const applyModifySelection = useCallback(async () => {
    if (modifyMode === null) return;
    if (modifyMode === "smooth") {
      await runCommand("smooth_selection", { radius: modifyAmount });
    } else if (modifyMode === "border") {
      await runCommand("border_selection", { width: modifyAmount });
    } else {
      const command = modifyMode === "expand" ? "expand_selection" : "contract_selection";
      await runCommand(command, { amount: modifyAmount });
    }
    setModifyMode(null);
  }, [runCommand, modifyMode, modifyAmount]);

  const applyThreshold = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("threshold", { id: selectedId, level: thresholdLevel });
    setShowThresholdDialog(false);
  }, [runCommand, selectedId, thresholdLevel]);

  const applyPosterize = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("posterize", { id: selectedId, levels: posterizeLevels });
    setShowPosterizeDialog(false);
  }, [runCommand, selectedId, posterizeLevels]);

  const applyBrightnessContrast = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("brightness_contrast", { id: selectedId, brightness, contrast });
    setShowBrightnessContrastDialog(false);
  }, [runCommand, selectedId, brightness, contrast]);

  const applyHueSaturation = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("hue_saturation", { id: selectedId, hue, saturation, lightness });
    setShowHueSaturationDialog(false);
  }, [runCommand, selectedId, hue, saturation, lightness]);

  const applyVibrance = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("vibrance", {
      id: selectedId,
      vibrance,
      saturation: vibranceSaturation,
    });
    setShowVibranceDialog(false);
  }, [runCommand, selectedId, vibrance, vibranceSaturation]);

  const applyPhotoFilter = useCallback(async () => {
    if (selectedId === null) return;
    const [r, g, b] = hexToRgb(photoFilterColor);
    await runCommand("photo_filter", {
      id: selectedId,
      color: [r, g, b],
      density: photoFilterDensity,
    });
    setShowPhotoFilterDialog(false);
  }, [runCommand, selectedId, photoFilterColor, photoFilterDensity]);

  const applyExposure = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("exposure", {
      id: selectedId,
      exposure: exposureStops,
      offset: exposureOffset,
      gamma: exposureGamma,
    });
    setShowExposureDialog(false);
  }, [runCommand, selectedId, exposureStops, exposureOffset, exposureGamma]);

  const applyGradientMap = useCallback(async () => {
    if (selectedId === null) return;
    const shadow = hexToRgb(gradientMapShadow);
    const highlight = hexToRgb(gradientMapHighlight);
    await runCommand("gradient_map", {
      id: selectedId,
      shadowColor: shadow,
      highlightColor: highlight,
    });
    setShowGradientMapDialog(false);
  }, [runCommand, selectedId, gradientMapShadow, gradientMapHighlight]);

  const setChannelMixerCell = useCallback((row: number, col: number, value: number) => {
    setChannelMixerMatrix((matrix) =>
      matrix.map((r, ri) => (ri === row ? r.map((c, ci) => (ci === col ? value : c)) : r)),
    );
  }, []);

  const applyChannelMixer = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("channel_mixer", { id: selectedId, matrix: channelMixerMatrix });
    setShowChannelMixerDialog(false);
  }, [runCommand, selectedId, channelMixerMatrix]);

  const applyLevels = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("levels", {
      id: selectedId,
      inputBlack: levelsInputBlack,
      inputWhite: levelsInputWhite,
      gamma: levelsGamma,
      outputBlack: levelsOutputBlack,
      outputWhite: levelsOutputWhite,
    });
    setShowLevelsDialog(false);
  }, [
    runCommand,
    selectedId,
    levelsInputBlack,
    levelsInputWhite,
    levelsGamma,
    levelsOutputBlack,
    levelsOutputWhite,
  ]);

  const setCurvePoint = useCallback((index: number, value: number) => {
    setCurvePoints((points) => points.map((p, i) => (i === index ? value : p)));
  }, []);

  const applyCurves = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("curves", { id: selectedId, points: curvePoints });
    setShowCurvesDialog(false);
  }, [runCommand, selectedId, curvePoints]);

  const setColorBalanceValue = useCallback(
    (setter: (updater: (values: number[]) => number[]) => void, index: number, value: number) => {
      setter((values) => values.map((v, i) => (i === index ? value : v)));
    },
    [],
  );

  const applyColorBalance = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("color_balance", {
      id: selectedId,
      shadows: colorBalanceShadows,
      midtones: colorBalanceMidtones,
      highlights: colorBalanceHighlights,
    });
    setShowColorBalanceDialog(false);
  }, [runCommand, selectedId, colorBalanceShadows, colorBalanceMidtones, colorBalanceHighlights]);

  const applySolidColorFill = useCallback(async () => {
    const [r, g, b] = hexToRgb(solidColorFill);
    await runCommand("add_solid_color_layer", { color: [r, g, b, 255] });
    setShowSolidColorFillDialog(false);
  }, [runCommand, solidColorFill]);

  const applyGradientFill = useCallback(async () => {
    const [r1, g1, b1] = hexToRgb(gradientFillStart);
    const [r2, g2, b2] = hexToRgb(gradientFillEnd);
    await runCommand("add_gradient_layer", {
      startColor: [r1, g1, b1, 255],
      endColor: [r2, g2, b2, 255],
    });
    setShowGradientFillDialog(false);
  }, [runCommand, gradientFillStart, gradientFillEnd]);

  // Whether the backend clipboard has something in it. Set once a Copy or
  // Cut succeeds and never cleared afterward — the backend clipboard itself
  // outlives undo/redo and even opening a different document (see
  // `AppState::clipboard` in `lib.rs`), so this mirrors that: it only ever
  // goes from false to true for the life of the app.
  const [canPaste, setCanPaste] = useState(false);

  const copySelection = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("copy", { id: selectedId });
    setCanPaste(true);
  }, [runCommand, selectedId]);

  const cutSelection = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("cut", { id: selectedId });
    setCanPaste(true);
  }, [runCommand, selectedId]);

  const pasteClipboard = useCallback(async () => {
    await runCommand("paste", {}, "top");
  }, [runCommand]);

  const deleteSelection = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("delete_selection", { id: selectedId });
  }, [runCommand, selectedId]);

  const applyFill = useCallback(async () => {
    if (selectedId === null) return;
    const [r, g, b] = hexToRgb(fillColor);
    await runCommand("fill_selection", { id: selectedId, color: [r, g, b, 255] });
    setShowFillDialog(false);
  }, [runCommand, selectedId, fillColor]);

  const applyBoxBlur = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("box_blur", { id: selectedId, radius: boxBlurRadius });
    setShowBoxBlurDialog(false);
  }, [runCommand, selectedId, boxBlurRadius]);

  const applyGaussianBlur = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("gaussian_blur", { id: selectedId, radius: gaussianBlurRadius });
    setShowGaussianBlurDialog(false);
  }, [runCommand, selectedId, gaussianBlurRadius]);

  const applySurfaceBlur = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("surface_blur", {
      id: selectedId,
      radius: surfaceBlurRadius,
      threshold: surfaceBlurThreshold,
    });
    setShowSurfaceBlurDialog(false);
  }, [runCommand, selectedId, surfaceBlurRadius, surfaceBlurThreshold]);

  const applyGlowingEdges = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("glowing_edges", {
      id: selectedId,
      edgeWidth: glowEdgeWidth,
      edgeBrightness: glowEdgeBrightness,
      smoothness: glowSmoothness,
    });
    setShowGlowingEdgesDialog(false);
  }, [runCommand, selectedId, glowEdgeWidth, glowEdgeBrightness, glowSmoothness]);

  const applyMosaic = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("mosaic", { id: selectedId, cellSize: mosaicCellSize });
    setShowMosaicDialog(false);
  }, [runCommand, selectedId, mosaicCellSize]);

  const applyRipple = useCallback(async () => {
    if (selectedId === null) return;
    const wavelength = RIPPLE_SIZES.find(([value]) => value === rippleSize)?.[2] ?? 16;
    // 100 % on the Small size is a one-pixel ripple; the amplitude scales
    // with the wavelength so each size keeps Photoshop's proportions.
    const amplitude = (rippleAmount / 100) * (wavelength / 8);
    await runCommand("ripple", { id: selectedId, amplitude, wavelength });
    setShowRippleDialog(false);
  }, [runCommand, selectedId, rippleAmount, rippleSize]);

  const applyTwirl = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("twirl", { id: selectedId, angle: twirlAngle });
    setShowTwirlDialog(false);
  }, [runCommand, selectedId, twirlAngle]);

  const applyPinch = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("pinch", { id: selectedId, amount: pinchAmount });
    setShowPinchDialog(false);
  }, [runCommand, selectedId, pinchAmount]);

  const applySpherize = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("spherize", { id: selectedId, amount: spherizeAmount });
    setShowSpherizeDialog(false);
  }, [runCommand, selectedId, spherizeAmount]);

  const applyZigZag = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("zig_zag", {
      id: selectedId,
      amount: zigZagAmount,
      ridges: zigZagRidges,
      style: zigZagStyle,
    });
    setShowZigZagDialog(false);
  }, [runCommand, selectedId, zigZagAmount, zigZagRidges, zigZagStyle]);

  const applyPolarCoordinates = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("polar_coordinates", { id: selectedId, toPolar: polarToPolar });
    setShowPolarDialog(false);
  }, [runCommand, selectedId, polarToPolar]);

  const applyWave = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, as with Add Noise/Crystallize.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("wave", {
      id: selectedId,
      generators: waveGenerators,
      wavelengthMin: waveWavelengthMin,
      wavelengthMax: waveWavelengthMax,
      amplitudeMin: waveAmplitudeMin,
      amplitudeMax: waveAmplitudeMax,
      horizontalScale: waveHorizontalScale,
      verticalScale: waveVerticalScale,
      seed,
    });
    setShowWaveDialog(false);
  }, [
    runCommand,
    selectedId,
    waveGenerators,
    waveWavelengthMin,
    waveWavelengthMax,
    waveAmplitudeMin,
    waveAmplitudeMax,
    waveHorizontalScale,
    waveVerticalScale,
  ]);

  const applyColorHalftone = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("color_halftone", { id: selectedId, maxRadius: colorHalftoneRadius });
    setShowColorHalftoneDialog(false);
  }, [runCommand, selectedId, colorHalftoneRadius]);

  const applyCrystallize = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, as with Add Noise: the backend is deterministic per seed.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("crystallize", { id: selectedId, cellSize: crystallizeCellSize, seed });
    setShowCrystallizeDialog(false);
  }, [runCommand, selectedId, crystallizeCellSize]);

  const applyPointillize = useCallback(async () => {
    if (selectedId === null) return;
    const [r, g, b] = hexToRgb(pointillizeBackground);
    // A fresh seed per apply, as with Add Noise/Crystallize.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("pointillize", {
      id: selectedId,
      cellSize: pointillizeCellSize,
      background: [r, g, b, 255],
      seed,
    });
    setShowPointillizeDialog(false);
  }, [runCommand, selectedId, pointillizeCellSize, pointillizeBackground]);

  const applyDiffuse = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, as with Add Noise: the backend is deterministic per seed.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("diffuse", { id: selectedId, mode: diffuseMode, seed });
    setShowDiffuseDialog(false);
  }, [runCommand, selectedId, diffuseMode]);

  const applyUnsharpMask = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("unsharp_mask", {
      id: selectedId,
      radius: unsharpMaskRadius,
      amount: unsharpMaskAmount / 100,
      threshold: unsharpMaskThreshold,
    });
    setShowUnsharpMaskDialog(false);
  }, [runCommand, selectedId, unsharpMaskRadius, unsharpMaskAmount, unsharpMaskThreshold]);

  const applyMotionBlur = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("motion_blur", {
      id: selectedId,
      angle: motionBlurAngle,
      distance: motionBlurDistance,
    });
    setShowMotionBlurDialog(false);
  }, [runCommand, selectedId, motionBlurAngle, motionBlurDistance]);

  const applyMedian = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("median", { id: selectedId, radius: medianRadius });
    setShowMedianDialog(false);
  }, [runCommand, selectedId, medianRadius]);

  const applyDustAndScratches = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("dust_and_scratches", {
      id: selectedId,
      radius: dustRadius,
      threshold: dustThreshold,
    });
    setShowDustAndScratchesDialog(false);
  }, [runCommand, selectedId, dustRadius, dustThreshold]);

  const applyAddNoise = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, so re-applying gives different grain — the
    // backend itself is deterministic per seed (that's what its tests rely on).
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("add_noise", {
      id: selectedId,
      amount: noiseAmount / 100,
      gaussian: noiseGaussian,
      monochromatic: noiseMonochromatic,
      seed,
    });
    setShowAddNoiseDialog(false);
  }, [runCommand, selectedId, noiseAmount, noiseGaussian, noiseMonochromatic]);

  const applyMaximum = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("maximum", { id: selectedId, radius: maximumRadius });
    setShowMaximumDialog(false);
  }, [runCommand, selectedId, maximumRadius]);

  const applyMinimum = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("minimum", { id: selectedId, radius: minimumRadius });
    setShowMinimumDialog(false);
  }, [runCommand, selectedId, minimumRadius]);

  const applyHighPass = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("high_pass", { id: selectedId, radius: highPassRadius });
    setShowHighPassDialog(false);
  }, [runCommand, selectedId, highPassRadius]);

  const applyOffset = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("offset", { id: selectedId, dx: offsetX, dy: offsetY });
    setShowOffsetDialog(false);
  }, [runCommand, selectedId, offsetX, offsetY]);

  const applyCustom = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("custom", {
      id: selectedId,
      kernel: customKernel.map(toInteger),
      scale: toInteger(customScale),
      offset: toInteger(customOffset),
    });
    setShowCustomDialog(false);
  }, [runCommand, selectedId, customKernel, customScale, customOffset]);

  const applyEmboss = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("emboss", {
      id: selectedId,
      angle: embossAngle,
      height: embossHeight,
      amount: embossAmount,
    });
    setShowEmbossDialog(false);
  }, [runCommand, selectedId, embossAngle, embossHeight, embossAmount]);

  const applyTraceContour = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("trace_contour", { id: selectedId, level: traceLevel, upper: traceUpper });
    setShowTraceContourDialog(false);
  }, [runCommand, selectedId, traceLevel, traceUpper]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return;
      if (isTypingTarget(event.target)) return;
      const key = event.key.toLowerCase();
      if (key === "z" && !event.shiftKey) {
        event.preventDefault();
        if (canUndo && !busy) undo();
      } else if ((key === "z" && event.shiftKey) || key === "y") {
        event.preventDefault();
        if (canRedo && !busy) redo();
      } else if (key === "d" && !event.shiftKey) {
        event.preventDefault();
        if (hasSelection && !busy) deselect();
      } else if (key === "d" && event.shiftKey) {
        event.preventDefault();
        if (canReselect && !busy) reselect();
      } else if (key === "a") {
        event.preventDefault();
        if (document !== null && !busy) selectAll();
      } else if (key === "i" && event.shiftKey) {
        event.preventDefault();
        if (hasSelection && !busy) invertSelection();
      } else if (key === "c") {
        event.preventDefault();
        if (selectedId !== null && !busy) void copySelection();
      } else if (key === "x") {
        event.preventDefault();
        if (selectedId !== null && !busy) void cutSelection();
      } else if (key === "v") {
        event.preventDefault();
        if (document !== null && canPaste && !busy) void pasteClipboard();
      } else if (key === "j" && !event.shiftKey) {
        event.preventDefault();
        if (selectedId !== null && !busy) {
          void runCommand("new_layer_via_copy", { id: selectedId }, "top");
        }
      } else if (key === "j" && event.shiftKey) {
        event.preventDefault();
        if (selectedId !== null && !busy) {
          void runCommand("new_layer_via_cut", { id: selectedId }, "top");
        }
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [
    canUndo,
    canRedo,
    busy,
    undo,
    redo,
    hasSelection,
    deselect,
    canReselect,
    reselect,
    document,
    selectAll,
    invertSelection,
    selectedId,
    copySelection,
    cutSelection,
    canPaste,
    pasteClipboard,
    runCommand,
  ]);

  useEffect(() => {
    invoke<BlendModeInfo[]>("blend_modes").then(setBlendModes).catch(() => {
      // A failure here only costs the picker its labels; the canvas still works.
      setBlendModes([]);
    });
  }, []);

  const openDocument = useCallback(async () => {
    const selected = await open({ multiple: false, directory: false, filters: PNG_FILTER });
    if (typeof selected === "string") await runCommand("open_document", { path: selected }, "top");
  }, [runCommand]);

  const addLayer = useCallback(async () => {
    const selected = await open({ multiple: false, directory: false, filters: PNG_FILTER });
    if (typeof selected === "string") await runCommand("add_layer", { path: selected }, "top");
  }, [runCommand]);

  // Unlike runCommand, exporting reads the open document but never mutates
  // it — there is no new Snapshot to apply, only success or an error to show.
  const exportDocument = useCallback(async () => {
    const destination = await save({ filters: PNG_FILTER, defaultPath: "untitled.png" });
    if (typeof destination !== "string") return;
    setBusy(true);
    try {
      await invoke("export_png", { path: destination });
      setError(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, []);

  // Unlike Export PNG…, this writes the full editable layer stack (order,
  // visibility, opacity, blend mode, and each layer's own pixels) to a
  // project file, not just the flattened composite — the counterpart to
  // openProject below. Reads the open document but never mutates it.
  const saveProject = useCallback(async () => {
    const destination = await save({ filters: PROJECT_FILTER, defaultPath: "untitled.iep" });
    if (typeof destination !== "string") return;
    setBusy(true);
    try {
      await invoke("save_project", { path: destination });
      setError(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, []);

  const openProject = useCallback(async () => {
    const selected = await open({ multiple: false, directory: false, filters: PROJECT_FILTER });
    if (typeof selected === "string") await runCommand("open_project", { path: selected }, "top");
  }, [runCommand]);

  const createNewDocument = useCallback(async () => {
    await runCommand("new_document", { width: newWidth, height: newHeight }, "top");
    setShowNewDialog(false);
  }, [runCommand, newWidth, newHeight]);

  // A drop opens the file when nothing is open, and stacks it as a layer when
  // something is.
  const hasDocument = document !== null;
  const hasDocumentRef = useRef(hasDocument);
  hasDocumentRef.current = hasDocument;

  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "over") {
        setDropping(true);
        return;
      }
      setDropping(false);
      if (event.payload.type !== "drop") return;
      const [first] = event.payload.paths;
      if (!first) return;
      void runCommand(
        hasDocumentRef.current ? "add_layer" : "open_document",
        { path: first },
        "top",
      );
    });
    return () => {
      void unlisten.then((off) => off());
    };
  }, [runCommand]);

  // Sends the segment `points` (1 or 2 document-space coordinates) to the
  // active tool's command. Not gated on `busy`, for the same reason the
  // opacity slider isn't: each pointer move is its own command, and stale
  // responses are already discarded by `runCommand`'s ticket.
  const applyStroke = useCallback(
    (points: [number, number][]) => {
      if (selectedId === null) return;
      if (tool === "eraser") {
        void runCommand("erase_stroke", { id: selectedId, points, radius: brushSize });
      } else {
        const [r, g, b] = hexToRgb(brushColor);
        const alpha = Math.round(brushOpacity * 255);
        void runCommand("paint_stroke", {
          id: selectedId,
          points,
          radius: brushSize,
          color: [r, g, b, alpha],
        });
      }
    },
    [runCommand, selectedId, tool, brushColor, brushOpacity, brushSize],
  );

  const canPaint = document !== null && selectedId !== null;
  const isMarqueeTool = tool === "selectRect" || tool === "selectEllipse";
  const isLineSelect = tool === "selectRow" || tool === "selectColumn";
  const isEyedropper = tool === "eyedropper";
  const isPaintBucket = tool === "paintBucket";
  const isGradient = tool === "gradient";

  const selectLineAt = useCallback(
    (event: React.PointerEvent<HTMLImageElement>) => {
      if (!document) return;
      const [x, y] = toDocPoint(event, document);
      const bounds =
        tool === "selectRow"
          ? { x0: 0, y0: Math.floor(y), x1: document.width, y1: Math.floor(y) + 1 }
          : { x0: Math.floor(x), y0: 0, x1: Math.floor(x) + 1, y1: document.height };
      void runCommand("select_rectangle", bounds);
    },
    [document, tool, runCommand],
  );

  const sampleColorAt = useCallback(
    (event: React.PointerEvent<HTMLImageElement>) => {
      if (!document) return;
      const [x, y] = toDocPoint(event, document);
      void invoke<[number, number, number, number]>("sample_color", {
        x: Math.floor(x),
        y: Math.floor(y),
      })
        .then(([r, g, b]) => setBrushColor(rgbToHex(r, g, b)))
        .catch((err) => setError(String(err)));
    },
    [document],
  );

  // Photoshop exposes Tolerance as its own slider; this build fixes it at a
  // reasonable middle value rather than adding a second numeric control
  // next to Flow — a deliberate scope cut, not an oversight.
  const PAINT_BUCKET_TOLERANCE = 32;

  const fillAt = useCallback(
    (event: React.PointerEvent<HTMLImageElement>) => {
      if (!document || selectedId === null) return;
      const [x, y] = toDocPoint(event, document);
      const [r, g, b] = hexToRgb(brushColor);
      const alpha = Math.round(brushOpacity * 255);
      void runCommand("flood_fill", {
        id: selectedId,
        x: Math.floor(x),
        y: Math.floor(y),
        color: [r, g, b, alpha],
        tolerance: PAINT_BUCKET_TOLERANCE,
      });
    },
    [document, selectedId, brushColor, brushOpacity, runCommand],
  );

  const handlePointerDown = useCallback(
    (event: React.PointerEvent<HTMLImageElement>) => {
      if (!document) return;
      if (isEyedropper) {
        sampleColorAt(event);
        return;
      }
      if (isPaintBucket) {
        if (canPaint) fillAt(event);
        return;
      }
      if (isLineSelect) {
        selectLineAt(event);
        return;
      }
      if (isGradient) {
        if (!canPaint) return;
        event.currentTarget.setPointerCapture(event.pointerId);
        gradientStart.current = toDocPoint(event, document);
        return;
      }
      if (isMarqueeTool) {
        event.currentTarget.setPointerCapture(event.pointerId);
        const point = toDocPoint(event, document);
        marqueeStart.current = point;
        setMarqueePreview({ start: point, current: point });
        return;
      }
      if (!canPaint) return;
      event.currentTarget.setPointerCapture(event.pointerId);
      const point = toDocPoint(event, document);
      lastPoint.current = point;
      // Two invoke() calls fired back to back in the same tick race for the
      // document lock on the Rust side with no guaranteed order — awaiting
      // the checkpoint's own promise is what actually guarantees it lands
      // before the stroke's first segment does.
      void checkpoint().then(() => applyStroke([point]));
    },
    [
      document,
      isEyedropper,
      sampleColorAt,
      isPaintBucket,
      fillAt,
      isLineSelect,
      selectLineAt,
      isGradient,
      isMarqueeTool,
      canPaint,
      checkpoint,
      applyStroke,
    ],
  );

  const handlePointerMove = useCallback(
    (event: React.PointerEvent<HTMLImageElement>) => {
      if (!document) return;
      if (isMarqueeTool) {
        if (marqueeStart.current === null) return;
        setMarqueePreview({ start: marqueeStart.current, current: toDocPoint(event, document) });
        return;
      }
      if (lastPoint.current === null) return;
      const point = toDocPoint(event, document);
      const previous = lastPoint.current;
      lastPoint.current = point;
      applyStroke([previous, point]);
    },
    [document, isMarqueeTool, applyStroke],
  );

  const endStroke = useCallback(
    (event: React.PointerEvent<HTMLImageElement>) => {
      if (event.currentTarget.hasPointerCapture(event.pointerId)) {
        event.currentTarget.releasePointerCapture(event.pointerId);
      }
      if (isMarqueeTool) {
        const start = marqueeStart.current;
        marqueeStart.current = null;
        setMarqueePreview(null);
        if (start && document) {
          const [x0, y0] = start;
          const [x1, y1] = toDocPoint(event, document);
          // A click with no drag has no area to select — silently a no-op,
          // rather than round-tripping to the backend just to show its
          // "must cover at least one pixel" error for an everyday click.
          if (x0 !== x1 || y0 !== y1) {
            const command = tool === "selectRect" ? "select_rectangle" : "select_ellipse";
            void runCommand(command, { x0, y0, x1, y1 });
          }
        }
        return;
      }
      if (isGradient) {
        const start = gradientStart.current;
        gradientStart.current = null;
        // A click with no drag has no direction — silently a no-op, the
        // same reasoning the marquee tools use for a zero-area selection.
        if (start && document && selectedId !== null) {
          const [x0, y0] = start;
          const [x1, y1] = toDocPoint(event, document);
          if (x0 !== x1 || y0 !== y1) {
            const [r, g, b] = hexToRgb(brushColor);
            const alpha = Math.round(brushOpacity * 255);
            const [er, eg, eb] = hexToRgb(gradientEndColor);
            void runCommand("gradient_fill", {
              id: selectedId,
              x0,
              y0,
              x1,
              y1,
              startColor: [r, g, b, alpha],
              endColor: [er, eg, eb, alpha],
            });
          }
        }
        return;
      }
      lastPoint.current = null;
    },
    [
      isMarqueeTool,
      isGradient,
      document,
      tool,
      runCommand,
      selectedId,
      brushColor,
      brushOpacity,
      gradientEndColor,
    ],
  );

  const layers = document?.layers ?? [];
  const compositeSrc = generation !== null ? `composite://composite.png?g=${generation}` : null;

  return (
    <div className={`app${dropping ? " app--dropping" : ""}`}>
      <header className="toolbar">
        <h1 className="toolbar__title">Image Editor</h1>
        <button
          className="button"
          onClick={() => setShowNewDialog(true)}
          disabled={busy}
        >
          New…
        </button>
        <button className="button" onClick={openDocument} disabled={busy}>
          Open PNG…
        </button>
        <button className="button button--quiet" onClick={addLayer} disabled={busy || !hasDocument}>
          Add layer…
        </button>
        <button
          className="button button--quiet"
          onClick={() => setShowSolidColorFillDialog(true)}
          disabled={busy || !hasDocument}
          title="Layer > New Fill Layer > Solid Color"
        >
          Solid Color…
        </button>
        <button
          className="button button--quiet"
          onClick={() => setShowGradientFillDialog(true)}
          disabled={busy || !hasDocument}
          title="Layer > New Fill Layer > Gradient"
        >
          Gradient Fill…
        </button>
        <button
          className="button button--quiet"
          onClick={exportDocument}
          disabled={busy || !hasDocument}
        >
          Export PNG…
        </button>
        <button className="button button--quiet" onClick={openProject} disabled={busy}>
          Open Project…
        </button>
        <button
          className="button button--quiet"
          onClick={saveProject}
          disabled={busy || !hasDocument}
        >
          Save Project…
        </button>

        <div className="tools" role="group" aria-label="Undo history">
          <button
            className="button button--quiet"
            onClick={undo}
            disabled={busy || !canUndo}
            title="Undo (Ctrl/Cmd+Z)"
          >
            Undo
          </button>
          <button
            className="button button--quiet"
            onClick={redo}
            disabled={busy || !canRedo}
            title="Redo (Ctrl/Cmd+Shift+Z)"
          >
            Redo
          </button>
        </div>

        <div className="tools" role="group" aria-label="Clipboard">
          <button
            className="button button--quiet"
            onClick={copySelection}
            disabled={busy || selectedId === null}
            title="Edit > Copy"
          >
            Copy
          </button>
          <button
            className="button button--quiet"
            onClick={cutSelection}
            disabled={busy || selectedId === null}
            title="Edit > Cut"
          >
            Cut
          </button>
          <button
            className="button button--quiet"
            onClick={pasteClipboard}
            disabled={busy || !hasDocument || !canPaste}
            title="Edit > Paste (always pastes back at its original position — see Edit > Paste Special > Paste in Place)"
          >
            Paste
          </button>
          <button
            className="button button--quiet"
            onClick={deleteSelection}
            disabled={busy || selectedId === null}
            title="Edit > Delete"
          >
            Delete
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowFillDialog(true)}
            disabled={busy || selectedId === null}
            title="Edit > Fill"
          >
            Fill…
          </button>
          <button
            className="button button--quiet"
            onClick={() =>
              selectedId !== null &&
              void runCommand("new_layer_via_copy", { id: selectedId }, "top")
            }
            disabled={busy || selectedId === null}
            title="Layer > New > Layer via Copy (Ctrl/Cmd+J) — copies the selection onto a new layer without touching the clipboard"
          >
            Layer via Copy
          </button>
          <button
            className="button button--quiet"
            onClick={() =>
              selectedId !== null &&
              void runCommand("new_layer_via_cut", { id: selectedId }, "top")
            }
            disabled={busy || selectedId === null}
            title="Layer > New > Layer via Cut (Ctrl/Cmd+Shift+J) — moves the selection onto a new layer without touching the clipboard"
          >
            Layer via Cut
          </button>
        </div>

        <div className="tools" role="group" aria-label="Image rotation">
          <button
            className="button button--quiet"
            onClick={() => void runCommand("rotate_document_90", { clockwise: true })}
            disabled={busy || !hasDocument}
            title="Image > Image Rotation > 90° Clockwise"
          >
            Rotate 90° CW
          </button>
          <button
            className="button button--quiet"
            onClick={() => void runCommand("rotate_document_90", { clockwise: false })}
            disabled={busy || !hasDocument}
            title="Image > Image Rotation > 90° Counter Clockwise"
          >
            Rotate 90° CCW
          </button>
        </div>

        <div className="tools" role="group" aria-label="Selection tool">
          <button
            className={`button button--quiet${tool === "selectRect" ? " button--active" : ""}`}
            disabled={!hasDocument}
            aria-pressed={tool === "selectRect"}
            onClick={() => setTool("selectRect")}
          >
            Rect Select
          </button>
          <button
            className={`button button--quiet${tool === "selectEllipse" ? " button--active" : ""}`}
            disabled={!hasDocument}
            aria-pressed={tool === "selectEllipse"}
            onClick={() => setTool("selectEllipse")}
          >
            Ellipse Select
          </button>
          <button
            className={`button button--quiet${tool === "selectRow" ? " button--active" : ""}`}
            disabled={!hasDocument}
            aria-pressed={tool === "selectRow"}
            onClick={() => setTool("selectRow")}
            title="Single Row Marquee: selects one full-width, 1px-tall row"
          >
            Single Row
          </button>
          <button
            className={`button button--quiet${tool === "selectColumn" ? " button--active" : ""}`}
            disabled={!hasDocument}
            aria-pressed={tool === "selectColumn"}
            onClick={() => setTool("selectColumn")}
            title="Single Column Marquee: selects one full-height, 1px-wide column"
          >
            Single Column
          </button>
          <button
            className="button button--quiet"
            onClick={selectAll}
            disabled={busy || !hasDocument}
            title="Select All (Ctrl/Cmd+A)"
          >
            Select All
          </button>
          <button
            className="button button--quiet"
            onClick={invertSelection}
            disabled={busy || !hasSelection}
            title="Invert Selection (Ctrl/Cmd+Shift+I)"
          >
            Invert
          </button>
          <button
            className="button button--quiet"
            onClick={() => setModifyMode("expand")}
            disabled={busy || !hasSelection}
            title="Select > Modify > Expand"
          >
            Expand…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setModifyMode("contract")}
            disabled={busy || !hasSelection}
            title="Select > Modify > Contract"
          >
            Contract…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setModifyMode("smooth")}
            disabled={busy || !hasSelection}
            title="Select > Modify > Smooth"
          >
            Smooth…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setModifyMode("border")}
            disabled={busy || !hasSelection}
            title="Select > Modify > Border"
          >
            Border…
          </button>
          <button
            className="button button--quiet"
            onClick={deselect}
            disabled={busy || !hasSelection}
            title="Deselect (Ctrl/Cmd+D)"
          >
            Deselect
          </button>
          <button
            className="button button--quiet"
            onClick={reselect}
            disabled={busy || !canReselect}
            title="Reselect (Ctrl/Cmd+Shift+D)"
          >
            Reselect
          </button>
        </div>

        <div className="tools" role="group" aria-label="Paint tool">
          <button
            className={`button button--quiet${tool === "brush" ? " button--active" : ""}`}
            disabled={!canPaint}
            aria-pressed={tool === "brush"}
            onClick={() => setTool("brush")}
          >
            Brush
          </button>
          <button
            className={`button button--quiet${tool === "eraser" ? " button--active" : ""}`}
            disabled={!canPaint}
            aria-pressed={tool === "eraser"}
            onClick={() => setTool("eraser")}
          >
            Eraser
          </button>
          <button
            className={`button button--quiet${tool === "eyedropper" ? " button--active" : ""}`}
            disabled={!hasDocument}
            aria-pressed={tool === "eyedropper"}
            onClick={() => setTool("eyedropper")}
            title="Eyedropper: click the canvas to pick up its color"
          >
            Eyedropper
          </button>
          <button
            className={`button button--quiet${tool === "paintBucket" ? " button--active" : ""}`}
            disabled={!canPaint}
            aria-pressed={tool === "paintBucket"}
            onClick={() => setTool("paintBucket")}
            title="Paint Bucket: click to fill the connected region under the pointer"
          >
            Paint Bucket
          </button>
          <button
            className={`button button--quiet${tool === "gradient" ? " button--active" : ""}`}
            disabled={!canPaint}
            aria-pressed={tool === "gradient"}
            onClick={() => setTool("gradient")}
            title="Gradient: drag to blend from color to end color along that line"
          >
            Gradient
          </button>
          <button
            className="button button--quiet"
            onClick={invertColors}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Invert"
          >
            Invert Colors
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowThresholdDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Threshold"
          >
            Threshold…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPosterizeDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Posterize"
          >
            Posterize…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowBrightnessContrastDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Brightness/Contrast"
          >
            Brightness/Contrast…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowHueSaturationDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Hue/Saturation"
          >
            Hue/Saturation…
          </button>
          <button
            className="button button--quiet"
            onClick={blackAndWhite}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Black & White"
          >
            Black &amp; White
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowVibranceDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Vibrance"
          >
            Vibrance…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPhotoFilterDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Photo Filter"
          >
            Photo Filter…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowExposureDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Exposure"
          >
            Exposure…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowGradientMapDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Gradient Map"
          >
            Gradient Map…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowChannelMixerDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Channel Mixer"
          >
            Channel Mixer…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowLevelsDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Levels"
          >
            Levels…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowCurvesDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Curves"
          >
            Curves…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowColorBalanceDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Color Balance"
          >
            Color Balance…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowBoxBlurDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Blur > Box Blur"
          >
            Box Blur…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowGaussianBlurDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Blur > Gaussian Blur"
          >
            Gaussian Blur…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowSurfaceBlurDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Blur > Surface Blur"
          >
            Surface Blur…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowUnsharpMaskDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Sharpen > Unsharp Mask"
          >
            Unsharp Mask…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowMotionBlurDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Blur > Motion Blur"
          >
            Motion Blur…
          </button>
          <button
            className="button button--quiet"
            onClick={() => selectedId !== null && void runCommand("blur", { id: selectedId })}
            disabled={busy || !canPaint}
            title="Filter > Blur > Blur (one-click, radius 1)"
          >
            Blur
          </button>
          <button
            className="button button--quiet"
            onClick={() =>
              selectedId !== null && void runCommand("blur_more", { id: selectedId })
            }
            disabled={busy || !canPaint}
            title="Filter > Blur > Blur More (one-click, radius 3)"
          >
            Blur More
          </button>
          <button
            className="button button--quiet"
            onClick={() => selectedId !== null && void runCommand("sharpen", { id: selectedId })}
            disabled={busy || !canPaint}
            title="Filter > Sharpen > Sharpen (one-click, 50%)"
          >
            Sharpen
          </button>
          <button
            className="button button--quiet"
            onClick={() =>
              selectedId !== null && void runCommand("sharpen_more", { id: selectedId })
            }
            disabled={busy || !canPaint}
            title="Filter > Sharpen > Sharpen More (one-click, 100%)"
          >
            Sharpen More
          </button>
          <button
            className="button button--quiet"
            onClick={() =>
              selectedId !== null && void runCommand("sharpen_edges", { id: selectedId })
            }
            disabled={busy || !canPaint}
            title="Filter > Sharpen > Sharpen Edges (one-click, 100% gated behind an edge threshold of 20)"
          >
            Sharpen Edges
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowMedianDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Noise > Median"
          >
            Median…
          </button>
          <button
            className="button button--quiet"
            onClick={() =>
              selectedId !== null && void runCommand("despeckle", { id: selectedId })
            }
            disabled={busy || !canPaint}
            title="Filter > Noise > Despeckle (one-click, 3x3 median)"
          >
            Despeckle
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowDustAndScratchesDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Noise > Dust & Scratches"
          >
            Dust &amp; Scratches…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowAddNoiseDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Noise > Add Noise"
          >
            Add Noise…
          </button>
          <button
            className="button button--quiet"
            onClick={() =>
              selectedId !== null &&
              void runCommand("equalize", { id: selectedId, entireImage: false })
            }
            disabled={busy || !canPaint}
            title="Image > Adjustments > Equalize (with a selection: equalize the selected area only)"
          >
            Equalize
          </button>
          <button
            className="button button--quiet"
            onClick={() =>
              selectedId !== null &&
              void runCommand("equalize", { id: selectedId, entireImage: true })
            }
            disabled={busy || !canPaint || !hasSelection}
            title="Image > Adjustments > Equalize > Equalize entire image based on selected area"
          >
            Equalize from Sel.
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowMaximumDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Other > Maximum"
          >
            Maximum…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowMinimumDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Other > Minimum"
          >
            Minimum…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowHighPassDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Other > High Pass"
          >
            High Pass…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowOffsetDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Other > Offset (wrap around)"
          >
            Offset…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowCustomDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Other > Custom (5×5 convolution kernel)"
          >
            Custom…
          </button>
          <button
            className="button button--quiet"
            onClick={() => selectedId !== null && void runCommand("find_edges", { id: selectedId })}
            disabled={busy || !canPaint}
            title="Filter > Stylize > Find Edges"
          >
            Find Edges
          </button>
          <button
            className="button button--quiet"
            onClick={() => selectedId !== null && void runCommand("solarize", { id: selectedId })}
            disabled={busy || !canPaint}
            title="Filter > Stylize > Solarize"
          >
            Solarize
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowEmbossDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Stylize > Emboss"
          >
            Emboss…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowTraceContourDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Stylize > Trace Contour"
          >
            Trace Contour…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowDiffuseDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Stylize > Diffuse"
          >
            Diffuse…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowGlowingEdgesDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Stylize > Glowing Edges"
          >
            Glowing Edges…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowMosaicDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Pixelate > Mosaic"
          >
            Mosaic…
          </button>
          <button
            className="button button--quiet"
            onClick={() => selectedId !== null && void runCommand("fragment", { id: selectedId })}
            disabled={busy || !canPaint}
            title="Filter > Pixelate > Fragment"
          >
            Fragment
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowRippleDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Distort > Ripple"
          >
            Ripple…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowTwirlDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Distort > Twirl"
          >
            Twirl…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPinchDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Distort > Pinch"
          >
            Pinch…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowSpherizeDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Distort > Spherize"
          >
            Spherize…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowZigZagDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Distort > ZigZag"
          >
            ZigZag…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPolarDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Distort > Polar Coordinates"
          >
            Polar Coordinates…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowWaveDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Distort > Wave"
          >
            Wave…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowColorHalftoneDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Pixelate > Color Halftone"
          >
            Color Halftone…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowCrystallizeDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Pixelate > Crystallize"
          >
            Crystallize…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPointillizeDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Pixelate > Pointillize"
          >
            Pointillize…
          </button>
          <input
            type="color"
            className="tools__color"
            value={brushColor}
            disabled={!canPaint || tool === "eraser"}
            aria-label="Brush color"
            onChange={(event) => setBrushColor(event.target.value)}
          />
          {tool === "gradient" && (
            <input
              type="color"
              className="tools__color"
              value={gradientEndColor}
              disabled={!canPaint}
              aria-label="Gradient end color"
              onChange={(event) => setGradientEndColor(event.target.value)}
            />
          )}
          <label className="tools__slider">
            Size
            <input
              type="range"
              min={1}
              max={150}
              value={brushSize}
              disabled={!canPaint}
              onChange={(event) => setBrushSize(Number(event.target.value))}
            />
          </label>
          <label className="tools__slider">
            Flow
            <input
              type="range"
              min={1}
              max={100}
              value={Math.round(brushOpacity * 100)}
              disabled={!canPaint || tool === "eraser"}
              onChange={(event) => setBrushOpacity(Number(event.target.value) / 100)}
            />
          </label>
        </div>
      </header>

      {showNewDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowNewDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="New document"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">New document</h2>
            <label className="control">
              <span className="control__label">Width</span>
              <input
                type="number"
                min={1}
                max={8000}
                value={newWidth}
                onChange={(event) => setNewWidth(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">Height</span>
              <input
                type="number"
                min={1}
                max={8000}
                value={newHeight}
                onChange={(event) => setNewHeight(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowNewDialog(false)}>
                Cancel
              </button>
              <button
                className="button"
                onClick={createNewDocument}
                disabled={busy || newWidth < 1 || newHeight < 1}
              >
                Create
              </button>
            </div>
          </div>
        </div>
      )}

      {modifyMode !== null && (
        <div
          className="modal-overlay"
          onClick={() => setModifyMode(null)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label={MODIFY_SELECTION_LABELS[modifyMode].heading}
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">{MODIFY_SELECTION_LABELS[modifyMode].heading}</h2>
            <label className="control">
              <span className="control__label">{MODIFY_SELECTION_LABELS[modifyMode].control}</span>
              <input
                type="number"
                min={1}
                max={4000}
                value={modifyAmount}
                onChange={(event) => setModifyAmount(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setModifyMode(null)}>
                Cancel
              </button>
              <button
                className="button"
                onClick={applyModifySelection}
                disabled={busy || modifyAmount < 1}
              >
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showThresholdDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowThresholdDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Threshold"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Threshold</h2>
            <label className="control">
              <span className="control__label">
                Level
                <span className="control__value">{thresholdLevel}</span>
              </span>
              <input
                type="range"
                min={1}
                max={255}
                value={thresholdLevel}
                onChange={(event) => setThresholdLevel(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowThresholdDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyThreshold} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showPosterizeDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowPosterizeDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Posterize"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Posterize</h2>
            <label className="control">
              <span className="control__label">
                Levels
                <span className="control__value">{posterizeLevels}</span>
              </span>
              <input
                type="range"
                min={2}
                max={64}
                value={posterizeLevels}
                onChange={(event) => setPosterizeLevels(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowPosterizeDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyPosterize} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showBrightnessContrastDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowBrightnessContrastDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Brightness/Contrast"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Brightness/Contrast</h2>
            <label className="control">
              <span className="control__label">
                Brightness
                <span className="control__value">{brightness}</span>
              </span>
              <input
                type="range"
                min={-150}
                max={150}
                value={brightness}
                onChange={(event) => setBrightness(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Contrast
                <span className="control__value">{contrast}</span>
              </span>
              <input
                type="range"
                min={-150}
                max={150}
                value={contrast}
                onChange={(event) => setContrast(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowBrightnessContrastDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyBrightnessContrast} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showHueSaturationDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowHueSaturationDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Hue/Saturation"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Hue/Saturation</h2>
            <label className="control">
              <span className="control__label">
                Hue
                <span className="control__value">{hue}</span>
              </span>
              <input
                type="range"
                min={-180}
                max={180}
                value={hue}
                onChange={(event) => setHue(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Saturation
                <span className="control__value">{saturation}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={saturation}
                onChange={(event) => setSaturation(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Lightness
                <span className="control__value">{lightness}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={lightness}
                onChange={(event) => setLightness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowHueSaturationDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyHueSaturation} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showVibranceDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowVibranceDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Vibrance"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Vibrance</h2>
            <label className="control">
              <span className="control__label">
                Vibrance
                <span className="control__value">{vibrance}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={vibrance}
                onChange={(event) => setVibrance(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Saturation
                <span className="control__value">{vibranceSaturation}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={vibranceSaturation}
                onChange={(event) => setVibranceSaturation(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowVibranceDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyVibrance} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showPhotoFilterDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowPhotoFilterDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Photo Filter"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Photo Filter</h2>
            <label className="control control--row">
              <span className="control__label">Filter Color</span>
              <input
                type="color"
                className="tools__color"
                value={photoFilterColor}
                onChange={(event) => setPhotoFilterColor(event.target.value)}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Density
                <span className="control__value">{photoFilterDensity}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={photoFilterDensity}
                onChange={(event) => setPhotoFilterDensity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowPhotoFilterDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyPhotoFilter} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showExposureDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowExposureDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Exposure"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Exposure</h2>
            <label className="control">
              <span className="control__label">
                Exposure
                <span className="control__value">{(exposureStops / 100).toFixed(2)}</span>
              </span>
              <input
                type="range"
                min={-200}
                max={200}
                value={exposureStops}
                onChange={(event) => setExposureStops(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Offset
                <span className="control__value">{(exposureOffset / 100).toFixed(2)}</span>
              </span>
              <input
                type="range"
                min={-50}
                max={50}
                value={exposureOffset}
                onChange={(event) => setExposureOffset(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Gamma
                <span className="control__value">{(exposureGamma / 100).toFixed(2)}</span>
              </span>
              <input
                type="range"
                min={10}
                max={300}
                value={exposureGamma}
                onChange={(event) => setExposureGamma(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowExposureDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyExposure} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showGradientMapDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowGradientMapDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Gradient Map"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Gradient Map</h2>
            <label className="control control--row">
              <span className="control__label">Shadows</span>
              <input
                type="color"
                className="tools__color"
                value={gradientMapShadow}
                onChange={(event) => setGradientMapShadow(event.target.value)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Highlights</span>
              <input
                type="color"
                className="tools__color"
                value={gradientMapHighlight}
                onChange={(event) => setGradientMapHighlight(event.target.value)}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowGradientMapDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyGradientMap} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showChannelMixerDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowChannelMixerDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Channel Mixer"
            style={{ width: 420 }}
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Channel Mixer</h2>
            <table className="channel-mixer">
              <thead>
                <tr>
                  <th />
                  <th>R</th>
                  <th>G</th>
                  <th>B</th>
                  <th>Constant</th>
                </tr>
              </thead>
              <tbody>
                {(["R", "G", "B"] as const).map((label, row) => (
                  <tr key={label}>
                    <th>{label}</th>
                    {channelMixerMatrix[row].map((value, col) => (
                      <td key={col}>
                        <input
                          type="number"
                          min={col === 3 ? -200 : -200}
                          max={200}
                          value={value}
                          onChange={(event) =>
                            setChannelMixerCell(row, col, Number(event.target.value))
                          }
                        />
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setChannelMixerMatrix(IDENTITY_CHANNEL_MIXER)}
              >
                Reset
              </button>
              <button
                className="button button--quiet"
                onClick={() => setShowChannelMixerDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyChannelMixer} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showLevelsDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowLevelsDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Levels"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Levels</h2>
            <label className="control">
              <span className="control__label">
                Input Black
                <span className="control__value">{levelsInputBlack}</span>
              </span>
              <input
                type="range"
                min={0}
                max={255}
                value={levelsInputBlack}
                onChange={(event) => setLevelsInputBlack(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Input White
                <span className="control__value">{levelsInputWhite}</span>
              </span>
              <input
                type="range"
                min={0}
                max={255}
                value={levelsInputWhite}
                onChange={(event) => setLevelsInputWhite(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Gamma
                <span className="control__value">{(levelsGamma / 100).toFixed(2)}</span>
              </span>
              <input
                type="range"
                min={10}
                max={300}
                value={levelsGamma}
                onChange={(event) => setLevelsGamma(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Output Black
                <span className="control__value">{levelsOutputBlack}</span>
              </span>
              <input
                type="range"
                min={0}
                max={255}
                value={levelsOutputBlack}
                onChange={(event) => setLevelsOutputBlack(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Output White
                <span className="control__value">{levelsOutputWhite}</span>
              </span>
              <input
                type="range"
                min={0}
                max={255}
                value={levelsOutputWhite}
                onChange={(event) => setLevelsOutputWhite(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowLevelsDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyLevels} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showCurvesDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowCurvesDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Curves"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Curves</h2>
            {curvePoints.map((value, index) => (
              <label className="control" key={index}>
                <span className="control__label">
                  Input {IDENTITY_CURVE[index]}
                  <span className="control__value">{value}</span>
                </span>
                <input
                  type="range"
                  min={0}
                  max={255}
                  value={value}
                  onChange={(event) => setCurvePoint(index, Number(event.target.value))}
                />
              </label>
            ))}
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setCurvePoints(IDENTITY_CURVE)}
              >
                Reset
              </button>
              <button
                className="button button--quiet"
                onClick={() => setShowCurvesDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyCurves} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showColorBalanceDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowColorBalanceDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Color Balance"
            style={{ width: 420 }}
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Color Balance</h2>
            <table className="channel-mixer">
              <thead>
                <tr>
                  <th />
                  <th>Cyan↔Red</th>
                  <th>Magenta↔Green</th>
                  <th>Yellow↔Blue</th>
                </tr>
              </thead>
              <tbody>
                {(
                  [
                    ["Shadows", colorBalanceShadows, setColorBalanceShadows],
                    ["Midtones", colorBalanceMidtones, setColorBalanceMidtones],
                    ["Highlights", colorBalanceHighlights, setColorBalanceHighlights],
                  ] as const
                ).map(([label, values, setter]) => (
                  <tr key={label}>
                    <th>{label}</th>
                    {values.map((value, index) => (
                      <td key={index}>
                        <input
                          type="number"
                          min={-100}
                          max={100}
                          value={value}
                          onChange={(event) =>
                            setColorBalanceValue(setter, index, Number(event.target.value))
                          }
                        />
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => {
                  setColorBalanceShadows([0, 0, 0]);
                  setColorBalanceMidtones([0, 0, 0]);
                  setColorBalanceHighlights([0, 0, 0]);
                }}
              >
                Reset
              </button>
              <button
                className="button button--quiet"
                onClick={() => setShowColorBalanceDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyColorBalance} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showSolidColorFillDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowSolidColorFillDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Solid Color Fill Layer"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Solid Color Fill Layer</h2>
            <label className="control control--row">
              <span className="control__label">Color</span>
              <input
                type="color"
                className="tools__color"
                value={solidColorFill}
                onChange={(event) => setSolidColorFill(event.target.value)}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowSolidColorFillDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applySolidColorFill} disabled={busy}>
                Add Layer
              </button>
            </div>
          </div>
        </div>
      )}

      {showGradientFillDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowGradientFillDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Gradient Fill Layer"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Gradient Fill Layer</h2>
            <label className="control control--row">
              <span className="control__label">Start Color</span>
              <input
                type="color"
                className="tools__color"
                value={gradientFillStart}
                onChange={(event) => setGradientFillStart(event.target.value)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">End Color</span>
              <input
                type="color"
                className="tools__color"
                value={gradientFillEnd}
                onChange={(event) => setGradientFillEnd(event.target.value)}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowGradientFillDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyGradientFill} disabled={busy}>
                Add Layer
              </button>
            </div>
          </div>
        </div>
      )}

      {showFillDialog && (
        <div className="modal-overlay" onClick={() => setShowFillDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Fill"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Edit &gt; Fill</h2>
            <label className="control control--row">
              <span className="control__label">Color</span>
              <input
                type="color"
                className="tools__color"
                value={fillColor}
                onChange={(event) => setFillColor(event.target.value)}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowFillDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyFill} disabled={busy}>
                Fill
              </button>
            </div>
          </div>
        </div>
      )}

      {showBoxBlurDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowBoxBlurDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Box Blur"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Blur &gt; Box Blur</h2>
            <label className="control">
              <span className="control__label">
                Radius
                <span className="control__value">{boxBlurRadius}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={40}
                value={boxBlurRadius}
                onChange={(event) => setBoxBlurRadius(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowBoxBlurDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyBoxBlur} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showUnsharpMaskDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowUnsharpMaskDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Unsharp Mask"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Sharpen &gt; Unsharp Mask</h2>
            <label className="control">
              <span className="control__label">
                Amount
                <span className="control__value">{unsharpMaskAmount}%</span>
              </span>
              <input
                type="range"
                min={1}
                max={500}
                value={unsharpMaskAmount}
                onChange={(event) => setUnsharpMaskAmount(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Radius
                <span className="control__value">{unsharpMaskRadius}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={40}
                value={unsharpMaskRadius}
                onChange={(event) => setUnsharpMaskRadius(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Threshold
                <span className="control__value">{unsharpMaskThreshold}</span>
              </span>
              <input
                type="range"
                min={0}
                max={255}
                value={unsharpMaskThreshold}
                onChange={(event) => setUnsharpMaskThreshold(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowUnsharpMaskDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyUnsharpMask} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showMotionBlurDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowMotionBlurDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Motion Blur"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Blur &gt; Motion Blur</h2>
            <label className="control">
              <span className="control__label">
                Angle
                <span className="control__value">{motionBlurAngle}°</span>
              </span>
              <input
                type="range"
                min={-180}
                max={180}
                value={motionBlurAngle}
                onChange={(event) => setMotionBlurAngle(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Distance
                <span className="control__value">{motionBlurDistance}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={60}
                value={motionBlurDistance}
                onChange={(event) => setMotionBlurDistance(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowMotionBlurDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyMotionBlur} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showMedianDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowMedianDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Median"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Noise &gt; Median</h2>
            <label className="control">
              <span className="control__label">
                Radius
                <span className="control__value">{medianRadius}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={16}
                value={medianRadius}
                onChange={(event) => setMedianRadius(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowMedianDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyMedian} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showDustAndScratchesDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowDustAndScratchesDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Dust & Scratches"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Noise &gt; Dust &amp; Scratches</h2>
            <label className="control">
              <span className="control__label">
                Radius
                <span className="control__value">{dustRadius}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={16}
                value={dustRadius}
                onChange={(event) => setDustRadius(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Threshold
                <span className="control__value">{dustThreshold}</span>
              </span>
              <input
                type="range"
                min={0}
                max={255}
                value={dustThreshold}
                onChange={(event) => setDustThreshold(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowDustAndScratchesDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyDustAndScratches} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showAddNoiseDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowAddNoiseDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Add Noise"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Noise &gt; Add Noise</h2>
            <label className="control">
              <span className="control__label">
                Amount
                <span className="control__value">{noiseAmount}%</span>
              </span>
              <input
                type="range"
                min={1}
                max={100}
                value={noiseAmount}
                onChange={(event) => setNoiseAmount(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Distribution</span>
              <select
                value={noiseGaussian ? "gaussian" : "uniform"}
                onChange={(event) => setNoiseGaussian(event.target.value === "gaussian")}
              >
                <option value="uniform">Uniform</option>
                <option value="gaussian">Gaussian</option>
              </select>
            </label>
            <label className="control control--row">
              <span className="control__label">Monochromatic</span>
              <input
                type="checkbox"
                checked={noiseMonochromatic}
                onChange={(event) => setNoiseMonochromatic(event.target.checked)}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowAddNoiseDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyAddNoise} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showMaximumDialog && (
        <div className="modal-overlay" onClick={() => setShowMaximumDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Maximum"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Other &gt; Maximum</h2>
            <label className="control">
              <span className="control__label">
                Radius
                <span className="control__value">{maximumRadius}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={16}
                value={maximumRadius}
                onChange={(event) => setMaximumRadius(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowMaximumDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyMaximum} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showMinimumDialog && (
        <div className="modal-overlay" onClick={() => setShowMinimumDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Minimum"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Other &gt; Minimum</h2>
            <label className="control">
              <span className="control__label">
                Radius
                <span className="control__value">{minimumRadius}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={16}
                value={minimumRadius}
                onChange={(event) => setMinimumRadius(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowMinimumDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyMinimum} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showHighPassDialog && (
        <div className="modal-overlay" onClick={() => setShowHighPassDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="High Pass"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Other &gt; High Pass</h2>
            <label className="control">
              <span className="control__label">
                Radius
                <span className="control__value">{highPassRadius}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={40}
                value={highPassRadius}
                onChange={(event) => setHighPassRadius(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowHighPassDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyHighPass} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showOffsetDialog && (
        <div className="modal-overlay" onClick={() => setShowOffsetDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Offset"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Other &gt; Offset</h2>
            <label className="control">
              <span className="control__label">
                Horizontal
                <span className="control__value">{offsetX}px</span>
              </span>
              <input
                type="range"
                min={-(document?.width ?? 1)}
                max={document?.width ?? 1}
                value={offsetX}
                onChange={(event) => setOffsetX(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Vertical
                <span className="control__value">{offsetY}px</span>
              </span>
              <input
                type="range"
                min={-(document?.height ?? 1)}
                max={document?.height ?? 1}
                value={offsetY}
                onChange={(event) => setOffsetY(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowOffsetDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyOffset} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showCustomDialog && (
        <div className="modal-overlay" onClick={() => setShowCustomDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Custom"
            style={{ width: 380 }}
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Other &gt; Custom</h2>
            <div className="kernel-grid">
              {customKernel.map((value, index) => (
                <input
                  key={index}
                  type="number"
                  min={-999}
                  max={999}
                  value={value}
                  aria-label={`Kernel row ${Math.floor(index / 5) + 1} column ${(index % 5) + 1}`}
                  onChange={(event) => {
                    const next = event.target.value;
                    setCustomKernel((kernel) => kernel.map((v, i) => (i === index ? next : v)));
                  }}
                />
              ))}
            </div>
            <label className="control">
              <span className="control__label">Scale</span>
              <input
                type="number"
                min={1}
                max={9999}
                value={customScale}
                onChange={(event) => setCustomScale(event.target.value)}
              />
            </label>
            <label className="control">
              <span className="control__label">Offset</span>
              <input
                type="number"
                min={-9999}
                max={9999}
                value={customOffset}
                onChange={(event) => setCustomOffset(event.target.value)}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => {
                  setCustomKernel(IDENTITY_KERNEL);
                  setCustomScale("1");
                  setCustomOffset("0");
                }}
              >
                Reset
              </button>
              <button className="button button--quiet" onClick={() => setShowCustomDialog(false)}>
                Cancel
              </button>
              <button
                className="button"
                onClick={applyCustom}
                disabled={busy || toInteger(customScale) === 0}
              >
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showEmbossDialog && (
        <div className="modal-overlay" onClick={() => setShowEmbossDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Emboss"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Stylize &gt; Emboss</h2>
            <label className="control">
              <span className="control__label">
                Angle
                <span className="control__value">{embossAngle}°</span>
              </span>
              <input
                type="range"
                min={-180}
                max={180}
                value={embossAngle}
                onChange={(event) => setEmbossAngle(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Height
                <span className="control__value">{embossHeight}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={100}
                value={embossHeight}
                onChange={(event) => setEmbossHeight(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Amount
                <span className="control__value">{embossAmount}%</span>
              </span>
              <input
                type="range"
                min={1}
                max={500}
                value={embossAmount}
                onChange={(event) => setEmbossAmount(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowEmbossDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyEmboss} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showTraceContourDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowTraceContourDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Trace Contour"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Stylize &gt; Trace Contour</h2>
            <label className="control">
              <span className="control__label">
                Level
                <span className="control__value">{traceLevel}</span>
              </span>
              <input
                type="range"
                min={0}
                max={255}
                value={traceLevel}
                onChange={(event) => setTraceLevel(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Upper edge</span>
              <input
                type="checkbox"
                checked={traceUpper}
                onChange={(event) => setTraceUpper(event.target.checked)}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowTraceContourDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyTraceContour} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showGaussianBlurDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowGaussianBlurDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Gaussian Blur"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Blur &gt; Gaussian Blur</h2>
            <label className="control">
              <span className="control__label">
                Radius
                <span className="control__value">{gaussianBlurRadius}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={25}
                value={gaussianBlurRadius}
                onChange={(event) => setGaussianBlurRadius(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowGaussianBlurDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyGaussianBlur} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showDiffuseDialog && (
        <div className="modal-overlay" onClick={() => setShowDiffuseDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Diffuse"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Stylize &gt; Diffuse</h2>
            {DIFFUSE_MODES.map(([value, label]) => (
              <label key={value} className="control control--row">
                <span className="control__label">{label}</span>
                <input
                  type="radio"
                  name="diffuse-mode"
                  value={value}
                  checked={diffuseMode === value}
                  onChange={() => setDiffuseMode(value)}
                />
              </label>
            ))}
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowDiffuseDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyDiffuse} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showSurfaceBlurDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowSurfaceBlurDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Surface Blur"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Blur &gt; Surface Blur</h2>
            <label className="control">
              <span className="control__label">
                Radius
                <span className="control__value">{surfaceBlurRadius}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={16}
                value={surfaceBlurRadius}
                onChange={(event) => setSurfaceBlurRadius(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Threshold
                <span className="control__value">{surfaceBlurThreshold} levels</span>
              </span>
              <input
                type="range"
                min={1}
                max={255}
                value={surfaceBlurThreshold}
                onChange={(event) => setSurfaceBlurThreshold(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowSurfaceBlurDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applySurfaceBlur} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showGlowingEdgesDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowGlowingEdgesDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Glowing Edges"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Stylize &gt; Glowing Edges</h2>
            <label className="control">
              <span className="control__label">
                Edge Width
                <span className="control__value">{glowEdgeWidth}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={14}
                value={glowEdgeWidth}
                onChange={(event) => setGlowEdgeWidth(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Edge Brightness
                <span className="control__value">{glowEdgeBrightness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={20}
                value={glowEdgeBrightness}
                onChange={(event) => setGlowEdgeBrightness(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Smoothness
                <span className="control__value">{glowSmoothness}</span>
              </span>
              <input
                type="range"
                min={1}
                max={15}
                value={glowSmoothness}
                onChange={(event) => setGlowSmoothness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowGlowingEdgesDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyGlowingEdges} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showMosaicDialog && (
        <div className="modal-overlay" onClick={() => setShowMosaicDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Mosaic"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Pixelate &gt; Mosaic</h2>
            <label className="control">
              <span className="control__label">
                Cell Size
                <span className="control__value">{mosaicCellSize}px</span>
              </span>
              <input
                type="range"
                min={2}
                max={64}
                value={mosaicCellSize}
                onChange={(event) => setMosaicCellSize(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowMosaicDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyMosaic} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showRippleDialog && (
        <div className="modal-overlay" onClick={() => setShowRippleDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Ripple"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Distort &gt; Ripple</h2>
            <label className="control">
              <span className="control__label">
                Amount
                <span className="control__value">{rippleAmount}%</span>
              </span>
              <input
                type="range"
                min={-999}
                max={999}
                value={rippleAmount}
                onChange={(event) => setRippleAmount(Number(event.target.value))}
              />
            </label>
            {RIPPLE_SIZES.map(([value, label]) => (
              <label key={value} className="control control--row">
                <span className="control__label">{label}</span>
                <input
                  type="radio"
                  name="ripple-size"
                  value={value}
                  checked={rippleSize === value}
                  onChange={() => setRippleSize(value)}
                />
              </label>
            ))}
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowRippleDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyRipple} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showTwirlDialog && (
        <div className="modal-overlay" onClick={() => setShowTwirlDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Twirl"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Distort &gt; Twirl</h2>
            <label className="control">
              <span className="control__label">
                Angle
                <span className="control__value">{twirlAngle}°</span>
              </span>
              <input
                type="range"
                min={-999}
                max={999}
                value={twirlAngle}
                onChange={(event) => setTwirlAngle(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowTwirlDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyTwirl} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showPinchDialog && (
        <div className="modal-overlay" onClick={() => setShowPinchDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Pinch"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Distort &gt; Pinch</h2>
            <label className="control">
              <span className="control__label">
                Amount
                <span className="control__value">{pinchAmount}%</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={pinchAmount}
                onChange={(event) => setPinchAmount(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowPinchDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyPinch} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showSpherizeDialog && (
        <div className="modal-overlay" onClick={() => setShowSpherizeDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Spherize"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Distort &gt; Spherize</h2>
            <label className="control">
              <span className="control__label">
                Amount
                <span className="control__value">{spherizeAmount}%</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={spherizeAmount}
                onChange={(event) => setSpherizeAmount(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowSpherizeDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applySpherize} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showZigZagDialog && (
        <div className="modal-overlay" onClick={() => setShowZigZagDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="ZigZag"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Distort &gt; ZigZag</h2>
            <label className="control">
              <span className="control__label">
                Amount
                <span className="control__value">{zigZagAmount}%</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={zigZagAmount}
                onChange={(event) => setZigZagAmount(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Ridges
                <span className="control__value">{zigZagRidges}</span>
              </span>
              <input
                type="range"
                min={1}
                max={20}
                value={zigZagRidges}
                onChange={(event) => setZigZagRidges(Number(event.target.value))}
              />
            </label>
            {ZIGZAG_STYLES.map(([value, label]) => (
              <label key={value} className="control control--row">
                <span className="control__label">{label}</span>
                <input
                  type="radio"
                  name="zigzag-style"
                  value={value}
                  checked={zigZagStyle === value}
                  onChange={() => setZigZagStyle(value)}
                />
              </label>
            ))}
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowZigZagDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyZigZag} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showPolarDialog && (
        <div className="modal-overlay" onClick={() => setShowPolarDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Polar Coordinates"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Distort &gt; Polar Coordinates</h2>
            <label className="control control--row">
              <span className="control__label">Rectangular to Polar</span>
              <input
                type="radio"
                name="polar-direction"
                checked={polarToPolar}
                onChange={() => setPolarToPolar(true)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Polar to Rectangular</span>
              <input
                type="radio"
                name="polar-direction"
                checked={!polarToPolar}
                onChange={() => setPolarToPolar(false)}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowPolarDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyPolarCoordinates} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showWaveDialog && (
        <div className="modal-overlay" onClick={() => setShowWaveDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Wave"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Distort &gt; Wave</h2>
            <label className="control">
              <span className="control__label">
                Number of Generators
                <span className="control__value">{waveGenerators}</span>
              </span>
              <input
                type="range"
                min={1}
                max={20}
                value={waveGenerators}
                onChange={(event) => setWaveGenerators(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Wavelength Min
                <span className="control__value">{waveWavelengthMin}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={200}
                value={waveWavelengthMin}
                onChange={(event) => {
                  const next = Number(event.target.value);
                  setWaveWavelengthMin(next);
                  if (next > waveWavelengthMax) setWaveWavelengthMax(next);
                }}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Wavelength Max
                <span className="control__value">{waveWavelengthMax}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={200}
                value={waveWavelengthMax}
                onChange={(event) => {
                  const next = Number(event.target.value);
                  setWaveWavelengthMax(next);
                  if (next < waveWavelengthMin) setWaveWavelengthMin(next);
                }}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Amplitude Min
                <span className="control__value">{waveAmplitudeMin}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={waveAmplitudeMin}
                onChange={(event) => {
                  const next = Number(event.target.value);
                  setWaveAmplitudeMin(next);
                  if (next > waveAmplitudeMax) setWaveAmplitudeMax(next);
                }}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Amplitude Max
                <span className="control__value">{waveAmplitudeMax}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={waveAmplitudeMax}
                onChange={(event) => {
                  const next = Number(event.target.value);
                  setWaveAmplitudeMax(next);
                  if (next < waveAmplitudeMin) setWaveAmplitudeMin(next);
                }}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Horizontal Scale
                <span className="control__value">{waveHorizontalScale}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={waveHorizontalScale}
                onChange={(event) => setWaveHorizontalScale(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Vertical Scale
                <span className="control__value">{waveVerticalScale}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={waveVerticalScale}
                onChange={(event) => setWaveVerticalScale(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowWaveDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyWave} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showColorHalftoneDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowColorHalftoneDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Color Halftone"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Pixelate &gt; Color Halftone</h2>
            <label className="control">
              <span className="control__label">
                Max Radius
                <span className="control__value">{colorHalftoneRadius}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={64}
                value={colorHalftoneRadius}
                onChange={(event) => setColorHalftoneRadius(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowColorHalftoneDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyColorHalftone} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showCrystallizeDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowCrystallizeDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Crystallize"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Pixelate &gt; Crystallize</h2>
            <label className="control">
              <span className="control__label">
                Cell Size
                <span className="control__value">{crystallizeCellSize}px</span>
              </span>
              <input
                type="range"
                min={3}
                max={64}
                value={crystallizeCellSize}
                onChange={(event) => setCrystallizeCellSize(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowCrystallizeDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyCrystallize} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showPointillizeDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowPointillizeDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Pointillize"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Pixelate &gt; Pointillize</h2>
            <label className="control">
              <span className="control__label">
                Cell Size
                <span className="control__value">{pointillizeCellSize}px</span>
              </span>
              <input
                type="range"
                min={3}
                max={64}
                value={pointillizeCellSize}
                onChange={(event) => setPointillizeCellSize(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Background</span>
              <input
                type="color"
                value={pointillizeBackground}
                onChange={(event) => setPointillizeBackground(event.target.value)}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowPointillizeDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyPointillize} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      <div className="workspace">
        <main className="stage">
          {error && (
            <div className="notice notice--error" role="alert">
              {error}
            </div>
          )}
          {!error && !compositeSrc && (
            <div className="notice">
              <p className="notice__lead">No image open</p>
              <p>
                Click <strong>Open PNG…</strong> or drop a .png file onto this window. Drop another
                to stack it as a layer.
              </p>
            </div>
          )}
          {compositeSrc && document && (
            <div className="canvas-wrap">
              <img
                className={`canvas${(isMarqueeTool || isLineSelect || isEyedropper ? hasDocument : canPaint) ? ` canvas--${tool}` : ""}`}
                src={compositeSrc}
                alt="Flattened composite"
                draggable={false}
                onDragStart={(event) => event.preventDefault()}
                onPointerDown={handlePointerDown}
                onPointerMove={handlePointerMove}
                onPointerUp={endStroke}
                onPointerCancel={endStroke}
                onPointerLeave={(event) => {
                  // Pointer capture keeps delivering move/up here even once the
                  // cursor leaves the element, but a mouse that was never
                  // pressed on the canvas has no capture to keep the stroke
                  // alive — treat leaving as the end of the stroke either way.
                  if (!event.currentTarget.hasPointerCapture(event.pointerId)) endStroke(event);
                }}
              />
              {marqueePreview && (
                <div
                  className={`selection-outline${tool === "selectEllipse" ? " selection-outline--ellipse" : ""}`}
                  style={overlayStyle(
                    marqueeBounds(marqueePreview.start, marqueePreview.current, document),
                    document,
                  )}
                />
              )}
              {!marqueePreview && document.selection && (
                <>
                  <div
                    className={`selection-outline${
                      document.selection.shape === "ellipse" ? " selection-outline--ellipse" : ""
                    }`}
                    style={{
                      ...overlayStyle(document.selection.bounds, document),
                      ...selectionRadiusStyle(document.selection.shape, document.selection.bounds),
                    }}
                  />
                  {document.selection.inverted && (
                    // Select > Inverse selects everywhere *outside* the shape above —
                    // a second outline around the whole canvas marks that outer edge.
                    <div
                      className="selection-outline"
                      style={overlayStyle(
                        { x0: 0, y0: 0, x1: document.width, y1: document.height },
                        document,
                      )}
                    />
                  )}
                  {document.selection.border !== null &&
                    (() => {
                      const inner = shrinkBounds(
                        document.selection.bounds,
                        document.selection.border,
                      );
                      // A border wide enough to swallow the whole shape (see
                      // shrink_rect / shrinkBounds) leaves no hole to outline.
                      if (!inner) return null;
                      return (
                        <div
                          className={`selection-outline${
                            document.selection.shape === "ellipse"
                              ? " selection-outline--ellipse"
                              : ""
                          }`}
                          style={{
                            ...overlayStyle(inner, document),
                            ...selectionRadiusStyle(document.selection.shape, inner),
                          }}
                        />
                      );
                    })()}
                </>
              )}
            </div>
          )}
        </main>

        <LayerPanel
          layers={layers}
          selectedId={selectedId}
          blendModes={blendModes}
          disabled={busy}
          onSelect={setSelectedId}
          onToggleVisible={(id, visible) =>
            void runCommand("set_layer_visible", { id, visible })
          }
          onToggleLocked={(id, locked) => void runCommand("set_layer_locked", { id, locked })}
          onOpacity={(id, opacity) => void runCommand("set_layer_opacity", { id, opacity })}
          onOpacityDragStart={checkpoint}
          onBlendMode={(id, blendMode: BlendMode) =>
            void runCommand("set_layer_blend_mode", { id, blendMode })
          }
          onMove={(id, direction: MoveDirection) =>
            void runCommand("move_layer", { id, direction })
          }
          onRemove={(id) => void runCommand("remove_layer", { id })}
          onDuplicate={(id) => void runCommand("duplicate_layer", { id }, { above: id })}
          onMergeVisible={() => void runCommand("merge_visible")}
          onFlattenImage={() => void runCommand("flatten_image")}
          onMergeDown={(id) => void runCommand("merge_down", { id })}
          onRasterize={(id) => void runCommand("rasterize_layer", { id })}
          onFlipHorizontal={(id) => void runCommand("flip_layer_horizontal", { id })}
          onFlipVertical={(id) => void runCommand("flip_layer_vertical", { id })}
          onRotate180={(id) => void runCommand("rotate_layer_180", { id })}
        />
      </div>

      <footer className="statusbar">
        {document ? (
          <>
            <span className="statusbar__name">
              {document.width} × {document.height}
            </span>
            <span>
              {layers.length} layer{layers.length === 1 ? "" : "s"}
            </span>
          </>
        ) : (
          <span className="statusbar__name">Ready</span>
        )}
      </footer>
    </div>
  );
}
