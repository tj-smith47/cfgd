use std::path::Path;

use super::*;

pub(crate) fn save_module_document(
    doc: &config::ModuleDocument,
    path: &Path,
) -> anyhow::Result<()> {
    let yaml = serde_yaml::to_string(doc)?;
    cfgd_core::atomic_write_str(path, &yaml)?;
    Ok(())
}

/// Write a freshly scaffolded module.yaml with the editor schema modeline.
///
/// Distinct from `save_module_document` because rewrite paths (update, add/
/// remove package) must never inject a modeline into a user-owned file —
/// only brand-new scaffolds get one.
pub(crate) fn scaffold_module_document(
    doc: &config::ModuleDocument,
    path: &Path,
) -> anyhow::Result<()> {
    crate::cli::helpers::write_scaffold(
        cfgd_core::config::SchemaDocKind::Module,
        path,
        &serde_yaml::to_string(doc)?,
    )
}
