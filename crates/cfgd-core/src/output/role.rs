use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Ok,
    Warn,
    Fail,
    Pending,
    Running,
    Skipped,
    Info,
    /// "Attention without alarm" — a terminal-positive notable change that does
    /// not warrant `Warn` severity. Mirrors `gh merged`, cargo's `Running`,
    /// homebrew's yellow-bg new-formula highlight. Suppressed at `Verbosity::Quiet`.
    Accent,
    /// "Structural pivot / label / identifier" — names a thing (a source, a
    /// scope, a module-kind) rather than carrying severity. Mirrors brew's
    /// `==>` bold-blue, kubecolor's resource-kind magenta. Suppressed at
    /// `Verbosity::Quiet`.
    Secondary,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_serializes_lowercase() {
        let json = serde_json::to_string(&Role::Ok).unwrap();
        assert_eq!(json, "\"ok\"");
        let json = serde_json::to_string(&Role::Fail).unwrap();
        assert_eq!(json, "\"fail\"");
    }

    #[test]
    fn role_round_trips() {
        for r in [
            Role::Ok,
            Role::Warn,
            Role::Fail,
            Role::Pending,
            Role::Running,
            Role::Skipped,
            Role::Info,
            Role::Accent,
            Role::Secondary,
        ] {
            let s = serde_json::to_string(&r).unwrap();
            let back: Role = serde_json::from_str(&s).unwrap();
            assert_eq!(r, back);
        }
    }

    #[test]
    fn accent_and_secondary_serialize_lowercase() {
        assert_eq!(serde_json::to_string(&Role::Accent).unwrap(), "\"accent\"");
        assert_eq!(
            serde_json::to_string(&Role::Secondary).unwrap(),
            "\"secondary\""
        );
    }
}
