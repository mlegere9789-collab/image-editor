// Help > Welcome Tour: a first-launch walk through the app's surfaces,
// one card at a time, each pointed at a real element of the page. The
// steps are data; `tourStep` moves through them; a flag in the browser
// remembers that the tour was seen so it runs once on its own and again
// only when asked.

export type TourStep = {
  /** A CSS selector for the element the card points at; `null` centres the card. */
  target: string | null;
  title: string;
  body: string;
};

export const TOUR_STORAGE_KEY = "legelabs.tourSeen";

export const TOUR_STEPS: readonly TourStep[] = [
  {
    target: null,
    title: "Welcome to LegeLabs",
    body: "A short tour of where things are. Every step points at the part of the window it describes; Esc leaves at any time, and Help > Welcome Tour… brings it back.",
  },
  {
    target: ".menubar",
    title: "The menu bar",
    body: "File, Edit, Image, Layer, Type, Select, Filter, View, Window, and Help, laid out the way Photoshop's are. Every command in the app is here, with its shortcut beside it.",
  },
  {
    target: ".toolbox",
    title: "The toolbox",
    body: "Every tool, in Photoshop's own column at the left of the canvas. Pick one and its options appear in the bar above; Edit > Toolbar hides the tools you never use.",
  },
  {
    target: "header.toolbar",
    title: "The options bar",
    body: "The active tool's settings, and a few commands no menu names. Window > Workspace > Compact Toolbar keeps it to that; turn it off to see every command as a button.",
  },
  {
    target: ".stage",
    title: "The canvas",
    body: "Your document. Drop a PNG here to open it, or File > New… to start one. Selections, guides, and transform handles draw over it.",
  },
  {
    target: ".dock-zone--right",
    title: "Panels",
    body: "Layers, Channels, and the other panels dock on the right; drag a tab to move one, or float it.",
  },
  {
    target: "footer.statusbar",
    title: "The status bar",
    body: "Document size, layer count, pointer readouts, and — while a long operation runs — its progress with a Cancel button. A recording action shows here too.",
  },
  {
    target: null,
    title: "That's the tour",
    body: "Edit > Preferences > Interface sets the theme and font size; Window > Actions records what you do; Help > Discover… finds any feature by name.",
  },
];

/** The step index after moving `delta` from `index`, or `null` past either end. */
export function tourStep(index: number, delta: number, count = TOUR_STEPS.length): number | null {
  const next = index + delta;
  return next < 0 || next >= count ? null : next;
}

/** Whether the tour was seen before (or the flag could not be read). */
export function tourSeen(): boolean {
  try {
    return localStorage.getItem(TOUR_STORAGE_KEY) === "true";
  } catch {
    return true;
  }
}

export function markTourSeen(): void {
  try {
    localStorage.setItem(TOUR_STORAGE_KEY, "true");
  } catch {
    // ignore
  }
}

/** Where the card sits for a target's box: below it when there is room, else above, else centred. */
export function cardPlacement(
  target: { top: number; left: number; width: number; height: number } | null,
  viewport: { width: number; height: number },
  card: { width: number; height: number },
): { top: number; left: number } {
  const margin = 12;
  if (!target) {
    return {
      top: Math.max(margin, (viewport.height - card.height) / 2),
      left: Math.max(margin, (viewport.width - card.width) / 2),
    };
  }
  const left = Math.min(
    Math.max(margin, target.left + target.width / 2 - card.width / 2),
    Math.max(margin, viewport.width - card.width - margin),
  );
  const below = target.top + target.height + margin;
  if (below + card.height <= viewport.height - margin) return { top: below, left };
  const above = target.top - margin - card.height;
  if (above >= margin) return { top: above, left };
  return { top: Math.max(margin, (viewport.height - card.height) / 2), left };
}
