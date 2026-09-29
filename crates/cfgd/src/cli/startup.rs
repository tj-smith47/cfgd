//! The config document every reader before dispatch shares.

use std::path::{Path, PathBuf};

use cfgd_core::config::CfgdConfig;

/// The config document as it stood when the process started, read and parsed
/// once and handed to every reader that runs before dispatch: the alias pass,
/// the printer's theme, hints and masking knobs, the load-time migration gate
/// and the startup update check.
///
/// A document that is absent, unreadable or malformed loads as `None` and is
/// never reported here: those readers are best-effort, and the command that
/// runs afterwards reports a load failure itself.
#[derive(Debug)]
pub struct StartupDocument {
    path: PathBuf,
    loaded: Option<(CfgdConfig, String)>,
    reads: u32,
}

impl StartupDocument {
    /// Read and parse the document `config_path` names, resolving a directory
    /// to the document inside it the way [`cfgd_core::config::load_config`]
    /// does.
    pub fn load(config_path: &Path) -> Self {
        let path = cfgd_core::config::resolve_config_path(config_path);
        let loaded = match cfgd_core::config::read_config_document(&path) {
            Ok(pair) => Some(pair),
            Err(error) => {
                tracing::debug!(path = %path.display(), %error, "config document not loaded"); // native-ok: log line
                None
            }
        };
        Self {
            path,
            loaded,
            reads: 1,
        }
    }

    /// The parsed document, or `None` when it did not load.
    pub fn config(&self) -> Option<&CfgdConfig> {
        self.loaded.as_ref().map(|(config, _)| config)
    }

    /// The text the document was parsed from, or `None` when it did not load.
    pub fn on_disk(&self) -> Option<&str> {
        self.loaded.as_ref().map(|(_, text)| text.as_str())
    }

    /// The resolved path of the document.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many times this document was read from disk: 1, plus one for each
    /// [`Self::reload_if_moved`] that had to read again.
    pub fn reads(&self) -> u32 {
        self.reads
    }

    /// This document, when `path` names the same file; the document at `path`
    /// otherwise.
    ///
    /// The alias pass settles the config location through the same call the
    /// startup path makes, so the two name one file whenever the alias pass
    /// could parse the argv; clap moves the path only for an argv it could
    /// not. The same file keeps the one read already made and takes the
    /// caller's spelling of the path, so [`Self::path`] always names what the
    /// caller asked for.
    pub fn reload_if_moved(self, path: &Path) -> Self {
        let resolved = cfgd_core::config::resolve_config_path(path);
        if cfgd_core::absolutize_path(&resolved) == cfgd_core::absolutize_path(&self.path) {
            return Self {
                path: resolved,
                ..self
            };
        }
        let reads = self.reads + 1;
        Self {
            reads,
            ..Self::load(&resolved)
        }
    }
}
