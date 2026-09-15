//! A folded `PATH` renders the order a shell sourcing the same declarations
//! would produce.
//!
//! `fold_env_layer` stands in for chained `export PATH=…` lines: a layer that
//! prepends puts its own directories in FRONT of the ones an earlier layer
//! prepended, while a layer that appends leaves the earlier layer's appended
//! directories in front. The rendered value is compared against a real shell
//! sourcing the two declarations, so the claim is settled by the thing it
//! stands in for rather than by a second copy of the fold's own reasoning.

#![cfg(unix)]

use std::process::Command;

use cfgd_core::config::EnvVar;
use cfgd_core::fold_env_layer;

const PIN_HOME: &str = "/home/pin";
const PIN_PATH: &str = "/usr/bin:/bin";

fn ev(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: name.into(),
        value: value.into(),
        platforms: vec![],
    }
}

/// `PATH` after a shell sourced a file holding `declarations`, under a pinned
/// `HOME` and starting `PATH` so the answer is the same on every host.
fn sourced_path(dir: &std::path::Path, declarations: &[&str]) -> String {
    let script = dir.join("layers.sh");
    let body: String = declarations
        .iter()
        .map(|d| format!("export PATH=\"{d}\"\n"))
        .collect();
    std::fs::write(&script, body).expect("write the declarations");
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            ". {}\nprintf '%s' \"$PATH\"",
            script.to_string_lossy()
        ))
        .env_clear()
        .env("HOME", PIN_HOME)
        .env("PATH", PIN_PATH)
        .output()
        .expect("a POSIX shell runs");
    assert!(
        output.status.success(),
        "sourcing the declarations failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("a PATH is utf8")
}

#[test]
fn a_folded_path_orders_its_entries_the_way_a_sourced_shell_does() {
    let base_decl = "$HOME/.cargo/bin:$PATH:$HOME/.cargo/late";
    let overlay_decl = "$HOME/.local/bin:$PATH:$HOME/.local/late";

    let mut base = vec![ev("PATH", base_decl)];
    fold_env_layer(&mut base, &[ev("PATH", overlay_decl)], ':');
    let folded = base
        .iter()
        .find(|e| e.name == "PATH")
        .expect("the fold leaves a PATH")
        .value
        .clone();

    assert_eq!(
        folded, "$HOME/.local/bin:$HOME/.cargo/bin:$PATH:$HOME/.cargo/late:$HOME/.local/late",
        "the overlay's prepend leads and its append trails"
    );

    let dir = tempfile::tempdir().expect("a temp dir");
    let chained = sourced_path(dir.path(), &[base_decl, overlay_decl]);
    let rendered = sourced_path(dir.path(), &[&folded]);
    assert_eq!(
        rendered, chained,
        "the folded declaration and the two it stands in for produce one PATH"
    );
    assert_eq!(
        chained,
        format!(
            "{PIN_HOME}/.local/bin:{PIN_HOME}/.cargo/bin:{PIN_PATH}:\
             {PIN_HOME}/.cargo/late:{PIN_HOME}/.local/late"
        ),
        "the shell itself puts the later prepend in front and the later append last"
    );
}
