//! Regression anchors: regression anchors. One golden per killed output-vocabulary
//! violation. Each test's doc-comment shows the BEFORE call site so future
//! reviewers can see what was migrated.

use std::time::Duration;

use crate::golden_doc;
use crate::output::{Doc, OwnerLabel, Role};

// BEFORE: cli/rollback.rs:108  printer.info(&format!("  {}", action));
// The non-file actions a rollback cannot undo, listed under their own heading.
// `Phase: Files` is the rollback's FILE work (status lines, no bullets) and is
// anchored by the `rollback/*` command goldens; this list is the surface the
// migrated call site produces, so the anchor keeps the heading it renders under.
golden_doc!(regression, rollback_action, |p, cap| {
    let s = p.section("Actions");
    s.bullet("revert /etc/hosts");
});

// BEFORE: cli/sync.rs:60  printer.warning(&format!("  - {}", change.description));
golden_doc!(regression, sync_change, |p, cap| {
    let s = p.section_or_collapse("Changed");
    s.bullet("config drift detected in /etc/foo");
});

// BEFORE: cli/module/registry.rs:281  printer.info(&format!("  {}", change));
golden_doc!(regression, registry_change, |p, cap| {
    let s = p.section("Registry changes");
    s.bullet("module foo @ 1.2.3 → 1.2.4");
});

// BEFORE: cli/module/registry.rs:352  printer.info(&format!("  + {}{}", pkg.name, ver));
golden_doc!(regression, registry_added_pkg, |p, cap| {
    let s = p.section_or_collapse("Added packages");
    s.bullet("nodejs@20");
});

// BEFORE: cli/module/registry.rs:360  printer.info(&format!("  {} -> {}", file.source, file.target));
golden_doc!(regression, registry_file_map, |p, cap| {
    let s = p.section("Files");
    s.bullet("./foo.txt → /etc/foo.txt");
});

// BEFORE: cli/module/registry.rs:373  printer.warning(&format!("  $ {}", script));
golden_doc!(regression, registry_script, |p, cap| {
    let s = p.section_or_collapse("Scripts (will run)");
    s.bullet("./post-install.sh");
});

// BEFORE: cli/compliance.rs:230  printer.success(&format!("  + {}", check_key(check)));
golden_doc!(regression, compliance_added, |p, cap| {
    let s = p.section_or_collapse("Added (1 check)");
    s.bullet("hardening.firewall.enabled");
});

// BEFORE: cli/compliance.rs:238  printer.warning(&format!("  - {}", check_key(check)));
golden_doc!(regression, compliance_removed, |p, cap| {
    let s = p.section_or_collapse("Removed (1 check)");
    s.bullet("legacy.telnet.disabled");
});

// BEFORE: cli/compliance.rs:258  printer.info(&format!("    {}", detail));
//         (4-space indent — was nested inside a Status, not a Section)
golden_doc!(regression, compliance_changed_with_detail, |p, cap| {
    let s = p.section_or_collapse("Changed (1)");
    s.status(Role::Fail, "ssh.password-auth (Pass → Violation)")
        .detail("sshd_config sets PasswordAuthentication=yes");
});

// BEFORE: cli/init/cmd_init.rs:288-290  three printer.info("  cfgd ...") lines
golden_doc!(regression, init_next_steps, |p, cap| {
    let s = p.section("Next steps");
    s.bullet("cfgd module create <name>");
    s.bullet("cfgd profile create <name>");
    s.bullet("cfgd apply");
});

// BEFORE: cli/config_cmd.rs:28  printer.key_value("  Branch", &origin.branch);
//         (key indent broke the {:>16} alignment)
golden_doc!(regression, config_origin_branch, |p, cap| {
    let doc = Doc::new().heading("Configuration").section("Origins", |s| {
        s.subsection("Primary", |o| {
            o.kv("Url", "git@github.com:tj/dotfiles.git")
                .kv("Type", "Git")
                .kv("Branch", "main")
        })
    });
    p.emit(doc);
});

// BEFORE: cli/config_cmd.rs:68 + 79  printer.key_value("  Reconcile interval", ...)
golden_doc!(regression, config_reconcile_settings, |p, cap| {
    let doc = Doc::new().heading("Configuration").section("Daemon", |s| {
        s.subsection("Reconcile", |r| {
            r.kv("Interval", "5m")
                .kv("On change", "yes")
                .kv("Auto apply", "yes")
        })
        .subsection("Sync", |y| y.kv("Interval", "10m"))
    });
    p.emit(doc);
});

// BEFORE: cli/profile/update.rs:137  printer.info(&format!("  {}", f.file_path));
golden_doc!(regression, profile_update_file, |p, cap| {
    let s = p.section_or_collapse("Updated files");
    s.bullet("/home/tj/.zshrc");
});

// BEFORE: cli/profile/update.rs:181  printer.info(&format!("  Restored: {}", f.file_path));
golden_doc!(regression, profile_restored_file, |p, cap| {
    let s = p.section_or_collapse("Restored");
    s.status(Role::Ok, "/home/tj/.gitconfig");
});

// BEFORE: cli/profile/update.rs:191  printer.info(&format!("  Removed: {}", f.file_path));
golden_doc!(regression, profile_removed_file, |p, cap| {
    let s = p.section_or_collapse("Removed");
    s.status(Role::Skipped, "/home/tj/.old-rc");
});

// BEFORE: cli/doctor.rs:361  printer.info(&format!("  {} — skipped (platform)", entry.name));
golden_doc!(regression, doctor_platform_skipped, |p, cap| {
    let s = p.section("Doctor");
    s.status(Role::Skipped, "macos-defaults")
        .detail("not applicable on Linux");
});

// BEFORE: cli/doctor.rs:364  printer.error(&format!("  {} — {}", entry.name, e));
golden_doc!(regression, doctor_check_failed, |p, cap| {
    let s = p.section("Doctor");
    s.status(Role::Fail, "shell-init").detail("file missing");
});

// BEFORE: cli/source/update.rs:61  printer.warning(&format!("  - {}", change.description));
golden_doc!(regression, source_update_change, |p, cap| {
    let s = p.section_or_collapse("Source updates");
    s.bullet("dotfiles repo updated");
});

// BEFORE: cli/module/list_show.rs:297  printer.info(&format!("  {}", script));
golden_doc!(regression, module_show_script, |p, cap| {
    let s = p.section_or_collapse("Scripts");
    s.bullet("./install.sh");
});

// BEFORE: cli/module/export.rs:154  printer.info(&format!("  {}/install.sh", feature_dir.display()));
golden_doc!(regression, export_install_path, |p, cap| {
    let s = p.section("Exported");
    s.bullet("./build/feature/install.sh");
});

// BEFORE: cli/init/cmd_init.rs:396  printer.info(&format!("  {}. {}", i+1, name));
golden_doc!(regression, init_picker_options, |p, cap| {
    let s = p.section("Available Profiles");
    s.bullet("1. dev");
    s.bullet("2. prod");
});

// BEFORE: header overload — `printer.header("Plan")` then `header("Apply")`
golden_doc!(regression, plan_then_apply_no_double_heading, |p, cap| {
    p.heading("Apply");
    p.kv_block([("Profile", "dev")]);
    let s = p.section("Files");
    s.status(Role::Ok, "Wrote /etc/hosts")
        .duration(Duration::from_millis(50));
});

// BEFORE: children pop to col 0 in apply flow
golden_doc!(regression, apply_results_stay_indented, |p, cap| {
    p.heading("Apply");
    let s = p.section("Files");
    s.status(Role::Ok, "Wrote /etc/hosts");
    s.status(Role::Warn, "Skipped /tmp/foo")
        .detail("permission denied");
});

// BEFORE: glyph zoo — `~ tmux (Pass → Warning)` was warning-styled
golden_doc!(regression, compliance_changed_with_role_status, |p, cap| {
    let s = p.section_or_collapse("Changed (1)");
    s.status(Role::Warn, "tmux (Pass → Warning)");
});

// BEFORE: info dumping ground — "Aborted" message
golden_doc!(regression, apply_aborted_uses_status, |p, cap| {
    p.heading("Apply");
    p.status_simple(Role::Skipped, "Aborted by user");
});

// =====================================================================
// Worked-example fidelity tests. Each reproduces the claimed rendered
// output for a worked example. If the renderer drifts from the
// documented shape, these fail loudly — a regression in the headline
// before/after promise.
// =====================================================================

// BEFORE: `cfgd config show` worked example.
golden_doc!(regression, worked_example_config_show, |p, cap| {
    let doc = Doc::new()
        .heading("Configuration")
        .kv_block([("File", "/etc/cfgd.yaml"), ("Profile", "dev")])
        .section("Origins", |s| {
            s.subsection("Primary", |o| {
                o.kv("Url", "git@github.com:tj/dotfiles.git")
                    .kv("Type", "Git")
                    .kv("Branch", "main")
            })
            .subsection("Secondary", |o| {
                o.kv("Url", "git@github.com:tj/work.git")
                    .kv("Type", "Git")
                    .kv("Branch", "main")
            })
        });
    p.emit(doc);
});

// BEFORE: `cfgd status` worked example (without payload; payload is
// exercised by per-command snapshots).
golden_doc!(regression, worked_example_status, |p, cap| {
    let doc = Doc::new()
        .heading("Status")
        .section("Last Apply", |s| {
            s.kv_block([
                ("Time", "3m ago"),
                ("Profile", "dev"),
                ("Result", "12 succeeded, 0 failed"),
            ])
        })
        .section("Drift", |s| {
            s.status_with(Role::Warn, "shell-config", |sf| {
                sf.detail("file changed since apply").target("~/.zshrc")
            })
            .status_with(Role::Warn, "git-config", |sf| {
                sf.detail("file changed since apply").target("~/.gitconfig")
            })
        })
        .section("Modules", |s| {
            // Subject is the owner token — the same string the module's group
            // is headed with in an apply tree — and the detail is the producer's
            // own `{pkgs}, {files}, {state}` order (`cli/status.rs`), so the
            // worked example cannot model a line the command does not emit.
            s.status_with(Role::Ok, "module:base", |sf| {
                sf.detail("5 pkgs, 3 files, installed")
            })
            .status_with(Role::Ok, "module:dev-tools", |sf| {
                sf.detail("18 pkgs, 12 files, installed")
            })
            .status_with(Role::Warn, "module:shell-config", |sf| {
                sf.detail("0 pkgs, 4 files, error")
            })
        });
    p.emit(doc);
});

// BEFORE: `cfgd apply` worked example. Streaming-shaped, so this test
// exercises the streaming surface (Printer + SectionGuard) rather than
// Doc::emit.
golden_doc!(regression, worked_example_apply, |p, cap| {
    p.heading("Apply");
    p.kv_block([("Config", "/etc/cfgd.yaml"), ("Profile", "dev")]);
    {
        let sec = p.section_or_collapse("Files");
        sec.status(Role::Ok, "Wrote /etc/hosts")
            .duration(Duration::from_millis(0));
        sec.status(Role::Warn, "Skipped /tmp/foo")
            .detail("permission denied");
        sec.status(Role::Ok, "Symlinked ~/.zshrc")
            .duration(Duration::from_millis(0));
    }
    {
        let sec = p.section_or_collapse("Packages");
        sec.status(Role::Ok, "Installed nodejs@20")
            .duration(Duration::from_millis(4100));
        sec.status(Role::Ok, "Installed pnpm@8")
            .duration(Duration::from_millis(1200));
    }
    p.status(Role::Ok, "5 actions applied")
        .duration(Duration::from_millis(5400));
});

// BEFORE: `cfgd compliance diff` worked example (the role-abuse case).
// Full Doc render to verify the claimed output shape.
golden_doc!(regression, worked_example_compliance_diff, |p, cap| {
    let doc = Doc::new()
        .heading("Compliance Diff #4 → #5")
        .kv_block([
            ("Snapshot 1", "2026-05-13 10:14:02 UTC"),
            ("Snapshot 2", "2026-05-14 09:02:11 UTC"),
        ])
        .section_or_collapse("Added (2 checks)", |s| {
            s.bullet("hardening.firewall.enabled")
                .bullet("hardening.audit.enabled")
        })
        .section_or_collapse("Removed (1 check)", |s| s.bullet("legacy.telnet.disabled"))
        .section_or_collapse("Changed (1 check)", |s| {
            s.status_with(Role::Fail, "ssh.password-auth (Pass → Violation)", |sf| {
                sf.detail("sshd_config sets PasswordAuthentication=yes")
            })
        });
    p.emit(doc);
});

// BEFORE: `cfgd init` worked example (the orphan-indent fix). Streaming
// Section + bullets.
golden_doc!(regression, worked_example_init_next_steps, |p, cap| {
    let next = p.section("Next steps");
    next.bullet("cfgd module create <name>   — create a module");
    next.bullet("cfgd profile create <name>  — create a profile");
    next.bullet("cfgd apply                  — apply configuration");
});

// `cfgd module list` table — per-cell roles. The `Source` value "remote" and
// `Status` value "pending" pick up secondary / accent styling via
// `Table::row_styled`. Exercises the Doc emit path (not the streaming
// `Printer::table`) because `build_module_list_doc` returns a Doc; the
// streaming path doesn't validate that `row_roles` survive the
// `Component::Table` round-trip.
golden_doc!(regression, module_list_table_styled_cells, |p, cap| {
    use crate::output::renderer::Table;
    let t = Table::new(["Module", "Source", "Status"])
        .row_styled([
            ("git".to_string(), None),
            ("local".to_string(), None),
            ("installed".to_string(), None),
        ])
        .row_styled([
            ("neovim".to_string(), None),
            ("remote".to_string(), Some(Role::Secondary)),
            ("pending".to_string(), Some(Role::Accent)),
        ]);
    p.emit(Doc::new().heading("Modules").table(t));
});

// Surface: `cfgd sync`'s per-source owner group. Each source is a
// `source:<name>` group and every line inside it names its outcome only —
// the heading already says whose it is. Snapshot anchors the nesting and the
// per-group alignment, which is what makes two sources readable as two blocks
// rather than one stream.
golden_doc!(regression, sync_per_source_owner_group, |p, cap| {
    let s = p.section("Sources");
    {
        let dotfiles = s.section_owner(&OwnerLabel::new("source", "dotfiles"));
        dotfiles
            .status(Role::Ok, "synced")
            .detail("commit: abc1234");
    }
    let manifests = s.section_owner(&OwnerLabel::new("source", "k8s-manifests"));
    manifests
        .status(Role::Fail, "sync failed")
        .detail("network unreachable");
});

// Surface: the reminder `cfgd apply` prints once at the end of a run whose Env
// phase changed something — the running shell predates the file, so the
// bootstrapped manager's PATH entries are one command away. Anchors the exact
// wording and the two-bullet shape.
golden_doc!(regression, apply_shell_env_reminder, |p, cap| {
    let s = p.section("Shell environment changed");
    s.bullet("run: source ~/.cfgd.env");
    s.bullet("or open a new shell");
});

// Same reminder in its real position: emitted after the apply summary line.
// Anchors the blank-line grouping that separates it from the summary, which is
// what makes it read as a closing note rather than another result row.
golden_doc!(
    regression,
    apply_shell_env_reminder_after_summary,
    |p, cap| {
        p.status(Role::Ok, "Applied 4 of 4 actions")
            .duration(Duration::from_millis(820));
        let s = p.section("Shell environment changed");
        s.bullet("run: source ~/.cfgd.env");
        s.bullet("or open a new shell");
    }
);

// Surface: `cfgd status` drift attribution. The `source:<name>` suffix is
// styled in `secondary` so the warn subject's yellow stays intact up to the
// suffix, then the suffix renders in pink. Tests run with colors off, so the
// snapshot shows the plain composed subject — the regression target is that
// the suffix lands at end-of-subject without breaking the line.
golden_doc!(regression, status_drift_secondary_suffix, |p, cap| {
    let s = p.section("Drift");
    s.status(
        Role::Warn,
        "file /etc/hosts — want: managed, have: external",
    )
    .label(Role::Secondary, "source:team-config");
});
