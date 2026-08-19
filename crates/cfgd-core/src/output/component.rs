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
        /// A subject qualifier (`curl: missing`) — role slot / warning colon /
        /// muted qualifier. Landed right after the subject, ahead of `label`,
        /// by `render_doc`; the colon and qualifier text are always styled the
        /// same way (never a per-call role), unlike `label`.
        ///
        /// Deliberately NOT folded into `subject` the way [`super::TitleLabel`]
        /// folds its label/value into one `heading` string: `Component`
        /// serializes structurally (`#[derive(Serialize)]`), so a JSON reader
        /// sees `{"subject": "…", "qualifier": "…"}` as two fields, split from
        /// what the pre-`qualifier` spelling rendered as one `"subject": "curl:
        /// missing"` string. Every current call site is safe from that split
        /// because a `Doc` carrying a qualifier also carries `with_data`
        /// (`Doc::data_or_self_json` prefers it over `to_json_value`), so this
        /// field never reaches an actual `-o json` payload today — but the
        /// first `status_with(...).qualifier(...)` built WITHOUT `with_data`
        /// changes that Doc's JSON shape silently. A caller adding a
        /// qualifier to a Doc with no typed payload must add `with_data` in
        /// the same change, not rely on this field staying unread.
        #[serde(skip_serializing_if = "Option::is_none")]
        qualifier: Option<String>,
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
    /// A tight, copy-pasteable block of verbatim lines (e.g. a YAML snippet).
    /// Each entry is one physical line, newline-free; the renderer emits them
    /// contiguously with no per-line glyph and no blank lines between rows.
    CodeBlock {
        lines: Vec<String>,
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
        /// Set only by `SectionBuilder::new_owner` / `subsection_owner`:
        /// `name` is a `kind:name` owner token that renders through
        /// `OwnerLabel`'s three slots rather than the section's ordinary
        /// single-colour heading coat. Never serialized — the JSON shape is
        /// the same plain `name` string either way, so this changes only
        /// the human render.
        #[serde(skip)]
        owner: bool,
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
            qualifier: None,
            label: None,
        };
        let json = serde_json::to_value(&c).unwrap();
        assert!(json.get("detail").is_none());
        assert!(json.get("duration_ms").is_none());
        assert!(json.get("target").is_none());
        assert!(json.get("qualifier").is_none());
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
            qualifier: None,
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
    fn status_qualifier_serializes_as_a_plain_string() {
        let c = Component::Status {
            role: Role::Warn,
            subject: "curl".into(),
            detail: None,
            duration_ms: None,
            target: None,
            qualifier: Some("missing".into()),
            label: None,
        };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["qualifier"], "missing");
    }

    #[test]
    fn section_keep_when_empty_distinguishes_variants() {
        let plain = Component::Section {
            name: "X".into(),
            keep_when_empty: true,
            empty_state: None,
            owner: false,
            children: vec![],
        };
        let collapse = Component::Section {
            name: "X".into(),
            keep_when_empty: false,
            empty_state: None,
            owner: false,
            children: vec![],
        };
        let p = serde_json::to_value(&plain).unwrap();
        let c = serde_json::to_value(&collapse).unwrap();
        assert_eq!(p["keep_when_empty"], true);
        assert_eq!(c["keep_when_empty"], false);
    }
}
