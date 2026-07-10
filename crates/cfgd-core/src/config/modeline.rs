//! Editor schema modelines for scaffolded documents.
//!
//! Newly scaffolded cfgd YAML documents get a `yaml-language-server` modeline
//! as their first line so editors validate them even when the file lives
//! outside the SchemaStore catalog's `fileMatch` globs (legacy flat profiles,
//! dot-dir checkouts, hand-renamed files). Rewrite paths (update/edit/rename
//! cascades) never inject a modeline — only fresh scaffolds do.

/// Leading marker shared by every yaml-language-server modeline.
const MODELINE_PREFIX: &str = "# yaml-language-server: $schema=";

/// Document kinds cfgd scaffolds, keyed to their SchemaStore catalog slug.
///
/// Slugs come from the catalog entry names in `.anodizer.yaml`'s
/// `schemastore.schemas` block (`slugify(name)`): "cfgd ConfigSource" →
/// `cfgd-configsource` — note this differs from the local schema filename
/// `cfgd-source.schema.json`.
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
            Self::ConfigSource => "cfgd-configsource",
        }
    }
}

/// Compose the schema modeline (including trailing newline) for a document kind.
///
/// The URL is version-suffixed because cfgd publishes to SchemaStore with
/// `versioned: true`: only `cfgd-<slug>-<version>.json` files exist upstream
/// (the catalog's canonical `url` points at the latest versioned file; an
/// unversioned alias 404s).
///
/// `version` must be the **cfgd binary crate's** version: the SchemaStore
/// entries declare `crate: cfgd` in `.anodizer.yaml`, so anodizer stamps the
/// vendored filenames with that crate's version. The workspace releases crates
/// on independent cadences (per-crate tags, per-crate `version =` lines), so
/// cfgd-core's own `CARGO_PKG_VERSION` is NOT a valid substitute — callers in
/// the cfgd binary pass `env!("CARGO_PKG_VERSION")` from their own crate.
pub fn schema_modeline(kind: SchemaDocKind, version: &str) -> String {
    format!(
        "{}https://www.schemastore.org/{}-{}.json\n",
        MODELINE_PREFIX,
        kind.slug(),
        version
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
            (SchemaDocKind::ConfigSource, "cfgd-configsource"),
        ] {
            let line = schema_modeline(kind, VER);
            assert_eq!(
                line,
                format!(
                    "# yaml-language-server: $schema=https://www.schemastore.org/{slug}-{VER}.json\n"
                )
            );
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
