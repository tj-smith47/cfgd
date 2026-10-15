//! `cfgd apply` with no home directory and a `~/` system target.
//!
//! An unresolved `~` read as a relative path names a directory called `~`
//! under the working directory, and `sshKeys` creates its key's parent with
//! `mkdir -p` before `ssh-keygen` runs. The run refuses while it plans and
//! writes nothing.

mod cfgd_binary;
use cfgd_binary::cfgd_bin;

#[test]
fn a_homeless_apply_of_a_tilde_ssh_key_refuses_and_writes_nothing_under_the_cwd() {
    let config_dir = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(config_dir.path().join("profiles")).unwrap();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: keys\n",
    )
    .unwrap();
    std::fs::write(
        config_dir.path().join("profiles/keys.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: keys\nspec:\n  system:\n    sshKeys:\n      - name: default\n        type: ed25519\n        path: ~/.ssh/id_ed25519\n",
    )
    .unwrap();
    // The configurator is only planned when `ssh-keygen` resolves; a no-op
    // stand-in keeps the test independent of the host's OpenSSH install.
    cfgd_core::test_helpers::write_probe_tool(tools.path(), "ssh-keygen");
    let path = std::env::join_paths(std::iter::once(tools.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();

    let out = cfgd_bin()
        .unwrap()
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .env("PATH", path)
        .current_dir(cwd.path())
        .args(["apply", "--yes", "--config"])
        .arg(config_dir.path().join("cfgd.yaml"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !out.status.success(),
        "a homeless apply of a `~/` key must refuse:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("cannot expand ~/.ssh/id_ed25519 for the SSH key path"),
        "the refusal names the path and its role:\nstdout: {stdout}\nstderr: {stderr}"
    );
    let written: Vec<_> = std::fs::read_dir(cwd.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        written.is_empty(),
        "the refused run wrote under the working directory: {written:?}"
    );
}
