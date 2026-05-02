//! Pure parsers used by `SimpleManager` for native package list output.
//!
//! Each function takes raw `<cmd> list` stdout and returns the set of
//! installed package names with platform-specific normalization (arch suffix,
//! version suffix, table columns) stripped.

use std::collections::HashSet;

use super::shared::{strip_arch_suffix, strip_version_suffix};

pub(super) fn parse_simple_lines(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

pub(super) fn parse_dnf_yum_lines(stdout: &str, skip_prefixes: &[&str]) -> HashSet<String> {
    stdout
        .lines()
        .filter(|l| !l.is_empty() && !skip_prefixes.iter().any(|prefix| l.starts_with(prefix)))
        .filter_map(|l| {
            let name = l.split_whitespace().next()?;
            Some(strip_arch_suffix(name))
        })
        .collect()
}

pub(super) fn parse_dnf_lines(stdout: &str) -> HashSet<String> {
    parse_dnf_yum_lines(stdout, &["Installed", "Last"])
}

pub(super) fn parse_yum_lines(stdout: &str) -> HashSet<String> {
    parse_dnf_yum_lines(stdout, &["Installed", "Loaded"])
}

pub(super) fn parse_apk_lines(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let name = l.split_whitespace().next()?;
            Some(strip_version_suffix(name))
        })
        .collect()
}

pub(super) fn parse_zypper_lines(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .filter(|l| l.contains('|') && !l.starts_with("--") && !l.starts_with("S "))
        .filter_map(|l| {
            let cols: Vec<&str> = l.split('|').map(|c| c.trim()).collect();
            if cols.len() >= 3 {
                let name = cols[1].trim();
                if !name.is_empty() && name != "Name" {
                    return Some(name.to_string());
                }
            }
            None
        })
        .collect()
}

pub(super) fn parse_pkg_lines(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| strip_version_suffix(l.trim()))
        .collect()
}
