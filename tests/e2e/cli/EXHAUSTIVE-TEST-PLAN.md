# Exhaustive CLI Test Plan

Every cfgd command, subcommand, and flag. All tests are self-contained — no external repos or services. Remote-dependent commands use local git repos as fixtures.

## Fixture Strategy

- **Config dir**: Created per test suite in a temp directory with profile/file fixtures
- **Local git repos**: Used for `init --from`, `source add`, `module upgrade` — created inline via `git init`
- **Secrets**: age keypair generated at test start; sops encrypts/decrypts against it
- **Templates**: Tera `.tera` files in fixtures dir with variable placeholders
- **EDITOR**: Set to `true` (no-op) for all edit commands

## Test Matrix

### Global Flags (G01–G10)

| ID | Command | Assertion |
|---|---|---|
| G01 | `--help` | Exit 0, output contains all top-level subcommands |
| G02 | `--version` | Exit 0, output contains "cfgd" |
| G03 | `--verbose` | Accepted without crash |
| G04 | `-v` | Same as --verbose |
| G05 | `--quiet` | Accepted without crash |
| G06 | `-q` | Same as --quiet |
| G07 | `--no-color` | Accepted without crash |
| G08 | `--profile base` | Overrides active profile |
| G09 | `--config /bad/path` | Exit non-zero |
| G10 | `nonexistent-command` | Exit non-zero |

### init (I01–I05)

| ID | Command | Assertion |
|---|---|---|
| I01 | `init --from <local-git-repo>` | Creates cfgd.yaml + profiles/ |
| I02 | `init --from <local> --branch master` | Respects branch flag |
| I03 | `init --from <local> --theme minimal` | Theme persisted in config |
| I04 | `init --from <local> --module nvim` | Module flag accepted |
| I05 | `init` (no --from, pre-seeded dir) | Detects existing config |

### apply (A01–A17)

| ID | Command | Assertion |
|---|---|---|
| A01 | `apply --help` | Lists all flags |
| A02 | `apply --dry-run` | Exit 0, no files written |
| A03 | `apply --yes` | Exit 0, files created |
| A04 | `apply -y` | Short flag works |
| A05 | `apply --dry-run --phase files` | Only file phase shown |
| A06 | `apply --dry-run --phase packages` | Only package phase shown |
| A07 | `apply --dry-run --phase system` | Only system phase shown |
| A08 | `apply --dry-run --phase env` | Only env phase shown |
| A09 | `apply --dry-run --phase secrets` | Only secrets phase shown |
| A10 | `apply --dry-run --skip files` | Files phase excluded |
| A11 | `apply --dry-run --only files` | Only files phase included |
| A12 | `apply --dry-run --skip f --skip p` | Multiple skips |
| A13 | `apply --dry-run --only f --only v` | Multiple onlys |
| A14 | `apply --dry-run --module nonexistent` | Graceful handling |
| A15 | `apply --dry-run --yes` | Both flags accepted |
| A16 | `apply --yes` (fresh) | Expected files exist on disk |
| A17 | `apply --yes` (idempotent) | Second run succeeds, no errors |

### status / diff / log / verify / doctor (S01–DR01)

| ID | Command | Assertion |
|---|---|---|
| S01 | `status` | Exit 0 |
| S02 | `status --verbose` | Extra output |
| S03 | `status --quiet` | Minimal output |
| D01 | `diff` | Exit 0 or 1 |
| L01 | `log` | Exit 0 |
| L02 | `log --limit 5` | Respects limit |
| L03 | `log -c 1` | Short flag |
| V01 | `verify` | Exit 0 after clean apply |
| DR01 | `doctor` | Exit 0 |

### profile (P01–P40)

| ID | Command | Assertion |
|---|---|---|
| P01 | `profile --help` | Lists subcommands |
| P02 | `profile list` | Shows base, dev, work-dev |
| P03 | `profile show` | Shows active profile |
| P04 | `profile create test-minimal` | Exit 0, file exists |
| P05 | `profile create X --inherit base` | Inherits field set |
| P06 | `profile create X --package brew:rg` | Package added |
| P07 | `profile create X --variable K=V` | Variable set |
| P08 | `profile create X --system K=V` | System setting set |
| P09 | `profile create X --file path` | File entry added |
| P10 | `profile create X --private-files` | Private flag set |
| P11 | `profile create X --module nvim` | Module ref added |
| P12 | `profile create X` (all flags) | Multiple flags compose |
| P13 | `profile create X --pre/post-reconcile` | Hook scripts added |
| P14 | `profile create X --secret spec` | Secret entry added |
| P15 | `profile create` (duplicate) | Exit non-zero |
| P16 | `profile switch base` | Active profile changes |
| P17 | `profile switch dev` | Switches back |
| P18–P36 | `profile update --active --<thing> [value/-value]` | Each unified flag pair (add/remove via `-` prefix) for: package, file, env, system, module, inherit, secret, pre-apply, post-apply, private |
| P37 | `profile update <name>` (named) | Updates non-active profile |
| P38 | `profile delete X --yes` | Removed |
| P39 | `profile delete X -y` | Short flag |
| P40 | `profile delete nonexistent` | Exit non-zero |

### module (M01–M35)

| ID | Command | Assertion |
|---|---|---|
| M01 | `module --help` | Lists subcommands |
| M02 | `module create nvim` | Exit 0, file exists |
| M03 | `module create X --description "..."` | Description set |
| M04 | `module create X --depends Y` | Dependency added |
| M05 | `module create X --package mgr:pkg` | Package added |
| M06 | `module create X --file path` | File entry added |
| M07 | `module create X --private-files` | Private flag set |
| M08 | `module create X --post-apply "cmd"` | Script added |
| M09 | `module create X --set pkg.K.F=V` | Override set |
| M10 | `module create` (all flags) | Multiple flags compose |
| M11 | `module create` (duplicate) | Exit non-zero |
| M12 | `module list` | Shows created modules |
| M13 | `module show nvim` | Shows module details |
| M14 | `module show nonexistent` | Exit non-zero |
| M15–M25 | `module update X --<thing> [value/-value]` | Each unified flag pair (add/remove via `-` prefix) for: package, file, depends, post-apply, set, description, private |
| M26 | `module delete X --yes` | Removed |
| M27 | `module delete X -y` | Short flag |
| M28 | `module delete nonexistent` | Exit non-zero |
| M29 | `module upgrade X --yes` (local) | Graceful "no remote" handling |
| M30 | `module upgrade X --ref master` | Flag accepted |
| M31 | `module upgrade X --allow-unsigned` | Flag accepted |
| M32 | `module search query` | Graceful without registry |
| M33 | `module registry list` | Lists registries (empty ok) |
| M34 | `module registry add <local-git>` | Registry added |
| M35 | `module registry remove X` | Registry removed |

### source (SRC01–SRC24)

All source tests use a local git repo created inline as the "remote."

| ID | Command | Assertion |
|---|---|---|
| SRC01 | `source --help` | Lists subcommands |
| SRC02 | `source list` (empty) | Exit 0 |
| SRC03 | `source add <local-git>` | Source registered |
| SRC04 | `source add --branch master` | Branch respected |
| SRC05 | `source add --profile base` | Profile filter set |
| SRC06 | `source add --accept-recommended` | Flag accepted |
| SRC07 | `source add --priority 10` | Priority set |
| SRC08 | `source add --opt-in packages` | Opt-in items set |
| SRC09 | `source add --sync-interval 1h` | Interval set |
| SRC10 | `source add --auto-apply` | Auto-apply enabled |
| SRC11 | `source add --pin-version ">=1.0"` | Pin set |
| SRC12 | `source list` (after adds) | Sources listed |
| SRC13 | `source show <name>` | Details shown |
| SRC14 | `source update` (all) | Fetches all sources |
| SRC15 | `source update <name>` | Fetches one source |
| SRC16 | `source priority X 5` | Priority updated |
| SRC17 | `source priority X` (show) | Priority displayed |
| SRC18 | `source override X set path val` | Override set |
| SRC19 | `source override X reject path` | Rejection set |
| SRC20 | `source replace old new-url` | Source URL updated |
| SRC21 | `source create --name X` | Local source manifest created |
| SRC22 | `source remove X --keep-all` | Removed, resources kept |
| SRC23 | `source remove X --remove-all` | Removed, resources cleaned |
| SRC24 | `source add` (no --profile, platformProfiles) | Auto-selects profile for platform |

### explain (E01–E12)

| ID | Command | Assertion |
|---|---|---|
| E01 | `explain` (no args) | Lists all resource types |
| E02–E09 | `explain <type>` | Each of: profile, module, cfgdconfig, configsource, machineconfig, configpolicy, driftalert, teamconfig |
| E10 | `explain --recursive profile` | Recursive expansion |
| E11 | `explain profile spec.packages` | Field path drill-down |
| E12 | `explain nonexistent` | Handled gracefully |

### config (CF01–CF03)

| ID | Command | Assertion |
|---|---|---|
| CF01 | `config --help` | Lists subcommands |
| CF02 | `config show` | Displays config |
| CF03 | `config edit` (EDITOR=true) | Runs without crash |

### completions (CMP01–CMP05)

| ID | Command | Assertion |
|---|---|---|
| CMP01 | `completions bash` | Valid bash completions |
| CMP02 | `completions zsh` | Valid zsh completions |
| CMP03 | `completions fish` | Valid fish completions |
| CMP04 | `completions powershell` | Valid powershell completions |
| CMP05 | `completions elvish` | Valid elvish completions |

### secret (SEC01–SEC05)

| ID | Command | Assertion |
|---|---|---|
| SEC01 | `secret --help` | Lists subcommands |
| SEC02 | `secret init` | Creates age key |
| SEC03 | `secret encrypt <file>` | File encrypted |
| SEC04 | `secret decrypt <file>` | File decrypted |
| SEC05 | `secret edit <file>` (EDITOR=true) | Runs without crash |

### decide (DEC01–DEC05)

| ID | Command | Assertion |
|---|---|---|
| DEC01 | `decide --help` | Shows usage |
| DEC02 | `decide accept --all` | No-op when no pending |
| DEC03 | `decide reject --all` | No-op when no pending |
| DEC04 | `decide accept --source X` | Source filter accepted |
| DEC05 | `decide accept <resource>` | Specific resource accepted |

### daemon (DM01–DM04)

| ID | Command | Assertion |
|---|---|---|
| DM01 | `daemon --help` | Lists flags |
| DM02 | `daemon --status` | Reports not running |
| DM03 | `daemon --install` | Graceful in container |
| DM04 | `daemon --uninstall` | Graceful in container |

### sync / pull (SP01–SP02)

| ID | Command | Assertion |
|---|---|---|
| SP01 | `sync` | Graceful without remote |
| SP02 | `pull` | Graceful without remote |

### upgrade (UP01–UP02)

| ID | Command | Assertion |
|---|---|---|
| UP01 | `upgrade --help` | Shows usage |
| UP02 | `upgrade --check` | Graceful without network |

### workflow (WF01–WF03)

| ID | Command | Assertion |
|---|---|---|
| WF01 | `workflow --help` | Shows usage |
| WF02 | `workflow generate` | Generates CI YAML |
| WF03 | `workflow generate --force` | Overwrites existing |

### checkin / enroll (CI01–EN05)

| ID | Command | Assertion |
|---|---|---|
| CI01 | `checkin --help` | Shows --server-url flag |
| CI02 | `checkin --server-url http://bad` | Connection refused error |
| EN01 | `enroll --help` | Shows --server-url flag |
| EN02 | `enroll --server-url http://bad` | Connection refused error |
| EN03 | `enroll --ssh-key path` | Flag accepted (fails on connect) |
| EN04 | `enroll --gpg-key ID` | Flag accepted (fails on connect) |
| EN05 | `enroll --username user` | Flag accepted (fails on connect) |

### aliases (AL01–AL02)

| ID | Command | Assertion |
|---|---|---|
| AL01 | `add <path>` (alias for profile update --file) | File added to profile |
| AL02 | `remove -<path>` (alias for profile update --file, user prefixes with -) | File removed from profile |

### Behavioral Tests

| ID | Test | Assertion |
|---|---|---|
| INH01 | 3-level inheritance | All ancestor files deployed |
| INH02 | Variable override | Child value wins |
| TPL01 | Tera template | Variables rendered in output |
| ERR01 | Nonexistent profile in config | apply fails |
| ERR02 | Switch to nonexistent profile | Fails |
| ERR03–06 | Show/edit nonexistent resources | Fails |
| DRIFT01 | File modified after apply | verify detects drift |
| DRIFT02 | diff after modification | Shows changes |
| DRIFT03 | apply after drift | Restores correct state |

## Running

```bash
# Docker (self-contained)
docker build -t cfgd-exhaustive -f tests/e2e/cli/Dockerfile .
docker run --rm cfgd-exhaustive

# Native (requires cfgd built + sops + age)
bash tests/e2e/cli/scripts/run-exhaustive-tests.sh
```

**Total: 201 test cases.**
