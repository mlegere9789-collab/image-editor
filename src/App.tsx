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

  const [showReplaceColorDialog, setShowReplaceColorDialog] = useState(false);
  const [replaceColorTarget, setReplaceColorTarget] = useState("#ff0000");
  const [replaceColorFuzziness, setReplaceColorFuzziness] = useState(40);
  const [replaceColorHue, setReplaceColorHue] = useState(0);
  const [replaceColorSaturation, setReplaceColorSaturation] = useState(0);
  const [replaceColorLightness, setReplaceColorLightness] = useState(0);

  const [showVibranceDialog, setShowVibranceDialog] = useState(false);
  const [vibrance, setVibrance] = useState(0);
  const [vibranceSaturation, setVibranceSaturation] = useState(0);

  const [showPhotoFilterDialog, setShowPhotoFilterDialog] = useState(false);
  const [photoFilterColor, setPhotoFilterColor] = useState("#ff9933");
  const [photoFilterDensity, setPhotoFilterDensity] = useState(25);
  const [showTemperatureTintDialog, setShowTemperatureTintDialog] = useState(false);
  const [temperatureValue, setTemperatureValue] = useState(0);
  const [tintValue, setTintValue] = useState(0);
  const [showHighlightsShadowsDialog, setShowHighlightsShadowsDialog] = useState(false);
  const [highlightsValue, setHighlightsValue] = useState(0);
  const [shadowsValue, setShadowsValue] = useState(0);
  const [showClarityDialog, setShowClarityDialog] = useState(false);
  const [clarityAmount, setClarityAmount] = useState(20);
  const [showCameraRawSaturationDialog, setShowCameraRawSaturationDialog] = useState(false);
  const [cameraRawSaturation, setCameraRawSaturation] = useState(25);
  const [histogramData, setHistogramData] = useState<{
    counts: number[][];
    shadowClipping: [number, number, number];
  } | null>(null);
  const [rgbLevels, setRgbLevels] = useState<[number, number, number, number] | null>(null);
  const lastLevelsPixel = useRef<string | null>(null);
  const [showPointCurveDialog, setShowPointCurveDialog] = useState(false);
  const [pointCurvePoints, setPointCurvePoints] = useState<number[]>(IDENTITY_CURVE);
  const [showColorMixerDialog, setShowColorMixerDialog] = useState(false);
  const [colorMixerRange, setColorMixerRange] = useState(0);
  const [colorMixerHue, setColorMixerHue] = useState(0);
  const [colorMixerSaturation, setColorMixerSaturation] = useState(0);
  const [colorMixerLuminance, setColorMixerLuminance] = useState(0);
  const [showColorGradingDialog, setShowColorGradingDialog] = useState(false);
  const [colorGrading, setColorGrading] = useState<[number, number][]>([
    [220, 0],
    [40, 0],
    [40, 0],
  ]);
  const [showDefringeDialog, setShowDefringeDialog] = useState(false);
  const [defringeAmount, setDefringeAmount] = useState(50);

  const [showExposureDialog, setShowExposureDialog] = useState(false);
  const [exposureStops, setExposureStops] = useState(0);
  const [exposureOffset, setExposureOffset] = useState(0);
  const [exposureGamma, setExposureGamma] = useState(100);

  const [showGradientMapDialog, setShowGradientMapDialog] = useState(false);
  const [gradientMapShadow, setGradientMapShadow] = useState("#000000");
  const [gradientMapHighlight, setGradientMapHighlight] = useState("#ffffff");

  const [showChannelMixerDialog, setShowChannelMixerDialog] = useState(false);
  const [showSelectiveColorDialog, setShowSelectiveColorDialog] = useState(false);
  const [selectiveColorCyan, setSelectiveColorCyan] = useState(0);
  const [selectiveColorMagenta, setSelectiveColorMagenta] = useState(0);
  const [selectiveColorYellow, setSelectiveColorYellow] = useState(0);
  const [selectiveColorBlack, setSelectiveColorBlack] = useState(0);
  const [showStrokeOutlineDialog, setShowStrokeOutlineDialog] = useState(false);
  const [strokeOutlineSize, setStrokeOutlineSize] = useState(3);
  const [strokeOutlineColor, setStrokeOutlineColor] = useState("#000000");
  const [strokeOutlineOpacity, setStrokeOutlineOpacity] = useState(100);
  const [showColorOverlayDialog, setShowColorOverlayDialog] = useState(false);
  const [colorOverlayColor, setColorOverlayColor] = useState("#ff0000");
  const [colorOverlayOpacity, setColorOverlayOpacity] = useState(100);
  const [showGradientOverlayDialog, setShowGradientOverlayDialog] = useState(false);
  const [gradientOverlayColor1, setGradientOverlayColor1] = useState("#000000");
  const [gradientOverlayColor2, setGradientOverlayColor2] = useState("#ffffff");
  const [gradientOverlayDirection, setGradientOverlayDirection] = useState(0);
  const [gradientOverlayOpacity, setGradientOverlayOpacity] = useState(100);
  const [showOuterGlowDialog, setShowOuterGlowDialog] = useState(false);
  const [outerGlowSize, setOuterGlowSize] = useState(10);
  const [outerGlowColor, setOuterGlowColor] = useState("#ffff00");
  const [outerGlowOpacity, setOuterGlowOpacity] = useState(75);
  const [showInnerGlowDialog, setShowInnerGlowDialog] = useState(false);
  const [innerGlowSize, setInnerGlowSize] = useState(10);
  const [innerGlowColor, setInnerGlowColor] = useState("#ffff00");
  const [innerGlowOpacity, setInnerGlowOpacity] = useState(75);
  const [showDropShadowDialog, setShowDropShadowDialog] = useState(false);
  const [dropShadowDistance, setDropShadowDistance] = useState(5);
  const [dropShadowAngle, setDropShadowAngle] = useState(135);
  const [dropShadowSize, setDropShadowSize] = useState(5);
  const [dropShadowColor, setDropShadowColor] = useState("#000000");
  const [dropShadowOpacity, setDropShadowOpacity] = useState(75);
  const [showInnerShadowDialog, setShowInnerShadowDialog] = useState(false);
  const [innerShadowDistance, setInnerShadowDistance] = useState(5);
  const [innerShadowAngle, setInnerShadowAngle] = useState(135);
  const [innerShadowSize, setInnerShadowSize] = useState(5);
  const [innerShadowColor, setInnerShadowColor] = useState("#000000");
  const [innerShadowOpacity, setInnerShadowOpacity] = useState(75);
  const [showPatternOverlayDialog, setShowPatternOverlayDialog] = useState(false);
  const [patternOverlayScale, setPatternOverlayScale] = useState(10);
  const [patternOverlayColor1, setPatternOverlayColor1] = useState("#000000");
  const [patternOverlayColor2, setPatternOverlayColor2] = useState("#ffffff");
  const [patternOverlayOpacity, setPatternOverlayOpacity] = useState(100);
  const [showBevelEmbossDialog, setShowBevelEmbossDialog] = useState(false);
  const [bevelEmbossSize, setBevelEmbossSize] = useState(5);
  const [bevelEmbossLightDirection, setBevelEmbossLightDirection] = useState(7);
  const [bevelEmbossStrength, setBevelEmbossStrength] = useState(50);
  const [showContourDialog, setShowContourDialog] = useState(false);
  const [contourSize, setContourSize] = useState(5);
  const [contourLightDirection, setContourLightDirection] = useState(7);
  const [contourStrength, setContourStrength] = useState(50);
  const [showTextureDialog, setShowTextureDialog] = useState(false);
  const [textureSize, setTextureSize] = useState(5);
  const [textureLightDirection, setTextureLightDirection] = useState(7);
  const [textureStrength, setTextureStrength] = useState(50);
  const [textureScale, setTextureScale] = useState(10);
  const [textureDepth, setTextureDepth] = useState(20);
  const [showTexturizerDialog, setShowTexturizerDialog] = useState(false);
  const [texturizerScale, setTexturizerScale] = useState(10);
  const [texturizerRelief, setTexturizerRelief] = useState(10);
  const [texturizerLightDirection, setTexturizerLightDirection] = useState(7);
  const [texturizerInvert, setTexturizerInvert] = useState(false);
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
  const [showRadialBlurDialog, setShowRadialBlurDialog] = useState(false);
  const [radialBlurAmount, setRadialBlurAmount] = useState(50);
  const [radialBlurCenterX, setRadialBlurCenterX] = useState(0);
  const [radialBlurCenterY, setRadialBlurCenterY] = useState(0);
  const [showTiltShiftDialog, setShowTiltShiftDialog] = useState(false);
  const [tiltShiftFocusRow, setTiltShiftFocusRow] = useState(0);
  const [tiltShiftHalfHeight, setTiltShiftHalfHeight] = useState(20);
  const [tiltShiftBlurRadius, setTiltShiftBlurRadius] = useState(15);
  const [showIrisBlurDialog, setShowIrisBlurDialog] = useState(false);
  const [irisBlurCenterX, setIrisBlurCenterX] = useState(0);
  const [irisBlurCenterY, setIrisBlurCenterY] = useState(0);
  const [irisBlurRadius, setIrisBlurRadius] = useState(50);
  const [irisBlurBlurRadius, setIrisBlurBlurRadius] = useState(15);
  const [showFieldBlurDialog, setShowFieldBlurDialog] = useState(false);
  const [fieldBlurX1, setFieldBlurX1] = useState(0);
  const [fieldBlurY1, setFieldBlurY1] = useState(0);
  const [fieldBlurRadius1, setFieldBlurRadius1] = useState(0);
  const [fieldBlurX2, setFieldBlurX2] = useState(0);
  const [fieldBlurY2, setFieldBlurY2] = useState(0);
  const [fieldBlurRadius2, setFieldBlurRadius2] = useState(15);
  const [showSpinBlurDialog, setShowSpinBlurDialog] = useState(false);
  const [spinBlurCenterX, setSpinBlurCenterX] = useState(0);
  const [spinBlurCenterY, setSpinBlurCenterY] = useState(0);
  const [spinBlurAngle, setSpinBlurAngle] = useState(15);
  const [showLensBlurDialog, setShowLensBlurDialog] = useState(false);
  const [lensBlurRadius, setLensBlurRadius] = useState(15);
  const [lensBlurInvert, setLensBlurInvert] = useState(false);
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
  const [showShearDialog, setShowShearDialog] = useState(false);
  const [shearControlPoints, setShearControlPoints] = useState([0, 0, 0, 0, 0]);
  const [shearWrapAround, setShearWrapAround] = useState(false);
  const [showDisplaceDialog, setShowDisplaceDialog] = useState(false);
  const [displaceMapLayerId, setDisplaceMapLayerId] = useState<number | null>(null);
  const [showMatchColorDialog, setShowMatchColorDialog] = useState(false);
  const [matchColorSourceLayerId, setMatchColorSourceLayerId] = useState<number | null>(null);
  const [matchColorFade, setMatchColorFade] = useState(100);
  const [displaceHorizontalScale, setDisplaceHorizontalScale] = useState(10);
  const [displaceVerticalScale, setDisplaceVerticalScale] = useState(10);
  const [displaceWrapAround, setDisplaceWrapAround] = useState(false);
  const [showColorHalftoneDialog, setShowColorHalftoneDialog] = useState(false);
  const [colorHalftoneRadius, setColorHalftoneRadius] = useState(8);
  const [showMezzotintDialog, setShowMezzotintDialog] = useState(false);
  const [mezzotintCellSize, setMezzotintCellSize] = useState(8);
  const [showExtrudeDialog, setShowExtrudeDialog] = useState(false);
  const [extrudeSize, setExtrudeSize] = useState(20);
  const [extrudeDepth, setExtrudeDepth] = useState(30);
  const [extrudeRandom, setExtrudeRandom] = useState(false);
  const [showColoredPencilDialog, setShowColoredPencilDialog] = useState(false);
  const [coloredPencilWidth, setColoredPencilWidth] = useState(4);
  const [coloredPencilPressure, setColoredPencilPressure] = useState(8);
  const [coloredPencilPaper, setColoredPencilPaper] = useState(25);
  const [showCutoutDialog, setShowCutoutDialog] = useState(false);
  const [cutoutLevels, setCutoutLevels] = useState(4);
  const [cutoutSimplicity, setCutoutSimplicity] = useState(4);
  const [showDryBrushDialog, setShowDryBrushDialog] = useState(false);
  const [dryBrushSize, setDryBrushSize] = useState(4);
  const [dryBrushDetail, setDryBrushDetail] = useState(6);
  const [showFilmGrainDialog, setShowFilmGrainDialog] = useState(false);
  const [filmGrainAmount, setFilmGrainAmount] = useState(6);
  const [filmGrainHighlightArea, setFilmGrainHighlightArea] = useState(4);
  const [filmGrainIntensity, setFilmGrainIntensity] = useState(6);
  const [showNeonGlowDialog, setShowNeonGlowDialog] = useState(false);
  const [neonGlowSize, setNeonGlowSize] = useState(5);
  const [neonGlowBrightness, setNeonGlowBrightness] = useState(25);
  const [neonGlowColor, setNeonGlowColor] = useState("#00ffff");
  const [showPosterEdgesDialog, setShowPosterEdgesDialog] = useState(false);
  const [posterEdgesThickness, setPosterEdgesThickness] = useState(2);
  const [posterEdgesIntensity, setPosterEdgesIntensity] = useState(4);
  const [posterEdgesLevels, setPosterEdgesLevels] = useState(4);
  const [showSpongeDialog, setShowSpongeDialog] = useState(false);
  const [spongeBrushSize, setSpongeBrushSize] = useState(3);
  const [spongeDefinition, setSpongeDefinition] = useState(12);
  const [showWatercolorDialog, setShowWatercolorDialog] = useState(false);
  const [watercolorBrushDetail, setWatercolorBrushDetail] = useState(10);
  const [watercolorShadowIntensity, setWatercolorShadowIntensity] = useState(3);
  const [showDarkStrokesDialog, setShowDarkStrokesDialog] = useState(false);
  const [darkStrokesBalance, setDarkStrokesBalance] = useState(4);
  const [darkStrokesBlackIntensity, setDarkStrokesBlackIntensity] = useState(6);
  const [darkStrokesWhiteIntensity, setDarkStrokesWhiteIntensity] = useState(3);
  const [showInkOutlinesDialog, setShowInkOutlinesDialog] = useState(false);
  const [inkOutlinesStrokeLength, setInkOutlinesStrokeLength] = useState(1);
  const [inkOutlinesDarkIntensity, setInkOutlinesDarkIntensity] = useState(20);
  const [inkOutlinesLightIntensity, setInkOutlinesLightIntensity] = useState(10);
  const [showSpatterDialog, setShowSpatterDialog] = useState(false);
  const [spatterSprayRadius, setSpatterSprayRadius] = useState(5);
  const [spatterSmoothness, setSpatterSmoothness] = useState(3);
  const [showCrosshatchDialog, setShowCrosshatchDialog] = useState(false);
  const [crosshatchStrokeLength, setCrosshatchStrokeLength] = useState(10);
  const [crosshatchSharpness, setCrosshatchSharpness] = useState(5);
  const [crosshatchStrength, setCrosshatchStrength] = useState(1);
  const [showAccentedEdgesDialog, setShowAccentedEdgesDialog] = useState(false);
  const [accentedEdgesWidth, setAccentedEdgesWidth] = useState(2);
  const [accentedEdgesBrightness, setAccentedEdgesBrightness] = useState(20);
  const [accentedEdgesSmoothness, setAccentedEdgesSmoothness] = useState(3);
  const [showAngledStrokesDialog, setShowAngledStrokesDialog] = useState(false);
  const [angledStrokesDirectionBalance, setAngledStrokesDirectionBalance] = useState(50);
  const [angledStrokesStrokeLength, setAngledStrokesStrokeLength] = useState(10);
  const [angledStrokesSharpness, setAngledStrokesSharpness] = useState(3);
  const [showSprayedStrokesDialog, setShowSprayedStrokesDialog] = useState(false);
  const [sprayedStrokesLength, setSprayedStrokesLength] = useState(10);
  const [sprayedStrokesRadius, setSprayedStrokesRadius] = useState(10);
  const [sprayedStrokesDirection, setSprayedStrokesDirection] = useState(1);
  const [showSumiEDialog, setShowSumiEDialog] = useState(false);
  const [sumiEStrokeWidth, setSumiEStrokeWidth] = useState(5);
  const [sumiEStrokePressure, setSumiEStrokePressure] = useState(8);
  const [sumiEContrast, setSumiEContrast] = useState(10);
  const [showSmudgeStickDialog, setShowSmudgeStickDialog] = useState(false);
  const [smudgeStickStrokeLength, setSmudgeStickStrokeLength] = useState(2);
  const [smudgeStickHighlightArea, setSmudgeStickHighlightArea] = useState(5);
  const [smudgeStickIntensity, setSmudgeStickIntensity] = useState(3);
  const [showPaintDaubsDialog, setShowPaintDaubsDialog] = useState(false);
  const [paintDaubsBrushSize, setPaintDaubsBrushSize] = useState(10);
  const [paintDaubsSharpness, setPaintDaubsSharpness] = useState(10);
  const [showPaletteKnifeDialog, setShowPaletteKnifeDialog] = useState(false);
  const [paletteKnifeStrokeSize, setPaletteKnifeStrokeSize] = useState(10);
  const [paletteKnifeStrokeDetail, setPaletteKnifeStrokeDetail] = useState(2);
  const [paletteKnifeSoftness, setPaletteKnifeSoftness] = useState(2);
  const [showPlasticWrapDialog, setShowPlasticWrapDialog] = useState(false);
  const [plasticWrapHighlightStrength, setPlasticWrapHighlightStrength] = useState(15);
  const [plasticWrapDetail, setPlasticWrapDetail] = useState(7);
  const [plasticWrapSmoothness, setPlasticWrapSmoothness] = useState(7);
  const [showFrescoDialog, setShowFrescoDialog] = useState(false);
  const [frescoBrushSize, setFrescoBrushSize] = useState(2);
  const [frescoBrushDetail, setFrescoBrushDetail] = useState(5);
  const [frescoTexture, setFrescoTexture] = useState(1);
  const [showRoughPastelsDialog, setShowRoughPastelsDialog] = useState(false);
  const [roughPastelsStrokeLength, setRoughPastelsStrokeLength] = useState(10);
  const [roughPastelsStrokeDetail, setRoughPastelsStrokeDetail] = useState(10);
  const [roughPastelsRelief, setRoughPastelsRelief] = useState(10);
  const [showUnderpaintingDialog, setShowUnderpaintingDialog] = useState(false);
  const [underpaintingBrushSize, setUnderpaintingBrushSize] = useState(8);
  const [underpaintingTextureCoverage, setUnderpaintingTextureCoverage] = useState(20);
  const [showStampDialog, setShowStampDialog] = useState(false);
  const [stampLightDarkBalance, setStampLightDarkBalance] = useState(12);
  const [stampSmoothness, setStampSmoothness] = useState(5);
  const [showPhotocopyDialog, setShowPhotocopyDialog] = useState(false);
  const [photocopyDetail, setPhotocopyDetail] = useState(3);
  const [photocopyDarkness, setPhotocopyDarkness] = useState(20);
  const [showReticulationDialog, setShowReticulationDialog] = useState(false);
  const [reticulationDensity, setReticulationDensity] = useState(15);
  const [reticulationForegroundLevel, setReticulationForegroundLevel] = useState(10);
  const [reticulationBackgroundLevel, setReticulationBackgroundLevel] = useState(40);
  const [showNotePaperDialog, setShowNotePaperDialog] = useState(false);
  const [notePaperImageBalance, setNotePaperImageBalance] = useState(25);
  const [notePaperGraininess, setNotePaperGraininess] = useState(5);
  const [showGraphicPenDialog, setShowGraphicPenDialog] = useState(false);
  const [graphicPenStrokeLength, setGraphicPenStrokeLength] = useState(5);
  const [graphicPenLightDarkBalance, setGraphicPenLightDarkBalance] = useState(25);
  const [graphicPenDirection, setGraphicPenDirection] = useState(1);
  const [showChalkAndCharcoalDialog, setShowChalkAndCharcoalDialog] = useState(false);
  const [chalkAndCharcoalCharcoalArea, setChalkAndCharcoalCharcoalArea] = useState(10);
  const [chalkAndCharcoalChalkArea, setChalkAndCharcoalChalkArea] = useState(5);
  const [chalkAndCharcoalStrokePressure, setChalkAndCharcoalStrokePressure] = useState(1);
  const [showPlasterDialog, setShowPlasterDialog] = useState(false);
  const [plasterImageBalance, setPlasterImageBalance] = useState(20);
  const [plasterSmoothness, setPlasterSmoothness] = useState(5);
  const [plasterLightDirection, setPlasterLightDirection] = useState(7);
  const [showWaterPaperDialog, setShowWaterPaperDialog] = useState(false);
  const [waterPaperFiberLength, setWaterPaperFiberLength] = useState(10);
  const [waterPaperBrightness, setWaterPaperBrightness] = useState(50);
  const [waterPaperContrast, setWaterPaperContrast] = useState(50);
  const [showTornEdgesDialog, setShowTornEdgesDialog] = useState(false);
  const [tornEdgesImageBalance, setTornEdgesImageBalance] = useState(10);
  const [tornEdgesSmoothness, setTornEdgesSmoothness] = useState(5);
  const [tornEdgesContrast, setTornEdgesContrast] = useState(10);
  const [showBasReliefDialog, setShowBasReliefDialog] = useState(false);
  const [basReliefDetail, setBasReliefDetail] = useState(8);
  const [basReliefSmoothness, setBasReliefSmoothness] = useState(5);
  const [basReliefLightDirection, setBasReliefLightDirection] = useState(2);
  const [showHalftonePatternDialog, setShowHalftonePatternDialog] = useState(false);
  const [halftonePatternSize, setHalftonePatternSize] = useState(4);
  const [halftonePatternContrast, setHalftonePatternContrast] = useState(0);
  const [halftonePatternType, setHalftonePatternType] = useState(0);
  const [showChromeDialog, setShowChromeDialog] = useState(false);
  const [chromeDetail, setChromeDetail] = useState(4);
  const [chromeSmoothness, setChromeSmoothness] = useState(7);
  const [showDiffuseGlowDialog, setShowDiffuseGlowDialog] = useState(false);
  const [diffuseGlowGraininess, setDiffuseGlowGraininess] = useState(4);
  const [diffuseGlowGlowAmount, setDiffuseGlowGlowAmount] = useState(10);
  const [diffuseGlowClearAmount, setDiffuseGlowClearAmount] = useState(10);
  const [showGlassDialog, setShowGlassDialog] = useState(false);
  const [glassDistortion, setGlassDistortion] = useState(5);
  const [glassSmoothness, setGlassSmoothness] = useState(4);
  const [showOceanRippleDialog, setShowOceanRippleDialog] = useState(false);
  const [oceanRippleSize, setOceanRippleSize] = useState(7);
  const [oceanRippleMagnitude, setOceanRippleMagnitude] = useState(10);
  const [showWindDialog, setShowWindDialog] = useState(false);
  const [windMethod, setWindMethod] = useState(0);
  const [windDirection, setWindDirection] = useState(0);
  const [showGrainDialog, setShowGrainDialog] = useState(false);
  const [grainIntensity, setGrainIntensity] = useState(20);
  const [grainContrast, setGrainContrast] = useState(10);
  const [showTilesDialog, setShowTilesDialog] = useState(false);
  const [tilesTileSize, setTilesTileSize] = useState(6);
  const [tilesMaxOffset, setTilesMaxOffset] = useState(50);
  const [showMosaicTilesDialog, setShowMosaicTilesDialog] = useState(false);
  const [mosaicTilesTileSize, setMosaicTilesTileSize] = useState(10);
  const [mosaicTilesGroutWidth, setMosaicTilesGroutWidth] = useState(2);
  const [mosaicTilesLightenGrout, setMosaicTilesLightenGrout] = useState(0);
  const [showPatchworkDialog, setShowPatchworkDialog] = useState(false);
  const [patchworkSquareSize, setPatchworkSquareSize] = useState(10);
  const [patchworkRelief, setPatchworkRelief] = useState(10);
  const [showStainedGlassDialog, setShowStainedGlassDialog] = useState(false);
  const [stainedGlassCellSize, setStainedGlassCellSize] = useState(10);
  const [stainedGlassBorderThickness, setStainedGlassBorderThickness] = useState(2);
  const [stainedGlassLightIntensity, setStainedGlassLightIntensity] = useState(3);
  const [showCraquelureDialog, setShowCraquelureDialog] = useState(false);
  const [craquelureCrackSpacing, setCraquelureCrackSpacing] = useState(10);
  const [craquelureCrackDepth, setCraquelureCrackDepth] = useState(4);
  const [craquelureCrackBrightness, setCraquelureCrackBrightness] = useState(4);
  const [showCrystallizeDialog, setShowCrystallizeDialog] = useState(false);
  const [crystallizeCellSize, setCrystallizeCellSize] = useState(16);
  const [showPointillizeDialog, setShowPointillizeDialog] = useState(false);
  const [pointillizeCellSize, setPointillizeCellSize] = useState(16);
  const [pointillizeBackground, setPointillizeBackground] = useState("#ffffff");
  const [showCloudsDialog, setShowCloudsDialog] = useState(false);
  const [cloudsForeground, setCloudsForeground] = useState("#ffffff");
  const [cloudsBackground, setCloudsBackground] = useState("#000000");
  const [showDifferenceCloudsDialog, setShowDifferenceCloudsDialog] = useState(false);
  const [differenceCloudsForeground, setDifferenceCloudsForeground] = useState("#ffffff");
  const [differenceCloudsBackground, setDifferenceCloudsBackground] = useState("#000000");
  const [showFibersDialog, setShowFibersDialog] = useState(false);
  const [fibersVariance, setFibersVariance] = useState(50);
  const [fibersStrength, setFibersStrength] = useState(4);
  const [fibersForeground, setFibersForeground] = useState("#ffffff");
  const [fibersBackground, setFibersBackground] = useState("#000000");
  const [showLensFlareDialog, setShowLensFlareDialog] = useState(false);
  const [lensFlareCenterX, setLensFlareCenterX] = useState(0);
  const [lensFlareCenterY, setLensFlareCenterY] = useState(0);
  const [lensFlareBrightness, setLensFlareBrightness] = useState(100);
  const [showLightingEffectsDialog, setShowLightingEffectsDialog] = useState(false);
  const [lightingLightX, setLightingLightX] = useState(0);
  const [lightingLightY, setLightingLightY] = useState(0);
  const [lightingLightHeight, setLightingLightHeight] = useState(30);
  const [lightingIntensity, setLightingIntensity] = useState(100);
  const [lightingAmbience, setLightingAmbience] = useState(20);
  const [lightingBumpHeight, setLightingBumpHeight] = useState(50);
  const [lightingColor, setLightingColor] = useState("#ffffff");
  const [showDiffuseDialog, setShowDiffuseDialog] = useState(false);
  const [diffuseMode, setDiffuseMode] = useState<DiffuseMode>("normal");

  const [showUnsharpMaskDialog, setShowUnsharpMaskDialog] = useState(false);
  const [unsharpMaskRadius, setUnsharpMaskRadius] = useState(2);
  const [unsharpMaskAmount, setUnsharpMaskAmount] = useState(100);
  const [unsharpMaskThreshold, setUnsharpMaskThreshold] = useState(4);
  const [showSmartSharpenDialog, setShowSmartSharpenDialog] = useState(false);
  const [smartSharpenRadius, setSmartSharpenRadius] = useState(2);
  const [smartSharpenAmount, setSmartSharpenAmount] = useState(100);
  const [smartSharpenReduceNoise, setSmartSharpenReduceNoise] = useState(10);
  const [showReduceNoiseDialog, setShowReduceNoiseDialog] = useState(false);
  const [reduceNoiseStrength, setReduceNoiseStrength] = useState(6);
  const [reduceNoisePreserveDetails, setReduceNoisePreserveDetails] = useState(60);

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

  const applyReplaceColor = useCallback(async () => {
    if (selectedId === null) return;
    const [r, g, b] = hexToRgb(replaceColorTarget);
    await runCommand("replace_color", {
      id: selectedId,
      target: [r, g, b],
      fuzziness: replaceColorFuzziness,
      hue: replaceColorHue,
      saturation: replaceColorSaturation,
      lightness: replaceColorLightness,
    });
    setShowReplaceColorDialog(false);
  }, [
    runCommand,
    selectedId,
    replaceColorTarget,
    replaceColorFuzziness,
    replaceColorHue,
    replaceColorSaturation,
    replaceColorLightness,
  ]);

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

  const applyTemperatureTint = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("temperature_tint", {
      id: selectedId,
      temperature: temperatureValue,
      tint: tintValue,
    });
    setShowTemperatureTintDialog(false);
  }, [runCommand, selectedId, temperatureValue, tintValue]);

  const applyHighlightsShadows = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("highlights_shadows", {
      id: selectedId,
      highlights: highlightsValue,
      shadows: shadowsValue,
    });
    setShowHighlightsShadowsDialog(false);
  }, [runCommand, selectedId, highlightsValue, shadowsValue]);

  const applyClarity = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("clarity", { id: selectedId, amount: clarityAmount });
    setShowClarityDialog(false);
  }, [runCommand, selectedId, clarityAmount]);

  const applyCameraRawSaturation = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("camera_raw_saturation", {
      id: selectedId,
      saturation: cameraRawSaturation,
    });
    setShowCameraRawSaturationDialog(false);
  }, [runCommand, selectedId, cameraRawSaturation]);

  const openHistogramDialog = useCallback(() => {
    if (selectedId === null) return;
    void Promise.all([
      invoke<number[][]>("histogram", { id: selectedId }),
      invoke<[number, number, number]>("shadow_clipping", { id: selectedId }),
    ])
      .then(([counts, shadowClipping]) => setHistogramData({ counts, shadowClipping }))
      .catch((err) => setError(String(err)));
  }, [selectedId]);

  const setPointCurvePoint = useCallback((index: number, value: number) => {
    setPointCurvePoints((points) => points.map((p, i) => (i === index ? value : p)));
  }, []);

  const applyPointCurve = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("camera_raw_point_curve", { id: selectedId, points: pointCurvePoints });
    setShowPointCurveDialog(false);
  }, [runCommand, selectedId, pointCurvePoints]);

  const setColorGradingValue = useCallback((range: number, slot: 0 | 1, value: number) => {
    setColorGrading((wheels) =>
      wheels.map((wheel, i) =>
        i === range ? ((slot === 0 ? [value, wheel[1]] : [wheel[0], value]) as [number, number]) : wheel,
      ),
    );
  }, []);

  const applyColorGrading = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("color_grading", {
      id: selectedId,
      shadows: colorGrading[0],
      midtones: colorGrading[1],
      highlights: colorGrading[2],
    });
    setShowColorGradingDialog(false);
  }, [runCommand, selectedId, colorGrading]);

  const applyColorMixer = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("color_mixer", {
      id: selectedId,
      range: colorMixerRange,
      hue: colorMixerHue,
      saturation: colorMixerSaturation,
      luminance: colorMixerLuminance,
    });
    setShowColorMixerDialog(false);
  }, [
    runCommand,
    selectedId,
    colorMixerRange,
    colorMixerHue,
    colorMixerSaturation,
    colorMixerLuminance,
  ]);

  const applyDefringe = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("defringe", { id: selectedId, amount: defringeAmount });
    setShowDefringeDialog(false);
  }, [runCommand, selectedId, defringeAmount]);

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

  const applySelectiveColor = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("selective_color", {
      id: selectedId,
      cyan: selectiveColorCyan,
      magenta: selectiveColorMagenta,
      yellow: selectiveColorYellow,
      black: selectiveColorBlack,
    });
    setShowSelectiveColorDialog(false);
  }, [
    runCommand,
    selectedId,
    selectiveColorCyan,
    selectiveColorMagenta,
    selectiveColorYellow,
    selectiveColorBlack,
  ]);

  const applyStrokeOutline = useCallback(async () => {
    if (selectedId === null) return;
    const [r, g, b] = hexToRgb(strokeOutlineColor);
    await runCommand("stroke_outline", {
      id: selectedId,
      size: strokeOutlineSize,
      color: [r, g, b],
      opacity: strokeOutlineOpacity,
    });
    setShowStrokeOutlineDialog(false);
  }, [runCommand, selectedId, strokeOutlineSize, strokeOutlineColor, strokeOutlineOpacity]);

  const applyColorOverlay = useCallback(async () => {
    if (selectedId === null) return;
    const [r, g, b] = hexToRgb(colorOverlayColor);
    await runCommand("color_overlay", {
      id: selectedId,
      color: [r, g, b],
      opacity: colorOverlayOpacity,
    });
    setShowColorOverlayDialog(false);
  }, [runCommand, selectedId, colorOverlayColor, colorOverlayOpacity]);

  const applyGradientOverlay = useCallback(async () => {
    if (selectedId === null) return;
    const [r1, g1, b1] = hexToRgb(gradientOverlayColor1);
    const [r2, g2, b2] = hexToRgb(gradientOverlayColor2);
    await runCommand("gradient_overlay", {
      id: selectedId,
      color1: [r1, g1, b1],
      color2: [r2, g2, b2],
      direction: gradientOverlayDirection,
      opacity: gradientOverlayOpacity,
    });
    setShowGradientOverlayDialog(false);
  }, [
    runCommand,
    selectedId,
    gradientOverlayColor1,
    gradientOverlayColor2,
    gradientOverlayDirection,
    gradientOverlayOpacity,
  ]);

  const applyOuterGlow = useCallback(async () => {
    if (selectedId === null) return;
    const [r, g, b] = hexToRgb(outerGlowColor);
    await runCommand("outer_glow", {
      id: selectedId,
      size: outerGlowSize,
      color: [r, g, b],
      opacity: outerGlowOpacity,
    });
    setShowOuterGlowDialog(false);
  }, [runCommand, selectedId, outerGlowSize, outerGlowColor, outerGlowOpacity]);

  const applyInnerGlow = useCallback(async () => {
    if (selectedId === null) return;
    const [r, g, b] = hexToRgb(innerGlowColor);
    await runCommand("inner_glow", {
      id: selectedId,
      size: innerGlowSize,
      color: [r, g, b],
      opacity: innerGlowOpacity,
    });
    setShowInnerGlowDialog(false);
  }, [runCommand, selectedId, innerGlowSize, innerGlowColor, innerGlowOpacity]);

  const applyDropShadow = useCallback(async () => {
    if (selectedId === null) return;
    const [r, g, b] = hexToRgb(dropShadowColor);
    await runCommand("drop_shadow", {
      id: selectedId,
      distance: dropShadowDistance,
      angle: dropShadowAngle,
      size: dropShadowSize,
      color: [r, g, b],
      opacity: dropShadowOpacity,
    });
    setShowDropShadowDialog(false);
  }, [
    runCommand,
    selectedId,
    dropShadowDistance,
    dropShadowAngle,
    dropShadowSize,
    dropShadowColor,
    dropShadowOpacity,
  ]);

  const applyInnerShadow = useCallback(async () => {
    if (selectedId === null) return;
    const [r, g, b] = hexToRgb(innerShadowColor);
    await runCommand("inner_shadow", {
      id: selectedId,
      distance: innerShadowDistance,
      angle: innerShadowAngle,
      size: innerShadowSize,
      color: [r, g, b],
      opacity: innerShadowOpacity,
    });
    setShowInnerShadowDialog(false);
  }, [
    runCommand,
    selectedId,
    innerShadowDistance,
    innerShadowAngle,
    innerShadowSize,
    innerShadowColor,
    innerShadowOpacity,
  ]);

  const applyPatternOverlay = useCallback(async () => {
    if (selectedId === null) return;
    const [r1, g1, b1] = hexToRgb(patternOverlayColor1);
    const [r2, g2, b2] = hexToRgb(patternOverlayColor2);
    await runCommand("pattern_overlay", {
      id: selectedId,
      scale: patternOverlayScale,
      color1: [r1, g1, b1],
      color2: [r2, g2, b2],
      opacity: patternOverlayOpacity,
    });
    setShowPatternOverlayDialog(false);
  }, [
    runCommand,
    selectedId,
    patternOverlayScale,
    patternOverlayColor1,
    patternOverlayColor2,
    patternOverlayOpacity,
  ]);

  const applyBevelEmboss = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("bevel_emboss", {
      id: selectedId,
      size: bevelEmbossSize,
      lightDirection: bevelEmbossLightDirection,
      strength: bevelEmbossStrength,
    });
    setShowBevelEmbossDialog(false);
  }, [runCommand, selectedId, bevelEmbossSize, bevelEmbossLightDirection, bevelEmbossStrength]);

  const applyContour = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("contour", {
      id: selectedId,
      size: contourSize,
      lightDirection: contourLightDirection,
      strength: contourStrength,
    });
    setShowContourDialog(false);
  }, [runCommand, selectedId, contourSize, contourLightDirection, contourStrength]);

  const applyTexture = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("texture", {
      id: selectedId,
      size: textureSize,
      lightDirection: textureLightDirection,
      strength: textureStrength,
      scale: textureScale,
      depth: textureDepth,
    });
    setShowTextureDialog(false);
  }, [
    runCommand,
    selectedId,
    textureSize,
    textureLightDirection,
    textureStrength,
    textureScale,
    textureDepth,
  ]);

  const applyTexturizer = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("texturizer", {
      id: selectedId,
      scale: texturizerScale,
      relief: texturizerRelief,
      lightDirection: texturizerLightDirection,
      invert: texturizerInvert,
    });
    setShowTexturizerDialog(false);
  }, [
    runCommand,
    selectedId,
    texturizerScale,
    texturizerRelief,
    texturizerLightDirection,
    texturizerInvert,
  ]);

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

  const applyShear = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("shear", {
      id: selectedId,
      controlPoints: shearControlPoints,
      wrapAround: shearWrapAround,
    });
    setShowShearDialog(false);
  }, [runCommand, selectedId, shearControlPoints, shearWrapAround]);

  const openDisplaceDialog = useCallback(() => {
    const layers = document?.layers ?? [];
    const other = layers.find((layer) => layer.id !== selectedId) ?? null;
    setDisplaceMapLayerId(other?.id ?? null);
    setShowDisplaceDialog(true);
  }, [document, selectedId]);

  const applyDisplace = useCallback(async () => {
    if (selectedId === null || displaceMapLayerId === null) return;
    await runCommand("displace", {
      id: selectedId,
      mapLayerId: displaceMapLayerId,
      horizontalScale: displaceHorizontalScale,
      verticalScale: displaceVerticalScale,
      wrapAround: displaceWrapAround,
    });
    setShowDisplaceDialog(false);
  }, [
    runCommand,
    selectedId,
    displaceMapLayerId,
    displaceHorizontalScale,
    displaceVerticalScale,
    displaceWrapAround,
  ]);

  const openMatchColorDialog = useCallback(() => {
    const layers = document?.layers ?? [];
    const other = layers.find((layer) => layer.id !== selectedId) ?? null;
    setMatchColorSourceLayerId(other?.id ?? null);
    setShowMatchColorDialog(true);
  }, [document, selectedId]);

  const applyMatchColor = useCallback(async () => {
    if (selectedId === null || matchColorSourceLayerId === null) return;
    await runCommand("match_color", {
      id: selectedId,
      sourceLayerId: matchColorSourceLayerId,
      fade: matchColorFade,
    });
    setShowMatchColorDialog(false);
  }, [runCommand, selectedId, matchColorSourceLayerId, matchColorFade]);

  const applyColorHalftone = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("color_halftone", { id: selectedId, maxRadius: colorHalftoneRadius });
    setShowColorHalftoneDialog(false);
  }, [runCommand, selectedId, colorHalftoneRadius]);

  const applyMezzotint = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, as with Add Noise/Crystallize.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("mezzotint", { id: selectedId, cellSize: mezzotintCellSize, seed });
    setShowMezzotintDialog(false);
  }, [runCommand, selectedId, mezzotintCellSize]);

  const applyExtrude = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, as with Add Noise/Mezzotint (used only in Random mode).
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("extrude", {
      id: selectedId,
      cellSize: extrudeSize,
      depth: extrudeDepth,
      random: extrudeRandom,
      seed,
    });
    setShowExtrudeDialog(false);
  }, [runCommand, selectedId, extrudeSize, extrudeDepth, extrudeRandom]);

  const applyColoredPencil = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("colored_pencil", {
      id: selectedId,
      pencilWidth: coloredPencilWidth,
      strokePressure: coloredPencilPressure,
      paperBrightness: coloredPencilPaper,
    });
    setShowColoredPencilDialog(false);
  }, [runCommand, selectedId, coloredPencilWidth, coloredPencilPressure, coloredPencilPaper]);

  const applyCutout = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("cutout", {
      id: selectedId,
      levels: cutoutLevels,
      edgeSimplicity: cutoutSimplicity,
    });
    setShowCutoutDialog(false);
  }, [runCommand, selectedId, cutoutLevels, cutoutSimplicity]);

  const applyDryBrush = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("dry_brush", {
      id: selectedId,
      brushSize: dryBrushSize,
      brushDetail: dryBrushDetail,
    });
    setShowDryBrushDialog(false);
  }, [runCommand, selectedId, dryBrushSize, dryBrushDetail]);

  const applyFilmGrain = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, as with Add Noise.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("film_grain", {
      id: selectedId,
      grain: filmGrainAmount,
      highlightArea: filmGrainHighlightArea,
      intensity: filmGrainIntensity,
      seed,
    });
    setShowFilmGrainDialog(false);
  }, [runCommand, selectedId, filmGrainAmount, filmGrainHighlightArea, filmGrainIntensity]);

  const applyNeonGlow = useCallback(async () => {
    if (selectedId === null) return;
    const [r, g, b] = hexToRgb(neonGlowColor);
    await runCommand("neon_glow", {
      id: selectedId,
      glowSize: neonGlowSize,
      glowBrightness: neonGlowBrightness,
      color: [r, g, b],
    });
    setShowNeonGlowDialog(false);
  }, [runCommand, selectedId, neonGlowSize, neonGlowBrightness, neonGlowColor]);

  const applyPosterEdges = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("poster_edges", {
      id: selectedId,
      edgeThickness: posterEdgesThickness,
      edgeIntensity: posterEdgesIntensity,
      levels: posterEdgesLevels,
    });
    setShowPosterEdgesDialog(false);
  }, [runCommand, selectedId, posterEdgesThickness, posterEdgesIntensity, posterEdgesLevels]);

  const applySponge = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, as with Crystallize.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("sponge", {
      id: selectedId,
      brushSize: spongeBrushSize,
      definition: spongeDefinition,
      seed,
    });
    setShowSpongeDialog(false);
  }, [runCommand, selectedId, spongeBrushSize, spongeDefinition]);

  const applyWatercolor = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("watercolor", {
      id: selectedId,
      brushDetail: watercolorBrushDetail,
      shadowIntensity: watercolorShadowIntensity,
    });
    setShowWatercolorDialog(false);
  }, [runCommand, selectedId, watercolorBrushDetail, watercolorShadowIntensity]);

  const applyDarkStrokes = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("dark_strokes", {
      id: selectedId,
      balance: darkStrokesBalance,
      blackIntensity: darkStrokesBlackIntensity,
      whiteIntensity: darkStrokesWhiteIntensity,
    });
    setShowDarkStrokesDialog(false);
  }, [
    runCommand,
    selectedId,
    darkStrokesBalance,
    darkStrokesBlackIntensity,
    darkStrokesWhiteIntensity,
  ]);

  const applyInkOutlines = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("ink_outlines", {
      id: selectedId,
      strokeLength: inkOutlinesStrokeLength,
      darkIntensity: inkOutlinesDarkIntensity,
      lightIntensity: inkOutlinesLightIntensity,
    });
    setShowInkOutlinesDialog(false);
  }, [
    runCommand,
    selectedId,
    inkOutlinesStrokeLength,
    inkOutlinesDarkIntensity,
    inkOutlinesLightIntensity,
  ]);

  const applySpatter = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, as with Diffuse.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("spatter", {
      id: selectedId,
      sprayRadius: spatterSprayRadius,
      smoothness: spatterSmoothness,
      seed,
    });
    setShowSpatterDialog(false);
  }, [runCommand, selectedId, spatterSprayRadius, spatterSmoothness]);

  const applyCrosshatch = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("crosshatch", {
      id: selectedId,
      strokeLength: crosshatchStrokeLength,
      sharpness: crosshatchSharpness,
      strength: crosshatchStrength,
    });
    setShowCrosshatchDialog(false);
  }, [runCommand, selectedId, crosshatchStrokeLength, crosshatchSharpness, crosshatchStrength]);

  const applyAccentedEdges = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("accented_edges", {
      id: selectedId,
      edgeWidth: accentedEdgesWidth,
      edgeBrightness: accentedEdgesBrightness,
      smoothness: accentedEdgesSmoothness,
    });
    setShowAccentedEdgesDialog(false);
  }, [runCommand, selectedId, accentedEdgesWidth, accentedEdgesBrightness, accentedEdgesSmoothness]);

  const applyAngledStrokes = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("angled_strokes", {
      id: selectedId,
      directionBalance: angledStrokesDirectionBalance,
      strokeLength: angledStrokesStrokeLength,
      sharpness: angledStrokesSharpness,
    });
    setShowAngledStrokesDialog(false);
  }, [runCommand, selectedId, angledStrokesDirectionBalance, angledStrokesStrokeLength, angledStrokesSharpness]);

  const applySprayedStrokes = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("sprayed_strokes", {
      id: selectedId,
      strokeLength: sprayedStrokesLength,
      sprayRadius: sprayedStrokesRadius,
      direction: sprayedStrokesDirection,
    });
    setShowSprayedStrokesDialog(false);
  }, [runCommand, selectedId, sprayedStrokesLength, sprayedStrokesRadius, sprayedStrokesDirection]);

  const applySumiE = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("sumi_e", {
      id: selectedId,
      strokeWidth: sumiEStrokeWidth,
      strokePressure: sumiEStrokePressure,
      contrast: sumiEContrast,
    });
    setShowSumiEDialog(false);
  }, [runCommand, selectedId, sumiEStrokeWidth, sumiEStrokePressure, sumiEContrast]);

  const applySmudgeStick = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("smudge_stick", {
      id: selectedId,
      strokeLength: smudgeStickStrokeLength,
      highlightArea: smudgeStickHighlightArea,
      intensity: smudgeStickIntensity,
    });
    setShowSmudgeStickDialog(false);
  }, [runCommand, selectedId, smudgeStickStrokeLength, smudgeStickHighlightArea, smudgeStickIntensity]);

  const applyPaintDaubs = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("paint_daubs", {
      id: selectedId,
      brushSize: paintDaubsBrushSize,
      sharpness: paintDaubsSharpness,
    });
    setShowPaintDaubsDialog(false);
  }, [runCommand, selectedId, paintDaubsBrushSize, paintDaubsSharpness]);

  const applyPaletteKnife = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("palette_knife", {
      id: selectedId,
      strokeSize: paletteKnifeStrokeSize,
      strokeDetail: paletteKnifeStrokeDetail,
      softness: paletteKnifeSoftness,
    });
    setShowPaletteKnifeDialog(false);
  }, [runCommand, selectedId, paletteKnifeStrokeSize, paletteKnifeStrokeDetail, paletteKnifeSoftness]);

  const applyPlasticWrap = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("plastic_wrap", {
      id: selectedId,
      highlightStrength: plasticWrapHighlightStrength,
      detail: plasticWrapDetail,
      smoothness: plasticWrapSmoothness,
    });
    setShowPlasticWrapDialog(false);
  }, [runCommand, selectedId, plasticWrapHighlightStrength, plasticWrapDetail, plasticWrapSmoothness]);

  const applyFresco = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("fresco", {
      id: selectedId,
      brushSize: frescoBrushSize,
      brushDetail: frescoBrushDetail,
      texture: frescoTexture,
    });
    setShowFrescoDialog(false);
  }, [runCommand, selectedId, frescoBrushSize, frescoBrushDetail, frescoTexture]);

  const applyRoughPastels = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("rough_pastels", {
      id: selectedId,
      strokeLength: roughPastelsStrokeLength,
      strokeDetail: roughPastelsStrokeDetail,
      relief: roughPastelsRelief,
    });
    setShowRoughPastelsDialog(false);
  }, [runCommand, selectedId, roughPastelsStrokeLength, roughPastelsStrokeDetail, roughPastelsRelief]);

  const applyUnderpainting = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("underpainting", {
      id: selectedId,
      brushSize: underpaintingBrushSize,
      textureCoverage: underpaintingTextureCoverage,
    });
    setShowUnderpaintingDialog(false);
  }, [runCommand, selectedId, underpaintingBrushSize, underpaintingTextureCoverage]);

  const applyStamp = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("stamp", {
      id: selectedId,
      lightDarkBalance: stampLightDarkBalance,
      smoothness: stampSmoothness,
    });
    setShowStampDialog(false);
  }, [runCommand, selectedId, stampLightDarkBalance, stampSmoothness]);

  const applyPhotocopy = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("photocopy", {
      id: selectedId,
      detail: photocopyDetail,
      darkness: photocopyDarkness,
    });
    setShowPhotocopyDialog(false);
  }, [runCommand, selectedId, photocopyDetail, photocopyDarkness]);

  const applyReticulation = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, as with Film Grain.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("reticulation", {
      id: selectedId,
      density: reticulationDensity,
      foregroundLevel: reticulationForegroundLevel,
      backgroundLevel: reticulationBackgroundLevel,
      seed,
    });
    setShowReticulationDialog(false);
  }, [runCommand, selectedId, reticulationDensity, reticulationForegroundLevel, reticulationBackgroundLevel]);

  const applyNotePaper = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, as with Film Grain.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("note_paper", {
      id: selectedId,
      imageBalance: notePaperImageBalance,
      graininess: notePaperGraininess,
      seed,
    });
    setShowNotePaperDialog(false);
  }, [runCommand, selectedId, notePaperImageBalance, notePaperGraininess]);

  const applyGraphicPen = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("graphic_pen", {
      id: selectedId,
      strokeLength: graphicPenStrokeLength,
      lightDarkBalance: graphicPenLightDarkBalance,
      direction: graphicPenDirection,
    });
    setShowGraphicPenDialog(false);
  }, [runCommand, selectedId, graphicPenStrokeLength, graphicPenLightDarkBalance, graphicPenDirection]);

  const applyChalkAndCharcoal = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("chalk_and_charcoal", {
      id: selectedId,
      charcoalArea: chalkAndCharcoalCharcoalArea,
      chalkArea: chalkAndCharcoalChalkArea,
      strokePressure: chalkAndCharcoalStrokePressure,
    });
    setShowChalkAndCharcoalDialog(false);
  }, [runCommand, selectedId, chalkAndCharcoalCharcoalArea, chalkAndCharcoalChalkArea, chalkAndCharcoalStrokePressure]);

  const applyPlaster = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("plaster", {
      id: selectedId,
      imageBalance: plasterImageBalance,
      smoothness: plasterSmoothness,
      lightDirection: plasterLightDirection,
    });
    setShowPlasterDialog(false);
  }, [runCommand, selectedId, plasterImageBalance, plasterSmoothness, plasterLightDirection]);

  const applyWaterPaper = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("water_paper", {
      id: selectedId,
      fiberLength: waterPaperFiberLength,
      brightness: waterPaperBrightness,
      contrast: waterPaperContrast,
    });
    setShowWaterPaperDialog(false);
  }, [runCommand, selectedId, waterPaperFiberLength, waterPaperBrightness, waterPaperContrast]);

  const applyTornEdges = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, as with Film Grain.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("torn_edges", {
      id: selectedId,
      imageBalance: tornEdgesImageBalance,
      smoothness: tornEdgesSmoothness,
      contrast: tornEdgesContrast,
      seed,
    });
    setShowTornEdgesDialog(false);
  }, [runCommand, selectedId, tornEdgesImageBalance, tornEdgesSmoothness, tornEdgesContrast]);

  const applyBasRelief = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("bas_relief", {
      id: selectedId,
      detail: basReliefDetail,
      smoothness: basReliefSmoothness,
      lightDirection: basReliefLightDirection,
    });
    setShowBasReliefDialog(false);
  }, [runCommand, selectedId, basReliefDetail, basReliefSmoothness, basReliefLightDirection]);

  const applyHalftonePattern = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("halftone_pattern", {
      id: selectedId,
      size: halftonePatternSize,
      contrast: halftonePatternContrast,
      patternType: halftonePatternType,
    });
    setShowHalftonePatternDialog(false);
  }, [runCommand, selectedId, halftonePatternSize, halftonePatternContrast, halftonePatternType]);

  const applyChrome = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("chrome", {
      id: selectedId,
      detail: chromeDetail,
      smoothness: chromeSmoothness,
    });
    setShowChromeDialog(false);
  }, [runCommand, selectedId, chromeDetail, chromeSmoothness]);

  const applyDiffuseGlow = useCallback(async () => {
    if (selectedId === null) return;
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("diffuse_glow", {
      id: selectedId,
      graininess: diffuseGlowGraininess,
      glowAmount: diffuseGlowGlowAmount,
      clearAmount: diffuseGlowClearAmount,
      seed,
    });
    setShowDiffuseGlowDialog(false);
  }, [
    runCommand,
    selectedId,
    diffuseGlowGraininess,
    diffuseGlowGlowAmount,
    diffuseGlowClearAmount,
  ]);

  const applyGlass = useCallback(async () => {
    if (selectedId === null) return;
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("glass", {
      id: selectedId,
      distortion: glassDistortion,
      smoothness: glassSmoothness,
      seed,
    });
    setShowGlassDialog(false);
  }, [runCommand, selectedId, glassDistortion, glassSmoothness]);

  const applyOceanRipple = useCallback(async () => {
    if (selectedId === null) return;
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("ocean_ripple", {
      id: selectedId,
      rippleSize: oceanRippleSize,
      rippleMagnitude: oceanRippleMagnitude,
      seed,
    });
    setShowOceanRippleDialog(false);
  }, [runCommand, selectedId, oceanRippleSize, oceanRippleMagnitude]);

  const applyWind = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("wind", {
      id: selectedId,
      method: windMethod,
      direction: windDirection,
    });
    setShowWindDialog(false);
  }, [runCommand, selectedId, windMethod, windDirection]);

  const applyGrain = useCallback(async () => {
    if (selectedId === null) return;
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("grain", {
      id: selectedId,
      intensity: grainIntensity,
      contrast: grainContrast,
      seed,
    });
    setShowGrainDialog(false);
  }, [runCommand, selectedId, grainIntensity, grainContrast]);

  const applyTiles = useCallback(async () => {
    if (selectedId === null) return;
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("tiles", {
      id: selectedId,
      tileSize: tilesTileSize,
      maxOffset: tilesMaxOffset,
      seed,
    });
    setShowTilesDialog(false);
  }, [runCommand, selectedId, tilesTileSize, tilesMaxOffset]);

  const applyMosaicTiles = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("mosaic_tiles", {
      id: selectedId,
      tileSize: mosaicTilesTileSize,
      groutWidth: mosaicTilesGroutWidth,
      lightenGrout: mosaicTilesLightenGrout,
    });
    setShowMosaicTilesDialog(false);
  }, [
    runCommand,
    selectedId,
    mosaicTilesTileSize,
    mosaicTilesGroutWidth,
    mosaicTilesLightenGrout,
  ]);

  const applyPatchwork = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("patchwork", {
      id: selectedId,
      squareSize: patchworkSquareSize,
      relief: patchworkRelief,
    });
    setShowPatchworkDialog(false);
  }, [runCommand, selectedId, patchworkSquareSize, patchworkRelief]);

  const applyStainedGlass = useCallback(async () => {
    if (selectedId === null) return;
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("stained_glass", {
      id: selectedId,
      cellSize: stainedGlassCellSize,
      borderThickness: stainedGlassBorderThickness,
      lightIntensity: stainedGlassLightIntensity,
      seed,
    });
    setShowStainedGlassDialog(false);
  }, [
    runCommand,
    selectedId,
    stainedGlassCellSize,
    stainedGlassBorderThickness,
    stainedGlassLightIntensity,
  ]);

  const applyCraquelure = useCallback(async () => {
    if (selectedId === null) return;
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("craquelure", {
      id: selectedId,
      crackSpacing: craquelureCrackSpacing,
      crackDepth: craquelureCrackDepth,
      crackBrightness: craquelureCrackBrightness,
      seed,
    });
    setShowCraquelureDialog(false);
  }, [
    runCommand,
    selectedId,
    craquelureCrackSpacing,
    craquelureCrackDepth,
    craquelureCrackBrightness,
  ]);

  const applyCrystallize = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, as with Add Noise: the backend is deterministic per seed.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("crystallize", { id: selectedId, cellSize: crystallizeCellSize, seed });
    setShowCrystallizeDialog(false);
  }, [runCommand, selectedId, crystallizeCellSize]);

  const applyFacet = useCallback(async () => {
    if (selectedId === null) return;
    // A fresh seed per apply, as with Add Noise/Crystallize.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("facet", { id: selectedId, seed });
  }, [runCommand, selectedId]);

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

  const applyClouds = useCallback(async () => {
    if (selectedId === null) return;
    const [fr, fg, fb] = hexToRgb(cloudsForeground);
    const [br, bg, bb] = hexToRgb(cloudsBackground);
    // A fresh seed per apply, as with Add Noise/Crystallize/Pointillize.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("clouds", {
      id: selectedId,
      foreground: [fr, fg, fb, 255],
      background: [br, bg, bb, 255],
      seed,
    });
    setShowCloudsDialog(false);
  }, [runCommand, selectedId, cloudsForeground, cloudsBackground]);

  const applyDifferenceClouds = useCallback(async () => {
    if (selectedId === null) return;
    const [fr, fg, fb] = hexToRgb(differenceCloudsForeground);
    const [br, bg, bb] = hexToRgb(differenceCloudsBackground);
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("difference_clouds", {
      id: selectedId,
      foreground: [fr, fg, fb, 255],
      background: [br, bg, bb, 255],
      seed,
    });
    setShowDifferenceCloudsDialog(false);
  }, [runCommand, selectedId, differenceCloudsForeground, differenceCloudsBackground]);

  const applyFibers = useCallback(async () => {
    if (selectedId === null) return;
    const [fr, fg, fb] = hexToRgb(fibersForeground);
    const [br, bg, bb] = hexToRgb(fibersBackground);
    // A fresh seed per apply, as with Add Noise/Clouds.
    const seed = (Date.now() ^ Math.floor(Math.random() * 0xffffffff)) >>> 0;
    await runCommand("fibers", {
      id: selectedId,
      variance: fibersVariance,
      strength: fibersStrength,
      foreground: [fr, fg, fb, 255],
      background: [br, bg, bb, 255],
      seed,
    });
    setShowFibersDialog(false);
  }, [runCommand, selectedId, fibersVariance, fibersStrength, fibersForeground, fibersBackground]);

  const openLensFlareDialog = useCallback(() => {
    setLensFlareCenterX(Math.round((document?.width ?? 2) / 2));
    setLensFlareCenterY(Math.round((document?.height ?? 2) / 2));
    setShowLensFlareDialog(true);
  }, [document]);

  const openRadialBlurDialog = useCallback(() => {
    setRadialBlurCenterX(Math.round((document?.width ?? 2) / 2));
    setRadialBlurCenterY(Math.round((document?.height ?? 2) / 2));
    setShowRadialBlurDialog(true);
  }, [document]);

  const applyRadialBlur = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("radial_blur", {
      id: selectedId,
      amount: radialBlurAmount,
      centerX: radialBlurCenterX,
      centerY: radialBlurCenterY,
    });
    setShowRadialBlurDialog(false);
  }, [runCommand, selectedId, radialBlurAmount, radialBlurCenterX, radialBlurCenterY]);

  const openTiltShiftDialog = useCallback(() => {
    setTiltShiftFocusRow(Math.round((document?.height ?? 2) / 2));
    setShowTiltShiftDialog(true);
  }, [document]);

  const applyTiltShift = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("tilt_shift", {
      id: selectedId,
      focusRow: tiltShiftFocusRow,
      halfHeight: tiltShiftHalfHeight,
      blurRadius: tiltShiftBlurRadius,
    });
    setShowTiltShiftDialog(false);
  }, [runCommand, selectedId, tiltShiftFocusRow, tiltShiftHalfHeight, tiltShiftBlurRadius]);

  const openIrisBlurDialog = useCallback(() => {
    setIrisBlurCenterX(Math.round((document?.width ?? 2) / 2));
    setIrisBlurCenterY(Math.round((document?.height ?? 2) / 2));
    setShowIrisBlurDialog(true);
  }, [document]);

  const applyIrisBlur = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("iris_blur", {
      id: selectedId,
      centerX: irisBlurCenterX,
      centerY: irisBlurCenterY,
      radius: irisBlurRadius,
      blurRadius: irisBlurBlurRadius,
    });
    setShowIrisBlurDialog(false);
  }, [
    runCommand,
    selectedId,
    irisBlurCenterX,
    irisBlurCenterY,
    irisBlurRadius,
    irisBlurBlurRadius,
  ]);

  const openFieldBlurDialog = useCallback(() => {
    setFieldBlurX1(Math.round((document?.width ?? 2) / 4));
    setFieldBlurY1(Math.round((document?.height ?? 2) / 4));
    setFieldBlurX2(Math.round(((document?.width ?? 2) * 3) / 4));
    setFieldBlurY2(Math.round(((document?.height ?? 2) * 3) / 4));
    setShowFieldBlurDialog(true);
  }, [document]);

  const applyFieldBlur = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("field_blur", {
      id: selectedId,
      x1: fieldBlurX1,
      y1: fieldBlurY1,
      radius1: fieldBlurRadius1,
      x2: fieldBlurX2,
      y2: fieldBlurY2,
      radius2: fieldBlurRadius2,
    });
    setShowFieldBlurDialog(false);
  }, [
    runCommand,
    selectedId,
    fieldBlurX1,
    fieldBlurY1,
    fieldBlurRadius1,
    fieldBlurX2,
    fieldBlurY2,
    fieldBlurRadius2,
  ]);

  const openSpinBlurDialog = useCallback(() => {
    setSpinBlurCenterX(Math.round((document?.width ?? 2) / 2));
    setSpinBlurCenterY(Math.round((document?.height ?? 2) / 2));
    setShowSpinBlurDialog(true);
  }, [document]);

  const applySpinBlur = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("spin_blur", {
      id: selectedId,
      centerX: spinBlurCenterX,
      centerY: spinBlurCenterY,
      angle: spinBlurAngle,
    });
    setShowSpinBlurDialog(false);
  }, [runCommand, selectedId, spinBlurCenterX, spinBlurCenterY, spinBlurAngle]);

  const applyLensBlur = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("lens_blur", {
      id: selectedId,
      maxRadius: lensBlurRadius,
      invert: lensBlurInvert,
    });
    setShowLensBlurDialog(false);
  }, [runCommand, selectedId, lensBlurRadius, lensBlurInvert]);

  const applyLensFlare = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("lens_flare", {
      id: selectedId,
      centerX: lensFlareCenterX,
      centerY: lensFlareCenterY,
      brightness: lensFlareBrightness,
    });
    setShowLensFlareDialog(false);
  }, [runCommand, selectedId, lensFlareCenterX, lensFlareCenterY, lensFlareBrightness]);

  const openLightingEffectsDialog = useCallback(() => {
    setLightingLightX(Math.round((document?.width ?? 2) / 2));
    setLightingLightY(Math.round((document?.height ?? 2) / 2));
    setShowLightingEffectsDialog(true);
  }, [document]);

  const applyLightingEffects = useCallback(async () => {
    if (selectedId === null) return;
    const [r, g, b] = hexToRgb(lightingColor);
    await runCommand("lighting_effects", {
      id: selectedId,
      lightX: lightingLightX,
      lightY: lightingLightY,
      lightHeight: lightingLightHeight,
      intensity: lightingIntensity,
      ambience: lightingAmbience,
      bumpHeight: lightingBumpHeight,
      color: [r, g, b],
    });
    setShowLightingEffectsDialog(false);
  }, [
    runCommand,
    selectedId,
    lightingLightX,
    lightingLightY,
    lightingLightHeight,
    lightingIntensity,
    lightingAmbience,
    lightingBumpHeight,
    lightingColor,
  ]);

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

  const applySmartSharpen = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("smart_sharpen", {
      id: selectedId,
      radius: smartSharpenRadius,
      amount: smartSharpenAmount / 100,
      reduceNoise: smartSharpenReduceNoise,
    });
    setShowSmartSharpenDialog(false);
  }, [runCommand, selectedId, smartSharpenRadius, smartSharpenAmount, smartSharpenReduceNoise]);

  const applyReduceNoise = useCallback(async () => {
    if (selectedId === null) return;
    await runCommand("reduce_noise", {
      id: selectedId,
      strength: reduceNoiseStrength,
      preserveDetails: reduceNoisePreserveDetails,
    });
    setShowReduceNoiseDialog(false);
  }, [runCommand, selectedId, reduceNoiseStrength, reduceNoisePreserveDetails]);

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

  const readLevelsAt = useCallback(
    (event: React.PointerEvent<HTMLImageElement>) => {
      if (!document || selectedId === null) return;
      const [fx, fy] = toDocPoint(event, document);
      const x = Math.floor(fx);
      const y = Math.floor(fy);
      if (x < 0 || y < 0 || x >= document.width || y >= document.height) return;
      const key = `${selectedId}:${x}:${y}`;
      if (lastLevelsPixel.current === key) return;
      lastLevelsPixel.current = key;
      void invoke<[number, number, number, number]>("rgb_levels", { id: selectedId, x, y })
        .then((levels) => setRgbLevels(levels))
        .catch(() => setRgbLevels(null));
    },
    [document, selectedId],
  );

  // A repaint under a stationary pointer would otherwise leave the readout
  // showing the pre-edit value until the pointer moved to a new pixel. Every
  // snapshot hands back a fresh document view, so keying on it catches
  // every edit.
  useEffect(() => {
    lastLevelsPixel.current = null;
  }, [document]);

  const handlePointerMove = useCallback(
    (event: React.PointerEvent<HTMLImageElement>) => {
      if (!document) return;
      readLevelsAt(event);
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
    [document, isMarqueeTool, applyStroke, readLevelsAt],
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
            onClick={() => setShowReplaceColorDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Replace Color"
          >
            Replace Color…
          </button>
          <button
            className="button button--quiet"
            onClick={openMatchColorDialog}
            disabled={busy || !canPaint || (document?.layers.length ?? 0) < 2}
            title="Image > Adjustments > Match Color"
          >
            Match Color…
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
            onClick={() => setShowTemperatureTintDialog(true)}
            disabled={busy || !canPaint}
            title="Camera Raw Filter > Temperature/Tint"
          >
            Temperature/Tint…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowHighlightsShadowsDialog(true)}
            disabled={busy || !canPaint}
            title="Camera Raw Filter > Highlights/Shadows"
          >
            Highlights/Shadows…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowClarityDialog(true)}
            disabled={busy || !canPaint}
            title="Camera Raw Filter > Clarity"
          >
            Clarity…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowCameraRawSaturationDialog(true)}
            disabled={busy || !canPaint}
            title="Camera Raw Filter > Saturation"
          >
            Saturation…
          </button>
          <button
            className="button button--quiet"
            onClick={openHistogramDialog}
            disabled={busy || selectedId === null}
            title="Camera Raw Filter > Histogram"
          >
            Histogram…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPointCurveDialog(true)}
            disabled={busy || !canPaint}
            title="Camera Raw Filter > Curve > Point Curve"
          >
            Point Curve…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowColorGradingDialog(true)}
            disabled={busy || !canPaint}
            title="Camera Raw Filter > Color Grading"
          >
            Color Grading…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowColorMixerDialog(true)}
            disabled={busy || !canPaint}
            title="Camera Raw Filter > Color Mixer"
          >
            Color Mixer…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowDefringeDialog(true)}
            disabled={busy || !canPaint}
            title="Camera Raw Filter > Optics > Defringe"
          >
            Defringe…
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
            onClick={() => setShowSelectiveColorDialog(true)}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Selective Color"
          >
            Selective Color…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowStrokeOutlineDialog(true)}
            disabled={busy || !canPaint}
            title="Layer > Layer Style > Stroke"
          >
            Stroke…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowColorOverlayDialog(true)}
            disabled={busy || !canPaint}
            title="Layer > Layer Style > Color Overlay"
          >
            Color Overlay…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowGradientOverlayDialog(true)}
            disabled={busy || !canPaint}
            title="Layer > Layer Style > Gradient Overlay"
          >
            Gradient Overlay…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowOuterGlowDialog(true)}
            disabled={busy || !canPaint}
            title="Layer > Layer Style > Outer Glow"
          >
            Outer Glow…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowInnerGlowDialog(true)}
            disabled={busy || !canPaint}
            title="Layer > Layer Style > Inner Glow"
          >
            Inner Glow…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowDropShadowDialog(true)}
            disabled={busy || !canPaint}
            title="Layer > Layer Style > Drop Shadow"
          >
            Drop Shadow…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowInnerShadowDialog(true)}
            disabled={busy || !canPaint}
            title="Layer > Layer Style > Inner Shadow"
          >
            Inner Shadow…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPatternOverlayDialog(true)}
            disabled={busy || !canPaint}
            title="Layer > Layer Style > Pattern Overlay"
          >
            Pattern Overlay…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowBevelEmbossDialog(true)}
            disabled={busy || !canPaint}
            title="Layer > Layer Style > Bevel & Emboss"
          >
            Bevel &amp; Emboss…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowContourDialog(true)}
            disabled={busy || !canPaint}
            title="Layer > Layer Style > Contour"
          >
            Contour…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowTextureDialog(true)}
            disabled={busy || !canPaint}
            title="Layer > Layer Style > Texture"
          >
            Texture…
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
            onClick={() => setShowSmartSharpenDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Sharpen > Smart Sharpen"
          >
            Smart Sharpen…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowReduceNoiseDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Noise > Reduce Noise"
          >
            Reduce Noise…
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
            onClick={() => selectedId !== null && void runCommand("auto_tone", { id: selectedId })}
            disabled={busy || !canPaint}
            title="Image > Adjustments > Auto Tone"
          >
            Auto Tone
          </button>
          <button
            className="button button--quiet"
            onClick={() =>
              selectedId !== null && void runCommand("auto_contrast", { id: selectedId })
            }
            disabled={busy || !canPaint}
            title="Image > Adjustments > Auto Contrast"
          >
            Auto Contrast
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
            onClick={() => setShowExtrudeDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Stylize > Extrude"
          >
            Extrude…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowWindDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Stylize > Wind"
          >
            Wind…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowTilesDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Stylize > Tiles"
          >
            Tiles…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowGrainDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Texture > Grain"
          >
            Grain…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowMosaicTilesDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Texture > Mosaic Tiles"
          >
            Mosaic Tiles…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPatchworkDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Texture > Patchwork"
          >
            Patchwork…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowStainedGlassDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Texture > Stained Glass"
          >
            Stained Glass…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowCraquelureDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Texture > Craquelure"
          >
            Craquelure…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowTexturizerDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Texture > Texturizer"
          >
            Texturizer…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowColoredPencilDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Colored Pencil"
          >
            Colored Pencil…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowCutoutDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Cutout"
          >
            Cutout…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowDryBrushDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Dry Brush"
          >
            Dry Brush…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowFilmGrainDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Film Grain"
          >
            Film Grain…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowNeonGlowDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Neon Glow"
          >
            Neon Glow…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPosterEdgesDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Poster Edges"
          >
            Poster Edges…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowSpongeDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Sponge"
          >
            Sponge…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowWatercolorDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Watercolor"
          >
            Watercolor…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowSmudgeStickDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Smudge Stick"
          >
            Smudge Stick…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPaintDaubsDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Paint Daubs"
          >
            Paint Daubs…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPaletteKnifeDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Palette Knife"
          >
            Palette Knife…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPlasticWrapDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Plastic Wrap"
          >
            Plastic Wrap…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowFrescoDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Fresco"
          >
            Fresco…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowRoughPastelsDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Rough Pastels"
          >
            Rough Pastels…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowUnderpaintingDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Artistic > Underpainting"
          >
            Underpainting…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowStampDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Sketch > Stamp"
          >
            Stamp…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPhotocopyDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Sketch > Photocopy"
          >
            Photocopy…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowReticulationDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Sketch > Reticulation"
          >
            Reticulation…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowNotePaperDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Sketch > Note Paper"
          >
            Note Paper…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowGraphicPenDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Sketch > Graphic Pen"
          >
            Graphic Pen…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowChalkAndCharcoalDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Sketch > Chalk & Charcoal"
          >
            Chalk &amp; Charcoal…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPlasterDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Sketch > Plaster"
          >
            Plaster…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowWaterPaperDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Sketch > Water Paper"
          >
            Water Paper…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowTornEdgesDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Sketch > Torn Edges"
          >
            Torn Edges…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowBasReliefDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Sketch > Bas Relief"
          >
            Bas Relief…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowHalftonePatternDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Sketch > Halftone Pattern"
          >
            Halftone Pattern…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowChromeDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Sketch > Chrome"
          >
            Chrome…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowDarkStrokesDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Brush Strokes > Dark Strokes"
          >
            Dark Strokes…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowInkOutlinesDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Brush Strokes > Ink Outlines"
          >
            Ink Outlines…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowSpatterDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Brush Strokes > Spatter"
          >
            Spatter…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowCrosshatchDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Brush Strokes > Crosshatch"
          >
            Crosshatch…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowAccentedEdgesDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Brush Strokes > Accented Edges"
          >
            Accented Edges…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowAngledStrokesDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Brush Strokes > Angled Strokes"
          >
            Angled Strokes…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowSprayedStrokesDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Brush Strokes > Sprayed Strokes"
          >
            Sprayed Strokes…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowSumiEDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Brush Strokes > Sumi-e"
          >
            Sumi-e…
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
            onClick={openRadialBlurDialog}
            disabled={busy || !canPaint}
            title="Filter > Blur > Radial Blur"
          >
            Radial Blur…
          </button>
          <button
            className="button button--quiet"
            onClick={openTiltShiftDialog}
            disabled={busy || !canPaint}
            title="Filter Gallery > Blur Gallery > Tilt-Shift"
          >
            Tilt-Shift…
          </button>
          <button
            className="button button--quiet"
            onClick={openIrisBlurDialog}
            disabled={busy || !canPaint}
            title="Filter Gallery > Blur Gallery > Iris Blur"
          >
            Iris Blur…
          </button>
          <button
            className="button button--quiet"
            onClick={openFieldBlurDialog}
            disabled={busy || !canPaint}
            title="Filter Gallery > Blur Gallery > Field Blur"
          >
            Field Blur…
          </button>
          <button
            className="button button--quiet"
            onClick={openSpinBlurDialog}
            disabled={busy || !canPaint}
            title="Filter Gallery > Blur Gallery > Spin Blur"
          >
            Spin Blur…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowLensBlurDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Blur > Lens Blur"
          >
            Lens Blur…
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
            onClick={() => setShowShearDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Distort > Shear"
          >
            Shear…
          </button>
          <button
            className="button button--quiet"
            onClick={openDisplaceDialog}
            disabled={busy || !canPaint || (document?.layers.length ?? 0) < 2}
            title="Filter > Distort > Displace"
          >
            Displace…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowDiffuseGlowDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Distort > Diffuse Glow"
          >
            Diffuse Glow…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowGlassDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Distort > Glass"
          >
            Glass…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowOceanRippleDialog(true)}
            disabled={busy || !canPaint}
            title="Filter Gallery > Distort > Ocean Ripple"
          >
            Ocean Ripple…
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
            onClick={() => setShowMezzotintDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Pixelate > Mezzotint"
          >
            Mezzotint…
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
            onClick={() => void applyFacet()}
            disabled={busy || !canPaint}
            title="Filter > Pixelate > Facet"
          >
            Facet
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowPointillizeDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Pixelate > Pointillize"
          >
            Pointillize…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowCloudsDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Render > Clouds"
          >
            Clouds…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowDifferenceCloudsDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Render > Difference Clouds"
          >
            Difference Clouds…
          </button>
          <button
            className="button button--quiet"
            onClick={() => setShowFibersDialog(true)}
            disabled={busy || !canPaint}
            title="Filter > Render > Fibers"
          >
            Fibers…
          </button>
          <button
            className="button button--quiet"
            onClick={openLensFlareDialog}
            disabled={busy || !canPaint}
            title="Filter > Render > Lens Flare"
          >
            Lens Flare…
          </button>
          <button
            className="button button--quiet"
            onClick={openLightingEffectsDialog}
            disabled={busy || !canPaint}
            title="Filter > Render > Lighting Effects"
          >
            Lighting Effects…
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

      {showReplaceColorDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowReplaceColorDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Replace Color"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Image &gt; Adjustments &gt; Replace Color</h2>
            <label className="control control--row">
              <span className="control__label">Target Color</span>
              <input
                type="color"
                value={replaceColorTarget}
                onChange={(event) => setReplaceColorTarget(event.target.value)}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Fuzziness
                <span className="control__value">{replaceColorFuzziness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={200}
                value={replaceColorFuzziness}
                onChange={(event) => setReplaceColorFuzziness(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Hue
                <span className="control__value">{replaceColorHue}</span>
              </span>
              <input
                type="range"
                min={-180}
                max={180}
                value={replaceColorHue}
                onChange={(event) => setReplaceColorHue(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Saturation
                <span className="control__value">{replaceColorSaturation}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={replaceColorSaturation}
                onChange={(event) => setReplaceColorSaturation(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Lightness
                <span className="control__value">{replaceColorLightness}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={replaceColorLightness}
                onChange={(event) => setReplaceColorLightness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowReplaceColorDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyReplaceColor} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showMatchColorDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowMatchColorDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Match Color"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Image &gt; Adjustments &gt; Match Color</h2>
            <label className="control control--row">
              <span className="control__label">Source Layer</span>
              <select
                value={matchColorSourceLayerId ?? ""}
                onChange={(event) => setMatchColorSourceLayerId(Number(event.target.value))}
              >
                {(document?.layers ?? [])
                  .filter((layer) => layer.id !== selectedId)
                  .map((layer) => (
                    <option key={layer.id} value={layer.id}>
                      {layer.name}
                    </option>
                  ))}
              </select>
            </label>
            <label className="control">
              <span className="control__label">
                Fade
                <span className="control__value">{matchColorFade}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={matchColorFade}
                onChange={(event) => setMatchColorFade(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowMatchColorDialog(false)}
              >
                Cancel
              </button>
              <button
                className="button"
                onClick={applyMatchColor}
                disabled={busy || matchColorSourceLayerId === null}
              >
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

      {showTemperatureTintDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowTemperatureTintDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Temperature/Tint"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">
              Camera Raw Filter &gt; Temperature/Tint
            </h2>
            <label className="control">
              <span className="control__label">
                Temperature
                <span className="control__value">{temperatureValue}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={temperatureValue}
                onChange={(event) => setTemperatureValue(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Tint
                <span className="control__value">{tintValue}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={tintValue}
                onChange={(event) => setTintValue(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowTemperatureTintDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyTemperatureTint} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showHighlightsShadowsDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowHighlightsShadowsDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Highlights/Shadows"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">
              Camera Raw Filter &gt; Highlights/Shadows
            </h2>
            <label className="control">
              <span className="control__label">
                Highlights
                <span className="control__value">{highlightsValue}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={highlightsValue}
                onChange={(event) => setHighlightsValue(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Shadows
                <span className="control__value">{shadowsValue}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={shadowsValue}
                onChange={(event) => setShadowsValue(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowHighlightsShadowsDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyHighlightsShadows} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showClarityDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowClarityDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Clarity"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Camera Raw Filter &gt; Clarity</h2>
            <label className="control">
              <span className="control__label">
                Clarity
                <span className="control__value">{clarityAmount}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={clarityAmount}
                onChange={(event) => setClarityAmount(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowClarityDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyClarity} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showCameraRawSaturationDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowCameraRawSaturationDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Camera Raw Saturation"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Camera Raw Filter &gt; Saturation</h2>
            <label className="control">
              <span className="control__label">
                Saturation
                <span className="control__value">{cameraRawSaturation}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={cameraRawSaturation}
                onChange={(event) => setCameraRawSaturation(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowCameraRawSaturationDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyCameraRawSaturation} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {histogramData && (
        <div
          className="modal-overlay"
          onClick={() => setHistogramData(null)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Histogram"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Camera Raw Filter &gt; Histogram</h2>
            <p className="modal__hint">
              Red, green, and blue value distributions for the selected layer
              (the active selection only, when there is one). Each curve is
              scaled to the tallest bin across all three channels.
            </p>
            {(() => {
              const peak = Math.max(1, ...histogramData.counts.flat());
              const colors = ["#e5484d", "#46a758", "#3e63dd"];
              return (
                <svg
                  className="histogram"
                  viewBox="0 0 256 100"
                  preserveAspectRatio="none"
                  role="img"
                  aria-label="RGB histogram"
                >
                  {histogramData.counts.map((counts, channel) => (
                    <path
                      key={channel}
                      fill={colors[channel]}
                      fillOpacity={0.45}
                      d={
                        `M0,100 ` +
                        counts
                          .map((count, value) => `L${value},${100 - (count / peak) * 100}`)
                          .join(" ") +
                        " L255,100 Z"
                      }
                    />
                  ))}
                </svg>
              );
            })()}
            <div
              className="clipping"
              title="Camera Raw Filter > Shadow Clipping: sampled pixels with that channel at 0"
            >
              <span className="control__label">Shadow clipping</span>
              {(["R", "G", "B"] as const).map((name, channel) => {
                const count = histogramData.shadowClipping[channel];
                return (
                  <span
                    key={name}
                    className={`clipping__channel${count > 0 ? " clipping__channel--lit" : ""}`}
                  >
                    {name} {count}
                  </span>
                );
              })}
            </div>
            <div className="modal__actions">
              <button className="button" onClick={() => setHistogramData(null)}>
                Close
              </button>
            </div>
          </div>
        </div>
      )}

      {showPointCurveDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowPointCurveDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Point Curve"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Camera Raw Filter &gt; Curve &gt; Point Curve</h2>
            {pointCurvePoints.map((value, index) => (
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
                  onChange={(event) => setPointCurvePoint(index, Number(event.target.value))}
                />
              </label>
            ))}
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setPointCurvePoints(IDENTITY_CURVE)}
              >
                Reset
              </button>
              <button
                className="button button--quiet"
                onClick={() => setShowPointCurveDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyPointCurve} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showColorGradingDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowColorGradingDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Color Grading"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Camera Raw Filter &gt; Color Grading</h2>
            {(["Shadows", "Midtones", "Highlights"] as const).map((name, range) => (
              <div key={name}>
                <label className="control">
                  <span className="control__label">
                    {name} hue
                    <span className="control__value">{colorGrading[range][0]}°</span>
                  </span>
                  <input
                    type="range"
                    min={0}
                    max={360}
                    value={colorGrading[range][0]}
                    onChange={(event) =>
                      setColorGradingValue(range, 0, Number(event.target.value))
                    }
                  />
                </label>
                <label className="control">
                  <span className="control__label">
                    {name} saturation
                    <span className="control__value">{colorGrading[range][1]}</span>
                  </span>
                  <input
                    type="range"
                    min={0}
                    max={100}
                    value={colorGrading[range][1]}
                    onChange={(event) =>
                      setColorGradingValue(range, 1, Number(event.target.value))
                    }
                  />
                </label>
              </div>
            ))}
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowColorGradingDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyColorGrading} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showColorMixerDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowColorMixerDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Color Mixer"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Camera Raw Filter &gt; Color Mixer</h2>
            <label className="control">
              <span className="control__label">Range</span>
              <select
                value={colorMixerRange}
                onChange={(event) => setColorMixerRange(Number(event.target.value))}
              >
                {["Reds", "Oranges", "Yellows", "Greens", "Aquas", "Blues", "Purples", "Magentas"].map(
                  (name, index) => (
                    <option key={name} value={index}>
                      {name}
                    </option>
                  ),
                )}
              </select>
            </label>
            <label className="control">
              <span className="control__label">
                Hue
                <span className="control__value">{colorMixerHue}°</span>
              </span>
              <input
                type="range"
                min={-180}
                max={180}
                value={colorMixerHue}
                onChange={(event) => setColorMixerHue(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Saturation
                <span className="control__value">{colorMixerSaturation}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={colorMixerSaturation}
                onChange={(event) => setColorMixerSaturation(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Luminance
                <span className="control__value">{colorMixerLuminance}</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={colorMixerLuminance}
                onChange={(event) => setColorMixerLuminance(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowColorMixerDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyColorMixer} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showDefringeDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowDefringeDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Defringe"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">
              Camera Raw Filter &gt; Optics &gt; Defringe
            </h2>
            <label className="control">
              <span className="control__label">
                Amount
                <span className="control__value">{defringeAmount}</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={defringeAmount}
                onChange={(event) => setDefringeAmount(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowDefringeDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyDefringe} disabled={busy}>
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

      {showSelectiveColorDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowSelectiveColorDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Selective Color"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Selective Color (Neutrals)</h2>
            <label className="control">
              <span className="control__label">
                Cyan
                <span className="control__value">{selectiveColorCyan}%</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={selectiveColorCyan}
                onChange={(event) => setSelectiveColorCyan(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Magenta
                <span className="control__value">{selectiveColorMagenta}%</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={selectiveColorMagenta}
                onChange={(event) => setSelectiveColorMagenta(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Yellow
                <span className="control__value">{selectiveColorYellow}%</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={selectiveColorYellow}
                onChange={(event) => setSelectiveColorYellow(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Black
                <span className="control__value">{selectiveColorBlack}%</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={selectiveColorBlack}
                onChange={(event) => setSelectiveColorBlack(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowSelectiveColorDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applySelectiveColor} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showStrokeOutlineDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowStrokeOutlineDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Stroke"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Layer &gt; Layer Style &gt; Stroke</h2>
            <label className="control">
              <span className="control__label">
                Size
                <span className="control__value">{strokeOutlineSize}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={250}
                value={strokeOutlineSize}
                onChange={(event) => setStrokeOutlineSize(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Color</span>
              <input
                type="color"
                value={strokeOutlineColor}
                onChange={(event) => setStrokeOutlineColor(event.target.value)}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Opacity
                <span className="control__value">{strokeOutlineOpacity}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={strokeOutlineOpacity}
                onChange={(event) => setStrokeOutlineOpacity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowStrokeOutlineDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyStrokeOutline} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showColorOverlayDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowColorOverlayDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Color Overlay"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Layer &gt; Layer Style &gt; Color Overlay</h2>
            <label className="control control--row">
              <span className="control__label">Color</span>
              <input
                type="color"
                value={colorOverlayColor}
                onChange={(event) => setColorOverlayColor(event.target.value)}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Opacity
                <span className="control__value">{colorOverlayOpacity}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={colorOverlayOpacity}
                onChange={(event) => setColorOverlayOpacity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowColorOverlayDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyColorOverlay} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showGradientOverlayDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowGradientOverlayDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Gradient Overlay"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Layer &gt; Layer Style &gt; Gradient Overlay</h2>
            <label className="control control--row">
              <span className="control__label">Color 1</span>
              <input
                type="color"
                value={gradientOverlayColor1}
                onChange={(event) => setGradientOverlayColor1(event.target.value)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Color 2</span>
              <input
                type="color"
                value={gradientOverlayColor2}
                onChange={(event) => setGradientOverlayColor2(event.target.value)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Direction</span>
              <select
                value={gradientOverlayDirection}
                onChange={(event) => setGradientOverlayDirection(Number(event.target.value))}
              >
                <option value={0}>Horizontal</option>
                <option value={1}>Vertical</option>
              </select>
            </label>
            <label className="control">
              <span className="control__label">
                Opacity
                <span className="control__value">{gradientOverlayOpacity}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={gradientOverlayOpacity}
                onChange={(event) => setGradientOverlayOpacity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowGradientOverlayDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyGradientOverlay} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showOuterGlowDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowOuterGlowDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Outer Glow"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Layer &gt; Layer Style &gt; Outer Glow</h2>
            <label className="control">
              <span className="control__label">
                Size
                <span className="control__value">{outerGlowSize}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={250}
                value={outerGlowSize}
                onChange={(event) => setOuterGlowSize(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Color</span>
              <input
                type="color"
                value={outerGlowColor}
                onChange={(event) => setOuterGlowColor(event.target.value)}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Opacity
                <span className="control__value">{outerGlowOpacity}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={outerGlowOpacity}
                onChange={(event) => setOuterGlowOpacity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowOuterGlowDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyOuterGlow} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showInnerGlowDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowInnerGlowDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Inner Glow"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Layer &gt; Layer Style &gt; Inner Glow</h2>
            <label className="control">
              <span className="control__label">
                Size
                <span className="control__value">{innerGlowSize}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={250}
                value={innerGlowSize}
                onChange={(event) => setInnerGlowSize(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Color</span>
              <input
                type="color"
                value={innerGlowColor}
                onChange={(event) => setInnerGlowColor(event.target.value)}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Opacity
                <span className="control__value">{innerGlowOpacity}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={innerGlowOpacity}
                onChange={(event) => setInnerGlowOpacity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowInnerGlowDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyInnerGlow} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showDropShadowDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowDropShadowDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Drop Shadow"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Layer &gt; Layer Style &gt; Drop Shadow</h2>
            <label className="control">
              <span className="control__label">
                Distance
                <span className="control__value">{dropShadowDistance}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={dropShadowDistance}
                onChange={(event) => setDropShadowDistance(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Angle
                <span className="control__value">{dropShadowAngle}&deg;</span>
              </span>
              <input
                type="range"
                min={0}
                max={360}
                value={dropShadowAngle}
                onChange={(event) => setDropShadowAngle(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Size
                <span className="control__value">{dropShadowSize}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={250}
                value={dropShadowSize}
                onChange={(event) => setDropShadowSize(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Color</span>
              <input
                type="color"
                value={dropShadowColor}
                onChange={(event) => setDropShadowColor(event.target.value)}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Opacity
                <span className="control__value">{dropShadowOpacity}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={dropShadowOpacity}
                onChange={(event) => setDropShadowOpacity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowDropShadowDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyDropShadow} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showInnerShadowDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowInnerShadowDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Inner Shadow"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Layer &gt; Layer Style &gt; Inner Shadow</h2>
            <label className="control">
              <span className="control__label">
                Distance
                <span className="control__value">{innerShadowDistance}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={innerShadowDistance}
                onChange={(event) => setInnerShadowDistance(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Angle
                <span className="control__value">{innerShadowAngle}&deg;</span>
              </span>
              <input
                type="range"
                min={0}
                max={360}
                value={innerShadowAngle}
                onChange={(event) => setInnerShadowAngle(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Size
                <span className="control__value">{innerShadowSize}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={250}
                value={innerShadowSize}
                onChange={(event) => setInnerShadowSize(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Color</span>
              <input
                type="color"
                value={innerShadowColor}
                onChange={(event) => setInnerShadowColor(event.target.value)}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Opacity
                <span className="control__value">{innerShadowOpacity}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={innerShadowOpacity}
                onChange={(event) => setInnerShadowOpacity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowInnerShadowDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyInnerShadow} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showPatternOverlayDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowPatternOverlayDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Pattern Overlay"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Layer &gt; Layer Style &gt; Pattern Overlay</h2>
            <label className="control">
              <span className="control__label">
                Scale
                <span className="control__value">{patternOverlayScale}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={250}
                value={patternOverlayScale}
                onChange={(event) => setPatternOverlayScale(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Color 1</span>
              <input
                type="color"
                value={patternOverlayColor1}
                onChange={(event) => setPatternOverlayColor1(event.target.value)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Color 2</span>
              <input
                type="color"
                value={patternOverlayColor2}
                onChange={(event) => setPatternOverlayColor2(event.target.value)}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Opacity
                <span className="control__value">{patternOverlayOpacity}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={patternOverlayOpacity}
                onChange={(event) => setPatternOverlayOpacity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowPatternOverlayDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyPatternOverlay} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showBevelEmbossDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowBevelEmbossDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Bevel & Emboss"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Layer &gt; Layer Style &gt; Bevel &amp; Emboss</h2>
            <label className="control">
              <span className="control__label">
                Size
                <span className="control__value">{bevelEmbossSize}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={250}
                value={bevelEmbossSize}
                onChange={(event) => setBevelEmbossSize(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Light Direction</span>
              <select
                value={bevelEmbossLightDirection}
                onChange={(event) => setBevelEmbossLightDirection(Number(event.target.value))}
              >
                <option value={0}>Top</option>
                <option value={1}>Top Right</option>
                <option value={2}>Right</option>
                <option value={3}>Bottom Right</option>
                <option value={4}>Bottom</option>
                <option value={5}>Bottom Left</option>
                <option value={6}>Left</option>
                <option value={7}>Top Left</option>
              </select>
            </label>
            <label className="control">
              <span className="control__label">
                Strength
                <span className="control__value">{bevelEmbossStrength}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={bevelEmbossStrength}
                onChange={(event) => setBevelEmbossStrength(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowBevelEmbossDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyBevelEmboss} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showContourDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowContourDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Contour"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Layer &gt; Layer Style &gt; Contour</h2>
            <label className="control">
              <span className="control__label">
                Size
                <span className="control__value">{contourSize}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={250}
                value={contourSize}
                onChange={(event) => setContourSize(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Light Direction</span>
              <select
                value={contourLightDirection}
                onChange={(event) => setContourLightDirection(Number(event.target.value))}
              >
                <option value={0}>Top</option>
                <option value={1}>Top Right</option>
                <option value={2}>Right</option>
                <option value={3}>Bottom Right</option>
                <option value={4}>Bottom</option>
                <option value={5}>Bottom Left</option>
                <option value={6}>Left</option>
                <option value={7}>Top Left</option>
              </select>
            </label>
            <label className="control">
              <span className="control__label">
                Strength
                <span className="control__value">{contourStrength}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={contourStrength}
                onChange={(event) => setContourStrength(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowContourDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyContour} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showTextureDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowTextureDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Texture"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Layer &gt; Layer Style &gt; Texture</h2>
            <label className="control">
              <span className="control__label">
                Size
                <span className="control__value">{textureSize}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={250}
                value={textureSize}
                onChange={(event) => setTextureSize(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Light Direction</span>
              <select
                value={textureLightDirection}
                onChange={(event) => setTextureLightDirection(Number(event.target.value))}
              >
                <option value={0}>Top</option>
                <option value={1}>Top Right</option>
                <option value={2}>Right</option>
                <option value={3}>Bottom Right</option>
                <option value={4}>Bottom</option>
                <option value={5}>Bottom Left</option>
                <option value={6}>Left</option>
                <option value={7}>Top Left</option>
              </select>
            </label>
            <label className="control">
              <span className="control__label">
                Strength
                <span className="control__value">{textureStrength}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={textureStrength}
                onChange={(event) => setTextureStrength(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Scale
                <span className="control__value">{textureScale}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={250}
                value={textureScale}
                onChange={(event) => setTextureScale(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Depth
                <span className="control__value">{textureDepth}</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={textureDepth}
                onChange={(event) => setTextureDepth(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowTextureDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyTexture} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showTexturizerDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowTexturizerDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Texturizer"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Texture &gt; Texturizer</h2>
            <label className="control">
              <span className="control__label">
                Scale
                <span className="control__value">{texturizerScale}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={250}
                value={texturizerScale}
                onChange={(event) => setTexturizerScale(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Relief
                <span className="control__value">{texturizerRelief}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={texturizerRelief}
                onChange={(event) => setTexturizerRelief(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Light Direction</span>
              <select
                value={texturizerLightDirection}
                onChange={(event) => setTexturizerLightDirection(Number(event.target.value))}
              >
                <option value={0}>Top</option>
                <option value={1}>Top Right</option>
                <option value={2}>Right</option>
                <option value={3}>Bottom Right</option>
                <option value={4}>Bottom</option>
                <option value={5}>Bottom Left</option>
                <option value={6}>Left</option>
                <option value={7}>Top Left</option>
              </select>
            </label>
            <label className="control control--row">
              <span className="control__label">Invert</span>
              <input
                type="checkbox"
                checked={texturizerInvert}
                onChange={(event) => setTexturizerInvert(event.target.checked)}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowTexturizerDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyTexturizer} disabled={busy}>
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

      {showSmartSharpenDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowSmartSharpenDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Smart Sharpen"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Sharpen &gt; Smart Sharpen</h2>
            <label className="control">
              <span className="control__label">
                Amount
                <span className="control__value">{smartSharpenAmount}%</span>
              </span>
              <input
                type="range"
                min={1}
                max={500}
                value={smartSharpenAmount}
                onChange={(event) => setSmartSharpenAmount(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Radius
                <span className="control__value">{smartSharpenRadius}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={40}
                value={smartSharpenRadius}
                onChange={(event) => setSmartSharpenRadius(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Reduce Noise
                <span className="control__value">{smartSharpenReduceNoise}</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={smartSharpenReduceNoise}
                onChange={(event) => setSmartSharpenReduceNoise(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowSmartSharpenDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applySmartSharpen} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showReduceNoiseDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowReduceNoiseDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Reduce Noise"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Noise &gt; Reduce Noise</h2>
            <label className="control">
              <span className="control__label">
                Strength
                <span className="control__value">{reduceNoiseStrength}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={reduceNoiseStrength}
                onChange={(event) => setReduceNoiseStrength(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Preserve Details
                <span className="control__value">{reduceNoisePreserveDetails}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={reduceNoisePreserveDetails}
                onChange={(event) => setReduceNoisePreserveDetails(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowReduceNoiseDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyReduceNoise} disabled={busy}>
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

      {showExtrudeDialog && (
        <div className="modal-overlay" onClick={() => setShowExtrudeDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Extrude"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Stylize &gt; Extrude</h2>
            <label className="control">
              <span className="control__label">
                Size
                <span className="control__value">{extrudeSize}px</span>
              </span>
              <input
                type="range"
                min={2}
                max={255}
                value={extrudeSize}
                onChange={(event) => setExtrudeSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Depth
                <span className="control__value">{extrudeDepth}</span>
              </span>
              <input
                type="range"
                min={1}
                max={255}
                value={extrudeDepth}
                onChange={(event) => setExtrudeDepth(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Level-based</span>
              <input
                type="radio"
                name="extrude-depth-basis"
                checked={!extrudeRandom}
                onChange={() => setExtrudeRandom(false)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Random</span>
              <input
                type="radio"
                name="extrude-depth-basis"
                checked={extrudeRandom}
                onChange={() => setExtrudeRandom(true)}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowExtrudeDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyExtrude} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showWindDialog && (
        <div className="modal-overlay" onClick={() => setShowWindDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Wind"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Stylize &gt; Wind</h2>
            <label className="control control--row">
              <span className="control__label">Wind</span>
              <input
                type="radio"
                name="wind-method"
                checked={windMethod === 0}
                onChange={() => setWindMethod(0)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Blast</span>
              <input
                type="radio"
                name="wind-method"
                checked={windMethod === 1}
                onChange={() => setWindMethod(1)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Stagger</span>
              <input
                type="radio"
                name="wind-method"
                checked={windMethod === 2}
                onChange={() => setWindMethod(2)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">From the Right</span>
              <input
                type="radio"
                name="wind-direction"
                checked={windDirection === 0}
                onChange={() => setWindDirection(0)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">From the Left</span>
              <input
                type="radio"
                name="wind-direction"
                checked={windDirection === 1}
                onChange={() => setWindDirection(1)}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowWindDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyWind} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showTilesDialog && (
        <div className="modal-overlay" onClick={() => setShowTilesDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Tiles"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Stylize &gt; Tiles</h2>
            <label className="control">
              <span className="control__label">
                Tile Size
                <span className="control__value">{tilesTileSize}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={15}
                value={tilesTileSize}
                onChange={(event) => setTilesTileSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Maximum Offset
                <span className="control__value">{tilesMaxOffset}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={99}
                value={tilesMaxOffset}
                onChange={(event) => setTilesMaxOffset(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowTilesDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyTiles} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showGrainDialog && (
        <div className="modal-overlay" onClick={() => setShowGrainDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Grain"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Texture &gt; Grain</h2>
            <label className="control">
              <span className="control__label">
                Intensity
                <span className="control__value">{grainIntensity}</span>
              </span>
              <input
                type="range"
                min={0}
                max={40}
                value={grainIntensity}
                onChange={(event) => setGrainIntensity(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Contrast
                <span className="control__value">{grainContrast}</span>
              </span>
              <input
                type="range"
                min={0}
                max={40}
                value={grainContrast}
                onChange={(event) => setGrainContrast(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowGrainDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyGrain} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showMosaicTilesDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowMosaicTilesDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Mosaic Tiles"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Texture &gt; Mosaic Tiles</h2>
            <label className="control">
              <span className="control__label">
                Tile Size
                <span className="control__value">{mosaicTilesTileSize}px</span>
              </span>
              <input
                type="range"
                min={2}
                max={100}
                value={mosaicTilesTileSize}
                onChange={(event) => setMosaicTilesTileSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Grout Width
                <span className="control__value">{mosaicTilesGroutWidth}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={15}
                value={mosaicTilesGroutWidth}
                onChange={(event) => setMosaicTilesGroutWidth(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Lighten Grout
                <span className="control__value">{mosaicTilesLightenGrout}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={mosaicTilesLightenGrout}
                onChange={(event) => setMosaicTilesLightenGrout(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowMosaicTilesDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyMosaicTiles} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showPatchworkDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowPatchworkDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Patchwork"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Texture &gt; Patchwork</h2>
            <label className="control">
              <span className="control__label">
                Square Size
                <span className="control__value">{patchworkSquareSize}px</span>
              </span>
              <input
                type="range"
                min={2}
                max={100}
                value={patchworkSquareSize}
                onChange={(event) => setPatchworkSquareSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Relief
                <span className="control__value">{patchworkRelief}</span>
              </span>
              <input
                type="range"
                min={0}
                max={25}
                value={patchworkRelief}
                onChange={(event) => setPatchworkRelief(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowPatchworkDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyPatchwork} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showStainedGlassDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowStainedGlassDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Stained Glass"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Texture &gt; Stained Glass</h2>
            <label className="control">
              <span className="control__label">
                Cell Size
                <span className="control__value">{stainedGlassCellSize}px</span>
              </span>
              <input
                type="range"
                min={2}
                max={50}
                value={stainedGlassCellSize}
                onChange={(event) => setStainedGlassCellSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Border Thickness
                <span className="control__value">{stainedGlassBorderThickness}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={20}
                value={stainedGlassBorderThickness}
                onChange={(event) => setStainedGlassBorderThickness(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Light Intensity
                <span className="control__value">{stainedGlassLightIntensity}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={stainedGlassLightIntensity}
                onChange={(event) => setStainedGlassLightIntensity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowStainedGlassDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyStainedGlass} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showCraquelureDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowCraquelureDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Craquelure"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Texture &gt; Craquelure</h2>
            <label className="control">
              <span className="control__label">
                Crack Spacing
                <span className="control__value">{craquelureCrackSpacing}px</span>
              </span>
              <input
                type="range"
                min={2}
                max={100}
                value={craquelureCrackSpacing}
                onChange={(event) => setCraquelureCrackSpacing(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Crack Depth
                <span className="control__value">{craquelureCrackDepth}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={craquelureCrackDepth}
                onChange={(event) => setCraquelureCrackDepth(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Crack Brightness
                <span className="control__value">{craquelureCrackBrightness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={craquelureCrackBrightness}
                onChange={(event) => setCraquelureCrackBrightness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowCraquelureDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyCraquelure} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showColoredPencilDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowColoredPencilDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Colored Pencil"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Colored Pencil</h2>
            <label className="control">
              <span className="control__label">
                Pencil Width
                <span className="control__value">{coloredPencilWidth}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={24}
                value={coloredPencilWidth}
                onChange={(event) => setColoredPencilWidth(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Stroke Pressure
                <span className="control__value">{coloredPencilPressure}</span>
              </span>
              <input
                type="range"
                min={0}
                max={15}
                value={coloredPencilPressure}
                onChange={(event) => setColoredPencilPressure(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Paper Brightness
                <span className="control__value">{coloredPencilPaper}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={coloredPencilPaper}
                onChange={(event) => setColoredPencilPaper(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowColoredPencilDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyColoredPencil} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showCutoutDialog && (
        <div className="modal-overlay" onClick={() => setShowCutoutDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Cutout"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Cutout</h2>
            <label className="control">
              <span className="control__label">
                Number of Levels
                <span className="control__value">{cutoutLevels}</span>
              </span>
              <input
                type="range"
                min={2}
                max={8}
                value={cutoutLevels}
                onChange={(event) => setCutoutLevels(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Edge Simplicity
                <span className="control__value">{cutoutSimplicity}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={cutoutSimplicity}
                onChange={(event) => setCutoutSimplicity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowCutoutDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyCutout} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showDryBrushDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowDryBrushDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Dry Brush"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Dry Brush</h2>
            <label className="control">
              <span className="control__label">
                Brush Size
                <span className="control__value">{dryBrushSize}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={dryBrushSize}
                onChange={(event) => setDryBrushSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Brush Detail
                <span className="control__value">{dryBrushDetail}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={dryBrushDetail}
                onChange={(event) => setDryBrushDetail(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowDryBrushDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyDryBrush} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showFilmGrainDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowFilmGrainDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Film Grain"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Film Grain</h2>
            <label className="control">
              <span className="control__label">
                Grain
                <span className="control__value">{filmGrainAmount}</span>
              </span>
              <input
                type="range"
                min={0}
                max={20}
                value={filmGrainAmount}
                onChange={(event) => setFilmGrainAmount(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Highlight Area
                <span className="control__value">{filmGrainHighlightArea}</span>
              </span>
              <input
                type="range"
                min={0}
                max={20}
                value={filmGrainHighlightArea}
                onChange={(event) => setFilmGrainHighlightArea(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Intensity
                <span className="control__value">{filmGrainIntensity}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={filmGrainIntensity}
                onChange={(event) => setFilmGrainIntensity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowFilmGrainDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyFilmGrain} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showNeonGlowDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowNeonGlowDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Neon Glow"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Neon Glow</h2>
            <label className="control">
              <span className="control__label">
                Glow Size
                <span className="control__value">{neonGlowSize}</span>
              </span>
              <input
                type="range"
                min={0}
                max={24}
                value={neonGlowSize}
                onChange={(event) => setNeonGlowSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Glow Brightness
                <span className="control__value">{neonGlowBrightness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={neonGlowBrightness}
                onChange={(event) => setNeonGlowBrightness(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Glow Color</span>
              <input
                type="color"
                className="tools__color"
                value={neonGlowColor}
                onChange={(event) => setNeonGlowColor(event.target.value)}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowNeonGlowDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyNeonGlow} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showPosterEdgesDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowPosterEdgesDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Poster Edges"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Poster Edges</h2>
            <label className="control">
              <span className="control__label">
                Edge Thickness
                <span className="control__value">{posterEdgesThickness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={posterEdgesThickness}
                onChange={(event) => setPosterEdgesThickness(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Edge Intensity
                <span className="control__value">{posterEdgesIntensity}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={posterEdgesIntensity}
                onChange={(event) => setPosterEdgesIntensity(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Posterization
                <span className="control__value">{posterEdgesLevels}</span>
              </span>
              <input
                type="range"
                min={2}
                max={6}
                value={posterEdgesLevels}
                onChange={(event) => setPosterEdgesLevels(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowPosterEdgesDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyPosterEdges} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showSpongeDialog && (
        <div className="modal-overlay" onClick={() => setShowSpongeDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Sponge"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Sponge</h2>
            <label className="control">
              <span className="control__label">
                Brush Size
                <span className="control__value">{spongeBrushSize}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={spongeBrushSize}
                onChange={(event) => setSpongeBrushSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Definition
                <span className="control__value">{spongeDefinition}</span>
              </span>
              <input
                type="range"
                min={0}
                max={25}
                value={spongeDefinition}
                onChange={(event) => setSpongeDefinition(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowSpongeDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applySponge} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showWatercolorDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowWatercolorDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Watercolor"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Watercolor</h2>
            <label className="control">
              <span className="control__label">
                Brush Detail
                <span className="control__value">{watercolorBrushDetail}</span>
              </span>
              <input
                type="range"
                min={1}
                max={14}
                value={watercolorBrushDetail}
                onChange={(event) => setWatercolorBrushDetail(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Shadow Intensity
                <span className="control__value">{watercolorShadowIntensity}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={watercolorShadowIntensity}
                onChange={(event) => setWatercolorShadowIntensity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowWatercolorDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyWatercolor} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showSmudgeStickDialog && (
        <div className="modal-overlay" onClick={() => setShowSmudgeStickDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Smudge Stick"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Smudge Stick</h2>
            <label className="control">
              <span className="control__label">
                Stroke Length
                <span className="control__value">{smudgeStickStrokeLength}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={smudgeStickStrokeLength}
                onChange={(event) => setSmudgeStickStrokeLength(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Highlight Area
                <span className="control__value">{smudgeStickHighlightArea}</span>
              </span>
              <input
                type="range"
                min={0}
                max={20}
                value={smudgeStickHighlightArea}
                onChange={(event) => setSmudgeStickHighlightArea(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Intensity
                <span className="control__value">{smudgeStickIntensity}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={smudgeStickIntensity}
                onChange={(event) => setSmudgeStickIntensity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowSmudgeStickDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applySmudgeStick} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showPaintDaubsDialog && (
        <div className="modal-overlay" onClick={() => setShowPaintDaubsDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Paint Daubs"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Paint Daubs</h2>
            <label className="control">
              <span className="control__label">
                Brush Size
                <span className="control__value">{paintDaubsBrushSize}</span>
              </span>
              <input
                type="range"
                min={1}
                max={50}
                value={paintDaubsBrushSize}
                onChange={(event) => setPaintDaubsBrushSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Sharpness
                <span className="control__value">{paintDaubsSharpness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={40}
                value={paintDaubsSharpness}
                onChange={(event) => setPaintDaubsSharpness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowPaintDaubsDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyPaintDaubs} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showPaletteKnifeDialog && (
        <div className="modal-overlay" onClick={() => setShowPaletteKnifeDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Palette Knife"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Palette Knife</h2>
            <label className="control">
              <span className="control__label">
                Stroke Size
                <span className="control__value">{paletteKnifeStrokeSize}</span>
              </span>
              <input
                type="range"
                min={1}
                max={50}
                value={paletteKnifeStrokeSize}
                onChange={(event) => setPaletteKnifeStrokeSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Stroke Detail
                <span className="control__value">{paletteKnifeStrokeDetail}</span>
              </span>
              <input
                type="range"
                min={1}
                max={3}
                value={paletteKnifeStrokeDetail}
                onChange={(event) => setPaletteKnifeStrokeDetail(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Softness
                <span className="control__value">{paletteKnifeSoftness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={paletteKnifeSoftness}
                onChange={(event) => setPaletteKnifeSoftness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowPaletteKnifeDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyPaletteKnife} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showPlasticWrapDialog && (
        <div className="modal-overlay" onClick={() => setShowPlasticWrapDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Plastic Wrap"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Plastic Wrap</h2>
            <label className="control">
              <span className="control__label">
                Highlight Strength
                <span className="control__value">{plasticWrapHighlightStrength}</span>
              </span>
              <input
                type="range"
                min={0}
                max={20}
                value={plasticWrapHighlightStrength}
                onChange={(event) => setPlasticWrapHighlightStrength(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Detail
                <span className="control__value">{plasticWrapDetail}</span>
              </span>
              <input
                type="range"
                min={0}
                max={15}
                value={plasticWrapDetail}
                onChange={(event) => setPlasticWrapDetail(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Smoothness
                <span className="control__value">{plasticWrapSmoothness}</span>
              </span>
              <input
                type="range"
                min={1}
                max={15}
                value={plasticWrapSmoothness}
                onChange={(event) => setPlasticWrapSmoothness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowPlasticWrapDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyPlasticWrap} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showFrescoDialog && (
        <div className="modal-overlay" onClick={() => setShowFrescoDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Fresco"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Fresco</h2>
            <label className="control">
              <span className="control__label">
                Brush Size
                <span className="control__value">{frescoBrushSize}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={frescoBrushSize}
                onChange={(event) => setFrescoBrushSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Brush Detail
                <span className="control__value">{frescoBrushDetail}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={frescoBrushDetail}
                onChange={(event) => setFrescoBrushDetail(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Texture
                <span className="control__value">{frescoTexture}</span>
              </span>
              <input
                type="range"
                min={1}
                max={3}
                value={frescoTexture}
                onChange={(event) => setFrescoTexture(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowFrescoDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyFresco} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showRoughPastelsDialog && (
        <div className="modal-overlay" onClick={() => setShowRoughPastelsDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Rough Pastels"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Rough Pastels</h2>
            <label className="control">
              <span className="control__label">
                Stroke Length
                <span className="control__value">{roughPastelsStrokeLength}</span>
              </span>
              <input
                type="range"
                min={0}
                max={40}
                value={roughPastelsStrokeLength}
                onChange={(event) => setRoughPastelsStrokeLength(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Stroke Detail
                <span className="control__value">{roughPastelsStrokeDetail}</span>
              </span>
              <input
                type="range"
                min={1}
                max={20}
                value={roughPastelsStrokeDetail}
                onChange={(event) => setRoughPastelsStrokeDetail(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Relief
                <span className="control__value">{roughPastelsRelief}</span>
              </span>
              <input
                type="range"
                min={0}
                max={40}
                value={roughPastelsRelief}
                onChange={(event) => setRoughPastelsRelief(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowRoughPastelsDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyRoughPastels} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showUnderpaintingDialog && (
        <div className="modal-overlay" onClick={() => setShowUnderpaintingDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Underpainting"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Artistic &gt; Underpainting</h2>
            <label className="control">
              <span className="control__label">
                Brush Size
                <span className="control__value">{underpaintingBrushSize}</span>
              </span>
              <input
                type="range"
                min={0}
                max={40}
                value={underpaintingBrushSize}
                onChange={(event) => setUnderpaintingBrushSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Texture Coverage
                <span className="control__value">{underpaintingTextureCoverage}</span>
              </span>
              <input
                type="range"
                min={0}
                max={40}
                value={underpaintingTextureCoverage}
                onChange={(event) => setUnderpaintingTextureCoverage(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowUnderpaintingDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyUnderpainting} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showStampDialog && (
        <div className="modal-overlay" onClick={() => setShowStampDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Stamp"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Sketch &gt; Stamp</h2>
            <label className="control">
              <span className="control__label">
                Light/Dark Balance
                <span className="control__value">{stampLightDarkBalance}</span>
              </span>
              <input
                type="range"
                min={0}
                max={25}
                value={stampLightDarkBalance}
                onChange={(event) => setStampLightDarkBalance(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Smoothness
                <span className="control__value">{stampSmoothness}</span>
              </span>
              <input
                type="range"
                min={1}
                max={25}
                value={stampSmoothness}
                onChange={(event) => setStampSmoothness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowStampDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyStamp} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showPhotocopyDialog && (
        <div className="modal-overlay" onClick={() => setShowPhotocopyDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Photocopy"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Sketch &gt; Photocopy</h2>
            <label className="control">
              <span className="control__label">
                Detail
                <span className="control__value">{photocopyDetail}</span>
              </span>
              <input
                type="range"
                min={0}
                max={24}
                value={photocopyDetail}
                onChange={(event) => setPhotocopyDetail(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Darkness
                <span className="control__value">{photocopyDarkness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={photocopyDarkness}
                onChange={(event) => setPhotocopyDarkness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowPhotocopyDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyPhotocopy} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showReticulationDialog && (
        <div className="modal-overlay" onClick={() => setShowReticulationDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Reticulation"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Sketch &gt; Reticulation</h2>
            <label className="control">
              <span className="control__label">
                Density
                <span className="control__value">{reticulationDensity}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={reticulationDensity}
                onChange={(event) => setReticulationDensity(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Foreground Level
                <span className="control__value">{reticulationForegroundLevel}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={reticulationForegroundLevel}
                onChange={(event) => setReticulationForegroundLevel(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Background Level
                <span className="control__value">{reticulationBackgroundLevel}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={reticulationBackgroundLevel}
                onChange={(event) => setReticulationBackgroundLevel(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowReticulationDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyReticulation} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showNotePaperDialog && (
        <div className="modal-overlay" onClick={() => setShowNotePaperDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Note Paper"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Sketch &gt; Note Paper</h2>
            <label className="control">
              <span className="control__label">
                Image Balance
                <span className="control__value">{notePaperImageBalance}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={notePaperImageBalance}
                onChange={(event) => setNotePaperImageBalance(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Graininess
                <span className="control__value">{notePaperGraininess}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={notePaperGraininess}
                onChange={(event) => setNotePaperGraininess(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowNotePaperDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyNotePaper} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showGraphicPenDialog && (
        <div className="modal-overlay" onClick={() => setShowGraphicPenDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Graphic Pen"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Sketch &gt; Graphic Pen</h2>
            <label className="control">
              <span className="control__label">
                Stroke Length
                <span className="control__value">{graphicPenStrokeLength}</span>
              </span>
              <input
                type="range"
                min={0}
                max={15}
                value={graphicPenStrokeLength}
                onChange={(event) => setGraphicPenStrokeLength(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Light/Dark Balance
                <span className="control__value">{graphicPenLightDarkBalance}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={graphicPenLightDarkBalance}
                onChange={(event) => setGraphicPenLightDarkBalance(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Stroke Direction</span>
              <select
                value={graphicPenDirection}
                onChange={(event) => setGraphicPenDirection(Number(event.target.value))}
              >
                <option value={0}>Right Diagonal</option>
                <option value={1}>Horizontal</option>
                <option value={2}>Left Diagonal</option>
                <option value={3}>Vertical</option>
              </select>
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowGraphicPenDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyGraphicPen} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showChalkAndCharcoalDialog && (
        <div className="modal-overlay" onClick={() => setShowChalkAndCharcoalDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Chalk & Charcoal"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Sketch &gt; Chalk &amp; Charcoal</h2>
            <label className="control">
              <span className="control__label">
                Charcoal Area
                <span className="control__value">{chalkAndCharcoalCharcoalArea}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={chalkAndCharcoalCharcoalArea}
                onChange={(event) => setChalkAndCharcoalCharcoalArea(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Chalk Area
                <span className="control__value">{chalkAndCharcoalChalkArea}</span>
              </span>
              <input
                type="range"
                min={0}
                max={20}
                value={chalkAndCharcoalChalkArea}
                onChange={(event) => setChalkAndCharcoalChalkArea(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Stroke Pressure
                <span className="control__value">{chalkAndCharcoalStrokePressure}</span>
              </span>
              <input
                type="range"
                min={0}
                max={5}
                value={chalkAndCharcoalStrokePressure}
                onChange={(event) => setChalkAndCharcoalStrokePressure(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowChalkAndCharcoalDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyChalkAndCharcoal} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showPlasterDialog && (
        <div className="modal-overlay" onClick={() => setShowPlasterDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Plaster"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Sketch &gt; Plaster</h2>
            <label className="control">
              <span className="control__label">
                Image Balance
                <span className="control__value">{plasterImageBalance}</span>
              </span>
              <input
                type="range"
                min={0}
                max={40}
                value={plasterImageBalance}
                onChange={(event) => setPlasterImageBalance(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Smoothness
                <span className="control__value">{plasterSmoothness}</span>
              </span>
              <input
                type="range"
                min={1}
                max={15}
                value={plasterSmoothness}
                onChange={(event) => setPlasterSmoothness(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Light Direction</span>
              <select
                value={plasterLightDirection}
                onChange={(event) => setPlasterLightDirection(Number(event.target.value))}
              >
                <option value={0}>Top</option>
                <option value={1}>Top Right</option>
                <option value={2}>Right</option>
                <option value={3}>Bottom Right</option>
                <option value={4}>Bottom</option>
                <option value={5}>Bottom Left</option>
                <option value={6}>Left</option>
                <option value={7}>Top Left</option>
              </select>
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowPlasterDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyPlaster} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showWaterPaperDialog && (
        <div className="modal-overlay" onClick={() => setShowWaterPaperDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Water Paper"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Sketch &gt; Water Paper</h2>
            <label className="control">
              <span className="control__label">
                Fiber Length
                <span className="control__value">{waterPaperFiberLength}</span>
              </span>
              <input
                type="range"
                min={3}
                max={50}
                value={waterPaperFiberLength}
                onChange={(event) => setWaterPaperFiberLength(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Brightness
                <span className="control__value">{waterPaperBrightness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={waterPaperBrightness}
                onChange={(event) => setWaterPaperBrightness(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Contrast
                <span className="control__value">{waterPaperContrast}</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={waterPaperContrast}
                onChange={(event) => setWaterPaperContrast(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowWaterPaperDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyWaterPaper} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showTornEdgesDialog && (
        <div className="modal-overlay" onClick={() => setShowTornEdgesDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Torn Edges"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Sketch &gt; Torn Edges</h2>
            <label className="control">
              <span className="control__label">
                Image Balance
                <span className="control__value">{tornEdgesImageBalance}</span>
              </span>
              <input
                type="range"
                min={0}
                max={25}
                value={tornEdgesImageBalance}
                onChange={(event) => setTornEdgesImageBalance(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Smoothness
                <span className="control__value">{tornEdgesSmoothness}</span>
              </span>
              <input
                type="range"
                min={1}
                max={15}
                value={tornEdgesSmoothness}
                onChange={(event) => setTornEdgesSmoothness(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Contrast
                <span className="control__value">{tornEdgesContrast}</span>
              </span>
              <input
                type="range"
                min={1}
                max={25}
                value={tornEdgesContrast}
                onChange={(event) => setTornEdgesContrast(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowTornEdgesDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyTornEdges} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showBasReliefDialog && (
        <div className="modal-overlay" onClick={() => setShowBasReliefDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Bas Relief"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Sketch &gt; Bas Relief</h2>
            <label className="control">
              <span className="control__label">
                Detail
                <span className="control__value">{basReliefDetail}</span>
              </span>
              <input
                type="range"
                min={0}
                max={15}
                value={basReliefDetail}
                onChange={(event) => setBasReliefDetail(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Smoothness
                <span className="control__value">{basReliefSmoothness}</span>
              </span>
              <input
                type="range"
                min={1}
                max={15}
                value={basReliefSmoothness}
                onChange={(event) => setBasReliefSmoothness(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Light Direction</span>
              <select
                value={basReliefLightDirection}
                onChange={(event) => setBasReliefLightDirection(Number(event.target.value))}
              >
                <option value={0}>Top</option>
                <option value={1}>Top Right</option>
                <option value={2}>Right</option>
                <option value={3}>Bottom Right</option>
                <option value={4}>Bottom</option>
                <option value={5}>Bottom Left</option>
                <option value={6}>Left</option>
                <option value={7}>Top Left</option>
              </select>
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowBasReliefDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyBasRelief} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showHalftonePatternDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowHalftonePatternDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Halftone Pattern"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Sketch &gt; Halftone Pattern</h2>
            <label className="control">
              <span className="control__label">
                Size
                <span className="control__value">{halftonePatternSize}</span>
              </span>
              <input
                type="range"
                min={1}
                max={12}
                value={halftonePatternSize}
                onChange={(event) => setHalftonePatternSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Contrast
                <span className="control__value">{halftonePatternContrast}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={halftonePatternContrast}
                onChange={(event) => setHalftonePatternContrast(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Pattern Type</span>
              <select
                value={halftonePatternType}
                onChange={(event) => setHalftonePatternType(Number(event.target.value))}
              >
                <option value={0}>Line</option>
                <option value={1}>Dot</option>
              </select>
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowHalftonePatternDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyHalftonePattern} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showChromeDialog && (
        <div className="modal-overlay" onClick={() => setShowChromeDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Chrome"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Sketch &gt; Chrome</h2>
            <label className="control">
              <span className="control__label">
                Detail
                <span className="control__value">{chromeDetail}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={chromeDetail}
                onChange={(event) => setChromeDetail(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Smoothness
                <span className="control__value">{chromeSmoothness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={chromeSmoothness}
                onChange={(event) => setChromeSmoothness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowChromeDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyChrome} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showDarkStrokesDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowDarkStrokesDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Dark Strokes"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Brush Strokes &gt; Dark Strokes</h2>
            <label className="control">
              <span className="control__label">
                Balance
                <span className="control__value">{darkStrokesBalance}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={darkStrokesBalance}
                onChange={(event) => setDarkStrokesBalance(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Black Intensity
                <span className="control__value">{darkStrokesBlackIntensity}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={darkStrokesBlackIntensity}
                onChange={(event) => setDarkStrokesBlackIntensity(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                White Intensity
                <span className="control__value">{darkStrokesWhiteIntensity}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={darkStrokesWhiteIntensity}
                onChange={(event) => setDarkStrokesWhiteIntensity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowDarkStrokesDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyDarkStrokes} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showInkOutlinesDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowInkOutlinesDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Ink Outlines"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Brush Strokes &gt; Ink Outlines</h2>
            <label className="control">
              <span className="control__label">
                Stroke Length
                <span className="control__value">{inkOutlinesStrokeLength}</span>
              </span>
              <input
                type="range"
                min={1}
                max={50}
                value={inkOutlinesStrokeLength}
                onChange={(event) => setInkOutlinesStrokeLength(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Dark Intensity
                <span className="control__value">{inkOutlinesDarkIntensity}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={inkOutlinesDarkIntensity}
                onChange={(event) => setInkOutlinesDarkIntensity(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Light Intensity
                <span className="control__value">{inkOutlinesLightIntensity}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={inkOutlinesLightIntensity}
                onChange={(event) => setInkOutlinesLightIntensity(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowInkOutlinesDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyInkOutlines} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showSpatterDialog && (
        <div className="modal-overlay" onClick={() => setShowSpatterDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Spatter"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Brush Strokes &gt; Spatter</h2>
            <label className="control">
              <span className="control__label">
                Spray Radius
                <span className="control__value">{spatterSprayRadius}</span>
              </span>
              <input
                type="range"
                min={0}
                max={25}
                value={spatterSprayRadius}
                onChange={(event) => setSpatterSprayRadius(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Smoothness
                <span className="control__value">{spatterSmoothness}</span>
              </span>
              <input
                type="range"
                min={1}
                max={15}
                value={spatterSmoothness}
                onChange={(event) => setSpatterSmoothness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowSpatterDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applySpatter} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showCrosshatchDialog && (
        <div className="modal-overlay" onClick={() => setShowCrosshatchDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Crosshatch"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Brush Strokes &gt; Crosshatch</h2>
            <label className="control">
              <span className="control__label">
                Stroke Length
                <span className="control__value">{crosshatchStrokeLength}</span>
              </span>
              <input
                type="range"
                min={3}
                max={50}
                value={crosshatchStrokeLength}
                onChange={(event) => setCrosshatchStrokeLength(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Sharpness
                <span className="control__value">{crosshatchSharpness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={20}
                value={crosshatchSharpness}
                onChange={(event) => setCrosshatchSharpness(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Strength
                <span className="control__value">{crosshatchStrength}</span>
              </span>
              <input
                type="range"
                min={1}
                max={3}
                value={crosshatchStrength}
                onChange={(event) => setCrosshatchStrength(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowCrosshatchDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyCrosshatch} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showAccentedEdgesDialog && (
        <div className="modal-overlay" onClick={() => setShowAccentedEdgesDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Accented Edges"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Brush Strokes &gt; Accented Edges</h2>
            <label className="control">
              <span className="control__label">
                Edge Width
                <span className="control__value">{accentedEdgesWidth}</span>
              </span>
              <input
                type="range"
                min={1}
                max={14}
                value={accentedEdgesWidth}
                onChange={(event) => setAccentedEdgesWidth(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Edge Brightness
                <span className="control__value">{accentedEdgesBrightness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={50}
                value={accentedEdgesBrightness}
                onChange={(event) => setAccentedEdgesBrightness(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Smoothness
                <span className="control__value">{accentedEdgesSmoothness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={15}
                value={accentedEdgesSmoothness}
                onChange={(event) => setAccentedEdgesSmoothness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowAccentedEdgesDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyAccentedEdges} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showAngledStrokesDialog && (
        <div className="modal-overlay" onClick={() => setShowAngledStrokesDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Angled Strokes"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Brush Strokes &gt; Angled Strokes</h2>
            <label className="control">
              <span className="control__label">
                Direction Balance
                <span className="control__value">{angledStrokesDirectionBalance}</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={angledStrokesDirectionBalance}
                onChange={(event) => setAngledStrokesDirectionBalance(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Stroke Length
                <span className="control__value">{angledStrokesStrokeLength}</span>
              </span>
              <input
                type="range"
                min={3}
                max={50}
                value={angledStrokesStrokeLength}
                onChange={(event) => setAngledStrokesStrokeLength(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Sharpness
                <span className="control__value">{angledStrokesSharpness}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={angledStrokesSharpness}
                onChange={(event) => setAngledStrokesSharpness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowAngledStrokesDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyAngledStrokes} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showSprayedStrokesDialog && (
        <div className="modal-overlay" onClick={() => setShowSprayedStrokesDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Sprayed Strokes"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Brush Strokes &gt; Sprayed Strokes</h2>
            <label className="control">
              <span className="control__label">
                Stroke Length
                <span className="control__value">{sprayedStrokesLength}</span>
              </span>
              <input
                type="range"
                min={0}
                max={20}
                value={sprayedStrokesLength}
                onChange={(event) => setSprayedStrokesLength(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Spray Radius
                <span className="control__value">{sprayedStrokesRadius}</span>
              </span>
              <input
                type="range"
                min={0}
                max={25}
                value={sprayedStrokesRadius}
                onChange={(event) => setSprayedStrokesRadius(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Stroke Direction</span>
              <select
                value={sprayedStrokesDirection}
                onChange={(event) => setSprayedStrokesDirection(Number(event.target.value))}
              >
                <option value={0}>Right Diagonal</option>
                <option value={1}>Horizontal</option>
                <option value={2}>Left Diagonal</option>
                <option value={3}>Vertical</option>
              </select>
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowSprayedStrokesDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applySprayedStrokes} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showSumiEDialog && (
        <div className="modal-overlay" onClick={() => setShowSumiEDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Sumi-e"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Brush Strokes &gt; Sumi-e</h2>
            <label className="control">
              <span className="control__label">
                Stroke Width
                <span className="control__value">{sumiEStrokeWidth}</span>
              </span>
              <input
                type="range"
                min={3}
                max={15}
                value={sumiEStrokeWidth}
                onChange={(event) => setSumiEStrokeWidth(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Stroke Pressure
                <span className="control__value">{sumiEStrokePressure}</span>
              </span>
              <input
                type="range"
                min={0}
                max={15}
                value={sumiEStrokePressure}
                onChange={(event) => setSumiEStrokePressure(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Contrast
                <span className="control__value">{sumiEContrast}</span>
              </span>
              <input
                type="range"
                min={0}
                max={40}
                value={sumiEContrast}
                onChange={(event) => setSumiEContrast(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowSumiEDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applySumiE} disabled={busy}>
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

      {showShearDialog && (
        <div className="modal-overlay" onClick={() => setShowShearDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Shear"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Distort &gt; Shear</h2>
            {shearControlPoints.map((point, index) => (
              <label className="control" key={index}>
                <span className="control__label">
                  {index === 0
                    ? "Top"
                    : index === shearControlPoints.length - 1
                      ? "Bottom"
                      : `Anchor ${index}`}
                  <span className="control__value">{point}px</span>
                </span>
                <input
                  type="range"
                  min={-100}
                  max={100}
                  value={point}
                  onChange={(event) => {
                    const next = Number(event.target.value);
                    setShearControlPoints((points) =>
                      points.map((p, i) => (i === index ? next : p)),
                    );
                  }}
                />
              </label>
            ))}
            <label className="control control--row">
              <span className="control__label">Repeat Edge Pixels</span>
              <input
                type="radio"
                name="shear-undefined-areas"
                checked={!shearWrapAround}
                onChange={() => setShearWrapAround(false)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Wrap Around</span>
              <input
                type="radio"
                name="shear-undefined-areas"
                checked={shearWrapAround}
                onChange={() => setShearWrapAround(true)}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowShearDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyShear} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showDisplaceDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowDisplaceDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Displace"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Distort &gt; Displace</h2>
            <label className="control control--row">
              <span className="control__label">Displacement Map</span>
              <select
                value={displaceMapLayerId ?? ""}
                onChange={(event) => setDisplaceMapLayerId(Number(event.target.value))}
              >
                {(document?.layers ?? [])
                  .filter((layer) => layer.id !== selectedId)
                  .map((layer) => (
                    <option key={layer.id} value={layer.id}>
                      {layer.name}
                    </option>
                  ))}
              </select>
            </label>
            <label className="control">
              <span className="control__label">
                Horizontal Scale
                <span className="control__value">{displaceHorizontalScale}px</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={displaceHorizontalScale}
                onChange={(event) => setDisplaceHorizontalScale(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Vertical Scale
                <span className="control__value">{displaceVerticalScale}px</span>
              </span>
              <input
                type="range"
                min={-100}
                max={100}
                value={displaceVerticalScale}
                onChange={(event) => setDisplaceVerticalScale(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Repeat Edge Pixels</span>
              <input
                type="radio"
                name="displace-undefined-areas"
                checked={!displaceWrapAround}
                onChange={() => setDisplaceWrapAround(false)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Wrap Around</span>
              <input
                type="radio"
                name="displace-undefined-areas"
                checked={displaceWrapAround}
                onChange={() => setDisplaceWrapAround(true)}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowDisplaceDialog(false)}
              >
                Cancel
              </button>
              <button
                className="button"
                onClick={applyDisplace}
                disabled={busy || displaceMapLayerId === null}
              >
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showDiffuseGlowDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowDiffuseGlowDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Diffuse Glow"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Distort &gt; Diffuse Glow</h2>
            <label className="control">
              <span className="control__label">
                Graininess
                <span className="control__value">{diffuseGlowGraininess}</span>
              </span>
              <input
                type="range"
                min={0}
                max={10}
                value={diffuseGlowGraininess}
                onChange={(event) => setDiffuseGlowGraininess(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Glow Amount
                <span className="control__value">{diffuseGlowGlowAmount}</span>
              </span>
              <input
                type="range"
                min={0}
                max={20}
                value={diffuseGlowGlowAmount}
                onChange={(event) => setDiffuseGlowGlowAmount(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Clear Amount
                <span className="control__value">{diffuseGlowClearAmount}</span>
              </span>
              <input
                type="range"
                min={0}
                max={20}
                value={diffuseGlowClearAmount}
                onChange={(event) => setDiffuseGlowClearAmount(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowDiffuseGlowDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyDiffuseGlow} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showGlassDialog && (
        <div className="modal-overlay" onClick={() => setShowGlassDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Glass"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Distort &gt; Glass</h2>
            <label className="control">
              <span className="control__label">
                Distortion
                <span className="control__value">{glassDistortion}</span>
              </span>
              <input
                type="range"
                min={0}
                max={20}
                value={glassDistortion}
                onChange={(event) => setGlassDistortion(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Smoothness
                <span className="control__value">{glassSmoothness}</span>
              </span>
              <input
                type="range"
                min={1}
                max={15}
                value={glassSmoothness}
                onChange={(event) => setGlassSmoothness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowGlassDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyGlass} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showOceanRippleDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowOceanRippleDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Ocean Ripple"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter Gallery &gt; Distort &gt; Ocean Ripple</h2>
            <label className="control">
              <span className="control__label">
                Ripple Size
                <span className="control__value">{oceanRippleSize}</span>
              </span>
              <input
                type="range"
                min={1}
                max={15}
                value={oceanRippleSize}
                onChange={(event) => setOceanRippleSize(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Ripple Magnitude
                <span className="control__value">{oceanRippleMagnitude}</span>
              </span>
              <input
                type="range"
                min={0}
                max={20}
                value={oceanRippleMagnitude}
                onChange={(event) => setOceanRippleMagnitude(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowOceanRippleDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyOceanRipple} disabled={busy}>
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

      {showMezzotintDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowMezzotintDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Mezzotint"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Pixelate &gt; Mezzotint</h2>
            <label className="control">
              <span className="control__label">
                Cell Size
                <span className="control__value">{mezzotintCellSize}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={64}
                value={mezzotintCellSize}
                onChange={(event) => setMezzotintCellSize(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowMezzotintDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyMezzotint} disabled={busy}>
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

      {showCloudsDialog && (
        <div className="modal-overlay" onClick={() => setShowCloudsDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Clouds"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Render &gt; Clouds</h2>
            <label className="control control--row">
              <span className="control__label">Foreground</span>
              <input
                type="color"
                value={cloudsForeground}
                onChange={(event) => setCloudsForeground(event.target.value)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Background</span>
              <input
                type="color"
                value={cloudsBackground}
                onChange={(event) => setCloudsBackground(event.target.value)}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowCloudsDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyClouds} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showDifferenceCloudsDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowDifferenceCloudsDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Difference Clouds"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Render &gt; Difference Clouds</h2>
            <label className="control control--row">
              <span className="control__label">Foreground</span>
              <input
                type="color"
                value={differenceCloudsForeground}
                onChange={(event) => setDifferenceCloudsForeground(event.target.value)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Background</span>
              <input
                type="color"
                value={differenceCloudsBackground}
                onChange={(event) => setDifferenceCloudsBackground(event.target.value)}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowDifferenceCloudsDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyDifferenceClouds} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showFibersDialog && (
        <div className="modal-overlay" onClick={() => setShowFibersDialog(false)} role="presentation">
          <div
            className="modal"
            role="dialog"
            aria-label="Fibers"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Render &gt; Fibers</h2>
            <label className="control">
              <span className="control__label">
                Variance
                <span className="control__value">{fibersVariance}</span>
              </span>
              <input
                type="range"
                min={1}
                max={100}
                value={fibersVariance}
                onChange={(event) => setFibersVariance(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Strength
                <span className="control__value">{fibersStrength}</span>
              </span>
              <input
                type="range"
                min={1}
                max={64}
                value={fibersStrength}
                onChange={(event) => setFibersStrength(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Foreground</span>
              <input
                type="color"
                value={fibersForeground}
                onChange={(event) => setFibersForeground(event.target.value)}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Background</span>
              <input
                type="color"
                value={fibersBackground}
                onChange={(event) => setFibersBackground(event.target.value)}
              />
            </label>
            <div className="modal__actions">
              <button className="button button--quiet" onClick={() => setShowFibersDialog(false)}>
                Cancel
              </button>
              <button className="button" onClick={applyFibers} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showLensFlareDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowLensFlareDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Lens Flare"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Render &gt; Lens Flare</h2>
            <label className="control">
              <span className="control__label">
                Center X
                <span className="control__value">{lensFlareCenterX}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={document?.width ?? 1}
                value={lensFlareCenterX}
                onChange={(event) => setLensFlareCenterX(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Center Y
                <span className="control__value">{lensFlareCenterY}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={document?.height ?? 1}
                value={lensFlareCenterY}
                onChange={(event) => setLensFlareCenterY(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Brightness
                <span className="control__value">{lensFlareBrightness}%</span>
              </span>
              <input
                type="range"
                min={10}
                max={300}
                value={lensFlareBrightness}
                onChange={(event) => setLensFlareBrightness(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowLensFlareDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyLensFlare} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showRadialBlurDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowRadialBlurDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Radial Blur"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Blur &gt; Radial Blur</h2>
            <label className="control">
              <span className="control__label">
                Amount
                <span className="control__value">{radialBlurAmount}</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={radialBlurAmount}
                onChange={(event) => setRadialBlurAmount(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Center X
                <span className="control__value">{radialBlurCenterX}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={document?.width ?? 1}
                value={radialBlurCenterX}
                onChange={(event) => setRadialBlurCenterX(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Center Y
                <span className="control__value">{radialBlurCenterY}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={document?.height ?? 1}
                value={radialBlurCenterY}
                onChange={(event) => setRadialBlurCenterY(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowRadialBlurDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyRadialBlur} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showTiltShiftDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowTiltShiftDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Tilt-Shift"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">
              Filter Gallery &gt; Blur Gallery &gt; Tilt-Shift
            </h2>
            <label className="control">
              <span className="control__label">
                Focus Row
                <span className="control__value">{tiltShiftFocusRow}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={document?.height ?? 1}
                value={tiltShiftFocusRow}
                onChange={(event) => setTiltShiftFocusRow(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Sharp Band Half-Height
                <span className="control__value">{tiltShiftHalfHeight}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={document?.height ?? 1}
                value={tiltShiftHalfHeight}
                onChange={(event) => setTiltShiftHalfHeight(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Blur Radius
                <span className="control__value">{tiltShiftBlurRadius}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={100}
                value={tiltShiftBlurRadius}
                onChange={(event) => setTiltShiftBlurRadius(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowTiltShiftDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyTiltShift} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showIrisBlurDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowIrisBlurDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Iris Blur"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">
              Filter Gallery &gt; Blur Gallery &gt; Iris Blur
            </h2>
            <label className="control">
              <span className="control__label">
                Center X
                <span className="control__value">{irisBlurCenterX}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={document?.width ?? 1}
                value={irisBlurCenterX}
                onChange={(event) => setIrisBlurCenterX(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Center Y
                <span className="control__value">{irisBlurCenterY}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={document?.height ?? 1}
                value={irisBlurCenterY}
                onChange={(event) => setIrisBlurCenterY(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Sharp Radius
                <span className="control__value">{irisBlurRadius}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={Math.max(document?.width ?? 1, document?.height ?? 1)}
                value={irisBlurRadius}
                onChange={(event) => setIrisBlurRadius(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Blur Radius
                <span className="control__value">{irisBlurBlurRadius}px</span>
              </span>
              <input
                type="range"
                min={1}
                max={100}
                value={irisBlurBlurRadius}
                onChange={(event) => setIrisBlurBlurRadius(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowIrisBlurDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyIrisBlur} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showFieldBlurDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowFieldBlurDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Field Blur"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">
              Filter Gallery &gt; Blur Gallery &gt; Field Blur
            </h2>
            <label className="control control--row">
              <span className="control__label">Pin 1 X / Y</span>
              <input
                type="number"
                min={0}
                max={document?.width ?? 1}
                value={fieldBlurX1}
                onChange={(event) => setFieldBlurX1(Number(event.target.value))}
              />
              <input
                type="number"
                min={0}
                max={document?.height ?? 1}
                value={fieldBlurY1}
                onChange={(event) => setFieldBlurY1(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Pin 1 Blur Radius
                <span className="control__value">{fieldBlurRadius1}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={fieldBlurRadius1}
                onChange={(event) => setFieldBlurRadius1(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Pin 2 X / Y</span>
              <input
                type="number"
                min={0}
                max={document?.width ?? 1}
                value={fieldBlurX2}
                onChange={(event) => setFieldBlurX2(Number(event.target.value))}
              />
              <input
                type="number"
                min={0}
                max={document?.height ?? 1}
                value={fieldBlurY2}
                onChange={(event) => setFieldBlurY2(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Pin 2 Blur Radius
                <span className="control__value">{fieldBlurRadius2}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={fieldBlurRadius2}
                onChange={(event) => setFieldBlurRadius2(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowFieldBlurDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyFieldBlur} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showSpinBlurDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowSpinBlurDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Spin Blur"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">
              Filter Gallery &gt; Blur Gallery &gt; Spin Blur
            </h2>
            <label className="control control--row">
              <span className="control__label">Center X / Y</span>
              <input
                type="number"
                min={0}
                max={document?.width ?? 1}
                value={spinBlurCenterX}
                onChange={(event) => setSpinBlurCenterX(Number(event.target.value))}
              />
              <input
                type="number"
                min={0}
                max={document?.height ?? 1}
                value={spinBlurCenterY}
                onChange={(event) => setSpinBlurCenterY(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Angle
                <span className="control__value">{spinBlurAngle}°</span>
              </span>
              <input
                type="range"
                min={0}
                max={360}
                value={spinBlurAngle}
                onChange={(event) => setSpinBlurAngle(Number(event.target.value))}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowSpinBlurDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applySpinBlur} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showLensBlurDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowLensBlurDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Lens Blur"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Blur &gt; Lens Blur</h2>
            <p className="modal__hint">
              The layer's own alpha channel is the depth map: opaque pixels
              blur at the full radius, transparent pixels stay sharp.
            </p>
            <label className="control">
              <span className="control__label">
                Radius
                <span className="control__value">{lensBlurRadius}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={lensBlurRadius}
                onChange={(event) => setLensBlurRadius(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <input
                type="checkbox"
                checked={lensBlurInvert}
                onChange={(event) => setLensBlurInvert(event.target.checked)}
              />
              <span className="control__label">Invert depth map</span>
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowLensBlurDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyLensBlur} disabled={busy}>
                Apply
              </button>
            </div>
          </div>
        </div>
      )}

      {showLightingEffectsDialog && (
        <div
          className="modal-overlay"
          onClick={() => setShowLightingEffectsDialog(false)}
          role="presentation"
        >
          <div
            className="modal"
            role="dialog"
            aria-label="Lighting Effects"
            onClick={(event) => event.stopPropagation()}
          >
            <h2 className="modal__heading">Filter &gt; Render &gt; Lighting Effects</h2>
            <label className="control">
              <span className="control__label">
                Light X
                <span className="control__value">{lightingLightX}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={document?.width ?? 1}
                value={lightingLightX}
                onChange={(event) => setLightingLightX(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Light Y
                <span className="control__value">{lightingLightY}px</span>
              </span>
              <input
                type="range"
                min={0}
                max={document?.height ?? 1}
                value={lightingLightY}
                onChange={(event) => setLightingLightY(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Height
                <span className="control__value">{lightingLightHeight}</span>
              </span>
              <input
                type="range"
                min={1}
                max={200}
                value={lightingLightHeight}
                onChange={(event) => setLightingLightHeight(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Intensity
                <span className="control__value">{lightingIntensity}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={lightingIntensity}
                onChange={(event) => setLightingIntensity(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Ambience
                <span className="control__value">{lightingAmbience}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={lightingAmbience}
                onChange={(event) => setLightingAmbience(Number(event.target.value))}
              />
            </label>
            <label className="control">
              <span className="control__label">
                Bump Height
                <span className="control__value">{lightingBumpHeight}%</span>
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={lightingBumpHeight}
                onChange={(event) => setLightingBumpHeight(Number(event.target.value))}
              />
            </label>
            <label className="control control--row">
              <span className="control__label">Light Color</span>
              <input
                type="color"
                className="tools__color"
                value={lightingColor}
                onChange={(event) => setLightingColor(event.target.value)}
              />
            </label>
            <div className="modal__actions">
              <button
                className="button button--quiet"
                onClick={() => setShowLightingEffectsDialog(false)}
              >
                Cancel
              </button>
              <button className="button" onClick={applyLightingEffects} disabled={busy}>
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
                  lastLevelsPixel.current = null;
                  setRgbLevels(null);
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
            {rgbLevels && (
              <span
                className="statusbar__levels"
                title="Camera Raw Filter > RGB Levels: the selected layer's own pixel under the pointer"
              >
                R {rgbLevels[0]} G {rgbLevels[1]} B {rgbLevels[2]} A {rgbLevels[3]}
              </span>
            )}
          </>
        ) : (
          <span className="statusbar__name">Ready</span>
        )}
      </footer>
    </div>
  );
}
