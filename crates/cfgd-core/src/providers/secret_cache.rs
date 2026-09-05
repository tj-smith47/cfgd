//! One resolution per (backend, reference), for the length of ONE unit of work.
//!
//! A secret backend resolves by SPAWNING: `sops -d`, `op read`, `bw get`,
//! `vault kv get`. The declarations that reach for one are per-OCCURRENCE — a
//! reference written into a file target and into an env var is two secret
//! actions, and a reference interpolated into five templates is five
//! substitutions — so the same value was fetched once per place it appeared,
//! each spawn a process and, for the hosted providers, a network round-trip.
//!
//! Unlike every other memo in this crate, this one holds PLAINTEXT. That
//! decides its scope: it is owned by the object that performs one run's work
//! (the reconciler for a run's secret actions, the file manager for a run's
//! template renders) and it dies with that object. It is never process-global,
//! never persisted, never logged, and it carries no `Debug` that could print a
//! value — so the longest a resolved secret lives in memory is the run that
//! asked for it, and a rotated secret is re-fetched by the very next run.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use secrecy::SecretString;

use crate::errors::Result;

/// What identifies a resolution: the backend or provider that produced the
/// value, and the reference it was asked for. Two backends answering the same
/// reference are two different questions.
type CacheKey = (String, String);

/// Resolved secrets for one run, keyed by (backend, reference).
#[derive(Default)]
pub struct SecretCache {
    entries: Mutex<HashMap<CacheKey, Arc<SecretString>>>,
}

impl SecretCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The value `backend` resolves for `reference`, fetching through `resolve`
    /// only when this run has not already asked.
    ///
    /// An error is never recorded — the next ask spawns again, matching every
    /// other memo in this crate. The lock is NOT held across `resolve`: a
    /// backend spawns a child process, and holding a lock across that is how a
    /// deadlock is built. Two threads racing on one key therefore both fetch and
    /// agree on the result, which costs one spawn and risks nothing.
    pub fn resolve_with(
        &self,
        backend: &str,
        reference: &str,
        resolve: impl FnOnce() -> Result<SecretString>,
    ) -> Result<Arc<SecretString>> {
        let key = (backend.to_string(), reference.to_string());
        if let Some(hit) = self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return Ok(Arc::clone(hit));
        }
        let value = Arc::new(resolve()?);
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, Arc::clone(&value));
        Ok(value)
    }

    /// How many distinct (backend, reference) pairs this run has resolved.
    /// Never the values themselves.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Elides the entries outright. A `Debug` that printed them would put every
/// resolved secret into whatever log line formatted the holder.
impl std::fmt::Debug for SecretCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretCache")
            .field("entries", &self.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn one_reference_resolves_once_however_many_occurrences_ask() {
        let cache = SecretCache::new();
        let spawns = AtomicUsize::new(0);
        let fetch = || {
            spawns.fetch_add(1, Ordering::SeqCst);
            Ok(SecretString::from("hunter2".to_string()))
        };

        for _ in 0..5 {
            let value = cache
                .resolve_with("1password", "op://vault/item", fetch)
                .unwrap();
            assert_eq!(value.expose_secret(), "hunter2");
        }
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn two_backends_answering_one_reference_are_two_questions() {
        let cache = SecretCache::new();
        let spawns = AtomicUsize::new(0);
        let fetch = || {
            spawns.fetch_add(1, Ordering::SeqCst);
            Ok(SecretString::from("v".to_string()))
        };

        cache.resolve_with("sops", "db/password", fetch).unwrap();
        cache.resolve_with("age", "db/password", fetch).unwrap();
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn a_failed_resolution_is_never_recorded() {
        let cache = SecretCache::new();
        let err = || {
            Err(crate::errors::SecretError::UnresolvableRef {
                reference: "missing".to_string(),
            }
            .into())
        };
        assert!(cache.resolve_with("sops", "missing", err).is_err());
        assert!(cache.is_empty());

        let value = cache
            .resolve_with("sops", "missing", || {
                Ok(SecretString::from("late".to_string()))
            })
            .unwrap();
        assert_eq!(value.expose_secret(), "late");
    }

    #[test]
    fn debug_never_prints_a_value() {
        let cache = SecretCache::new();
        cache
            .resolve_with("sops", "db/password", || {
                Ok(SecretString::from("s3cr3t".to_string()))
            })
            .unwrap();
        let rendered = format!("{cache:?}");
        assert!(!rendered.contains("s3cr3t"), "{rendered}");
        assert!(!rendered.contains("db/password"), "{rendered}");
    }
}
