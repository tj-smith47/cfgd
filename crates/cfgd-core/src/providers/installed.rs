//! The per-run memo of what each package manager reports as installed.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::{PackageInfo, PackageManager};

/// One manager's installed-package enumeration, read once per run.
///
/// Both views are folded from ONE call to
/// [`PackageManager::installed_packages_with_versions`]. That method and
/// [`PackageManager::installed_packages`] describe the same population in two
/// spellings — [`PackageManager::listed_identity`] exists precisely to map the
/// first into the second's space, and the planner has always diffed against
/// that mapping — so asking both is asking the machine the same question twice.
/// The versioned call is the one that survives, because the version half cannot
/// be recovered from a bare name set while the name set is a fold away.
pub struct InstalledPackages {
    identities: HashSet<String>,
    listed: Vec<PackageInfo>,
}

impl InstalledPackages {
    pub(super) fn from_listing(manager: &dyn PackageManager, listed: Vec<PackageInfo>) -> Self {
        let identities = listed
            .iter()
            .map(|pkg| manager.listed_identity(&pkg.name))
            .collect();
        Self { identities, listed }
    }

    /// Whether the manager reports `identity` installed. `identity` is a name
    /// already mapped into the manager's identity space — usually by
    /// [`PackageManager::package_identity`] for a declared entry.
    pub fn contains(&self, identity: &str) -> bool {
        self.identities.contains(identity)
    }

    /// Every installed package, in the identity space the planner diffs in.
    pub fn identities(&self) -> &HashSet<String> {
        &self.identities
    }

    /// Every installed package as the manager listed it, with its version.
    /// Names here are NOT folded — a display-case listing is what the scan and
    /// status surfaces render.
    pub fn listed(&self) -> &[PackageInfo] {
        &self.listed
    }
}

/// Per-manager enumeration entries, keyed by REGISTERED manager name.
///
/// The name rather than [`crate::manager_family`]: `brew` and `brew-cask` list
/// different populations with different commands, so collapsing them onto one
/// entry would answer a question about casks with an answer about formulae.
///
/// Locking is two-level and deliberately so. The outer map lock is held only
/// long enough to hand out a slot, never across an enumeration, so a slow `apt`
/// listing cannot block a lane asking about `npm`. Each slot then has its own
/// lock, which IS held across its manager's enumeration — two threads racing
/// for the same manager should produce one spawn, not two, which is the same
/// one-operation-per-manager rule the apply lanes run under.
#[derive(Default)]
pub struct InstalledEnumerations {
    slots: Mutex<HashMap<String, Arc<Mutex<Option<CachedEnumeration>>>>>,
}

pub(super) struct CachedEnumeration {
    pub(super) generation: u64,
    pub(super) packages: Arc<InstalledPackages>,
}

impl InstalledEnumerations {
    pub(super) fn slot(&self, manager: &str) -> Arc<Mutex<Option<CachedEnumeration>>> {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(slots.entry(manager.to_string()).or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_managers_enumeration_does_not_shut_another_manager_out() {
        let memo = Arc::new(InstalledEnumerations::default());
        let apt = memo.slot("apt");
        // Stands in for an enumeration in flight: `installed_for` holds exactly
        // this lock across the manager's spawn.
        let _in_flight = apt.lock().unwrap_or_else(|e| e.into_inner());

        let (tx, rx) = std::sync::mpsc::channel();
        let other = Arc::clone(&memo);
        std::thread::spawn(move || {
            let npm = other.slot("npm");
            drop(npm.lock().unwrap_or_else(|e| e.into_inner()));
            let _ = tx.send(());
        });

        // The timeout is a deadlock escape, not a timing assertion: under one
        // map-wide lock held across the enumeration, nothing is ever sent.
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok(),
            "a second manager must be reachable while the first is enumerating"
        );
    }

    #[test]
    fn a_manager_gets_the_same_slot_every_time() {
        let memo = InstalledEnumerations::default();
        assert!(Arc::ptr_eq(&memo.slot("apt"), &memo.slot("apt")));
        assert!(!Arc::ptr_eq(&memo.slot("apt"), &memo.slot("npm")));
    }
}
