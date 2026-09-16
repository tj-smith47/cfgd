//! Node-level `SystemConfigurator` implementations.
//!
//! Each configurator owns a single submodule; the seven `*Configurator` unit
//! structs are re-exported here so `system::mod.rs::pub use node::*` continues
//! to surface them to the rest of the crate.
//!
//! The types compile on every host and every one of them answers
//! `is_available()` off a Linux kernel interface, a Linux-only binary or the
//! target family, so an off-platform host gets the registered configurator's
//! own refusal rather than the planner's "no configurator registered".

mod apparmor;
mod certificates;
mod containerd;
mod format;
mod kernel_modules;
mod kubelet;
mod seccomp;
mod sysctl;

pub use apparmor::AppArmorConfigurator;
pub use certificates::CertificateConfigurator;
pub use containerd::ContainerdConfigurator;
pub use kernel_modules::KernelModuleConfigurator;
pub use kubelet::KubeletConfigurator;
pub use seccomp::SeccompConfigurator;
pub use sysctl::SysctlConfigurator;

// The production types above are portable; the tests below read real
// /proc, /sys and trust-store state that exists on unix alone.
#[cfg(all(test, unix))]
mod tests;
