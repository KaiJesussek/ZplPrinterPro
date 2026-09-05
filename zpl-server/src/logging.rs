//! Central logging choke point.
//!
//! Every diagnostic the server emits goes through [`log!`] rather than a bare
//! `eprintln!`, so there is exactly one place that decides where a line lands.
//! That place is stderr - on every OS, in both the foreground and the Windows
//! service. On Windows each line is *additionally* written to the Windows
//! Event Log (the Application log, source `zpl-server`), because a service has
//! no console of its own. The event-log write is purely additive: console
//! output is never suppressed or redirected, so foreground behavior on every
//! OS is unchanged apart from the new Windows event-log entries.
//!
//! Off Windows the event-log branch is compiled out and [`emit`] is a plain
//! `eprintln!`.

#[cfg(windows)]
use log::Log as _;
#[cfg(windows)]
use std::sync::LazyLock;

/// The event-log source name: appears under `Applications and Services Logs >
/// Application` in Event Viewer.
#[cfg(windows)]
const EVENTLOG_SOURCE: &str = "zpl-server";

/// Shared Windows Event Log handle, opened lazily on first use and held for the
/// process. `EventLog` is `Send + Sync`, so every server task shares this one
/// handle (the FFI `ReportEventW` call is internally serialized).
///
/// `eventlog::register` records the source in the registry so Event Viewer
/// renders our (embedded) message templates instead of a raw string; it needs
/// administrator rights. A plain foreground run (no admin) simply skips it and
/// the handle below is still usable - the events just show the raw message.
#[cfg(windows)]
static EVENTLOG: LazyLock<Option<eventlog::EventLog>> = LazyLock::new(|| {
    let _ = eventlog::register(EVENTLOG_SOURCE);
    eventlog::EventLog::new(EVENTLOG_SOURCE, log::Level::Trace).ok()
});

/// On Windows, write one line to the Event Log as an Information event. Split
/// out of [`emit`] so the stderr write stays the single shared, always-on path.
#[cfg(windows)]
fn write_eventlog(msg: &str) {
    let Some(ev) = &*EVENTLOG else { return };
    // Bind the format args to a name: the `Arguments` temporary must outlive the
    // `Record` that borrows it, so it can't be inlined into `.args(...)`.
    let args = format_args!("{msg}");
    let rec = log::Record::builder()
        .level(log::Level::Info)
        .target(EVENTLOG_SOURCE)
        .args(args)
        .build();
    ev.log(&rec);
}

/// Write one line to stderr (always) and, on Windows, the Event Log. `msg` is
/// already formatted by the `log!` macro.
pub fn emit(msg: &str) {
    eprintln!("{msg}");
    #[cfg(windows)]
    write_eventlog(msg);
}

/// Format-and-log one diagnostic. A drop-in for `eprintln!` that routes the
/// line through the choke point above so it also reaches the Windows Event Log.
#[macro_export]
macro_rules! log {
    ($($t:tt)*) => {
        {
            let msg = format!($($t)*);
            $crate::logging::emit(&msg);
        }
    };
}
