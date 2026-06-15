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

/// `cfgd skill update` with neither a kind nor `--all` is an incoherent request
/// (nothing to update), constrained at the clap layer via `required_unless_present`.
#[test]
fn skill_update_requires_kind_or_all() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["skill", "update"])
        .assert()
        .failure();
}

/// `cfgd skill update --all` is the coherent "update everything" request and parses.
#[test]
fn skill_update_all_parses() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["skill", "update", "--all"])
        .assert()
        .success();
}

/// `kind` and `--all` together is contradictory and rejected at the clap layer
/// via `conflicts_with`.
#[test]
fn skill_update_kind_and_all_conflict() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["skill", "update", "module", "--all"])
        .assert()
        .failure();
}

/// The `-y` short for `--yes` works on `skill rm`, matching every other
/// destructive cfgd verb (`cfgd module rm -y`, `cfgd source rm -y`).
#[test]
fn skill_remove_accepts_short_yes() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["skill", "rm", "module", "-y"])
        .assert()
        .success();
}
