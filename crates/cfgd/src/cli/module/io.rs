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
