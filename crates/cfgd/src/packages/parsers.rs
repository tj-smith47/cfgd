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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_lines_basic() {
        let result = parse_simple_lines("curl\nwget\n\nvim\n");
        assert_eq!(result.len(), 3);
        assert!(result.contains("curl"));
        assert!(result.contains("wget"));
        assert!(result.contains("vim"));
    }

    #[test]
    fn parse_dnf_lines_skips_headers() {
        let result = parse_dnf_lines(
            "Installed Packages\ncurl.x86_64  7.88  @base\nwget.x86_64  1.21  @base\nLast metadata check\n",
        );
        assert!(result.contains("curl"));
        assert!(result.contains("wget"));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_yum_lines_skips_headers() {
        let result =
            parse_yum_lines("Installed Packages\nvim.x86_64  8.2  @base\nLoaded plugins\n");
        assert!(result.contains("vim"));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_apk_lines_strips_version() {
        let result = parse_apk_lines("curl-7.88.1-r1\nwget-1.21.4-r0\n");
        assert!(result.contains("curl"));
        assert!(result.contains("wget"));
    }

    #[test]
    fn parse_zypper_lines_parses_table() {
        let output = "S  | Name      | Summary\n---+-----------+--------\ni+ | vim       | Vi IMproved\ni  | curl      | URL tool\n";
        let result = parse_zypper_lines(output);
        assert!(result.contains("vim"));
        assert!(result.contains("curl"));
    }

    #[test]
    fn parse_zypper_lines_skips_header_row() {
        let output = "S | Name | Type\n--+------+-----\ni | vim  | package\n";
        let result = parse_zypper_lines(output);
        assert!(result.contains("vim"));
        assert!(!result.contains("Name"));
    }

    #[test]
    fn parse_pkg_lines_strips_version() {
        let result = parse_pkg_lines("curl-7.88.1\nnginx-1.25.3\n");
        assert!(result.contains("curl"));
        assert!(result.contains("nginx"));
    }

    #[test]
    fn parse_dnf_yum_lines_empty_input() {
        let result = parse_dnf_yum_lines("", &["Installed", "Last"]);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_dnf_yum_lines_only_headers() {
        let input = "Installed Packages\nLast metadata expiration check\n";
        let result = parse_dnf_yum_lines(input, &["Installed", "Last"]);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_dnf_yum_lines_strips_arch_from_real_output() {
        // Realistic dnf list installed output
        let input = "\
Installed Packages\n\
bash.x86_64                     5.2.15-3.fc39        @anaconda\n\
coreutils.x86_64                9.3-4.fc39           @anaconda\n\
glibc.i686                      2.38-11.fc39         @updates\n\
kernel.x86_64                   6.5.6-300.fc39       @updates\n\
Last metadata expiration check: 0:42:17 ago\n";
        let result = parse_dnf_yum_lines(input, &["Installed", "Last"]);
        assert_eq!(result.len(), 4);
        assert!(result.contains("bash"));
        assert!(result.contains("coreutils"));
        assert!(result.contains("glibc"));
        assert!(result.contains("kernel"));
        // Arch suffixes should be stripped
        assert!(!result.contains("bash.x86_64"));
        assert!(!result.contains("glibc.i686"));
    }

    #[test]
    fn parse_dnf_yum_lines_noarch_packages() {
        let input = "python3-pip.noarch              22.3.1-3.fc39      @fedora\n\
                     tzdata.noarch                   2023c-1.fc39       @updates\n";
        let result = parse_dnf_yum_lines(input, &[]);
        assert!(result.contains("python3-pip"));
        assert!(result.contains("tzdata"));
    }

    #[test]
    fn parse_dnf_yum_lines_blank_lines_ignored() {
        let input = "\n\ncurl.x86_64  8.0  @base\n\n\n";
        let result = parse_dnf_yum_lines(input, &[]);
        assert_eq!(result.len(), 1);
        assert!(result.contains("curl"));
    }

    #[test]
    fn parse_yum_lines_with_loaded_plugins() {
        // yum output has "Loaded plugins:" header
        let input = "Loaded plugins: fastestmirror, langpacks\n\
                     Installed Packages\n\
                     vim-enhanced.x86_64    8.2.4328-1.el8    @appstream\n\
                     wget.x86_64            1.21.1-7.el8      @baseos\n";
        let result = parse_yum_lines(input);
        assert_eq!(result.len(), 2);
        assert!(result.contains("vim-enhanced"));
        assert!(result.contains("wget"));
    }

    #[test]
    fn parse_apk_lines_empty() {
        let result = parse_apk_lines("");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_apk_lines_multiple_hyphens_in_name() {
        // Package names can have hyphens; only strip the last one before a digit
        let result = parse_apk_lines("lib-xml2-utils-2.10.3-r0\n");
        // Should strip from the last hyphen before a digit
        assert!(result.contains("lib-xml2-utils"));
    }

    #[test]
    fn parse_apk_lines_with_extra_columns() {
        // apk output may have extra whitespace-separated columns
        let result = parse_apk_lines("curl-7.88.1-r1 x86_64\nwget-1.21.4 x86_64\n");
        assert!(result.contains("curl"));
        assert!(result.contains("wget"));
    }

    #[test]
    fn parse_zypper_lines_empty() {
        let result = parse_zypper_lines("");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_zypper_lines_skips_separator_and_status_header() {
        let output = "S  | Name | Version\n--+------+--------\nS | Name | Version\n";
        let result = parse_zypper_lines(output);
        // "S " lines at start are excluded, "--" lines are excluded, "Name" header excluded
        assert!(result.is_empty());
    }

    #[test]
    fn parse_zypper_lines_no_pipes() {
        // Lines without pipes are ignored
        let output = "Some random line\nanother line\n";
        let result = parse_zypper_lines(output);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_zypper_lines_empty_name_column() {
        let output = "i |   | 1.0\n";
        let result = parse_zypper_lines(output);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_pkg_lines_empty() {
        let result = parse_pkg_lines("");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_pkg_lines_no_version() {
        // Packages without a version suffix
        let result = parse_pkg_lines("bash\nzsh\n");
        assert!(result.contains("bash"));
        assert!(result.contains("zsh"));
    }

    #[test]
    fn parse_dnf_lines_multi_arch_packages() {
        let input = "\
bash.x86_64     5.2.15  @anaconda\n\
glibc.i686      2.38    @updates\n\
glibc.x86_64    2.38    @updates\n";
        let result = parse_dnf_lines(input);
        // Both glibc entries collapse to "glibc" since arch is stripped
        assert!(result.contains("bash"));
        assert!(result.contains("glibc"));
    }

    #[test]
    fn parse_yum_lines_empty() {
        let result = parse_yum_lines("");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_yum_lines_only_headers() {
        let input = "Installed Packages\nLoaded plugins: fastestmirror\n";
        let result = parse_yum_lines(input);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_apk_lines_no_version_in_name() {
        // Package name without any version-like suffix
        let result = parse_apk_lines("busybox\nmusl\n");
        assert!(result.contains("busybox"));
        assert!(result.contains("musl"));
    }

    #[test]
    fn parse_zypper_lines_fewer_than_3_columns() {
        // Line with pipes but fewer than 3 columns
        let output = "i | vim\n";
        let result = parse_zypper_lines(output);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_zypper_lines_real_output() {
        let output = "\
S  | Name           | Type     | Version        | Arch   | Repository\n\
---+----------------+----------+----------------+--------+-----------\n\
i+ | bash           | package  | 5.1.16-2.1     | x86_64 | Main\n\
i  | coreutils      | package  | 9.1-2.2        | x86_64 | Main\n\
i  | vim            | package  | 9.0.1894-1.1   | x86_64 | Main\n";
        let result = parse_zypper_lines(output);
        assert_eq!(result.len(), 3);
        assert!(result.contains("bash"));
        assert!(result.contains("coreutils"));
        assert!(result.contains("vim"));
    }

    #[test]
    fn parse_pkg_lines_with_complex_names() {
        let result = parse_pkg_lines("py39-pip-23.0\nrust-1.75.0\n");
        assert!(result.contains("py39-pip"));
        assert!(result.contains("rust"));
    }

    #[test]
    fn parse_simple_lines_empty() {
        let result = parse_simple_lines("");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_simple_lines_only_whitespace() {
        let result = parse_simple_lines("   \n  \n\n  ");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_simple_lines_trims_whitespace() {
        let result = parse_simple_lines("  curl  \n  wget  \n");
        assert!(result.contains("curl"));
        assert!(result.contains("wget"));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_apk_lines_real_world() {
        // Real apk list output format
        let output = "alpine-baselayout-3.4.3-r2 x86_64 {alpine-baselayout}\nbusybox-1.36.1-r19 x86_64 {busybox}\ncurl-8.5.0-r0 x86_64 {curl}\n";
        let result = parse_apk_lines(output);
        assert!(result.contains("alpine-baselayout"));
        assert!(result.contains("busybox"));
        assert!(result.contains("curl"));
    }

    #[test]
    fn parse_zypper_lines_real_world_with_many_columns() {
        let output = "\
S  | Name           | Type     | Version        | Arch   | Repository
---+----------------+----------+----------------+--------+-----------
i+ | bash           | package  | 5.1.16-2.1     | x86_64 | Main
i  | coreutils      | package  | 9.1-2.2        | x86_64 | Main
i  | gcc            | package  | 13.2.0-1.1     | x86_64 | Main
i  | glibc          | package  | 2.38-3.1       | x86_64 | Main
i  | python3        | package  | 3.12.1-1.1     | x86_64 | Main
";
        let result = parse_zypper_lines(output);
        assert_eq!(result.len(), 5);
        assert!(result.contains("bash"));
        assert!(result.contains("python3"));
    }

    #[test]
    fn parse_dnf_lines_real_world_with_multi_word_repos() {
        let input = "\
Installed Packages
NetworkManager.x86_64             1.44.2-3.fc39        @anaconda
bash.x86_64                       5.2.21-2.fc39        @anaconda
dnf.noarch                        4.18.2-2.fc39        @anaconda
Last metadata expiration check: 2:15:33 ago on Mon 01 Jan 2024 12:00:00 PM UTC.
";
        let result = parse_dnf_lines(input);
        assert_eq!(result.len(), 3);
        assert!(result.contains("NetworkManager"));
        assert!(result.contains("bash"));
        assert!(result.contains("dnf"));
    }

    #[test]
    fn parse_dnf_yum_lines_whitespace_only_lines_treated_as_empty() {
        let input = "   \n\t\ncurl.x86_64  8.0  @base\n";
        let result = parse_dnf_yum_lines(input, &[]);
        // Whitespace-only lines are not empty per .is_empty() so they pass through
        // but split_whitespace().next() may return None for all-whitespace lines
        // Actually "   " is not empty and doesn't match skip_prefixes,
        // but split_whitespace().next() returns None for "   "
        // filter_map with None → filtered out
        assert_eq!(result.len(), 1);
        assert!(result.contains("curl"));
    }

    #[test]
    fn parse_apk_lines_package_with_single_char_name() {
        // Edge case: very short package name
        let result = parse_apk_lines("a-1.0\n");
        assert!(result.contains("a"));
    }

    #[test]
    fn parse_apk_lines_package_ending_in_hyphen_no_digit() {
        // "-abc" after last hyphen is not a digit → treated as name
        let result = parse_apk_lines("foo-bar-abc\n");
        assert!(result.contains("foo-bar-abc"));
    }

    #[test]
    fn parse_zypper_lines_plus_separator() {
        // Real zypper uses ---+--- separators
        let output =
            "S  | Name | Version\n---+------+--------\ni  | gcc  | 13.2\ni  | vim  | 9.0\n";
        let result = parse_zypper_lines(output);
        assert_eq!(result.len(), 2);
        assert!(result.contains("gcc"));
        assert!(result.contains("vim"));
    }

    #[test]
    fn parse_pkg_lines_with_whitespace() {
        let result = parse_pkg_lines("  curl-7.88.1  \n  nginx-1.25.3  \n");
        assert!(result.contains("curl"));
        assert!(result.contains("nginx"));
    }

    #[test]
    fn parse_simple_lines_deduplicates_via_hashset() {
        let result = parse_simple_lines("curl\nwget\ncurl\n");
        assert_eq!(result.len(), 2);
        assert!(result.contains("curl"));
        assert!(result.contains("wget"));
    }

    #[test]
    fn parse_dnf_yum_lines_multiple_skip_prefixes() {
        let input = "Installed Packages\nLoaded plugins\ncurl.x86_64 8.0 @base\nLast check\n";
        let result = parse_dnf_yum_lines(input, &["Installed", "Loaded", "Last"]);
        assert_eq!(result.len(), 1);
        assert!(result.contains("curl"));
    }

    #[test]
    fn parse_apk_lines_real_alpine_output() {
        let output = "\
alpine-baselayout-3.4.3-r2 x86_64 {alpine-baselayout} (GPL-2.0-only) [installed]
busybox-1.36.1-r19 x86_64 {busybox} (GPL-2.0-only) [installed]
ca-certificates-20240226-r0 x86_64 {ca-certificates} (MPL-2.0 AND MIT) [installed]
curl-8.5.0-r0 x86_64 {curl} (MIT) [installed]
";
        let result = parse_apk_lines(output);
        assert_eq!(result.len(), 4);
        assert!(result.contains("alpine-baselayout"));
        assert!(result.contains("busybox"));
        assert!(result.contains("ca-certificates"));
        assert!(result.contains("curl"));
    }
}
