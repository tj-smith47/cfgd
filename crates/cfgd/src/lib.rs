//! cfgd library surface — exposes the CLI entry point so external integration
//! tests can build the clap tree without spawning the binary. The binary at
//! `src/main.rs` is a thin wrapper over this library.

pub mod ai;
pub mod cli;
pub mod files;
pub mod generate;
pub mod mcp;
pub mod packages;
pub mod secrets;
pub mod system;

pub use cli::Cli;

/// The `CFGD_*_BIN` variables this crate's package managers, secret backends
/// and system configurators read a tool's path from, for a test pointing one at
/// a shim. Integration tests build as crates of their own and reach them here.
/// A manager tool whose variable has no const of its own is named through
/// `tool_seam_var`, the derivation production reads it by.
#[cfg(any(test, feature = "test-helpers"))]
pub mod seams {
    pub use crate::packages::shared::BREW_BIN_ENV;
    pub use crate::packages::shared::tool_seam_var;
    pub use crate::packages::simple::APT_GET_BIN_ENV;
    pub use crate::packages::versions::APK_BIN_ENV;
    pub use crate::packages::versions::APT_CACHE_BIN_ENV;
    pub use crate::packages::versions::DNF_BIN_ENV;
    pub use crate::packages::versions::DPKG_QUERY_BIN_ENV;
    pub use crate::packages::versions::PACMAN_BIN_ENV;
    pub use crate::packages::versions::PKG_BIN_ENV;
    pub use crate::packages::versions::RPM_BIN_ENV;
    pub use crate::packages::versions::YUM_BIN_ENV;
    pub use crate::packages::versions::ZYPPER_BIN_ENV;
    pub use crate::secrets::age::AGE_BIN_ENV;
    pub use crate::secrets::bitwarden::BW_BIN_ENV;
    pub use crate::secrets::lastpass::LPASS_BIN_ENV;
    pub use crate::secrets::onepassword::OP_BIN_ENV;
    pub use crate::secrets::sops::SOPS_BIN_ENV;
    pub use crate::secrets::vault::VAULT_BIN_ENV;
    pub use crate::system::gpg_keys::GPG_BIN_ENV;
    pub use crate::system::gsettings::GSETTINGS_BIN_ENV;
    pub use crate::system::kde_config::KREADCONFIG_BIN_ENV;
    pub use crate::system::kde_config::KWRITECONFIG_BIN_ENV;
    pub use crate::system::macos_defaults::DEFAULTS_BIN_ENV;
    pub use crate::system::xfconf::XFCONF_QUERY_BIN_ENV;
}
