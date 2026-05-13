//! Dogfood test: cfgd's CLI tree must produce a valid brontes tool list.
//!
//! Exercises brontes' library API against the full cfgd clap surface so that
//! a regression in either crate (cfgd Cli reshape, brontes walker
//! behaviour) is caught here before it reaches downstream consumers.

use brontes::Config;
use cfgd::Cli;
use clap::CommandFactory;

#[test]
fn cfgd_cli_produces_valid_brontes_tool_list() {
    let cmd = Cli::command();
    let cfg = Config::default().tool_name_prefix("cfgd");
    let tools = brontes::generate_tools(&cmd, &cfg)
        .expect("cfgd CLI must produce a valid brontes tool list");

    assert!(!tools.is_empty(), "expected at least one tool");
    for tool in &tools {
        let name = tool.name.as_ref();
        assert!(
            name == "cfgd" || name.starts_with("cfgd_"),
            "tool name must start with cfgd prefix, got {name}"
        );
    }
}
