//! Windows Event Log `tracing` subscriber Layer.
//!
//! Opt-in alternative (or supplement) to the file-based daemon log at
//! `%LOCALAPPDATA%\cfgd\daemon.log`. The two coexist — when this layer is
//! enabled, every tracing event is delivered to BOTH sinks. See
//! `docs/daemon.md` for the user-facing toggle (`daemon.windowsEventLog`)
//! and the underlying `--enable-event-log` service argument.
//!
//! Implementation is a thin `unsafe` wrapper over `RegisterEventSourceW` /
//! `ReportEventW` / `DeregisterEventSource`. We deliberately avoid third-party
//! event-log crates because they are largely unmaintained and the surface
//! we need is small (~3 FFI calls).
//!
//! Event source registration is performed at service install time
//! (`install_windows_service`) so that Event Viewer can render messages
//! without "description not found" warnings. The registration points
//! `EventMessageFile` at the system-shipped `EventCreate.exe`, which carries
//! generic templates that echo the inserted strings — sufficient for our
//! free-form log lines without shipping a custom message-resource DLL.

#![cfg(windows)]

use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicPtr, Ordering};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::EventLog::{
    DeregisterEventSource, EVENTLOG_ERROR_TYPE, EVENTLOG_INFORMATION_TYPE, EVENTLOG_WARNING_TYPE,
    RegisterEventSourceW, ReportEventW,
};

/// Canonical event source name. Matches the registry key created by
/// `install_windows_service` under
/// `HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\<source>`.
pub const EVENT_SOURCE_NAME: &str = "cfgd";

/// Shared event-source HANDLE (`*mut c_void` on Windows). Stored as
/// `AtomicPtr` so it's `Send + Sync` without unsafe-impl gymnastics; the
/// underlying handle is reentrant from the OS's perspective once
/// `RegisterEventSourceW` returns.
static EVENT_SOURCE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Convert an `&str` to a NUL-terminated UTF-16 buffer suitable for the
/// `*W` (wide) Win32 entry points.
fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Register the event source one time per process. Idempotent — repeat
/// calls are no-ops.
fn register_source() -> HANDLE {
    let existing = EVENT_SOURCE.load(Ordering::Acquire);
    if !existing.is_null() {
        return existing;
    }
    let source = to_wide(EVENT_SOURCE_NAME);
    // SAFETY: `lpUNCServerName` of NULL means "local machine"; `source` is a
    // valid NUL-terminated wide string for the lifetime of this call.
    let handle = unsafe { RegisterEventSourceW(std::ptr::null(), source.as_ptr()) };
    if !handle.is_null() {
        EVENT_SOURCE.store(handle, Ordering::Release);
    }
    handle
}

/// Tear down the registered source. Called from the service stop path so we
/// don't leak a kernel handle on graceful shutdown. Safe to call without a
/// matching `register_source()`.
pub fn deregister_source() {
    let handle = EVENT_SOURCE.swap(std::ptr::null_mut(), Ordering::AcqRel);
    if !handle.is_null() {
        // SAFETY: we owned this handle from a successful RegisterEventSourceW.
        unsafe {
            DeregisterEventSource(handle);
        }
    }
}

/// Map a tracing `Level` to the Win32 `EVENTLOG_*_TYPE` severity flag.
fn level_to_event_type(level: &Level) -> u16 {
    match *level {
        Level::ERROR => EVENTLOG_ERROR_TYPE,
        Level::WARN => EVENTLOG_WARNING_TYPE,
        // INFO / DEBUG / TRACE all map to Information — Event Log only has
        // three "regular" severities. DEBUG/TRACE volume is filtered out by
        // the `EnvFilter` layer above us, not here.
        _ => EVENTLOG_INFORMATION_TYPE,
    }
}

/// Visitor that flattens an event's fields into a single human-readable
/// string suitable for ReportEventW's `lpStrings` slot.
struct MessageBuilder {
    out: String,
}

impl MessageBuilder {
    fn new() -> Self {
        Self { out: String::new() }
    }

    fn push(&mut self, name: &str, value: &dyn std::fmt::Debug) {
        if !self.out.is_empty() {
            self.out.push(' ');
        }
        if name == "message" {
            // The bare message field has no key prefix in fmt output; mirror
            // that here so downstream readers see the natural form.
            let _ = std::fmt::write(&mut self.out, format_args!("{:?}", value));
        } else {
            let _ = std::fmt::write(&mut self.out, format_args!("{}={:?}", name, value));
        }
    }
}

impl Visit for MessageBuilder {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.push(field.name(), value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field.name(), &value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field.name(), &value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field.name(), &value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field.name(), &value);
    }
}

/// `tracing-subscriber` Layer that mirrors every event into the Windows
/// Event Log under the `cfgd` source.
#[derive(Default)]
pub struct EventLogLayer;

impl<S> Layer<S> for EventLogLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let handle = register_source();
        if handle.is_null() {
            return;
        }

        let mut builder = MessageBuilder::new();
        event.record(&mut builder);

        let target = event.metadata().target();
        let composed = if target.is_empty() {
            builder.out
        } else {
            format!("[{target}] {}", builder.out)
        };

        let wide = to_wide(&composed);
        let strings: [*const u16; 1] = [wide.as_ptr()];

        // SAFETY: `handle` is a valid event source handle from
        // RegisterEventSourceW. `strings` points at one wide string that
        // lives for the duration of this call. Other pointers are the
        // documented NULL-acceptable variants.
        unsafe {
            ReportEventW(
                handle,
                level_to_event_type(event.metadata().level()),
                0, // wCategory: no category resource
                1, // dwEventID: single generic id; payload is the message string
                std::ptr::null_mut(),
                strings.len() as u16,
                0,
                strings.as_ptr(),
                std::ptr::null(),
            );
        }
    }
}
