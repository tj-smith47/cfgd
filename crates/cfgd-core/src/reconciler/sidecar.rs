//! The sidecar copy cfgd leaves beside a target it adopts from the user.
//!
//! The copy happens while the file action that displaces the target EXECUTES,
//! not while the plan is built: a preview must change nothing on disk, and the
//! line reporting the copy belongs in the phase whose work it is part of.

use std::path::{Path, PathBuf};

use crate::PathDisplayExt;
use crate::errors::{FileError, Result};

/// Suffix of the sidecar copy cfgd leaves beside a target it adopted.
pub const CFGD_BACKUP_SUFFIX: &str = ".cfgd-backup";

/// How many `-N` disambiguators are tried before a reservation gives up.
///
/// A bound rather than an unbounded scan: past this many distinct originals
/// adopted at one target within one second, the situation is a loop and the
/// honest answer is an error, not a hundredth sidecar.
const BACKUP_DISAMBIGUATOR_LIMIT: u32 = 64;

/// The sidecar path for `target`, suffixed with `extra` (empty for the primary
/// `<target>.cfgd-backup`).
///
/// The ONE derivation of that name: the adoption path writes it, module removal
/// and profile update offer to restore it, and a byte of disagreement between
/// them orphans a user's only copy of their original file. Built by appending
/// to the target's `OsStr` rather than to a rendered `Display`, so a filename
/// no `str` can round-trip still names the file beside it.
pub fn cfgd_backup_path(target: &Path, extra: &str) -> PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(CFGD_BACKUP_SUFFIX);
    name.push(extra);
    PathBuf::from(name)
}

fn failed(target: &Path, message: impl std::fmt::Display) -> crate::errors::CfgdError {
    FileError::BackupFailed {
        path: target.to_path_buf(),
        message: message.to_string(),
    }
    .into()
}

/// What one sidecar copy did: where it landed, and whether a copy already
/// holding the same bytes was reused rather than written again.
///
/// Returned rather than printed, because the copy is part of the action that
/// displaces the target: the run reports it as that action's detail, on the
/// action's own row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarOutcome {
    pub path: PathBuf,
    pub reused: bool,
}

impl SidecarOutcome {
    fn new(path: PathBuf, reused: bool) -> Self {
        Self { path, reused }
    }

    /// The words an action row carries about this copy.
    pub fn detail(&self) -> String {
        let verb = if self.reused {
            "already backed up at"
        } else {
            "backed up to"
        };
        format!("{verb} {}", self.path.posix())
    }
}

/// Copy an unmanaged target aside before cfgd overwrites it, and return where
/// the copy landed.
///
/// A COPY, never a rename. The rename this replaced left a window in which the
/// user's file was at neither path: a crash between the rename and the apply's
/// own write lost it outright. Copying leaves the original at `target` until
/// the write rename-replaces it, so at every instant the content exists at the
/// sidecar, at the target, or at both.
///
/// The copied bytes are re-read and hashed before the copy is reported, so a
/// short write or a full disk is an error rather than a sidecar that silently
/// holds less than the file it claims to preserve.
pub fn backup_file(target: &Path) -> Result<SidecarOutcome> {
    let meta = target
        .symlink_metadata()
        .map_err(|e| failed(target, format!("{e}")))?;

    if meta.file_type().is_symlink() {
        let dest = target
            .read_link()
            .map_err(|e| failed(target, format!("{e}")))?;
        // Reserved unoccupied, so the link is created rather than replacing
        // whatever a previous adoption left — including a dangling link, which
        // `symlink_metadata` still counts as an entry someone made.
        let backup_path = reserve_backup_path(target, None)?;
        crate::create_symlink(&dest, &backup_path).map_err(|e| failed(target, format!("{e}")))?;
        return Ok(SidecarOutcome::new(backup_path, false));
    }

    if meta.is_dir() {
        // Same reservation, and load-bearing here: `copy_dir_recursive` writes
        // INTO an existing directory, so an occupied sidecar would silently
        // merge two different originals into one tree.
        let backup_path = reserve_backup_path(target, None)?;
        crate::copy_dir_recursive(target, &backup_path)
            .map_err(|e| failed(target, format!("{e}")))?;
        return Ok(SidecarOutcome::new(backup_path, false));
    }

    let content = std::fs::read(target).map_err(|e| failed(target, format!("{e}")))?;
    let hash = crate::sha256_hex(&content);
    let backup_path = reserve_backup_path(target, Some(&hash))?;
    // An earlier adoption already preserved these exact bytes; rewriting the
    // sidecar would only widen the window in which it is half-written.
    if sidecar_holds(&backup_path, &hash) {
        return Ok(SidecarOutcome::new(backup_path, true));
    }
    crate::atomic_write(&backup_path, &content).map_err(|e| failed(target, format!("{e}")))?;
    if !sidecar_holds(&backup_path, &hash) {
        return Err(failed(
            target,
            format!(
                "copy to {} did not verify (content hash mismatch)",
                backup_path.posix()
            ),
        ));
    }
    // Full `0o7777`: a sidecar is the file it preserves, and a setuid or sticky
    // bit dropped in the copy is not restorable from it.
    if let Some(mode) = crate::file_permissions_mode_full(&meta) {
        crate::set_file_permissions(&backup_path, mode)
            .map_err(|e| failed(target, format!("mode of {}: {e}", backup_path.posix())))?;
    }
    Ok(SidecarOutcome::new(backup_path, false))
}

/// Where this backup may be written without destroying an older one.
///
/// The primary `<target>.cfgd-backup` is what `profile update` and module
/// removal offer to restore, so it keeps the FIRST content adopted there — the
/// one that predates cfgd. A second, different original is stamped instead of
/// clobbering it, because a sidecar overwritten by the file that displaced it
/// is the same data loss the copy exists to prevent.
///
/// The stamp has one-second resolution, so it is a hint at a free name and
/// never a guarantee of one: two adoptions of the same target inside one second
/// derive the same stamp, and the second would clobber the first. Every
/// candidate is therefore checked, and a taken one moves to `-1`, `-2`, … until
/// a free name is found or the limit is reached.
fn reserve_backup_path(target: &Path, hash: Option<&str>) -> Result<PathBuf> {
    let primary = cfgd_backup_path(target, "");
    if !sidecar_occupied(&primary, hash) {
        return Ok(primary);
    }
    let stamp = crate::utc_now_backup_stamp();
    let stamped = cfgd_backup_path(target, &format!(".{stamp}"));
    if !sidecar_occupied(&stamped, hash) {
        return Ok(stamped);
    }
    for n in 1..=BACKUP_DISAMBIGUATOR_LIMIT {
        let candidate = cfgd_backup_path(target, &format!(".{stamp}-{n}"));
        if !sidecar_occupied(&candidate, hash) {
            return Ok(candidate);
        }
    }
    Err(failed(
        target,
        format!(
            "no free backup path: {} and {} disambiguators are all taken",
            stamped.posix(),
            BACKUP_DISAMBIGUATOR_LIMIT
        ),
    ))
}

/// Whether a candidate sidecar path is spoken for.
///
/// Judged with `symlink_metadata`, so a dangling link or a directory counts as
/// an entry someone made — writing over either is the loss the reservation
/// exists to avoid. `hash` is `Some` only for a regular-file backup, where a
/// sidecar already holding exactly these bytes is not an obstacle but the same
/// backup, and reusing it is the whole point.
fn sidecar_occupied(path: &Path, hash: Option<&str>) -> bool {
    if path.symlink_metadata().is_err() {
        return false;
    }
    match hash {
        Some(h) => !sidecar_holds(path, h),
        None => true,
    }
}

/// Whether the sidecar at `path` is a regular file holding exactly `hash`.
fn sidecar_holds(path: &Path, hash: &str) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.is_file())
        && std::fs::read(path).is_ok_and(|bytes| crate::sha256_hex(&bytes) == hash)
}

#[cfg(test)]
mod tests {
    use crate::PathDisplayExt;

    #[test]
    fn backup_file_copies_to_cfgd_backup_suffix_and_leaves_the_original() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("myfile.txt");
        std::fs::write(&original, "original content").unwrap();

        let written = super::backup_file(&original).unwrap();

        let backup = tmp.path().join("myfile.txt.cfgd-backup");
        assert_eq!(
            written.path, backup,
            "backup should land at the sidecar path"
        );
        assert!(backup.exists(), "backup file should exist at expected path");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "original content",
            "the sidecar must hold the original bytes"
        );
        // The crash window the rename opened: between moving the file away and
        // writing the managed one, the content existed at neither path.
        assert!(
            original.exists(),
            "the original must stay in place until the apply's own atomic write replaces it"
        );
        assert_eq!(
            std::fs::read_to_string(&original).unwrap(),
            "original content",
            "the original content must be untouched by the backup"
        );

        // What the copy did travels back to the caller, which renders it on
        // the action row that displaced the file.
        assert!(!written.reused, "a fresh copy is not a reused one");
        assert_eq!(written.detail(), format!("backed up to {}", backup.posix()),);
    }

    #[test]
    fn backup_file_nonexistent_target_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does_not_exist.txt");

        let result = super::backup_file(&missing);
        assert!(result.is_err(), "backup of nonexistent file should fail");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("failed to back up") && err_msg.contains("does_not_exist.txt"),
            "error should describe the backup failure and name its target, got: {err_msg}"
        );
    }

    #[test]
    fn backup_file_verifies_and_preserves_the_mode_of_its_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("secret.env");
        std::fs::write(&original, "TOKEN=keep-me\n").unwrap();
        crate::set_file_permissions(&original, 0o600).unwrap();

        let backup = super::backup_file(&original).unwrap().path;

        let meta = std::fs::metadata(&backup).unwrap();
        assert_eq!(
            crate::file_permissions_mode(&meta),
            crate::file_permissions_mode(&std::fs::metadata(&original).unwrap()),
            "the sidecar must carry the mode of the file it preserves"
        );
        assert_eq!(
            crate::sha256_hex(&std::fs::read(&backup).unwrap()),
            crate::sha256_hex(&std::fs::read(&original).unwrap()),
            "the sidecar must hash identically to the original"
        );
    }

    #[test]
    fn backup_file_never_clobbers_an_older_sidecar() {
        // The primary sidecar holds the content that predates cfgd; a second,
        // different original is stamped instead of destroying the first.
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("conf.toml");
        let primary = tmp.path().join("conf.toml.cfgd-backup");
        std::fs::write(&primary, "the original").unwrap();
        std::fs::write(&target, "something else entirely").unwrap();

        let written = super::backup_file(&target).unwrap().path;

        assert_ne!(written, primary, "an occupied sidecar must not be reused");
        assert_eq!(
            std::fs::read_to_string(&primary).unwrap(),
            "the original",
            "the older sidecar must survive untouched"
        );
        assert_eq!(
            std::fs::read_to_string(&written).unwrap(),
            "something else entirely"
        );
    }

    #[test]
    fn backup_file_reuses_a_sidecar_that_already_holds_the_same_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("conf.toml");
        let primary = tmp.path().join("conf.toml.cfgd-backup");
        std::fs::write(&target, "same bytes").unwrap();
        std::fs::write(&primary, "same bytes").unwrap();

        let written = super::backup_file(&target).unwrap();

        assert_eq!(
            written.path, primary,
            "an identical sidecar is reused, not stamped"
        );
        let entries: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 2, "no second sidecar should be created");
        assert!(written.reused, "a reused sidecar says so");
        assert!(written.detail().starts_with("already backed up at "));
    }

    #[test]
    fn two_adoptions_in_the_same_second_land_beside_each_other_never_on_top() {
        // The stamp has one-second resolution, so it is a hint at a free name and
        // never a guarantee of one: unchecked, the second adoption of a second
        // original overwrites the sidecar holding the first.
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("live.conf");
        let primary = tmp.path().join("live.conf.cfgd-backup");
        std::fs::write(&primary, "first original").unwrap();

        std::fs::write(&target, "second original").unwrap();
        let second = super::backup_file(&target).unwrap().path;
        std::fs::write(&target, "third original").unwrap();
        let third = super::backup_file(&target).unwrap().path;

        assert_ne!(second, third, "back-to-back adoptions need distinct names");
        assert_eq!(std::fs::read_to_string(&primary).unwrap(), "first original");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "second original");
        assert_eq!(std::fs::read_to_string(&third).unwrap(), "third original");
    }

    #[test]
    fn a_directory_backup_never_merges_into_an_occupied_sidecar() {
        // `copy_dir_recursive` writes INTO an existing directory, so an occupied
        // sidecar silently fuses two different originals into one tree.
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("conf.d");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("new.conf"), "new").unwrap();

        let primary = tmp.path().join("conf.d.cfgd-backup");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::write(primary.join("old.conf"), "old").unwrap();

        let first = super::backup_file(&target).unwrap().path;

        // A second, different original in the same second: the stamp alone would
        // name the directory the first one just filled.
        std::fs::remove_file(target.join("new.conf")).unwrap();
        std::fs::write(target.join("newer.conf"), "newer").unwrap();
        let second = super::backup_file(&target).unwrap().path;

        assert_ne!(
            first, primary,
            "an occupied sidecar directory is not reused"
        );
        assert_ne!(first, second, "two originals need two directories");
        assert_eq!(
            std::fs::read_dir(&primary).unwrap().count(),
            1,
            "the older sidecar must not gain the newer originals' entries"
        );
        assert!(primary.join("old.conf").exists());
        assert!(first.join("new.conf").exists() && !first.join("newer.conf").exists());
        assert!(second.join("newer.conf").exists() && !second.join("new.conf").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_sidecar_carries_the_setuid_bit_of_the_file_it_preserves() {
        // A backup is the file it preserves; a special bit dropped in the copy is
        // not restorable from it.
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("helper.sh");
        std::fs::write(&target, "#!/bin/sh\n").unwrap();
        crate::set_file_permissions(&target, 0o4755).unwrap();

        let backup = super::backup_file(&target).unwrap().path;

        let mode = crate::file_permissions_mode_full(&std::fs::metadata(&backup).unwrap());
        assert_eq!(
            mode,
            Some(0o4755),
            "the sidecar must reproduce the mode it is a copy of"
        );
    }

    #[test]
    fn a_symlinked_target_is_backed_up_as_a_link_not_as_its_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("elsewhere.conf");
        let target = tmp.path().join("live.conf");
        std::fs::write(&dest, "the destination\n").unwrap();
        crate::create_symlink(&dest, &target).unwrap();

        let backup = super::backup_file(&target).unwrap().path;

        assert_eq!(
            backup.read_link().unwrap(),
            dest,
            "the sidecar must preserve the link, not materialize its destination"
        );
        assert!(target.symlink_metadata().is_ok(), "the link stays in place");
    }
}
