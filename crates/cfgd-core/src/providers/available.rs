//! The process-scoped memo of what each package manager OFFERS.
//!
//! Sibling of [`super::installed`], which memoizes what each manager HAS. The
//! two answer different questions and are keyed differently: an enumeration is
//! one listing per manager, while an offer is one query per package, so this
//! memo is keyed by `(manager, package)` and lives for the process rather than
//! for a [`super::PackageContext`].
//!
//! Process-scoped deliberately. `available_version` is asked from
//! [`crate::modules::resolve_package`] (a free function reached with nothing but
//! a manager map), from the reconciler's display fill, and from two CLI
//! introspection surfaces — none of which share a context object, and threading
//! one through every module-resolution signature would buy nothing the
//! generation counter does not already give. That counter is what makes a
//! process-scoped answer safe: every install, uninstall, provision and lifecycle
//! script cfgd performs bumps it, retiring every entry taken before it.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::errors::Result;

struct CachedVersion {
    generation: u64,
    computed: Instant,
    version: Option<String>,
}

static AVAILABLE_VERSIONS: OnceLock<Mutex<HashMap<(String, String), CachedVersion>>> =
    OnceLock::new();

/// How long an offer stands before the manager is asked again.
///
/// The same thirty seconds [`crate::command_path`]'s memo and the installed
/// enumeration carry, for the same reason: the generation counter covers every
/// change cfgd makes itself, and this ceiling covers the one it cannot see — a
/// human running `brew upgrade` in another terminal while a daemon or an MCP
/// session is held open.
const AVAILABLE_VERSION_MEMO_TTL: Duration = Duration::from_secs(30);

/// Entry ceiling before the map is dropped wholesale. Keys are declared package
/// names, so a config would have to declare a thousand packages to reach it; it
/// exists so a daemon running for weeks cannot grow the map without bound.
const AVAILABLE_VERSION_MEMO_CAP: usize = 1024;

/// Millisecond override of [`AVAILABLE_VERSION_MEMO_TTL`], or [`u64::MAX`] for
/// "no override". A test asserting that a memoized offer STANDS pins the ceiling
/// out of reach; one asserting that it expires pins it to zero. Neither then
/// depends on how long two adjacent statements took.
#[cfg(any(test, feature = "test-helpers"))]
static AVAILABLE_VERSION_MEMO_TTL_OVERRIDE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX);

fn available_version_memo_ttl() -> Duration {
    #[cfg(any(test, feature = "test-helpers"))]
    {
        let millis = AVAILABLE_VERSION_MEMO_TTL_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed);
        if millis != u64::MAX {
            return Duration::from_millis(millis);
        }
    }
    AVAILABLE_VERSION_MEMO_TTL
}

/// Pin the offer TTL, or hand back the default with `None`. Returns what was
/// pinned before, so a guard can put it back.
///
/// Reach for it through `test_helpers::AvailableVersionMemoTtlGuard`, never
/// directly.
#[cfg(any(test, feature = "test-helpers"))]
pub(crate) fn set_available_version_memo_ttl_override(millis: Option<u64>) -> Option<u64> {
    let prior = AVAILABLE_VERSION_MEMO_TTL_OVERRIDE.swap(
        millis.unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
    (prior != u64::MAX).then_some(prior)
}

fn memo() -> MutexGuard<'static, HashMap<(String, String), CachedVersion>> {
    // A poisoned lock still holds usable answers: a panic on another thread is
    // no reason to stop answering what a manager offers.
    AVAILABLE_VERSIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// `manager`'s answer for `package`, asking `query` when there is none that
/// still stands.
///
/// The lock is NOT held across `query` — a version query spawns a subprocess and
/// may reach the network, and holding a process-wide lock across it would
/// serialize every other manager's questions behind it. The cost is that two
/// threads racing for the same key may both ask; the alternative (a per-key slot
/// lock) buys one saved subprocess in a race that the callers — a module
/// resolution and a display fill, both single-threaded per run — do not have.
///
/// An error is not recorded: the next caller asks again.
pub(super) fn get_or_query(
    manager: &str,
    package: &str,
    query: impl FnOnce() -> Result<Option<String>>,
) -> Result<Option<String>> {
    let generation = crate::command_resolution_generation();
    let ttl = available_version_memo_ttl();
    let key = (manager.to_string(), package.to_string());
    if let Some(hit) = memo()
        .get(&key)
        .filter(|c| c.generation == generation && c.computed.elapsed() < ttl)
    {
        return Ok(hit.version.clone());
    }
    let version = query()?;
    let mut memo = memo();
    if memo.len() >= AVAILABLE_VERSION_MEMO_CAP {
        memo.clear();
    }
    memo.insert(
        key,
        CachedVersion {
            // Re-read rather than reused: a bootstrap or an install that landed
            // while this query ran must void the answer it produced, not be
            // recorded as having happened before it.
            generation: crate::command_resolution_generation(),
            computed: Instant::now(),
            version: version.clone(),
        },
    );
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `u64::MAX` millis is the "no override" sentinel. A pin asking for a
    /// ceiling that large means "out of reach", so it must not fold back into
    /// the default the caller pinned to escape.
    #[test]
    #[serial_test::serial(available_version_memo)]
    fn a_ceiling_pinned_at_the_sentinel_is_still_a_pin() {
        let _ttl = crate::test_helpers::AvailableVersionMemoTtlGuard::pinned(
            Duration::from_millis(u64::MAX),
        );
        assert!(available_version_memo_ttl() > AVAILABLE_VERSION_MEMO_TTL);
    }

    #[test]
    #[serial_test::serial(available_version_memo)]
    fn an_expired_offer_is_asked_for_again() {
        let _ttl = crate::test_helpers::AvailableVersionMemoTtlGuard::always_expired();
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let ask = || {
            asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some("1.0".to_string()))
        };
        let manager = "expiring-mgr";
        assert_eq!(
            get_or_query(manager, "jq", ask).unwrap().as_deref(),
            Some("1.0")
        );
        assert_eq!(
            get_or_query(manager, "jq", ask).unwrap().as_deref(),
            Some("1.0")
        );
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// A manager that reports nothing for a package is still an answer, and
    /// re-asking is the expensive half — a miss costs the same subprocess a hit
    /// does.
    #[test]
    #[serial_test::serial(available_version_memo)]
    fn a_manager_that_offers_nothing_is_not_asked_twice() {
        let _ttl = crate::test_helpers::AvailableVersionMemoTtlGuard::never_expires();
        let asked = crate::test_helpers::measured_in_a_stable_generation(|| {
            let asked = std::sync::atomic::AtomicUsize::new(0);
            let ask = || {
                asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(None)
            };
            // A key no other test in this binary shares, so the count describes
            // this test's own questions.
            let manager = "offers-nothing-mgr";
            assert!(get_or_query(manager, "absent", ask).unwrap().is_none());
            assert!(get_or_query(manager, "absent", ask).unwrap().is_none());
            asked.load(std::sync::atomic::Ordering::SeqCst)
        });
        assert_eq!(asked, 1);
    }

    #[test]
    #[serial_test::serial(available_version_memo)]
    fn an_error_is_never_recorded_as_an_answer() {
        let _ttl = crate::test_helpers::AvailableVersionMemoTtlGuard::never_expires();
        let manager = "erroring-mgr";
        assert!(
            get_or_query(manager, "jq", || Err(crate::errors::CfgdError::Io(
                std::io::Error::other("nope")
            )))
            .is_err()
        );
        assert_eq!(
            get_or_query(manager, "jq", || Ok(Some("2.0".to_string())))
                .unwrap()
                .as_deref(),
            Some("2.0")
        );
    }
}
