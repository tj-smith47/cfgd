use crate::PathDisplayExt;
use crate::config::{MergedProfile, PolicyItems, ProfileLayer, ProfileSpec, SourceConstraints};
use crate::errors::{CfgdError, CompositionError, Result};

/// Describe every element of `spec` that runs source-supplied code, in the
/// wording an error or a plan note uses.
///
/// The single enumeration of what `constraints.noScripts` governs. The
/// fail-closed check and the `allowScripts` disclosure note both read this
/// list, so a new script surface cannot reach one without reaching the other.
pub fn script_surfaces(spec: &ProfileSpec) -> Vec<String> {
    let mut surfaces = Vec::new();

    if let Some(ref scripts) = spec.scripts {
        for (label, entries) in [
            ("preApply", &scripts.pre_apply),
            ("postApply", &scripts.post_apply),
            ("preReconcile", &scripts.pre_reconcile),
            ("postReconcile", &scripts.post_reconcile),
            ("onChange", &scripts.on_change),
            ("onDrift", &scripts.on_drift),
        ] {
            if !entries.is_empty() {
                surfaces.push(format!("a {label} script"));
            }
        }
    }

    for backup in &spec.backups {
        for (label, entries) in [
            ("preBackup", &backup.pre_backup),
            ("postBackup", &backup.post_backup),
        ] {
            if !entries.is_empty() {
                surfaces.push(format!("a {label} hook on backup '{}'", backup.name));
            }
        }
    }

    if let Some(ref files) = spec.files {
        for managed in &files.managed {
            if managed
                .patch
                .as_ref()
                .is_some_and(|patch| patch.script.is_some())
            {
                surfaces.push(format!("a patch script for {}", managed.target.posix()));
            }
        }
    }

    surfaces
}

/// Poison every `patch.script` in `layers` that `source_name` is not permitted
/// to run, so a read path cannot execute it.
///
/// `Report` mode records a source's violation and keeps composing — the read
/// still has to render — but a `patch.script` is not inert data: `diff`,
/// `verify`, `status` and a compliance snapshot all evaluate a `Patch` file,
/// and evaluating one runs the filter. Marking the spec (rather than dropping
/// the file, which would describe a state `apply` would never produce) leaves
/// the file visible and degrades it with a named failure at every evaluation
/// site.
///
/// Called unconditionally: in `Enforce` mode a barred source carrying a script
/// has already aborted composition, so marking is a no-op there.
pub(super) fn block_barred_scripts(
    source_name: &str,
    constraints: &SourceConstraints,
    allow_scripts: bool,
    layers: &mut [ProfileLayer],
) {
    if !constraints.no_scripts || allow_scripts {
        return;
    }
    for layer in layers {
        let Some(ref mut files) = layer.spec.files else {
            continue;
        };
        for managed in &mut files.managed {
            if let Some(ref mut patch) = managed.patch
                && patch.script.is_some()
            {
                patch.blocked_by = Some(source_name.to_string());
            }
        }
    }
}

/// Every security-constraint violation `spec` commits against `constraints`,
/// in declaration order.
///
/// The single constraint check. Collecting rather than short-circuiting is what
/// lets `Report` mode agree with itself: a source with two barred patch filters
/// degrades both files, so the warning has to name both. A fail-closed caller
/// takes the first entry — the one it would have aborted on either way — so
/// both modes read the same enumeration and cannot drift apart.
///
/// `allow_scripts` is the subscriber's `subscription.allowScripts` opt-in: when
/// `true` the source's `constraints.noScripts` no longer rejects scripts (the
/// subscriber has accepted the risk), matching the source-delivered module-body
/// enforcement. Path/system/encryption constraints are unaffected.
pub(super) fn collect_constraint_violations(
    source_name: &str,
    constraints: &SourceConstraints,
    spec: &ProfileSpec,
    allow_scripts: bool,
) -> Vec<CfgdError> {
    let mut violations: Vec<CfgdError> = Vec::new();

    if constraints.no_scripts && !allow_scripts {
        for kind in script_surfaces(spec) {
            violations.push(
                CompositionError::ScriptsNotAllowed {
                    source_name: source_name.to_string(),
                    kind,
                }
                .into(),
            );
        }
    }

    if !constraints.allow_system_changes {
        for setting in spec.system.keys() {
            violations.push(
                CompositionError::SystemChangeNotAllowed {
                    source_name: source_name.to_string(),
                    setting: setting.clone(),
                }
                .into(),
            );
        }
    }

    if !constraints.allowed_target_paths.is_empty() {
        if let Some(ref files) = spec.files {
            for managed in &files.managed {
                // Folded to `/`: the allow-list globs are authored once in the
                // source manifest and must match identically on every
                // subscriber's OS.
                let target_str = crate::to_posix_string(&managed.target);
                if !path_matches_any(&target_str, &constraints.allowed_target_paths) {
                    violations.push(
                        CompositionError::PathNotAllowed {
                            source_name: source_name.to_string(),
                            path: target_str,
                        }
                        .into(),
                    );
                }
            }
        }

        // A backup's `destination` is a path the source makes cfgd WRITE to, so
        // it is bound by the same allow-list as a managed file's target. Its
        // `source` is deliberately unconstrained: snapshotting a path the
        // allow-list does not cover (`~/.ssh` before a risky apply) is the
        // feature's primary use, and a snapshot can only ever land inside a
        // destination this check already covers.
        for backup in &spec.backups {
            let Some(ref destination) = backup.destination else {
                // The default destination is `<state_dir>/backups/<name>/` —
                // cfgd's own state dir, not a path the source chose.
                continue;
            };
            let destination_str = crate::to_posix_string(destination);
            if !path_matches_any(&destination_str, &constraints.allowed_target_paths) {
                violations.push(
                    CompositionError::PathNotAllowed {
                        source_name: source_name.to_string(),
                        path: destination_str,
                    }
                    .into(),
                );
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
            let target_str = crate::to_posix_string(&managed.target);
            let Some(matched_pattern) =
                find_matching_pattern(&target_str, &enc_constraint.required_targets)
            else {
                continue;
            };
            match managed.encryption.as_ref() {
                None => violations.push(
                    CompositionError::EncryptionRequired {
                        source_name: source_name.to_string(),
                        path: target_str,
                        pattern: matched_pattern,
                    }
                    .into(),
                ),
                Some(enc_spec) => {
                    if let Some(ref required_backend) = enc_constraint.backend
                        && enc_spec.backend != *required_backend
                    {
                        violations.push(
                            CompositionError::EncryptionBackendMismatch {
                                source_name: source_name.to_string(),
                                path: target_str.clone(),
                                pattern: matched_pattern.clone(),
                                actual_backend: enc_spec.backend.clone(),
                                required_backend: required_backend.clone(),
                            }
                            .into(),
                        );
                    }
                    if let Some(ref required_mode) = enc_constraint.mode
                        && enc_spec.mode != *required_mode
                    {
                        violations.push(
                            CompositionError::EncryptionModeMismatch {
                                source_name: source_name.to_string(),
                                path: target_str,
                                pattern: matched_pattern,
                                actual_mode: format!("{:?}", enc_spec.mode),
                                required_mode: format!("{:?}", required_mode),
                            }
                            .into(),
                        );
                    }
                }
            }
        }
    }

    violations
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
                    resource: crate::to_posix_string(&locked_file.target),
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
