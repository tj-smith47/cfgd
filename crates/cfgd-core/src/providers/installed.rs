//! The per-run memo of what each package manager reports as installed.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::{PackageInfo, PackageManager};
use crate::errors::Result;

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
/// `Default` is the EMPTY listing — what a caller that could not read a
/// manager judges against, so the fail-open arm and the read arm share one
/// predicate instead of two filters.
#[derive(Default)]
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
/// long enough to hand out a slot, never across an enumeration, so an `apt`
/// listing in flight cannot hold up a question about `npm`. Each slot then has
/// its own lock, which IS held across its manager's enumeration — two callers
/// racing for the same manager should produce one spawn, not two, which is the
/// same one-operation-per-manager rule the apply lanes run under.
///
/// The whole protocol lives in `InstalledEnumerations::get_or_enumerate`
/// rather than in the caller, so the tests that pin it drive the same code
/// `PackageContext::installed_for` does. `PackageContext` borrows its printer
/// and state store and is therefore neither `Send` nor `Sync`; this type is
/// both, and is the only half of the memo that can be put under two threads.
#[derive(Default)]
pub struct InstalledEnumerations {
    slots: Mutex<HashMap<String, Arc<Mutex<Option<CachedEnumeration>>>>>,
}

pub(super) struct CachedEnumeration {
    pub(super) generation: u64,
    pub(super) computed: std::time::Instant,
    pub(super) packages: Arc<InstalledPackages>,
}

/// How long an enumeration stands before it is asked again, bounding the
/// staleness cfgd cannot see coming.
///
/// The generation counter covers every install, uninstall, provision and
/// lifecycle script cfgd performs itself, and for a memo that dies with its
/// context that is the whole space of events. It is NOT the whole space for a
/// holder that outlives one unit of work: the MCP server answers tool calls for
/// as long as its client is connected and none of them installs anything, so
/// the only thing that can change what `brew list` would say is a human
/// installing a package in another terminal — which bumps nothing. Without a
/// ceiling, the second `scan_installed_packages` of such a session would report
/// the inventory taken at the first, for the life of the server.
///
/// Thirty seconds, the same ceiling [`crate::command_path`]'s memo carries and
/// for the same reason: long enough that a burst of questions inside one
/// operation costs one listing, short enough that a session held open across a
/// human's install re-asks.
const ENUMERATION_MEMO_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Millisecond override of [`ENUMERATION_MEMO_TTL`], or [`u64::MAX`] for "no
/// override". It exists so a test never depends on wall time: a test asserting
/// that a memoized enumeration STANDS pins the TTL out of reach, and one
/// asserting that it expires pins it to zero.
#[cfg(any(test, feature = "test-helpers"))]
static ENUMERATION_MEMO_TTL_OVERRIDE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX);

/// How long an enumeration stands, honouring the test override.
fn enumeration_memo_ttl() -> std::time::Duration {
    #[cfg(any(test, feature = "test-helpers"))]
    {
        let millis = ENUMERATION_MEMO_TTL_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed);
        if millis != u64::MAX {
            return std::time::Duration::from_millis(millis);
        }
    }
    ENUMERATION_MEMO_TTL
}

/// Pin the enumeration TTL, or hand back the default with `None`. Returns what
/// was pinned before, so a guard can put it back.
///
/// Reach for it through `test_helpers::EnumerationMemoTtlGuard`, never directly.
#[cfg(any(test, feature = "test-helpers"))]
pub(crate) fn set_enumeration_memo_ttl_override(millis: Option<u64>) -> Option<u64> {
    let prior = ENUMERATION_MEMO_TTL_OVERRIDE.swap(
        millis.unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
    (prior != u64::MAX).then_some(prior)
}

impl InstalledEnumerations {
    /// `manager`'s entry, enumerating through `enumerate` when there is none
    /// for the current resolution generation.
    ///
    /// `enumerate` runs under `manager`'s own slot lock and under no other, so
    /// a second caller asking about the SAME manager waits for this answer
    /// instead of spawning a second listing, while a caller asking about any
    /// other manager is not held up at all. An error is not recorded — the next
    /// caller asks again.
    ///
    /// An entry stands only while BOTH keys hold: the resolution generation it
    /// was taken under (every change cfgd itself makes) and
    /// [`ENUMERATION_MEMO_TTL`] (the ceiling on a change cfgd cannot see).
    pub(super) fn get_or_enumerate(
        &self,
        manager: &str,
        enumerate: impl FnOnce() -> Result<InstalledPackages>,
    ) -> Result<Arc<InstalledPackages>> {
        let slot = self.slot(manager);
        let mut cached = slot.lock().unwrap_or_else(|e| e.into_inner());
        // Read under the slot lock, so a mutation that landed while this caller
        // waited for another's enumeration voids that answer here rather than
        // being served it.
        let generation = crate::command_resolution_generation();
        let ttl = enumeration_memo_ttl();
        if let Some(hit) = cached
            .as_ref()
            .filter(|c| c.generation == generation && c.computed.elapsed() < ttl)
        {
            return Ok(Arc::clone(&hit.packages));
        }
        let packages = Arc::new(enumerate()?);
        *cached = Some(CachedEnumeration {
            generation,
            computed: std::time::Instant::now(),
            packages: Arc::clone(&packages),
        });
        Ok(packages)
    }

    fn slot(&self, manager: &str) -> Arc<Mutex<Option<CachedEnumeration>>> {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(slots.entry(manager.to_string()).or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Condvar;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn listing(names: &[&str]) -> InstalledPackages {
        InstalledPackages {
            identities: names.iter().map(|n| (*n).to_string()).collect(),
            listed: names
                .iter()
                .map(|n| PackageInfo {
                    name: (*n).to_string(),
                    version: "unknown".to_string(),
                })
                .collect(),
        }
    }

    /// A gate an enumeration blocks on, so a test decides when a listing
    /// "returns" without a sleep standing in for it.
    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        opened: Condvar,
    }

    impl Gate {
        fn wait(&self) {
            let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
            while !*open {
                open = self.opened.wait(open).unwrap_or_else(|e| e.into_inner());
            }
        }

        fn open(&self) {
            *self.open.lock().unwrap_or_else(|e| e.into_inner()) = true;
            self.opened.notify_all();
        }
    }

    #[test]
    fn one_managers_enumeration_does_not_shut_another_manager_out() {
        let memo = Arc::new(InstalledEnumerations::default());
        let gate = Arc::new(Gate::default());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (npm_done_tx, npm_done_rx) = std::sync::mpsc::channel();

        let apt_memo = Arc::clone(&memo);
        let apt_gate = Arc::clone(&gate);
        let apt = std::thread::spawn(move || {
            apt_memo.get_or_enumerate("apt", || {
                entered_tx.send(()).ok();
                apt_gate.wait();
                Ok(listing(&["curl"]))
            })
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("apt's enumeration must start");

        let npm_memo = Arc::clone(&memo);
        let npm = std::thread::spawn(move || {
            let answered = npm_memo
                .get_or_enumerate("npm", || Ok(listing(&["left-pad"])))
                .is_ok();
            npm_done_tx.send(answered).ok();
        });

        // The timeout is a deadlock escape, not a timing assertion — the
        // assertion is on the value received. Hold the map lock across the
        // enumeration instead of the slot lock and nothing is ever sent,
        // because apt is still inside its listing.
        let npm_answered = npm_done_rx.recv_timeout(std::time::Duration::from_secs(5));
        gate.open();
        apt.join().expect("apt thread").expect("apt enumeration");
        npm.join().expect("npm thread");
        assert_eq!(
            npm_answered.ok(),
            Some(true),
            "a second manager must be answerable while the first is enumerating"
        );
    }

    #[test]
    #[serial_test::serial(enumeration_memo)]
    fn two_callers_racing_for_one_manager_enumerate_it_once() {
        // A generation bump from any other test in this binary legitimately
        // retires the first caller's entry, which would make the second
        // re-enumerate — so the count is only measurable while the generation
        // holds still.
        // "Enumerated once" is also a claim that the first answer had not
        // aged out by the time the second caller read it, so the ceiling is
        // pinned out of reach rather than trusted to be longer than the gate.
        let _ttl = crate::test_helpers::EnumerationMemoTtlGuard::never_expires();
        let enumerations = crate::test_helpers::measured_in_a_stable_generation(|| {
            let memo = Arc::new(InstalledEnumerations::default());
            let gate = Arc::new(Gate::default());
            let enumerations = Arc::new(AtomicUsize::new(0));
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();

            let ask = |memo: Arc<InstalledEnumerations>| {
                let gate = Arc::clone(&gate);
                let enumerations = Arc::clone(&enumerations);
                let entered_tx = entered_tx.clone();
                std::thread::spawn(move || {
                    memo.get_or_enumerate("apt", || {
                        enumerations.fetch_add(1, Ordering::SeqCst);
                        entered_tx.send(()).ok();
                        // Both racers block here under a broken protocol; the
                        // gate is opened for all of them, so a regression fails
                        // the count assertion rather than hanging the suite.
                        gate.wait();
                        Ok(listing(&["curl"]))
                    })
                    .map(|packages| packages.contains("curl"))
                })
            };

            let first = ask(Arc::clone(&memo));
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("the first caller's enumeration must start");
            let second = ask(Arc::clone(&memo));
            gate.open();

            assert_eq!(first.join().expect("first thread").ok(), Some(true));
            assert_eq!(second.join().expect("second thread").ok(), Some(true));
            enumerations.load(Ordering::SeqCst)
        });

        assert_eq!(
            enumerations, 1,
            "the second caller must wait for the first's answer, not spawn its own listing"
        );
    }

    /// `u64::MAX` millis is the "no override" sentinel. A pin asking for a
    /// ceiling that large means "out of reach", so it must not fold back into
    /// the 30s default the caller pinned to escape.
    #[test]
    #[serial_test::serial(enumeration_memo)]
    fn a_ceiling_pinned_at_the_sentinel_is_still_a_pin() {
        let _ttl = crate::test_helpers::EnumerationMemoTtlGuard::pinned(
            std::time::Duration::from_millis(u64::MAX),
        );
        assert!(enumeration_memo_ttl() > ENUMERATION_MEMO_TTL);
    }

    #[test]
    fn a_manager_gets_the_same_slot_every_time() {
        let memo = InstalledEnumerations::default();
        assert!(Arc::ptr_eq(&memo.slot("apt"), &memo.slot("apt")));
        assert!(!Arc::ptr_eq(&memo.slot("apt"), &memo.slot("npm")));
    }
}
