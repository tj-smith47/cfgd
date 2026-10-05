//! The config document every reader before dispatch shares.

use std::path::{Path, PathBuf};

use cfgd_core::config::CfgdConfig;
use cfgd_core::errors::{CfgdError, ConfigError};

/// The config document as it stood when the process started, read and parsed
/// once and handed to every reader of the invocation: the alias pass, the
/// printer's theme, hints and masking knobs, the load-time migration gate, the
/// startup update check, and the command itself through its `RunContext`.
///
/// A document that is absent, unreadable or malformed is never reported here.
/// The pre-dispatch readers are best-effort and see `None` through
/// [`Self::config`]; the command reports the failure through
/// [`Self::config_result`], which hands back the error the load produced.
#[derive(Debug)]
pub struct StartupDocument {
    path: PathBuf,
    loaded: cfgd_core::errors::Result<(CfgdConfig, String)>,
    reads: u32,
}

impl StartupDocument {
    /// Read and parse the document `config_path` names, resolving a directory
    /// to the document inside it the way [`cfgd_core::config::load_config`]
    /// does.
    pub fn load(config_path: &Path) -> Self {
        let path = cfgd_core::config::resolve_config_path(config_path);
        let loaded = cfgd_core::config::read_config_document(&path);
        if let Err(error) = &loaded {
            tracing::debug!(path = %path.display(), %error, "config document not loaded"); // native-ok: log line
        }
        Self {
            path,
            loaded,
            reads: 1,
        }
    }

    /// The parsed document, or `None` when it did not load.
    pub fn config(&self) -> Option<&CfgdConfig> {
        self.loaded.as_ref().ok().map(|(config, _)| config)
    }

    /// The parsed document, or the error loading it produced: the same
    /// variant and message [`cfgd_core::config::load_config`] returns for the
    /// same file.
    pub fn config_result(&self) -> cfgd_core::errors::Result<&CfgdConfig> {
        self.document().map(|(config, _)| config)
    }

    /// The parsed document and the text it was parsed from, or the error
    /// [`Self::config_result`] reports.
    pub fn document(&self) -> cfgd_core::errors::Result<(&CfgdConfig, &str)> {
        match &self.loaded {
            Ok((config, text)) => Ok((config, text.as_str())),
            Err(CfgdError::Config(error)) => Err(error.clone().into()),
            // `read_config_document` fails only with a `ConfigError`. Any
            // other variant is not `Clone`, so its message travels as an
            // invalid config.
            Err(other) => Err(ConfigError::Invalid {
                message: other.to_string(),
            }
            .into()),
        }
    }

    /// The text the document was parsed from, or `None` when it did not load.
    pub fn on_disk(&self) -> Option<&str> {
        self.loaded.as_ref().ok().map(|(_, text)| text.as_str())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn same_error(original: &CfgdError, copy: &CfgdError) {
        assert_eq!(original.to_string(), copy.to_string());
        assert_eq!(original.kind(), copy.kind());
        if let (CfgdError::Config(a), CfgdError::Config(b)) = (original, copy) {
            assert_eq!(
                std::mem::discriminant(a),
                std::mem::discriminant(b),
                "{a:?} vs {b:?}"
            );
        }
    }

    /// Every document `load_config` refuses reports, through
    /// `config_result`, the variant and the message `load_config` reports.
    #[test]
    fn config_result_reports_what_load_config_reports() {
        let dir = tempfile::tempdir().unwrap();
        let documents: [(PathBuf, Option<&[u8]>); 8] = [
            (dir.path().join("missing.yaml"), None),
            // The loader reports any leading `~` that reaches it as an unset home.
            (PathBuf::from("~").join("no-such-cfgd.yaml"), None),
            (
                dir.path().join("malformed.yaml"),
                Some(b"apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: [unclosed\n"),
            ),
            (
                dir.path().join("unknown-key.yaml"),
                Some(b"apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  bogus: 1\n"),
            ),
            (
                dir.path().join("version.yaml"),
                Some(b"apiVersion: cfgd.io/v9\nkind: Config\nmetadata:\n  name: t\nspec: {}\n"),
            ),
            (dir.path().join("not-utf8.yaml"), Some(b"apiVersion: \xff\xfe\n")),
            (
                dir.path().join("malformed.toml"),
                Some(b"apiVersion = \"cfgd.io/v1alpha1\"\nkind = \n"),
            ),
            (
                dir.path().join("unknown-key.toml"),
                Some(b"apiVersion = \"cfgd.io/v1alpha1\"\nkind = \"Config\"\n[metadata]\nname = \"t\"\n[spec]\nbogus = 1\n"),
            ),
        ];
        for (path, bytes) in documents {
            if let Some(bytes) = bytes {
                std::fs::write(&path, bytes).unwrap();
            }
            let expected = cfgd_core::config::load_config(&path).unwrap_err();
            let document = StartupDocument::load(&path);
            assert!(document.config().is_none(), "{}", path.display());
            same_error(&expected, &document.config_result().unwrap_err());
            // Asked twice, the kept error answers twice.
            same_error(&expected, &document.config_result().unwrap_err());
        }
    }

    /// A load failure outside `ConfigError` still reaches the verb with its
    /// message, as an invalid config.
    #[test]
    fn a_load_error_outside_config_error_keeps_its_message() {
        let document = StartupDocument {
            path: PathBuf::from("cfgd.yaml"),
            loaded: Err(CfgdError::Io(std::io::Error::other("disk gone"))),
            reads: 1,
        };
        let error = document.config_result().unwrap_err();
        assert_eq!(
            error.to_string(),
            "config error: invalid config: io error: disk gone"
        );
        assert!(matches!(
            error,
            CfgdError::Config(ConfigError::Invalid { .. })
        ));
    }
}
