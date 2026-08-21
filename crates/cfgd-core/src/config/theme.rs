use serde::{Deserialize, Serialize};

/// `spec.theme`: the active output preset and any per-color/icon overrides.
///
/// Accepts either a bare string (the preset name) or a mapping:
///
/// ```yaml
/// theme: dracula
/// # or
/// theme:
///   name: dracula
///   overrides:
///     header: "#ff0000"
///     iconOk: "Y"
/// ```
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ThemeConfig {
    /// Preset name (`default`, `dracula`, `solarized`, `minimal`, …). Default: `default`.
    #[serde(default = "default_theme_name")]
    pub name: String,
    /// Per-color and per-icon overrides applied on top of the named preset.
    #[serde(default, skip_serializing_if = "ThemeOverrides::is_empty")]
    pub overrides: ThemeOverrides,
}

fn default_theme_name() -> String {
    "default".to_string()
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: default_theme_name(),
            overrides: ThemeOverrides::default(),
        }
    }
}

// Accept both `theme: "dracula"` (string) and `theme: { name: dracula, overrides: ... }` (struct)
impl<'de> serde::Deserialize<'de> for ThemeConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct ThemeVisitor;
        impl<'de> de::Visitor<'de> for ThemeVisitor {
            type Value = ThemeConfig;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a theme name string or a theme config mapping")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<ThemeConfig, E> {
                Ok(ThemeConfig {
                    name: v.to_string(),
                    overrides: ThemeOverrides::default(),
                })
            }
            fn visit_map<M: de::MapAccess<'de>>(
                self,
                map: M,
            ) -> std::result::Result<ThemeConfig, M::Error> {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Inner {
                    #[serde(default = "default_theme_name")]
                    name: String,
                    #[serde(default)]
                    overrides: ThemeOverrides,
                }
                let inner = Inner::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(ThemeConfig {
                    name: inner.name,
                    overrides: inner.overrides,
                })
            }
        }
        deserializer.deserialize_any(ThemeVisitor)
    }
}

// no deny_unknown_fields — legacy theme keys (`subheader`, `iconSuccess`, etc.)
// are deliberately ignored at the typed-deserialize layer so old configs keep
// parsing; `parse::warn_on_legacy_theme_keys` collects them into
// `CfgdConfig.deprecations`, drained through `printer.deprecation()` at the
// command boundary, so users see their override did nothing and can migrate
// cleanly.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ThemeOverrides {
    // Style overrides (13) — hex colors applied on top of the active preset.
    /// Fills `Theme::primary`, the style an action subject at the deepest level
    /// of the run tree is painted with. Presets that carry no palette
    /// foreground of their own leave it unset.
    pub primary: Option<String>,
    /// Fills `Theme::header`, the style for a top-level heading. Hex color (`"#ff0000"`).
    pub header: Option<String>,
    /// Fills `Theme::success`, the style for `Role::Ok` status lines. Hex color.
    pub success: Option<String>,
    /// Fills `Theme::warning`, the style for `Role::Warn` status lines. Hex color.
    pub warning: Option<String>,
    /// Fills `Theme::error`, the style for `Role::Fail` status lines. Hex color.
    pub error: Option<String>,
    /// Fills `Theme::info`, the style for `Role::Info` status lines. Hex color.
    pub info: Option<String>,
    /// Fills `Theme::muted`, the style for de-emphasized text (hints, notes,
    /// qualifiers). Hex color.
    pub muted: Option<String>,
    /// Fills `Theme::running`, the style for `Role::Running` status lines and
    /// in-flight spinner labels. Hex color.
    pub running: Option<String>,
    /// Fills `Theme::diff_add`, the style for an added diff line. Hex color.
    pub diff_add: Option<String>,
    /// Fills `Theme::diff_remove`, the style for a removed diff line. Hex color.
    pub diff_remove: Option<String>,
    /// Fills `Theme::diff_context`, the style for an unchanged diff context line.
    /// Hex color.
    pub diff_context: Option<String>,
    /// Fills `Theme::accent`, the style for `Role::Accent` status lines
    /// ("attention without alarm"). Hex color.
    pub accent: Option<String>,
    /// Fills `Theme::secondary`, the style for `Role::Secondary` status lines
    /// (structural pivots, labels, identifiers). Hex color.
    pub secondary: Option<String>,

    // Icon overrides (8) — single glyphs (or short strings) for status roles.
    /// Glyph for `Role::Ok` status lines. Default varies by preset (e.g. `✓`).
    pub icon_ok: Option<String>,
    /// Glyph for `Role::Warn` status lines. Default varies by preset (e.g. `⚠`).
    pub icon_warn: Option<String>,
    /// Glyph for `Role::Fail` status lines. Default varies by preset (e.g. `✗`).
    pub icon_fail: Option<String>,
    /// Glyph for `Role::Pending` status lines. Default varies by preset.
    pub icon_pending: Option<String>,
    /// Glyph for `Role::Running` status lines. Default varies by preset.
    pub icon_running: Option<String>,
    /// Glyph for `Role::Skipped` status lines. Default varies by preset.
    pub icon_skipped: Option<String>,
    /// Glyph rendered for an `old -> new` relationship (e.g. `→`).
    pub icon_arrow: Option<String>,
    /// Glyph for `Role::Info` status lines. Default varies by preset.
    pub icon_info: Option<String>,
}

impl ThemeOverrides {
    pub fn is_empty(&self) -> bool {
        self.primary.is_none()
            && self.header.is_none()
            && self.success.is_none()
            && self.warning.is_none()
            && self.error.is_none()
            && self.info.is_none()
            && self.muted.is_none()
            && self.running.is_none()
            && self.diff_add.is_none()
            && self.diff_remove.is_none()
            && self.diff_context.is_none()
            && self.accent.is_none()
            && self.secondary.is_none()
            && self.icon_ok.is_none()
            && self.icon_warn.is_none()
            && self.icon_fail.is_none()
            && self.icon_pending.is_none()
            && self.icon_running.is_none()
            && self.icon_skipped.is_none()
            && self.icon_arrow.is_none()
            && self.icon_info.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_config_uses_default_name() {
        let tc = ThemeConfig::default();
        assert_eq!(tc.name, "default");
        assert!(tc.overrides.is_empty());
    }

    #[test]
    fn deserialize_string_shorthand() {
        let tc: ThemeConfig = serde_yaml::from_str("\"dracula\"").unwrap();
        assert_eq!(tc.name, "dracula");
        assert!(tc.overrides.is_empty());
    }

    #[test]
    fn deserialize_map_with_name_only() {
        let tc: ThemeConfig = serde_yaml::from_str("name: monokai").unwrap();
        assert_eq!(tc.name, "monokai");
        assert!(tc.overrides.is_empty());
    }

    #[test]
    fn deserialize_map_with_overrides() {
        let yaml = r##"
name: custom
overrides:
  header: "#ff0000"
  iconOk: "Y"
"##;
        let tc: ThemeConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(tc.name, "custom");
        assert_eq!(tc.overrides.header.as_deref(), Some("#ff0000"));
        assert_eq!(tc.overrides.icon_ok.as_deref(), Some("Y"));
        assert!(!tc.overrides.is_empty());
    }

    #[test]
    fn deserialize_map_defaults_name_when_omitted() {
        let tc: ThemeConfig = serde_yaml::from_str("overrides: {}").unwrap();
        assert_eq!(tc.name, "default");
    }

    #[test]
    fn overrides_is_empty_when_default() {
        let o = ThemeOverrides::default();
        assert!(o.is_empty());
    }

    #[test]
    fn overrides_not_empty_when_any_field_set() {
        let o = ThemeOverrides {
            error: Some("#f00".to_string()),
            ..ThemeOverrides::default()
        };
        assert!(!o.is_empty());
    }
}
