use std::path::PathBuf;

use super::default_state_dir;
use crate::errors::{Result, StateError};

/// Filename of the desired config the device gateway pushed for later
/// reconciliation. A migration sidecar artifact in the state dir.
pub const PENDING_CONFIG_FILENAME: &str = "pending-server-config.json";

/// Save a desired config received from the device gateway for later reconciliation.
pub fn save_pending_server_config(config: &serde_json::Value) -> Result<PathBuf> {
    let dir = default_state_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|_| StateError::DirectoryNotWritable { path: dir.clone() })?;
    let path = dir.join(PENDING_CONFIG_FILENAME);
    let json = serde_json::to_string_pretty(config).map_err(|e| StateError::Serialize {
        context: "encode pending config",
        source: e,
    })?;
    crate::atomic_write_str(&path, &json).map_err(|e| StateError::FilesystemIo {
        path: path.clone(),
        source: e,
    })?;
    Ok(path)
}

/// Load a pending server config, if one exists.
pub fn load_pending_server_config() -> Result<Option<serde_json::Value>> {
    let dir = default_state_dir()?;
    let path = dir.join(PENDING_CONFIG_FILENAME);
    if !path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&path).map_err(|e| StateError::FilesystemIo {
        path: path.clone(),
        source: e,
    })?;
    let value: serde_json::Value =
        serde_json::from_str(&contents).map_err(|e| StateError::Serialize {
            context: "parse pending config",
            source: e,
        })?;
    Ok(Some(value))
}

/// Remove the pending server config file after it has been consumed.
pub fn clear_pending_server_config() -> Result<()> {
    let dir = default_state_dir()?;
    let path = dir.join(PENDING_CONFIG_FILENAME);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StateError::FilesystemIo { path, source: e }.into()),
    }
}
