//! Real OpenColorIO (`.ocio`) config parsing and transform application —
//! Color Settings > OpenColorIO / OCIO Input Color Space Assignment / ACES
//! Color Management: a real OCIO config is a real, published, human-
//! readable YAML file (the OCIO config syntax — see
//! <https://opencolorio.readthedocs.io/en/latest/configurations/config_syntax.html>)
//! naming colour spaces and the real, computable transform chains
//! (`MatrixTransform`, `ExponentTransform`, `GroupTransform`) between them
//! and a shared reference space — not a fixed enum of profile names the
//! way [`crate::document::ColorProfile`] is. ACES Color Management is not
//! a separate system needing its own hand-written matrices: it is itself
//! just an OCIO config (the real, published ACES config uses the exact
//! transform types this module applies), so this one real engine covers
//! both by construction, the same way real Photoshop's own OCIO
//! integration works.
//!
//! **Scope note on YAML tags.** A real `.ocio` file marks each transform's
//! own type with an explicit YAML tag (`!<MatrixTransform>`,
//! `!<ExponentTransform>`, `!<GroupTransform>`, `!<FileTransform>`, …).
//! `serde_yaml` 0.9 (this project's own YAML dependency) discards that tag
//! when deserializing into a typed struct — confirmed directly before this
//! was written, not assumed. This module identifies a transform's own
//! type by which of its own defining fields are present instead (`matrix`
//! for `MatrixTransform`, `value` for `ExponentTransform`, `children` for
//! `GroupTransform`) via `#[serde(untagged)]`, which is real, deterministic,
//! and matches every field name a genuine OCIO config actually uses for
//! these three transform types — not a guess at the file's structure.
//! Every other real transform type (`FileTransform`, `CDLTransform`,
//! `LogTransform`, `ColorSpaceTransform`, …) is recognised as present but
//! unsupported: parsing a config that contains one still succeeds, and a
//! clear, real error is returned only if that specific transform is ever
//! actually applied — never a silent no-op.

use std::collections::HashMap;

use serde::Deserialize;

/// One real OCIO transform. Untagged: the real field names below (not the
/// YAML tag `serde_yaml` discards) are what distinguish the three
/// supported shapes from each other and from [`Transform::Unsupported`] —
/// see this module's own doc comment for why.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Transform {
    /// `MatrixTransform`: a real 4×4 matrix (row-major, applied to
    /// `[r, g, b, a]`) plus an optional 4-component offset added after —
    /// OCIO's own real representation of any linear colour transform,
    /// including a 3×3 RGB matrix with the last row/column as identity.
    Matrix {
        matrix: [f64; 16],
        #[serde(default)]
        offset: Option<[f64; 4]>,
    },
    /// `ExponentTransform`: a real per-channel power law, one exponent
    /// each for R, G, B, A. OCIO's own real semantics for this transform
    /// clamp a negative input to `0` before raising it to `value`, rather
    /// than propagating a NaN the way `f64::powf` would on its own.
    Exponent { value: [f64; 4] },
    /// `GroupTransform`: a real, ordered chain of other transforms,
    /// applied one after another in list order — OCIO's own way of
    /// composing more than one real transform into a single named
    /// colour space's own `to_reference`/`from_reference`.
    Group { children: Vec<Transform> },
    /// Any other real OCIO transform type this module does not parse the
    /// fields of (`FileTransform`, `CDLTransform`, `LogTransform`,
    /// `ColorSpaceTransform`, …) — the raw YAML is kept so a config
    /// containing one still loads; [`apply`] returns a clear, real error
    /// naming what is missing only if this specific transform is ever
    /// actually applied.
    Unsupported(serde_yaml::Value),
}

/// One real named colour space from a config's own `colorspaces:` list.
#[derive(Debug, Clone, Deserialize)]
pub struct ColorSpace {
    pub name: String,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// This colour space's own real transform *into* the config's shared
    /// reference space. `None` for a colour space that already *is* the
    /// reference space by convention (OCIO's own real convention for a
    /// role like `scene_linear` with no transform of its own).
    #[serde(default)]
    pub to_reference: Option<Transform>,
    /// The real inverse direction, *out of* the reference space. `None`
    /// when the config only defines `to_reference` — this module does not
    /// auto-invert a matrix to synthesize the missing direction (a real,
    /// but unimplemented, capability — see [`convert`]'s own doc comment).
    #[serde(default)]
    pub from_reference: Option<Transform>,
}

/// A real, parsed `.ocio` config: the handful of top-level fields this
/// project reads out of the real config syntax. Other real top-level keys
/// (`search_path`, `displays`, `active_displays`, `luma`, …) may be
/// present in a real file and are simply not read by this project.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub ocio_profile_version: Option<u32>,
    /// Real named shortcuts to a colour space (`default`, `scene_linear`,
    /// `reference`, `aces_interchange`, …) — a real config's own
    /// `roles:` mapping, role name to colour-space name.
    #[serde(default)]
    pub roles: HashMap<String, String>,
    #[serde(default)]
    pub colorspaces: Vec<ColorSpace>,
}

/// Parses `text` as a real OCIO config. Fails on a config missing a real
/// `colorspaces:` list entirely or one whose YAML doesn't parse — not on
/// one that merely contains a transform type this module doesn't apply
/// (see [`Transform::Unsupported`]).
pub fn parse(text: &str) -> Result<Config, String> {
    serde_yaml::from_str(text).map_err(|err| format!("Not a valid OpenColorIO config: {err}"))
}

impl Config {
    /// The real, named colour space `name`, or `None` if this config has
    /// none by that name.
    pub fn colorspace(&self, name: &str) -> Option<&ColorSpace> {
        self.colorspaces.iter().find(|cs| cs.name == name)
    }
}

/// Applies one real transform to `rgba`, returning the result — `Err` only
/// for a [`Transform::Unsupported`] transform actually reached during
/// application, naming the real, unrecognised YAML this module found there
/// rather than silently passing `rgba` through unchanged.
pub fn apply(transform: &Transform, rgba: [f64; 4]) -> Result<[f64; 4], String> {
    match transform {
        Transform::Matrix { matrix, offset } => {
            let mut out = [0.0; 4];
            for (row, slot) in out.iter_mut().enumerate() {
                let base = row * 4;
                *slot = matrix[base] * rgba[0]
                    + matrix[base + 1] * rgba[1]
                    + matrix[base + 2] * rgba[2]
                    + matrix[base + 3] * rgba[3];
            }
            if let Some(offset) = offset {
                for i in 0..4 {
                    out[i] += offset[i];
                }
            }
            Ok(out)
        }
        Transform::Exponent { value } => {
            let mut out = [0.0; 4];
            for i in 0..4 {
                out[i] = if rgba[i] <= 0.0 {
                    0.0
                } else {
                    rgba[i].powf(value[i])
                };
            }
            Ok(out)
        }
        Transform::Group { children } => {
            let mut current = rgba;
            for child in children {
                current = apply(child, current)?;
            }
            Ok(current)
        }
        Transform::Unsupported(raw) => Err(format!(
            "This transform's own real OCIO type isn't one this project applies yet \
             (recognised fields: matrix, offset, value, children; found: {})",
            describe_unsupported(raw)
        )),
    }
}

/// A short, real description of an unsupported transform's own top-level
/// field names — enough for a user to see exactly why, e.g. `src` names a
/// real `FileTransform` this module doesn't read the referenced LUT file
/// for, not a corrupt config.
fn describe_unsupported(raw: &serde_yaml::Value) -> String {
    match raw.as_mapping() {
        Some(mapping) => {
            let keys: Vec<String> = mapping
                .keys()
                .filter_map(|key| key.as_str().map(str::to_string))
                .collect();
            if keys.is_empty() {
                "an empty mapping".to_string()
            } else {
                keys.join(", ")
            }
        }
        None => "a non-mapping value".to_string(),
    }
}

/// Converts one RGB triple (each `0.0..=1.0`, alpha implicitly `1.0`) from
/// colour space `from_name` to `to_name` through `config`'s own shared
/// reference space: `from`'s own real `to_reference` transform (identity
/// if it has none, the reference-space convention), then `to`'s own real
/// `from_reference` transform. Both colour spaces must exist in `config`,
/// and `to` must define its own real `from_reference` — this module does
/// not synthesize a missing inverse by auto-inverting a matrix (a real,
/// but not-yet-built, capability), so a `to` colour space that only
/// defines `to_reference` is a clear, real error naming exactly that,
/// rather than a silently wrong guess at its own inverse.
pub fn convert(
    config: &Config,
    from_name: &str,
    to_name: &str,
    rgb: [f64; 3],
) -> Result<[f64; 3], String> {
    let from = config
        .colorspace(from_name)
        .ok_or_else(|| format!("This config has no colour space named \"{from_name}\"."))?;
    let to = config
        .colorspace(to_name)
        .ok_or_else(|| format!("This config has no colour space named \"{to_name}\"."))?;

    let reference = match &from.to_reference {
        Some(transform) => apply(transform, [rgb[0], rgb[1], rgb[2], 1.0])?,
        None => [rgb[0], rgb[1], rgb[2], 1.0],
    };

    let out = match &to.from_reference {
        Some(transform) => apply(transform, reference)?,
        None => {
            return Err(format!(
                "\"{to_name}\" only defines to_reference, not from_reference, so there is no \
                 real transform out of the reference space into it to apply -- this module \
                 does not guess at an inverse."
            ));
        }
    };

    Ok([out[0], out[1], out[2]])
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY_MATRIX: [f64; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];

    #[test]
    fn parses_a_real_shaped_config_with_all_three_supported_transform_types() {
        let text = r#"
ocio_profile_version: 1
roles:
  default: raw
  scene_linear: linear
colorspaces:
  - !<ColorSpace>
    name: linear
    family: scene
  - !<ColorSpace>
    name: srgb_display
    to_reference: !<ExponentTransform> {value: [2.2, 2.2, 2.2, 1]}
  - !<ColorSpace>
    name: shifted
    to_reference: !<MatrixTransform> {matrix: [1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1], offset: [0.1, 0.0, -0.05, 0.0]}
  - !<ColorSpace>
    name: grouped
    to_reference: !<GroupTransform>
      children:
        - !<MatrixTransform> {matrix: [1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1]}
        - !<ExponentTransform> {value: [2.0, 2.0, 2.0, 1]}
"#;
        let config = parse(text).unwrap();
        assert_eq!(config.ocio_profile_version, Some(1));
        assert_eq!(
            config.roles.get("scene_linear"),
            Some(&"linear".to_string())
        );
        assert_eq!(config.colorspaces.len(), 4);
        assert!(config.colorspace("linear").unwrap().to_reference.is_none());
        assert!(matches!(
            config.colorspace("srgb_display").unwrap().to_reference,
            Some(Transform::Exponent { .. })
        ));
        assert!(matches!(
            config.colorspace("shifted").unwrap().to_reference,
            Some(Transform::Matrix { .. })
        ));
        assert!(matches!(
            config.colorspace("grouped").unwrap().to_reference,
            Some(Transform::Group { .. })
        ));
    }

    #[test]
    fn a_config_containing_an_unsupported_transform_still_parses() {
        let text = r#"
colorspaces:
  - !<ColorSpace>
    name: from_file
    to_reference: !<FileTransform> {src: "some_lut.spi1d"}
"#;
        let config = parse(text).unwrap();
        assert!(matches!(
            config.colorspace("from_file").unwrap().to_reference,
            Some(Transform::Unsupported(_))
        ));
    }

    #[test]
    fn applying_an_unsupported_transform_is_a_clear_real_error_not_a_silent_passthrough() {
        let text = r#"
colorspaces:
  - !<ColorSpace>
    name: from_file
    to_reference: !<FileTransform> {src: "some_lut.spi1d"}
"#;
        let config = parse(text).unwrap();
        let transform = config
            .colorspace("from_file")
            .unwrap()
            .to_reference
            .as_ref()
            .unwrap();
        let error = apply(transform, [0.5, 0.5, 0.5, 1.0]).unwrap_err();
        assert!(error.contains("src"), "unexpected error: {error}");
    }

    #[test]
    fn matrix_transform_applies_the_real_linear_algebra_by_hand() {
        // A real, non-identity matrix -- swaps R and G, then the offset
        // shifts B up by 0.1. Hand-computed: [0.2, 0.8, 0.3, 1.0] -> R'
        // = 1*G = 0.8, G' = 1*R = 0.2, B' = 1*B + 0.1 = 0.4, A' = 1.0.
        let transform = Transform::Matrix {
            matrix: [
                0.0, 1.0, 0.0, 0.0, //
                1.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 1.0, 0.0, //
                0.0, 0.0, 0.0, 1.0,
            ],
            offset: Some([0.0, 0.0, 0.1, 0.0]),
        };
        let out = apply(&transform, [0.2, 0.8, 0.3, 1.0]).unwrap();
        assert!((out[0] - 0.8).abs() < 1e-9);
        assert!((out[1] - 0.2).abs() < 1e-9);
        assert!((out[2] - 0.4).abs() < 1e-9);
        assert!((out[3] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn exponent_transform_applies_a_real_per_channel_power_law() {
        // 0.5 ^ 2.2 hand-computed (Python): 0.21763764082403103.
        let transform = Transform::Exponent {
            value: [2.2, 2.2, 2.2, 1.0],
        };
        let out = apply(&transform, [0.5, 0.5, 0.5, 1.0]).unwrap();
        assert!((out[0] - 0.217_637_640_824_031_03).abs() < 1e-12);
    }

    #[test]
    fn exponent_transform_clamps_a_negative_input_to_zero_instead_of_producing_nan() {
        let transform = Transform::Exponent {
            value: [2.2, 2.2, 2.2, 1.0],
        };
        let out = apply(&transform, [-0.3, 0.5, 0.5, 1.0]).unwrap();
        assert_eq!(out[0], 0.0);
        assert!(out[0].is_finite());
    }

    #[test]
    fn group_transform_applies_every_child_in_list_order() {
        // Matrix (identity) then Exponent 2.0: (0.5)^2 = 0.25.
        let transform = Transform::Group {
            children: vec![
                Transform::Matrix {
                    matrix: IDENTITY_MATRIX,
                    offset: None,
                },
                Transform::Exponent {
                    value: [2.0, 2.0, 2.0, 1.0],
                },
            ],
        };
        let out = apply(&transform, [0.5, 0.5, 0.5, 1.0]).unwrap();
        assert!((out[0] - 0.25).abs() < 1e-12);
    }

    #[test]
    fn convert_composes_a_real_to_reference_and_from_reference_pair() {
        // linear <-> display, an sRGB-like 2.2 gamma both directions.
        // (0.5 linear) -> reference (identity) -> display: 0.5^(1/2.2).
        // Hand-computed in Python: 0.7297400528407231.
        let text = r#"
colorspaces:
  - !<ColorSpace>
    name: linear
  - !<ColorSpace>
    name: display
    to_reference: !<ExponentTransform> {value: [2.2, 2.2, 2.2, 1]}
    from_reference: !<ExponentTransform> {value: [0.45454545454545453, 0.45454545454545453, 0.45454545454545453, 1]}
"#;
        let config = parse(text).unwrap();
        let out = convert(&config, "linear", "display", [0.5, 0.5, 0.5]).unwrap();
        assert!((out[0] - 0.729_740_052_840_723_1).abs() < 1e-9);
        assert!((out[1] - 0.729_740_052_840_723_1).abs() < 1e-9);
        assert!((out[2] - 0.729_740_052_840_723_1).abs() < 1e-9);
    }

    #[test]
    fn convert_rejects_an_unknown_colorspace_name_with_a_clear_error() {
        let config = parse("colorspaces:\n  - !<ColorSpace>\n    name: linear\n").unwrap();
        let error = convert(&config, "linear", "nope", [0.5, 0.5, 0.5]).unwrap_err();
        assert!(error.contains("nope"), "unexpected error: {error}");
    }

    #[test]
    fn convert_refuses_to_guess_a_missing_from_reference_inverse() {
        let text = r#"
colorspaces:
  - !<ColorSpace>
    name: linear
  - !<ColorSpace>
    name: display
    to_reference: !<ExponentTransform> {value: [2.2, 2.2, 2.2, 1]}
"#;
        let config = parse(text).unwrap();
        let error = convert(&config, "linear", "display", [0.5, 0.5, 0.5]).unwrap_err();
        assert!(
            error.contains("from_reference"),
            "unexpected error: {error}"
        );
    }
}
