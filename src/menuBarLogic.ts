// The menu bar's model: a Photoshop-style menu tree built from the
// commands the toolbar already carries, so the toolbar buttons stay the
// one registry of what the app can do and the menu bar is a view over it.
//
// Every toolbar button that stands in for a Photoshop menu command names
// its place in a `title` of the form `Menu > Submenu > Command: what it
// does`. `menuPath` reads that prefix; `buildMenuTree` folds the entries
// into the ten top-level menus in Photoshop's own order. Tool buttons
// (which carry `data-tool`) and contextual buttons whose titles name no
// menu are left out, as they are in Photoshop's own menus.

export const TOP_MENUS = [
  "File",
  "Edit",
  "Image",
  "Layer",
  "Type",
  "Select",
  "Filter",
  "View",
  "Window",
  "Help",
] as const;

export type TopMenu = (typeof TOP_MENUS)[number];

/** Photoshop's own fixed Menu Color palette (Edit > Menus' colour picker). */
export const MENU_COLORS = [
  "red",
  "orange",
  "yellow",
  "green",
  "blue",
  "violet",
  "gray",
] as const;

export type MenuColor = (typeof MENU_COLORS)[number];

/** Toolbar titles that start at a Photoshop submenu rather than a menu. */
const SUBMENU_ROOTS: Record<string, string[]> = {
  "Filter Gallery": ["Filter", "Filter Gallery"],
  "Blur Gallery": ["Filter", "Blur Gallery"],
  "Neural Filters": ["Filter", "Neural Filters"],
  "Camera Raw Filter": ["Filter", "Camera Raw Filter"],
  "Color Settings": ["Edit", "Color Settings"],
  Adjustments: ["Image", "Adjustments"],
};

/** One toolbar command as the menu bar sees it. */
export type MenuEntry = {
  label: string;
  title: string;
  disabled: boolean;
  /** A toggle's state (`aria-pressed`), or `null` for a plain command. */
  checked?: boolean | null;
  run: () => void;
};

export type MenuCommand = {
  kind: "command";
  label: string;
  /** The menu path above the command, e.g. `["Edit", "Transform"]`. */
  path: string[];
  shortcut: string | null;
  /** The title's description, after the menu path. */
  hint: string;
  disabled: boolean;
  checked: boolean | null;
  run: () => void;
  /** Edit > Menus' own Menu Color, or `null` for the plain, uncoloured row. */
  color: MenuColor | null;
};

export type MenuGroup = {
  kind: "group";
  label: string;
  path: string[];
  items: MenuItem[];
};

export type MenuItem = MenuCommand | MenuGroup;

/**
 * The menu path a toolbar title names, without the command's own name
 * (the button's label is the command), or `null` when the title names
 * no Photoshop menu.
 *
 * `"Edit > Transform > Rotate (any angle)"` → `["Edit", "Transform"]`;
 * `"Filter Gallery > Artistic > Poster Edges"` → `["Filter", "Filter
 * Gallery", "Artistic"]`; `"Undo (Ctrl/Cmd+Z)"` → `null`.
 */
export function menuPath(title: string): string[] | null {
  const head = title.split(/\s*[:(—–]|\s--\s/, 1)[0].trim();
  if (!head.includes(" > ")) return null;
  const segments = head
    .split(" > ")
    .map((segment) => segment.trim())
    .filter((segment) => segment.length > 0);
  if (segments.length < 2) return null;
  const root = SUBMENU_ROOTS[segments[0]];
  const top = root
    ? root
    : (TOP_MENUS as readonly string[]).includes(segments[0])
      ? [segments[0]]
      : null;
  if (top === null) return null;
  const path = [...top, ...segments.slice(1, -1)];
  // Photoshop's Blur Gallery is a Filter submenu of its own, not a
  // Filter Gallery group, however this app's titles file it.
  const blurGallery = path.indexOf("Blur Gallery");
  if (blurGallery > 0 && path[blurGallery - 1] === "Filter Gallery")
    path.splice(blurGallery - 1, 1);
  return path;
}

/** The identity of a command across renders: its menu path and label. */
export function commandKey(command: { path: string[]; label: string }): string {
  return [...command.path, command.label].join(" > ");
}

/**
 * Photoshop's own order for the commands each menu leads with; anything
 * a menu carries beyond these follows in the toolbar's order. Keyed by
 * the menu path, so submenus have an order of their own.
 */
const MENU_ORDER: Record<string, string[]> = {
  File: [
    "New…",
    "Open PNG…",
    "Open Project…",
    "Save Project…",
    "Cloud Documents",
    "Share",
    "Import",
    "Export",
  ],
  Edit: [
    "Undo",
    "Redo",
    "Cut",
    "Copy",
    "Copy Merged",
    "Paste",
    "Paste Special",
    "Delete",
    "Fill…",
    "Content-Aware Fill",
    "Content-Aware Scale…",
    "Puppet Warp…",
    "Perspective Warp…",
    "Free Transform…",
    "Transform",
    "Define Brush Preset",
    "Define Pattern",
    "Presets…",
    "Convert…",
    "Color Settings",
    "Keyboard Shortcuts…",
    "Customize Menus…",
    "Customize Toolbar…",
    "Preferences",
  ],
  Image: [
    "Adjustments",
    "Canvas Size…",
    "Image Rotation",
    "Generative Expand…",
    "Apply Image…",
    "Calculations…",
  ],
  "Image > Adjustments": [
    "Brightness/Contrast…",
    "Levels…",
    "Curves…",
    "Exposure…",
    "Vibrance…",
    "Hue/Saturation…",
    "Color Balance…",
    "Black & White",
    "Photo Filter…",
    "Channel Mixer…",
    "Color Lookup…",
    "Invert Colors",
    "Posterize…",
    "Threshold…",
    "Gradient Map…",
    "Selective Color…",
    "Match Color…",
    "Replace Color…",
    "Equalize",
    "Auto Tone",
    "Auto Contrast",
    "Auto Color",
  ],
  Layer: [
    "New",
    "New Fill Layer",
    "Adjustment Layer…",
    "Fill Layer…",
    "Smart Object…",
    "Layer Style",
    "Add Mask",
    "Layer Mask",
    "Group Layers",
    "Remove Background",
    "Mask All Objects",
  ],
  Select: [
    "Select All",
    "Deselect",
    "Reselect",
    "Invert",
    "Select Subject",
    "Select Subject (Cloud)",
    "Select Sky",
    "Select People",
    "People",
    "Select Hair",
    "Subject",
    "Focus Area…",
    "Color Range…",
    "Select and Mask…",
    "Modify",
    "Grow",
    "Similar",
    "Transform Selection…",
    "Move Selection…",
    "Load Selection…",
    "Save Selection…",
  ],
  Filter: [
    "Camera Raw Filter…",
    "Camera Raw Filter",
    "Lens Correction…",
    "Liquify…",
    "Liquify",
    "Vanishing Point…",
    "Adaptive Wide Angle…",
    "Neural Filters",
    "Generative Fill (AI)",
    "Generative Fill…",
    "Generate Similar (AI)",
    "Generative",
    "Blur",
    "Blur Gallery",
    "Distort",
    "Noise",
    "Pixelate",
    "Render",
    "Sharpen",
    "Stylize",
    "Filter Gallery",
    "Other",
  ],
  Window: [
    "Workspace",
    "Artboards…",
    "Brush Settings…",
    "Layer Comps…",
    "Libraries…",
    "Boards…",
    "Assistant…",
  ],
};

function sortItems(items: MenuItem[], path: string[]): void {
  const order = MENU_ORDER[path.join(" > ")];
  if (order) {
    const rank = (item: MenuItem, index: number) => {
      const at = order.indexOf(item.label);
      return at === -1 ? order.length + index : at;
    };
    const ranked = items.map((item, index) => ({ item, rank: rank(item, index) }));
    ranked.sort((a, b) => a.rank - b.rank);
    items.splice(0, items.length, ...ranked.map(({ item }) => item));
  }
  for (const item of items) if (item.kind === "group") sortItems(item.items, item.path);
}

function pruneEmptyGroups(items: MenuItem[]): MenuItem[] {
  return items
    .map((item) =>
      item.kind === "group" ? { ...item, items: pruneEmptyGroups(item.items) } : item,
    )
    .filter((item) => item.kind === "command" || item.items.length > 0);
}

/** The `(Ctrl/Cmd+…)` hint in a title, as shown beside the command. */
export function menuShortcut(title: string): string | null {
  const match = /\(((?:Ctrl\/Cmd|Ctrl|Cmd|Shift|Alt)\+[^):\s]*)/.exec(title);
  return match ? match[1] : null;
}

/** The description a title carries after its menu path, if any. */
export function menuHint(title: string): string {
  const match = /(?::\s*|\s[—–]\s|\s--\s)(.*)$/s.exec(title);
  return match ? match[1].trim() : "";
}

function groupAt(items: MenuItem[], path: string[], depth: number): MenuItem[] {
  if (depth === path.length) return items;
  const label = path[depth];
  let group = items.find(
    (item): item is MenuGroup => item.kind === "group" && item.label === label,
  );
  if (!group) {
    group = { kind: "group", label, path: path.slice(0, depth + 1), items: [] };
    items.push(group);
  }
  return groupAt(group.items, path, depth + 1);
}

/**
 * Folds toolbar entries into the ten top-level menus, in Photoshop's
 * order, keeping the toolbar's own order inside each menu; submenus sit
 * where their first command does. Entries whose titles name no menu are
 * skipped, as are commands whose key (see `commandKey`) is in `hidden`
 * — Edit > Menus — and any submenu that leaves empty. Every top-level
 * menu is present, empty or not, so the bar always reads File … Help,
 * and each menu leads with Photoshop's own order (`MENU_ORDER`). `colors`
 * — Edit > Menus' own Menu Color — labels each surviving command with
 * whichever colour its own key was given, or `null` for none.
 */
export function buildMenuTree(
  entries: MenuEntry[],
  hidden: ReadonlySet<string> = new Set(),
  colors: ReadonlyMap<string, MenuColor> = new Map(),
): MenuGroup[] {
  const menus: MenuGroup[] = TOP_MENUS.map((label) => ({
    kind: "group",
    label,
    path: [label],
    items: [],
  }));
  for (const entry of entries) {
    const path = menuPath(entry.title);
    if (path === null) continue;
    const key = commandKey({ path, label: entry.label });
    if (hidden.has(key)) continue;
    const menu = menus.find((candidate) => candidate.label === path[0]);
    if (!menu) continue;
    const items = groupAt(menu.items, path, 1);
    items.push({
      kind: "command",
      label: entry.label,
      path,
      shortcut: menuShortcut(entry.title),
      hint: menuHint(entry.title),
      disabled: entry.disabled,
      checked: entry.checked ?? null,
      run: entry.run,
      color: colors.get(key) ?? null,
    });
  }
  for (const menu of menus) {
    menu.items = pruneEmptyGroups(menu.items);
    sortItems(menu.items, menu.path);
  }
  return menus;
}

/**
 * A signature of what a tree shows — labels, disabled states, toggles —
 * so a rebuild from the same toolbar can be recognised as unchanged.
 */
export function menuSignature(entries: MenuEntry[]): string {
  return entries
    .map(
      (entry) =>
        `${entry.title}\u0000${entry.label}\u0000${entry.disabled ? 1 : 0}${entry.checked ?? "-"}`,
    )
    .join("\u0001");
}

/** Every command in a tree, depth first — what a search or a count sees. */
export function flattenMenuTree(items: MenuItem[]): MenuCommand[] {
  const out: MenuCommand[] = [];
  for (const item of items) {
    if (item.kind === "command") out.push(item);
    else out.push(...flattenMenuTree(item.items));
  }
  return out;
}
