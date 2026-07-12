use super::*;
use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer, Role};

pub fn cmd_workflow_generate(cli: &Cli, printer: &Printer, force: bool) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    let workflow_dir = config_dir.join(".github").join("workflows");
    let workflow_path = workflow_dir.join("cfgd-release.yml");

    // Scan for profiles and modules
    let profile_names = scan_profile_names(&config_dir.join("profiles"), printer)?;
    let module_names = scan_module_names(&config_dir.join("modules"), printer)?;

    let default_branch =
        cfgd_core::detect_default_branch(&config_dir).unwrap_or_else(|| "master".to_string());

    if profile_names.is_empty() && module_names.is_empty() {
        printer.emit(
            Doc::new()
                .status(
                    Role::Warn,
                    "No profiles or modules found — nothing to generate",
                )
                .with_data(serde_json::json!({
                    "path": cfgd_core::to_posix_string(&workflow_path),
                    "profiles": Vec::<String>::new(),
                    "modules": Vec::<String>::new(),
                    "skipped": true,
                })),
        );
        return Ok(());
    }

    // Check for existing file
    if workflow_path.exists()
        && !force
        && !printer
            .prompt_confirm(&format!(
                "Workflow already exists at {} — overwrite?",
                workflow_path.posix()
            ))
            .unwrap_or(false)
    {
        printer.emit(
            Doc::new()
                .status(Role::Info, "Skipped workflow generation")
                .with_data(serde_json::json!({
                    "path": cfgd_core::to_posix_string(&workflow_path),
                    "skipped": true,
                    "profiles": &profile_names,
                    "modules": &module_names,
                })),
        );
        return Ok(());
    }

    let yaml = generate_release_workflow_yaml(&module_names, &profile_names, &default_branch)?;

    std::fs::create_dir_all(&workflow_dir).map_err(|e| {
        crate::cli::cli_error(
            cfgd_core::to_posix_string(&workflow_path),
            "write_failed",
            format!("failed to create workflow directory: {}", e),
            serde_json::json!({ "path": cfgd_core::to_posix_string(&workflow_path) }),
        )
    })?;
    cfgd_core::atomic_write_str(&workflow_path, &yaml).map_err(|e| {
        crate::cli::cli_error(
            cfgd_core::to_posix_string(&workflow_path),
            "write_failed",
            format!("failed to write workflow file: {}", e),
            serde_json::json!({ "path": cfgd_core::to_posix_string(&workflow_path) }),
        )
    })?;

    printer.emit(
        Doc::new()
            .status(
                Role::Ok,
                format!("Generated release workflow at {}", workflow_path.posix()),
            )
            .kv("Modules", module_names.len().to_string())
            .kv("Profiles", profile_names.len().to_string())
            .with_data(serde_json::json!({
                "path": cfgd_core::to_posix_string(&workflow_path),
                "profiles": &profile_names,
                "modules": &module_names,
            })),
    );

    Ok(())
}

/// Fold a resource name into a shell/expression-safe job-output key:
/// `-` and `.` become `_` so the key stays referenceable inside
/// `${{ steps.changes.outputs.<key> }}` (a literal `.` would parse as a
/// property accessor in the expression).
fn output_key(name: &str) -> String {
    name.replace(['-', '.'], "_")
}

/// Escape POSIX basic-regex metacharacters so a resource name interpolates
/// into the generated `grep` patterns as a literal. Validated names only
/// permit `.` among the specials, but escape the full BRE set for
/// robustness. `(`/`)` stay bare: in BRE a BACKSLASHED paren is a group.
fn escape_bre_literal(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '.' | '[' | ']' | '*' | '^' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub(super) fn generate_release_workflow_yaml(
    modules: &[String],
    profiles: &[String],
    default_branch: &str,
) -> anyhow::Result<String> {
    // Distinct names can fold to the same output key (`web.app` / `web-app` /
    // `web_app` → `web_app`), which would emit duplicate YAML mapping keys
    // GitHub rejects at load. Fail here naming the sources — silently
    // suffixing would change which paths wire to which tag job.
    let mut folded: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for m in modules {
        folded
            .entry(format!("module_{}", output_key(m)))
            .or_default()
            .push(format!("module '{m}'"));
    }
    for p in profiles {
        folded
            .entry(format!("profile_{}", output_key(p)))
            .or_default()
            .push(format!("profile '{p}'"));
    }
    let collisions: Vec<String> = folded
        .into_iter()
        .filter(|(_, sources)| sources.len() > 1)
        .map(|(key, sources)| format!("{} <- {}", key, sources.join(", ")))
        .collect();
    if !collisions.is_empty() {
        anyhow::bail!(
            "cannot generate workflow: resource names fold to the same job-output key ({}); rename one name in each colliding set so they stay distinct after '-'/'.' fold to '_'",
            collisions.join("; ")
        );
    }

    let mut yaml = String::new();
    let has_targets = !modules.is_empty() || !profiles.is_empty();

    // Header
    yaml.push_str(&format!(
        "# Auto-generated by cfgd — manages release tagging for modules and profiles.\n\
         # Regenerate with: cfgd workflow generate --force\n\
         name: cfgd Release\n\
         \n\
         on:\n\
         \x20 push:\n\
         \x20   branches: [{}]\n",
        default_branch,
    ));

    if has_targets {
        yaml.push_str("    paths:\n");
        for m in modules {
            yaml.push_str(&format!("      - 'modules/{}/**'\n", m));
        }
        for p in profiles {
            // Flat form (legacy) plus the canonical bundle form emitted by
            // `cfgd profile migrate` — `profiles/<name>/**` covers the bundle
            // manifest AND its files/ payload. Both are kept for the dual-read
            // support window so edits to either layout still trigger a release.
            yaml.push_str(&format!("      - 'profiles/{}.yaml'\n", p));
            yaml.push_str(&format!("      - 'profiles/{}.yml'\n", p));
            yaml.push_str(&format!("      - 'profiles/{}/**'\n", p));
        }
    } else {
        yaml.push_str(
            "    # paths: (auto-populated when modules/profiles are created)\n\
             \x20   #   - 'modules/<name>/**'\n\
             \x20   #   - 'profiles/<name>.yaml'\n\
             \x20   #   - 'profiles/<name>/**'\n",
        );
    }

    yaml.push_str(
        "\n\
         permissions:\n\
         \x20 contents: write\n\
         \n\
         jobs:\n",
    );

    if !has_targets {
        yaml.push_str(
            "  # Jobs are auto-generated when modules or profiles are created.\n\
             \x20 # Run `cfgd workflow generate --force` to regenerate manually.\n\
             \x20 placeholder:\n\
             \x20   runs-on: ubuntu-latest\n\
             \x20   steps:\n\
             \x20     - run: echo \"No modules or profiles to tag yet.\"\n",
        );
        return Ok(yaml);
    }

    // Detect changes job
    yaml.push_str(
        "\x20 detect-changes:\n\
         \x20   runs-on: ubuntu-latest\n\
         \x20   outputs:\n",
    );
    for m in modules {
        let safe = output_key(m);
        yaml.push_str(&format!(
            "      module_{}: ${{{{ steps.changes.outputs.module_{} }}}}\n",
            safe, safe
        ));
    }
    for p in profiles {
        let safe = output_key(p);
        yaml.push_str(&format!(
            "      profile_{}: ${{{{ steps.changes.outputs.profile_{} }}}}\n",
            safe, safe
        ));
    }

    yaml.push_str(
        "\x20   steps:\n\
         \x20     - uses: actions/checkout@v4\n\
         \x20       with:\n\
         \x20         fetch-depth: 0\n\
         \x20     - id: changes\n\
         \x20       run: |\n\
         \x20         if git rev-parse HEAD~1 >/dev/null 2>&1; then\n\
         \x20           CHANGED=$(git diff --name-only HEAD~1 HEAD)\n\
         \x20         else\n\
         \x20           CHANGED=$(git diff-tree --no-commit-id --name-only -r HEAD)\n\
         \x20         fi\n",
    );

    for m in modules {
        let safe = output_key(m);
        yaml.push_str(&format!(
            "          if echo \"$CHANGED\" | grep -q '^modules/{}/'; then\n\
             \x20           echo \"module_{}=true\" >> $GITHUB_OUTPUT\n\
             \x20         else\n\
             \x20           echo \"module_{}=false\" >> $GITHUB_OUTPUT\n\
             \x20         fi\n",
            escape_bre_literal(m),
            safe,
            safe
        ));
    }
    for p in profiles {
        let safe = output_key(p);
        // BRE alternation covering BOTH manifest forms while rejecting
        // sibling prefixes: the flat file must be exactly
        // `profiles/<name>.yaml|yml` (anchored, so `profiles/<name>.app.yaml`
        // does not match) and the bundle form requires the `/` separator
        // (so `profiles/<name>-other/...` does not match).
        yaml.push_str(&format!(
            "          if echo \"$CHANGED\" | grep -q '^profiles/{}\\(\\.\\(yaml\\|yml\\)$\\|/\\)'; then\n\
             \x20           echo \"profile_{}=true\" >> $GITHUB_OUTPUT\n\
             \x20         else\n\
             \x20           echo \"profile_{}=false\" >> $GITHUB_OUTPUT\n\
             \x20         fi\n",
            escape_bre_literal(p),
            safe,
            safe
        ));
    }

    // Tag modules job
    if !modules.is_empty() {
        yaml.push_str(
            "\n\
             \x20 tag-modules:\n\
             \x20   runs-on: ubuntu-latest\n\
             \x20   needs: detect-changes\n\
             \x20   strategy:\n\
             \x20     matrix:\n\
             \x20       include:\n",
        );
        for m in modules {
            let safe = output_key(m);
            yaml.push_str(&format!(
                "          - name: {}\n\
                 \x20           changed: ${{{{ needs.detect-changes.outputs.module_{} }}}}\n",
                m, safe
            ));
        }
        yaml.push_str(
            "\x20   steps:\n\
             \x20     - uses: actions/checkout@v4\n\
             \x20       if: matrix.changed == 'true'\n\
             \x20       with:\n\
             \x20         fetch-depth: 0\n\
             \x20     - name: Read module version\n\
             \x20       if: matrix.changed == 'true'\n\
             \x20       id: version\n\
             \x20       run: |\n\
             \x20         VERSION=$(grep -oP 'version:\\s*\"?\\K[^\"\\s]+' \"modules/${{ matrix.name }}/module.yaml\" || echo \"0.1.0\")\n\
             \x20         echo \"version=$VERSION\" >> $GITHUB_OUTPUT\n\
             \x20     - name: Tag module release\n\
             \x20       if: matrix.changed == 'true'\n\
             \x20       run: |\n\
             \x20         TAG=\"${{ matrix.name }}/v${{ steps.version.outputs.version }}\"\n\
             \x20         git tag -f \"$TAG\"\n\
             \x20         git push origin \"$TAG\" --force\n",
        );
    }

    // Tag profiles job
    if !profiles.is_empty() {
        yaml.push_str(
            "\n\
             \x20 tag-profiles:\n\
             \x20   runs-on: ubuntu-latest\n\
             \x20   needs: detect-changes\n\
             \x20   strategy:\n\
             \x20     matrix:\n\
             \x20       include:\n",
        );
        for p in profiles {
            let safe = output_key(p);
            yaml.push_str(&format!(
                "          - name: {}\n\
                 \x20           changed: ${{{{ needs.detect-changes.outputs.profile_{} }}}}\n",
                p, safe
            ));
        }
        yaml.push_str(
            "\x20   steps:\n\
             \x20     - uses: actions/checkout@v4\n\
             \x20       if: matrix.changed == 'true'\n\
             \x20       with:\n\
             \x20         fetch-depth: 0\n\
             \x20     - name: Tag profile release\n\
             \x20       if: matrix.changed == 'true'\n\
             \x20       run: |\n\
             \x20         DATE=$(date +%Y%m%d)\n\
             \x20         TAG=\"profile/${{ matrix.name }}/${DATE}\"\n\
             \x20         git tag -f \"$TAG\"\n\
             \x20         git push origin \"$TAG\" --force\n",
        );
    }

    Ok(yaml)
}

pub(super) fn maybe_update_workflow(
    cli: &Cli,
    printer: &cfgd_core::output::Printer,
) -> anyhow::Result<()> {
    let config_dir = config_dir(cli);
    init::regenerate_workflow(&config_dir, printer)?;
    Ok(())
}
