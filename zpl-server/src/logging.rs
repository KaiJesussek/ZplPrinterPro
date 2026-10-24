//! Central logging choke point.
//!
//! Every diagnostic the server emits goes through [`log!`] rather than a bare
//! `eprintln!`, so there is exactly one place that decides where a line lands.
//! That place is stderr - on every OS, in both the foreground and the Windows
//! service. On Windows, lines are *additionally* written to the Windows Event
//! Log (the Application log, source `zpl-server`) **only when the process was
//! launched by the Service Control Manager** ([`set_service_mode`]), because a
//! service has no console of its own. A console launch keeps stderr as its
//! only sink and never touches the event log - so it never needs the admin
//! rights that registering the event source requires.
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
/// administrator rights - which the SCM service (Local System) has. A console
/// run never reaches this handle at all (see [`SERVICE_MODE`]).

/// Set once by the service worker when the SCM launched this process. Only
/// then does [`emit`] write the Event Log; a console launch keeps stderr as
/// its only sink and never registers or opens the event source.
static SERVICE_MODE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Mark this process as SCM-launched; call before the first log line.
pub fn set_service_mode() {
    SERVICE_MODE.store(true, std::sync::atomic::Ordering::Relaxed);
}

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
    if SERVICE_MODE.load(std::sync::atomic::Ordering::Relaxed) {
        write_eventlog(msg);
    }
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
