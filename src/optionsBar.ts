// The options bar: the toolbar shows only the active tool's settings.
// Most controls already render only for their tool; the few shared
// ones — brush size and flow, the tip and dynamics toggles, the people
// selectors — carry `data-tool-option="tool tool …"`, and `optionsRule`
// is the one stylesheet rule that hides every such control whose list
// leaves the active tool out.

/** Tools that paint with the brush engine and so take Size and Flow. */
export const BRUSH_TOOLS = [
  "brush",
  "eraser",
  "backgroundEraser",
  "dodge",
  "burn",
  "sponge",
  "blur",
  "sharpen",
  "smudge",
  "colorReplace",
  "selectionBrush",
  "quickSelection",
  "patternStamp",
  "cloneStamp",
  "healingBrush",
  "spotHealingBrush",
  "remove",
  "historyBrush",
  "mixerBrush",
  "artHistoryBrush",
] as const;

/** Tools whose strokes can use the defined tip and Brush Settings. */
export const TIP_TOOLS = ["brush", "eraser"] as const;

/** Tools that pick people and their parts. */
export const PEOPLE_TOOLS = ["objectSelect", "objectSelectLasso"] as const;

/** The rule that hides every `data-tool-option` control not listed for `tool`. */
export function optionsRule(tool: string): string {
  const safe = tool.replace(/[^A-Za-z0-9_-]/g, "");
  return `header.toolbar [data-tool-option]:not([data-tool-option~="${safe}"]){display:none!important}`;
}
