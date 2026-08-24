//! Editor schema modelines for scaffolded documents.
//!
//! Newly scaffolded cfgd YAML documents get a `yaml-language-server` modeline
//! as their first line so editors validate them wherever the file lives
//! (legacy flat profiles, dot-dir checkouts, hand-renamed files). Rewrite
//! paths (update/edit/rename cascades) never inject a modeline — only fresh
//! scaffolds do.

/// Leading marker shared by every yaml-language-server modeline.
const MODELINE_PREFIX: &str = "# yaml-language-server: $schema=";

/// Where the modeline points: the schema file committed under `schemas/` in
/// this repository, read at the release tag of the cfgd version that wrote
/// the document. The tag is immutable, so the URL never moves under a file
/// once it has been scaffolded, and it resolves the moment the tag is pushed
/// (the SchemaStore registration in `.anodizer.yaml` is human-gated upstream
/// and is not what a modeline may depend on).
const SCHEMA_URL_BASE: &str = "https://raw.githubusercontent.com/tj-smith47/cfgd";

/// Document kinds cfgd scaffolds, keyed to their schema filename under
/// `schemas/` (`cfgd-<slug>.schema.json`).
///
/// The slug is the local filename's, NOT the SchemaStore catalog's: the
/// catalog slugifies the entry name ("cfgd ConfigSource" → `cfgd-configsource`)
/// while the committed file is `cfgd-source.schema.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaDocKind {
    /// `cfgd.yaml` root config.
    Config,
    /// `modules/<name>/module.yaml`.
    Module,
    /// `profiles/<name>/profile.yaml` (or legacy flat form).
    Profile,
    /// `cfgd-source.yaml` multi-source manifest.
    ConfigSource,
}

impl SchemaDocKind {
    fn slug(self) -> &'static str {
        match self {
            Self::Config => "cfgd-config",
            Self::Module => "cfgd-module",
            Self::Profile => "cfgd-profile",
            Self::ConfigSource => "cfgd-source",
        }
    }
}

/// Compose the schema modeline (including trailing newline) for a document kind:
/// `https://raw.githubusercontent.com/tj-smith47/cfgd/v<version>/schemas/cfgd-<slug>.schema.json`.
///
/// `version` must be the **cfgd binary crate's** version: the release tag is
/// `v<cfgd version>` (`tag_template` in `.anodizer.yaml`), and the schemas are
/// generated from that crate's types. The workspace releases crates on
/// independent cadences (per-crate tags, per-crate `version =` lines), so
/// cfgd-core's own `CARGO_PKG_VERSION` is NOT a valid substitute — callers in
/// the cfgd binary pass `env!("CARGO_PKG_VERSION")` from their own crate.
pub fn schema_modeline(kind: SchemaDocKind, version: &str) -> String {
    format!(
        "{}{}/v{}/schemas/{}.schema.json\n",
        MODELINE_PREFIX,
        SCHEMA_URL_BASE,
        version,
        kind.slug()
    )
}

/// Prepend the schema modeline to a YAML document body.
///
/// `version` follows the same rule as [`schema_modeline`]: the cfgd binary
/// crate's version, never cfgd-core's.
///
/// Idempotent: content that already begins with a yaml-language-server
/// modeline (e.g. AI-generated documents that included one) is returned
/// unchanged.
pub fn with_schema_modeline(kind: SchemaDocKind, version: &str, yaml: &str) -> String {
    if yaml.starts_with(MODELINE_PREFIX) {
        return yaml.to_string();
    }
    format!("{}{}", schema_modeline(kind, version), yaml)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 9.9.x sentinel per testing.md: fixture versions must never collide with
    // a real release stream.
    const VER: &str = "9.9.9";

    #[test]
    fn modeline_shape_per_kind() {
        for (kind, slug) in [
            (SchemaDocKind::Config, "cfgd-config"),
            (SchemaDocKind::Module, "cfgd-module"),
            (SchemaDocKind::Profile, "cfgd-profile"),
            (SchemaDocKind::ConfigSource, "cfgd-source"),
        ] {
            let line = schema_modeline(kind, VER);
            assert_eq!(
                line,
                format!(
                    "# yaml-language-server: $schema=https://raw.githubusercontent.com/tj-smith47/cfgd/v{VER}/schemas/{slug}.schema.json\n"
                )
            );
        }
    }

    /// Every slug the modeline can name is a file committed under `schemas/`,
    /// so the URL it composes has something to resolve to at the tag.
    #[test]
    fn every_modeline_slug_names_a_committed_schema_file() {
        let schemas = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
        for kind in [
            SchemaDocKind::Config,
            SchemaDocKind::Module,
            SchemaDocKind::Profile,
            SchemaDocKind::ConfigSource,
        ] {
            let file = schemas.join(format!("{}.schema.json", kind.slug()));
            assert!(file.is_file(), "{} is not committed", file.display());
        }
    }

    #[test]
    fn with_modeline_prepends_as_first_line() {
        let body = "apiVersion: cfgd.io/v1alpha1\nkind: Module\n";
        let out = with_schema_modeline(SchemaDocKind::Module, VER, body);
        let mut lines = out.lines();
        assert_eq!(
            lines.next().unwrap(),
            schema_modeline(SchemaDocKind::Module, VER).trim_end()
        );
        assert_eq!(lines.next().unwrap(), "apiVersion: cfgd.io/v1alpha1");
    }

    #[test]
    fn with_modeline_is_idempotent() {
        let body = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\n";
        let once = with_schema_modeline(SchemaDocKind::Profile, VER, body);
        let twice = with_schema_modeline(SchemaDocKind::Profile, VER, &once);
        assert_eq!(once, twice);
    }

    #[test]
    fn modeline_yaml_still_parses() {
        let body = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec: {}\n";
        let out = with_schema_modeline(SchemaDocKind::Config, VER, body);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
        assert_eq!(parsed["kind"], serde_yaml::Value::from("Config"));
    }
}
