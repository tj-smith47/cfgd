//! Bucket (c): Status × Role coverage. 7 cases — `Status` rendered at every
//! Role × default theme × Normal verbosity. Locks the role-glyph mapping
//! into goldens.

use crate::golden_doc;
use crate::output::Role;

golden_doc!(bucket_c, ok, |p, cap| {
    p.status_simple(Role::Ok, "ok msg");
});
golden_doc!(bucket_c, warn, |p, cap| {
    p.status_simple(Role::Warn, "warn msg");
});
golden_doc!(bucket_c, fail, |p, cap| {
    p.status_simple(Role::Fail, "fail msg");
});
golden_doc!(bucket_c, pending, |p, cap| {
    p.status_simple(Role::Pending, "pending msg");
});
golden_doc!(bucket_c, running, |p, cap| {
    p.status_simple(Role::Running, "running msg");
});
golden_doc!(bucket_c, skipped, |p, cap| {
    p.status_simple(Role::Skipped, "skipped msg");
});
golden_doc!(bucket_c, info, |p, cap| {
    p.status_simple(Role::Info, "info msg");
});
