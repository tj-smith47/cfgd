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
    /// A "command — description" list — `KvBlock`'s counterpart for a left
    /// column that is a shell command rather than a data-carrying key. See
    /// `Renderer::render_command_list` for why it needs its own layout
    /// (uncapped key column, `" — "` glue) rather than reusing `KvBlock`'s.
    ///
    /// Carries [`CommandPair`], not [`KvPair`]: nothing renders a command
    /// row's annotation (`render_doc` drops it converting to the renderer's
    /// `(String, String)` pairs), so `KvPair` let a caller build a
    /// human/JSON-divergent value the type should make unrepresentable.
    CommandList {
        pairs: Vec<CommandPair>,
    },
    Bullet {
        text: String,
    },
    /// A prose paragraph: wrapped body text with no glyph, no key column and
    /// no verbatim contract — what a documentation surface says ABOUT the
    /// thing the heading above it just named (`cfgd explain`'s description of
    /// a resource or a field).
    ///
    /// None of the neighbouring text components carries that: [`Note`] is
    /// Verbose-only and muted, [`Hint`] prefixes an arrow because it is advice
    /// about what to do next, [`Bullet`] is a list item, and [`CodeBlock`] is
    /// verbatim lines that must never wrap. A description is none of those —
    /// it is the body text of the document.
    ///
    /// [`Note`]: Component::Note
    /// [`Hint`]: Component::Hint
    /// [`Bullet`]: Component::Bullet
    /// [`CodeBlock`]: Component::CodeBlock
    Paragraph {
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
        /// Set by `Table::wrapping`: a cell too wide for its column wraps
        /// instead of truncating. Never serialized — display-only, so the
        /// JSON shape is the same with or without it.
        #[serde(skip)]
        wrap_cells: bool,
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
    /// A trailing note ABOUT the value, styled by the renderer and never by
    /// the caller (`(3 modules skipped: unsupported platform)`).
    ///
    /// The slot exists so a row that wants an annotation does not have to
    /// paint one into `value` itself: the renderer folds every key and value
    /// through [`crate::output::cursor_safe`], which would eat a caller's own
    /// SGR, and a value slot that sometimes carries styling cannot be folded
    /// at all. Split in two, the untrusted half is always folded and the
    /// styled half is always the renderer's — neither can be the other.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation: Option<String>,
    /// The role whose theme slot tints this row's VALUE, painted by the
    /// renderer after the fold. Same split as `annotation` and for the same
    /// reason: a caller cannot paint a value itself, so a row that must
    /// colour-code its value names the role and the renderer owns the coat.
    ///
    /// Never serialized — like [`Component::Section`]'s `owner`, the tint is
    /// display-only and a `-o json` reader sees the same plain `value` string
    /// with or without it.
    #[serde(skip)]
    pub value_role: Option<Role>,
}

impl KvPair {
    pub fn new(k: impl Into<String>, v: impl Into<String>) -> Self {
        Self {
            key: k.into(),
            value: v.into(),
            annotation: None,
            value_role: None,
        }
    }

    /// A pair whose value carries a trailing renderer-styled note.
    pub fn annotated(
        k: impl Into<String>,
        v: impl Into<String>,
        annotation: impl Into<String>,
    ) -> Self {
        Self {
            key: k.into(),
            value: v.into(),
            annotation: Some(annotation.into()),
            value_role: None,
        }
    }

    /// A pair whose value is tinted with `role`'s theme slot by the renderer
    /// (`Status  Drifted` in the warning colour).
    ///
    /// `role` is the same vocabulary a status line takes, resolved through the
    /// one `Role` → theme mapping (`renderer::role_glyph`): `Ok` → success,
    /// `Warn` → warning, `Fail` → error, `Skipped`/`Pending` → muted.
    pub fn role_valued(k: impl Into<String>, v: impl Into<String>, role: Role) -> Self {
        Self {
            key: k.into(),
            value: v.into(),
            annotation: None,
            value_role: Some(role),
        }
    }
}

/// A `command_list` row: a shell command and its description, nothing else.
///
/// `KvPair`'s `annotation` slot exists so a data-carrying row can style a
/// trailing note about its value; a `command_list` row has no such slot
/// rendered anywhere (`render_doc` converts `CommandList` to bare
/// `(String, String)` pairs before handing them to the renderer), so this
/// type carries only what can actually reach the screen.
#[derive(Debug, Clone, Serialize)]
pub struct CommandPair {
    pub key: String,
    pub value: String,
}

impl CommandPair {
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

    /// Pins `CommandList`'s wire shape at exactly `{"key": …, "value": …}` per
    /// pair — the same shape `KvPair::new(k, v)` (annotation `None`, skipped
    /// by `skip_serializing_if`) already produced before `CommandPair`
    /// replaced it, so this fix changes what the type can hold, never what
    /// `-o json` emits for an existing `command_list` payload.
    #[test]
    fn command_list_pair_serializes_as_key_value_only() {
        let c = Component::CommandList {
            pairs: vec![CommandPair::new("cfgd apply", "apply configuration")],
        };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["type"], "command_list");
        let first = &json["pairs"][0];
        assert_eq!(first["key"], "cfgd apply");
        assert_eq!(first["value"], "apply configuration");
        assert_eq!(
            first.as_object().unwrap().len(),
            2,
            "a command_list pair must serialize as exactly {{key, value}}; \
             an annotation field would diverge from the pre-fix KvPair(annotation: None) shape: {first}"
        );
    }
}
