// The Ruler tool's own protractor: Photoshop lets you Alt-drag a second
// leg from the endpoint of a measured line, then reads out the interior
// angle between the two legs (0-180 degrees) rather than either leg's
// own angle from horizontal. The geometry is pure: given the two legs'
// own `measure()` angles (already degrees counter-clockwise from
// horizontal), the interior angle is just the shorter arc between them.

/**
 * The interior angle (0..=180) between two ruler legs, given each leg's
 * own angle from horizontal (as `measure()` already returns). Wraps at
 * 360 so a leg near 0/360 and one near 350 read as 10 degrees apart, not
 * 350, and always returns the smaller of the two arcs around the circle.
 */
export function protractorAngle(firstLegAngle: number, secondLegAngle: number): number {
  const diff = Math.abs(firstLegAngle - secondLegAngle) % 360;
  return diff > 180 ? 360 - diff : diff;
}
