use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use cfgd_core::errors::{CfgdError, GenerateError};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_FILE_SIZE: u64 = 64 * 1024; // 64 KB

const BLOCKED_PATTERNS: &[&str] = &[
    ".ssh/id_",
    ".gnupg/private-keys",
    ".pem",
    ".key",
    "credentials",
    "secret",
    "token",
];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReadResult {
    pub path: PathBuf,
    pub content: String,
    pub size_bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub entry_type: String,
    pub size_bytes: Option<u64>,
}

// ---------------------------------------------------------------------------
// Security model
// ---------------------------------------------------------------------------

/// Check if a path is within home or repo root AND not matched by any blocked pattern.
///
/// The check has two parts:
/// - Canonicalize the path (resolves symlinks — symlinks pointing outside allowed
///   roots are caught here) and confirm it starts within `home` OR `repo_root`.
/// - Verify the path string doesn't match any BLOCKED_PATTERNS substring.
fn is_path_allowed(path: &Path, home: &Path, repo_root: &Path) -> Result<(), CfgdError> {
    // Canonicalize and check containment.
    // We try home first, then repo_root. If both fail we reject.
    let in_home = cfgd_core::validate_path_within(path, home).is_ok();
    let in_repo = cfgd_core::validate_path_within(path, repo_root).is_ok();

    if !in_home && !in_repo {
        return Err(CfgdError::Generate(GenerateError::FileAccessDenied {
            path: path.to_path_buf(),
            reason: "path is outside home directory and repository root".to_string(),
        }));
    }

    // Check blocked patterns against the path string. Normalize backslashes
    // to forward slashes so patterns like ".ssh/id_" match on Windows too.
    let path_str = path.to_string_lossy().replace('\\', "/");
    for pattern in BLOCKED_PATTERNS {
        if path_str.contains(pattern) {
            return Err(CfgdError::Generate(GenerateError::FileAccessDenied {
                path: path.to_path_buf(),
                reason: format!("path matches blocked pattern '{pattern}'"),
            }));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Read a file with security constraints.
///
/// The file must be within `home` or `repo_root` and must not match any
/// `BLOCKED_PATTERNS`. Files larger than `MAX_FILE_SIZE` bytes are truncated;
/// the `truncated` field of the returned [`FileReadResult`] indicates this.
pub fn read_file(path: &Path, home: &Path, repo_root: &Path) -> Result<FileReadResult, CfgdError> {
    is_path_allowed(path, home, repo_root)?;

    let meta = fs::metadata(path).map_err(|e| {
        CfgdError::Generate(GenerateError::FileAccessDenied {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })
    })?;

    let size_bytes = meta.len();

    let (content, truncated) = if size_bytes > MAX_FILE_SIZE {
        // Read only the first MAX_FILE_SIZE bytes.
        use std::io::Read;
        let mut f = fs::File::open(path).map_err(|e| {
            CfgdError::Generate(GenerateError::FileAccessDenied {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })
        })?;
        let mut buf = vec![0u8; MAX_FILE_SIZE as usize];
        f.read_exact(&mut buf).map_err(|e| {
            CfgdError::Generate(GenerateError::FileAccessDenied {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })
        })?;
        // Convert to UTF-8, replacing invalid sequences.
        (String::from_utf8_lossy(&buf).into_owned(), true)
    } else {
        let raw = fs::read(path).map_err(|e| {
            CfgdError::Generate(GenerateError::FileAccessDenied {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })
        })?;
        (String::from_utf8_lossy(&raw).into_owned(), false)
    };

    Ok(FileReadResult {
        path: path.to_path_buf(),
        content,
        size_bytes,
        truncated,
    })
}

/// List directory entries with security constraints.
///
/// The directory must be within `home` or `repo_root` and must not match any
/// `BLOCKED_PATTERNS`. Returns one [`DirectoryEntry`] per entry, with
/// `entry_type` set to `"file"`, `"directory"`, or `"symlink"`.
pub fn list_directory(
    path: &Path,
    home: &Path,
    repo_root: &Path,
) -> Result<Vec<DirectoryEntry>, CfgdError> {
    is_path_allowed(path, home, repo_root)?;

    let read_dir = fs::read_dir(path).map_err(|e| {
        CfgdError::Generate(GenerateError::FileAccessDenied {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })
    })?;

    let mut entries = Vec::new();

    for item in read_dir {
        let item = item.map_err(|e| {
            CfgdError::Generate(GenerateError::FileAccessDenied {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })
        })?;

        let name = item.file_name().to_string_lossy().into_owned();

        // Use symlink_metadata so we classify symlinks as "symlink" rather than
        // following them and reporting the target type.
        let meta = std::fs::symlink_metadata(item.path()).map_err(|e| {
            CfgdError::Generate(GenerateError::FileAccessDenied {
                path: item.path(),
                reason: e.to_string(),
            })
        })?;

        let file_type = item.file_type().map_err(|e| {
            CfgdError::Generate(GenerateError::FileAccessDenied {
                path: item.path(),
                reason: e.to_string(),
            })
        })?;

        let entry_type = if file_type.is_symlink() {
            "symlink".to_string()
        } else if file_type.is_dir() {
            "directory".to_string()
        } else {
            "file".to_string()
        };

        let size_bytes = if file_type.is_file() {
            Some(meta.len())
        } else {
            None
        };

        entries.push(DirectoryEntry {
            name,
            entry_type,
            size_bytes,
        });
    }

    // Stable order for deterministic output.
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(entries)
}

/// Copy config files into module/profile directories.
///
/// `source_paths` is a slice of `(source_path, relative_dest)` pairs. Each
/// source is copied to `target_dir/relative_dest`. Parent directories are
/// created as needed. Returns the list of written absolute destination paths.
///
/// Callers must validate source paths via `is_path_allowed` before passing
/// them here. This function does not perform security checks on source paths.
pub fn adopt_files(
    source_paths: &[(PathBuf, PathBuf)],
    target_dir: &Path,
) -> Result<Vec<PathBuf>, CfgdError> {
    let mut written = Vec::new();

    for (source, relative_dest) in source_paths {
        let destination = target_dir.join(relative_dest);

        let content = fs::read(source).map_err(|e| {
            CfgdError::Generate(GenerateError::FileAccessDenied {
                path: source.clone(),
                reason: e.to_string(),
            })
        })?;

        // atomic_write creates parent dirs itself, but let's be explicit so errors
        // surface with a clear path context.
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                CfgdError::Generate(GenerateError::FileAccessDenied {
                    path: parent.to_path_buf(),
                    reason: e.to_string(),
                })
            })?;
        }

        cfgd_core::atomic_write(&destination, &content).map_err(|e| {
            CfgdError::Generate(GenerateError::FileAccessDenied {
                path: destination.clone(),
                reason: e.to_string(),
            })
        })?;

        written.push(destination);
    }

    Ok(written)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    // Helper: create a temp home dir and return (TempDir, PathBuf).
    fn make_home() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    // Helper: create a distinct repo-root temp dir.
    fn make_repo() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    // ---------------------------------------------------------------------------
    // read_file tests
    // ---------------------------------------------------------------------------

    #[test]
    fn test_read_file_within_home() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();

        let file = home.join("config.toml");
        fs::write(&file, "key = \"value\"").unwrap();

        let result = read_file(&file, &home, &repo).unwrap();
        assert_eq!(result.content, "key = \"value\"");
        assert!(!result.truncated);
        assert_eq!(result.size_bytes, 13);
    }

    #[test]
    fn test_read_file_outside_home_rejected() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();

        // Create a file in a completely separate temp dir.
        let other_dir = TempDir::new().unwrap();
        let file = other_dir.path().join("outside.txt");
        fs::write(&file, "data").unwrap();

        let err = read_file(&file, &home, &repo).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("outside home directory") || msg.contains("access denied"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_read_file_ssh_key_blocked() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();

        let ssh_dir = home.join(".ssh");
        fs::create_dir_all(&ssh_dir).unwrap();
        let key_file = ssh_dir.join("id_rsa");
        fs::write(&key_file, "-----BEGIN RSA PRIVATE KEY-----").unwrap();

        let err = read_file(&key_file, &home, &repo).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("blocked pattern") || msg.contains("access denied"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_read_file_truncation() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();

        // Write 128 KB file (double the limit).
        let file = home.join("large.txt");
        let content = vec![b'A'; 128 * 1024];
        fs::write(&file, &content).unwrap();

        let result = read_file(&file, &home, &repo).unwrap();
        assert!(result.truncated, "expected truncated=true for 128 KB file");
        assert_eq!(result.content.len(), MAX_FILE_SIZE as usize);
        assert_eq!(result.size_bytes, 128 * 1024);
    }

    #[test]
    fn test_read_file_at_size_limit_not_truncated() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();

        let file = home.join("exact.txt");
        let content = vec![b'B'; MAX_FILE_SIZE as usize];
        fs::write(&file, &content).unwrap();

        let result = read_file(&file, &home, &repo).unwrap();
        assert!(!result.truncated);
        assert_eq!(result.content.len(), MAX_FILE_SIZE as usize);
    }

    #[test]
    fn test_read_file_within_repo_root() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();

        let file = repo.join("cfgd.yaml");
        fs::write(&file, "apiVersion: cfgd.io/v1alpha1").unwrap();

        let result = read_file(&file, &home, &repo).unwrap();
        assert_eq!(result.content, "apiVersion: cfgd.io/v1alpha1");
        assert!(!result.truncated);
    }

    // ---------------------------------------------------------------------------
    // list_directory tests
    // ---------------------------------------------------------------------------

    #[test]
    fn test_list_directory_within_home() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();

        let sub = home.join("dotfiles");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("zshrc"), "# zsh config").unwrap();
        fs::write(sub.join("vimrc"), "\" vim config").unwrap();
        fs::create_dir_all(sub.join("vim")).unwrap();

        let entries = list_directory(&sub, &home, &repo).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"zshrc"), "missing zshrc: {names:?}");
        assert!(names.contains(&"vimrc"), "missing vimrc: {names:?}");
        assert!(names.contains(&"vim"), "missing vim dir: {names:?}");

        let vim_entry = entries.iter().find(|e| e.name == "vim").unwrap();
        assert_eq!(vim_entry.entry_type, "directory");
        assert!(vim_entry.size_bytes.is_none());

        let zshrc_entry = entries.iter().find(|e| e.name == "zshrc").unwrap();
        assert_eq!(zshrc_entry.entry_type, "file");
        assert!(zshrc_entry.size_bytes.is_some());
    }

    #[test]
    fn test_list_directory_outside_home_rejected() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();

        let other = TempDir::new().unwrap();
        let err = list_directory(other.path(), &home, &repo).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("outside home directory") || msg.contains("access denied"),
            "unexpected error: {msg}"
        );
    }

    // ---------------------------------------------------------------------------
    // adopt_files tests
    // ---------------------------------------------------------------------------

    #[test]
    fn test_adopt_files_copies_to_target() {
        let (_home_dir, home) = make_home();
        let target_dir = TempDir::new().unwrap();

        // Write source files.
        let src1 = home.join("zshrc");
        fs::write(&src1, "# zsh config").unwrap();
        let src2 = home.join("vimrc");
        fs::write(&src2, "\" vim config").unwrap();

        let pairs = vec![
            (src1.clone(), PathBuf::from("shell/zshrc")),
            (src2.clone(), PathBuf::from("editor/vimrc")),
        ];

        let written = adopt_files(&pairs, target_dir.path()).unwrap();

        assert_eq!(written.len(), 2);
        let zshrc_dest = target_dir.path().join("shell/zshrc");
        let vimrc_dest = target_dir.path().join("editor/vimrc");
        assert!(zshrc_dest.exists(), "zshrc not copied");
        assert!(vimrc_dest.exists(), "vimrc not copied");
        assert_eq!(fs::read_to_string(&zshrc_dest).unwrap(), "# zsh config");
        assert_eq!(fs::read_to_string(&vimrc_dest).unwrap(), "\" vim config");
    }

    #[test]
    fn test_adopt_files_creates_parent_dirs() {
        let (_home_dir, home) = make_home();
        let target_dir = TempDir::new().unwrap();

        let src = home.join("bashrc");
        fs::write(&src, "# bash").unwrap();

        let pairs = vec![(src, PathBuf::from("deeply/nested/dir/bashrc"))];
        let written = adopt_files(&pairs, target_dir.path()).unwrap();

        assert_eq!(written.len(), 1);
        assert!(target_dir.path().join("deeply/nested/dir/bashrc").exists());
    }

    // ---------------------------------------------------------------------------
    // Symlink escape test
    // ---------------------------------------------------------------------------

    #[test]
    #[cfg(unix)]
    fn test_symlink_escape_blocked() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();

        // Create a file outside both home and repo.
        let secret_dir = TempDir::new().unwrap();
        let secret_file = secret_dir.path().join("secret.txt");
        fs::write(&secret_file, "top secret").unwrap();

        // Create a symlink inside home pointing outside.
        let link = home.join("escape_link");
        std::os::unix::fs::symlink(&secret_file, &link).unwrap();

        let err = read_file(&link, &home, &repo).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("outside home directory")
                || msg.contains("access denied")
                || msg.contains("escapes root"),
            "symlink escape should be blocked, got: {msg}"
        );
    }

    // ---------------------------------------------------------------------------
    // read_file — error paths
    // ---------------------------------------------------------------------------

    #[test]
    fn read_file_nonexistent_within_home_returns_access_denied_error() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();
        // metadata() lookup fails — the function maps to FileAccessDenied.
        let missing = home.join("does-not-exist.txt");
        let err = read_file(&missing, &home, &repo).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("does-not-exist.txt") || msg.contains("access denied"),
            "error must reference the missing path: {msg}"
        );
    }

    #[test]
    fn read_file_blocked_pattern_pem_rejected() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();
        let pem = home.join("server.pem");
        fs::write(&pem, "fake pem").unwrap();
        let err = read_file(&pem, &home, &repo).unwrap_err();
        assert!(
            err.to_string().contains(".pem") || err.to_string().contains("blocked"),
            "blocked-pattern path must surface in error"
        );
    }

    #[test]
    fn read_file_blocked_pattern_credentials_rejected() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();
        let path = home.join("aws-credentials.txt");
        fs::write(&path, "fake creds").unwrap();
        let err = read_file(&path, &home, &repo).unwrap_err();
        assert!(err.to_string().contains("blocked") || err.to_string().contains("credentials"));
    }

    #[test]
    fn read_file_blocked_pattern_secret_rejected() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();
        let path = home.join("my-secret-token");
        fs::write(&path, "data").unwrap();
        // Matches multiple patterns ("secret" and "token") — either is fine.
        let err = read_file(&path, &home, &repo).unwrap_err();
        assert!(err.to_string().contains("blocked"));
    }

    #[test]
    fn read_file_just_under_size_limit_reads_full_content() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();
        let file = home.join("near-limit.txt");
        let content = vec![b'C'; MAX_FILE_SIZE as usize - 1];
        fs::write(&file, &content).unwrap();

        let result = read_file(&file, &home, &repo).unwrap();
        assert!(!result.truncated);
        assert_eq!(result.size_bytes, MAX_FILE_SIZE - 1);
        assert_eq!(result.content.len(), (MAX_FILE_SIZE - 1) as usize);
    }

    // ---------------------------------------------------------------------------
    // list_directory — error and edge cases
    // ---------------------------------------------------------------------------

    #[test]
    fn list_directory_nonexistent_within_home_returns_access_denied() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();
        let missing = home.join("no-such-dir");
        // No mkdir — read_dir will fail with NotFound and map to access denied.
        let err = list_directory(&missing, &home, &repo).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no-such-dir") || msg.contains("access denied"),
            "error must reference the missing dir: {msg}"
        );
    }

    #[test]
    fn list_directory_empty_directory_returns_empty_vec() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();
        let empty = home.join("empty-dir");
        fs::create_dir_all(&empty).unwrap();
        let entries = list_directory(&empty, &home, &repo).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn list_directory_entries_sorted_alphabetically() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();
        let dir = home.join("sorted");
        fs::create_dir_all(&dir).unwrap();
        // Insertion order != sorted order; output must be sorted.
        for name in ["zeta.txt", "alpha.txt", "mid.txt"] {
            fs::write(dir.join(name), "x").unwrap();
        }
        let entries = list_directory(&dir, &home, &repo).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha.txt", "mid.txt", "zeta.txt"]);
    }

    #[test]
    #[cfg(unix)]
    fn list_directory_classifies_symlinks_with_no_size() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();
        let dir = home.join("withlink");
        fs::create_dir_all(&dir).unwrap();
        // Target file inside home so containment check passes.
        let target = home.join("target.txt");
        fs::write(&target, "target").unwrap();
        let link = dir.join("the_link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let entries = list_directory(&dir, &home, &repo).unwrap();
        let sym = entries
            .iter()
            .find(|e| e.name == "the_link")
            .expect("symlink entry present");
        assert_eq!(sym.entry_type, "symlink");
        assert!(
            sym.size_bytes.is_none(),
            "symlinks must not report a file size (size_bytes is for regular files only)"
        );
    }

    #[test]
    fn list_directory_within_repo_root_allowed() {
        let (_home_dir, home) = make_home();
        let (_repo_dir, repo) = make_repo();
        let dir = repo.join("subdir");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.yaml"), "x").unwrap();
        let entries = list_directory(&dir, &home, &repo).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.yaml");
        assert_eq!(entries[0].entry_type, "file");
    }

    // ---------------------------------------------------------------------------
    // adopt_files — error paths
    // ---------------------------------------------------------------------------

    #[test]
    fn adopt_files_nonexistent_source_returns_access_denied() {
        let (_home_dir, home) = make_home();
        let target = TempDir::new().unwrap();
        let missing = home.join("does-not-exist.txt");
        let pairs = vec![(missing.clone(), PathBuf::from("dst/file"))];
        let err = adopt_files(&pairs, target.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("does-not-exist.txt") || msg.contains("access denied"),
            "error must reference the missing source path: {msg}"
        );
    }

    #[test]
    fn adopt_files_empty_pairs_returns_empty_vec() {
        let target = TempDir::new().unwrap();
        let written = adopt_files(&[], target.path()).unwrap();
        assert!(written.is_empty());
    }

    #[test]
    fn adopt_files_overwrites_existing_destination() {
        let (_home_dir, home) = make_home();
        let target = TempDir::new().unwrap();
        let src = home.join("config.toml");
        fs::write(&src, "new content").unwrap();
        // Pre-existing destination file with old content.
        let dest_rel = PathBuf::from("dotfiles/config.toml");
        let dest_abs = target.path().join(&dest_rel);
        fs::create_dir_all(dest_abs.parent().unwrap()).unwrap();
        fs::write(&dest_abs, "old content").unwrap();

        let pairs = vec![(src.clone(), dest_rel.clone())];
        let written = adopt_files(&pairs, target.path()).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0], dest_abs);
        let content = fs::read_to_string(&dest_abs).unwrap();
        assert_eq!(content, "new content");
    }

    #[test]
    fn adopt_files_returns_absolute_destination_paths() {
        let (_home_dir, home) = make_home();
        let target = TempDir::new().unwrap();
        let src = home.join("a");
        fs::write(&src, "1").unwrap();
        let pairs = vec![(src, PathBuf::from("nested/a"))];
        let written = adopt_files(&pairs, target.path()).unwrap();
        assert!(written[0].is_absolute());
        assert!(written[0].starts_with(target.path()));
    }
}
