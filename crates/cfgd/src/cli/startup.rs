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
        match &self.loaded {
            Ok((config, _)) => Ok(config),
            Err(error) => Err(reissue_load_error(error)),
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

/// A fresh copy of an error [`cfgd_core::config::read_config_document`]
/// returned, with the same variant (the exit code and the CLI's error routing
/// match on it) and the same message.
///
/// `CfgdError` is not `Clone` because the parser errors it wraps are not, and
/// the document is read once while every caller of
/// [`StartupDocument::config_result`] needs an owned error to propagate. The
/// parser errors are rebuilt from their rendered text, which is all a
/// consumer of a load failure reads.
fn reissue_load_error(error: &CfgdError) -> CfgdError {
    use serde::de::Error as _;
    let config = match error {
        CfgdError::Config(config) => config,
        CfgdError::Io(io) => return CfgdError::Io(std::io::Error::new(io.kind(), io.to_string())),
        // `read_config_document` produces nothing else; anything it adds
        // later is reported as an invalid config carrying its message.
        other => {
            return ConfigError::Invalid {
                message: other.to_string(),
            }
            .into();
        }
    };
    let copy = match config {
        ConfigError::NotFound { path } => ConfigError::NotFound { path: path.clone() },
        ConfigError::HomeUnresolved { path } => ConfigError::HomeUnresolved { path: path.clone() },
        ConfigError::Invalid { message } => ConfigError::Invalid {
            message: message.clone(),
        },
        ConfigError::UnsupportedApiVersion { found } => ConfigError::UnsupportedApiVersion {
            found: found.clone(),
        },
        ConfigError::CircularInheritance { chain } => ConfigError::CircularInheritance {
            chain: chain.clone(),
        },
        ConfigError::ProfileNotFound { name } => {
            ConfigError::ProfileNotFound { name: name.clone() }
        }
        ConfigError::KeyNotFound { key, undeclared } => ConfigError::KeyNotFound {
            key: key.clone(),
            undeclared: undeclared.clone(),
        },
        ConfigError::AmbiguousProfile { name, paths } => ConfigError::AmbiguousProfile {
            name: name.clone(),
            paths: paths.clone(),
        },
        ConfigError::Yaml(yaml) => ConfigError::Yaml(serde_yaml::Error::custom(yaml)),
        // A rebuilt toml error renders its message followed by a newline, and
        // the original's rendering already ends in one.
        ConfigError::Toml(toml) => {
            let text = toml.to_string();
            ConfigError::Toml(toml::de::Error::custom(
                text.strip_suffix('\n').unwrap_or(&text),
            ))
        }
    };
    copy.into()
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
        let documents = [
            ("missing.yaml", None),
            (
                "malformed.yaml",
                Some(
                    "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: [unclosed\n",
                ),
            ),
            (
                "unknown-key.yaml",
                Some(
                    "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  bogus: 1\n",
                ),
            ),
            (
                "version.yaml",
                Some("apiVersion: cfgd.io/v9\nkind: Config\nmetadata:\n  name: t\nspec: {}\n"),
            ),
            (
                "malformed.toml",
                Some("apiVersion = \"cfgd.io/v1alpha1\"\nkind = \n"),
            ),
            (
                "unknown-key.toml",
                Some(
                    "apiVersion = \"cfgd.io/v1alpha1\"\nkind = \"Config\"\n[metadata]\nname = \"t\"\n[spec]\nbogus = 1\n",
                ),
            ),
        ];
        for (name, text) in documents {
            let path = dir.path().join(name);
            if let Some(text) = text {
                std::fs::write(&path, text).unwrap();
            }
            let expected = cfgd_core::config::load_config(&path).unwrap_err();
            let document = StartupDocument::load(&path);
            assert!(document.config().is_none(), "{name}");
            let first = document.config_result().unwrap_err();
            same_error(&expected, &first);
            // Asked twice, the kept error answers twice.
            same_error(&expected, &document.config_result().unwrap_err());
        }
    }

    /// The variants a load never reaches through a file on this host are
    /// rebuilt to the same variant and message as well.
    #[test]
    fn every_reissued_config_error_keeps_its_variant_and_message() {
        let errors: Vec<CfgdError> = vec![
            ConfigError::HomeUnresolved {
                path: PathBuf::from("~/cfgd.yaml"),
            }
            .into(),
            ConfigError::Invalid {
                message: "too large".into(),
            }
            .into(),
            ConfigError::CircularInheritance {
                chain: vec!["a".into(), "b".into()],
            }
            .into(),
            ConfigError::ProfileNotFound { name: "p".into() }.into(),
            ConfigError::KeyNotFound {
                key: "a.b".into(),
                undeclared: Some("a".into()),
            }
            .into(),
            ConfigError::AmbiguousProfile {
                name: "p".into(),
                paths: vec![PathBuf::from("p.yaml"), PathBuf::from("p/profile.yaml")],
            }
            .into(),
            CfgdError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "gone")),
        ];
        for error in &errors {
            same_error(error, &reissue_load_error(error));
        }
    }
}
