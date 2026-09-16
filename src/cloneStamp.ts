// The Clone Stamp's own Aligned/Not Aligned rule: Photoshop's Aligned
// (this project's own long-standing default and, until now, its only
// mode) keeps one fixed offset between the Alt-clicked source and the
// brush for as many separate strokes as follow, so painting continues
// sampling from further and further across the source the more you
// paint. Not Aligned resets that offset at the start of every new
// stroke, so each stroke starts sampling from the same source point
// again, wherever that stroke itself happens to begin. Pulled out pure
// so the continuity rule is unit-testable without a pointer event.

/** The offset (`source - point`, rounded to whole document pixels) a
 * stroke's brush samples the Clone Stamp source at: `current` if Aligned
 * already has one from an earlier stroke, `source - point` freshly
 * otherwise -- Not Aligned always takes the fresh branch, so it never
 * reuses a stroke's own offset once that stroke ends. */
export function nextCloneOffset(
  current: [number, number] | null,
  aligned: boolean,
  source: [number, number],
  point: [number, number],
): [number, number] {
  if (aligned && current !== null) return current;
  return [Math.round(source[0] - point[0]), Math.round(source[1] - point[1])];
}
