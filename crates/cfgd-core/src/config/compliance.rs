use serde::{Deserialize, Serialize};

use super::source::default_true;

// ---------------------------------------------------------------------------
// Compliance configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComplianceConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_compliance_interval")]
    pub interval: String,
    #[serde(default = "default_compliance_retention")]
    pub retention: String,
    #[serde(default)]
    pub scope: ComplianceScope,
    #[serde(default)]
    pub export: ComplianceExport,
}

fn default_compliance_interval() -> String {
    "1h".into()
}
fn default_compliance_retention() -> String {
    "30d".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComplianceScope {
    #[serde(default = "default_true")]
    pub files: bool,
    #[serde(default = "default_true")]
    pub packages: bool,
    #[serde(default = "default_true")]
    pub system: bool,
    #[serde(default = "default_true")]
    pub secrets: bool,
    #[serde(default)]
    pub watch_paths: Vec<String>,
    #[serde(default)]
    pub watch_package_managers: Vec<String>,
}

impl Default for ComplianceScope {
    fn default() -> Self {
        Self {
            files: true,
            packages: true,
            system: true,
            secrets: true,
            watch_paths: Vec::new(),
            watch_package_managers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, schemars::JsonSchema)]
pub enum ComplianceFormat {
    #[default]
    Json,
    Yaml,
}

case_insensitive_enum!(ComplianceFormat {
    "Json" => ComplianceFormat::Json,
    "Yaml" => ComplianceFormat::Yaml,
});

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComplianceExport {
    #[serde(default)]
    pub format: ComplianceFormat,
    #[serde(default = "default_compliance_path")]
    pub path: String,
}

/// Default compliance export directory: `compliance/` under the state root.
///
/// Compliance exports are machine-local generated data, so they live with state
/// (`~/.local/state/cfgd/compliance/` on Linux), not in the data/share root.
/// Kept as a tilde literal rather than deriving from [`crate::state::default_state_dir`]
/// because this is a serde `default` (must be infallible `fn() -> String`) and the
/// value is a stored config string that consumers expand at use-time via
/// `expand_tilde` — baking an env-resolved absolute path here would break that
/// contract and config portability.
fn default_compliance_path() -> String {
    "~/.local/state/cfgd/compliance/".into()
}

impl Default for ComplianceExport {
    fn default() -> Self {
        Self {
            format: ComplianceFormat::default(),
            path: default_compliance_path(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compliance_format_parses_case_insensitively() {
        for token in ["json", "JSON", "Json", "jSoN"] {
            let parsed: ComplianceFormat = serde_yaml::from_str(token)
                .unwrap_or_else(|e| panic!("`{token}` should parse as Json: {e}"));
            assert_eq!(parsed, ComplianceFormat::Json, "token {token}");
        }
        for token in ["yaml", "YAML", "Yaml"] {
            let parsed: ComplianceFormat = serde_yaml::from_str(token)
                .unwrap_or_else(|e| panic!("`{token}` should parse as Yaml: {e}"));
            assert_eq!(parsed, ComplianceFormat::Yaml, "token {token}");
        }
    }

    #[test]
    fn compliance_format_rejects_garbage() {
        serde_yaml::from_str::<ComplianceFormat>("toml")
            .expect_err("unknown ComplianceFormat must error");
    }

    #[test]
    fn compliance_format_serializes_canonical_pascalcase() {
        let s = serde_yaml::to_string(&ComplianceFormat::Json).expect("serialize");
        assert_eq!(s.trim(), "Json");
    }

    #[test]
    fn default_compliance_path_resolves_under_state_root_not_share() {
        // The default export dir lives with state, not in the data/share root.
        assert_eq!(default_compliance_path(), "~/.local/state/cfgd/compliance/");
        assert!(!default_compliance_path().contains("/share/"));
    }

    #[test]
    #[serial_test::serial]
    fn default_compliance_path_expands_to_state_compliance_tail() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = crate::with_test_home(dir.path(), || {
            crate::expand_tilde(std::path::Path::new(&default_compliance_path()))
        });
        assert!(
            resolved.starts_with(dir.path()),
            "must expand under the test home, got: {}",
            resolved.display()
        );
        assert!(
            resolved.ends_with("state/cfgd/compliance"),
            "tail must be state/cfgd/compliance, got: {}",
            resolved.display()
        );
    }
}
