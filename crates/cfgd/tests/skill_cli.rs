#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

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

/// Spawn `cfgd skill install <args>` with a hermetic HOME (off the real $HOME)
/// and CWD pinned to the given repo dir, returning the assertion handle.
fn install_in(
    repo: &std::path::Path,
    home: &std::path::Path,
    args: &[&str],
) -> assert_cmd::Command {
    let mut cmd = Command::cargo_bin("cfgd").unwrap();
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .current_dir(repo)
        .args(["skill", "install"])
        .args(args);
    cmd
}

/// Find one `results[]` entry by provider id in the structured install payload.
fn result_for<'a>(payload: &'a Value, provider: &str) -> &'a Value {
    payload["results"]
        .as_array()
        .expect("results is an array")
        .iter()
        .find(|r| r["provider"] == provider)
        .unwrap_or_else(|| panic!("no result row for provider {provider} in {payload}"))
}

/// Project-scope auto-detect installs only into the detected providers and
/// reports the undetected ones as skipped (`not detected`), exiting 0.
#[test]
fn install_project_scope_writes_detected_providers_only() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    // claude-code present at project scope (.claude/), gemini absent (no .gemini/).
    std::fs::create_dir_all(repo.path().join(".claude")).expect("mk .claude");

    let assert = install_in(repo.path(), home.path(), &["module", "-o", "json"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");
    let payload: Value = serde_json::from_str(&out).expect("json payload");

    // `kind` matches the canonical PascalCase resource-kind token (`kind:` in
    // YAML, and the `skill list` payload), not the lowercase command token.
    assert_eq!(payload["kind"], "Module");
    assert_eq!(payload["scope"], "project");

    let claude = result_for(&payload, "claude-code");
    assert_eq!(claude["status"], "installed", "claude-code should install");
    assert!(
        claude["path"]
            .as_str()
            .expect("path string")
            .ends_with(".claude/skills/cfgd-module/SKILL.md"),
        "unexpected claude path: {claude}"
    );
    assert!(
        repo.path()
            .join(".claude/skills/cfgd-module/SKILL.md")
            .exists(),
        "SKILL.md must be on disk"
    );

    let gemini = result_for(&payload, "gemini");
    assert_eq!(gemini["status"], "skipped", "gemini undetected → skipped");
    assert!(
        gemini["reason"]
            .as_str()
            .expect("reason string")
            .contains("not detected"),
        "unexpected gemini reason: {gemini}"
    );
}

/// A partial failure (one targeted provider's install fails while another
/// succeeds) reports per-provider status AND exits non-zero. Failure is
/// injected root-safely by occupying the failing provider's parent-dir path
/// with a regular FILE so `create_dir_all` fails with `NotADirectory` even as
/// uid 0.
#[test]
fn install_reports_failure_and_exits_nonzero_on_partial() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    // Both providers detected at project scope.
    std::fs::create_dir_all(repo.path().join(".claude")).expect("mk .claude");
    std::fs::create_dir_all(repo.path().join(".gemini")).expect("mk .gemini");

    // Sabotage claude-code: its target is .claude/skills/cfgd-module/SKILL.md, so
    // make .claude/skills a regular FILE — create_dir_all(parent) then fails.
    let skills_path = repo.path().join(".claude/skills");
    std::fs::write(&skills_path, b"not a directory").expect("write blocker file");

    let assert = install_in(repo.path(), home.path(), &["module", "-o", "json"])
        .assert()
        .failure();
    let out = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");
    let payload: Value = serde_json::from_str(&out).expect("json payload");

    let claude = result_for(&payload, "claude-code");
    assert_eq!(claude["status"], "failed", "sabotaged claude must fail");
    assert!(
        !claude["reason"].as_str().expect("reason").is_empty(),
        "failed row must carry a reason: {claude}"
    );

    let gemini = result_for(&payload, "gemini");
    assert_eq!(gemini["status"], "installed", "gemini must still succeed");
}

/// At user scope (`-g`), cursor and copilot have no user-scope primitive: each is
/// reported `skipped` with the provider's "no user-scope primitive" reason (never
/// a fabricated path), exit 0 (a scope-skip is not a failure). The wire `status`
/// stays `"skipped"` — a structured consumer distinguishes the severity via the
/// `reason`, and the JSON must NOT carry the human-only `warn` field.
#[test]
fn global_scope_skips_cursor_and_copilot_with_warning() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    let home = tempfile::tempdir().expect("home tempdir");

    let assert = install_in(repo.path(), home.path(), &["module", "-g", "-o", "json"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");
    let payload: Value = serde_json::from_str(&out).expect("json payload");

    assert_eq!(payload["scope"], "user");

    for provider in ["cursor", "copilot"] {
        let row = result_for(&payload, provider);
        assert_eq!(
            row["status"], "skipped",
            "{provider} has no user-scope primitive → skipped: {row}"
        );
        assert!(
            row["reason"]
                .as_str()
                .expect("reason string")
                .contains("no user-scope primitive"),
            "unexpected {provider} reason: {row}"
        );
        assert!(
            row.get("warn").is_none(),
            "warn is human-only and must not reach the JSON wire: {row}"
        );
        assert!(
            row.get("path").is_none(),
            "an unsupported-scope skip must never fabricate a path: {row}"
        );
    }

    // The human render carries the reason text as a visible warning detail. The
    // human Doc renders on the Printer's stderr channel (structured payloads use
    // stdout); assert the reason text appears, not ANSI/role styling bytes.
    let human = install_in(repo.path(), home.path(), &["module", "-g"])
        .assert()
        .success();
    let human_err = String::from_utf8(human.get_output().stderr.clone()).expect("utf8 stderr");
    assert!(
        human_err.contains("no user-scope primitive"),
        "human output must surface the unsupported-scope reason: {human_err}"
    );
}

/// Spawn `cfgd skill <subcommand...>` with a hermetic HOME and pinned CWD.
fn skill_in(repo: &std::path::Path, home: &std::path::Path, args: &[&str]) -> assert_cmd::Command {
    let mut cmd = Command::cargo_bin("cfgd").unwrap();
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .current_dir(repo)
        .args(["skill"])
        .args(args);
    cmd
}

/// `skill list` reports installed skills and flags one whose stamped
/// `cfgd-version` predates the running version as `stale`, while a freshly
/// installed skill (current stamp) reports `stale == false`.
#[test]
fn list_shows_installed_with_stale_flag() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".claude")).expect("mk .claude");

    // Fresh install of `profile` → current version stamp → stale == false.
    skill_in(
        repo.path(),
        home.path(),
        &["install", "profile", "--provider", "claude-code"],
    )
    .assert()
    .success();

    // Hand-write a `module` SKILL.md with a mismatched stamp → stale == true.
    // 9.9.x sentinel: any mismatch with the running version reads as stale.
    let module_path = repo.path().join(".claude/skills/cfgd-module/SKILL.md");
    std::fs::create_dir_all(module_path.parent().unwrap()).expect("mk skill dir");
    std::fs::write(
        &module_path,
        "---\nname: cfgd-module\ndescription: x\nuser-invocable: true\ncfgd-version: 9.9.0\ncfgd-min-version: 9.9.0\n---\n\nbody\n",
    )
    .expect("write stale skill");

    let assert = skill_in(repo.path(), home.path(), &["list", "-o", "json"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");
    let payload: Value = serde_json::from_str(&out).expect("json payload");

    assert_eq!(payload["scope"], "project");

    let entries = payload["installed"].as_array().expect("installed array");
    let module = entries
        .iter()
        .find(|e| e["kind"] == "Module" && e["provider"] == "claude-code")
        .unwrap_or_else(|| panic!("no module entry in {payload}"));
    assert_eq!(module["stale"], true, "9.9.0 stamp must be stale: {module}");

    let profile = entries
        .iter()
        .find(|e| e["kind"] == "Profile" && e["provider"] == "claude-code")
        .unwrap_or_else(|| panic!("no profile entry in {payload}"));
    assert_eq!(
        profile["stale"], false,
        "freshly installed skill must not be stale: {profile}"
    );
}

/// `skill remove --provider codex` excises only the cfgd-managed block from a
/// user-co-owned `AGENTS.md`, preserving the surrounding user prose verbatim.
#[test]
fn remove_excises_only_managed_block_in_agents_md() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    let home = tempfile::tempdir().expect("home tempdir");

    // Codex detects via a project-root AGENTS.md; seed it with user prose so the
    // managed block splices BETWEEN the leading and trailing bytes.
    let agents = repo.path().join("AGENTS.md");
    let lead = "# My AGENTS.md\n\nLeading user prose.\n";
    std::fs::write(&agents, lead).expect("seed AGENTS.md");

    skill_in(
        repo.path(),
        home.path(),
        &["install", "module", "--provider", "codex"],
    )
    .assert()
    .success();

    let after_install = std::fs::read_to_string(&agents).expect("read AGENTS.md");
    assert!(
        after_install.contains("cfgd:skill:module"),
        "install must splice the managed block: {after_install}"
    );
    assert!(
        after_install.contains("Leading user prose."),
        "user prose must survive install: {after_install}"
    );

    skill_in(
        repo.path(),
        home.path(),
        &["remove", "module", "--provider", "codex", "--yes"],
    )
    .assert()
    .success();

    let after_remove = std::fs::read_to_string(&agents).expect("read AGENTS.md");
    assert!(
        !after_remove.contains("cfgd:skill:module"),
        "remove must excise the managed block: {after_remove}"
    );
    assert!(
        after_remove.contains("# My AGENTS.md") && after_remove.contains("Leading user prose."),
        "surrounding user bytes must survive verbatim: {after_remove}"
    );
}

/// `skill update --all` re-renders every installed skill at scope, bumping each
/// stale on-disk stamp to the running version and reporting `updated` per row.
#[test]
fn update_all_rerenders_every_installed_skill_at_scope() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".claude")).expect("mk .claude");

    // Seed two installed claude-code skills with a mismatched stamp by hand, so a
    // real re-render (current stamp) is observable.
    // 9.9.x sentinel: any mismatch with the running version reads as stale.
    for token in ["module", "profile"] {
        let p = repo
            .path()
            .join(format!(".claude/skills/cfgd-{token}/SKILL.md"));
        std::fs::create_dir_all(p.parent().unwrap()).expect("mk skill dir");
        std::fs::write(
            &p,
            format!("---\nname: cfgd-{token}\ndescription: x\nuser-invocable: true\ncfgd-version: 9.9.0\ncfgd-min-version: 9.9.0\n---\n\nbody\n"),
        )
        .expect("write stale skill");
    }

    let running = env!("CARGO_PKG_VERSION");

    let assert = skill_in(repo.path(), home.path(), &["update", "--all", "-o", "json"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");
    let payload: Value = serde_json::from_str(&out).expect("json payload");

    for token in ["module", "profile"] {
        let p = repo
            .path()
            .join(format!(".claude/skills/cfgd-{token}/SKILL.md"));
        let content = std::fs::read_to_string(&p).expect("read updated skill");
        assert!(
            content.contains(&format!("cfgd-version: {running}")),
            "{token} must be re-rendered to running version: {content}"
        );
        assert!(
            !content.contains("cfgd-version: 9.9.0"),
            "{token} stale stamp must be gone: {content}"
        );
    }

    // Both kinds report `updated` in the per-target `results[]` (the wire shape
    // shared with install/remove).
    let updated_count = payload["results"]
        .as_array()
        .expect("results is an array")
        .iter()
        .filter(|r| r["status"] == "updated")
        .count();
    assert!(
        updated_count >= 2,
        "both seeded skills must report updated: {payload}"
    );
}

/// An unknown `--provider` id is a hard user error (never silently ignored): the
/// command exits non-zero and the structured error payload names the bogus id AND
/// lists the valid provider ids (so the user can self-correct). Under `-o json`
/// the CLI-boundary error renders as the canonical `{error, message, name}`
/// payload on stdout.
#[test]
fn install_unknown_provider_is_hard_error() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    let home = tempfile::tempdir().expect("home tempdir");

    let assert = install_in(
        repo.path(),
        home.path(),
        &["module", "--provider", "bogus", "-o", "json"],
    )
    .assert()
    .failure();
    let out = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");
    let payload: Value = serde_json::from_str(&out).expect("json error payload");
    let message = payload["message"].as_str().expect("error message string");
    assert!(
        message.contains("bogus"),
        "error must name the unknown provider: {payload}"
    );
    assert!(
        message.contains("claude-code"),
        "error must list the valid provider ids: {payload}"
    );
}

/// `--force` into an explicitly-named, UNDETECTED provider still installs: in a
/// repo with no provider dirs, `--provider gemini --force` writes the gemini
/// command file (force overrides the absent-detection skip). The result row
/// reports `installed` and the `.gemini/commands/cfgd-module.toml` file lands on
/// disk.
#[test]
fn install_force_into_undetected_provider_installs() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    // No provider dirs seeded → gemini is undetected at project scope.

    let assert = install_in(
        repo.path(),
        home.path(),
        &["module", "--provider", "gemini", "--force", "-o", "json"],
    )
    .assert()
    .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");
    let payload: Value = serde_json::from_str(&out).expect("json payload");

    let gemini = result_for(&payload, "gemini");
    assert_eq!(
        gemini["status"], "installed",
        "force must install into an undetected provider: {gemini}"
    );
    assert!(
        repo.path()
            .join(".gemini/commands/cfgd-module.toml")
            .exists(),
        "forced gemini command file must be on disk"
    );
}

/// `update <kind>` when nothing is installed is a coherent no-op: every targeted
/// provider reports `skipped` with reason "not installed", and the command exits
/// 0 (an empty update is not a failure).
#[test]
fn update_kind_not_installed_skips_exit_zero() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    // Nothing installed anywhere.

    let assert = skill_in(
        repo.path(),
        home.path(),
        &["update", "module", "-o", "json"],
    )
    .assert()
    .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");
    let payload: Value = serde_json::from_str(&out).expect("json payload");

    let results = payload["results"].as_array().expect("results is an array");
    assert!(!results.is_empty(), "every provider gets a row: {payload}");
    for row in results {
        assert_eq!(
            row["status"], "skipped",
            "nothing installed → every row skipped: {row}"
        );
        assert!(
            row["reason"]
                .as_str()
                .expect("reason string")
                .contains("not installed"),
            "skip reason must be 'not installed': {row}"
        );
    }
}

/// User-scope install (`-g`) writes under $HOME: with a hermetic home containing
/// `~/.claude/` (which is claude-code's user-scope detection trigger), the
/// claude-code skill installs at user scope and its `SKILL.md` lands under the
/// home tempdir — proving the positive `-g` path, not just the cursor/copilot
/// no-user-scope skip.
#[test]
fn install_global_scope_writes_under_home() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    // claude-code detects user scope via ~/.claude existing.
    std::fs::create_dir_all(home.path().join(".claude")).expect("mk ~/.claude");

    let assert = install_in(repo.path(), home.path(), &["module", "-g", "-o", "json"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8 stdout");
    let payload: Value = serde_json::from_str(&out).expect("json payload");

    assert_eq!(payload["scope"], "user");

    let claude = result_for(&payload, "claude-code");
    assert_eq!(
        claude["status"], "installed",
        "claude-code must install at user scope: {claude}"
    );
    let written = claude["path"].as_str().expect("path string");
    assert!(
        written.ends_with(".claude/skills/cfgd-module/SKILL.md"),
        "unexpected user-scope path: {claude}"
    );
    assert!(
        std::path::Path::new(written).starts_with(home.path()),
        "user-scope install must land under $HOME: {written}"
    );
    assert!(
        home.path()
            .join(".claude/skills/cfgd-module/SKILL.md")
            .exists(),
        "user-scope SKILL.md must be on disk under home"
    );
}
