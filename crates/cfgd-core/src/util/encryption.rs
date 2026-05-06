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
