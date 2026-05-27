// tar.gz packaging and extraction for OCI module layers.
// Skips symlinks/hardlinks both during creation and extraction to prevent
// path-traversal attacks via crafted archives.

use std::path::Path;

use crate::PathDisplayExt;
use crate::errors::OciError;

/// Create a tar.gz archive of a directory's contents.
pub fn create_tar_gz(dir: &Path) -> Result<Vec<u8>, OciError> {
    let buf = Vec::new();
    let encoder = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);

    // Add directory contents relative to dir
    add_dir_to_tar(&mut archive, dir, dir)?;

    let encoder = archive.into_inner().map_err(|e| OciError::ArchiveError {
        message: format!("tar finalization failed: {e}"),
    })?;
    let compressed = encoder.finish().map_err(|e| OciError::ArchiveError {
        message: format!("gzip finalization failed: {e}"),
    })?;
    Ok(compressed)
}

fn add_dir_to_tar<W: std::io::Write>(
    archive: &mut tar::Builder<W>,
    dir: &Path,
    root: &Path,
) -> Result<(), OciError> {
    let entries = std::fs::read_dir(dir).map_err(|e| OciError::ArchiveError {
        message: format!("cannot read directory {}: {e}", dir.posix()),
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| OciError::ArchiveError {
            message: format!("directory entry error: {e}"),
        })?;
        let file_type = entry.file_type().map_err(|e| OciError::ArchiveError {
            message: format!("file type error: {e}"),
        })?;
        // Skip symlinks — prevents including content from outside the module directory
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|e| OciError::ArchiveError {
                message: format!("path prefix error: {e}"),
            })?;

        if file_type.is_dir() {
            archive
                .append_dir(relative, &path)
                .map_err(|e| OciError::ArchiveError {
                    message: format!("tar append dir failed: {e}"),
                })?;
            add_dir_to_tar(archive, &path, root)?;
        } else {
            archive
                .append_path_with_name(&path, relative)
                .map_err(|e| OciError::ArchiveError {
                    message: format!("tar append file failed: {e}"),
                })?;
        }
    }
    Ok(())
}

/// Extract a tar.gz archive into a directory.
pub fn extract_tar_gz(data: &[u8], output_dir: &Path) -> Result<(), OciError> {
    let decoder = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(decoder);

    std::fs::create_dir_all(output_dir).map_err(|e| OciError::ArchiveError {
        message: format!("cannot create output directory: {e}"),
    })?;

    // Extract entries individually to validate against symlink attacks.
    // The tar crate rejects `..` and absolute paths by default (since 0.4.26),
    // but symlinks can still point outside the output directory.
    let canonical_output = output_dir
        .canonicalize()
        .map_err(|e| OciError::ArchiveError {
            message: format!("cannot canonicalize output directory: {e}"),
        })?;

    for entry in archive.entries().map_err(|e| OciError::ArchiveError {
        message: format!("tar iteration failed: {e}"),
    })? {
        let mut entry = entry.map_err(|e| OciError::ArchiveError {
            message: format!("tar entry read failed: {e}"),
        })?;

        // Skip symlinks — prevents symlink-based path traversal
        if entry.header().entry_type().is_symlink() || entry.header().entry_type().is_hard_link() {
            let path = entry.path().unwrap_or_default();
            tracing::warn!(path = %path.posix(), "skipping symlink/hardlink in OCI archive");
            continue;
        }

        entry
            .unpack_in(&canonical_output)
            .map_err(|e| OciError::ArchiveError {
                message: format!("tar entry extraction failed: {e}"),
            })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- tar+gzip round-trip ---

    #[test]
    fn tar_gz_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();

        // Create test files
        std::fs::write(dir_path.join("module.yaml"), "name: test\n").unwrap();
        std::fs::create_dir(dir_path.join("files")).unwrap();
        std::fs::write(
            dir_path.join("files/config.toml"),
            "[section]\nkey = \"value\"\n",
        )
        .unwrap();

        // Create archive
        let archive = create_tar_gz(dir_path).unwrap();
        assert!(!archive.is_empty());

        // Extract to new directory
        let out_dir = tempfile::tempdir().unwrap();
        extract_tar_gz(&archive, out_dir.path()).unwrap();

        // Verify contents
        let module_yaml = std::fs::read_to_string(out_dir.path().join("module.yaml")).unwrap();
        assert_eq!(module_yaml, "name: test\n");

        let config = std::fs::read_to_string(out_dir.path().join("files/config.toml")).unwrap();
        assert_eq!(config, "[section]\nkey = \"value\"\n");
    }

    // --- tar_gz symlink handling ---

    #[test]
    fn tar_gz_round_trip_via_create_and_extract() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("module");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("module.yaml"), "name: test").unwrap();

        let archive = create_tar_gz(&subdir).unwrap();
        assert!(!archive.is_empty());

        let output = tempfile::tempdir().unwrap();
        let result = extract_tar_gz(&archive, output.path());
        assert!(result.is_ok());
        assert!(output.path().join("module.yaml").exists());
        let content = std::fs::read_to_string(output.path().join("module.yaml")).unwrap();
        assert_eq!(content, "name: test");
    }

    // --- extract_tar_gz path traversal protection ---

    #[test]
    fn extract_tar_gz_prevents_path_traversal_via_dotdot() {
        // Build a tar.gz archive with an entry whose path contains ".."
        // We must write the raw tar bytes to bypass the tar crate's own
        // set_path safety checks (which also reject "..")
        let mut buf = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);

            let data = b"malicious content";
            let mut header = tar::Header::new_gnu();
            // Set a benign path first, then overwrite the raw bytes
            header.set_path("placeholder.txt").unwrap();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);

            // Overwrite the path field in the raw header with "../escaped.txt"
            let path_bytes = b"../escaped.txt";
            header.as_mut_bytes()[..path_bytes.len()].copy_from_slice(path_bytes);
            header.as_mut_bytes()[path_bytes.len()] = 0; // null-terminate
            header.set_cksum();

            archive.append(&header, &data[..]).unwrap();

            let encoder = archive.into_inner().unwrap();
            encoder.finish().unwrap();
        }

        let output = tempfile::tempdir().unwrap();
        // extract_tar_gz uses unpack_in which silently skips entries with ".."
        // (returns Ok(false) for unsafe paths). The key safety guarantee is
        // that the file is NOT created outside the output directory.
        let _ = extract_tar_gz(&buf, output.path());

        // Verify the malicious file was NOT created outside the output directory
        let parent = output.path().parent().unwrap();
        assert!(
            !parent.join("escaped.txt").exists(),
            "path traversal file should not have been created outside output dir"
        );

        // Also verify nothing was created inside the output directory with this name
        assert!(
            !output.path().join("escaped.txt").exists(),
            "traversal entry should not have been extracted"
        );
    }

    #[test]
    fn extract_tar_gz_skips_symlinks() {
        // Build a tar.gz archive containing a symlink entry
        let mut buf = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);

            // Add a regular file first
            let data = b"normal file content";
            let mut header = tar::Header::new_gnu();
            header.set_path("normal.txt").unwrap();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append(&header, &data[..]).unwrap();

            // Add a symlink pointing outside the output directory
            let mut sym_header = tar::Header::new_gnu();
            sym_header.set_path("evil_link").unwrap();
            sym_header.set_entry_type(tar::EntryType::Symlink);
            sym_header.set_link_name("/etc/passwd").unwrap();
            sym_header.set_size(0);
            sym_header.set_mode(0o777);
            sym_header.set_cksum();
            archive.append(&sym_header, &[][..]).unwrap();

            let encoder = archive.into_inner().unwrap();
            encoder.finish().unwrap();
        }

        let output = tempfile::tempdir().unwrap();
        let result = extract_tar_gz(&buf, output.path());
        assert!(
            result.is_ok(),
            "extraction should succeed, skipping symlinks"
        );

        // The regular file should exist
        let normal_content = std::fs::read_to_string(output.path().join("normal.txt")).unwrap();
        assert_eq!(normal_content, "normal file content");

        // The symlink should NOT have been created
        assert!(
            !output.path().join("evil_link").exists(),
            "symlink entry should have been skipped during extraction"
        );
    }

    #[test]
    fn extract_tar_gz_skips_hardlinks() {
        // Build a tar.gz archive containing a hardlink entry
        let mut buf = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);

            // Add a regular file
            let data = b"target content";
            let mut header = tar::Header::new_gnu();
            header.set_path("target.txt").unwrap();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append(&header, &data[..]).unwrap();

            // Add a hardlink
            let mut link_header = tar::Header::new_gnu();
            link_header.set_path("hardlink.txt").unwrap();
            link_header.set_entry_type(tar::EntryType::Link);
            link_header.set_link_name("target.txt").unwrap();
            link_header.set_size(0);
            link_header.set_mode(0o644);
            link_header.set_cksum();
            archive.append(&link_header, &[][..]).unwrap();

            let encoder = archive.into_inner().unwrap();
            encoder.finish().unwrap();
        }

        let output = tempfile::tempdir().unwrap();
        let result = extract_tar_gz(&buf, output.path());
        assert!(
            result.is_ok(),
            "extraction should succeed, skipping hardlinks"
        );

        // Regular file should exist
        assert!(output.path().join("target.txt").exists());
        let content = std::fs::read_to_string(output.path().join("target.txt")).unwrap();
        assert_eq!(content, "target content");

        // Hardlink should NOT have been created (it gets skipped)
        assert!(
            !output.path().join("hardlink.txt").exists(),
            "hardlink entry should have been skipped during extraction"
        );
    }

    #[test]
    fn extract_tar_gz_extracts_multiple_files_preserving_directories() {
        // Build a tar.gz archive with nested structure
        let mut buf = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);

            // Add a directory entry
            let mut dir_header = tar::Header::new_gnu();
            dir_header.set_path("subdir/").unwrap();
            dir_header.set_entry_type(tar::EntryType::Directory);
            dir_header.set_size(0);
            dir_header.set_mode(0o755);
            dir_header.set_cksum();
            archive.append(&dir_header, &[][..]).unwrap();

            // Add files in subdirectory
            let data1 = b"file one";
            let mut h1 = tar::Header::new_gnu();
            h1.set_path("subdir/one.txt").unwrap();
            h1.set_size(data1.len() as u64);
            h1.set_mode(0o644);
            h1.set_cksum();
            archive.append(&h1, &data1[..]).unwrap();

            let data2 = b"file two";
            let mut h2 = tar::Header::new_gnu();
            h2.set_path("subdir/two.txt").unwrap();
            h2.set_size(data2.len() as u64);
            h2.set_mode(0o644);
            h2.set_cksum();
            archive.append(&h2, &data2[..]).unwrap();

            let encoder = archive.into_inner().unwrap();
            encoder.finish().unwrap();
        }

        let output = tempfile::tempdir().unwrap();
        extract_tar_gz(&buf, output.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(output.path().join("subdir/one.txt")).unwrap(),
            "file one"
        );
        assert_eq!(
            std::fs::read_to_string(output.path().join("subdir/two.txt")).unwrap(),
            "file two"
        );
    }

    #[test]
    fn extract_tar_gz_empty_archive() {
        // Build a tar.gz with no entries
        let mut buf = Vec::new();
        {
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let archive = tar::Builder::new(encoder);
            let encoder = archive.into_inner().unwrap();
            encoder.finish().unwrap();
        }

        let output = tempfile::tempdir().unwrap();
        let result = extract_tar_gz(&buf, output.path());
        assert!(result.is_ok(), "empty archive should extract successfully");
        // Output directory should exist but be empty
        assert!(output.path().exists());
        let entries: Vec<_> = std::fs::read_dir(output.path()).unwrap().collect();
        assert_eq!(entries.len(), 0, "output directory should be empty");
    }

    // --- create_tar_gz: symlinks skipped ---

    #[test]
    #[cfg(unix)]
    fn create_tar_gz_skips_symlinks_in_source_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "content").unwrap();
        // Create a symlink
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("link")).unwrap();

        let archive = create_tar_gz(dir.path()).unwrap();

        // Extract and verify only the regular file exists
        let out = tempfile::tempdir().unwrap();
        extract_tar_gz(&archive, out.path()).unwrap();

        assert!(out.path().join("file.txt").exists());
        // The symlink should not have been included in the archive
        assert!(
            !out.path().join("link").exists(),
            "symlinks should be skipped during archive creation"
        );
    }

    // --- create_tar_gz with nested directories ---

    #[test]
    fn create_tar_gz_nested_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("deep.txt"), "nested content").unwrap();
        std::fs::write(dir.path().join("root.txt"), "top level").unwrap();

        let archive = create_tar_gz(dir.path()).unwrap();

        let out = tempfile::tempdir().unwrap();
        extract_tar_gz(&archive, out.path()).unwrap();

        let root_content = std::fs::read_to_string(out.path().join("root.txt")).unwrap();
        assert_eq!(root_content, "top level");

        let deep_content = std::fs::read_to_string(out.path().join("a/b/c/deep.txt")).unwrap();
        assert_eq!(deep_content, "nested content");
    }

    // --- create_tar_gz empty directory ---

    #[test]
    fn create_tar_gz_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let archive = create_tar_gz(dir.path()).unwrap();
        assert!(
            !archive.is_empty(),
            "even empty dir should produce a valid gzip stream"
        );

        let out = tempfile::tempdir().unwrap();
        let result = extract_tar_gz(&archive, out.path());
        assert!(result.is_ok());
    }

    // --- tar gz with symlinks skipped ---

    #[test]
    fn create_tar_gz_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.txt"), "real content").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("real.txt", dir.path().join("link.txt")).unwrap();

        let archive = create_tar_gz(dir.path()).unwrap();
        assert!(!archive.is_empty());

        // Extract and verify symlink was skipped
        let out = tempfile::tempdir().unwrap();
        extract_tar_gz(&archive, out.path()).unwrap();
        assert!(out.path().join("real.txt").exists());
        // Symlink should NOT be in the extracted output
        assert!(
            !out.path().join("link.txt").exists(),
            "symlinks should be skipped in create_tar_gz"
        );
    }

    // --- extract_tar_gz rejects symlinks ---

    #[test]
    fn extract_tar_gz_skips_symlinks_in_archive() {
        // Build a tar.gz that contains a symlink entry
        use flate2::write::GzEncoder;

        let buf = Vec::new();
        let encoder = GzEncoder::new(buf, flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);

        // Add a normal file
        let content = b"normal file";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, "normal.txt", &content[..])
            .unwrap();

        // Add a symlink entry
        let mut sym_header = tar::Header::new_gnu();
        sym_header.set_entry_type(tar::EntryType::Symlink);
        sym_header.set_size(0);
        sym_header.set_mode(0o777);
        sym_header.set_cksum();
        archive
            .append_link(&mut sym_header, "evil-link", "/etc/passwd")
            .unwrap();

        let encoder = archive.into_inner().unwrap();
        let compressed = encoder.finish().unwrap();

        let out = tempfile::tempdir().unwrap();
        extract_tar_gz(&compressed, out.path()).unwrap();

        assert!(out.path().join("normal.txt").exists());
        assert!(
            !out.path().join("evil-link").exists(),
            "symlinks should be skipped during extraction"
        );
    }
}
