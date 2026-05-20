use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::output::{Printer, Role};

use cfgd_core::providers::{SystemConfigurator, SystemDrift};

use std::path::Path;

/// LaunchAgentConfigurator — manages macOS LaunchAgent plists.
pub struct LaunchAgentConfigurator;

impl SystemConfigurator for LaunchAgentConfigurator {
    fn name(&self) -> &str {
        "launchAgents"
    }

    fn is_available(&self) -> bool {
        cfg!(target_os = "macos")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Sequence(Vec::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let mut drifts = Vec::new();

        let agents = match desired.as_sequence() {
            Some(s) => s,
            None => return Ok(drifts),
        };

        for agent in agents {
            let name = match agent.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let plist_path = launch_agent_plist_path(name);
            let program = agent.get("program").and_then(|v| v.as_str()).unwrap_or("");
            let run_at_load = agent
                .get("runAtLoad")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let args: Vec<&str> = agent
                .get("args")
                .and_then(|v| v.as_sequence())
                .map(|seq| seq.iter().filter_map(|v| v.as_str()).collect::<Vec<&str>>())
                .unwrap_or_default();
            let expected_content = generate_launch_agent_plist(name, program, &args, run_at_load);

            if !plist_path.exists() {
                drifts.push(SystemDrift {
                    key: format!("{}.plist", name),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
            } else if let Ok(current_content) = std::fs::read_to_string(&plist_path)
                && current_content != expected_content
            {
                drifts.push(SystemDrift {
                    key: format!("{}.plist", name),
                    expected: "updated".to_string(),
                    actual: "outdated".to_string(),
                });
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let agents = match desired.as_sequence() {
            Some(s) => s,
            None => return Ok(()),
        };

        for agent in agents {
            let name = match agent.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let program = agent.get("program").and_then(|v| v.as_str()).unwrap_or("");
            let run_at_load = agent
                .get("runAtLoad")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let args: Vec<&str> = agent
                .get("args")
                .and_then(|v| v.as_sequence())
                .map(|seq| seq.iter().filter_map(|v| v.as_str()).collect::<Vec<&str>>())
                .unwrap_or_default();

            let plist_content = generate_launch_agent_plist(name, program, &args, run_at_load);
            let plist_path = launch_agent_plist_path(name);

            printer.status_simple(
                Role::Info,
                format!("Writing launch agent: {}", plist_path.display()),
            );

            cfgd_core::atomic_write_str(&plist_path, &plist_content)?;

            // Unload existing agent (best-effort — may not be loaded yet)
            if let Err(e) = Command::new("launchctl")
                .args(["unload", &plist_path.display().to_string()])
                .output()
            {
                tracing::debug!("launchctl unload (pre-load cleanup): {e}");
            }

            let output = Command::new("launchctl")
                .args(["load", &plist_path.display().to_string()])
                .output()
                .map_err(cfgd_core::errors::CfgdError::Io)?;

            if !output.status.success() {
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "launchctl load failed for {}: {}",
                        name,
                        cfgd_core::stderr_lossy_trimmed(&output)
                    ),
                );
            }
        }

        Ok(())
    }
}

fn launch_agent_plist_path(name: &str) -> std::path::PathBuf {
    cfgd_core::expand_tilde(Path::new("~"))
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", name))
}

fn generate_launch_agent_plist(
    label: &str,
    program: &str,
    args: &[&str],
    run_at_load: bool,
) -> String {
    let mut program_args = String::new();
    if !program.is_empty() || !args.is_empty() {
        program_args.push_str("    <key>ProgramArguments</key>\n    <array>\n");
        if !program.is_empty() {
            program_args.push_str(&format!(
                "        <string>{}</string>\n",
                cfgd_core::xml_escape(program)
            ));
        }
        for arg in args {
            program_args.push_str(&format!(
                "        <string>{}</string>\n",
                cfgd_core::xml_escape(arg)
            ));
        }
        program_args.push_str("    </array>\n");
    }

    let run_at_load_str = if run_at_load { "true" } else { "false" };

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
{}    <key>RunAtLoad</key>
    <{} />
</dict>
</plist>
"#,
        cfgd_core::xml_escape(label),
        program_args,
        run_at_load_str
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_plist_content() {
        let plist =
            generate_launch_agent_plist("com.example.test", "/usr/bin/test", &["--flag"], true);
        assert!(plist.contains("com.example.test"));
        assert!(plist.contains("/usr/bin/test"));
        assert!(plist.contains("--flag"));
        assert!(plist.contains("<true />"));
    }

    #[test]
    fn generate_plist_no_args() {
        let plist = generate_launch_agent_plist("com.example.test", "/usr/bin/test", &[], false);
        assert!(plist.contains("com.example.test"));
        assert!(plist.contains("<false />"));
    }

    #[test]
    fn generate_plist_xml_escaped_args() {
        let plist =
            generate_launch_agent_plist("com.example.test", "/usr/bin/test", &["--key=a&b"], false);
        // Should contain XML-escaped ampersand
        assert!(
            plist.contains("&amp;"),
            "ampersand must be XML-escaped in plist"
        );
    }

    #[test]
    fn generate_plist_multiple_args() {
        let plist = generate_launch_agent_plist(
            "com.example.test",
            "/usr/bin/test",
            &["--flag1", "--flag2", "value"],
            true,
        );
        assert!(plist.contains("--flag1"));
        assert!(plist.contains("--flag2"));
        assert!(plist.contains("value"));
    }

    #[test]
    fn launch_agents_not_available_on_linux() {
        let la = LaunchAgentConfigurator;
        assert!(!la.is_available());
    }

    #[test]
    fn generate_plist_empty_program_and_no_args() {
        let plist = generate_launch_agent_plist("com.example.empty", "", &[], true);
        assert!(plist.contains("com.example.empty"));
        assert!(plist.contains("<true />"));
        // No ProgramArguments block when both program and args are empty
        assert!(!plist.contains("ProgramArguments"));
    }

    #[test]
    fn generate_plist_xml_escape_in_label() {
        let plist = generate_launch_agent_plist("com.example.<test>&", "/bin/sh", &[], false);
        assert!(plist.contains("com.example.&lt;test&gt;&amp;"));
    }

    #[test]
    fn generate_plist_args_only_no_program() {
        let plist = generate_launch_agent_plist("com.example.test", "", &["--verbose"], false);
        assert!(plist.contains("ProgramArguments"));
        assert!(plist.contains("--verbose"));
        // No <string></string> for empty program
        let program_strings: Vec<&str> = plist
            .lines()
            .filter(|l| l.contains("<string>") && l.contains("</string>"))
            .collect();
        // Only the label string and the arg string should be present
        assert!(
            program_strings.iter().any(|l| l.contains("--verbose")),
            "should contain the arg"
        );
    }

    #[test]
    fn launch_agent_diff_non_sequence_desired() {
        let la = LaunchAgentConfigurator;
        let desired = serde_yaml::Value::String("not a sequence".into());
        let drifts = la.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn launch_agent_diff_agent_without_name_skipped() {
        let la = LaunchAgentConfigurator;
        let mut agent = serde_yaml::Mapping::new();
        agent.insert(
            serde_yaml::Value::String("program".into()),
            serde_yaml::Value::String("/usr/bin/true".into()),
        );
        let desired = serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(agent)]);
        let drifts = la.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn launch_agent_current_state_returns_empty_sequence() {
        let la = LaunchAgentConfigurator;
        let state = la.current_state().unwrap();
        assert!(state.is_sequence());
        assert!(state.as_sequence().unwrap().is_empty());
    }

    #[test]
    fn generate_plist_special_chars_in_program() {
        let plist =
            generate_launch_agent_plist("com.example.test", "/path/with spaces/prog", &[], false);
        assert!(plist.contains("/path/with spaces/prog"));
    }

    #[test]
    fn generate_plist_xml_escape_in_program() {
        let plist = generate_launch_agent_plist("com.test", "/usr/bin/test<>", &[], false);
        assert!(
            plist.contains("&lt;") && plist.contains("&gt;"),
            "program path should be XML-escaped"
        );
    }

    #[test]
    fn generate_launch_agent_plist_basic_with_program() {
        let plist = generate_launch_agent_plist("com.test.agent", "/usr/bin/test", &[], true);
        assert!(
            plist.contains("<?xml version=\"1.0\""),
            "should have XML declaration"
        );
        assert!(
            plist.contains("<string>com.test.agent</string>"),
            "should contain label"
        );
        assert!(
            plist.contains("<string>/usr/bin/test</string>"),
            "should contain program"
        );
        assert!(plist.contains("<true />"), "should have RunAtLoad=true");
        assert!(
            plist.contains("ProgramArguments"),
            "should have ProgramArguments key"
        );
    }

    #[test]
    fn generate_launch_agent_plist_with_args() {
        let plist = generate_launch_agent_plist(
            "com.test.daemon",
            "/usr/local/bin/cfgd",
            &["--config", "/etc/cfgd.yaml", "daemon"],
            false,
        );
        assert!(plist.contains("<string>/usr/local/bin/cfgd</string>"));
        assert!(plist.contains("<string>--config</string>"));
        assert!(plist.contains("<string>/etc/cfgd.yaml</string>"));
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<false />"), "should have RunAtLoad=false");
    }

    #[test]
    fn generate_launch_agent_plist_no_program_no_args() {
        let plist = generate_launch_agent_plist("com.test.empty", "", &[], true);
        assert!(
            !plist.contains("ProgramArguments"),
            "should not have ProgramArguments when both empty"
        );
        assert!(plist.contains("<string>com.test.empty</string>"));
    }

    #[test]
    fn generate_launch_agent_plist_xml_escaping() {
        let plist =
            generate_launch_agent_plist("com.test.<special>&", "/bin/test", &["--flag=a&b"], true);
        assert!(
            plist.contains("com.test.&lt;special&gt;&amp;"),
            "label should be XML-escaped"
        );
        assert!(
            plist.contains("--flag=a&amp;b"),
            "args should be XML-escaped"
        );
    }

    #[test]
    fn generate_launch_agent_plist_args_only_no_program() {
        let plist = generate_launch_agent_plist("com.test.argsonly", "", &["arg1", "arg2"], false);
        assert!(
            plist.contains("ProgramArguments"),
            "should have ProgramArguments with args"
        );
        assert!(plist.contains("<string>arg1</string>"));
        assert!(plist.contains("<string>arg2</string>"));
        // Empty program string should not generate a string element
        // (the function only adds program if !program.is_empty())
    }

    #[test]
    fn launch_agent_plist_path_contains_label() {
        let path = launch_agent_plist_path("com.cfgd.test");
        let path_str = path.display().to_string();
        assert!(
            path_str.contains("Library/LaunchAgents"),
            "should be in LaunchAgents dir"
        );
        assert!(
            path_str.ends_with("com.cfgd.test.plist"),
            "should end with label.plist"
        );
    }

    #[test]
    fn launch_agent_plist_generation_is_deterministic() {
        // Verify the generated plist would differ from stale content
        let expected = generate_launch_agent_plist("com.test.outdated", "/usr/bin/true", &[], true);
        assert_ne!(expected, "stale plist content");
        // And that generated content for the same args is deterministic
        let expected2 =
            generate_launch_agent_plist("com.test.outdated", "/usr/bin/true", &[], true);
        assert_eq!(expected, expected2);
    }

    #[test]
    fn launch_agent_apply_empty_sequence_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let la = LaunchAgentConfigurator;
        let yaml = serde_yaml::Value::Sequence(Vec::new());
        la.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn launch_agent_apply_non_sequence_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let la = LaunchAgentConfigurator;
        let yaml = serde_yaml::Value::String("not a sequence".into());
        la.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn launch_agent_apply_skips_agents_without_name() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let la = LaunchAgentConfigurator;
        let mut agent = serde_yaml::Mapping::new();
        agent.insert(
            serde_yaml::Value::String("program".into()),
            serde_yaml::Value::String("/usr/bin/true".into()),
        );
        let yaml = serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(agent)]);
        la.apply(&yaml, &printer).unwrap();
    }

    #[test]
    fn generate_plist_run_at_load_true_produces_true_tag() {
        let plist = generate_launch_agent_plist("com.test", "", &[], true);
        assert!(
            plist.contains("<true />"),
            "RunAtLoad=true should produce <true /> tag"
        );
        assert!(!plist.contains("<false />"));
    }

    #[test]
    fn generate_plist_run_at_load_false_produces_false_tag() {
        let plist = generate_launch_agent_plist("com.test", "", &[], false);
        assert!(
            plist.contains("<false />"),
            "RunAtLoad=false should produce <false /> tag"
        );
        assert!(!plist.contains("<true />"));
    }
}
