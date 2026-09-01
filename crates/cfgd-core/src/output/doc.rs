use std::time::Duration;

use serde::Serialize;

use super::OwnerLabel;
use super::Role;
use super::TitleLabel;
use super::component::{CommandPair, Component, KvPair, StatusLabel};
use super::renderer::Table;

/// A `Doc`'s top-level heading, deferred past construction because the
/// styled render needs a `Theme` a `Doc` is built without — `render_doc`
/// is the first point in the pipeline that holds one.
pub(crate) enum HeadingKind {
    Plain(String),
    Title(TitleLabel),
    OwnerPrefixed { prefix: String, owner: OwnerLabel },
}

impl HeadingKind {
    /// The uncoloured form every structured/quiet/`-o json` path reads;
    /// `to_json_value`'s `"heading"` field is always this string, whichever
    /// variant produced it.
    fn plain_text(&self) -> String {
        match self {
            HeadingKind::Plain(text) => text.clone(),
            HeadingKind::Title(label) => label.plain(),
            HeadingKind::OwnerPrefixed { prefix, owner } => format!("{prefix} {}", owner.plain()),
        }
    }
}

/// One status row's optional fields, used by `status_with`.
#[derive(Default)]
pub struct StatusFields {
    pub detail: Option<String>,
    pub duration: Option<Duration>,
    pub target: Option<String>,
    pub qualifier: Option<String>,
    pub label: Option<StatusLabel>,
    pub verdict: Option<String>,
}

impl StatusFields {
    pub fn detail(mut self, s: impl Into<String>) -> Self {
        self.detail = Some(s.into());
        self
    }
    pub fn detail_opt(mut self, s: Option<&str>) -> Self {
        self.detail = s.map(|x| x.to_string());
        self
    }
    /// A planned-vs-actual mismatch's detail slot: `want: <expected>, have:
    /// <actual>`, composed through [`super::drift_detail`] — the same
    /// canonical spelling `StatusBuilder::drift` composes for the streaming
    /// path, so the Doc and streaming renders of one mismatch never diverge.
    pub fn drift(self, expected: impl std::fmt::Display, actual: impl std::fmt::Display) -> Self {
        self.detail(super::drift_detail(expected, actual))
    }

    pub fn duration(mut self, d: Duration) -> Self {
        self.duration = Some(d);
        self
    }
    pub fn target(mut self, s: impl Into<String>) -> Self {
        self.target = Some(s.into());
        self
    }
    /// A subject qualifier (`curl: missing`) — role slot / warning colon /
    /// muted qualifier, composed through `super::renderer::finalize_subject`
    /// at render time. Lands ahead of `label` in the same at-end-of-subject
    /// slot; the colon and qualifier text are always styled the same way,
    /// never a per-call role.
    pub fn qualifier(mut self, text: impl Into<String>) -> Self {
        self.qualifier = Some(text.into());
        self
    }
    /// Trailing styled label (e.g. `[source-name]`). Rendered at the END of
    /// the subject so the inner SGR reset cannot be followed by outer-role
    /// styled text — the only safe nesting shape for the streaming renderer.
    pub fn label(mut self, role: Role, text: impl Into<String>) -> Self {
        self.label = Some(StatusLabel {
            role,
            text: text.into(),
        });
        self
    }
    /// A verdict-led detail: the health word (`Synced`, `Failed`) painted with
    /// the row's own role at render time, with `detail` demoted to the muted
    /// parenthetical after it — `— Synced (24 packages, 6 files)`. The one way
    /// a detail slot's leading word takes the role's colour: painted at a call
    /// site the coat is eaten by the renderer's `cursor_safe` fold, and inside
    /// a plain detail string the verdict is one more muted clause, invisible
    /// beside counts that read the same on a healthy component and a broken
    /// one. Pinned by `component_health_lists_every_owner_with_a_themed_verdict`.
    pub fn verdict(mut self, word: impl Into<String>) -> Self {
        self.verdict = Some(word.into());
        self
    }
}

/// Top-level buffered document. Built then handed to `Printer::emit`.
pub struct Doc {
    pub(crate) heading: Option<HeadingKind>,
    pub(crate) children: Vec<Component>,
    /// Optional payload that REPLACES Doc-derived JSON in structured modes.
    pub(crate) data: Option<serde_json::Value>,
    /// Set only by [`super::error_doc`]. A selector output format
    /// (`jsonpath=`/`template=`/`name`) applies the reader's SUCCESS-shaped
    /// selector to whatever this doc happens to carry, and an error doc's
    /// shape (`error`/`message`/`name`) is not that shape — the selector
    /// almost always misses, printing nothing to stdout, and unlike `json`/
    /// `yaml` (which dump the whole payload regardless of selector) that
    /// leaves NO diagnostic anywhere. `emit_structured` reads this flag to
    /// echo the failure to stderr regardless of which selector was asked for,
    /// the same "always visible independent of `-o`" guarantee `alert`/
    /// `deprecation` already give a run-affecting notice.
    pub(crate) is_error: bool,
}

impl Default for Doc {
    fn default() -> Self {
        Self::new()
    }
}

impl Doc {
    pub fn new() -> Self {
        Self {
            heading: None,
            children: Vec::new(),
            data: None,
            is_error: false,
        }
    }

    pub fn heading(mut self, text: impl Into<String>) -> Self {
        self.heading = Some(HeadingKind::Plain(text.into()));
        self
    }

    /// A structured `Label: value` heading (`Status: dev-tools`), styled
    /// through [`TitleLabel`]'s three slots at render time instead of
    /// `heading`'s single `theme.header` coat.
    pub fn heading_title(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.heading = Some(HeadingKind::Title(TitleLabel::new(label, value)));
        self
    }

    /// A `Label: value` heading whose value carries a schema TYPE span
    /// (`Explain: module.spec.files <[]ModuleFileEntry>`), painted with the
    /// type slot instead of the value's accent coat — the same slot the field
    /// rows below it render their own types in.
    pub fn heading_title_typed(
        mut self,
        label: impl Into<String>,
        value: impl Into<String>,
        type_span: impl Into<String>,
    ) -> Self {
        self.heading = Some(HeadingKind::Title(TitleLabel::typed(
            label, value, type_span,
        )));
        self
    }

    /// A `<Verb> <kind>:<name>` heading (`Show source:team`) — the buffered
    /// counterpart of [`super::Printer::heading_owner_prefixed`], and the only
    /// heading slot an owner token may occupy. A command whose whole report is
    /// about one owned thing names it this way rather than inventing a second
    /// noun for the owner's kind in a `Label: value` title.
    pub fn heading_owner_prefixed(mut self, prefix: impl Into<String>, owner: OwnerLabel) -> Self {
        self.heading = Some(HeadingKind::OwnerPrefixed {
            prefix: prefix.into(),
            owner,
        });
        self
    }

    pub fn kv(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        // Consecutive standalone kv() calls must render as one aligned block
        // (the buffered surface mirrors the streaming auto-batching rule).
        let pair = KvPair::new(key, value);
        if let Some(Component::KvBlock { pairs }) = self.children.last_mut() {
            pairs.push(pair);
        } else {
            self.children.push(Component::KvBlock { pairs: vec![pair] });
        }
        self
    }

    pub fn kv_block<I, K, V>(mut self, pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let pairs: Vec<KvPair> = pairs.into_iter().map(|(k, v)| KvPair::new(k, v)).collect();
        if !pairs.is_empty() {
            // kv_block is an explicit batch — it does NOT coalesce with prior
            // KvBlock children. Author intent matters here; consecutive
            // kv_block calls remain as separate aligned blocks.
            self.children.push(Component::KvBlock { pairs });
        }
        self
    }

    /// `kv_block` over rows built by hand, so a row can carry an annotation
    /// ([`KvPair::annotated`]) or a role-tinted value
    /// ([`KvPair::role_valued`]) — the buffered counterpart of
    /// [`crate::output::SectionGuard::kv_rows`].
    ///
    /// Those two slots are the ONLY ways styling reaches a kv value: the
    /// renderer folds every key and value through
    /// [`crate::output::cursor_safe`], which would eat a coat a caller painted
    /// on itself. Reach for `kv_block` when no row needs one.
    pub fn kv_rows(mut self, rows: impl IntoIterator<Item = KvPair>) -> Self {
        let pairs: Vec<KvPair> = rows.into_iter().collect();
        if !pairs.is_empty() {
            self.children.push(Component::KvBlock { pairs });
        }
        self
    }

    /// A "command — description" list (see [`Component::CommandList`]) —
    /// `kv_block`'s counterpart for a left column that is a shell command
    /// rather than a data-carrying key.
    pub fn command_list<I>(mut self, pairs: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<CommandPair>,
    {
        let pairs: Vec<CommandPair> = pairs.into_iter().map(Into::into).collect();
        if !pairs.is_empty() {
            self.children.push(Component::CommandList { pairs });
        }
        self
    }

    pub fn status(mut self, role: Role, subject: impl Into<String>) -> Self {
        self.children.push(Component::Status {
            role,
            subject: subject.into(),
            detail: None,
            duration_ms: None,
            target: None,
            qualifier: None,
            label: None,
            verdict: None,
        });
        self
    }

    pub fn status_with(
        mut self,
        role: Role,
        subject: impl Into<String>,
        build: impl FnOnce(StatusFields) -> StatusFields,
    ) -> Self {
        let f = build(StatusFields::default());
        self.children.push(Component::Status {
            role,
            subject: subject.into(),
            detail: f.detail,
            duration_ms: f.duration.map(|d| d.as_millis()),
            target: f.target,
            qualifier: f.qualifier,
            label: f.label,
            verdict: f.verdict,
        });
        self
    }

    pub fn hint(mut self, text: impl Into<String>) -> Self {
        self.children.push(Component::Hint { text: text.into() });
        self
    }

    /// Append a prose paragraph (see [`Component::Paragraph`]) — body text
    /// about whatever the heading above it named.
    ///
    /// Empty text appends nothing, so a caller rendering a description that
    /// may be absent (a schema field carrying no rustdoc) does not branch and
    /// cannot leave an empty line behind.
    pub fn paragraph(mut self, text: impl Into<String>) -> Self {
        let text = text.into();
        if !text.is_empty() {
            self.children.push(Component::Paragraph { text });
        }
        self
    }

    pub fn note(mut self, text: impl Into<String>) -> Self {
        self.children.push(Component::Note { text: text.into() });
        self
    }

    /// Append a tight, copy-pasteable block of verbatim lines (e.g. a YAML
    /// snippet). Each entry is one physical line; the renderer emits them
    /// contiguously with no `→` glyph and no blank lines between rows.
    pub fn code_block(mut self, lines: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.children.push(Component::CodeBlock {
            lines: lines.into_iter().map(Into::into).collect(),
        });
        self
    }

    pub fn table(mut self, t: Table) -> Self {
        self.children.push(Component::Table {
            headers: t.headers,
            rows: t.rows,
            row_roles: t.row_roles,
            wrap_cells: t.wrap_cells,
        });
        self
    }

    pub fn section<F>(mut self, name: impl Into<String>, build: F) -> Self
    where
        F: FnOnce(SectionBuilder) -> SectionBuilder,
    {
        let sb = build(SectionBuilder::new(name, /*keep_when_empty=*/ true));
        self.children.push(sb.into_component());
        self
    }

    /// A top-level section headed by a styled owner token (`source:acme`) —
    /// the buffered counterpart of [`super::Printer::section_owner`], and the
    /// shape an owner token takes in a `Doc`. An owner names WHOSE the rows
    /// below it are, which is a section's job; a `Doc` heading names the
    /// report, so a bare `kind:name` never occupies the heading slot.
    pub fn section_owner<F>(mut self, owner: &OwnerLabel, build: F) -> Self
    where
        F: FnOnce(SectionBuilder) -> SectionBuilder,
    {
        let sb = build(SectionBuilder::new_owner(
            owner, /*keep_when_empty=*/ true,
        ));
        self.children.push(sb.into_component());
        self
    }

    pub fn section_or_collapse<F>(mut self, name: impl Into<String>, build: F) -> Self
    where
        F: FnOnce(SectionBuilder) -> SectionBuilder,
    {
        let sb = build(SectionBuilder::new(name, /*keep_when_empty=*/ false));
        self.children.push(sb.into_component());
        self
    }

    pub fn section_if_nonempty<T, F>(self, name: impl Into<String>, items: &[T], build: F) -> Self
    where
        F: FnOnce(SectionBuilder, &[T]) -> SectionBuilder,
    {
        if items.is_empty() {
            return self;
        }
        let mut s = self;
        let sb = build(SectionBuilder::new(name, true), items);
        s.children.push(sb.into_component());
        s
    }

    /// A section whose heading carries a muted trailing annotation —
    /// `Component Health (checked 3m ago)`. The annotation is a fact ABOUT
    /// the rows below, dated on the heading rather than spent on a row of its
    /// own; the renderer owns the parentheses and the muted coat, so a caller
    /// can neither paint it nor promote it to a row by accident.
    pub fn section_annotated<F>(
        mut self,
        name: impl Into<String>,
        annotation: impl Into<String>,
        build: F,
    ) -> Self
    where
        F: FnOnce(SectionBuilder) -> SectionBuilder,
    {
        let mut sb = SectionBuilder::new(name, /*keep_when_empty=*/ true);
        sb.annotation = Some(annotation.into());
        self.children.push(build(sb).into_component());
        self
    }

    /// Attach a typed payload that REPLACES Doc-derived JSON in structured modes.
    pub fn with_data<T: Serialize>(mut self, value: T) -> Self {
        self.data = Some(serde_json::to_value(&value).unwrap_or(serde_json::Value::Null));
        self
    }

    /// Convert the Doc into a JSON value (excluding `data`); used by tests +
    /// the structured emit path when no `with_data` was set.
    pub(crate) fn to_json_value(&self) -> serde_json::Value {
        let children: Vec<serde_json::Value> = self
            .children
            .iter()
            .map(|c| serde_json::to_value(c).unwrap_or(serde_json::Value::Null))
            .collect();
        let mut obj = serde_json::Map::new();
        if let Some(h) = &self.heading {
            obj.insert("heading".into(), serde_json::Value::String(h.plain_text()));
        }
        obj.insert("children".into(), serde_json::Value::Array(children));
        serde_json::Value::Object(obj)
    }

    pub(crate) fn data_or_self_json(&self) -> serde_json::Value {
        self.data.clone().unwrap_or_else(|| self.to_json_value())
    }

    /// The failure's human message, when this doc [`Self::is_error`] — the
    /// subject of its (always-present, `error_doc`-authored) `Role::Fail`
    /// status. Used by `emit_structured`'s selector formats to echo the
    /// failure to stderr; a call site that never built the doc through
    /// `error_doc` sees `None` regardless of what its own children happen to
    /// contain.
    pub(crate) fn error_message(&self) -> Option<&str> {
        if !self.is_error {
            return None;
        }
        self.children.iter().find_map(|c| match c {
            Component::Status {
                role: Role::Fail,
                subject,
                ..
            } => Some(subject.as_str()),
            _ => None,
        })
    }
}

/// Builder for one Section. Same vocabulary as Doc plus `subsection`.
pub struct SectionBuilder {
    name: String,
    /// Set only by [`Self::new_owner`]: the section's `name` is a
    /// `kind:name` owner token that should render through [`OwnerLabel`]'s
    /// three slots rather than `render_section_open`'s single `theme.header`/
    /// `theme.secondary` coat. Never serialized — `Component::Section`'s
    /// `name` stays the same plain string either way, so the flag changes
    /// only the human render, never the `-o json` shape.
    owner: bool,
    keep_when_empty: bool,
    empty_state: Option<String>,
    /// Set only by [`Doc::section_annotated`]: a muted trailing annotation on
    /// the heading. Display-only, like `owner`.
    annotation: Option<String>,
    children: Vec<Component>,
}

impl SectionBuilder {
    pub(crate) fn new(name: impl Into<String>, keep_when_empty: bool) -> Self {
        Self {
            name: name.into(),
            owner: false,
            keep_when_empty,
            empty_state: None,
            annotation: None,
            children: Vec::new(),
        }
    }

    /// A section headed by a styled owner token (`module:nvim`) — the
    /// buffered-`Doc` counterpart of [`super::Printer::section_owner`].
    pub(crate) fn new_owner(owner: &OwnerLabel, keep_when_empty: bool) -> Self {
        Self {
            name: owner.plain(),
            owner: true,
            keep_when_empty,
            empty_state: None,
            annotation: None,
            children: Vec::new(),
        }
    }

    pub(crate) fn into_component(self) -> Component {
        Component::Section {
            name: self.name,
            owner: self.owner,
            keep_when_empty: self.keep_when_empty,
            empty_state: self.empty_state,
            annotation: self.annotation,
            children: self.children,
        }
    }

    pub fn bullet(mut self, text: impl Into<String>) -> Self {
        self.children.push(Component::Bullet { text: text.into() });
        self
    }

    pub fn kv(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        // Coalesce consecutive standalone kv() calls into one aligned block.
        let pair = KvPair::new(key, value);
        if let Some(Component::KvBlock { pairs }) = self.children.last_mut() {
            pairs.push(pair);
        } else {
            self.children.push(Component::KvBlock { pairs: vec![pair] });
        }
        self
    }

    pub fn kv_block<I, K, V>(mut self, pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let pairs: Vec<KvPair> = pairs.into_iter().map(|(k, v)| KvPair::new(k, v)).collect();
        if !pairs.is_empty() {
            self.children.push(Component::KvBlock { pairs });
        }
        self
    }

    /// [`Doc::kv_rows`], nested: rows built by hand so one can carry an
    /// annotation or a role-tinted value.
    pub fn kv_rows(mut self, rows: impl IntoIterator<Item = KvPair>) -> Self {
        let pairs: Vec<KvPair> = rows.into_iter().collect();
        if !pairs.is_empty() {
            self.children.push(Component::KvBlock { pairs });
        }
        self
    }

    /// A "command — description" list (see [`Component::CommandList`]) —
    /// `kv_block`'s counterpart for a left column that is a shell command
    /// rather than a data-carrying key.
    pub fn command_list<I>(mut self, pairs: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<CommandPair>,
    {
        let pairs: Vec<CommandPair> = pairs.into_iter().map(Into::into).collect();
        if !pairs.is_empty() {
            self.children.push(Component::CommandList { pairs });
        }
        self
    }

    pub fn status(mut self, role: Role, subject: impl Into<String>) -> Self {
        self.children.push(Component::Status {
            role,
            subject: subject.into(),
            detail: None,
            duration_ms: None,
            target: None,
            qualifier: None,
            label: None,
            verdict: None,
        });
        self
    }

    pub fn status_with(
        mut self,
        role: Role,
        subject: impl Into<String>,
        build: impl FnOnce(StatusFields) -> StatusFields,
    ) -> Self {
        let f = build(StatusFields::default());
        self.children.push(Component::Status {
            role,
            subject: subject.into(),
            detail: f.detail,
            duration_ms: f.duration.map(|d| d.as_millis()),
            target: f.target,
            qualifier: f.qualifier,
            label: f.label,
            verdict: f.verdict,
        });
        self
    }

    pub fn hint(mut self, text: impl Into<String>) -> Self {
        self.children.push(Component::Hint { text: text.into() });
        self
    }

    /// The nested counterpart of [`Doc::paragraph`] — prose about whatever the
    /// section heading above it named, for a description that belongs to a
    /// section rather than to the report (a source's summary of one profile it
    /// provides). Empty text appends nothing, same as the `Doc` form.
    pub fn paragraph(mut self, text: impl Into<String>) -> Self {
        let text = text.into();
        if !text.is_empty() {
            self.children.push(Component::Paragraph { text });
        }
        self
    }

    pub fn note(mut self, text: impl Into<String>) -> Self {
        self.children.push(Component::Note { text: text.into() });
        self
    }

    pub fn table(mut self, t: Table) -> Self {
        self.children.push(Component::Table {
            headers: t.headers,
            rows: t.rows,
            row_roles: t.row_roles,
            wrap_cells: t.wrap_cells,
        });
        self
    }

    pub fn empty_state(mut self, text: impl Into<String>) -> Self {
        self.empty_state = Some(text.into());
        self
    }

    pub fn subsection<F>(mut self, name: impl Into<String>, build: F) -> Self
    where
        F: FnOnce(SectionBuilder) -> SectionBuilder,
    {
        let sb = build(SectionBuilder::new(name, /*keep_when_empty=*/ true));
        self.children.push(sb.into_component());
        self
    }

    /// A nested subsection headed by a styled owner token (`source:acme`)
    /// instead of a hand-built `format!("{kind}:{name}")` string.
    pub fn subsection_owner<F>(mut self, owner: &OwnerLabel, build: F) -> Self
    where
        F: FnOnce(SectionBuilder) -> SectionBuilder,
    {
        let sb = build(SectionBuilder::new_owner(
            owner, /*keep_when_empty=*/ true,
        ));
        self.children.push(sb.into_component());
        self
    }

    pub fn subsection_if_nonempty<T, F>(
        mut self,
        name: impl Into<String>,
        items: &[T],
        build: F,
    ) -> Self
    where
        F: FnOnce(SectionBuilder, &[T]) -> SectionBuilder,
    {
        if items.is_empty() {
            return self;
        }
        let sb = build(SectionBuilder::new(name, true), items);
        self.children.push(sb.into_component());
        self
    }

    /// Helper: extend a builder by iterating `items`, applying `build` for each.
    /// Avoids the `let mut s = s; for ... { s = ... } s` boilerplate.
    pub fn extend<I, F>(mut self, items: I, mut build: F) -> Self
    where
        I: IntoIterator,
        F: FnMut(SectionBuilder, I::Item) -> SectionBuilder,
    {
        for item in items {
            self = build(self, item);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_doc_serializes_minimally() {
        let d = Doc::new();
        let v = d.to_json_value();
        assert_eq!(v["children"].as_array().unwrap().len(), 0);
        assert!(v.get("heading").is_none());
    }

    #[test]
    fn heading_and_kv_round_trip() {
        let d = Doc::new().heading("Status").kv("Profile", "dev");
        let v = d.to_json_value();
        assert_eq!(v["heading"], "Status");
        let kids = v["children"].as_array().unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0]["type"], "kv_block");
        assert_eq!(kids[0]["pairs"][0]["key"], "Profile");
    }

    /// `heading_title`'s JSON field is the plain `Label: value` string, the
    /// same shape `heading` produces — a `-o json` reader must not see the
    /// two builder entry points as two different fields.
    #[test]
    fn heading_title_serializes_as_the_plain_label_colon_value_string() {
        let d = Doc::new().heading_title("Status", "dev-tools");
        let v = d.to_json_value();
        assert_eq!(v["heading"], "Status: dev-tools");
    }

    /// The owner-prefixed heading serializes as the same `prefix owner` plain
    /// string `Printer::heading_owner_prefixed` writes uncoloured — a `-o json`
    /// reader sees one `heading` field whichever of the three builders a
    /// command reached for, and the owner token keeps the `kind:name` spelling
    /// every other surface matches on.
    #[test]
    fn heading_owner_prefixed_serializes_as_the_plain_prefixed_owner_token() {
        let d = Doc::new().heading_owner_prefixed("Show", OwnerLabel::new("source", "team"));
        let v = d.to_json_value();
        assert_eq!(v["heading"], "Show source:team");
    }

    #[test]
    fn section_if_nonempty_skips_empty() {
        let d: Doc = Doc::new().section_if_nonempty::<i32, _>("Items", &[], |s, _| s);
        assert_eq!(d.children.len(), 0);
    }

    #[test]
    fn section_if_nonempty_emits_when_present() {
        let d = Doc::new().section_if_nonempty("Items", &[1, 2, 3], |s, items| {
            let mut s = s;
            for i in items {
                s = s.bullet(format!("{i}"));
            }
            s
        });
        let v = d.to_json_value();
        let kids = v["children"].as_array().unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0]["type"], "section");
        assert_eq!(kids[0]["children"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn extend_threads_correctly() {
        let s = SectionBuilder::new("X", true)
            .extend([1, 2, 3], |sb, n| sb.bullet(format!("item {n}")));
        let c = s.into_component();
        if let Component::Section { children, .. } = c {
            assert_eq!(children.len(), 3);
        } else {
            panic!("expected Section");
        }
    }

    #[test]
    fn consecutive_kvs_coalesce_in_doc() {
        // Doc::kv must coalesce consecutive standalone calls into one aligned
        // block. The trailing `.note(...)` is a non-Kv child used to verify
        // the coalescing boundary — Kv runs end when a non-Kv child arrives.
        let d = Doc::new().kv("Foo", "1").kv("LongerKey", "2").note("Next");
        assert_eq!(d.children.len(), 2, "expected coalesced kvs + note");
        if let Component::KvBlock { pairs } = &d.children[0] {
            assert_eq!(pairs.len(), 2);
            assert_eq!(pairs[0].key, "Foo");
            assert_eq!(pairs[1].key, "LongerKey");
        } else {
            panic!(
                "expected first child to be a coalesced KvBlock; got {:?}",
                d.children[0]
            );
        }
    }

    #[test]
    fn consecutive_kvs_coalesce_in_section_builder() {
        let s = SectionBuilder::new("X", true)
            .kv("Foo", "1")
            .kv("LongerKey", "2")
            .bullet("After"); // non-Kv child should break coalescing
        let c = s.into_component();
        if let Component::Section { children, .. } = c {
            assert_eq!(children.len(), 2, "expected coalesced KvBlock + Bullet");
            if let Component::KvBlock { pairs } = &children[0] {
                assert_eq!(pairs.len(), 2);
                assert_eq!(pairs[0].key, "Foo");
                assert_eq!(pairs[1].key, "LongerKey");
            } else {
                panic!(
                    "expected first child to be a coalesced KvBlock; got {:?}",
                    children[0]
                );
            }
        } else {
            panic!("expected Section");
        }
    }

    #[test]
    fn explicit_kv_block_does_not_coalesce_with_kv() {
        // kv_block expresses author intent — keep as a separate block.
        let d = Doc::new().kv("a", "1").kv_block([("b", "2"), ("c", "3")]);
        assert_eq!(
            d.children.len(),
            2,
            "kv_block should NOT merge with prior kv"
        );
    }

    #[test]
    fn with_data_overrides_doc_json() {
        #[derive(serde::Serialize)]
        struct Payload {
            x: i32,
        }
        let d = Doc::new().heading("Foo").with_data(Payload { x: 7 });
        let v = d.data_or_self_json();
        assert_eq!(v["x"], 7);
    }

    #[test]
    fn data_or_self_json_falls_back_to_doc_tree_without_data() {
        let d = Doc::new().heading("Hi").kv("a", "b");
        let v = d.data_or_self_json();
        assert_eq!(v["heading"], "Hi");
        assert!(!v["children"].as_array().unwrap().is_empty());
    }

    #[test]
    fn doc_default_is_empty() {
        let d = Doc::default();
        assert!(d.heading.is_none());
        assert!(d.children.is_empty());
        assert!(d.data.is_none());
    }

    #[test]
    fn doc_status_adds_status_component() {
        let d = Doc::new().status(Role::Ok, "applied");
        assert_eq!(d.children.len(), 1);
        if let Component::Status {
            role,
            subject,
            detail,
            duration_ms,
            target,
            qualifier,
            label,
            verdict,
        } = &d.children[0]
        {
            assert!(matches!(role, Role::Ok));
            assert!(verdict.is_none());
            assert_eq!(subject, "applied");
            assert!(detail.is_none());
            assert!(duration_ms.is_none());
            assert!(target.is_none());
            assert!(qualifier.is_none());
            assert!(label.is_none());
        } else {
            panic!("expected Status");
        }
    }

    #[test]
    fn doc_status_with_populates_all_fields() {
        let d = Doc::new().status_with(Role::Warn, "drift detected", |f| {
            f.detail("3 files changed")
                .duration(Duration::from_millis(42))
                .target("/etc/config")
                .qualifier("unresolved")
                .label(Role::Secondary, "source-a")
        });
        if let Component::Status {
            role,
            subject,
            detail,
            duration_ms,
            target,
            qualifier,
            label,
            verdict: _,
        } = &d.children[0]
        {
            assert!(matches!(role, Role::Warn));
            assert_eq!(subject, "drift detected");
            assert_eq!(detail.as_deref(), Some("3 files changed"));
            assert_eq!(*duration_ms, Some(42));
            assert_eq!(target.as_deref(), Some("/etc/config"));
            assert_eq!(qualifier.as_deref(), Some("unresolved"));
            let l = label.as_ref().unwrap();
            assert!(matches!(l.role, Role::Secondary));
            assert_eq!(l.text, "source-a");
        } else {
            panic!("expected Status");
        }
    }

    /// `StatusFields::drift` composes the same `want: X, have: Y` spelling
    /// as the streaming `StatusBuilder::drift` — proven directly against
    /// `super::super::drift_detail`, not re-derived.
    #[test]
    fn doc_status_with_drift_composes_the_detail_slot() {
        let d = Doc::new().status_with(Role::Warn, "sysctl.net.ipv4.ip_forward", |f| {
            f.drift("1", "0")
        });
        if let Component::Status { detail, .. } = &d.children[0] {
            assert_eq!(
                detail.as_deref(),
                Some(super::super::drift_detail("1", "0").as_str())
            );
        } else {
            panic!("expected Status");
        }
    }

    #[test]
    fn status_fields_detail_opt_sets_none_for_none() {
        let f = StatusFields::default().detail_opt(None);
        assert!(f.detail.is_none());
    }

    #[test]
    fn status_fields_detail_opt_sets_some() {
        let f = StatusFields::default().detail_opt(Some("x"));
        assert_eq!(f.detail.as_deref(), Some("x"));
    }

    #[test]
    fn doc_hint_adds_hint_component() {
        let d = Doc::new().hint("run cfgd apply");
        if let Component::Hint { text } = &d.children[0] {
            assert_eq!(text, "run cfgd apply");
        } else {
            panic!("expected Hint");
        }
    }

    #[test]
    fn doc_note_adds_note_component() {
        let d = Doc::new().note("see docs");
        if let Component::Note { text } = &d.children[0] {
            assert_eq!(text, "see docs");
        } else {
            panic!("expected Note");
        }
    }

    #[test]
    fn doc_table_adds_table_component() {
        let t = Table::new(["Name", "Version"]).row(["foo", "1.0"]);
        let d = Doc::new().table(t);
        if let Component::Table {
            headers,
            rows,
            row_roles,
            wrap_cells,
        } = &d.children[0]
        {
            assert_eq!(headers.len(), 2);
            assert_eq!(rows.len(), 1);
            assert_eq!(row_roles.len(), 1);
            assert!(!wrap_cells);
        } else {
            panic!("expected Table");
        }
    }

    /// The wrap flag is the difference between a truncated package list and a
    /// complete one, and the buffered path rebuilds the table from its own
    /// component — a flag that does not survive that rebuild leaves the
    /// streaming and buffered renders of one table disagreeing.
    #[test]
    fn doc_table_carries_the_wrap_flag_through_its_component() {
        let d = Doc::new().table(Table::new(["Name"]).row(["foo"]).wrapping());
        if let Component::Table { wrap_cells, .. } = &d.children[0] {
            assert!(wrap_cells);
        } else {
            panic!("expected Table");
        }
    }

    #[test]
    fn doc_section_builds_section_component() {
        let d = Doc::new().section("Packages", |s| s.bullet("foo").bullet("bar"));
        if let Component::Section {
            name,
            keep_when_empty,
            children,
            ..
        } = &d.children[0]
        {
            assert_eq!(name, "Packages");
            assert!(keep_when_empty);
            assert_eq!(children.len(), 2);
        } else {
            panic!("expected Section");
        }
    }

    #[test]
    fn doc_section_or_collapse_sets_keep_when_empty_false() {
        let d = Doc::new().section_or_collapse("Empty", |s| s);
        if let Component::Section {
            keep_when_empty, ..
        } = &d.children[0]
        {
            assert!(!keep_when_empty);
        } else {
            panic!("expected Section");
        }
    }

    #[test]
    fn doc_kv_block_stays_separate_from_prior_kv() {
        let d = Doc::new()
            .kv("standalone", "1")
            .kv_block([("a", "2"), ("b", "3")]);
        assert_eq!(d.children.len(), 2);
        if let Component::KvBlock { pairs } = &d.children[1] {
            assert_eq!(pairs.len(), 2);
            assert_eq!(pairs[0].key, "a");
        } else {
            panic!("expected KvBlock");
        }
    }

    #[test]
    fn doc_kv_block_empty_is_noop() {
        let d = Doc::new().kv_block::<Vec<(&str, &str)>, _, _>(vec![]);
        assert!(d.children.is_empty());
    }

    #[test]
    fn section_builder_status_adds_status() {
        let s = SectionBuilder::new("X", true).status(Role::Info, "checking");
        let c = s.into_component();
        if let Component::Section { children, .. } = c {
            assert_eq!(children.len(), 1);
            assert!(
                matches!(&children[0], Component::Status { role: Role::Info, subject, .. } if subject == "checking")
            );
        } else {
            panic!("expected Section");
        }
    }

    #[test]
    fn section_builder_status_with_populates_fields() {
        let s =
            SectionBuilder::new("X", true).status_with(Role::Fail, "error", |f| f.detail("oops"));
        let c = s.into_component();
        if let Component::Section { children, .. } = c {
            if let Component::Status { detail, .. } = &children[0] {
                assert_eq!(detail.as_deref(), Some("oops"));
            } else {
                panic!("expected Status");
            }
        } else {
            panic!("expected Section");
        }
    }

    #[test]
    fn section_builder_hint_and_note() {
        let s = SectionBuilder::new("X", true)
            .hint("try this")
            .note("see also");
        let c = s.into_component();
        if let Component::Section { children, .. } = c {
            assert_eq!(children.len(), 2);
            assert!(matches!(&children[0], Component::Hint { text } if text == "try this"));
            assert!(matches!(&children[1], Component::Note { text } if text == "see also"));
        } else {
            panic!("expected Section");
        }
    }

    #[test]
    fn section_builder_table() {
        let t = Table::new(["H"]).row(["R"]);
        let s = SectionBuilder::new("X", true).table(t);
        let c = s.into_component();
        if let Component::Section { children, .. } = c {
            assert!(matches!(&children[0], Component::Table { .. }));
        } else {
            panic!("expected Section");
        }
    }

    #[test]
    fn section_builder_empty_state() {
        let s = SectionBuilder::new("X", true).empty_state("nothing here");
        let c = s.into_component();
        if let Component::Section { empty_state, .. } = c {
            assert_eq!(empty_state.as_deref(), Some("nothing here"));
        } else {
            panic!("expected Section");
        }
    }

    #[test]
    fn section_builder_subsection() {
        let s = SectionBuilder::new("Parent", true).subsection("Child", |sub| sub.bullet("inner"));
        let c = s.into_component();
        if let Component::Section { children, .. } = c {
            assert_eq!(children.len(), 1);
            if let Component::Section { name, children, .. } = &children[0] {
                assert_eq!(name, "Child");
                assert_eq!(children.len(), 1);
            } else {
                panic!("expected nested Section");
            }
        } else {
            panic!("expected Section");
        }
    }

    #[test]
    fn section_builder_subsection_if_nonempty_skips_empty() {
        let s = SectionBuilder::new("P", true).subsection_if_nonempty::<i32, _>(
            "Empty",
            &[],
            |sub, _| sub,
        );
        let c = s.into_component();
        if let Component::Section { children, .. } = c {
            assert!(children.is_empty());
        } else {
            panic!("expected Section");
        }
    }

    #[test]
    fn section_builder_subsection_if_nonempty_emits_when_present() {
        let s = SectionBuilder::new("P", true).subsection_if_nonempty(
            "Items",
            &["a", "b"],
            |sub, items| sub.extend(items.iter(), |sb, item| sb.bullet(*item)),
        );
        let c = s.into_component();
        if let Component::Section { children, .. } = c {
            assert_eq!(children.len(), 1);
            if let Component::Section {
                name,
                children: inner,
                ..
            } = &children[0]
            {
                assert_eq!(name, "Items");
                assert_eq!(inner.len(), 2);
            } else {
                panic!("expected nested Section");
            }
        } else {
            panic!("expected Section");
        }
    }

    #[test]
    fn section_builder_kv_block_separate() {
        let s = SectionBuilder::new("X", true)
            .kv("a", "1")
            .kv_block([("b", "2")]);
        let c = s.into_component();
        if let Component::Section { children, .. } = c {
            assert_eq!(children.len(), 2);
        } else {
            panic!("expected Section");
        }
    }

    #[test]
    fn section_builder_kv_block_empty_noop() {
        let s = SectionBuilder::new("X", true).kv_block::<Vec<(&str, &str)>, _, _>(vec![]);
        let c = s.into_component();
        if let Component::Section { children, .. } = c {
            assert!(children.is_empty());
        } else {
            panic!("expected Section");
        }
    }
}
