//! CRD YAML generator binary for the Helm chart.
//!
//! # Hard-Rule #1 exemption
//!
//! This is a standalone build-tool binary whose entire contract is
//! "emit well-formed YAML so the caller can `> file.yaml`". The
//! `output::Printer` abstraction is a structured terminal interface
//! (headers, spinners, styling) and cannot produce raw YAML on stdout
//! without corrupting the output. The direct `print!` below is therefore
//! the correct tool, documented here so future audits / reviewers don't
//! re-flag it as a Hard-Rule #1 violation.
//!
//! This file is the ONLY `print!`/`println!` use outside of the
//! `output` module in the cfgd workspace.
//!
//! Usage:
//!   cfgd-gen-crds                      # concatenated YAML to stdout
//!   cfgd-gen-crds --out-dir <dir>      # one <dir>/<plural>.yaml per CRD

use std::path::Path;

use cfgd_operator::gen_crds::{render_all, render_each};

/// Build-tool entry point. Returning `Err` makes `main` print the error via
/// `Debug` and exit non-zero — no terminal abstraction needed for a YAML
/// emitter whose stderr is just a build-failure signal.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    match parse_out_dir(&args[1..]) {
        Some(dir) => write_chart_files(Path::new(&dir))?,
        None => print!("{}", render_all()?),
    }
    Ok(())
}

/// Parse `--out-dir <dir>` (or `--out-dir=<dir>`) from the argument list;
/// returns `None` when the flag is absent (stdout mode).
fn parse_out_dir(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--out-dir=") {
            return Some(value.to_string());
        }
        if arg == "--out-dir" {
            return iter.next().cloned();
        }
    }
    None
}

/// Write one `<dir>/<plural>.yaml` per CRD. The filename strips the `.cfgd.io`
/// group suffix from each CRD's `metadata.name` to match the existing
/// `chart/cfgd/crds/` layout (e.g. `machineconfigs.cfgd.io` → `machineconfigs.yaml`).
fn write_chart_files(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    for crd in render_each()? {
        let stem = crd.name.strip_suffix(".cfgd.io").unwrap_or(&crd.name);
        std::fs::write(dir.join(format!("{stem}.yaml")), crd.yaml)?;
    }
    Ok(())
}
