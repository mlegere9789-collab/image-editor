// The Blur Gallery's on-canvas controls: Field Blur's pins, Iris Blur's
// centre and ring, Tilt-Shift's focus and band lines — Photoshop's own
// way of placing a blur by dragging on the picture rather than typing
// coordinates. The geometry is pure: where a pointer lands in document
// pixels, and where a document point sits on the canvas as a percentage.

export type Size = { width: number; height: number };
export type Box = { left: number; top: number; width: number; height: number };

/** The document pixel under a pointer over the canvas box, clamped to the canvas. */
export function documentPoint(
  canvas: Box,
  clientX: number,
  clientY: number,
  document: Size,
): { x: number; y: number } {
  const fx = canvas.width > 0 ? (clientX - canvas.left) / canvas.width : 0;
  const fy = canvas.height > 0 ? (clientY - canvas.top) / canvas.height : 0;
  return {
    x: Math.round(Math.min(Math.max(fx, 0), 1) * document.width),
    y: Math.round(Math.min(Math.max(fy, 0), 1) * document.height),
  };
}

/** A document x as a percentage of the canvas width (or y of its height). */
export function percentOf(value: number, extent: number): string {
  return `${extent > 0 ? (value / extent) * 100 : 0}%`;
}

/** The distance between two document points, rounded to whole pixels. */
export function pixelDistance(a: { x: number; y: number }, b: { x: number; y: number }): number {
  return Math.round(Math.hypot(a.x - b.x, a.y - b.y));
}

/** The CSS of a ring of document radius `radius` about a document point. */
export function ringStyle(
  center: { x: number; y: number },
  radius: number,
  document: Size,
): { left: string; top: string; width: string; height: string } {
  return {
    left: percentOf(center.x - radius, document.width),
    top: percentOf(center.y - radius, document.height),
    width: percentOf(2 * radius, document.width),
    height: percentOf(2 * radius, document.height),
  };
}
