// Container-based module build: detect docker/podman, generate a Dockerfile
// for the target base image, build the image, and copy the result out into a
// directory ready for `push_module()`.

use std::path::Path;

use crate::errors::OciError;

const DOCKER_BIN_ENV: &str = "CFGD_DOCKER_BIN";
const PODMAN_BIN_ENV: &str = "CFGD_PODMAN_BIN";

/// Detect which container runtime is available (docker or podman).
pub fn detect_container_runtime() -> Option<&'static str> {
    if crate::command_available_with_seam(DOCKER_BIN_ENV, "docker") {
        Some("docker")
    } else if crate::command_available_with_seam(PODMAN_BIN_ENV, "podman") {
        Some("podman")
    } else {
        None
    }
}

/// Build a `Command` for the given container runtime, honoring env-var seams.
fn runtime_cmd(runtime: &str) -> std::process::Command {
    match runtime {
        "docker" => crate::tool_cmd(DOCKER_BIN_ENV, "docker"),
        "podman" => crate::tool_cmd(PODMAN_BIN_ENV, "podman"),
        _ => unreachable!(),
    }
}

/// Detect the package manager install command based on the base image name.
fn detect_pkg_install_cmd(base_image: &str) -> &'static str {
    let lower = base_image.to_ascii_lowercase();
    if lower.starts_with("alpine") || lower.contains("/alpine") {
        "apk add --no-cache"
    } else if lower.starts_with("fedora")
        || lower.contains("/fedora")
        || lower.starts_with("rockylinux")
        || lower.contains("/rockylinux")
        || lower.starts_with("almalinux")
        || lower.contains("/almalinux")
    {
        "dnf install -y"
    } else if lower.starts_with("centos") || lower.contains("/centos") {
        "yum install -y"
    } else if lower.starts_with("archlinux") || lower.contains("/archlinux") {
        "pacman -Sy --noconfirm"
    } else {
        // Debian, Ubuntu, and default
        "apt-get update && apt-get install -y"
    }
}

/// Generate a Dockerfile for building a module in an isolated container.
fn build_dockerfile(base_image: &str, packages: &[&str]) -> String {
    let mut lines = vec![format!("FROM {base_image}")];
    if !packages.is_empty() {
        let pkg_list = packages.join(" ");
        let install_cmd = detect_pkg_install_cmd(base_image);
        if install_cmd.starts_with("apt-get") {
            lines.push(format!(
                "RUN {install_cmd} {pkg_list} && rm -rf /var/lib/apt/lists/*"
            ));
        } else {
            lines.push(format!("RUN {install_cmd} {pkg_list}"));
        }
    }
    lines.push("WORKDIR /build".to_string());
    lines.push("COPY . /build/".to_string());
    lines.join("\n")
}

/// Build a module directory into an OCI-ready artifact using a container.
///
/// Creates a Dockerfile, builds a container image, copies out the installed
/// files, and packages them as a tar.gz layer ready for `push_module()`.
///
/// Returns the path to the build output directory.
pub fn build_module(
    dir: &Path,
    target_platform: Option<&str>,
    base_image: Option<&str>,
) -> Result<std::path::PathBuf, OciError> {
    let module_yaml_path = dir.join("module.yaml");
    if !module_yaml_path.exists() {
        return Err(OciError::ModuleYamlNotFound {
            dir: dir.to_path_buf(),
        });
    }

    let runtime = detect_container_runtime().ok_or(OciError::ToolNotFound {
        tool: "docker or podman".to_string(),
    })?;

    let module_yaml = std::fs::read_to_string(&module_yaml_path)?;
    let module_doc =
        crate::config::parse_module(&module_yaml).map_err(|e| OciError::BuildError {
            message: format!("invalid module.yaml: {e}"),
        })?;

    // Extract package names from module spec
    let pkg_names: Vec<String> = module_doc
        .spec
        .packages
        .iter()
        .map(|p| p.name.clone())
        .collect();
    let packages: Vec<&str> = pkg_names.iter().map(|s| s.as_str()).collect();

    let base = base_image.unwrap_or("ubuntu:22.04");
    let dockerfile_content = build_dockerfile(base, &packages);

    // Copy module directory into temp build context, then write Dockerfile
    // (write after copy so a user's Dockerfile doesn't overwrite the generated one)
    let build_dir = tempfile::tempdir().map_err(|e| OciError::BuildError {
        message: format!("cannot create temp dir: {e}"),
    })?;
    crate::copy_dir_recursive(dir, build_dir.path())?;
    crate::atomic_write_str(&build_dir.path().join("Dockerfile"), &dockerfile_content)?;

    // Build the container image
    let tag = format!(
        "cfgd-build-{}:{}",
        module_doc.metadata.name,
        std::process::id(),
    );
    let container_name = format!(
        "cfgd-build-{}-{}",
        std::process::id(),
        crate::utc_now_filename_safe(),
    );

    let mut build_cmd = runtime_cmd(runtime);
    build_cmd.arg("build").arg("-t").arg(&tag);

    if let Some(platform) = target_platform {
        build_cmd.arg("--platform").arg(platform);
    }

    build_cmd
        .arg("-f")
        .arg(build_dir.path().join("Dockerfile"))
        .arg(build_dir.path());

    let build_output = build_cmd.output().map_err(|e| OciError::BuildError {
        message: format!("{runtime} build failed: {e}"),
    })?;

    if !build_output.status.success() {
        return Err(OciError::BuildError {
            message: format!(
                "{runtime} build failed:\n{}",
                crate::stderr_lossy_trimmed(&build_output)
            ),
        });
    }

    // Create container and copy build output
    let output_dir = tempfile::tempdir().map_err(|e| OciError::BuildError {
        message: format!("cannot create output dir: {e}"),
    })?;

    let create_output = runtime_cmd(runtime)
        .args(["create", "--name", &container_name, &tag])
        .output()
        .map_err(|e| OciError::BuildError {
            message: format!("container create failed: {e}"),
        })?;

    if !create_output.status.success() {
        return Err(OciError::BuildError {
            message: format!(
                "container create failed: {}",
                crate::stderr_lossy_trimmed(&create_output)
            ),
        });
    }

    // Copy /build directory out of the container
    let cp_output = runtime_cmd(runtime)
        .args([
            "cp",
            &format!("{container_name}:/build/."),
            &output_dir.path().display().to_string(),
        ])
        .output()
        .map_err(|e| OciError::BuildError {
            message: format!("container cp failed: {e}"),
        })?;

    // Cleanup container and image (best effort)
    let _ = runtime_cmd(runtime)
        .args(["rm", "-f", &container_name])
        .output();
    let _ = runtime_cmd(runtime).args(["rmi", "-f", &tag]).output();

    if !cp_output.status.success() {
        return Err(OciError::BuildError {
            message: format!(
                "container cp failed: {}",
                crate::stderr_lossy_trimmed(&cp_output)
            ),
        });
    }

    let out = output_dir.path().to_path_buf();
    let _keep = output_dir.keep();
    tracing::info!(output = %out.display(), "module built");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Build ---

    #[test]
    fn build_module_rejects_missing_module_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let result = build_module(dir.path(), None, None);
        assert!(matches!(result, Err(OciError::ModuleYamlNotFound { .. })));
    }

    #[test]
    fn detect_container_runtime_returns_option() {
        let rt = detect_container_runtime();
        if let Some(name) = rt {
            assert!(name == "docker" || name == "podman");
        }
    }

    #[test]
    fn generate_build_dockerfile_content() {
        let dockerfile = build_dockerfile("ubuntu:22.04", &["curl", "wget"]);
        assert!(dockerfile.contains("FROM ubuntu:22.04"));
        assert!(dockerfile.contains("curl"));
        assert!(dockerfile.contains("wget"));
        assert!(dockerfile.contains("WORKDIR /build"));
    }

    #[test]
    fn generate_build_dockerfile_no_packages() {
        let dockerfile = build_dockerfile("alpine:3.18", &[]);
        assert!(dockerfile.contains("FROM alpine:3.18"));
        assert!(!dockerfile.contains("apt-get"));
    }

    // --- Dockerfile generation ---

    #[test]
    fn build_dockerfile_debian_default() {
        let df = build_dockerfile("ubuntu:22.04", &["curl", "jq"]);
        assert!(df.contains("FROM ubuntu:22.04"));
        assert!(df.contains("apt-get"));
        assert!(df.contains("curl jq"));
        assert!(df.contains("rm -rf /var/lib/apt/lists"));
    }

    #[test]
    fn build_dockerfile_alpine() {
        let df = build_dockerfile("alpine:3.18", &["curl"]);
        assert!(df.contains("apk add --no-cache"));
        assert!(!df.contains("apt-get"));
    }

    #[test]
    fn build_dockerfile_fedora() {
        let df = build_dockerfile("fedora:39", &["strace"]);
        assert!(df.contains("dnf install -y"));
    }

    #[test]
    fn build_dockerfile_no_packages() {
        let df = build_dockerfile("ubuntu:22.04", &[]);
        assert!(!df.contains("RUN"));
        assert!(df.contains("WORKDIR /build"));
    }

    // --- detect_pkg_install_cmd ---

    #[test]
    fn detect_pkg_install_cmd_centos() {
        assert_eq!(detect_pkg_install_cmd("centos:8"), "yum install -y");
    }

    #[test]
    fn detect_pkg_install_cmd_archlinux() {
        assert_eq!(
            detect_pkg_install_cmd("archlinux:latest"),
            "pacman -Sy --noconfirm"
        );
    }

    #[test]
    fn detect_pkg_install_cmd_unknown_defaults_to_apt() {
        let cmd = detect_pkg_install_cmd("someunknownimage:latest");
        assert!(
            cmd.contains("apt-get"),
            "unknown image should default to apt-get, got: {cmd}"
        );
    }

    #[test]
    fn detect_pkg_install_cmd_rockylinux() {
        assert_eq!(detect_pkg_install_cmd("rockylinux:9"), "dnf install -y");
    }

    #[test]
    fn detect_pkg_install_cmd_almalinux() {
        assert_eq!(detect_pkg_install_cmd("almalinux:8"), "dnf install -y");
    }

    #[test]
    fn detect_pkg_install_cmd_fedora() {
        assert_eq!(detect_pkg_install_cmd("fedora:40"), "dnf install -y");
    }

    #[test]
    fn detect_pkg_install_cmd_alpine_with_registry() {
        assert_eq!(
            detect_pkg_install_cmd("docker.io/library/alpine:3.19"),
            "apk add --no-cache"
        );
    }

    // --- detect_pkg_install_cmd: container registry-prefixed images ---

    #[test]
    fn detect_pkg_install_cmd_registry_prefixed_fedora() {
        assert_eq!(
            detect_pkg_install_cmd("registry.example.com/fedora:39"),
            "dnf install -y"
        );
    }

    #[test]
    fn detect_pkg_install_cmd_registry_prefixed_centos() {
        assert_eq!(
            detect_pkg_install_cmd("quay.io/centos/centos:stream8"),
            "yum install -y"
        );
    }

    #[test]
    fn detect_pkg_install_cmd_registry_prefixed_archlinux() {
        assert_eq!(
            detect_pkg_install_cmd("docker.io/library/archlinux:base"),
            "pacman -Sy --noconfirm"
        );
    }

    // --- build_dockerfile additional coverage ---

    #[test]
    fn build_dockerfile_centos() {
        let df = build_dockerfile("centos:8", &["vim", "git"]);
        assert!(df.contains("FROM centos:8"));
        assert!(df.contains("yum install -y"));
        assert!(df.contains("vim git"));
        assert!(!df.contains("apt-get"));
        assert!(!df.contains("rm -rf /var/lib/apt/lists"));
    }

    #[test]
    fn build_dockerfile_archlinux() {
        let df = build_dockerfile("archlinux:latest", &["base-devel"]);
        assert!(df.contains("FROM archlinux:latest"));
        assert!(df.contains("pacman -Sy --noconfirm"));
        assert!(df.contains("base-devel"));
    }

    #[test]
    fn build_dockerfile_rockylinux() {
        let df = build_dockerfile("rockylinux:9", &["httpd"]);
        assert!(df.contains("FROM rockylinux:9"));
        assert!(df.contains("dnf install -y"));
        assert!(df.contains("httpd"));
    }

    #[test]
    fn build_dockerfile_almalinux() {
        let df = build_dockerfile("almalinux:8", &["nginx"]);
        assert!(df.contains("FROM almalinux:8"));
        assert!(df.contains("dnf install -y"));
    }

    #[test]
    fn build_dockerfile_debian_cleans_apt_lists() {
        let df = build_dockerfile("debian:bookworm", &["curl"]);
        assert!(df.contains("FROM debian:bookworm"));
        assert!(df.contains("apt-get update && apt-get install -y"));
        assert!(
            df.contains("rm -rf /var/lib/apt/lists"),
            "debian-based images should clean apt lists"
        );
    }

    #[test]
    fn build_dockerfile_always_includes_workdir_and_copy() {
        // Even with no packages, WORKDIR and COPY lines should be present
        let df = build_dockerfile("scratch", &[]);
        assert!(df.contains("WORKDIR /build"));
        assert!(df.contains("COPY . /build/"));
        assert!(!df.contains("RUN"));
    }

    #[test]
    fn build_dockerfile_registry_prefixed_alpine() {
        // Images with registry prefix like "docker.io/library/alpine" should still be detected
        let df = build_dockerfile("docker.io/library/alpine:3.19", &["curl"]);
        assert!(df.contains("apk add --no-cache"));
        assert!(!df.contains("apt-get"));
    }

    #[test]
    fn build_dockerfile_multiple_packages() {
        let df = build_dockerfile("ubuntu:22.04", &["curl", "jq", "vim", "git"]);
        assert!(df.contains("curl jq vim git"));
    }

    // --- build_dockerfile: comprehensive generation ---

    #[test]
    fn build_dockerfile_ubuntu_with_packages_cleans_apt_lists() {
        let df = build_dockerfile("ubuntu:24.04", &["git", "curl", "make"]);
        assert_eq!(df.lines().count(), 4, "FROM + RUN + WORKDIR + COPY");
        assert!(df.starts_with("FROM ubuntu:24.04"));
        assert!(df.contains("apt-get update && apt-get install -y git curl make"));
        assert!(df.contains("rm -rf /var/lib/apt/lists/*"));
        assert!(df.contains("WORKDIR /build"));
        assert!(df.contains("COPY . /build/"));
    }

    #[test]
    fn build_dockerfile_debian_uses_apt() {
        let df = build_dockerfile("debian:bookworm", &["vim"]);
        assert!(df.contains("apt-get"), "debian should use apt-get");
        assert!(df.contains("rm -rf /var/lib/apt/lists/*"));
    }

    #[test]
    fn build_dockerfile_alpine_uses_apk() {
        let df = build_dockerfile("alpine:3.20", &["bash", "coreutils"]);
        assert!(df.contains("apk add --no-cache bash coreutils"));
        assert!(!df.contains("apt-get"));
        assert!(!df.contains("rm -rf"));
    }

    #[test]
    fn build_dockerfile_fedora_uses_dnf() {
        let df = build_dockerfile("fedora:41", &["gcc", "gdb"]);
        assert!(df.contains("dnf install -y gcc gdb"));
    }

    #[test]
    fn build_dockerfile_rockylinux_uses_dnf() {
        let df = build_dockerfile("rockylinux:9.3", &["python3"]);
        assert!(df.contains("dnf install -y python3"));
    }

    #[test]
    fn build_dockerfile_almalinux_uses_dnf() {
        let df = build_dockerfile("almalinux:9", &["wget"]);
        assert!(df.contains("dnf install -y wget"));
    }

    #[test]
    fn build_dockerfile_centos_uses_yum() {
        let df = build_dockerfile("centos:7", &["nmap"]);
        assert!(df.contains("yum install -y nmap"));
    }

    #[test]
    fn build_dockerfile_archlinux_uses_pacman() {
        let df = build_dockerfile("archlinux:base", &["neovim"]);
        assert!(df.contains("pacman -Sy --noconfirm neovim"));
    }

    #[test]
    fn build_dockerfile_registry_prefixed_alpine_custom() {
        let df = build_dockerfile("ghcr.io/custom/alpine:edge", &["jq"]);
        assert!(df.contains("apk add --no-cache jq"));
        assert!(df.starts_with("FROM ghcr.io/custom/alpine:edge"));
    }

    #[test]
    fn build_dockerfile_registry_prefixed_fedora() {
        let df = build_dockerfile("quay.io/fedora/fedora:40", &["strace"]);
        assert!(df.contains("dnf install -y strace"));
    }

    #[test]
    fn build_dockerfile_empty_packages_has_no_run() {
        let df = build_dockerfile("scratch", &[]);
        assert_eq!(df.lines().count(), 3, "FROM + WORKDIR + COPY, no RUN");
        assert!(!df.contains("RUN"));
    }

    #[test]
    fn build_dockerfile_single_package() {
        let df = build_dockerfile("ubuntu:22.04", &["curl"]);
        assert!(df.contains("curl"));
        // Should only have one RUN line
        let run_count = df.lines().filter(|l| l.starts_with("RUN")).count();
        assert_eq!(run_count, 1);
    }

    // --- detect_pkg_install_cmd: all mappings ---

    #[test]
    fn detect_pkg_install_cmd_alpine() {
        assert_eq!(detect_pkg_install_cmd("alpine:3.19"), "apk add --no-cache");
    }

    #[test]
    fn detect_pkg_install_cmd_ubuntu() {
        let cmd = detect_pkg_install_cmd("ubuntu:22.04");
        assert!(
            cmd.starts_with("apt-get"),
            "ubuntu should use apt-get: {cmd}"
        );
    }

    #[test]
    fn detect_pkg_install_cmd_debian() {
        let cmd = detect_pkg_install_cmd("debian:12");
        assert!(
            cmd.starts_with("apt-get"),
            "debian should use apt-get: {cmd}"
        );
    }

    #[test]
    fn detect_pkg_install_cmd_case_insensitive() {
        // The function lowercases the image name
        assert_eq!(detect_pkg_install_cmd("ALPINE:3.19"), "apk add --no-cache");
        assert_eq!(detect_pkg_install_cmd("Fedora:39"), "dnf install -y");
    }

    // --- ToolShim-based tests for env-var seams ---

    #[cfg(unix)]
    mod shim_tests {
        use serial_test::serial;

        use super::*;
        use crate::test_helpers::ToolShim;

        fn sample_module_yaml() -> &'static str {
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n"
        }

        #[test]
        #[serial]
        fn build_module_passes_platform_flag() {
            let _shim = ToolShim::install("CFGD_DOCKER_BIN", 0, "", "");
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("module.yaml"), sample_module_yaml()).unwrap();

            let _ = build_module(dir.path(), Some("linux/arm64"), None);
            let log = _shim.argv_log();
            assert!(log.contains("--platform"), "must pass --platform flag");
            assert!(log.contains("linux/arm64"));
        }

        #[test]
        #[serial]
        fn build_module_failure_propagates_error() {
            let _shim = ToolShim::install("CFGD_DOCKER_BIN", 1, "", "build error: out of disk");
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("module.yaml"), sample_module_yaml()).unwrap();

            let result = build_module(dir.path(), None, None);
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("build failed"),
                "error should mention build failure: {err}"
            );
        }

        #[test]
        #[serial]
        fn build_module_passes_tag_flag() {
            let _shim = ToolShim::install("CFGD_DOCKER_BIN", 0, "", "");
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("module.yaml"), sample_module_yaml()).unwrap();

            let _ = build_module(dir.path(), None, None);
            let log = _shim.argv_log();
            assert!(log.contains("-t"), "must pass -t flag for image tag");
            assert!(
                log.contains("cfgd-build-test-mod:"),
                "tag should include module name"
            );
        }

        #[test]
        #[serial]
        fn detect_runtime_podman_fallback() {
            // Docker shim missing (env var set to non-existent path), podman present
            let _docker_guard = crate::test_helpers::EnvVarGuard::set(
                "CFGD_DOCKER_BIN",
                "/nonexistent/docker-fake",
            );
            let _podman_shim = ToolShim::install("CFGD_PODMAN_BIN", 0, "", "");

            let rt = detect_container_runtime();
            assert_eq!(rt, Some("podman"));
        }

        #[test]
        #[serial]
        fn detect_runtime_docker_preferred() {
            let _docker_shim = ToolShim::install("CFGD_DOCKER_BIN", 0, "", "");
            let _podman_shim = ToolShim::install("CFGD_PODMAN_BIN", 0, "", "");

            let rt = detect_container_runtime();
            assert_eq!(rt, Some("docker"));
        }

        #[test]
        #[serial]
        fn detect_runtime_none_available() {
            let _docker_guard = crate::test_helpers::EnvVarGuard::set(
                "CFGD_DOCKER_BIN",
                "/nonexistent/docker-fake",
            );
            let _podman_guard = crate::test_helpers::EnvVarGuard::set(
                "CFGD_PODMAN_BIN",
                "/nonexistent/podman-fake",
            );

            let rt = detect_container_runtime();
            assert_eq!(rt, None);
        }
    }
}
