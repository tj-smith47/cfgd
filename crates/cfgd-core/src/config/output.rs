use cfgd_schema::case_insensitive_enum;
use serde::{Deserialize, Serialize};

use super::theme::ThemeConfig;

/// `spec.output`: how cfgd renders what it reports.
///
/// ```yaml
/// spec:
///   output:
///     theme: dracula
///     usageHints: true
///     maskEnvValues: All
/// ```
///
/// Each key has a per-invocation override (`--theme` / `CFGD_THEME`,
/// `--no-hints` / `CFGD_USAGE_HINTS`, `--mask-env-values` /
/// `CFGD_MASK_ENV_VALUES`) that outranks what is stored here.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputConfig {
    /// Colours and glyphs cfgd renders with: a named preset (`default`,
    /// `dracula`, `solarized-dark`, `solarized-light`, `nord`, `monokai`,
    /// `adventure-time`, `catppuccin-mocha`, `gruvbox-dark`, `tokyo-night`,
    /// `one-dark`, `minimal`) plus per-slot overrides. Omitted, the
    /// `default` preset applies.
    #[serde(default)]
    pub theme: Option<ThemeConfig>,

    /// Whether closing `→` usage hints render. Omitted, hints render.
    #[serde(default)]
    pub usage_hints: Option<bool>,

    /// Which declared env values cfgd hides where it renders one. Omitted,
    /// every value is masked.
    #[serde(default)]
    pub mask_env_values: Option<MaskEnvValues>,
}

/// Which declared env values cfgd masks on the surfaces that render one
/// (`module show`, `profile show`, `source show`, `status <module>`).
///
/// A masked value renders as `***` plus its last three characters; the stored
/// value and `-o json` are untouched either way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub enum MaskEnvValues {
    /// Mask every declared value. The default.
    #[default]
    All,
    /// Mask only a value a declared secret exports: a name listed in any
    /// `spec.secrets[].envs` of the resolved chain. Every other value renders
    /// in full.
    Secrets,
    /// Mask nothing: every declared value renders in full, as though
    /// `--show-values` had been passed to every verb that takes it.
    None,
}

case_insensitive_enum!(MaskEnvValues {
    "All" => MaskEnvValues::All,
    "Secrets" => MaskEnvValues::Secrets,
    "None" => MaskEnvValues::None,
});

impl MaskEnvValues {
    /// Whether EVERY declared env value renders masked under this policy.
    ///
    /// The blunt question, for a caller with no name in hand. A caller
    /// rendering one named value asks `cli::EnvValueMasking::masks(name)`,
    /// which is the only reader that can answer for [`Self::Secrets`].
    #[must_use]
    pub fn masks(self) -> bool {
        matches!(self, Self::All)
    }

    /// Whether this policy masks a value only because a declared secret
    /// exports its name.
    #[must_use]
    pub fn masks_only_secrets(self) -> bool {
        matches!(self, Self::Secrets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_env_values_parses_every_casing_of_its_three_words() {
        for (raw, want) in [
            ("All", MaskEnvValues::All),
            ("all", MaskEnvValues::All),
            ("Secrets", MaskEnvValues::Secrets),
            ("secrets", MaskEnvValues::Secrets),
            ("SECRETS", MaskEnvValues::Secrets),
            ("NONE", MaskEnvValues::None),
            ("none", MaskEnvValues::None),
        ] {
            let parsed: MaskEnvValues = serde_yaml::from_str(raw).expect("value parses");
            assert_eq!(parsed, want, "{raw} must parse as {want:?}");
        }
        assert!(
            serde_yaml::from_str::<MaskEnvValues>("some").is_err(),
            "a word no variant spells must be refused"
        );
        assert!(
            MaskEnvValues::All.masks() && !MaskEnvValues::All.masks_only_secrets(),
            "All masks everything and is not the secrets-only policy"
        );
        assert!(
            !MaskEnvValues::Secrets.masks() && MaskEnvValues::Secrets.masks_only_secrets(),
            "Secrets masks by name, so the blunt question answers no"
        );
        assert!(
            !MaskEnvValues::None.masks() && !MaskEnvValues::None.masks_only_secrets(),
            "None masks nothing"
        );
    }

    #[test]
    fn the_output_block_reads_the_legacy_theme_shapes_under_its_own_key() {
        let cfg: OutputConfig =
            serde_yaml::from_str("theme: dracula\nusageHints: false\nmaskEnvValues: none\n")
                .expect("output block parses");
        assert_eq!(cfg.theme.expect("theme").name, "dracula");
        assert_eq!(cfg.usage_hints, Some(false));
        assert_eq!(cfg.mask_env_values, Some(MaskEnvValues::None));
    }
}
