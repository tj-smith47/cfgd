mod abort;
mod apply_lock;
mod constants;
mod encryption;
mod env_session;
mod file_io;
mod fs_perms;
mod git;
mod hashing;
mod paths;
mod process;
mod reconcile;
mod strings;
mod time;
mod yaml_merge;

pub use abort::*;
pub use apply_lock::*;
pub use constants::*;
pub use encryption::*;
pub use env_session::*;
pub use file_io::*;
pub use fs_perms::*;
pub use git::*;
pub use hashing::*;
pub use paths::*;
pub use process::*;
pub use reconcile::*;
pub use strings::*;
pub use time::*;
pub use yaml_merge::*;

// `home_dir_var` and `test_home_override` are `pub(crate)` in paths.rs (not `pub`),
// so the `pub use paths::*;` glob above can't re-export them — that glob only widens
// items whose source visibility is at least `pub`. Tests in cfgd-core/src/tests.rs
// use bare names via `use super::*;`, so we expose them at this level (and again at
// the crate root in lib.rs) under #[cfg(test)] only.
#[cfg(test)]
pub(crate) use paths::{home_dir_var, test_home_override};
