//! Separable blend functions from the W3C Compositing and Blending spec.
//!
//! Every function takes non-premultiplied backdrop and source channel values in
//! `0.0..=1.0` and returns the blended channel, also in `0.0..=1.0`. Channels are
//! blended independently, which is what "separable" means — the four
//! non-separable modes (hue, saturation, color, luminosity) need all three
//! channels at once and are not implemented here.
//!
//! <https://www.w3.org/TR/compositing-1/#blending>

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
}

impl BlendMode {
    /// Every mode, in the order the UI lists them.
    pub const ALL: [BlendMode; 12] = [
        Self::Normal,
        Self::Multiply,
        Self::Screen,
        Self::Overlay,
        Self::Darken,
        Self::Lighten,
        Self::ColorDodge,
        Self::ColorBurn,
        Self::HardLight,
        Self::SoftLight,
        Self::Difference,
        Self::Exclusion,
    ];

    /// Human-readable name for the layer panel.
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Multiply => "Multiply",
            Self::Screen => "Screen",
            Self::Overlay => "Overlay",
            Self::Darken => "Darken",
            Self::Lighten => "Lighten",
            Self::ColorDodge => "Color Dodge",
            Self::ColorBurn => "Color Burn",
            Self::HardLight => "Hard Light",
            Self::SoftLight => "Soft Light",
            Self::Difference => "Difference",
            Self::Exclusion => "Exclusion",
        }
    }

    /// `B(Cb, Cs)` — blend one channel of the source over the backdrop.
    pub fn blend(self, cb: f32, cs: f32) -> f32 {
        match self {
            Self::Normal => cs,
            Self::Multiply => cb * cs,
            Self::Screen => cb + cs - cb * cs,
            // Overlay is Hard Light with the operands swapped.
            Self::Overlay => hard_light(cs, cb),
            Self::Darken => cb.min(cs),
            Self::Lighten => cb.max(cs),
            Self::ColorDodge => {
                if cb <= 0.0 {
                    0.0
                } else if cs >= 1.0 {
                    1.0
                } else {
                    (cb / (1.0 - cs)).min(1.0)
                }
            }
            Self::ColorBurn => {
                if cb >= 1.0 {
                    1.0
                } else if cs <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - cb) / cs).min(1.0)
                }
            }
            Self::HardLight => hard_light(cb, cs),
            Self::SoftLight => soft_light(cb, cs),
            Self::Difference => (cb - cs).abs(),
            Self::Exclusion => cb + cs - 2.0 * cb * cs,
        }
    }
}

fn hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        // Multiply against the doubled source.
        cb * (2.0 * cs)
    } else {
        let d = 2.0 * cs - 1.0;
        cb + d - cb * d
    }
}

fn soft_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
    } else {
        let d = if cb <= 0.25 {
            ((16.0 * cb - 12.0) * cb + 4.0) * cb
        } else {
            cb.sqrt()
        };
        cb + (2.0 * cs - 1.0) * (d - cb)
    }
}

#[cfg(test)]
mod tests {
    use super::BlendMode::*;
    use super::*;

    /// Compare with a tolerance that is well below one 8-bit step (1/255).
    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    macro_rules! assert_close {
        ($a:expr, $b:expr) => {
            assert!(close($a, $b), "expected {}, got {}", $b, $a)
        };
    }

    #[test]
    fn normal_returns_the_source() {
        for cs in [0.0, 0.25, 0.5, 1.0] {
            assert_close!(Normal.blend(0.7, cs), cs);
        }
    }

    #[test]
    fn known_values_from_the_spec() {
        assert_close!(Multiply.blend(0.5, 0.5), 0.25);
        assert_close!(Screen.blend(0.5, 0.5), 0.75);
        assert_close!(Darken.blend(0.3, 0.8), 0.3);
        assert_close!(Lighten.blend(0.3, 0.8), 0.8);
        assert_close!(Difference.blend(0.8, 0.3), 0.5);
        assert_close!(Exclusion.blend(0.5, 0.5), 0.5);
        assert_close!(HardLight.blend(0.5, 0.25), 0.25);
        assert_close!(Overlay.blend(0.25, 0.5), 0.25);
    }

    #[test]
    fn identities_that_must_hold_for_every_mode() {
        for mode in BlendMode::ALL {
            // Multiply/Screen/Darken/Lighten style duals all agree at the extremes
            // that white and black are absorbing or neutral, but the invariant every
            // separable mode shares is that output stays in range.
            for &cb in &[0.0, 0.2, 0.5, 0.75, 1.0] {
                for &cs in &[0.0, 0.2, 0.5, 0.75, 1.0] {
                    let out = mode.blend(cb, cs);
                    assert!(
                        (0.0..=1.0).contains(&out) && out.is_finite(),
                        "{mode:?} blend({cb}, {cs}) = {out} is out of range"
                    );
                }
            }
        }
    }

    #[test]
    fn multiply_and_screen_are_duals() {
        // Screen(a, b) == 1 - Multiply(1-a, 1-b)
        for &a in &[0.0, 0.3, 0.6, 1.0] {
            for &b in &[0.0, 0.3, 0.6, 1.0] {
                assert_close!(Screen.blend(a, b), 1.0 - Multiply.blend(1.0 - a, 1.0 - b));
            }
        }
    }

    #[test]
    fn overlay_is_hard_light_with_swapped_operands() {
        for &cb in &[0.0, 0.2, 0.45, 0.55, 0.9, 1.0] {
            for &cs in &[0.0, 0.2, 0.45, 0.55, 0.9, 1.0] {
                assert_close!(Overlay.blend(cb, cs), HardLight.blend(cs, cb));
            }
        }
    }

    #[test]
    fn dodge_and_burn_handle_their_singularities() {
        // cs == 1 would divide by zero in the dodge formula.
        assert_close!(ColorDodge.blend(0.5, 1.0), 1.0);
        // A black backdrop stays black no matter how hard it is dodged.
        assert_close!(ColorDodge.blend(0.0, 1.0), 0.0);
        // cs == 0 would divide by zero in the burn formula.
        assert_close!(ColorBurn.blend(0.5, 0.0), 0.0);
        // A white backdrop stays white no matter how hard it is burned.
        assert_close!(ColorBurn.blend(1.0, 0.0), 1.0);
    }

    #[test]
    fn soft_light_is_continuous_across_its_branches() {
        // The cs <= 0.5 and cs > 0.5 branches must agree at the seam.
        for &cb in &[0.0, 0.1, 0.24, 0.26, 0.5, 0.9, 1.0] {
            assert_close!(SoftLight.blend(cb, 0.5), cb);
        }
        // And the cb <= 0.25 / cb > 0.25 seam inside the upper branch.
        assert_close!(SoftLight.blend(0.25, 0.75), SoftLight.blend(0.250001, 0.75));
    }

    #[test]
    fn labels_are_unique() {
        let mut labels: Vec<_> = BlendMode::ALL.iter().map(|m| m.label()).collect();
        labels.sort_unstable();
        let count = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), count);
    }
}
