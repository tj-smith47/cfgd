//! What a desired-state derivation READ, and whether any of it has moved since.
//!
//! A daemon tick that re-parses an unchanged `cfgd.yaml`, re-resolves an
//! unchanged profile chain and re-composes an unchanged source cache produces,
//! by construction, the object it produced last tick. Reusing it needs one
//! fact: that none of the files the derivation read has changed. The recorder
//! collects that set from the reads THEMSELVES — every manifest reader in the
//! config, source and module domains reports here — so the set is what was
//! actually opened rather than a layout the caller guessed at.
//!
//! Absence is recorded as a state of its own: a `modules.lock` that does not
//! exist is an input whose later appearance changes the resolution, so it is
//! stamped `None` and its arrival reads as a change.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What an input looked like when it was read: its modification time (where the
/// host reports one) and its length.
///
/// The same `(mtime, len)` pair `crate::packages::ManifestCache` judges a
/// re-read on, for the same reason: it is one `stat`, it needs no read of the
/// file, and every writer in cfgd lands content through a temp-and-rename that
/// stamps a fresh mtime.
type InputStamp = (Option<SystemTime>, u64);

/// One input path and the stamp it carried when the derivation read it.
type Entry = (PathBuf, Option<InputStamp>);

fn stamp_of(path: &Path) -> Option<InputStamp> {
    std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.modified().ok(), meta.len()))
}

thread_local! {
    /// Every recorder currently collecting on this thread. A read reports to
    /// all of them, so a nested derivation cannot swallow an outer one's
    /// inputs.
    static FRAMES: RefCell<Vec<Vec<Entry>>> = const { RefCell::new(Vec::new()) };
}

/// Report that `path` is an input of the derivation in flight.
///
/// A no-op when nothing is recording, which is every path but the daemon's
/// tick derivation. Call it BEFORE the read (and before any existence check),
/// so a file rewritten between the stamp and the read is seen as changed on the
/// next comparison instead of being recorded as already-current.
pub fn record_config_input(path: &Path) {
    FRAMES.with(|frames| {
        let mut frames = frames.borrow_mut();
        if frames.is_empty() {
            return;
        }
        let stamp = stamp_of(path);
        for frame in frames.iter_mut() {
            if frame.iter().any(|(p, _)| p == path) {
                continue;
            }
            frame.push((path.to_path_buf(), stamp));
        }
    });
}

/// The inputs one derivation read, and the stamps they carried.
#[derive(Debug, Clone, Default)]
pub struct ConfigInputs {
    entries: Vec<Entry>,
}

impl ConfigInputs {
    /// Whether every recorded input still carries the stamp it was read with.
    ///
    /// An EMPTY set is never unchanged. A derivation that recorded nothing read
    /// nothing this recorder saw, and the one thing a reuse decision must never
    /// do is answer "nothing moved" about a set it does not have.
    pub fn unchanged(&self) -> bool {
        !self.entries.is_empty()
            && self
                .entries
                .iter()
                .all(|(path, stamp)| stamp_of(path) == *stamp)
    }

    /// How many distinct paths the derivation read.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The recorded paths, in the order they were first read.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.entries.iter().map(|(path, _)| path.as_path())
    }
}

/// Collects the inputs of the derivation that runs while it is alive.
///
/// RAII: the frame is popped on drop, so an unwinding derivation cannot leave
/// the thread recording into a set nobody will read.
pub struct ConfigInputRecorder {
    finished: bool,
}

impl ConfigInputRecorder {
    /// Begin recording on this thread.
    pub fn start() -> Self {
        FRAMES.with(|frames| frames.borrow_mut().push(Vec::new()));
        Self { finished: false }
    }

    /// Stop recording and take what was collected.
    pub fn finish(mut self) -> ConfigInputs {
        self.finished = true;
        let entries = FRAMES.with(|frames| frames.borrow_mut().pop().unwrap_or_default());
        ConfigInputs { entries }
    }
}

impl Drop for ConfigInputRecorder {
    fn drop(&mut self) {
        if !self.finished {
            FRAMES.with(|frames| {
                frames.borrow_mut().pop();
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unarmed_thread_records_nothing() {
        record_config_input(Path::new("/nonexistent/unarmed"));
        assert!(FRAMES.with(|f| f.borrow().is_empty()));
    }

    #[test]
    fn a_recorded_file_reads_unchanged_until_it_is_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        std::fs::write(&path, "a").unwrap();

        let rec = ConfigInputRecorder::start();
        record_config_input(&path);
        let inputs = rec.finish();

        assert_eq!(inputs.len(), 1);
        assert!(inputs.unchanged());

        std::fs::write(&path, "bb").unwrap();
        assert!(
            !inputs.unchanged(),
            "a rewritten input must read as changed"
        );
    }

    #[test]
    fn an_absent_input_changes_when_it_appears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("modules.lock");

        let rec = ConfigInputRecorder::start();
        record_config_input(&path);
        let inputs = rec.finish();

        assert!(inputs.unchanged());
        std::fs::write(&path, "modules: []").unwrap();
        assert!(
            !inputs.unchanged(),
            "an input that did not exist must read as changed once it does"
        );
    }

    #[test]
    fn an_empty_input_set_is_never_unchanged() {
        assert!(!ConfigInputs::default().unchanged());
        let rec = ConfigInputRecorder::start();
        assert!(!rec.finish().unchanged());
    }

    #[test]
    fn one_path_read_twice_is_recorded_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfgd.yaml");
        std::fs::write(&path, "a").unwrap();

        let rec = ConfigInputRecorder::start();
        record_config_input(&path);
        record_config_input(&path);
        assert_eq!(rec.finish().len(), 1);
    }

    #[test]
    fn a_nested_recorder_does_not_swallow_the_outer_frame() {
        let dir = tempfile::tempdir().unwrap();
        let outer_path = dir.path().join("outer.yaml");
        let inner_path = dir.path().join("inner.yaml");
        std::fs::write(&outer_path, "a").unwrap();
        std::fs::write(&inner_path, "a").unwrap();

        let outer = ConfigInputRecorder::start();
        record_config_input(&outer_path);
        let inner = ConfigInputRecorder::start();
        record_config_input(&inner_path);
        let inner_inputs = inner.finish();
        let outer_inputs = outer.finish();

        assert_eq!(inner_inputs.len(), 1);
        assert_eq!(outer_inputs.len(), 2);
    }

    #[test]
    fn a_dropped_recorder_leaves_no_frame_behind() {
        {
            let _rec = ConfigInputRecorder::start();
            record_config_input(Path::new("/nonexistent/dropped"));
        }
        assert!(FRAMES.with(|f| f.borrow().is_empty()));
    }
}
