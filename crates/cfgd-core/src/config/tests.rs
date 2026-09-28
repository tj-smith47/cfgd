use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::parse::parse_config;
use super::resolve::merge_layers;
use super::*;
use crate::PathDisplayExt;
use crate::deep_merge_yaml;
use crate::errors::ConfigError;
use crate::test_helpers::{SAMPLE_CONFIG_NO_ORIGIN_YAML, SAMPLE_CONFIG_YAML, SAMPLE_PROFILE_YAML};

#[test]
fn parse_yaml_config() {
    let config = parse_config(SAMPLE_CONFIG_YAML, Path::new("cfgd.yaml")).unwrap();
    assert_eq!(config.metadata.name, "test-config");
    assert_eq!(config.spec.profile.as_deref(), Some("default"));
    assert_eq!(config.spec.origin.len(), 1);
    assert_eq!(
        config.spec.origin[0].url,
        "https://github.com/test/repo.git"
    );
    assert_eq!(config.spec.origin[0].branch, "master");
}

#[test]
fn parse_config_without_origin() {
    let config = parse_config(SAMPLE_CONFIG_NO_ORIGIN_YAML, Path::new("cfgd.yaml")).unwrap();
    assert!(config.spec.origin.is_empty());
    assert!(config.spec.sources.is_empty());
}

#[test]
fn parse_config_rejects_unknown_apiversion() {
    let yaml = "apiVersion: cfgd.io/v1alpha2\nkind: Config\nmetadata:\n  name: m\nspec:\n  profile: default\n";
    let err = parse_config(yaml, Path::new("cfgd.yaml")).unwrap_err();
    assert!(err.to_string().contains("apiVersion"));
    assert!(err.to_string().contains("cfgd.io/v1alpha1")); // names the supported version
}

/// A synthetic older version routes through the table; the shipped table's
/// single entry is the identity. Nothing here invents a v1alpha2 schema:
/// the claim is that the ROUTE exists and that every entry lands on the
/// version this build reads.
#[test]
fn a_synthetic_older_api_version_routes_through_the_conversion_table() {
    use super::parse::{API_VERSION_CONVERSIONS, ApiVersionConversion, convertible_from};
    const SYNTHETIC: &[ApiVersionConversion] = &[
        ApiVersionConversion {
            from: "cfgd.io/v1alpha0",
            to: crate::API_VERSION,
        },
        ApiVersionConversion {
            from: crate::API_VERSION,
            to: crate::API_VERSION,
        },
    ];
    assert_eq!(
        convertible_from(SYNTHETIC, "cfgd.io/v1alpha0"),
        Some(crate::API_VERSION)
    );
    assert_eq!(convertible_from(SYNTHETIC, "cfgd.io/v9"), None);
    assert_eq!(
        convertible_from(API_VERSION_CONVERSIONS, crate::API_VERSION),
        Some(crate::API_VERSION),
        "the shipped table's identity entry answers for the current version"
    );
}

/// Every entry converts INTO the version this build reads, so no table row
/// can route a document to a version nothing parses.
#[test]
fn every_api_version_the_table_names_converts_to_the_current_one() {
    use super::parse::API_VERSION_CONVERSIONS;
    assert!(
        !API_VERSION_CONVERSIONS.is_empty(),
        "the table always carries its identity entry"
    );
    for entry in API_VERSION_CONVERSIONS {
        assert_eq!(
            entry.to,
            crate::API_VERSION,
            "{} routes to {}",
            entry.from,
            entry.to
        );
    }
}

/// The refusal names every version the table holds, the current one included.
///
/// A build carrying a second conversion row accepts a document the constant
/// does not name, so a message spelling the constant would refuse a version the
/// same build reads. The expectation is composed from the shipped table as
/// data: a synthetic table cannot reach the error type, which reads the
/// shipped one.
#[test]
fn the_unsupported_api_version_refusal_names_every_readable_version() {
    use super::parse::{API_VERSION_CONVERSIONS, ApiVersionConversion, readable_api_versions};
    // The shipped table's `from` and `to` are the same bytes while the identity
    // is its only row, so the column the composer reads is provable only
    // against a table whose two columns differ.
    const SYNTHETIC: &[ApiVersionConversion] = &[
        ApiVersionConversion {
            from: "cfgd.io/v1alpha0",
            to: crate::API_VERSION,
        },
        ApiVersionConversion {
            from: "cfgd.io/v1alpha1",
            to: crate::API_VERSION,
        },
    ];
    assert_eq!(
        readable_api_versions(SYNTHETIC),
        "cfgd.io/v1alpha0, cfgd.io/v1alpha1",
        "the composer names every `from` the table holds, in table order"
    );

    assert!(
        !API_VERSION_CONVERSIONS.is_empty(),
        "the table always carries its identity entry"
    );
    let message = ConfigError::UnsupportedApiVersion {
        found: "cfgd.io/v9".to_string(),
    }
    .to_string();
    for entry in API_VERSION_CONVERSIONS {
        assert!(
            message.contains(entry.from),
            "the refusal {message:?} does not name {}, which this build reads",
            entry.from
        );
    }
    assert!(
        message.ends_with(&readable_api_versions(API_VERSION_CONVERSIONS)),
        "the refusal {message:?} closes on something other than the table's own versions"
    );
}

/// Every site judging an incoming document's `apiVersion` asks the conversion
/// table, in every crate.
///
/// [`super::parse::validate_api_version`] is `pub(crate)` to cfgd-core, so a
/// device gateway or CSI site parsing a document it was handed cannot reach it,
/// and that is exactly where a hand-written comparison against the current
/// version goes in. Such a site accepts one version while the parser accepts a
/// set, and the two disagree in the release a second row lands in.
///
/// What it asks of a site is a comparison's two halves: an identifier holding
/// `api_version` or `apiVersion` on one side of `==`, `!=`, `matches!(`,
/// `.starts_with(`, `.contains(`, `.eq(` or `.ne(`, and `API_VERSION` or an
/// inline literal on the other, in either direction and at every place the
/// operator appears in the text. The question is put to a STATEMENT: rows are
/// gathered from one terminator to the next and joined, so a comparison split
/// across rows is one text while two neighbouring statements stay two. A `;`,
/// `{` or `}` terminates wherever it falls, and so does a `,` at the
/// statement's own bracket depth, which is what keeps two comma-separated
/// `match` arms from answering for each other. Literals and comments are
/// blanked before any of that, so a tell written inside either is invisible,
/// and an offender is reported at the row its statement opened on.
///
/// The population is every `<crate>/src` under `crates/`, read off the
/// directory so a crate added to the workspace joins it, and NAMED so a renamed
/// root fails by name, and whatever else appears cannot stand in for it. The
/// floor is stated per root: one number for the workspace is the biggest tree's
/// count plus the rest, so `crates/cfgd/src` could go dark inside it.
///
/// `validate_api_version` and `convertible_from` are exempt BY NAME, with no
/// hatch, because they ARE the comparison every other site is routed to. The
/// validator is judged by the reach check below, which is what catches
/// a body that keeps the table call and compares anyway.
#[test]
fn no_production_site_compares_an_api_version_by_hand() {
    use crate::test_helpers::{
        blank_non_code, calls_free_fn, carries_hatch, code_line, fn_declarations,
        production_slice_of, rust_sources_under, workspace_root,
    };

    /// Every crate root the walk must still be reading, workspace-relative,
    /// with a floor at the production sources each holds today, so a tree going
    /// dark fails on its own name. Each floor is a minimum the root must keep,
    /// and a count above it passes.
    const WALK_ROOTS: &[(&str, usize)] = &[
        // 196 sources `is_test_source` leaves in, less the 3 built only for tests
        // (bin/fake_cosign.rs, output/test_capture.rs, test_helpers.rs).
        ("crates/cfgd-core/src", 193),
        ("crates/cfgd-crd/src", 1),
        ("crates/cfgd-csi/src", 8),
        ("crates/cfgd-operator/src", 42),
        ("crates/cfgd-schema/src", 2),
        ("crates/cfgd/src", 144),
    ];
    const HATCH: &str = "// api-version-compare-ok:";
    const EXEMPT: &[&str] = &["validate_api_version", "convertible_from"];

    // Every statement of a masked slice, as the rows from one terminator to
    // the next joined with a space, with the row it opened on and the row it
    // closed on. A comparison written across rows is one expression that a
    // per-row read sees neither half of: `if doc.api_version` carries no
    // operator and `!= "cfgd.io/v1alpha1"` carries no field. Reading the whole
    // slice as one text is the other failure, where two neighbouring
    // statements answer for each other. Terminators are counted on code the
    // masking has already blanked, so one inside a literal or a comment ends
    // nothing. A `,` ends a statement only at the bracket depth the statement
    // opened at: a comma-separated `match` arm carries no other terminator, so
    // two arms would otherwise read as one statement and a version literal in
    // the first would answer for the field name in the second, while a comma
    // between a call's arguments separates operands of one expression. A row
    // is scanned to its end after its terminator, so a bracket opened behind
    // one (`} => format!(`) is still open when the next statement starts and
    // a comparison split across that call's arguments stays one statement.
    fn statements(code: &[String]) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        let mut open: Option<usize> = None;
        let mut depth = 0i32;
        for (n, line) in code.iter().enumerate() {
            if open.is_none() && line.trim().is_empty() {
                continue;
            }
            let first = *open.get_or_insert(n);
            let mut ends = false;
            for c in line.chars() {
                match c {
                    '(' | '[' => depth += 1,
                    ')' | ']' => depth -= 1,
                    ';' | '{' | '}' => ends = true,
                    ',' if depth <= 0 => ends = true,
                    _ => {}
                }
            }
            if ends {
                out.push((first, n, code[first..=n].join(" ")));
                open = None;
            }
        }
        if let Some(first) = open {
            out.push((first, code.len() - 1, code[first..].join(" ")));
        }
        out
    }
    // A comparison's two halves, at EVERY place the tell appears: taking only
    // the first split lets an unrelated comparison earlier in the statement
    // hide the one the tell names. `matches!(subject, PATTERN)` holds both
    // operands to the right of the macro name, so its own comma is the split.
    fn halves<'a>(code: &'a str, tell: &'a str) -> impl Iterator<Item = (&'a str, &'a str)> {
        code.match_indices(tell).filter_map(move |(at, _)| {
            let (left, right) = (&code[..at], &code[at + tell.len()..]);
            if tell == "matches!(" {
                return right.split_once(',');
            }
            Some((left, right))
        })
    }
    // The field a document carries is spelled one of two ways, and neither is
    // the constant: `API_VERSION` holds no lowercase `api_version`.
    fn names_the_field(part: &str) -> bool {
        part.contains("api_version") || part.contains("apiVersion")
    }
    // A literal reaches this walk as its own quotes around blanks, so the quote
    // is how a version spelled inline is seen at all.
    fn names_a_version(part: &str) -> bool {
        part.contains("API_VERSION") || part.contains('"')
    }
    fn compares_by_hand(code: &str) -> bool {
        [
            "==",
            "!=",
            "matches!(",
            ".starts_with(",
            ".contains(",
            ".eq(",
            ".ne(",
        ]
        .iter()
        .any(|tell| {
            halves(code, tell).any(|(left, right)| {
                (names_the_field(left) && names_a_version(right))
                    || (names_the_field(right) && names_a_version(left))
            })
        })
    }

    let workspace = workspace_root();
    let crates_dir = workspace.join("crates");
    let mut roots: Vec<PathBuf> = std::fs::read_dir(&crates_dir)
        .unwrap_or_else(|e| panic!("{}: {e}", crates_dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| {
                    panic!(
                        "{}: the walk must read every entry: {e}",
                        crates_dir.display()
                    )
                })
                .path()
                .join("src")
        })
        .filter(|src| src.is_dir())
        .collect();
    roots.sort();
    let read: Vec<String> = roots
        .iter()
        .map(|root| crate::to_posix_string(root.strip_prefix(&workspace).unwrap_or(root)))
        .collect();

    let declared =
        crate::test_helpers::workspace_declarations(crate::test_helpers::WORKSPACE_CRATES);
    let mut per_root: Vec<(String, usize)> = Vec::new();
    let mut offenders: Vec<String> = Vec::new();
    for (root, relative_root) in roots.iter().zip(read.clone()) {
        let mut files = 0usize;
        for path in rust_sources_under(root) {
            // Test scaffolding carries no `#[cfg(test)]` for the slice to cut at. A file built
            // only for tests (`is_test_only_file`) is named out as well: no shipped binary
            // compiles it, and cfgd-core's `test_helpers.rs` holds an inline test module the
            // slice would cut at, leaving a fraction of the file behind.
            if crate::test_helpers::is_test_source(&path)
                || crate::test_helpers::is_test_only_file(&path)
            {
                continue;
            }
            let production = production_slice_of(&path);
            // `blank_non_code` carries the state a per-line read cannot: a
            // `/* */` comment or a literal spanning rows leaves every row below
            // it read as code. `code_line` is then the per-line cut each
            // judgement is taken on, and both keep the row count, so the raw
            // line beside it is the one a hatch and a report are read off.
            let blanked = blank_non_code(&production);
            files += 1;
            let raw: Vec<&str> = production.lines().collect();
            let code: Vec<String> = blanked.lines().map(code_line).collect();
            let joined = code.join("\n");
            let exempt: Vec<std::ops::RangeInclusive<usize>> = declared
                .sites
                .iter()
                .zip(&declared.rows)
                .filter(|((_, site, _), (name, ..))| {
                    *site == path.as_path() && EXEMPT.contains(&name.as_str())
                })
                .filter_map(|(_, (_, _, body))| {
                    // Blanked the way this file's rows are, so the body is
                    // found at the rows it occupies.
                    let body = blank_non_code(body);
                    let at = joined.find(&body)?;
                    let first = joined[..at].matches('\n').count();
                    Some(first..=first + body.matches('\n').count())
                })
                .collect();
            let relative = crate::to_posix_string(path.strip_prefix(&workspace).unwrap_or(&path));
            for (first, last, statement) in statements(&code) {
                if !compares_by_hand(&statement) || exempt.iter().any(|rows| rows.contains(&first))
                {
                    continue;
                }
                // The hatch is read over the statement's own rows and the one
                // above it, because a comparison spanning rows carries its
                // reason wherever its author could see it.
                if raw[first.saturating_sub(1)..=last]
                    .iter()
                    .any(|l| carries_hatch(l, HATCH))
                {
                    continue;
                }
                let quoted: Vec<&str> = raw[first..=last].iter().map(|l| l.trim()).collect();
                offenders.push(format!(
                    "{relative}:{}: compares an apiVersion by hand; ask \
                     `cfgd_core::config::validate_api_version` (or the conversion table it \
                     reads), else say why with `{HATCH} <why this site cannot ask the table>`: {}",
                    first + 1,
                    quoted.join(" ")
                ));
            }
        }
        per_root.push((relative_root, files));
    }

    let unread: Vec<&str> = WALK_ROOTS
        .iter()
        .map(|(named, _)| *named)
        .filter(|named| !read.iter().any(|seen| seen == named))
        .collect();
    assert!(
        unread.is_empty(),
        "the walk no longer reads {unread:?}; it read {read:?} — a renamed or moved \
         crate root leaves its apiVersion comparisons judged by nobody"
    );
    // A root the walk never reported on reads as zero: a missing entry is the
    // whole tree going dark, which is what the floor is for.
    let short: Vec<(&str, usize, usize)> = WALK_ROOTS
        .iter()
        .map(|(named, floor)| {
            let seen = per_root
                .iter()
                .find(|(root, _)| root == named)
                .map_or(0, |(_, files)| *files);
            (*named, seen, *floor)
        })
        .filter(|(_, seen, floor)| seen < floor)
        .collect();
    assert!(
        short.is_empty() && read.len() >= WALK_ROOTS.len(),
        "a crate root contributed fewer production sources than it holds, so its \
         apiVersion comparisons are judged by nobody: {short:?} of {per_root:?}"
    );
    assert!(
        offenders.is_empty(),
        "every site judging a document's apiVersion asks the conversion table:\n{}",
        offenders.join("\n")
    );

    // The validator is exempt from the walk above, so the rule holds only while
    // its body is seen to ASK the table and to compare nothing itself: a body
    // that calls `convertible_from`, discards the answer and compares against
    // the constant passes every other pin in this file.
    let parse_rs = workspace
        .join("crates")
        .join("cfgd-core")
        .join("src")
        .join("config")
        .join("parse.rs");
    // one-file-declarations-ok: `config/parse.rs` alone, the one literal path above.
    let validator = fn_declarations(&blank_non_code(&production_slice_of(&parse_rs)))
        .into_iter()
        .find(|(name, ..)| name == "validate_api_version")
        .map(|(_, _, body)| body)
        .expect("config/parse.rs declares the one apiVersion validator");
    assert!(
        calls_free_fn(&validator, "convertible_from"),
        "validate_api_version no longer asks the conversion table:\n{validator}"
    );
    // Judged by the same statement grammar the walk uses, on a body
    // `fn_declarations` already handed back blanked.
    let validator_rows: Vec<String> = validator.lines().map(str::to_string).collect();
    assert!(
        !statements(&validator_rows)
            .iter()
            .any(|(.., statement)| compares_by_hand(statement)),
        "validate_api_version compares a version string itself where the table \
         should answer:\n{validator}"
    );
}

/// The global strategy is the fallback for files that declare none, and a
/// `Patch` file is defined by its own `patch:` block — so a file inheriting
/// the global could never satisfy it. Rejecting at load keeps that
/// unrepresentable instead of failing every such file at apply.
#[test]
fn parse_config_rejects_patch_as_the_global_file_strategy() {
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: m\nspec:\n  profile: default\n  fileStrategy: Patch\n";
    let err = parse_config(yaml, Path::new("cfgd.yaml")).unwrap_err();
    assert!(
        err.to_string().contains("fileStrategy"),
        "names the field: {err}"
    );
    assert!(
        err.to_string().contains("per-file"),
        "points at the per-file form: {err}"
    );
}

#[test]
fn parse_config_rejects_patch_as_the_global_file_strategy_in_toml() {
    let toml = "apiVersion = \"cfgd.io/v1alpha1\"\nkind = \"Config\"\n\n[metadata]\nname = \"m\"\n\n[spec]\nprofile = \"default\"\nfileStrategy = \"Patch\"\n";
    let err = parse_config(toml, Path::new("cfgd.toml")).unwrap_err();
    assert!(
        err.to_string().contains("fileStrategy"),
        "names the field: {err}"
    );
}

/// Drives off `FileStrategy::ALL` so a newly added variant is exercised here
/// automatically — the rejection must stay confined to what
/// `valid_as_global_default` excludes.
#[test]
fn parse_config_accepts_exactly_the_globally_valid_file_strategies() {
    for strategy in FileStrategy::ALL {
        let value = strategy.as_str();
        let yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: m\nspec:\n  profile: default\n  fileStrategy: {value}\n"
        );
        let parsed = parse_config(&yaml, Path::new("cfgd.yaml"));
        assert_eq!(
            parsed.is_ok(),
            strategy.valid_as_global_default(),
            "{value}: parse acceptance must match valid_as_global_default"
        );
    }
}

/// `ALL` is hand-maintained next to the enum; this pins it against the
/// deserializer, which has its own token list in `case_insensitive_enum!`.
#[test]
fn file_strategy_all_round_trips_through_the_deserializer() {
    for strategy in FileStrategy::ALL {
        let parsed: FileStrategy = serde_yaml::from_str(strategy.as_str())
            .unwrap_or_else(|e| panic!("{} must deserialize: {e}", strategy.as_str()));
        assert_eq!(parsed, *strategy);
        assert_eq!(
            serde_yaml::to_string(strategy).unwrap().trim(),
            strategy.as_str(),
            "as_str must match the serialized form"
        );
    }
}

/// `spec.migrationPolicy` is the persistent twin of `--migration-policy`, so
/// every word the flag accepts reaches the parser, spelled as a hand-written
/// document spells it; an absent key materializes the policy that writes
/// nothing on its own.
#[test]
fn parse_config_reads_every_migration_policy_and_defaults_to_prompt() {
    let absent = parse_config(SAMPLE_CONFIG_YAML, Path::new("cfgd.yaml")).unwrap();
    assert_eq!(absent.spec.migration_policy, MigrationPolicy::Prompt);

    for policy in MigrationPolicy::ALL {
        let value = policy.as_str().to_lowercase();
        let yaml = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: m\nspec:\n  profile: default\n  migrationPolicy: {value}\n"
        );
        let parsed = parse_config(&yaml, Path::new("cfgd.yaml"))
            .unwrap_or_else(|e| panic!("{value} must parse: {e}"));
        assert_eq!(parsed.spec.migration_policy, *policy);
    }
}

#[test]
fn load_profile_rejects_unknown_apiversion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("default.yaml");
    std::fs::write(
        &path,
        "apiVersion: cfgd.io/v1alpha2\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
    )
    .unwrap();
    let err = load_profile(&path).unwrap_err();
    assert!(err.to_string().contains("apiVersion"));
    assert!(err.to_string().contains("cfgd.io/v1alpha1")); // names the supported version
}

#[test]
fn load_profile_missing_path_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does-not-exist.yaml");
    let err = load_profile(&path).unwrap_err();
    assert!(
        matches!(
            err,
            crate::errors::CfgdError::Config(ConfigError::NotFound { .. })
        ),
        "a missing profile path must surface as ConfigError::NotFound, got: {err}",
    );
}

#[cfg(unix)]
#[test]
fn scan_profiles_ignores_broken_symlink() {
    // A dangling symlink in the profiles dir has no resolvable metadata; the
    // scanner must skip it rather than error or mistake it for a profile.
    let dir = tempfile::tempdir().unwrap();
    let profiles = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(profiles.join("real.yaml"), SAMPLE_PROFILE_YAML).unwrap();
    std::os::unix::fs::symlink(
        profiles.join("missing-target.yaml"),
        profiles.join("dangling.yaml"),
    )
    .unwrap();

    let entries = scan_profiles(&profiles).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"real"), "the real profile must be found");
    assert!(
        !names.contains(&"dangling"),
        "a broken symlink must not be scanned as a profile",
    );
}

#[test]
fn parse_profile_yaml() {
    let doc: ProfileDocument = serde_yaml::from_str(SAMPLE_PROFILE_YAML).unwrap();
    assert_eq!(doc.metadata.name, "base");
    assert_eq!(doc.spec.env.len(), 2);
    let pkgs = doc.spec.packages.as_ref().unwrap();
    let brew = pkgs.brew.as_ref().unwrap();
    assert_eq!(brew.formulae, vec!["ripgrep", "fd"]);
    assert_eq!(pkgs.cargo.as_ref().unwrap().packages, vec!["bat"]);
}

#[test]
fn backup_spec_schedule_owner_round_trips() {
    let yaml = "\
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  backups:
    - name: notes
      source: ~/notes
      scheduleOwner: local
";
    let doc: ProfileDocument = serde_yaml::from_str(yaml).expect("profile parses");
    assert_eq!(doc.spec.backups[0].schedule_owner, ScheduleOwner::Local);

    let rendered = serde_yaml::to_string(&doc.spec.backups[0]).expect("serialize");
    assert!(
        rendered.contains("scheduleOwner: Local\n"),
        "the pin survives a re-serialize: {rendered}"
    );
}

#[test]
fn merge_env_override() {
    let layer1 = ProfileLayer {
        source: "local".into(),
        profile_name: "base".into(),
        priority: crate::config::LOCAL_LAYER_PRIORITY,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            env: vec![
                EnvVar {
                    name: "editor".into(),
                    value: "vim".into(),
                    platforms: vec![],
                },
                EnvVar {
                    name: "shell".into(),
                    value: "/bin/bash".into(),
                    platforms: vec![],
                },
            ],
            ..Default::default()
        },
    };
    let layer2 = ProfileLayer {
        source: "local".into(),
        profile_name: "work".into(),
        priority: crate::config::LOCAL_LAYER_PRIORITY,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            env: vec![EnvVar {
                name: "editor".into(),
                value: "code".into(),
                platforms: vec![],
            }],
            ..Default::default()
        },
    };

    let merged = merge_layers(&[layer1, layer2]);
    assert_eq!(
        merged
            .env
            .iter()
            .find(|e| e.name == "editor")
            .map(|e| &e.value),
        Some(&"code".to_string())
    );
    assert_eq!(
        merged
            .env
            .iter()
            .find(|e| e.name == "shell")
            .map(|e| &e.value),
        Some(&"/bin/bash".to_string())
    );
}

#[test]
fn merge_packages_union() {
    let layer1 = ProfileLayer {
        source: "local".into(),
        profile_name: "base".into(),
        priority: crate::config::LOCAL_LAYER_PRIORITY,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            packages: Some(PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into()],
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    };
    let layer2 = ProfileLayer {
        source: "local".into(),
        profile_name: "work".into(),
        priority: crate::config::LOCAL_LAYER_PRIORITY,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            packages: Some(PackagesSpec {
                cargo: Some(CargoSpec {
                    file: None,
                    packages: vec!["bat".into(), "exa".into()],
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    };

    let merged = merge_layers(&[layer1, layer2]);
    assert_eq!(
        merged.packages.cargo.as_ref().unwrap().packages,
        vec!["bat", "exa"]
    );
}

#[test]
fn merge_files_overlay() {
    let layer1 = ProfileLayer {
        source: "local".into(),
        profile_name: "base".into(),
        priority: crate::config::LOCAL_LAYER_PRIORITY,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            files: Some(FilesSpec {
                managed: vec![ManagedFileSpec {
                    patch: None,
                    source: "base/.zshrc".into(),
                    target: PathBuf::from("/home/user/.zshrc"),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            }),
            ..Default::default()
        },
    };
    let layer2 = ProfileLayer {
        source: "local".into(),
        profile_name: "work".into(),
        priority: crate::config::LOCAL_LAYER_PRIORITY,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            files: Some(FilesSpec {
                managed: vec![ManagedFileSpec {
                    patch: None,
                    source: "work/.zshrc".into(),
                    target: PathBuf::from("/home/user/.zshrc"),
                    strategy: None,
                    private: false,
                    origin: None,
                    encryption: None,
                    permissions: None,
                }],
                ..Default::default()
            }),
            ..Default::default()
        },
    };

    let merged = merge_layers(&[layer1, layer2]);
    assert_eq!(merged.files.managed.len(), 1);
    assert_eq!(merged.files.managed[0].source, "work/.zshrc");
}

#[test]
fn deep_merge_yaml_maps() {
    let mut base = serde_yaml::from_str::<serde_yaml::Value>(
        r#"
            domain1:
              key1: value1
              key2: value2
            "#,
    )
    .unwrap();

    let overlay = serde_yaml::from_str::<serde_yaml::Value>(
        r#"
            domain1:
              key2: overridden
              key3: value3
            "#,
    )
    .unwrap();

    deep_merge_yaml(&mut base, &overlay);

    let map = base.as_mapping().unwrap();
    let domain = map
        .get(serde_yaml::Value::String("domain1".into()))
        .unwrap()
        .as_mapping()
        .unwrap();
    assert_eq!(
        domain.get(serde_yaml::Value::String("key1".into())),
        Some(&serde_yaml::Value::String("value1".into()))
    );
    assert_eq!(
        domain.get(serde_yaml::Value::String("key2".into())),
        Some(&serde_yaml::Value::String("overridden".into()))
    );
    assert_eq!(
        domain.get(serde_yaml::Value::String("key3".into())),
        Some(&serde_yaml::Value::String("value3".into()))
    );
}

#[test]
fn profile_resolution_with_filesystem() {
    let dir = tempfile::tempdir().unwrap();

    // Create base profile
    std::fs::write(
        dir.path().join("base.yaml"),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  env:
    - name: editor
      value: vim
  packages:
    cargo:
      - bat
"#,
    )
    .unwrap();

    // Create work profile inheriting base
    std::fs::write(
        dir.path().join("work.yaml"),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: work
spec:
  inherits:
    - base
  env:
    - name: editor
      value: code
  packages:
    cargo:
      - exa
"#,
    )
    .unwrap();

    let resolved = resolve_profile("work", dir.path()).unwrap();

    assert_eq!(resolved.layers.len(), 2);
    assert_eq!(resolved.layers[0].profile_name, "base");
    assert_eq!(resolved.layers[1].profile_name, "work");

    // editor should be overridden by work
    assert_eq!(
        resolved
            .merged
            .env
            .iter()
            .find(|e| e.name == "editor")
            .map(|e| &e.value),
        Some(&"code".to_string())
    );
    // packages should be unioned
    assert_eq!(
        resolved.merged.packages.cargo.as_ref().unwrap().packages,
        vec!["bat", "exa"]
    );
}

/// A profile whose declared step has nothing to run is refused while the
/// profile is resolved, which is the only path a machine with no
/// `spec.sources` ever takes: composition is skipped outright there, so a
/// refusal living only in the compose engine would let a blank step reach an
/// apply on every solo machine.
#[test]
fn a_profile_script_step_with_a_blank_run_is_refused_while_the_profile_resolves() {
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("solo.yaml"),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: solo
spec:
  scripts:
    postApply:
      - echo applied
      - "   "
"#,
    )
    .unwrap();

    let err = resolve_profile("solo", dir.path())
        .expect_err("a blank step must be refused as the profile resolves")
        .to_string();
    assert!(
        err.contains("profile")
            && err.contains("scripts.postApply[1]")
            && err.contains("empty 'run'"),
        "the refusal names the hook and the step it judged, got: {err}"
    );
}

#[test]
fn circular_inheritance_detected() {
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("a.yaml"),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: a
spec:
  inherits:
    - b
"#,
    )
    .unwrap();

    std::fs::write(
        dir.path().join("b.yaml"),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: b
spec:
  inherits:
    - a
"#,
    )
    .unwrap();

    let result = resolve_profile("a", dir.path());
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("circular"));
}

#[test]
fn config_not_found_error() {
    let result = load_config(Path::new("/nonexistent/cfgd.yaml"));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn parse_config_source_manifest() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: acme-corp-dev
  version: "2.1.0"
  description: "ACME Corp developer environment"
spec:
  provides:
    profiles:
      - acme-base
      - acme-backend
  policy:
    required:
      packages:
        brew:
          formulae:
            - git-secrets
            - pre-commit
    recommended:
      packages:
        brew:
          formulae:
            - k9s
      env:
        - name: EDITOR
          value: "code --wait"
    locked:
      files:
        - source: "security/policy.yaml"
          target: "~/.config/company/security-policy.yaml"
    constraints:
      noScripts: true
      noSecretsRead: true
      allowedTargetPaths:
        - "~/.config/acme/"
        - "~/.eslintrc*"
"#;
    let doc = parse_config_source(yaml).unwrap();
    assert_eq!(doc.metadata.name, "acme-corp-dev");
    assert_eq!(doc.metadata.version.as_deref(), Some("2.1.0"));
    assert_eq!(doc.spec.provides.profiles.len(), 2);

    let required_pkgs = doc.spec.policy.required.packages.as_ref().unwrap();
    let brew = required_pkgs.brew.as_ref().unwrap();
    assert_eq!(brew.formulae, vec!["git-secrets", "pre-commit"]);

    assert!(doc.spec.policy.constraints.no_scripts);
    assert_eq!(doc.spec.policy.constraints.allowed_target_paths.len(), 2);
    assert_eq!(doc.spec.policy.locked.files.len(), 1);
}

#[test]
fn parse_config_source_wrong_kind() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: not-a-source
spec:
  provides:
    profiles: []
  policy: {}
"#;
    let result = parse_config_source(yaml);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("ConfigSource"));
}

#[test]
fn source_spec_defaults() {
    let yaml = r#"
name: test-source
origin:
  type: Git
  url: https://example.com/config.git
"#;
    let spec: SourceSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.subscription.priority, 500);
    assert_eq!(spec.sync.interval, "1h");
    assert!(!spec.sync.auto_apply);
}

// A section written `key: null` (what a writer serializing an emptied block
// prints) reads as the bare `key:` beside it does.
#[test]
fn a_config_section_written_null_reads_as_its_default() {
    let origin = "name: test-source\norigin:\n  type: Git\n  url: https://example.com/config.git\n";
    for key in ["subscription", "sync"] {
        for value in ["", " null", " ~"] {
            let yaml = format!("{origin}{key}:{value}\n");
            let spec: SourceSpec =
                serde_yaml::from_str(&yaml).unwrap_or_else(|e| panic!("{key}:{value}: {e}"));
            assert_eq!(spec.subscription.priority, 500, "{key}:{value}");
            assert_eq!(spec.sync.interval, "1h", "{key}:{value}");
        }
    }

    let policy: ConfigSourcePolicy = serde_yaml::from_str(
        "required: null\nrecommended: ~\noptional: null\nlocked: null\nconstraints: null\n",
    )
    .unwrap();
    assert!(
        policy.constraints.no_scripts,
        "the constraints default holds"
    );
    let source: ConfigSourceSpec = serde_yaml::from_str("provides: null\npolicy: null\n").unwrap();
    assert!(source.provides.platform_profiles.is_empty());

    let compliance: ComplianceConfig =
        serde_yaml::from_str("enabled: true\nscope: null\nexport: null\n").unwrap();
    assert!(compliance.scope.files, "the scope default holds");

    let theme: ThemeConfig = serde_yaml::from_str("name: dracula\noverrides: null\n").unwrap();
    assert!(theme.overrides.is_empty());
    let update: UpdateConfig = serde_yaml::from_str("skills: null\n").unwrap();
    assert_eq!(update.skills.policy, SkillUpdatePolicy::default());
    let files: FilesSpec = serde_yaml::from_str("permissions: null\n").unwrap();
    assert!(files.permissions.is_empty());
    let profile: ProfileSpec = serde_yaml::from_str("system: null\n").unwrap();
    assert!(profile.system.is_empty());
    let packages: PackagesSpec = serde_yaml::from_str("apk: null\nbrew: null\n").unwrap();
    assert!(packages.apk.is_empty() && packages.brew.is_none());
    let output: OutputConfig =
        serde_yaml::from_str("theme:\n  name: dracula\n  overrides: null\n").unwrap();
    assert!(output.theme.unwrap().overrides.is_empty());
}

/// The key a field is read under: its `rename`, else its camelCase spelling.
fn serialized_field_name(ident: &str, attrs: &str) -> String {
    if let Some((_, after)) = attrs.split_once("rename = \"") {
        return after.split('"').next().unwrap_or_default().to_string();
    }
    let mut out = String::new();
    let mut up = false;
    for c in ident.chars() {
        if c == '_' {
            up = true;
        } else if up {
            out.push(c.to_ascii_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// The serialized names of the fields a hand-written `Deserialize` body reads
/// through `null_as_default`. Each mention counts only for the field its
/// attribute sits on, so one section restating the rule never vouches for a
/// sibling that does not.
fn fields_reading_null_by_hand(body: &[&str]) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut attrs = String::new();
    let mut depth = 0i32;
    for line in body {
        let code = line.trim_start();
        if depth > 0 || code.starts_with("#[") {
            depth += code.matches('[').count() as i32 - code.matches(']').count() as i32;
            attrs.push_str(code);
            continue;
        }
        if code.starts_with("//") {
            continue;
        }
        let field = crate::test_helpers::strip_item_lead(code);
        if let Some((ident, _)) = field.split_once(':')
            && !ident.is_empty()
            && ident.chars().all(|c| c.is_alphanumeric() || c == '_')
            && attrs.contains("deserialize_with = \"crate::config::null_as_default\"")
        {
            out.insert(serialized_field_name(ident, &attrs));
        }
        attrs.clear();
    }
    out
}

#[test]
fn a_hand_written_reader_restates_the_null_rule_per_section() {
    let body = "        struct Inner {\n            #[serde(default, deserialize_with = \"crate::config::null_as_default\")]\n            overrides: ThemeOverrides,\n            #[serde(default)]\n            extra_colors: Palette,\n            #[serde(\n                default,\n                deserialize_with = \"crate::config::null_as_default\"\n            )]\n            #[serde(rename = \"fonts\")]\n            font_set: Fonts,\n        }";
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(
        fields_reading_null_by_hand(&lines),
        ["fonts".to_string(), "overrides".to_string()].into()
    );
}

fn schema_type_names(node: &serde_json::Value) -> Vec<&str> {
    match &node["type"] {
        serde_json::Value::String(t) => vec![t.as_str()],
        serde_json::Value::Array(ts) => ts.iter().filter_map(serde_json::Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// Every optional object-shaped property of every local kind's live schema,
/// as (owning type, serialized name), and the ones whose schema refuses `null`.
fn defaulted_section_population() -> (std::collections::BTreeSet<(String, String)>, Vec<String>) {
    use serde_json::Value;

    let mut population = std::collections::BTreeSet::new();
    let mut refuses_null = Vec::new();
    for entry in crate::schema::KIND_REGISTRY.iter().filter(|e| !e.crd) {
        let schema: Value = serde_json::from_str(&entry.pretty_schema())
            .unwrap_or_else(|e| panic!("{} schema: {e}", entry.kind));
        let empty = serde_json::Map::new();
        let defs = schema["definitions"].as_object().unwrap_or(&empty);
        let is_object_ref = |m: &Value| {
            m["$ref"]
                .as_str()
                .and_then(|r| defs.get(r.trim_start_matches("#/definitions/")))
                .is_some_and(|d| schema_type_names(d).contains(&"object"))
        };
        let title = schema["title"]
            .as_str()
            .expect("a kind schema carries its type name");
        let owners =
            std::iter::once((title, &schema)).chain(defs.iter().map(|(k, v)| (k.as_str(), v)));
        for (owner, node) in owners {
            let Some(props) = node["properties"].as_object() else {
                continue;
            };
            let required: Vec<&str> = node["required"]
                .as_array()
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            for (name, prop) in props {
                if required.contains(&name.as_str()) {
                    continue;
                }
                let members: Vec<&Value> = match prop["anyOf"].as_array() {
                    Some(arms) => arms.iter().collect(),
                    None => vec![prop],
                };
                let object_shaped = members
                    .iter()
                    .any(|m| schema_type_names(m).contains(&"object") || is_object_ref(m));
                if !object_shaped {
                    continue;
                }
                if !members
                    .iter()
                    .any(|m| schema_type_names(m).contains(&"null"))
                {
                    refuses_null.push(format!("{} {owner}.{name}: {prop}", entry.kind));
                }
                population.insert((owner.to_string(), name.clone()));
            }
        }
    }
    (population, refuses_null)
}

/// A section a document may leave out (a struct or a map with a default) is
/// read from an explicit `null` as from a bare `key:`, and the published
/// schema says so: `deserialize_with = "crate::config::null_as_default"` and
/// `#[schemars(with = "Option<..>")]` are one statement, written together. The
/// population is read off the live schema of every local kind (each optional
/// object-shaped property); each member is then found in the source and held
/// to both halves, and no field in the source carries one half alone.
#[test]
fn every_defaulted_config_section_reads_null_as_its_default() {
    use std::collections::BTreeMap;

    let (population, refuses_null) = defaulted_section_population();
    assert!(
        refuses_null.is_empty(),
        "an optional section's schema must admit the `null` its loader reads as the default:\n{}",
        refuses_null.join("\n")
    );

    // (struct, serialized field) -> (file, rust type, reads null, schema admits null)
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/config");
    // A package field's union deserializer reads `null` itself, and
    // `every_list_or_map_package_field_declares_both_shapes_in_its_schema`
    // holds its schema.
    const READS_NULL_ITSELF: [&str; 2] = [
        "deserialize_with = \"list_or_packages_vec\"",
        "deserialize_with = \"list_or_struct\"",
    ];
    let mut fields: BTreeMap<(String, String), (String, String, bool, bool)> = BTreeMap::new();
    // A type deserialized by hand ignores its derive's field attributes, so its
    // impl restates the rule: owner -> the fields its body reads `null` for.
    let mut manual_impls: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
    let mut unpaired = Vec::new();
    let mut paired_per_file: BTreeMap<String, usize> = BTreeMap::new();
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for dirent in entries {
        let path = dirent
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .path();
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        if !file.ends_with(".rs") || crate::test_helpers::is_test_source(&path) {
            continue;
        }
        let src = crate::test_helpers::production_slice_of(&path);
        let lines: Vec<&str> = src.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            let Some((_, target)) = line.split_once("Deserialize<'de> for ") else {
                continue;
            };
            let owner: String = target
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let body_end = lines[n..]
                .iter()
                .position(|l| *l == "}")
                .map_or(lines.len(), |end| n + end);
            manual_impls.insert(owner, fields_reading_null_by_hand(&lines[n..body_end]));
        }
        let mut owner = String::new();
        let mut attrs = String::new();
        let mut depth = 0i32;
        for line in src.lines() {
            let code = line.trim_start();
            if depth > 0 || code.starts_with("#[") {
                depth += code.matches('[').count() as i32 - code.matches(']').count() as i32;
                attrs.push_str(code);
                continue;
            }
            if code.starts_with("//") {
                continue;
            }
            let rest = crate::test_helpers::strip_item_lead(code);
            if let Some(decl) = rest.strip_prefix("struct ") {
                owner = decl
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or_default()
                    .to_string();
            } else if rest != code
                && let Some((ident, ty)) = rest.split_once(':')
                && !ident.is_empty()
                && ident.chars().all(|c| c.is_alphanumeric() || c == '_')
                && !ty.starts_with(':')
            {
                let serialized = serialized_field_name(ident, &attrs);
                let reads_null = attrs
                    .contains("deserialize_with = \"crate::config::null_as_default\"")
                    || READS_NULL_ITSELF.iter().any(|d| attrs.contains(d));
                let schema_null = attrs.contains("#[schemars(with = \"Option<")
                    || READS_NULL_ITSELF.iter().any(|d| attrs.contains(d));
                if reads_null != schema_null {
                    unpaired.push(format!("{file}: {owner}.{ident}"));
                }
                if attrs.contains("#[schemars(with = \"Option<") {
                    *paired_per_file.entry(file.clone()).or_default() += 1;
                }
                let ty = ty.trim().trim_end_matches(',').to_string();
                fields.insert(
                    (owner.clone(), serialized),
                    (file.clone(), ty, reads_null, schema_null),
                );
            }
            attrs.clear();
        }
    }
    assert!(
        unpaired.is_empty(),
        "a field reading `null` as its default and a schema admitting it are written together:\n{}",
        unpaired.join("\n")
    );

    let mut unreached = Vec::new();
    let mut refuses_null_on_load = Vec::new();
    let mut members = 0;
    for (owner, name) in &population {
        match fields.get(&(owner.clone(), name.clone())) {
            None => unreached.push(format!("{owner}.{name}")),
            Some((_, ty, ..)) if ty.starts_with("Option<") => {}
            Some((file, _, reads_null, schema_null)) => {
                members += 1;
                let hand_read = manual_impls
                    .get(owner)
                    .is_none_or(|read| read.contains(name));
                if !(*reads_null && *schema_null && hand_read) {
                    refuses_null_on_load.push(format!("{file}: {owner}.{name}"));
                }
            }
        }
    }
    assert!(
        unreached.is_empty(),
        "the source walk no longer finds these schema properties: {unreached:?}"
    );
    assert!(
        refuses_null_on_load.is_empty(),
        "an optional section must read an explicit `null` as its default:\n{}",
        refuses_null_on_load.join("\n")
    );
    assert!(
        manual_impls
            .get("ThemeConfig")
            .is_some_and(|read| read.contains("overrides")),
        "the walk no longer finds the hand-written ThemeConfig reader: {manual_impls:?}"
    );
    assert!(
        members >= 20,
        "the walk no longer reaches the defaulted sections: it found {members} in {population:?}"
    );
    for (file, floor) in [
        ("compliance.rs", 2),
        ("module.rs", 2),
        ("profile_spec.rs", 2),
        ("root.rs", 2),
        ("source.rs", 11),
        ("theme.rs", 1),
    ] {
        let found = paired_per_file.get(file).copied().unwrap_or(0);
        assert!(
            found >= floor,
            "{file}: the walk reached {found} defaulted sections, expected at least {floor}"
        );
    }
}

/// Every defaulted section written `null` loads through the reader cfgd uses
/// for its kind, so a read struct standing between the document and the typed
/// one (the root config's legacy-key fold) is held to the rule the typed struct
/// and the schema state. One document per (kind, member): the member set to
/// `null`, everything around it the smallest document the schema accepts.
#[test]
fn every_defaulted_section_written_null_loads_through_its_kinds_reader() {
    use serde_json::{Map, Value, json};
    use std::collections::BTreeSet;

    fn resolve<'a>(r: &str, defs: &'a Map<String, Value>) -> (&'a str, &'a Value) {
        let name = r.trim_start_matches("#/definitions/");
        let (key, node) = defs
            .get_key_value(name)
            .unwrap_or_else(|| panic!("dangling schema reference {r}"));
        (key.as_str(), node)
    }

    fn minimal(node: &Value, defs: &Map<String, Value>) -> Value {
        if let Some(r) = node["$ref"].as_str() {
            return minimal(resolve(r, defs).1, defs);
        }
        if let Some(c) = node.get("const") {
            return c.clone();
        }
        if let Some(first) = node["enum"].as_array().and_then(|e| e.first()) {
            return first.clone();
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(arm) = node[key]
                .as_array()
                .and_then(|arms| arms.iter().find(|a| schema_type_names(a) != ["null"]))
            {
                return minimal(arm, defs);
            }
        }
        let ty = schema_type_names(node)
            .into_iter()
            .find(|t| *t != "null")
            .unwrap_or("string");
        match ty {
            "object" => {
                // A plain string field reads an empty default the schema allows
                // and a reader's own check may refuse (a file's `source`, a
                // package's `name`), so each one is written too.
                let required = node["required"].as_array().cloned().unwrap_or_default();
                let mut out = Map::new();
                for (name, prop) in node["properties"].as_object().into_iter().flatten() {
                    if required.contains(&json!(name)) || schema_type_names(prop) == ["string"] {
                        out.insert(name.clone(), minimal(prop, defs));
                    }
                }
                Value::Object(out)
            }
            "array" => json!([]),
            "integer" | "number" => json!(0),
            "boolean" => json!(false),
            _ => json!("x"),
        }
    }

    /// The smallest value of `node` that holds `target` written `null`, with
    /// the member's slash path, or `None` when `node` cannot reach it.
    fn with_null_at(
        node: &Value,
        owner: &str,
        target: &(String, String),
        defs: &Map<String, Value>,
        visiting: &mut Vec<String>,
    ) -> Option<(Value, String)> {
        if let Some(r) = node["$ref"].as_str() {
            let (name, def) = resolve(r, defs);
            if visiting.iter().any(|v| v == name) {
                return None;
            }
            visiting.push(name.to_string());
            let found = with_null_at(def, name, target, defs, visiting);
            visiting.pop();
            return found;
        }
        for key in ["anyOf", "oneOf"] {
            for arm in node[key].as_array().into_iter().flatten() {
                if let Some(found) = with_null_at(arm, owner, target, defs, visiting) {
                    return Some(found);
                }
            }
        }
        if let Some(props) = node["properties"].as_object() {
            for (name, prop) in props {
                let (value, path) = if owner == target.0 && *name == target.1 {
                    (Value::Null, name.clone())
                } else if let Some((v, p)) = with_null_at(prop, "", target, defs, visiting) {
                    (v, format!("{name}/{p}"))
                } else {
                    continue;
                };
                let mut obj = minimal(node, defs);
                obj[name.as_str()] = value;
                return Some((obj, path));
            }
        }
        if node["items"].is_object()
            && let Some((v, p)) = with_null_at(&node["items"], "", target, defs, visiting)
        {
            return Some((json!([v]), format!("0/{p}")));
        }
        if node["additionalProperties"].is_object()
            && let Some((v, p)) =
                with_null_at(&node["additionalProperties"], "", target, defs, visiting)
        {
            return Some((json!({ "k": v }), format!("k/{p}")));
        }
        None
    }

    let (population, _) = defaulted_section_population();
    let home = tempfile::tempdir().unwrap();
    let mut reached = BTreeSet::new();
    let mut refused = Vec::new();
    for entry in crate::schema::KIND_REGISTRY.iter().filter(|e| !e.crd) {
        let schema: Value = serde_json::from_str(&entry.pretty_schema())
            .unwrap_or_else(|e| panic!("{} schema: {e}", entry.kind));
        let defs = schema["definitions"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let root = schema["title"]
            .as_str()
            .expect("a kind schema carries its type name");
        for member in &population {
            let Some((value, path)) = with_null_at(&schema, root, member, &defs, &mut Vec::new())
            else {
                continue;
            };
            reached.insert(member.clone());
            let (doc, path) = if entry.kind == "Config" {
                (value, path)
            } else {
                (json!({ "spec": value }), format!("spec/{path}"))
            };
            let mut doc = doc;
            doc["apiVersion"] = json!(entry.api_version);
            doc["kind"] = json!(entry.kind);
            doc["metadata"] = json!({ "name": "null-sections" });
            let yaml = serde_yaml::to_string(&doc).expect("a JSON value serializes as YAML");
            let read = match entry.kind {
                "Module" => super::parse_module(&yaml).map(drop),
                "ConfigSource" => super::parse_config_source(&yaml).map(drop),
                "Profile" | "Config" => {
                    let file = home.path().join(format!("{}.yaml", entry.kind));
                    std::fs::write(&file, &yaml).unwrap();
                    if entry.kind == "Profile" {
                        super::load_profile(&file).map(drop)
                    } else {
                        super::load_config(&file).map(drop)
                    }
                }
                other => panic!("no reader is named here for the local kind {other}"),
            };
            if let Err(e) = read {
                refused.push(format!("{} {path}: {e}\n{yaml}", entry.kind));
            }
        }
    }
    assert!(
        refused.is_empty(),
        "a defaulted section written `null` must load through its kind's reader:\n{}",
        refused.join("\n")
    );
    let unreached: Vec<_> = population.difference(&reached).collect();
    assert!(
        unreached.is_empty(),
        "no document of any local kind reaches these sections: {unreached:?}"
    );
    assert!(
        reached.len() >= 20,
        "the walk reached only {} sections: {reached:?}",
        reached.len()
    );
}

#[test]
fn cargo_spec_deserialize_list() {
    // Dual-form lives on the `PackagesSpec::cargo` field, the real consumer
    // path — not on `CargoSpec` itself.
    let yaml = r#"
cargo:
  - bat
  - ripgrep
"#;
    let spec: PackagesSpec = serde_yaml::from_str(yaml).unwrap();
    let cargo = spec.cargo.unwrap();
    assert_eq!(cargo.packages, vec!["bat", "ripgrep"]);
    assert!(cargo.file.is_none());
}

#[test]
fn cargo_spec_deserialize_map() {
    let yaml = r#"
cargo:
  file: Cargo.toml
  packages:
    - extra-pkg
"#;
    let spec: PackagesSpec = serde_yaml::from_str(yaml).unwrap();
    let cargo = spec.cargo.unwrap();
    assert_eq!(cargo.file.as_deref(), Some("Cargo.toml"));
    assert_eq!(cargo.packages, vec!["extra-pkg"]);
}

#[test]
fn cargo_spec_deserialize_file_only() {
    let yaml = r#"
cargo:
  file: Cargo.toml
"#;
    let spec: PackagesSpec = serde_yaml::from_str(yaml).unwrap();
    let cargo = spec.cargo.unwrap();
    assert_eq!(cargo.file.as_deref(), Some("Cargo.toml"));
    assert!(cargo.packages.is_empty());
}

#[test]
fn packages_spec_with_manifest_files() {
    let yaml = r#"
brew:
  file: Brewfile
  formulae:
    - extra-tool
apt:
  file: packages.apt.txt
npm:
  file: package.json
  global:
    - extra-global
cargo:
  file: Cargo.toml
"#;
    let spec: PackagesSpec = serde_yaml::from_str(yaml).unwrap();

    let brew = spec.brew.as_ref().unwrap();
    assert_eq!(brew.file.as_deref(), Some("Brewfile"));
    assert_eq!(brew.formulae, vec!["extra-tool"]);

    let apt = spec.apt.as_ref().unwrap();
    assert_eq!(apt.file.as_deref(), Some("packages.apt.txt"));

    let npm = spec.npm.as_ref().unwrap();
    assert_eq!(npm.file.as_deref(), Some("package.json"));
    assert_eq!(npm.global, vec!["extra-global"]);

    let cargo = spec.cargo.as_ref().unwrap();
    assert_eq!(cargo.file.as_deref(), Some("Cargo.toml"));
}

#[test]
fn merge_manifest_file_fields() {
    let layer1 = ProfileLayer {
        source: "local".into(),
        profile_name: "base".into(),
        priority: crate::config::LOCAL_LAYER_PRIORITY,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            packages: Some(PackagesSpec {
                brew: Some(BrewSpec {
                    file: Some("Brewfile".into()),
                    formulae: vec!["git".into()],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    };
    let layer2 = ProfileLayer {
        source: "local".into(),
        profile_name: "work".into(),
        priority: crate::config::LOCAL_LAYER_PRIORITY,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            packages: Some(PackagesSpec {
                brew: Some(BrewSpec {
                    formulae: vec!["ripgrep".into()],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
    };

    let merged = merge_layers(&[layer1, layer2]);
    let brew = merged.packages.brew.as_ref().unwrap();
    // file from base preserved (layer2 didn't override)
    assert_eq!(brew.file.as_deref(), Some("Brewfile"));
    // formulae unioned
    assert_eq!(brew.formulae, vec!["git", "ripgrep"]);
}

#[test]
fn parse_config_source_with_profile_details() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: acme
spec:
  provides:
    profiles:
      - acme-base
      - acme-backend
    profileDetails:
      - name: acme-base
        description: "Core tools and security"
        path: profiles/base.yaml
      - name: acme-backend
        description: "Go, k8s tools"
        path: profiles/backend.yaml
        inherits:
          - acme-base
    platformProfiles:
      macos: acme-base
      debian: acme-backend
  policy: {}
"#;
    let doc = parse_config_source(yaml).unwrap();
    assert_eq!(doc.spec.provides.profile_details.len(), 2);
    assert_eq!(doc.spec.provides.profile_details[0].name, "acme-base");
    assert_eq!(
        doc.spec.provides.profile_details[0].description.as_deref(),
        Some("Core tools and security")
    );
    assert_eq!(
        doc.spec.provides.profile_details[1].inherits,
        vec!["acme-base"]
    );
    assert_eq!(doc.spec.provides.platform_profiles.len(), 2);
    assert_eq!(
        doc.spec.provides.platform_profiles.get("macos").unwrap(),
        "acme-base"
    );
}

#[test]
fn parse_os_release_debian() {
    let content = r#"PRETTY_NAME="Debian GNU/Linux 12 (bookworm)"
NAME="Debian GNU/Linux"
VERSION_ID="12"
ID=debian
"#;
    let fields = crate::platform::parse_os_release_content(content);
    assert_eq!(
        fields.get("ID").map(|v| v.to_lowercase()).as_deref(),
        Some("debian")
    );
    assert_eq!(fields.get("VERSION_ID").map(|s| s.as_str()), Some("12"));
}

#[test]
fn parse_os_release_ubuntu() {
    let content = r#"NAME="Ubuntu"
VERSION="22.04.3 LTS (Jammy Jellyfish)"
ID=ubuntu
VERSION_ID="22.04"
"#;
    let fields = crate::platform::parse_os_release_content(content);
    assert_eq!(
        fields.get("ID").map(|v| v.to_lowercase()).as_deref(),
        Some("ubuntu")
    );
    assert_eq!(fields.get("VERSION_ID").map(|s| s.as_str()), Some("22.04"));
}

#[test]
fn parse_os_release_empty() {
    let fields = crate::platform::parse_os_release_content("");
    assert!(!fields.contains_key("ID"));
    assert!(!fields.contains_key("VERSION_ID"));
}

#[test]
fn match_platform_profile_exact_distro() {
    let mut profiles = HashMap::new();
    profiles.insert("macos".into(), "profiles/macos.yaml".into());
    profiles.insert("debian".into(), "profiles/debian.yaml".into());

    let platform = PlatformInfo {
        os: "linux".into(),
        distro: Some("debian".into()),
        distro_version: Some("12".into()),
    };
    assert_eq!(
        match_platform_profile(&platform, &profiles),
        Some("profiles/debian.yaml".into())
    );
}

#[test]
fn match_platform_profile_os_fallback() {
    let mut profiles = HashMap::new();
    profiles.insert("macos".into(), "profiles/macos.yaml".into());
    profiles.insert("linux".into(), "profiles/linux.yaml".into());

    let platform = PlatformInfo {
        os: "linux".into(),
        distro: Some("arch".into()),
        distro_version: None,
    };
    // No "arch" key, falls back to "linux"
    assert_eq!(
        match_platform_profile(&platform, &profiles),
        Some("profiles/linux.yaml".into())
    );
}

#[test]
fn match_platform_profile_no_match() {
    let mut profiles = HashMap::new();
    profiles.insert("debian".into(), "profiles/debian.yaml".into());

    let platform = PlatformInfo {
        os: "macos".into(),
        distro: None,
        distro_version: None,
    };
    assert!(match_platform_profile(&platform, &profiles).is_none());
}

#[test]
fn source_profile_names_from_details() {
    let provides = ConfigSourceProvides {
        profiles: vec!["old-name".into()],
        profile_details: vec![
            ConfigSourceProfileEntry {
                name: "base".into(),
                description: Some("Base profile".into()),
                path: None,
                inherits: vec![],
            },
            ConfigSourceProfileEntry {
                name: "backend".into(),
                description: None,
                path: None,
                inherits: vec!["base".into()],
            },
        ],
        platform_profiles: HashMap::new(),
        modules: vec![],
    };
    // profile_details takes precedence
    assert_eq!(source_profile_names(&provides), vec!["base", "backend"]);
}

#[test]
fn source_profile_names_fallback_to_profiles() {
    let provides = ConfigSourceProvides {
        profiles: vec!["alpha".into(), "beta".into()],
        profile_details: vec![],
        platform_profiles: HashMap::new(),
        modules: vec![],
    };
    assert_eq!(source_profile_names(&provides), vec!["alpha", "beta"]);
}

#[test]
fn auto_apply_policy_deserializes() {
    let yaml = r#"
newRecommended: Accept
newOptional: Notify
lockedConflict: Reject
"#;
    let policy: AutoApplyPolicyConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(policy.new_recommended, PolicyAction::Accept);
    assert_eq!(policy.new_optional, PolicyAction::Notify);
    assert_eq!(policy.locked_conflict, PolicyAction::Reject);
}

#[test]
fn reconcile_config_with_policy_deserializes() {
    let yaml = r#"
interval: 5m
onChange: true
autoApply: true
policy:
  newRecommended: Accept
  newOptional: Ignore
  lockedConflict: Notify
"#;
    let config: ReconcileConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.auto_apply);
    assert!(config.on_change);
    let policy = config.policy.unwrap();
    assert_eq!(policy.new_recommended, PolicyAction::Accept);
}

#[test]
fn reconcile_patches_deserialize() {
    let yaml = r#"
interval: 5m
patches:
  - kind: Module
    name: certificates
    interval: 1m
    driftPolicy: Auto
  - kind: Profile
    name: work
    autoApply: true
  - kind: Module
    interval: 30s
"#;
    let config: ReconcileConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(config.patches.len(), 3);
    assert_eq!(config.patches[0].kind, ReconcilePatchKind::Module);
    assert_eq!(config.patches[0].name.as_deref(), Some("certificates"));
    assert_eq!(config.patches[0].interval.as_deref(), Some("1m"));
    assert_eq!(config.patches[0].drift_policy, Some(DriftPolicy::Auto));
    assert!(config.patches[0].auto_apply.is_none());
    assert_eq!(config.patches[1].kind, ReconcilePatchKind::Profile);
    assert_eq!(config.patches[1].name.as_deref(), Some("work"));
    assert_eq!(config.patches[1].auto_apply, Some(true));
    assert!(config.patches[1].interval.is_none());
    // Kind-wide patch (no name)
    assert_eq!(config.patches[2].kind, ReconcilePatchKind::Module);
    assert!(config.patches[2].name.is_none());
    assert_eq!(config.patches[2].interval.as_deref(), Some("30s"));
}

#[test]
fn reconcile_config_without_patches_has_empty_vec() {
    let yaml = "interval: 10m\n";
    let config: ReconcileConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.patches.is_empty());
}

// --- desired_packages_for_spec ---

#[test]
fn desired_packages_brew_formulae() {
    let spec = PackagesSpec {
        brew: Some(BrewSpec {
            formulae: vec!["curl".into(), "wget".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        desired_packages_for_spec("brew", &spec),
        vec!["curl", "wget"]
    );
}

#[test]
fn desired_packages_brew_taps() {
    let spec = PackagesSpec {
        brew: Some(BrewSpec {
            taps: vec!["homebrew/core".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        desired_packages_for_spec("brew-tap", &spec),
        vec!["homebrew/core"]
    );
}

#[test]
fn desired_packages_brew_casks() {
    let spec = PackagesSpec {
        brew: Some(BrewSpec {
            casks: vec!["firefox".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        desired_packages_for_spec("brew-cask", &spec),
        vec!["firefox"]
    );
}

#[test]
fn desired_packages_apt() {
    let spec = PackagesSpec {
        apt: Some(AptSpec {
            packages: vec!["git".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(desired_packages_for_spec("apt", &spec), vec!["git"]);
}

#[test]
fn desired_packages_cargo() {
    let spec = PackagesSpec {
        cargo: Some(CargoSpec {
            packages: vec!["ripgrep".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(desired_packages_for_spec("cargo", &spec), vec!["ripgrep"]);
}

#[test]
fn desired_packages_npm() {
    let spec = PackagesSpec {
        npm: Some(NpmSpec {
            global: vec!["typescript".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(desired_packages_for_spec("npm", &spec), vec!["typescript"]);
}

#[test]
fn desired_packages_pipx() {
    let spec = PackagesSpec {
        pipx: vec!["black".into()],
        ..Default::default()
    };
    assert_eq!(desired_packages_for_spec("pipx", &spec), vec!["black"]);
}

#[test]
fn desired_packages_snap_merges_classic() {
    let spec = PackagesSpec {
        snap: Some(SnapSpec {
            packages: vec!["core".into()],
            classic: vec!["code".into()],
        }),
        ..Default::default()
    };
    let result = desired_packages_for_spec("snap", &spec);
    assert_eq!(result, vec!["core", "code"]);
}

#[test]
fn desired_packages_snap_classic_dedup() {
    let spec = PackagesSpec {
        snap: Some(SnapSpec {
            packages: vec!["code".into()],
            classic: vec!["code".into()],
        }),
        ..Default::default()
    };
    let result = desired_packages_for_spec("snap", &spec);
    assert_eq!(result, vec!["code"]);
}

#[test]
fn desired_packages_custom_manager() {
    let spec = PackagesSpec {
        custom: vec![CustomManagerSpec {
            name: "my-mgr".into(),
            check: "which my-mgr".into(),
            list_installed: "my-mgr list".into(),
            install: "my-mgr install".into(),
            uninstall: "my-mgr remove".into(),
            update: None,
            packages: vec!["tool-a".into()],
        }],
        ..Default::default()
    };
    assert_eq!(desired_packages_for_spec("my-mgr", &spec), vec!["tool-a"]);
}

#[test]
fn desired_packages_unknown_manager() {
    let spec = PackagesSpec::default();
    assert!(desired_packages_for_spec("nonexistent", &spec).is_empty());
}

#[test]
fn desired_packages_winget() {
    let spec = PackagesSpec {
        winget: vec!["Microsoft.VisualStudioCode".into(), "Git.Git".into()],
        ..Default::default()
    };
    assert_eq!(
        desired_packages_for_spec("winget", &spec),
        vec!["Microsoft.VisualStudioCode", "Git.Git"]
    );
}

#[test]
fn desired_packages_chocolatey() {
    let spec = PackagesSpec {
        chocolatey: vec!["nodejs".into()],
        ..Default::default()
    };
    assert_eq!(
        desired_packages_for_spec("chocolatey", &spec),
        vec!["nodejs"]
    );
}

#[test]
fn desired_packages_scoop() {
    let spec = PackagesSpec {
        scoop: vec!["ripgrep".into()],
        ..Default::default()
    };
    assert_eq!(desired_packages_for_spec("scoop", &spec), vec!["ripgrep"]);
}

// --- load_config filesystem ---

#[test]
fn load_config_valid_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cfgd.yaml");
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n".to_string();
    std::fs::write(&path, &yaml).unwrap();
    let cfg = load_config(&path).unwrap();
    assert_eq!(cfg.metadata.name, "test");
}

#[test]
fn load_config_valid_toml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cfgd.toml");
    let toml = "apiVersion = \"cfgd.io/v1alpha1\"\nkind = \"Config\"\n\n[metadata]\nname = \"test\"\n\n[spec]\nprofile = \"default\"\n";
    std::fs::write(&path, toml).unwrap();
    let cfg = load_config(&path).unwrap();
    assert_eq!(cfg.metadata.name, "test");
}

#[test]
fn load_config_missing_file() {
    let result = load_config(std::path::Path::new("/nonexistent-12345/cfgd.yaml"));
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("config file not found"),
        "expected 'config file not found' in error, got: {msg}"
    );
    assert!(
        msg.contains("/nonexistent-12345/cfgd.yaml"),
        "expected path in error, got: {msg}"
    );
}

#[test]
fn load_config_dir_infers_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: from-yaml\nspec:\n  profile: default\n";
    std::fs::write(dir.path().join(CONFIG_FILENAME), yaml).unwrap();
    let cfg = load_config(dir.path()).unwrap();
    assert_eq!(cfg.metadata.name, "from-yaml");
}

#[test]
fn load_config_dir_infers_toml() {
    let dir = tempfile::tempdir().unwrap();
    let toml = "apiVersion = \"cfgd.io/v1alpha1\"\nkind = \"Config\"\n\n[metadata]\nname = \"from-toml\"\n\n[spec]\nprofile = \"default\"\n";
    std::fs::write(dir.path().join(CONFIG_FILENAME_TOML), toml).unwrap();
    let cfg = load_config(dir.path()).unwrap();
    assert_eq!(cfg.metadata.name, "from-toml");
}

#[test]
fn load_config_dir_prefers_yaml_over_toml() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: from-yaml\nspec:\n  profile: default\n";
    let toml = "apiVersion = \"cfgd.io/v1alpha1\"\nkind = \"Config\"\n\n[metadata]\nname = \"from-toml\"\n\n[spec]\nprofile = \"default\"\n";
    std::fs::write(dir.path().join(CONFIG_FILENAME), yaml).unwrap();
    std::fs::write(dir.path().join(CONFIG_FILENAME_TOML), toml).unwrap();
    let cfg = load_config(dir.path()).unwrap();
    assert_eq!(cfg.metadata.name, "from-yaml");
}

#[test]
fn load_config_empty_dir_reports_yaml_filename() {
    let dir = tempfile::tempdir().unwrap();
    let err = load_config(dir.path()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("config file not found"),
        "expected 'config file not found' in error, got: {msg}"
    );
    assert!(
        msg.ends_with(CONFIG_FILENAME) || msg.contains(&format!("/{CONFIG_FILENAME}")),
        "expected error to name {CONFIG_FILENAME}, not the bare dir, got: {msg}"
    );
    assert!(
        !msg.contains("Is a directory"),
        "directory arg must not surface a raw OS read error, got: {msg}"
    );
}

// --- resolve_profile deeper inheritance ---

#[test]
fn test_ai_config_defaults() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec: {}
"#;
    let config: CfgdConfig = serde_yaml::from_str(yaml).unwrap();
    let ai = config.spec.ai.unwrap_or_default();
    assert_eq!(ai.provider, "claude");
    assert_eq!(ai.model, "claude-sonnet-5");
    assert_eq!(ai.api_key_env, "ANTHROPIC_API_KEY");
}

#[test]
fn test_ai_config_custom() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  ai:
    provider: claude
    model: claude-opus-5
    apiKeyEnv: MY_CLAUDE_KEY
"#;
    let config: CfgdConfig = serde_yaml::from_str(yaml).unwrap();
    let ai = config.spec.ai.unwrap_or_default();
    assert_eq!(ai.model, "claude-opus-5");
    assert_eq!(ai.api_key_env, "MY_CLAUDE_KEY");
}

#[test]
fn test_existing_config_without_ai_still_parses() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-workstation
spec:
  profile: work
  output:
    theme: default
"#;
    let config: CfgdConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.spec.ai.is_none());
}

#[test]
fn three_level_inheritance() {
    let dir = tempfile::tempdir().unwrap();
    let profiles = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();

    let grandparent = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: grandparent\nspec:\n  inherits: []\n  modules: []\n  env:\n    - name: A\n      value: '1'\n";
    let parent = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: parent\nspec:\n  inherits:\n    - grandparent\n  modules: []\n  env:\n    - name: B\n      value: '2'\n";
    let child = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: child\nspec:\n  inherits:\n    - parent\n  modules: []\n  env:\n    - name: C\n      value: '3'\n";

    std::fs::write(profiles.join("grandparent.yaml"), grandparent).unwrap();
    std::fs::write(profiles.join("parent.yaml"), parent).unwrap();
    std::fs::write(profiles.join("child.yaml"), child).unwrap();

    let resolved = resolve_profile("child", &profiles).unwrap();

    // Should have all three env vars merged
    let names: Vec<&str> = resolved
        .merged
        .env
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert!(names.contains(&"A"));
    assert!(names.contains(&"B"));
    assert!(names.contains(&"C"));
}

#[test]
fn resolved_profile_inherits_chain_is_nearest_parent_first() {
    let dir = tempfile::tempdir().unwrap();
    let profiles = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();

    let shared = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: shared\nspec:\n  inherits: []\n  modules: []\n";
    let core = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: core\nspec:\n  inherits:\n    - shared\n  modules: []\n";
    let base = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  inherits:\n    - core\n  modules: []\n";

    std::fs::write(profiles.join("shared.yaml"), shared).unwrap();
    std::fs::write(profiles.join("core.yaml"), core).unwrap();
    std::fs::write(profiles.join("base.yaml"), base).unwrap();

    let resolved = resolve_profile("base", &profiles).unwrap();

    assert_eq!(resolved.inherits_chain(), vec!["core", "shared"]);
}

#[test]
fn script_entry_deserialize_simple() {
    let yaml = r#""echo hello""#;
    let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
    match entry {
        ScriptEntry::Simple(s) => assert_eq!(s, "echo hello"),
        _ => panic!("expected Simple variant"),
    }
}

#[test]
fn script_entry_deserialize_full() {
    let yaml = r#"
run: scripts/check.sh
timeout: 30s
continueOnError: true
"#;
    let entry: ScriptEntry = serde_yaml::from_str(yaml).unwrap();
    match entry {
        ScriptEntry::Full(ScriptCommand {
            run,
            timeout,
            continue_on_error,
            ..
        }) => {
            assert_eq!(run, "scripts/check.sh");
            assert_eq!(timeout, Some("30s".to_string()));
            assert_eq!(continue_on_error, Some(true));
        }
        _ => panic!("expected Full variant"),
    }
}

#[test]
fn script_spec_deserialize_all_hooks() {
    let yaml = r#"
preApply:
  - scripts/pre.sh
postApply:
  - run: scripts/post.sh
    timeout: 60s
preReconcile:
  - scripts/reconcile-pre.sh
postReconcile:
  - scripts/reconcile-post.sh
onDrift:
  - scripts/drift.sh
onChange:
  - run: systemctl restart myservice
    continueOnError: true
"#;
    let spec: ScriptSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.pre_apply.len(), 1);
    assert_eq!(spec.post_apply.len(), 1);
    assert_eq!(spec.pre_reconcile.len(), 1);
    assert_eq!(spec.post_reconcile.len(), 1);
    assert_eq!(spec.on_drift.len(), 1);
    assert_eq!(spec.on_change.len(), 1);
}

#[test]
fn script_spec_backward_compat_empty() {
    let yaml = "{}";
    let spec: ScriptSpec = serde_yaml::from_str(yaml).unwrap();
    assert!(spec.pre_apply.is_empty());
    assert!(spec.post_apply.is_empty());
    assert!(spec.pre_reconcile.is_empty());
    assert!(spec.post_reconcile.is_empty());
    assert!(spec.on_drift.is_empty());
    assert!(spec.on_change.is_empty());
}

// --- Encryption types ---

#[test]
fn encryption_mode_default_is_in_repo() {
    let mode = EncryptionMode::default();
    assert_eq!(mode, EncryptionMode::InRepo);
}

#[test]
fn managed_file_spec_encryption_in_repo() {
    let yaml = r#"
source: dotfiles/.zshrc
target: ~/.zshrc
encryption:
  backend: sops
  mode: InRepo
"#;
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    let enc = spec.encryption.expect("encryption should be Some");
    assert_eq!(enc.backend, "sops");
    assert_eq!(enc.mode, EncryptionMode::InRepo);
}

#[test]
fn managed_file_spec_encryption_always() {
    let yaml = r#"
source: secrets/.env
target: ~/.env
encryption:
  backend: age
  mode: Always
"#;
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    let enc = spec.encryption.expect("encryption should be Some");
    assert_eq!(enc.backend, "age");
    assert_eq!(enc.mode, EncryptionMode::Always);
}

#[test]
fn managed_file_spec_no_encryption() {
    let yaml = r#"
source: dotfiles/.bashrc
target: ~/.bashrc
"#;
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    assert!(spec.encryption.is_none());
}

#[test]
fn managed_file_spec_permissions() {
    let yaml = r#"
source: dotfiles/.ssh/config
target: ~/.ssh/config
permissions: "600"
"#;
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.permissions.as_deref(), Some("600"));
}

#[test]
fn managed_file_spec_permissions_absent() {
    let yaml = r#"
source: dotfiles/.vimrc
target: ~/.vimrc
"#;
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    assert!(spec.permissions.is_none());
}

#[test]
fn source_constraints_encryption() {
    let yaml = r#"
noScripts: true
noSecretsRead: true
allowedTargetPaths: []
allowSystemChanges: false
requireSignedCommits: false
encryption:
  requiredTargets:
    - "~/.ssh/*"
    - "~/.gnupg/*"
  backend: sops
  mode: InRepo
"#;
    let sc: SourceConstraints = serde_yaml::from_str(yaml).unwrap();
    let enc = sc.encryption.expect("encryption should be Some");
    assert_eq!(enc.required_targets.len(), 2);
    assert_eq!(enc.required_targets[0], "~/.ssh/*");
    assert_eq!(enc.required_targets[1], "~/.gnupg/*");
    assert_eq!(enc.backend.as_deref(), Some("sops"));
    assert_eq!(enc.mode, Some(EncryptionMode::InRepo));
}

#[test]
fn source_constraints_no_encryption_defaults_none() {
    let sc = SourceConstraints::default();
    assert!(sc.encryption.is_none());
}

#[test]
fn source_constraints_encryption_required_targets_only() {
    let yaml = r#"
encryption:
  requiredTargets:
    - "~/.aws/credentials"
"#;
    let sc: SourceConstraints = serde_yaml::from_str(yaml).unwrap();
    let enc = sc.encryption.expect("encryption should be Some");
    assert_eq!(enc.required_targets.len(), 1);
    assert!(enc.backend.is_none());
    assert!(enc.mode.is_none());
}

#[test]
fn module_file_entry_with_encryption() {
    let yaml = r#"
source: files/.gitconfig
target: ~/.gitconfig
encryption:
  backend: sops
  mode: InRepo
"#;
    let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
    let enc = entry.encryption.expect("encryption should be Some");
    assert_eq!(enc.backend, "sops");
    assert_eq!(enc.mode, EncryptionMode::InRepo);
}

#[test]
fn module_file_entry_no_encryption() {
    let yaml = r#"
source: files/.tmux.conf
target: ~/.tmux.conf
"#;
    let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
    assert!(entry.encryption.is_none());
}

#[test]
fn module_file_entry_permissions() {
    let yaml = r#"
source: files/git-helper
target: ~/.local/bin/git-helper
permissions: "755"
"#;
    let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(entry.permissions.as_deref(), Some("755"));
}

#[test]
fn module_file_entry_permissions_absent() {
    let yaml = r#"
source: files/.tmux.conf
target: ~/.tmux.conf
"#;
    let entry: ModuleFileEntry = serde_yaml::from_str(yaml).unwrap();
    assert!(entry.permissions.is_none());
}

#[test]
fn encryption_spec_mode_defaults_to_in_repo_when_omitted() {
    let yaml = r#"
backend: sops
"#;
    let spec: EncryptionSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.backend, "sops");
    assert_eq!(spec.mode, EncryptionMode::InRepo);
}

#[test]
fn secret_spec_with_envs_only() {
    let yaml = r#"
source: op://vault/item/password
envs:
  - DB_PASSWORD
"#;
    let spec: SecretSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.source, "op://vault/item/password");
    assert!(spec.target.is_none());
    assert_eq!(spec.envs.as_ref().unwrap(), &["DB_PASSWORD"]);
}

#[test]
fn secret_spec_with_target_and_envs() {
    let yaml = r#"
source: secrets/api-key.enc
target: ~/.config/app/key
envs:
  - API_KEY
"#;
    let spec: SecretSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.source, "secrets/api-key.enc");
    assert_eq!(
        spec.target.unwrap(),
        std::path::PathBuf::from("~/.config/app/key")
    );
    assert_eq!(spec.envs.as_ref().unwrap(), &["API_KEY"]);
}

#[test]
fn secret_spec_with_target_only() {
    let yaml = r#"
source: secrets/credentials.enc
target: ~/.config/app/credentials
"#;
    let spec: SecretSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.source, "secrets/credentials.enc");
    assert_eq!(
        spec.target.unwrap(),
        std::path::PathBuf::from("~/.config/app/credentials")
    );
    assert!(spec.envs.is_none());
}

#[test]
fn secret_spec_neither_target_nor_envs_fails_validation() {
    let specs = vec![SecretSpec {
        source: "secrets/orphan.enc".to_string(),
        target: None,
        template: None,
        backend: None,
        envs: None,
    }];
    let result = validate_secret_specs(&specs);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("must have at least one of 'target' or 'envs'"),
        "unexpected error: {}",
        err_msg
    );
}

/// A `template` on a sops FILE source is refused: a decrypted file is content,
/// not a value, so there is nothing for `${secret:value}` to stand for.
#[test]
fn secret_spec_template_is_refused_on_an_encrypted_file_source() {
    let specs = vec![SecretSpec {
        source: "secrets/api-key.enc".to_string(),
        target: Some(std::path::PathBuf::from("~/.config/app/key")),
        template: Some("key: ${secret:value}".to_string()),
        backend: None,
        envs: None,
    }];
    let err = validate_secret_specs(&specs).unwrap_err().to_string();
    assert!(
        err.contains("'template' applies only to a provider reference"),
        "unexpected error: {err}"
    );
    assert!(
        err.contains("op://"),
        "the error names the accepted schemes: {err}"
    );
}

/// A `template` without the placeholder would write the template and drop
/// the secret on the floor; it is refused at validation instead.
#[test]
fn secret_spec_template_must_carry_the_value_placeholder() {
    let specs = vec![SecretSpec {
        source: "op://Work/GitHub/token".to_string(),
        target: Some(std::path::PathBuf::from("~/.config/gh/token")),
        template: Some("token: ${secret:token}".to_string()),
        backend: None,
        envs: None,
    }];
    let err = validate_secret_specs(&specs).unwrap_err().to_string();
    assert!(
        err.contains("must contain ${secret:value}"),
        "unexpected error: {err}"
    );

    let ok = vec![SecretSpec {
        template: Some("token: ${secret:value}".to_string()),
        ..specs.into_iter().next().unwrap()
    }];
    validate_secret_specs(&ok).expect("a provider reference with the placeholder validates");
}

#[test]
fn secret_spec_validation_passes_with_target() {
    let specs = vec![SecretSpec {
        source: "secrets/key.enc".to_string(),
        target: Some(std::path::PathBuf::from("~/.ssh/key")),
        template: None,
        backend: None,
        envs: None,
    }];
    validate_secret_specs(&specs).expect("validation should pass for spec with target");
    assert_eq!(specs[0].source, "secrets/key.enc");
    assert_eq!(
        specs[0].target.as_deref(),
        Some(std::path::Path::new("~/.ssh/key"))
    );
    assert!(specs[0].envs.is_none());
}

#[test]
fn secret_spec_validation_passes_with_envs() {
    let specs = vec![SecretSpec {
        source: "op://vault/item".to_string(),
        target: None,
        template: None,
        backend: None,
        envs: Some(vec!["SECRET_KEY".to_string()]),
    }];
    validate_secret_specs(&specs).expect("validation should pass for spec with envs");
    assert_eq!(specs[0].source, "op://vault/item");
    assert!(specs[0].target.is_none());
    let envs = specs[0].envs.as_ref().expect("envs should be Some");
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0], "SECRET_KEY");
}

#[test]
fn managed_file_spec_patch_ensure_parses_for_each_format() {
    for fmt in ["ini", "json", "yaml", "toml"] {
        let yaml = format!(
            "target: /tmp/settings.{fmt}\nstrategy: patch\npatch:\n  format: {fmt}\n  ensure:\n    General:\n      theme: dark\n"
        );
        let spec: ManagedFileSpec = serde_yaml::from_str(&yaml)
            .unwrap_or_else(|e| panic!("format {fmt} should parse: {e}"));
        assert_eq!(spec.strategy, Some(FileStrategy::Patch));
        let patch = spec.patch.as_ref().expect("patch block should be present");
        assert!(patch.ensure.is_some());
        assert!(patch.script.is_none());
        validate_managed_file_specs(std::slice::from_ref(&spec))
            .unwrap_or_else(|e| panic!("format {fmt} should validate: {e}"));
    }
}

#[test]
fn managed_file_spec_patch_script_parses() {
    let yaml = "target: ~/.zshrc\nstrategy: patch\npatch:\n  script: scripts/patch-zshrc.sh\n";
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.strategy, Some(FileStrategy::Patch));
    let patch = spec.patch.as_ref().expect("patch block should be present");
    assert!(patch.script.is_some());
    assert!(patch.ensure.is_none());
    assert_eq!(spec.source, "");
    validate_managed_file_specs(&[spec]).expect("script-mode patch should validate");
}

#[test]
fn managed_file_spec_patch_rejects_ensure_and_script_together() {
    let yaml = "target: /tmp/a.ini\nstrategy: patch\npatch:\n  ensure:\n    a: b\n  script: x.sh\n";
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    let err = validate_managed_file_specs(&[spec]).unwrap_err();
    assert!(
        err.to_string()
            .contains("exactly one of 'ensure' or 'script'")
    );
}

#[test]
fn managed_file_spec_patch_rejects_neither_ensure_nor_script() {
    let yaml = "target: /tmp/a.ini\nstrategy: patch\npatch: {}\n";
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    let err = validate_managed_file_specs(&[spec]).unwrap_err();
    assert!(
        err.to_string()
            .contains("exactly one of 'ensure' or 'script'")
    );
}

#[test]
fn managed_file_spec_patch_block_without_patch_strategy_rejected() {
    let yaml = "source: a\ntarget: /tmp/a.ini\nstrategy: copy\npatch:\n  ensure:\n    a: b\n";
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    let err = validate_managed_file_specs(&[spec]).unwrap_err();
    assert!(
        err.to_string()
            .contains("only valid when strategy is 'patch'")
    );
}

/// `encryption` and `private` both constrain the SOURCE file a strategy
/// deploys. `Patch` has no source, so honouring either is impossible — the
/// parser must reject the pair rather than silently ignore the flag.
#[test]
fn managed_file_spec_patch_rejects_encryption() {
    let yaml = "target: /tmp/a.ini\nstrategy: patch\npatch:\n  ensure:\n    a: b\nencryption:\n  backend: sops\n";
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    let err = validate_managed_file_specs(&[spec]).unwrap_err();
    assert!(
        err.to_string()
            .contains("'encryption' is not supported with strategy 'patch'"),
        "unexpected error: {err}"
    );
}

#[test]
fn managed_file_spec_patch_rejects_private() {
    let yaml = "target: /tmp/a.ini\nstrategy: patch\nprivate: true\npatch:\n  ensure:\n    a: b\n";
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    assert!(spec.private);
    let err = validate_managed_file_specs(&[spec]).unwrap_err();
    assert!(
        err.to_string()
            .contains("'private' is not supported with strategy 'patch'"),
        "unexpected error: {err}"
    );
}

#[test]
fn managed_file_spec_patch_strategy_without_patch_block_rejected() {
    let yaml = "target: /tmp/a.ini\nstrategy: patch\n";
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    let err = validate_managed_file_specs(&[spec]).unwrap_err();
    assert!(err.to_string().contains("requires a 'patch' block"));
}

#[test]
fn managed_file_spec_non_patch_strategy_requires_nonempty_source() {
    let yaml = "target: /tmp/a.ini\nstrategy: copy\n";
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.source, "");
    let err = validate_managed_file_specs(&[spec]).unwrap_err();
    assert!(
        err.to_string()
            .contains("'source' is required unless strategy is 'patch'")
    );
}

#[test]
fn managed_file_spec_default_strategy_also_requires_source() {
    // strategy omitted entirely (defaults to Symlink at plan time) still needs a source.
    let yaml = "target: /tmp/a.ini\n";
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    assert!(spec.strategy.is_none());
    let err = validate_managed_file_specs(&[spec]).unwrap_err();
    assert!(
        err.to_string()
            .contains("'source' is required unless strategy is 'patch'")
    );
}

#[test]
fn managed_file_spec_ordinary_copy_with_source_validates() {
    let yaml = "source: a\ntarget: /tmp/a.ini\nstrategy: copy\n";
    let spec: ManagedFileSpec = serde_yaml::from_str(yaml).unwrap();
    validate_managed_file_specs(&[spec]).expect("ordinary copy spec with a source should validate");
}

#[test]
fn policy_items_with_secrets() {
    let yaml = r#"
secrets:
  - source: op://vault/db/password
    envs:
      - DB_PASSWORD
  - source: secrets/tls.enc
    target: /etc/tls/cert.pem
"#;
    let items: PolicyItems = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(items.secrets.len(), 2);
    assert_eq!(items.secrets[0].source, "op://vault/db/password");
    assert!(items.secrets[0].target.is_none());
    assert_eq!(items.secrets[0].envs.as_ref().unwrap(), &["DB_PASSWORD"]);
    assert_eq!(items.secrets[1].source, "secrets/tls.enc");
    assert_eq!(
        items.secrets[1].target.as_ref().unwrap(),
        &std::path::PathBuf::from("/etc/tls/cert.pem")
    );
    assert!(items.secrets[1].envs.is_none());
}

#[test]
fn policy_items_default_has_empty_secrets() {
    let items = PolicyItems::default();
    assert!(items.secrets.is_empty());
}

#[test]
fn module_spec_system_field_deserializes() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: git-setup
spec:
  system:
    git:
      user.name: Jane Doe
      user.email: jane@example.com
    sshKeys:
      - path: ~/.ssh/id_ed25519.pub
        comment: jane@example.com
"#;
    let doc: ModuleDocument = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(doc.spec.system.len(), 2);
    assert!(doc.spec.system.contains_key("git"));
    assert!(doc.spec.system.contains_key("sshKeys"));
    let git_val = &doc.spec.system["git"];
    assert_eq!(
        git_val["user.name"],
        serde_yaml::Value::String("Jane Doe".into())
    );
    assert_eq!(
        git_val["user.email"],
        serde_yaml::Value::String("jane@example.com".into())
    );
}

#[test]
fn module_spec_system_defaults_to_empty() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
spec:
  packages:
    - name: neovim
"#;
    let doc: ModuleDocument = serde_yaml::from_str(yaml).unwrap();
    assert!(doc.spec.system.is_empty());
}

#[test]
fn parsed_system_settings_keep_key_order_across_parses() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: ordered
spec:
  system:
    sysctl:
      vm.swappiness: 10
    launchd:
      label: com.example.agent
    gsettings:
      org.gnome.desktop.interface: {}
    apparmor:
      profiles: []
    kubelet:
      maxPods: 110
"#;
    let first: ProfileDocument = serde_yaml::from_str(yaml).unwrap();
    let second: ProfileDocument = serde_yaml::from_str(yaml).unwrap();

    let keys: Vec<&str> = first.spec.system.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["apparmor", "gsettings", "kubelet", "launchd", "sysctl"],
        "declaration order must not survive into iteration order"
    );

    // The checkin payload serializes this map and the compliance snapshot hashes
    // an artifact built by walking it; two parses in one process that disagree
    // report a machine that changed when nothing did.
    assert_eq!(
        serde_yaml::to_string(&first.spec.system).unwrap(),
        serde_yaml::to_string(&second.spec.system).unwrap(),
    );
}

#[test]
fn module_system_merges_into_profile_system() {
    // Simulate what plan_system does: deep-merge module system over profile system.
    let profile_yaml = r#"
git:
  user.name: Old Name
  user.signingkey: ABC123
"#;
    let module_yaml = r#"
git:
  user.name: New Name
  user.email: new@example.com
"#;
    let mut profile_system: SystemSettings = serde_yaml::from_str(profile_yaml).unwrap();
    let module_system: SystemSettings = serde_yaml::from_str(module_yaml).unwrap();

    // Apply module system on top of profile system (same logic as plan_system)
    for (key, value) in &module_system {
        crate::deep_merge_yaml(
            profile_system
                .entry(key.clone())
                .or_insert(serde_yaml::Value::Null),
            value,
        );
    }

    let git = &profile_system["git"];
    // Module overrides: user.name updated
    assert_eq!(
        git["user.name"],
        serde_yaml::Value::String("New Name".into())
    );
    // Module adds: user.email
    assert_eq!(
        git["user.email"],
        serde_yaml::Value::String("new@example.com".into())
    );
    // Profile value preserved when module doesn't mention it
    assert_eq!(
        git["user.signingkey"],
        serde_yaml::Value::String("ABC123".into())
    );
}

#[test]
fn module_system_overrides_profile_on_conflict() {
    let mut profile_system: SystemSettings = {
        let mut m = SystemSettings::new();
        m.insert(
            "git".to_string(),
            serde_yaml::from_str("user.name: Profile Name").unwrap(),
        );
        m
    };
    let module_system: SystemSettings = {
        let mut m = SystemSettings::new();
        m.insert(
            "git".to_string(),
            serde_yaml::from_str("user.name: Module Name").unwrap(),
        );
        m
    };

    for (key, value) in &module_system {
        crate::deep_merge_yaml(
            profile_system
                .entry(key.clone())
                .or_insert(serde_yaml::Value::Null),
            value,
        );
    }

    assert_eq!(
        profile_system["git"]["user.name"],
        serde_yaml::Value::String("Module Name".into())
    );
}

// ---------------------------------------------------------------------------
// ComplianceConfig tests
// ---------------------------------------------------------------------------

#[test]
fn parse_full_compliance_config() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  compliance:
    enabled: true
    interval: 30m
    retention: 90d
    scope:
      files: true
      packages: false
      system: true
      secrets: false
      watchPaths:
        - /etc
        - /usr/local
      watchPackageManagers:
        - brew
        - apt
    export:
      format: Yaml
      path: /var/lib/cfgd/compliance/
"#;
    let config = parse_config(yaml, Path::new("cfgd.yaml")).unwrap();
    let compliance = config.spec.compliance.as_ref().unwrap();
    assert!(compliance.enabled);
    assert_eq!(compliance.interval, "30m");
    assert_eq!(compliance.retention, "90d");
    assert!(compliance.scope.files);
    assert!(!compliance.scope.packages);
    assert!(compliance.scope.system);
    assert!(!compliance.scope.secrets);
    assert_eq!(compliance.scope.watch_paths, vec!["/etc", "/usr/local"]);
    assert_eq!(compliance.scope.watch_package_managers, vec!["brew", "apt"]);
    assert_eq!(compliance.export.format, ComplianceFormat::Yaml);
    assert_eq!(compliance.export.path, "/var/lib/cfgd/compliance/");
}

#[test]
fn parse_compliance_defaults_from_enabled_only() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  compliance:
    enabled: true
"#;
    let config = parse_config(yaml, Path::new("cfgd.yaml")).unwrap();
    let compliance = config.spec.compliance.as_ref().unwrap();
    assert!(compliance.enabled);
    assert_eq!(compliance.interval, "1h");
    assert_eq!(compliance.retention, "30d");
    // scope bools default to true
    assert!(compliance.scope.files);
    assert!(compliance.scope.packages);
    assert!(compliance.scope.system);
    assert!(compliance.scope.secrets);
    assert!(compliance.scope.watch_paths.is_empty());
    assert!(compliance.scope.watch_package_managers.is_empty());
    // export defaults
    assert_eq!(compliance.export.format, ComplianceFormat::Json);
    assert_eq!(compliance.export.path, "~/.local/state/cfgd/compliance/");
}

#[test]
fn parse_compliance_watch_paths_and_managers() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  compliance:
    enabled: true
    scope:
      watchPaths:
        - /home/user/.config
      watchPackageManagers:
        - cargo
        - npm
"#;
    let config = parse_config(yaml, Path::new("cfgd.yaml")).unwrap();
    let scope = &config.spec.compliance.as_ref().unwrap().scope;
    assert_eq!(scope.watch_paths, vec!["/home/user/.config"]);
    assert_eq!(scope.watch_package_managers, vec!["cargo", "npm"]);
}

#[test]
fn compliance_format_defaults_to_json() {
    let export = ComplianceExport::default();
    assert_eq!(export.format, ComplianceFormat::Json);
}

#[test]
fn parse_complete_cfgd_yaml_with_compliance() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: workstation
spec:
  profile: default
  compliance:
    enabled: true
    interval: 1h
    retention: 30d
    scope:
      files: true
      packages: true
      system: true
      secrets: true
    export:
      format: Json
      path: ~/.local/state/cfgd/compliance/
"#;
    let config = parse_config(yaml, Path::new("cfgd.yaml")).unwrap();
    assert_eq!(config.spec.profile.as_deref(), Some("default"));
    let compliance = config.spec.compliance.as_ref().unwrap();
    assert!(compliance.enabled);
    assert_eq!(compliance.interval, "1h");
    assert_eq!(compliance.retention, "30d");
    assert!(compliance.scope.files);
    assert!(compliance.scope.packages);
    assert!(compliance.scope.system);
    assert!(compliance.scope.secrets);
    assert_eq!(compliance.export.format, ComplianceFormat::Json);
    assert_eq!(compliance.export.path, "~/.local/state/cfgd/compliance/");
}

#[test]
fn compliance_absent_when_not_specified() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
"#;
    let config = parse_config(yaml, Path::new("cfgd.yaml")).unwrap();
    assert!(config.spec.compliance.is_none());
}

#[test]
fn yaml_anchor_limit_rejects_bomb() {
    // Generate a YAML document with excessive anchors
    let mut yaml = String::from("apiVersion: cfgd.io/v1alpha1\nkind: Config\n");
    for i in 0..300 {
        yaml.push_str(&format!("key{}: &anchor{} value{}\n", i, i, i));
    }
    let result = parse_config(&yaml, std::path::Path::new("bomb.yaml"));
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("too many YAML anchors")
    );
}

#[test]
fn yaml_anchor_limit_accepts_normal_config() {
    // A normal config with a few anchors should be fine
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
"#;
    let result = parse_config(yaml, std::path::Path::new("cfgd.yaml"));
    assert!(
        result.is_ok(),
        "normal config should parse: {:?}",
        result.err()
    );
    let config = result.unwrap();
    assert_eq!(config.metadata.name, "test");
    assert_eq!(config.spec.profile.as_deref(), Some("default"));
    assert_eq!(config.api_version, "cfgd.io/v1alpha1");
    assert_eq!(config.kind, "Config");
    assert!(config.spec.origin.is_empty(), "no origins configured");
    assert!(config.spec.sources.is_empty(), "no sources configured");
}

#[test]
fn env_var_rejects_invalid_name_at_deserialization() {
    let yaml = r#"
name: "MY_VAR; rm -rf /"
value: "safe"
"#;
    let result: std::result::Result<EnvVar, _> = serde_yaml::from_str(yaml);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("invalid env var name")
    );
}

#[test]
fn env_var_accepts_valid_name_at_deserialization() {
    let yaml = r#"
name: "MY_VAR_123"
value: "hello"
"#;
    let ev: EnvVar = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(ev.name, "MY_VAR_123");
    assert_eq!(ev.value, "hello");
}

#[test]
fn alias_rejects_invalid_name_at_deserialization() {
    let yaml = r#"
name: "my alias; evil"
command: "ls -la"
"#;
    let result: std::result::Result<ShellAlias, _> = serde_yaml::from_str(yaml);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("invalid alias name")
    );
}

#[test]
fn alias_accepts_valid_name_at_deserialization() {
    let yaml = r#"
name: "ll"
command: "ls -la"
"#;
    let alias: ShellAlias = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(alias.name, "ll");
    assert_eq!(alias.command, "ls -la");
}

// --- Legacy theme-override key warnings ---

#[test]
fn legacy_theme_subheader_emits_warning() {
    let yaml = r##"
spec:
  theme:
    name: dracula
    overrides:
      subheader: "#ff79c6"
"##;
    let messages = super::parse::warn_on_legacy_theme_keys(yaml);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("theme.overrides.subheader is no longer supported")),
        "expected legacy-key warning to fire, got: {messages:?}"
    );
}

#[test]
fn legacy_icon_success_emits_rename_warning() {
    let yaml = r##"
spec:
  theme:
    name: dracula
    overrides:
      iconSuccess: "++"
"##;
    let messages = super::parse::warn_on_legacy_theme_keys(yaml);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("iconSuccess is renamed to iconOk")),
        "expected rename warning to fire, got: {messages:?}"
    );
}

#[test]
fn a_live_icon_info_override_is_never_called_unsupported() {
    // `iconInfo` sat in REMOVED_THEME_KEYS while the field it names was live,
    // so declaring it produced both a working override and a notice saying the
    // override would be ignored. The structural guard below pins the lists; this
    // pins the symptom the user actually met.
    let yaml = r##"
spec:
  theme:
    name: dracula
    overrides:
      iconInfo: "i"
"##;
    let messages = super::parse::warn_on_legacy_theme_keys(yaml);
    assert!(
        messages.is_empty(),
        "iconInfo is a live ThemeOverrides field; it must draw no deprecation, got: {messages:?}"
    );
}

#[test]
fn modern_overrides_emit_no_warning() {
    let yaml = r##"
spec:
  theme:
    name: dracula
    overrides:
      iconOk: "✔"
      running: "#00ff00"
"##;
    let messages = super::parse::warn_on_legacy_theme_keys(yaml);
    assert!(
        messages.is_empty(),
        "expected no deprecations, got: {messages:?}"
    );
}

/// `REMOVED_THEME_KEYS` and `RENAMED_THEME_KEYS` are hand-maintained strings
/// describing `ThemeOverrides`, not values read off the struct — nothing stops
/// them drifting the moment a field they name is re-added, renamed again, or
/// dropped. This derives the struct's live field set from its own `schemars`
/// schema (never restating the fields by hand) and cross-checks both lists
/// against it, so a rename or a field coming back under an old name fails here
/// instead of silently mis-warning users forever.
#[test]
fn legacy_theme_key_lists_stay_consistent_with_theme_overrides_schema() {
    let schema = serde_json::to_value(schemars::schema_for!(super::ThemeOverrides))
        .expect("ThemeOverrides schema serializes to a Value");
    let live_fields: std::collections::BTreeSet<&str> = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("ThemeOverrides schema carries a properties object")
        .keys()
        .map(String::as_str)
        .collect();

    for old in super::parse::REMOVED_THEME_KEYS {
        assert!(
            !live_fields.contains(old),
            "REMOVED_THEME_KEYS names '{old}', but ThemeOverrides has a live field \
             called '{old}' — the field came back and the deprecation notice is now a \
             false alarm; drop it from REMOVED_THEME_KEYS"
        );
    }
    for (old, new) in super::parse::RENAMED_THEME_KEYS {
        assert!(
            !live_fields.contains(old),
            "RENAMED_THEME_KEYS names '{old}' as a legacy spelling, but ThemeOverrides \
             has a live field called '{old}' — the field came back under its old name"
        );
        assert!(
            live_fields.contains(new),
            "RENAMED_THEME_KEYS points '{old}' at '{new}', but ThemeOverrides has no \
             live field called '{new}' — the rename target itself has since moved"
        );
    }
}

/// `LEGACY_OUTPUT_KEYS` is a hand-written table of dotted paths, not values
/// read off either struct. This derives both live field sets from their own
/// `schemars` schemas, so an entry stops matching the code the moment a flat
/// key comes back onto `ConfigSpec` or a nested path moves off `OutputConfig`.
#[test]
fn legacy_output_key_lists_stay_consistent_with_the_config_schema() {
    fn properties_of(schema: &serde_json::Value) -> std::collections::BTreeSet<&str> {
        schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("schema carries a properties object")
            .keys()
            .map(String::as_str)
            .collect()
    }

    let spec_schema = serde_json::to_value(schemars::schema_for!(super::ConfigSpec))
        .expect("ConfigSpec schema serializes to a Value");
    let spec_fields = properties_of(&spec_schema);
    let output_schema = serde_json::to_value(schemars::schema_for!(super::OutputConfig))
        .expect("OutputConfig schema serializes to a Value");
    let output_fields = properties_of(&output_schema);

    assert!(
        spec_fields.contains("output"),
        "every entry replaces a flat key with a `spec.output.*` path, but ConfigSpec \
         has no `output` field"
    );
    for (old, new) in super::parse::LEGACY_OUTPUT_KEYS {
        let flat = old
            .strip_prefix("spec.")
            .expect("a legacy output key is spec-rooted");
        assert!(
            !spec_fields.contains(flat),
            "LEGACY_OUTPUT_KEYS calls '{old}' legacy, but ConfigSpec has a live field \
             called '{flat}' — the flat key came back and the deprecation is a false alarm"
        );
        let nested = new
            .strip_prefix("spec.output.")
            .unwrap_or_else(|| panic!("'{new}' must name a key under spec.output"));
        assert!(
            output_fields.contains(nested),
            "LEGACY_OUTPUT_KEYS points '{old}' at '{new}', but OutputConfig has no live \
             field called '{nested}' — the rename target itself has moved"
        );
    }
}

/// A document still carrying the flat keys loads: each folds into
/// `spec.output`, each is reported once as a deprecation naming its
/// replacement, and each is listed for `cfgd doctor` to render as a row.
#[test]
fn a_flat_presentation_key_folds_into_the_output_block_and_deprecates() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: legacy
spec:
  theme: dracula
  usageHints: false
"#;
    let cfg = super::parse_config(yaml, std::path::Path::new("cfgd.yaml")).expect("parses");
    assert_eq!(
        cfg.spec.theme().map(|t| t.name.as_str()),
        Some("dracula"),
        "the flat theme must be readable at spec.output.theme"
    );
    assert_eq!(cfg.spec.usage_hints(), Some(false));
    assert_eq!(
        cfg.legacy_output_keys,
        vec!["spec.theme".to_string(), "spec.usageHints".to_string()]
    );
    for (old, new) in super::parse::LEGACY_OUTPUT_KEYS {
        let reported = cfg
            .deprecations
            .iter()
            .find(|d| d.contains(old) && d.contains(new))
            .unwrap_or_else(|| {
                panic!(
                    "expected a deprecation naming {old} and {new}, got: {:?}",
                    cfg.deprecations
                )
            });
        // A deprecation tells the reader what to do now. A release it names no
        // number for, and a "for now" the reader cannot date, are both a
        // schedule cfgd does not keep.
        for hedge in ["for now", "future release", "will be removed"] {
            assert!(
                !reported.contains(hedge),
                "the deprecation for {old} dates itself against nothing: {reported}"
            );
        }
        assert!(
            reported.contains("Move the key"),
            "the deprecation for {old} must say what to do: {reported}"
        );
    }
}

/// Both spellings written, the nested one wins and the flat one is reported as
/// ignored rather than as a plain move.
#[test]
fn the_nested_presentation_key_wins_over_the_flat_one_it_duplicates() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: both
spec:
  theme: dracula
  output:
    theme: nord
"#;
    let cfg = super::parse_config(yaml, std::path::Path::new("cfgd.yaml")).expect("parses");
    assert_eq!(cfg.spec.theme().map(|t| t.name.as_str()), Some("nord"));
    assert!(
        cfg.deprecations
            .iter()
            .any(|d| d.contains("both set") && d.contains("spec.output.theme")),
        "expected the both-set deprecation, got: {:?}",
        cfg.deprecations
    );
}

/// The nested block alone is the quiet path: nothing deprecated, nothing for
/// `doctor` to report.
#[test]
fn the_nested_output_block_alone_reports_no_legacy_key() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: modern
spec:
  output:
    theme: nord
    usageHints: false
    maskEnvValues: none
"#;
    let cfg = super::parse_config(yaml, std::path::Path::new("cfgd.yaml")).expect("parses");
    assert_eq!(cfg.spec.theme().map(|t| t.name.as_str()), Some("nord"));
    assert_eq!(cfg.spec.usage_hints(), Some(false));
    assert_eq!(
        cfg.spec.mask_env_values(),
        Some(super::MaskEnvValues::None),
        "maskEnvValues has no flat spelling and reads only from the nested block"
    );
    assert!(cfg.legacy_output_keys.is_empty());
    assert!(
        cfg.deprecations.is_empty(),
        "the nested block deprecates nothing, got: {:?}",
        cfg.deprecations
    );
}

/// Resolve the primary package list a bare-list form should populate, for a
/// given manager field name, so the table test can assert both forms agree.
fn primary_list_for(spec: &PackagesSpec, manager: &str) -> Vec<String> {
    match manager {
        "brew" => spec.brew.as_ref().map(|b| b.formulae.clone()),
        "apt" => spec.apt.as_ref().map(|a| a.packages.clone()),
        "cargo" => spec.cargo.as_ref().map(|c| c.packages.clone()),
        "npm" => spec.npm.as_ref().map(|n| n.global.clone()),
        "snap" => spec.snap.as_ref().map(|s| s.packages.clone()),
        "flatpak" => spec.flatpak.as_ref().map(|f| f.packages.clone()),
        other => spec.simple_list(other).map(|s| s.to_vec()),
    }
    .unwrap_or_default()
}

/// Every manager — the 12 bare-`Vec<String>` ones and the 6 struct-backed ones
/// — must accept BOTH `manager: [a, b]` and `manager: {packages|global|formulae: [a, b]}`
/// and resolve to the identical package set. This makes the historical
/// three-shapes inconsistency unrepresentable: no manager may accept only one form.
#[test]
fn every_manager_accepts_list_and_struct_forms_identically() {
    // (field, struct-key-for-the-primary-list). Struct key is None for the
    // bare-Vec managers (their map form uses `packages:`).
    let struct_managers: &[(&str, &str)] = &[
        ("brew", "formulae"),
        ("apt", "packages"),
        ("cargo", "packages"),
        ("npm", "global"),
        ("snap", "packages"),
        ("flatpak", "packages"),
    ];
    let bare_managers: &[&str] = &[
        "pipx",
        "dnf",
        "apk",
        "pacman",
        "zypper",
        "yum",
        "pkg",
        "nix",
        "go",
        "winget",
        "chocolatey",
        "scoop",
    ];

    for (field, key) in struct_managers {
        let list_yaml = format!("{field}: [alpha, beta]\n");
        let struct_yaml = format!("{field}:\n  {key}: [alpha, beta]\n");
        let from_list: PackagesSpec = serde_yaml::from_str(&list_yaml)
            .unwrap_or_else(|e| panic!("{field} list form must parse: {e}"));
        let from_struct: PackagesSpec = serde_yaml::from_str(&struct_yaml)
            .unwrap_or_else(|e| panic!("{field} struct form must parse: {e}"));
        assert_eq!(
            primary_list_for(&from_list, field),
            vec!["alpha".to_string(), "beta".to_string()],
            "{field} list form resolved wrong set"
        );
        assert_eq!(
            primary_list_for(&from_list, field),
            primary_list_for(&from_struct, field),
            "{field} list and struct forms disagree"
        );
    }

    for field in bare_managers {
        let list_yaml = format!("{field}: [alpha, beta]\n");
        let struct_yaml = format!("{field}:\n  packages: [alpha, beta]\n");
        let from_list: PackagesSpec = serde_yaml::from_str(&list_yaml)
            .unwrap_or_else(|e| panic!("{field} list form must parse: {e}"));
        let from_struct: PackagesSpec = serde_yaml::from_str(&struct_yaml)
            .unwrap_or_else(|e| panic!("{field} struct (packages:) form must parse: {e}"));
        assert_eq!(
            primary_list_for(&from_list, field),
            vec!["alpha".to_string(), "beta".to_string()],
            "{field} list form resolved wrong set"
        );
        assert_eq!(
            primary_list_for(&from_list, field),
            primary_list_for(&from_struct, field),
            "{field} list and struct forms disagree"
        );
    }
}

/// `ALL_MANAGER_NAMES` must list exactly the names `desired_packages_for_spec`
/// handles via a dedicated match arm — never a name that silently falls through
/// to the custom-manager catch-all. A built-in name placed in `custom` must
/// resolve via its built-in arm (empty here, since the built-in field is empty),
/// NOT return the custom packages; an unknown name must hit the catch-all and
/// return the custom packages. This keeps the const and the match from drifting:
/// if `/add-package-manager` adds a name to the const without a match arm, the
/// built-in lookup would fall through and this test flips red.
#[test]
fn all_manager_names_resolve_via_builtin_arms() {
    for name in ALL_MANAGER_NAMES {
        let spec = PackagesSpec {
            custom: vec![custom_manager(name, "shadow")],
            ..Default::default()
        };
        let resolved = desired_packages_for_spec(name, &spec);
        assert!(
            resolved.is_empty(),
            "'{name}' is in ALL_MANAGER_NAMES but resolved to the custom-manager \
             fallthrough ({resolved:?}) — it has no built-in match arm in \
             desired_packages_for_spec"
        );
    }

    // Negative control: an unknown name must fall through to the custom lookup.
    let spec = PackagesSpec {
        custom: vec![custom_manager("definitely-not-a-builtin", "pkg-a")],
        ..Default::default()
    };
    assert_eq!(
        desired_packages_for_spec("definitely-not-a-builtin", &spec),
        vec!["pkg-a".to_string()],
        "unknown manager names must resolve via the custom-manager catch-all"
    );
}

/// Minimal `CustomManagerSpec` with one package, for the manager-name drift guard.
fn custom_manager(name: &str, package: &str) -> CustomManagerSpec {
    CustomManagerSpec {
        name: name.to_string(),
        check: String::new(),
        list_installed: String::new(),
        install: String::new(),
        uninstall: String::new(),
        update: None,
        packages: vec![package.to_string()],
    }
}

/// The reported root-cause symptom: `flatpak: [app]` must now parse where it
/// previously errored with `invalid type: sequence, expected struct FlatpakSpec`.
#[test]
fn flatpak_accepts_bare_list_form() {
    let spec: PackagesSpec = serde_yaml::from_str("flatpak: [org.gnome.Calculator]\n")
        .expect("flatpak bare-list form must parse");
    assert_eq!(
        spec.flatpak.unwrap().packages,
        vec!["org.gnome.Calculator".to_string()]
    );
}

/// A bare-Vec manager given a map form with an unknown key must still error
/// (the map form preserves typo-detection; only `packages:` is accepted).
#[test]
fn bare_vec_manager_map_form_rejects_unknown_key() {
    let err = serde_yaml::from_str::<PackagesSpec>("nix:\n  pakages: [hello]\n")
        .expect_err("unknown key in nix map form must error");
    assert!(
        format!("{err}").contains("packages") || format!("{err}").contains("pakages"),
        "expected an unknown-key error, got: {err}"
    );
}

/// The struct-backed managers keep `deny_unknown_fields` on their map form.
#[test]
fn struct_manager_map_form_rejects_unknown_key() {
    let err = serde_yaml::from_str::<PackagesSpec>("flatpak:\n  packges: [x]\n")
        .expect_err("unknown key in flatpak map form must error");
    assert!(
        format!("{err}").contains("unknown field"),
        "expected unknown-field error, got: {err}"
    );
}

// --- Profile bundle layout: dual-read precedence + ambiguity ---

fn profile_yaml(name: &str, inherits: &[&str]) -> String {
    let inherits_block = if inherits.is_empty() {
        String::new()
    } else {
        let items: Vec<String> = inherits.iter().map(|p| format!("    - {}", p)).collect();
        format!("  inherits:\n{}\n", items.join("\n"))
    };
    format!(
        r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: {name}
spec:
{inherits_block}  env:
    - name: origin_{name}
      value: "1"
"#
    )
}

fn write_canonical_profile(profiles_dir: &Path, name: &str, inherits: &[&str]) -> PathBuf {
    let dir = profiles_dir.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("profile.yaml");
    std::fs::write(&path, profile_yaml(name, inherits)).unwrap();
    path
}

fn write_legacy_profile(profiles_dir: &Path, filename: &str, name: &str) -> PathBuf {
    let path = profiles_dir.join(filename);
    std::fs::write(&path, profile_yaml(name, &[])).unwrap();
    path
}

#[test]
fn find_profile_path_canonical_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_canonical_profile(dir.path(), "work", &[]);
    assert_eq!(find_profile_path(dir.path(), "work").unwrap(), path);
}

#[test]
fn find_profile_path_legacy_yaml_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_legacy_profile(dir.path(), "work.yaml", "work");
    assert_eq!(find_profile_path(dir.path(), "work").unwrap(), path);
}

#[test]
fn find_profile_path_legacy_yml_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_legacy_profile(dir.path(), "work.yml", "work");
    assert_eq!(find_profile_path(dir.path(), "work").unwrap(), path);
}

#[test]
fn find_profile_path_missing_is_profile_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let err = find_profile_path(dir.path(), "work").unwrap_err();
    assert!(matches!(err, ConfigError::ProfileNotFound { ref name } if name == "work"));
}

#[test]
fn find_profile_path_payload_dir_without_manifest_is_not_a_profile() {
    let dir = tempfile::tempdir().unwrap();
    // The current legacy shape: flat manifest + <name>/files/ payload dir.
    std::fs::create_dir_all(dir.path().join("work").join("files")).unwrap();
    let legacy = write_legacy_profile(dir.path(), "work.yaml", "work");
    assert_eq!(find_profile_path(dir.path(), "work").unwrap(), legacy);
}

#[test]
fn find_profile_path_canonical_plus_legacy_yaml_is_ambiguous() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let legacy = write_legacy_profile(dir.path(), "work.yaml", "work");
    let err = find_profile_path(dir.path(), "work").unwrap_err();
    assert!(matches!(err, ConfigError::AmbiguousProfile { .. }));
    let msg = err.to_string();
    assert!(msg.contains(&canonical.posix().to_string()), "{msg}");
    assert!(msg.contains(&legacy.posix().to_string()), "{msg}");
    assert!(msg.contains("delete or rename one"), "{msg}");
}

#[test]
fn find_profile_path_canonical_plus_legacy_yml_is_ambiguous() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let legacy = write_legacy_profile(dir.path(), "work.yml", "work");
    let err = find_profile_path(dir.path(), "work").unwrap_err();
    assert!(matches!(err, ConfigError::AmbiguousProfile { .. }), "{err}");
    let msg = err.to_string();
    assert!(msg.contains(&canonical.posix().to_string()), "{msg}");
    assert!(msg.contains(&legacy.posix().to_string()), "{msg}");
    assert!(msg.contains("delete or rename one"), "{msg}");
}

#[test]
fn find_profile_path_legacy_yaml_plus_yml_is_ambiguous() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = write_legacy_profile(dir.path(), "work.yaml", "work");
    let yml = write_legacy_profile(dir.path(), "work.yml", "work");
    let err = find_profile_path(dir.path(), "work").unwrap_err();
    assert!(matches!(err, ConfigError::AmbiguousProfile { .. }));
    let msg = err.to_string();
    assert!(msg.contains(&yaml.posix().to_string()), "{msg}");
    assert!(msg.contains(&yml.posix().to_string()), "{msg}");
    assert!(msg.contains("delete or rename one"), "{msg}");
}

#[test]
fn find_profile_path_three_forms_names_every_path() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let yaml = write_legacy_profile(dir.path(), "work.yaml", "work");
    let yml = write_legacy_profile(dir.path(), "work.yml", "work");
    let err = find_profile_path(dir.path(), "work").unwrap_err();
    match &err {
        ConfigError::AmbiguousProfile { name, paths } => {
            assert_eq!(name, "work");
            assert_eq!(paths, &vec![canonical.clone(), yaml.clone(), yml.clone()]);
        }
        other => panic!("expected AmbiguousProfile, got {other}"),
    }
    let msg = err.to_string();
    assert!(msg.contains(&canonical.posix().to_string()), "{msg}");
    assert!(msg.contains(&yaml.posix().to_string()), "{msg}");
    assert!(msg.contains(&yml.posix().to_string()), "{msg}");
    assert!(msg.contains("delete or rename one"), "{msg}");
}

#[test]
fn find_profile_path_profile_named_profile_ranks_structurally() {
    // a bundle at profiles/profile/profile.yaml vs a flat profiles/profile.yaml:
    // canonical detection is structural (parent dir), not name-based, so the
    // pathological name 'profile' must still rank the bundle form first
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "profile", &[]);
    let flat = write_legacy_profile(dir.path(), "profile.yaml", "profile");
    let err = find_profile_path(dir.path(), "profile").unwrap_err();
    match &err {
        ConfigError::AmbiguousProfile { name, paths } => {
            assert_eq!(name, "profile");
            assert_eq!(paths, &vec![canonical, flat]);
        }
        other => panic!("expected AmbiguousProfile, got {other}"),
    }
}

#[test]
fn canonical_profile_path_is_pure_construction() {
    let path = canonical_profile_path(Path::new("/cfg/profiles"), "work");
    assert_eq!(path, Path::new("/cfg/profiles/work/profile.yaml"));
}

#[test]
fn resolve_profile_loads_canonical_form() {
    let dir = tempfile::tempdir().unwrap();
    write_canonical_profile(dir.path(), "work", &[]);
    let resolved = resolve_profile("work", dir.path()).unwrap();
    assert_eq!(resolved.layers.len(), 1);
    assert_eq!(resolved.layers[0].profile_name, "work");
}

#[test]
fn resolve_profile_inherits_across_both_forms() {
    let dir = tempfile::tempdir().unwrap();
    // legacy child inheriting a canonical parent
    write_canonical_profile(dir.path(), "base", &[]);
    std::fs::write(
        dir.path().join("work.yaml"),
        profile_yaml("work", &["base"]),
    )
    .unwrap();
    let resolved = resolve_profile("work", dir.path()).unwrap();
    assert_eq!(resolved.layers.len(), 2);
    assert_eq!(resolved.layers[0].profile_name, "base");
    assert_eq!(resolved.layers[1].profile_name, "work");

    // canonical child inheriting a legacy parent
    let dir2 = tempfile::tempdir().unwrap();
    write_legacy_profile(dir2.path(), "base.yaml", "base");
    write_canonical_profile(dir2.path(), "work", &["base"]);
    let resolved = resolve_profile("work", dir2.path()).unwrap();
    assert_eq!(resolved.layers.len(), 2);
    assert_eq!(resolved.layers[0].profile_name, "base");
    assert_eq!(resolved.layers[1].profile_name, "work");
}

#[test]
fn resolve_profile_ambiguous_parent_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    write_canonical_profile(dir.path(), "base", &[]);
    write_legacy_profile(dir.path(), "base.yaml", "base");
    std::fs::write(
        dir.path().join("work.yaml"),
        profile_yaml("work", &["base"]),
    )
    .unwrap();
    let err = resolve_profile("work", dir.path()).unwrap_err();
    assert!(err.to_string().contains("ambiguous profile 'base'"));
}

#[test]
fn scan_profiles_ambiguous_name_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    write_canonical_profile(dir.path(), "work", &[]);
    write_legacy_profile(dir.path(), "work.yaml", "work");
    let err = scan_profiles(dir.path()).unwrap_err();
    assert!(matches!(err, ConfigError::AmbiguousProfile { ref name, .. } if name == "work"));
    assert!(err.to_string().contains("delete or rename one"));
}

#[test]
fn scan_profiles_legacy_extension_dupe_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    write_legacy_profile(dir.path(), "work.yaml", "work");
    write_legacy_profile(dir.path(), "work.yml", "work");
    let err = scan_profiles(dir.path()).unwrap_err();
    assert!(matches!(err, ConfigError::AmbiguousProfile { ref name, .. } if name == "work"));
}

#[test]
fn scan_profiles_ambiguity_paths_ordered_canonical_first() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let legacy = write_legacy_profile(dir.path(), "work.yaml", "work");
    let err = scan_profiles(dir.path()).unwrap_err();
    match err {
        ConfigError::AmbiguousProfile { paths, .. } => {
            assert_eq!(paths, vec![canonical, legacy]);
        }
        other => panic!("expected AmbiguousProfile, got {other}"),
    }
}

#[test]
fn scan_profiles_ignores_uppercase_yaml_extension() {
    let dir = tempfile::tempdir().unwrap();
    write_legacy_profile(dir.path(), "work.YAML", "work");
    assert!(scan_profiles(dir.path()).unwrap().is_empty());
}

#[test]
fn scan_profiles_tolerant_yields_both_forms_sorted() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = write_legacy_profile(dir.path(), "zeta.yml", "zeta");
    let canonical = write_canonical_profile(dir.path(), "alpha", &[]);
    std::fs::create_dir_all(dir.path().join("zeta-payload").join("files")).unwrap();
    std::fs::write(dir.path().join("README.md"), "x").unwrap();

    let entries = scan_profiles_tolerant(dir.path()).unwrap();
    assert_eq!(entries.len(), 2);
    let ProfileScanEntry::Found(a) = &entries[0] else {
        panic!("alpha should be unambiguous: {:?}", entries[0]);
    };
    assert_eq!(a.name, "alpha");
    assert_eq!(a.form, ProfileForm::Canonical);
    assert_eq!(a.path, canonical);
    let ProfileScanEntry::Found(z) = &entries[1] else {
        panic!("zeta should be unambiguous: {:?}", entries[1]);
    };
    assert_eq!(z.name, "zeta");
    assert_eq!(z.form, ProfileForm::LegacyFlat);
    assert_eq!(z.path, legacy);
}

#[test]
fn scan_profiles_tolerant_carries_ambiguity_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let legacy = write_legacy_profile(dir.path(), "work.yaml", "work");
    write_legacy_profile(dir.path(), "solo.yaml", "solo");

    let entries = scan_profiles_tolerant(dir.path()).unwrap();
    assert_eq!(entries.len(), 2);
    let ProfileScanEntry::Found(solo) = &entries[0] else {
        panic!("solo should be unambiguous: {:?}", entries[0]);
    };
    assert_eq!(solo.name, "solo");
    let ProfileScanEntry::Ambiguous { name, paths, error } = &entries[1] else {
        panic!("work should be ambiguous: {:?}", entries[1]);
    };
    assert_eq!(name, "work");
    assert_eq!(paths, &vec![canonical.clone(), legacy.clone()]);
    match error {
        ConfigError::AmbiguousProfile { name, paths } => {
            assert_eq!(name, "work");
            assert_eq!(paths, &vec![canonical, legacy]);
        }
        other => panic!("expected AmbiguousProfile, got {other}"),
    }
}

#[test]
fn scan_profiles_tolerant_three_forms_still_one_ambiguous_entry() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let yaml = write_legacy_profile(dir.path(), "work.yaml", "work");
    let yml = write_legacy_profile(dir.path(), "work.yml", "work");

    let entries = scan_profiles_tolerant(dir.path()).unwrap();
    assert_eq!(entries.len(), 1);
    let ProfileScanEntry::Ambiguous { name, paths, error } = &entries[0] else {
        panic!("work should be ambiguous: {:?}", entries[0]);
    };
    assert_eq!(name, "work");
    assert_eq!(paths, &vec![canonical.clone(), yaml.clone(), yml.clone()]);
    let msg = error.to_string();
    assert!(msg.contains(&canonical.posix().to_string()), "{msg}");
    assert!(msg.contains(&yaml.posix().to_string()), "{msg}");
    assert!(msg.contains(&yml.posix().to_string()), "{msg}");
}

#[test]
fn scan_profile_manifests_found_has_single_path() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "alpha", &[]);
    let legacy = write_legacy_profile(dir.path(), "zeta.yml", "zeta");

    let entries = scan_profile_manifests(dir.path()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "alpha");
    assert_eq!(entries[0].paths, vec![canonical]);
    assert!(entries[0].ambiguity.is_none());
    assert_eq!(entries[1].name, "zeta");
    assert_eq!(entries[1].paths, vec![legacy]);
    assert!(entries[1].ambiguity.is_none());
}

#[test]
fn scan_profile_manifests_ambiguous_lists_every_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = write_canonical_profile(dir.path(), "work", &[]);
    let yaml = write_legacy_profile(dir.path(), "work.yaml", "work");
    let yml = write_legacy_profile(dir.path(), "work.yml", "work");

    let entries = scan_profile_manifests(dir.path()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "work");
    assert_eq!(entries[0].paths, vec![canonical, yaml, yml]);
    assert!(matches!(
        entries[0].ambiguity,
        Some(ConfigError::AmbiguousProfile { ref name, .. }) if name == "work"
    ));
}

#[test]
fn scan_profile_manifests_missing_dir_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        scan_profile_manifests(&dir.path().join("nope"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn scan_profiles_tolerant_missing_dir_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let entries = scan_profiles_tolerant(&dir.path().join("nope")).unwrap();
    assert!(entries.is_empty());
}

#[cfg(unix)]
#[test]
fn scan_profiles_tolerant_unreadable_dir_errors_as_non_root() {
    use std::os::unix::fs::PermissionsExt;
    if crate::is_root() {
        return; // root bypasses mode bits; the denial cannot be simulated
    }
    let dir = tempfile::tempdir().unwrap();
    let pdir = dir.path().join("profiles");
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::set_permissions(&pdir, std::fs::Permissions::from_mode(0o000)).unwrap();

    let err = scan_profiles_tolerant(&pdir).unwrap_err();
    assert!(
        err.to_string().contains("failed to read"),
        "unreadable dir must be an error, not an empty list: {err}"
    );

    std::fs::set_permissions(&pdir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn load_config_rejects_oversized_file() {
    // The YAML-bomb defense caps config files at 50 MiB by metadata size, before
    // any read. A sparse file (set_len) exceeds the cap without writing 50 MiB, so
    // the guard fires and load_config returns an Invalid error naming "too large".
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("cfgd.yaml");
    let f = std::fs::File::create(&cfg).unwrap();
    f.set_len(51 * 1024 * 1024).unwrap();
    drop(f);

    let err = load_config(&cfg).unwrap_err();
    assert!(
        err.to_string().contains("too large"),
        "oversized config must be rejected with a size message, got: {err}"
    );
}

// --- per-entry platform gating at the layer fold ---

/// A tag no host can ever match, spelled the way the validator accepts: it is
/// lowercase and unknown, which is exactly a distro cfgd does not name.
const NOWHERE: &str = "nowhere_at_all";

fn gated_layer(name: &str, env: Vec<EnvVar>, aliases: Vec<ShellAlias>) -> ProfileLayer {
    ProfileLayer {
        source: "local".into(),
        profile_name: name.into(),
        priority: crate::config::LOCAL_LAYER_PRIORITY,
        policy: LayerPolicy::Local,
        spec: ProfileSpec {
            env,
            aliases,
            ..Default::default()
        },
    }
}

fn tagged_env(name: &str, value: &str, platforms: &[&str]) -> EnvVar {
    EnvVar {
        name: name.into(),
        value: value.into(),
        platforms: platforms.iter().map(|t| t.to_string()).collect(),
    }
}

#[test]
fn a_gated_out_entry_never_reaches_the_layer_merge() {
    // The filter runs BEFORE the fold: reaching a last-writer-wins merge, a
    // gated entry would displace the value that DOES apply here and then have
    // to be un-displaced — and filtering afterwards deletes the base value.
    let here = crate::platform::Platform::current().os.as_str().to_string();
    let merged = merge_layers(&[
        gated_layer(
            "base",
            vec![tagged_env("EDITOR", "vim", &[])],
            vec![ShellAlias {
                name: "ll".into(),
                command: "ls -la".into(),
                platforms: vec![],
            }],
        ),
        gated_layer(
            "work",
            vec![
                tagged_env("EDITOR", "elsewhere", &[NOWHERE]),
                tagged_env("PAGER", "here", &[&here]),
            ],
            vec![ShellAlias {
                name: "ll".into(),
                command: "elsewhere".into(),
                platforms: vec![NOWHERE.to_string()],
            }],
        ),
    ]);

    assert_eq!(
        merged
            .env
            .iter()
            .find(|e| e.name == "EDITOR")
            .map(|e| &e.value),
        Some(&"vim".to_string()),
        "a gated-out overlay must not displace the value that applies here"
    );
    assert_eq!(
        merged
            .env
            .iter()
            .find(|e| e.name == "PAGER")
            .map(|e| &e.value),
        Some(&"here".to_string()),
        "an entry naming this host's platform survives"
    );
    assert_eq!(
        merged
            .aliases
            .iter()
            .find(|a| a.name == "ll")
            .map(|a| &a.command),
        Some(&"ls -la".to_string()),
        "the alias half is filtered on the same predicate"
    );
    // Absent, not annotated: an entry that does not apply here is no part of
    // this host's desired state, exactly as a platform-filtered package is not.
    assert!(
        !merged.env.iter().any(|e| e.value == "elsewhere"),
        "a gated-out entry reaches no surface: {:?}",
        merged.env
    );
}

#[test]
fn a_gated_path_declaration_concatenates_only_where_it_applies() {
    let here = crate::platform::Platform::current().os.as_str().to_string();
    // The layer fold joins on the host's separator, so the declarations are
    // written with it too: a `:`-joined value on Windows is one entry.
    let sep = crate::PATH_LIST_SEPARATOR;
    let merged = merge_layers(&[gated_layer(
        "base",
        vec![
            tagged_env("PATH", &format!("/common/bin{sep}$PATH"), &[]),
            tagged_env("PATH", &format!("/here/bin{sep}$PATH"), &[&here]),
            tagged_env("PATH", &format!("/elsewhere/bin{sep}$PATH"), &[NOWHERE]),
        ],
        vec![],
    )]);
    let path = merged
        .env
        .iter()
        .find(|e| e.name == "PATH")
        .expect("a PATH entry survives");
    assert_eq!(path.value, format!("/common/bin{sep}/here/bin{sep}$PATH"));
    assert_eq!(
        merged.env.iter().filter(|e| e.name == "PATH").count(),
        1,
        "however many declarations survive, one PATH entry comes out"
    );
    // The folded entry carries no gate of its own: its tags have done their
    // work, and a later reader must not re-apply one to a value several
    // declarations produced.
    assert!(path.platforms.is_empty());
}

#[test]
fn an_ungated_entry_serializes_exactly_as_it_did_before_the_field_existed() {
    // Two things ride on this. `rewrite_user_yaml` round-trips every existing
    // profile and module byte-identically, and a source decision is keyed on
    // `sha256(serde_json::to_string(entry))` — a `platforms: []` on the wire
    // would re-ask every recorded decision on the first upgrade.
    let env = EnvVar {
        name: "EDITOR".into(),
        value: "nvim".into(),
        platforms: vec![],
    };
    assert_eq!(
        serde_json::to_string(&env).unwrap(),
        r#"{"name":"EDITOR","value":"nvim"}"#
    );
    assert_eq!(
        serde_yaml::to_string(&env).unwrap(),
        "name: EDITOR\nvalue: nvim\n"
    );

    let alias = ShellAlias {
        name: "ll".into(),
        command: "ls -la".into(),
        platforms: vec![],
    };
    assert_eq!(
        serde_json::to_string(&alias).unwrap(),
        r#"{"name":"ll","command":"ls -la"}"#
    );

    // And a gated one carries the field, so the fingerprint moves exactly when
    // the declaration does.
    let gated = EnvVar {
        platforms: vec!["macos".into()],
        ..env.clone()
    };
    assert_eq!(
        serde_json::to_string(&gated).unwrap(),
        r#"{"name":"EDITOR","value":"nvim","platforms":["macos"]}"#
    );
    assert_ne!(env, gated, "a gate is part of what the entry declares");
}

/// A `deserialize_with` that WIDENS a field past its Rust type and the schema
/// the derive reflects are two statements of one grammar, and must be minted
/// together: `explain`'s Type column, its `Variants` section, `-o json`'s
/// `type` and the SchemaStore-published `cfgd-profile` / `cfgd-source`
/// schemas all read the reflection, and none of them can see serde. Without
/// the paired `schema_with`, the published `BrewSpec` was a bare object that
/// rejected `brew: [ripgrep, fzf]` — the form the rustdoc example used and
/// `cfgd apply` accepted. Walks the source for every field on either union
/// deserializer, then the live schema for both shapes.
#[test]
fn every_list_or_map_package_field_declares_both_shapes_in_its_schema() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/config/profile_spec.rs"),
    )
    .unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let mut widened = Vec::new();
    let mut unpaired = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        let widening = line.contains("deserialize_with = \"list_or_struct\"")
            || line.contains("deserialize_with = \"list_or_packages_vec\"");
        if !widening {
            continue;
        }
        // The field name is on the next line carrying a visibility lead; the
        // paired attribute sits between the two.
        let mut m = n + 1;
        let mut paired = false;
        let declares_field = |line: &str| {
            let code = line.trim_start();
            crate::test_helpers::strip_item_lead(code) != code
        };
        while m < lines.len() && !declares_field(lines[m]) {
            paired |= lines[m].contains("schema_with");
            m += 1;
        }
        let field = crate::test_helpers::strip_item_lead(lines[m].trim_start())
            .split(':')
            .next()
            .unwrap()
            .to_string();
        if !paired {
            unpaired.push(format!("{field} (line {})", n + 1));
        }
        widened.push(field);
    }
    assert!(
        widened.len() >= 18,
        "the walk no longer reaches the union-deserialized package fields — it found {widened:?}"
    );
    assert!(
        unpaired.is_empty(),
        "a `deserialize_with` that accepts two shapes needs a `schema_with` that says so:\n{}",
        unpaired.join("\n")
    );

    let schema = serde_json::to_value(schemars::schema_for!(super::PackagesSpec)).unwrap();
    let props = schema["properties"]
        .as_object()
        .expect("PackagesSpec is an object");
    let mut one_shaped = Vec::new();
    for field in &widened {
        let camel = {
            let mut out = String::new();
            let mut up = false;
            for c in field.chars() {
                if c == '_' {
                    up = true;
                } else if up {
                    out.push(c.to_ascii_uppercase());
                    up = false;
                } else {
                    out.push(c);
                }
            }
            out
        };
        let prop = &props[&camel];
        let members = prop["anyOf"].as_array().cloned().unwrap_or_default();
        let is_array = |m: &serde_json::Value| m["type"] == "array";
        let is_object = |m: &serde_json::Value| m["type"] == "object" || m.get("$ref").is_some();
        if !(members.iter().any(is_array) && members.iter().any(is_object)) {
            one_shaped.push(format!("{camel}: {prop}"));
        }
    }
    assert!(
        one_shaped.is_empty(),
        "a package field's schema must carry BOTH the list and the map shape its deserializer accepts:\n{}",
        one_shaped.join("\n")
    );
}
