#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x

use assert_cmd::Command;
use predicates::prelude::*;

/// `cfgd skill install --help` must list the author kinds (so the kind is
/// discoverable) and carry an `Examples:` block (the cfgd top-level-command
/// convention, regression-guarded by the ux-consistency audit).
#[test]
fn skill_help_lists_kinds_and_examples() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["skill", "install", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("module").and(predicate::str::contains("Examples:")));
}
