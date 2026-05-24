use crate::errors;

/// Check if a file is encrypted with the given backend.
///
/// - `sops`: parses YAML/JSON and checks for a top-level `sops` key with `mac` and `lastmodified`.
/// - `age`: checks if the file starts with the `age-encryption.org` header (reads as bytes to handle binary).
/// - Unknown backend: returns `FileError::UnknownEncryptionBackend`.
pub fn is_file_encrypted(
    path: &std::path::Path,
    backend: &str,
) -> std::result::Result<bool, errors::FileError> {
    use errors::FileError;
    match backend {
        "sops" => {
            let content = std::fs::read_to_string(path).map_err(|e| FileError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            // Try YAML first.  SOPS injects a top-level `sops` map with `mac` + `lastmodified`.
            let value: Option<serde_yaml::Value> = serde_yaml::from_str(&content).ok();
            if let Some(serde_yaml::Value::Mapping(map)) = value
                && let Some(serde_yaml::Value::Mapping(sops)) =
                    map.get(serde_yaml::Value::String("sops".to_string()))
                && sops.contains_key(serde_yaml::Value::String("mac".to_string()))
                && sops.contains_key(serde_yaml::Value::String("lastmodified".to_string()))
            {
                return Ok(true);
            }
            // Try JSON (SOPS can encrypt JSON files too).
            let json_value: Option<serde_json::Value> = serde_json::from_str(&content).ok();
            if let Some(serde_json::Value::Object(map)) = json_value
                && let Some(serde_json::Value::Object(sops)) = map.get("sops")
                && sops.contains_key("mac")
                && sops.contains_key("lastmodified")
            {
                return Ok(true);
            }
            Ok(false)
        }
        "age" => {
            let content = std::fs::read(path).map_err(|e| FileError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            Ok(content.starts_with(b"age-encryption.org"))
        }
        other => Err(FileError::UnknownEncryptionBackend {
            backend: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn sops_yaml_detected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("secret.yaml");
        fs::write(
            &path,
            "data: ENC[AES256_GCM]\nsops:\n  mac: abc123\n  lastmodified: '2024-01-01'\n",
        )
        .unwrap();
        assert!(is_file_encrypted(&path, "sops").unwrap());
    }

    #[test]
    fn sops_json_detected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("secret.json");
        fs::write(
            &path,
            r#"{"data":"enc","sops":{"mac":"abc","lastmodified":"2024"}}"#,
        )
        .unwrap();
        assert!(is_file_encrypted(&path, "sops").unwrap());
    }

    #[test]
    fn sops_plain_yaml_not_detected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("plain.yaml");
        fs::write(&path, "key: value\nnested:\n  foo: bar\n").unwrap();
        assert!(!is_file_encrypted(&path, "sops").unwrap());
    }

    #[test]
    fn sops_incomplete_sops_key_not_detected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("partial.yaml");
        fs::write(&path, "sops:\n  mac: abc\n").unwrap();
        assert!(!is_file_encrypted(&path, "sops").unwrap());
    }

    #[test]
    fn age_header_detected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("secret.age");
        fs::write(&path, "age-encryption.org/v1\n-> X25519 abc\ndata").unwrap();
        assert!(is_file_encrypted(&path, "age").unwrap());
    }

    #[test]
    fn age_plain_file_not_detected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("plain.txt");
        fs::write(&path, "just some text content").unwrap();
        assert!(!is_file_encrypted(&path, "age").unwrap());
    }

    #[test]
    fn unknown_backend_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("file.txt");
        fs::write(&path, "data").unwrap();
        let err = is_file_encrypted(&path, "gpg").unwrap_err();
        assert!(matches!(
            err,
            errors::FileError::UnknownEncryptionBackend { .. }
        ));
    }

    #[test]
    fn missing_file_returns_io_error() {
        let path = std::path::Path::new("/no/such/file/exists.yaml");
        let err = is_file_encrypted(path, "sops").unwrap_err();
        assert!(matches!(err, errors::FileError::Io { .. }));
    }

    #[test]
    fn age_missing_file_returns_io_error() {
        let path = std::path::Path::new("/no/such/file/exists.age");
        let err = is_file_encrypted(path, "age").unwrap_err();
        assert!(matches!(err, errors::FileError::Io { .. }));
    }
}
