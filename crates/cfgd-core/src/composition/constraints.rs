use crate::config::{MergedProfile, PolicyItems, ProfileSpec, SourceConstraints};
use crate::errors::{CompositionError, Result};

/// Validate security constraints for a source's contribution to the composed profile.
///
/// `allow_scripts` is the subscriber's `subscription.allowScripts` opt-in: when
/// `true` the source's `constraints.no_scripts` no longer rejects scripts (the
/// subscriber has accepted the risk), matching the source-delivered module-body
/// enforcement. Path/system/encryption constraints are unaffected.
pub fn validate_constraints(
    source_name: &str,
    constraints: &SourceConstraints,
    spec: &ProfileSpec,
    allow_scripts: bool,
) -> Result<()> {
    // Check script constraint
    if constraints.no_scripts
        && !allow_scripts
        && let Some(ref scripts) = spec.scripts
        && (!scripts.pre_apply.is_empty()
            || !scripts.post_apply.is_empty()
            || !scripts.pre_reconcile.is_empty()
            || !scripts.post_reconcile.is_empty()
            || !scripts.on_drift.is_empty()
            || !scripts.on_change.is_empty())
    {
        return Err(CompositionError::ScriptsNotAllowed {
            source_name: source_name.to_string(),
        }
        .into());
    }

    // Check system change constraint
    if !constraints.allow_system_changes && !spec.system.is_empty() {
        let first_key = spec.system.keys().next().cloned().unwrap_or_default();
        return Err(CompositionError::SystemChangeNotAllowed {
            source_name: source_name.to_string(),
            setting: first_key,
        }
        .into());
    }

    // Check path containment
    if !constraints.allowed_target_paths.is_empty()
        && let Some(ref files) = spec.files
    {
        for managed in &files.managed {
            let target_str = managed.target.to_string_lossy();
            if !path_matches_any(&target_str, &constraints.allowed_target_paths) {
                return Err(CompositionError::PathNotAllowed {
                    source_name: source_name.to_string(),
                    path: target_str.to_string(),
                }
                .into());
            }
        }
    }

    // Check encryption.requiredTargets: every file whose target matches a required-encryption
    // glob must have an encryption block, and if the constraint specifies a backend, it must
    // match the file's encryption backend.
    if let Some(ref enc_constraint) = constraints.encryption
        && !enc_constraint.required_targets.is_empty()
        && let Some(ref files) = spec.files
    {
        for managed in &files.managed {
            let target_str = managed.target.to_string_lossy();
            if let Some(matched_pattern) =
                find_matching_pattern(&target_str, &enc_constraint.required_targets)
            {
                match managed.encryption.as_ref() {
                    None => {
                        return Err(CompositionError::EncryptionRequired {
                            source_name: source_name.to_string(),
                            path: target_str.to_string(),
                            pattern: matched_pattern,
                        }
                        .into());
                    }
                    Some(enc_spec) => {
                        if let Some(ref required_backend) = enc_constraint.backend
                            && enc_spec.backend != *required_backend
                        {
                            return Err(CompositionError::EncryptionBackendMismatch {
                                source_name: source_name.to_string(),
                                path: target_str.to_string(),
                                pattern: matched_pattern.clone(),
                                actual_backend: enc_spec.backend.clone(),
                                required_backend: required_backend.clone(),
                            }
                            .into());
                        }
                        if let Some(ref required_mode) = enc_constraint.mode
                            && enc_spec.mode != *required_mode
                        {
                            return Err(CompositionError::EncryptionModeMismatch {
                                source_name: source_name.to_string(),
                                path: target_str.to_string(),
                                pattern: matched_pattern,
                                actual_mode: format!("{:?}", enc_spec.mode),
                                required_mode: format!("{:?}", required_mode),
                            }
                            .into());
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Check if a path matches any of the allowed patterns.
/// Supports glob patterns and prefix matching.
pub(super) fn path_matches_any(path: &str, allowed: &[String]) -> bool {
    find_matching_pattern(path, allowed).is_some()
}

/// Return the first pattern from `patterns` that matches `path`, or `None`.
/// Uses the same matching logic as `path_matches_any`.
pub(super) fn find_matching_pattern(path: &str, patterns: &[String]) -> Option<String> {
    for pattern in patterns {
        if let Ok(glob_pattern) = glob::Pattern::new(pattern)
            && glob_pattern.matches(path)
        {
            return Some(pattern.clone());
        }
        if pattern.ends_with('/') && path.starts_with(pattern.as_str()) {
            return Some(pattern.clone());
        }
        if path == pattern {
            return Some(pattern.clone());
        }
    }
    None
}

/// Check if a subscriber is trying to override a locked resource.
pub fn check_locked_violations(
    source_name: &str,
    locked: &PolicyItems,
    local_merged: &MergedProfile,
) -> Result<()> {
    // Check locked files — local cannot override these targets
    for locked_file in &locked.files {
        for local_file in &local_merged.files.managed {
            if local_file.target == locked_file.target && local_file.source != locked_file.source {
                return Err(CompositionError::LockedResource {
                    source_name: source_name.to_string(),
                    resource: locked_file.target.to_string_lossy().to_string(),
                }
                .into());
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod find_matching_pattern_tests {
    use super::find_matching_pattern;

    #[test]
    fn falls_back_to_literal_equality_for_invalid_glob() {
        // `a[b` is not a valid glob (unclosed class), so the glob branch is
        // skipped and the literal-equality arm matches the identical path.
        let patterns = vec!["a[b".to_string()];
        assert_eq!(
            find_matching_pattern("a[b", &patterns),
            Some("a[b".to_string())
        );
        assert_eq!(find_matching_pattern("other", &patterns), None);
    }
}
