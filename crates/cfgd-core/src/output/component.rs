use serde::Serialize;

use super::Role;

/// A node in a Doc's component tree. Streaming output does not produce these
/// (it pushes directly to the renderer); only `Doc` and `SectionBuilder` do.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Component {
    Heading {
        text: String,
    },
    KvBlock {
        pairs: Vec<KvPair>,
    },
    Bullet {
        text: String,
    },
    Status {
        role: Role,
        subject: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u128>,
        #[serde(skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        /// Trailing styled label (e.g. `[source-name]`). Rendered at the END of
        /// the subject by `render_doc` so the inner SGR reset can never be
        /// followed by outer-role-styled text — enforces the at-end layout that
        /// nested ANSI styling requires.
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<StatusLabel>,
    },
    Hint {
        text: String,
    },
    Note {
        text: String,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        /// Per-cell role tags, parallel to `rows`. Skipped from JSON when all
        /// cells are plain — keeps the structured-output shape stable for
        /// consumers that don't care about presentation styling.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        row_roles: Vec<Vec<Option<Role>>>,
    },
    Section {
        name: String,
        /// True for `section`; false for `section_or_collapse`.
        keep_when_empty: bool,
        /// Set when the user provided an explicit `empty_state(...)`.
        empty_state: Option<String>,
        children: Vec<Component>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusLabel {
    pub role: Role,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct KvPair {
    pub key: String,
    pub value: String,
}

impl KvPair {
    pub fn new(k: impl Into<String>, v: impl Into<String>) -> Self {
        Self {
            key: k.into(),
            value: v.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_serializes_with_type_tag() {
        let c = Component::Heading {
            text: "Status".into(),
        };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["type"], "heading");
        assert_eq!(json["text"], "Status");
    }

    #[test]
    fn status_omits_optional_fields_when_unset() {
        let c = Component::Status {
            role: Role::Ok,
            subject: "ok".into(),
            detail: None,
            duration_ms: None,
            target: None,
            label: None,
        };
        let json = serde_json::to_value(&c).unwrap();
        assert!(json.get("detail").is_none());
        assert!(json.get("duration_ms").is_none());
        assert!(json.get("target").is_none());
        assert!(json.get("label").is_none());
        assert_eq!(json["role"], "ok");
    }

    #[test]
    fn status_label_serializes_with_role_and_text() {
        let c = Component::Status {
            role: Role::Ok,
            subject: "ok".into(),
            detail: None,
            duration_ms: None,
            target: None,
            label: Some(StatusLabel {
                role: Role::Secondary,
                text: "[team-config]".into(),
            }),
        };
        let json = serde_json::to_value(&c).unwrap();
        let label = json.get("label").expect("label must serialize when set");
        assert_eq!(label["role"], "secondary");
        assert_eq!(label["text"], "[team-config]");
    }

    #[test]
    fn section_keep_when_empty_distinguishes_variants() {
        let plain = Component::Section {
            name: "X".into(),
            keep_when_empty: true,
            empty_state: None,
            children: vec![],
        };
        let collapse = Component::Section {
            name: "X".into(),
            keep_when_empty: false,
            empty_state: None,
            children: vec![],
        };
        let p = serde_json::to_value(&plain).unwrap();
        let c = serde_json::to_value(&collapse).unwrap();
        assert_eq!(p["keep_when_empty"], true);
        assert_eq!(c["keep_when_empty"], false);
    }
}
