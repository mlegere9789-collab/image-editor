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
};

/** Mirrors `SelectionShape` in src-tauri/src/document.rs (serde camelCase). */
export type SelectionShape = "rectangle" | "ellipse";

/** Mirrors `Selection` (aka `SelectionView`) in src-tauri/src/document.rs. */
export type Selection = {
  shape: SelectionShape;
  bounds: { x0: number; y0: number; x1: number; y1: number };
  /** Select > Inverse: selects everywhere *except* `shape`. */
  inverted: boolean;
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
};

/** Mirrors `HistoryState` in src-tauri/src/lib.rs. */
export type HistoryState = {
  canUndo: boolean;
  canRedo: boolean;
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
export type Tool =
  | "brush"
  | "eraser"
  | "selectRect"
  | "selectEllipse"
  | "eyedropper"
  | "paintBucket"
  | "gradient";
