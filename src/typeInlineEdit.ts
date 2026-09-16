// The Type tool's on-canvas editing: click the canvas with it selected and
// an editable box appears right there instead of a modal dialog — the
// gesture Photoshop's own Type tool has always used. The dialog (Type…)
// still exists for precise numeric entry; this is the on-canvas
// alternative the parity checklist called out as the remaining gap. The
// two pieces of real logic here -- whether a finished edit is worth
// keeping, and how big the live overlay's text should look on screen -- are
// pulled out pure so they are unit-testable without mounting the editor.

/** An edit is worth committing to a real text layer only if it has any
 * non-whitespace content -- an empty click-and-immediately-click-away
 * (or a click-away with everything deleted) discards instead of leaving
 * behind a blank text layer nobody meant to create. */
export function shouldCommitTypeEdit(text: string): boolean {
  return text.trim() !== "";
}

/** The on-canvas overlay's CSS `font-size`, in real screen pixels, for a
 * type layer sized `sizePx` in *document* pixels: the same ratio every
 * other on-canvas control already uses to track the canvas's own
 * displayed size (`document width -> canvas-wrap width`), applied to a
 * font size instead of a position -- so the live preview roughly matches
 * how large the committed layer will actually render, at any zoom.
 * Falls back to `sizePx` unscaled if the canvas has no measurable size or
 * width yet (a fresh mount, or a canvas-wrap not yet laid out). */
export function inlineFontSizePx(
  sizePx: number,
  canvasDisplayWidth: number,
  documentWidth: number,
): number {
  if (documentWidth <= 0 || canvasDisplayWidth <= 0) return sizePx;
  return sizePx * (canvasDisplayWidth / documentWidth);
}
