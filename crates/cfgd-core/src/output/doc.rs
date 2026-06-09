use std::time::Duration;

use serde::Serialize;

use super::Role;
use super::component::{Component, KvPair, StatusLabel};
use super::renderer::Table;

/// One status row's optional fields, used by `status_with`.
#[derive(Default)]
pub struct StatusFields {
    pub detail: Option<String>,
    pub duration: Option<Duration>,
    pub target: Option<String>,
    pub label: Option<StatusLabel>,
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
    pub fn duration(mut self, d: Duration) -> Self {
        self.duration = Some(d);
        self
    }
    pub fn target(mut self, s: impl Into<String>) -> Self {
        self.target = Some(s.into());
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
}

/// Top-level buffered document. Built then handed to `Printer::emit`.
pub struct Doc {
    pub(crate) heading: Option<String>,
    pub(crate) children: Vec<Component>,
    /// Optional payload that REPLACES Doc-derived JSON in structured modes.
    pub(crate) data: Option<serde_json::Value>,
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
        }
    }

    pub fn heading(mut self, text: impl Into<String>) -> Self {
        self.heading = Some(text.into());
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

    pub fn status(mut self, role: Role, subject: impl Into<String>) -> Self {
        self.children.push(Component::Status {
            role,
            subject: subject.into(),
            detail: None,
            duration_ms: None,
            target: None,
            label: None,
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
            label: f.label,
        });
        self
    }

    pub fn hint(mut self, text: impl Into<String>) -> Self {
        self.children.push(Component::Hint { text: text.into() });
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
            obj.insert("heading".into(), serde_json::Value::String(h.clone()));
        }
        obj.insert("children".into(), serde_json::Value::Array(children));
        serde_json::Value::Object(obj)
    }

    pub(crate) fn data_or_self_json(&self) -> serde_json::Value {
        self.data.clone().unwrap_or_else(|| self.to_json_value())
    }
}

/// Builder for one Section. Same vocabulary as Doc plus `subsection`.
pub struct SectionBuilder {
    name: String,
    keep_when_empty: bool,
    empty_state: Option<String>,
    children: Vec<Component>,
}

impl SectionBuilder {
    pub(crate) fn new(name: impl Into<String>, keep_when_empty: bool) -> Self {
        Self {
            name: name.into(),
            keep_when_empty,
            empty_state: None,
            children: Vec::new(),
        }
    }

    pub(crate) fn into_component(self) -> Component {
        Component::Section {
            name: self.name,
            keep_when_empty: self.keep_when_empty,
            empty_state: self.empty_state,
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

    pub fn status(mut self, role: Role, subject: impl Into<String>) -> Self {
        self.children.push(Component::Status {
            role,
            subject: subject.into(),
            detail: None,
            duration_ms: None,
            target: None,
            label: None,
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
            label: f.label,
        });
        self
    }

    pub fn hint(mut self, text: impl Into<String>) -> Self {
        self.children.push(Component::Hint { text: text.into() });
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
            label,
        } = &d.children[0]
        {
            assert!(matches!(role, Role::Ok));
            assert_eq!(subject, "applied");
            assert!(detail.is_none());
            assert!(duration_ms.is_none());
            assert!(target.is_none());
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
                .label(Role::Secondary, "source-a")
        });
        if let Component::Status {
            role,
            subject,
            detail,
            duration_ms,
            target,
            label,
        } = &d.children[0]
        {
            assert!(matches!(role, Role::Warn));
            assert_eq!(subject, "drift detected");
            assert_eq!(detail.as_deref(), Some("3 files changed"));
            assert_eq!(*duration_ms, Some(42));
            assert_eq!(target.as_deref(), Some("/etc/config"));
            let l = label.as_ref().unwrap();
            assert!(matches!(l.role, Role::Secondary));
            assert_eq!(l.text, "source-a");
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
        let t = Table {
            headers: vec!["Name".into(), "Version".into()],
            rows: vec![vec!["foo".into(), "1.0".into()]],
            row_roles: vec![],
        };
        let d = Doc::new().table(t);
        if let Component::Table {
            headers,
            rows,
            row_roles,
        } = &d.children[0]
        {
            assert_eq!(headers.len(), 2);
            assert_eq!(rows.len(), 1);
            assert!(row_roles.is_empty());
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
        let t = Table {
            headers: vec!["H".into()],
            rows: vec![vec!["R".into()]],
            row_roles: vec![],
        };
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
