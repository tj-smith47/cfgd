use serde::{Deserialize, Serialize};

use super::{OwnerLabel, PaintedSubject, Role};

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
        /// Set when the SUBJECT is built from parts carrying DIFFERENT roles,
        /// painted by the renderer instead of coated with the row's single
        /// role: an owner token left to the role style paints `module:nvim`
        /// entirely green on an `Ok` row, and a transition paints its old
        /// status word in the new state's colour.
        ///
        /// Never serialized — `subject` carries the plain form, which is what
        /// `-o json` and every colourless path already read.
        #[serde(skip)]
        painted: Option<PaintedSubject>,
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        commands: Vec<String>,
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
        /// Set by `Table::owner_column`: headers whose cells are recorded
        /// owner tokens. Display-only, like `wrap_cells`.
        #[serde(skip)]
        owner_columns: Vec<String>,
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
    /// The owner tokens this row's VALUE reads as, each painted through
    /// [`OwnerLabel`]'s three slots by the renderer. Empty for every other row.
    ///
    /// Same split as `value_role` and for the same reason: `value` stays the
    /// plain string `-o json` carries and the widths are measured over, and
    /// the coat is the renderer's. Display-only, never serialized.
    #[serde(skip)]
    pub owners: Vec<OwnerLabel>,
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
}

impl KvPair {
    pub fn new(k: impl Into<String>, v: impl Into<String>) -> Self {
        Self {
            key: k.into(),
            value: v.into(),
            annotation: None,
            value_role: None,
            owners: Vec::new(),
            nested: false,
            link: None,
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

    /// A pair whose value is a RECORDED scope — `Scope  module:nvim`, or the
    /// `Profile  base` a profile-scoped run records instead.
    ///
    /// The ONE slot every surface rendering a recorded scope takes: it asks
    /// [`crate::output::owner_tokens`] whether the string is an owner list and
    /// hands the tokens to the renderer when it is, so a `kind:name` in a kv
    /// value wears the same tri-colour coat as the one a heading over the same
    /// owner renders. A profile name — anything that is not a token list —
    /// falls through to a plain row rather than being half-styled.
    pub fn scope_valued(k: impl Into<String>, recorded: impl Into<String>) -> Self {
        let recorded = recorded.into();
        let owners = crate::output::owner_tokens(&recorded).unwrap_or_default();
        Self {
            owners,
            ..Self::new(k, recorded)
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
    /// The theme's arrow glyph, joining the `inherits:` chain — an ASCII
    /// preset overriding `icon_arrow` must not leave a literal `→` in the
    /// header. Sourced from [`crate::output::Printer::arrow`] at every
    /// constructor.
    pub arrow: &'a str,
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
        arrow,
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
            rows.push(KvPair::annotated(
                "Profile",
                profile,
                format!("inherits: {}", profile_inherits.join(&format!(" {arrow} "))),
            ));
        }
    }
    rows.extend(modules_header_row_for(modules));
    rows
}

/// One module a `Modules` header row names: the module itself, whether a
/// `depends:` pulled it in, and the reason this host contributes no work for
/// it.
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
    /// Set when a `depends:` pulled this module in rather than the profile or
    /// the invocation naming it, as [`crate::modules::ResolvedModule`] claimed
    /// it. Such a module is the row's ANNOTATION, never one of its names.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dep_pulled: bool,
}

impl HeaderModule {
    /// The header's view of an ISOLATED run's resolution — a run whose
    /// invocation named its own modules (`--module nvim`).
    ///
    /// Empty when the resolution added nothing to what was named: a `Modules
    /// nvim` row under a command the reader spelled `--module nvim` states
    /// only what they typed. A `depends:` that folded another module in, or a
    /// `spec.platforms` gate that dropped one, is news the invocation did not
    /// carry, so the whole row renders with its annotation.
    pub fn of_isolate(modules: &[crate::modules::ResolvedModule]) -> Vec<Self> {
        let rows = Self::of_resolved(modules);
        match rows
            .iter()
            .any(|m| m.dep_pulled || m.platform_skip_reason.is_some())
        {
            true => rows,
            false => Vec::new(),
        }
    }

    /// The header's view of a resolution, in the resolver's own order.
    pub fn of_resolved(modules: &[crate::modules::ResolvedModule]) -> Vec<Self> {
        modules
            .iter()
            .map(|module| Self {
                name: module.name.clone(),
                platform_skip_reason: module.platform_skip_reason.clone(),
                dep_pulled: module.dep_pulled,
            })
            .collect()
    }
}

/// The `Modules` header row — the ONE builder of the row naming what a
/// resolved profile puts on this machine.
///
/// Sits directly under `Profile` on every surface that reports on a resolved
/// configuration: the run header, `cfgd status`, `cfgd diff`, `cfgd sync` and
/// `cfgd daemon status`. Only the run header printed it, so the README demo
/// opened on a `cfgd status` naming a profile and nothing it resolved to, two
/// commands above an apply header that named `nvim`.
///
/// The row NAMES what was declared and ANNOTATES what the resolution added —
/// `Modules  git, nvim (depends: plugins)` — the shape the `Profile` row
/// already uses for its `inherits:` chain, so nesting reads the same way on
/// both rows. A flat list hid which member nobody had asked for. A module
/// gated out by `spec.platforms` leaves the value the same way and returns as
/// a `skipped:` clause, after the `depends:` one when a row carries both. The
/// names and the annotation travel in separate slots because the renderer owns
/// the muted coat and the parentheses, and folds the names, which are
/// module-supplied. `None` when there is nothing at all to name, which renders
/// no row.
///
/// A surface with no plan to read skips off passes modules carrying none; a row
/// naming a COUNT of modules or a cache directory is a different fact and
/// carries a `// modules-row-ok: <why>` marker instead.
pub fn modules_header_row_for(modules: &[HeaderModule]) -> Option<KvPair> {
    let listed = |pulled: bool| {
        modules
            .iter()
            .filter(|m| m.platform_skip_reason.is_none() && m.dep_pulled == pulled)
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let named = listed(false);
    let mut clauses: Vec<String> = Vec::new();
    let pulled = listed(true);
    if !pulled.is_empty() {
        clauses.push(format!("depends: {pulled}"));
    }
    clauses.extend(
        modules
            .iter()
            .filter_map(|m| Some((m.name.as_str(), m.platform_skip_reason.as_deref()?)))
            .map(|(name, reason)| format!("{name} skipped: {reason}")),
    );
    if named.is_empty() && clauses.is_empty() {
        return None;
    }
    Some(KvPair::annotated("Modules", named, clauses.join(", ")))
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

/// A hint, and the commands it introduces.
///
/// A hint whose payload is a command reads two ways. Named mid-sentence, one
/// short command belongs in the sentence and keeps its backticks. Introduced
/// by a colon — or worse, two of them spliced with a comma — the sentence
/// becomes a paragraph the reader has to parse for the part they are meant to
/// type. `Register it as a Module pointing at the cluster's registry address:
/// "kubectl apply -f the module resource", or "--apply" next time` buried one
/// command inside prose that also named a flag.
///
/// So `commands` is the payload and `text` the sentence that introduces it.
/// The renderer owns the layout — the two-space indent and the muted `$ `
/// prefix — which is what makes every block on one screen line up and what
/// keeps a command COMPLETE as printed: what follows the `$ ` is exactly what
/// the reader copies. A caller supplies the bare command and never a prefix
/// of its own.
///
/// An empty `commands` is a plain prose hint, which is why `String` and
/// `&str` convert straight into one and every existing `hint` call site is
/// unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HintCommands {
    pub text: String,
    pub commands: Vec<String>,
}

impl HintCommands {
    /// A hint whose prose introduces `commands`. The prose ends on the colon
    /// that introduces them; the renderer supplies everything else.
    pub fn new<C: Into<String>>(
        text: impl Into<String>,
        commands: impl IntoIterator<Item = C>,
    ) -> Self {
        Self {
            text: text.into(),
            commands: commands.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<String> for HintCommands {
    fn from(text: String) -> Self {
        Self {
            text,
            commands: Vec::new(),
        }
    }
}

impl From<&str> for HintCommands {
    fn from(text: &str) -> Self {
        Self::from(text.to_string())
    }
}

impl From<&String> for HintCommands {
    fn from(text: &String) -> Self {
        Self::from(text.clone())
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
            arrow: "->",
        });
        let profile_row = rows
            .iter()
            .find(|row| row.key == "Profile")
            .expect("Profile row must render");
        assert_eq!(profile_row.value, "base");
        assert_eq!(
            profile_row.annotation.as_deref(),
            Some("inherits: core -> shared")
        );

        let rows = config_header_rows(&ConfigHeader {
            config_path: None,
            sources: &[],
            profile: Some("base"),
            profile_inherits: &[],
            modules: &[],
            arrow: "->",
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
            painted: None,
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
            painted: None,
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
            painted: None,
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
