// Edit > Preferences > Interface: the app's look, as Photoshop's own
// Interface preferences shape it — a colour theme at one of four
// brightnesses, the highlight colour, and the UI font size. Kept in the
// browser, applied as attributes on the root element that the
// stylesheet's token sets key on, and applied again before the first
// paint by main.tsx so a light theme never flashes dark.

export const THEMES = ["darkest", "dark", "light", "lightest"] as const;
export const HIGHLIGHTS = ["blue", "grey"] as const;
export const UI_SIZES = ["small", "medium", "large"] as const;

export type Theme = (typeof THEMES)[number];
export type Highlight = (typeof HIGHLIGHTS)[number];
export type UiSize = (typeof UI_SIZES)[number];

export type InterfacePreferences = {
  theme: Theme;
  highlight: Highlight;
  uiSize: UiSize;
};

export const INTERFACE_STORAGE_KEY = "legelabs.interface";

export const INTERFACE_DEFAULTS: InterfacePreferences = {
  theme: "dark",
  highlight: "blue",
  uiSize: "medium",
};

function pick<T extends string>(allowed: readonly T[], value: unknown, fallback: T): T {
  return typeof value === "string" && (allowed as readonly string[]).includes(value)
    ? (value as T)
    : fallback;
}

/** The preferences a stored JSON string holds; anything missing or unknown falls back to the default. */
export function parseInterface(stored: string | null): InterfacePreferences {
  if (!stored) return { ...INTERFACE_DEFAULTS };
  let parsed: unknown;
  try {
    parsed = JSON.parse(stored);
  } catch {
    return { ...INTERFACE_DEFAULTS };
  }
  const record = parsed && typeof parsed === "object" ? (parsed as Record<string, unknown>) : {};
  return {
    theme: pick(THEMES, record.theme, INTERFACE_DEFAULTS.theme),
    highlight: pick(HIGHLIGHTS, record.highlight, INTERFACE_DEFAULTS.highlight),
    uiSize: pick(UI_SIZES, record.uiSize, INTERFACE_DEFAULTS.uiSize),
  };
}

/** The attributes the stylesheet keys on, set on `root` (the document element). */
export function applyInterface(
  root: { dataset: DOMStringMap | Record<string, string | undefined> },
  preferences: InterfacePreferences,
): void {
  root.dataset.theme = preferences.theme;
  root.dataset.highlight = preferences.highlight;
  root.dataset.uiSize = preferences.uiSize;
}

/** Reads the stored preferences and applies them — what main.tsx runs before the first paint. */
export function applyStoredInterface(): InterfacePreferences {
  let stored: string | null = null;
  try {
    stored = localStorage.getItem(INTERFACE_STORAGE_KEY);
  } catch {
    // Storage unavailable: the defaults apply.
  }
  const preferences = parseInterface(stored);
  applyInterface(document.documentElement, preferences);
  return preferences;
}
