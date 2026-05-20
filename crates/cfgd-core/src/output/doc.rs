use std::time::Duration;

use serde::Serialize;

use super::Role;
use super::component::{Component, KvPair};
use super::renderer::Table;

/// One status row's optional fields, used by `status_with`.
#[derive(Default)]
pub struct StatusFields {
    pub detail: Option<String>,
    pub duration: Option<Duration>,
    pub target: Option<String>,
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
}
