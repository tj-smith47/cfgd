use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeConfig {
    #[serde(default = "default_theme_name")]
    pub name: String,
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

// Unknown fields (legacy keys like `subheader`, `iconSuccess`, etc.) are silently
// ignored at the typed-deserialize layer; `parse::warn_on_legacy_theme_keys`
// surfaces them as `tracing::warn!` so users notice their override did nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeOverrides {
    // Style overrides (12) — hex colors applied on top of the active preset.
    pub header: Option<String>,
    pub success: Option<String>,
    pub warning: Option<String>,
    pub error: Option<String>,
    pub info: Option<String>,
    pub muted: Option<String>,
    pub running: Option<String>,
    pub diff_add: Option<String>,
    pub diff_remove: Option<String>,
    pub diff_context: Option<String>,
    pub accent: Option<String>,
    pub secondary: Option<String>,

    // Icon overrides (7) — single glyphs (or short strings) for status roles.
    pub icon_ok: Option<String>,
    pub icon_warn: Option<String>,
    pub icon_fail: Option<String>,
    pub icon_pending: Option<String>,
    pub icon_running: Option<String>,
    pub icon_skipped: Option<String>,
    pub icon_arrow: Option<String>,
}

impl ThemeOverrides {
    pub fn is_empty(&self) -> bool {
        self.header.is_none()
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
