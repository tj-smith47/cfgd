//! Golden schema-snapshot gate.
//!
//! A committed golden JSON schema per resource kind (in `tests/golden/schema/`)
//! pinned against the live `schemars`-derived schema. Any schema change — a new
//! field, a renamed field, a changed type — trips this test, forcing a conscious
//! "additive or breaking?" decision rather than silently shipping a schema drift.
//!
//! One mechanism covers both halves of the unified registry: the four local YAML
//! document kinds and the five cluster-side CRD kinds (behind the default-on
//! `crd` feature, so the CRD goldens are exercised in the normal test run).
//!
//! Bless (regenerate the goldens) by running with `CFGD_BLESS_SCHEMA=1` set —
//! `task schema:bless` does exactly that. The bless writer and the assert read
//! the SAME [`cfgd_core::schema::KindEntry::pretty_schema`] serialization, so the
//! bytes written match the bytes asserted byte-for-byte.

use cfgd_core::schema::KIND_REGISTRY;

/// Path to the committed golden for one registry entry. The CRD `Module` and the
/// local `Module` share the kind string `"Module"`, so CRD kinds carry a `-crd`
/// suffix to disambiguate the two goldens.
fn golden_path(kind: &str, crd: bool) -> String {
    format!(
        "tests/golden/schema/{}{}.json",
        kind,
        if crd { "-crd" } else { "" }
    )
}

#[test]
fn every_kind_schema_matches_its_committed_golden() {
    let bless = std::env::var("CFGD_BLESS_SCHEMA").is_ok();
    for entry in KIND_REGISTRY {
        let current = entry.pretty_schema();
        let path = golden_path(entry.kind, entry.crd);
        if bless {
            std::fs::write(&path, &current)
                .unwrap_or_else(|e| panic!("failed to write golden {path}: {e}"));
            continue;
        }
        let golden = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing golden {path} — run `task schema:bless`"));
        assert_eq!(
            current.trim(),
            golden.trim(),
            "{} schema changed. If intentional: `task schema:bless`, then decide additive vs \
             breaking (breaking ⇒ apiVersion bump + migration note).",
            entry.kind
        );
    }
}
