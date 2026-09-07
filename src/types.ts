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
};

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
};

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
