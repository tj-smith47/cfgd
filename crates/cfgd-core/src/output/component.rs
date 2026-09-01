use serde::{Deserialize, Serialize};

use super::{OwnerLabel, Role};

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
        /// The health word a verdict-led detail opens on (`Synced`). The
        /// renderer paints it with the row's own role and renders `detail`
        /// as the muted parenthetical after it (`— Synced (24 packages)`),
        /// so the one word a reader scans a column of components for is the
        /// one styled span on the line. Same `-o json` caveat as
        /// `qualifier`: every current call site carries `with_data`.
        #[serde(skip_serializing_if = "Option::is_none")]
        verdict: Option<String>,
    },
    /// A child row belonging to the Status row above it: `subject — detail`,
    /// no glyph, one depth below its parent — the buffered-`Doc` twin of the
    /// streaming `SectionGuard::child_row` (a deploy row's per-file child, a
    /// drifted owner's nested finding). `label` is the same trailing styled
    /// slot `Status.label` carries, for a finding annotated with the source
    /// that declared it.
    ChildRow {
        subject: String,
        detail: String,
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
        /// A muted trailing annotation on the heading itself
        /// (`Component Health (checked 3m ago)`) — a fact ABOUT the rows the
        /// section holds, dated where the reader's eye already is rather than
        /// spent on a row of its own. Never serialized, like `owner`: the
        /// instant it renders stays a field of the command's own payload.
        #[serde(skip)]
        annotation: Option<String>,
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
    /// Whether this row is a BREAKDOWN of the row above it, indented two
    /// columns in from the block's key column (`Scripts 7` / `  preApply 1`).
    ///
    /// A renderer-owned slot rather than leading spaces in `key`, for the same
    /// reason `annotation` and `value_role` are: the renderer folds every key
    /// through [`crate::output::cursor_safe`] and pads it to a column measured
    /// over the rendered text, so an indent baked into the string is untrusted
    /// text that happens to look like layout — and the alignment is computed
    /// over the INDENTED width, which only the renderer can know.
    ///
    /// Never serialized: a `-o json` reader sees the same plain `key` either
    /// way, and the breakdown a nested row renders is its own payload field.
    #[serde(skip)]
    pub nested: bool,
    /// A URL the VALUE opens when the terminal renders OSC 8 hyperlinks; on
    /// one that does not, the renderer prints the URL itself in the value's
    /// place, because a terminal auto-links a full URL and never a partial
    /// path. Renderer-owned like the three slots above: the escape is styling,
    /// and `cursor_safe` would eat a caller's own.
    ///
    /// Never serialized — a `-o json` reader gets the URL through its own
    /// payload field (`docsUrl`), never through the display row.
    #[serde(skip)]
    pub link: Option<String>,
    /// The owner tokens this row's VALUE is made of, painted by the renderer
    /// through [`OwnerLabel`]'s three slots rather than the value's own single
    /// coat — so a `kind:name` in a kv value reads exactly as the apply tree's
    /// group heading and the Managed Resources Owner column render it.
    ///
    /// Renderer-owned for the same reason `value_role` is: the renderer folds
    /// every value through [`crate::output::cursor_safe`], which would eat a
    /// caller's own SGR, so the token can only be painted after the fold.
    ///
    /// Never serialized — `value` carries the plain `kind:name` list, which is
    /// what a `-o json` reader and every colourless path already see.
    #[serde(skip)]
    pub owners: Vec<OwnerLabel>,
}

impl KvPair {
    pub fn new(k: impl Into<String>, v: impl Into<String>) -> Self {
        Self {
            key: k.into(),
            value: v.into(),
            annotation: None,
            value_role: None,
            nested: false,
            link: None,
            owners: Vec::new(),
        }
    }

    /// A pair whose value is a LINK: `text` is what a hyperlink-capable
    /// terminal shows and `url` where a click lands; a terminal without
    /// hyperlinks shows `url` itself (`Docs  docs/spec/module.md#fields`
    /// on iTerm, the full GitHub URL under `| cat`).
    pub fn linked(k: impl Into<String>, text: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            link: Some(url.into()),
            ..Self::new(k, text)
        }
    }

    /// A row that breaks down the row above it, indented two columns in from
    /// the block's key column and aligned with it.
    pub fn nested(k: impl Into<String>, v: impl Into<String>) -> Self {
        Self {
            nested: true,
            ..Self::new(k, v)
        }
    }

    /// A pair whose value carries a trailing renderer-styled note.
    pub fn annotated(
        k: impl Into<String>,
        v: impl Into<String>,
        annotation: impl Into<String>,
    ) -> Self {
        Self {
            annotation: Some(annotation.into()),
            ..Self::new(k, v)
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
            value_role: Some(role),
            ..Self::new(k, v)
        }
    }

    /// A pair whose value is one or more owner tokens (`Scope  module:nvim`),
    /// each painted through [`OwnerLabel`]'s three slots by the renderer.
    ///
    /// Several owners join with `, `, the same separator the recorded scope of
    /// a multi-module run is stored with, so the row reads as one list.
    pub fn owner_valued(
        k: impl Into<String>,
        owners: impl IntoIterator<Item = OwnerLabel>,
    ) -> Self {
        let owners: Vec<OwnerLabel> = owners.into_iter().collect();
        let value = owners
            .iter()
            .map(OwnerLabel::plain)
            .collect::<Vec<_>>()
            .join(crate::reconciler::Owner::TOKEN_SEPARATOR);
        Self {
            owners,
            ..Self::new(k, value)
        }
    }
}

/// The four facts a header block states, declared once for every surface that
/// states them.
///
/// One type rather than a parameter list per caller: a fifth header fact is
/// then added where the four already live, and no surface can permit a shape
/// another refuses.
#[derive(Clone, Copy)]
pub struct ConfigHeader<'a> {
    /// The config file everything below was resolved from.
    pub config_path: Option<&'a std::path::Path>,
    /// The sources the config subscribes to, in declaration order.
    pub sources: &'a [crate::reconciler::ComposedSource],
    /// The resolved profile's name, `None` for a surface with none to name.
    pub profile: Option<&'a str>,
    /// The profile's resolved `inherits:` chain, nearest parent first
    /// (`["core", "shared"]` for `base` → `core` → `shared`). Empty renders a
    /// bare `Profile` row; never re-walked here — the caller reads it off
    /// wherever the profile was already resolved
    /// ([`crate::config::ResolvedProfile::inherits_chain`]).
    pub profile_inherits: &'a [String],
    /// The modules that profile puts on this machine, dependency-first.
    pub modules: &'a [HeaderModule],
}

/// The header block every surface reporting ON a resolved configuration opens
/// with: `Config`, `Sources`, `Profile`, `Modules`, in that order.
///
/// The ONE builder, so no surface can order the four differently or drop one.
/// `Sources` was the run header's own push and nothing else emitted it, so a
/// `cfgd status` named the config and the profile while the apply two commands
/// later also named who that profile had been composed FROM — one machine, two
/// headers, only one of which said where its configuration came from. The
/// order is causal: the config file names the sources, the sources deliver the
/// profile, the profile resolves to the modules.
///
/// Every input is what the CALLER resolved. A surface with no profile to name
/// (a `--module` isolate, a heading that already names it) passes `None` and
/// gets no `Profile` row; an empty `sources` or `modules` renders no row of its
/// own. The surface's own rows (`Trigger`, `Source`, `Phases`, `Actions`,
/// `PID`) follow what this returns.
pub fn config_header_rows(head: &ConfigHeader<'_>) -> Vec<KvPair> {
    let &ConfigHeader {
        config_path,
        sources,
        profile,
        profile_inherits,
        modules,
    } = head;
    let mut rows = Vec::new();
    if let Some(path) = config_path {
        // Folded like every action row under it: a header that spelled
        // `/home/tj/.config/cfgd/cfgd.yaml` six lines above `write ~/.cfgd.env`
        // named one directory two ways in one report.
        rows.push(KvPair::new(
            "Config",
            crate::fold_home_in_text(&crate::PathDisplayExt::display_posix(&path)),
        ));
    }
    if !sources.is_empty() {
        // A plain value rather than an annotation: the subscribed profile is
        // part of what the row states, not an aside about it.
        rows.push(KvPair::new(
            "Sources",
            sources
                .iter()
                .map(crate::reconciler::ComposedSource::display)
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if let Some(profile) = profile {
        if profile_inherits.is_empty() {
            rows.push(KvPair::new("Profile", profile));
        } else {
            // A literal `→` joining a LIST, not `Theme::arrow()` — that glyph
            // is reserved for an old->new relationship the theme may recolor;
            // an inheritance chain has no "new" half to tint.
            rows.push(KvPair::annotated(
                "Profile",
                profile,
                format!("inherits: {}", profile_inherits.join(" → ")),
            ));
        }
    }
    rows.extend(modules_header_row_for(modules));
    rows
}

/// The `Modules` header row — the ONE builder of the row naming what a
/// resolved profile puts on this machine.
///
/// Sits directly under `Profile` on every surface that reports on a resolved
/// profile: the run header, `cfgd status`, `cfgd diff`, `cfgd sync` and
/// `cfgd daemon status`. Only the run header printed it, so the README demo
/// opened on a `cfgd status` naming a profile and nothing it resolved to, two
/// commands above an apply header that named `nvim`.
///
/// A module `skips` names contributed no work, so it leaves the value and
/// returns as the annotation — the render of `PhaseName::Modules`, which
/// prints no block of its own. The names and the annotation travel in separate
/// slots because the renderer owns the muted coat and the parentheses, and
/// folds the names, which are module-supplied. `None` when a profile resolves
/// to nothing at all, which renders no row.
///
/// A surface with no plan to read skips off passes `&[]`; a row naming a COUNT
/// of modules or a cache directory is a different fact and carries a
/// `// modules-row-ok: <why>` marker instead.
pub fn modules_header_row(names: &[String], skips: &[(&str, &str)]) -> Option<KvPair> {
    let listed: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| !skips.iter().any(|(skipped, _)| skipped == name))
        .collect();
    let annotation = skips
        .iter()
        .map(|(name, reason)| format!("{name} skipped: {reason}"))
        .collect::<Vec<_>>()
        .join(", ");
    if listed.is_empty() && annotation.is_empty() {
        return None;
    }
    Some(KvPair::annotated("Modules", listed.join(", "), annotation))
}

/// One module a `Modules` header row names: the module itself, and the reason
/// this host contributes no work for it.
///
/// [`HeaderModule::of_resolved`] is the ONE derivation of a header row's
/// inputs from a resolution, and every surface reporting on a resolved profile
/// reads it — so membership (a `dependsOn` pulls its dependency into the set),
/// ORDER (the resolver returns them dependency-first) and GATING cannot differ
/// between two reports about one machine. `status` and the apply header
/// derived their own and named the profile's DECLARED list, so a profile of
/// one module that depends on another read `nvim` on three surfaces and
/// `base, nvim` on two.
///
/// Carried over the daemon's status wire because that reader is another
/// process: it holds no `ResolvedModule` of its own and must not re-derive one
/// from the declared list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderModule {
    pub name: String,
    /// The `spec.platforms` gate that took this module out of the run, as
    /// `ResolvedModule` recorded it — the same string the plan's `Skip` action
    /// carries, so the two renders of one gated module agree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_skip_reason: Option<String>,
}

impl HeaderModule {
    /// The header's view of a resolution, in the resolver's own order.
    pub fn of_resolved(modules: &[crate::modules::ResolvedModule]) -> Vec<Self> {
        modules
            .iter()
            .map(|module| Self {
                name: module.name.clone(),
                platform_skip_reason: module.platform_skip_reason.clone(),
            })
            .collect()
    }
}

/// The `Modules` header row for a caller holding a resolution — what every
/// surface but the run header reads.
///
/// The run header keeps [`modules_header_row`] itself: its skips come from the
/// plan's own `Skip` actions, which `Reconciler::plan` builds from the very
/// `platform_skip_reason` this reads, so the two cannot disagree.
pub fn modules_header_row_for(modules: &[HeaderModule]) -> Option<KvPair> {
    let names: Vec<String> = modules.iter().map(|m| m.name.clone()).collect();
    let skips: Vec<(&str, &str)> = modules
        .iter()
        .filter_map(|m| Some((m.name.as_str(), m.platform_skip_reason.as_deref()?)))
        .collect();
    modules_header_row(&names, &skips)
}

/// A `command_list` row: a shell command (or a `name <type>` pair) and its
/// description.
///
/// `KvPair`'s `annotation` slot exists so a data-carrying row can style a
/// trailing note about its value; a `command_list` row has no such slot, so
/// this type carries only what can actually reach the screen.
#[derive(Debug, Clone, Serialize)]
pub struct CommandPair {
    pub key: String,
    pub value: String,
    /// A span INSIDE `key` the renderer paints with its own type colour — the
    /// `<[]ModuleFileEntry>` half of a `cfgd explain` field row, so a field's
    /// name and its type read as two columns rather than one coat.
    ///
    /// The same split `KvPair::value_role` takes, for the same reason: the
    /// renderer folds `key` through [`crate::output::cursor_safe`], which
    /// would eat a coat a caller applied itself, so the caller names the span
    /// and the renderer owns the paint. Never serialized — display-only, and a
    /// `-o json` reader sees the same `{key, value}` row with or without it.
    #[serde(skip)]
    pub type_span: Option<String>,
}

impl CommandPair {
    pub fn new(k: impl Into<String>, v: impl Into<String>) -> Self {
        Self {
            key: k.into(),
            value: v.into(),
            type_span: None,
        }
    }

    /// A row whose `type_span` substring of `key` is painted with the
    /// renderer's type colour.
    pub fn typed(k: impl Into<String>, type_span: impl Into<String>, v: impl Into<String>) -> Self {
        Self {
            key: k.into(),
            value: v.into(),
            type_span: Some(type_span.into()),
        }
    }
}

impl<K: Into<String>, V: Into<String>> From<(K, V)> for CommandPair {
    fn from((k, v): (K, V)) -> Self {
        Self::new(k, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_header_row_annotates_its_inheritance_chain() {
        let chain = ["core".to_string(), "shared".to_string()];
        let rows = config_header_rows(&ConfigHeader {
            config_path: None,
            sources: &[],
            profile: Some("base"),
            profile_inherits: &chain,
            modules: &[],
        });
        let profile_row = rows
            .iter()
            .find(|row| row.key == "Profile")
            .expect("Profile row must render");
        assert_eq!(profile_row.value, "base");
        assert_eq!(
            profile_row.annotation.as_deref(),
            Some("inherits: core → shared")
        );

        let rows = config_header_rows(&ConfigHeader {
            config_path: None,
            sources: &[],
            profile: Some("base"),
            profile_inherits: &[],
            modules: &[],
        });
        let profile_row = rows
            .iter()
            .find(|row| row.key == "Profile")
            .expect("Profile row must render");
        assert_eq!(profile_row.value, "base");
        assert_eq!(profile_row.annotation, None);
    }

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
            verdict: None,
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
            verdict: None,
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
            verdict: None,
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
            annotation: None,
            children: vec![],
        };
        let collapse = Component::Section {
            name: "X".into(),
            keep_when_empty: false,
            empty_state: None,
            owner: false,
            annotation: None,
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
