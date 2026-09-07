/** Mirrors `BlendMode` in src-tauri/src/blend.rs (serde camelCase). */
export type BlendMode =
  | "normal"
  | "multiply"
  | "screen"
  | "overlay"
  | "darken"
  | "lighten"
  | "colorDodge"
  | "colorBurn"
  | "hardLight"
  | "softLight"
  | "difference"
  | "exclusion";

export type BlendModeInfo = {
  mode: BlendMode;
  label: string;
};

/** Mirrors `LayerView` in src-tauri/src/document.rs. */
export type LayerView = {
  id: number;
  name: string;
  visible: boolean;
  /** 0..=1 */
  opacity: number;
  blendMode: BlendMode;
  /** Lock (image pixels): blocks paint/erase strokes onto this layer. */
  locked: boolean;
  /** Layer > Link Layers: linked layers move together under the Move tool. */
  linked: boolean;
  /** Layer > Create Clipping Mask: shows only where the layer below has pixels. */
  clipped: boolean;
  /** Whether the layer carries a layer mask. */
  hasMask: boolean;
  /** The live adjustment of an adjustment layer; `null` for a pixel layer. */
  adjustment: Adjustment | null;
  /** The recipe of a fill layer; `null` for any other layer. */
  fill: Fill | null;
};

/** Mirrors `ApplyChannel` in src-tauri/src/document.rs: Apply Image's
 * Channel list — the composite, one colour channel as a grey, or the
 * source's transparency as an opaque grey. */
export type ApplyChannel = "rgb" | "red" | "green" | "blue" | "transparency";

/** Mirrors `ApplyMask` in src-tauri/src/document.rs: Apply Image's Mask
 * group — a mask image (`null` = merged) read through one channel (RGB =
 * luma), optionally inverted. */
export type ApplyMask = { source: number | null; channel: ApplyChannel; invert: boolean };

/** Mirrors `ApplyBlend` in src-tauri/src/document.rs (serde internally
 * tagged by `kind`): Apply Image's Blending list — a layer blend mode, or
 * Add / Subtract with Scale (1–2) and Offset (−255..255). */
export type ApplyBlend =
  | { kind: "mode"; mode: BlendMode }
  | { kind: "add"; scale: number; offset: number }
  | { kind: "subtract"; scale: number; offset: number };

/** Mirrors `Fill` in src-tauri/src/document.rs (serde internally tagged
 * by `kind`): what a live fill layer paints. */
export type Fill =
  | { kind: "solidColor"; color: [number, number, number, number] }
  | {
      kind: "gradient";
      startColor: [number, number, number, number];
      endColor: [number, number, number, number];
    }
  | { kind: "pattern" };

/** Mirrors `Adjustment` in src-tauri/src/document.rs (serde internally
 * tagged by `kind`). */
export type Adjustment =
  | { kind: "invert" }
  | { kind: "brightnessContrast"; brightness: number; contrast: number }
  | { kind: "threshold"; level: number }
  | { kind: "posterize"; levels: number };

/** Mirrors `MaskSource` in src-tauri/src/document.rs. */
export type MaskSource = "revealAll" | "hideAll" | "revealSelection" | "hideSelection";

/** Mirrors `SelectionShape` in src-tauri/src/document.rs (serde camelCase).
 * `RoundedRectangle` is a struct variant, so serde's default external
 * tagging serializes it as `{ roundedRectangle: { radius } }` rather than a
 * bare string like the two unit variants. */
export type SelectionShape =
  | "rectangle"
  | "ellipse"
  | { roundedRectangle: { radius: number } }
  /** A pixel mask (Magic Wand and friends); only its bounding box is sent. */
  | "mask";

/** Mirrors `Selection` (aka `SelectionView`) in src-tauri/src/document.rs. */
export type Selection = {
  shape: SelectionShape;
  bounds: { x0: number; y0: number; x1: number; y1: number };
  /** Select > Inverse: selects everywhere *except* `shape`. */
  inverted: boolean;
  /** Select > Modify > Border: when set, only a band this many pixels wide
   * hugging the inside of `shape`'s own edge is selected. */
  border: number | null;
  /** Select > Modify > Feather: the edge's softening radius, 0 for hard. */
  feather: number;
  /** The selection tools' Anti-alias option: a supersampled edge. */
  antiAlias: boolean;
  /** Select and Mask > Contrast, 0–100. */
  contrast: number;
  /** Select and Mask > Shift Edge, −100..100. */
  shiftEdge: number;
};

/** Mirrors `RefineEdge` / `SelectAndMaskOutput` in src-tauri/src/document.rs. */
export type RefineEdge = { smooth: number; feather: number; contrast: number; shiftEdge: number };
export type SelectAndMaskOutput = "selection" | "layerMask" | "newLayer" | "newLayerWithMask";

/** Mirrors `DocumentView`. `layers` is bottom-to-top, as in the model. */
export type DocumentView = {
  width: number;
  height: number;
  layers: LayerView[];
  /** `null` when nothing is selected: no outline, every stroke unrestricted. */
  selection: Selection | null;
  /** Whether Select > Reselect has something to restore right now. */
  canReselect: boolean;
  /** Whether Edit > Transform > Again has a transform to repeat right now. */
  canTransformAgain: boolean;
  /** Whether Edit > Define Pattern has captured a pattern for fills to tile. */
  hasPattern: boolean;
  /** Names stored by Select > Save Selection, in the order first saved. */
  savedSelections: string[];
  /** The Count tool's marks, (x, y) in placement order; mark n is numbered n + 1. */
  countMarks: [number, number][];
  /** The Note tool's annotations in placement order. */
  notes: Note[];
  /** Layer Comps saved on the document, in the order first saved. */
  layerComps: string[];
  /** Ruler guides, in placement order. */
  guides: Guide[];
  /** Layer groups, in creation order; members are layer ids, bottom to top. */
  groups: LayerGroup[];
  /** Alpha channel names made by Image > Calculations, in creation order. */
  channels: string[];
  /** Image > Mode. */
  mode: ColorMode;
  /** How many colours Indexed Color's table holds; 0 in other modes. */
  colorTableSize: number;
  /** Duotone's inks; empty in other modes. */
  duotone: Ink[];
  /** Spot colour channels, in overprinting order. */
  spots: SpotChannelView[];
};

/** Mirrors `SpotChannelView` in src-tauri/src/document.rs: a spot colour
 * channel's name, screen colour, and Solidity percent. */
export type SpotChannelView = { name: string; color: [number, number, number]; solidity: number };

/** Mirrors `Ink` in src-tauri/src/document.rs: a Duotone ink's colour and
 * curve points (darkness → coverage); an empty curve is the straight line. */
export type Ink = { color: [number, number, number]; curve: [number, number][] };

/** Mirrors `ColorMode` / `BitmapMethod` / `Palette` in
 * src-tauri/src/document.rs. */
export type ColorMode =
  | "rgb"
  | "grayscale"
  | "bitmap"
  | "indexed"
  | "duotone"
  | "cmyk"
  | "lab"
  | "multichannel";
export type Palette = { kind: "exact" } | { kind: "uniform" } | { kind: "adaptive"; colors: number };
export type BitmapMethod = "threshold" | "patternDither" | "diffusionDither";

/** Mirrors `ColorSample` / `ColorRange` in src-tauri/src/document.rs:
 * Select > Color Range's Select list (serde tagged by `kind`). */
export type ColorSample = { color: [number, number, number]; position: [number, number] | null };
export type ColorRangePreset =
  | "reds"
  | "yellows"
  | "greens"
  | "cyans"
  | "blues"
  | "magentas"
  | "highlights"
  | "midtones"
  | "shadows"
  | "skinTones";
export type ColorRange =
  | { kind: "sampled"; samples: ColorSample[]; fuzziness: number; localized: number | null }
  | { kind: ColorRangePreset };

/** Mirrors `ReferencePoint` / `ContentAwareScale` in
 * src-tauri/src/document.rs: Edit > Content-Aware Scale's options bar. */
export type ReferencePoint =
  | "topLeft"
  | "top"
  | "topRight"
  | "left"
  | "center"
  | "right"
  | "bottomLeft"
  | "bottom"
  | "bottomRight";
export type ContentAwareScaleOptions = {
  widthPercent: number;
  heightPercent: number;
  amount: number;
  protect: string | null;
  protectSkin: boolean;
  reference: ReferencePoint;
  position: [number, number] | null;
};

/** Mirrors `PerspectivePlane` / `PerspectiveAuto` in
 * src-tauri/src/document.rs: Edit > Perspective Warp's planes. */
export type Quad = [number, number][];
export type PerspectivePlane = { source: Quad; target: Quad };
export type PerspectiveAuto =
  | { kind: "level" }
  | { kind: "vertical" }
  | { kind: "both" }
  | { kind: "edge"; plane: number; edge: number };

/** Mirrors `WarpMesh` / `WarpStyle` in src-tauri/src/document.rs: Edit >
 * Transform > Warp's sixteen control points, row-major, and its Warp Style
 * presets. */
export type WarpMesh = { points: [number, number][] };
export type WarpStyle =
  | "custom"
  | "arc"
  | "arcLower"
  | "arcUpper"
  | "arch"
  | "bulge"
  | "shellLower"
  | "shellUpper"
  | "flag"
  | "wave"
  | "fish"
  | "rise"
  | "fisheye"
  | "inflate"
  | "squeeze"
  | "twist";

/** Mirrors `PathBlur` in src-tauri/src/document.rs: Blur Gallery > Path
 * Blur's path, Speed, Taper, and Centered Blur. */
export type PathBlurOptions = {
  points: [number, number][];
  speed: number;
  taper: number;
  centered: boolean;
};

/** Mirrors `CameraRawMask` in src-tauri/src/document.rs: Camera Raw
 * Filter's Masking (serde tagged by `kind`). */
export type CameraRawMask =
  | { kind: "subject"; tolerance: number }
  | { kind: "radial"; x0: number; y0: number; x1: number; y1: number; feather: number; invert: boolean }
  | { kind: "colorRange"; color: [number, number, number]; fuzziness: number; invert: boolean };

/** Mirrors `PuppetWarp` and friends in src-tauri/src/document.rs: Edit >
 * Puppet Warp's mode, density, expansion, and pins, and the mesh Show Mesh
 * draws (vertices, where the pins move them, triangles as indices). */
export type PuppetMode = "rigid" | "normal" | "distort";
export type PuppetDensity = "fewer" | "normal" | "more";
export type PuppetPin = { source: [number, number]; target: [number, number]; depth: number };
export type PuppetWarpOptions = {
  mode: PuppetMode;
  density: PuppetDensity;
  expansion: number;
  pins: PuppetPin[];
};
export type PuppetMesh = {
  vertices: [number, number][];
  deformed: [number, number][];
  triangles: [number, number, number][];
};

/** Mirrors `Proof` in src-tauri/src/document.rs: View > Proof Setup >
 * Color Blindness. */
export type Proof = "protanopia" | "deuteranopia";

/** Mirrors `ChannelView` in src-tauri/src/document.rs (serde tagged by
 * `kind`): what the canvas shows — the composite, one colour channel as a
 * grey, or an alpha channel. */
export type ChannelView =
  | { kind: "composite" }
  | { kind: "red" }
  | { kind: "green" }
  | { kind: "blue" }
  | { kind: "cyan" }
  | { kind: "magenta" }
  | { kind: "yellow" }
  | { kind: "black" }
  | { kind: "lightness" }
  | { kind: "aStar" }
  | { kind: "bStar" }
  | { kind: "alpha"; name: string }
  | { kind: "spot"; name: string };

/** Mirrors `CalcSource` in src-tauri/src/document.rs: one of Image >
 * Calculations' two sources. */
export type CalcSource = { layer: number | null; channel: ApplyChannel; invert: boolean };

/** Mirrors `CalcResult` in src-tauri/src/document.rs. */
export type CalcResult = "newDocument" | "newChannel" | "selection";

/** Mirrors `LayerGroup` in src-tauri/src/document.rs. */
export type LayerGroup = { name: string; members: number[] };

/** Mirrors `GuideOrientation` / `Guide` in src-tauri/src/document.rs: a
 * guide line on a pixel boundary, `position` pixels from the top or left. */
export type GuideOrientation = "horizontal" | "vertical";
export type Guide = { orientation: GuideOrientation; position: number };

/** A Note tool annotation pinned to a pixel. */
export type Note = {
  x: number;
  y: number;
  text: string;
};

/** Mirrors `HistoryState` in src-tauri/src/lib.rs. */
export type HistoryState = {
  canUndo: boolean;
  canRedo: boolean;
  /** Whether a History Brush source has been set. */
  hasHistorySource: boolean;
};

/** Mirrors `Snapshot` in src-tauri/src/lib.rs. */
export type Snapshot = HistoryState & {
  document: DocumentView;
  /**
   * Bumped every time the composite changes. The frontend refetches
   * `composite://composite.png?g=<generation>` when this changes rather than
   * receiving the encoded image over IPC.
   */
  generation: number;
};

export type MoveDirection = "up" | "down";

/** What a pointer drag on the canvas does: edit the selected layer, or
 * redefine the document's selection. */
/** The Ruler tool's readout for one drag, in document pixels; the angle is
 * degrees counter-clockwise from the x axis, y up on screen. */
export type Measurement = {
  width: number;
  height: number;
  distance: number;
  angle: number;
};

/** Paint Symmetry: mirror every brush, eraser, and pattern-stamp stroke
 * about the canvas's vertical centre line, its horizontal one, or both. */
export type Symmetry = "vertical" | "horizontal" | "both";

/** How a new marquee combines with the current selection — the four mode
 * buttons in Photoshop's selection-tool options bar. */
export type SelectionMode = "new" | "add" | "subtract" | "intersect";

/** Mirrors `ShapeBlurKernel` in src-tauri/src/document.rs: the shape
 * Filter > Blur > Shape Blur averages over. */
export type ShapeBlurKernel = "square" | "diamond" | "circle";

/** Mirrors `LevelsChannel` in src-tauri/src/document.rs: the Levels
 * dialog's Channel dropdown. */
export type LevelsChannel = "rgb" | "red" | "green" | "blue";

export type Tool =
  | "move"
  | "brush"
  | "eraser"
  | "magicEraser"
  | "backgroundEraser"
  | "dodge"
  | "burn"
  | "sponge"
  | "blur"
  | "sharpen"
  | "smudge"
  | "colorReplace"
  | "redEye"
  | "ruler"
  | "colorSampler"
  | "count"
  | "note"
  | "patternStamp"
  | "cloneStamp"
  | "healingBrush"
  | "spotHealingBrush"
  | "remove"
  | "patch"
  | "contentAwareMove"
  | "rectangle"
  | "ellipse"
  | "line"
  | "polygon"
  | "star"
  | "triangle"
  | "historyBrush"
  | "magicWand"
  | "lasso"
  | "magneticLasso"
  | "vectorMask"
  | "objectSelect"
  | "objectSelectLasso"
  | "polygonLasso"
  | "selectionBrush"
  | "quickSelection"
  | "selectRect"
  | "selectEllipse"
  | "selectRow"
  | "selectColumn"
  | "eyedropper"
  | "paintBucket"
  | "gradient";
