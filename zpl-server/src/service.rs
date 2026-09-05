//! Windows service worker. Compiled only on Windows via `#[cfg(windows)] mod
//! service` in `main.rs`; the whole `windows-service` crate is itself a
//! no-op off-Windows, so this file never compiles on any other OS.
//!
//! Turns the shared `start_server` startup into a Service Control Manager
//! citizen: report `Running`, serve until the SCM sends `Stop`, then report
//! `Stopped` and abort the server tasks.

use std::ffi::OsString;
use std::sync::mpsc;
use std::time::Duration;

use tokio::runtime::Runtime;
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher, Error, Result,
};

/// Name the binary must be registered under with the SCM (`sc create`).
const SERVICE_NAME: &str = "zpl-server";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;
// Generate the low-level service entry point the SCM invokes: `ffi_service_main`
// parses the SCM's arguments and hands them to `my_service_main` on a background thread.
define_windows_service!(ffi_service_main, my_service_main);

/// `main`'s service gate. Tries to run this binary as a Windows service.
///
/// `service_dispatcher::start` blocks until the SCM stops the service when we
/// *were* launched by the SCM, so `true` means the service ran and shut down
/// cleanly and the process should exit here. When we were *not* launched by the
/// SCM (double-click, `cargo run`, console), `StartServiceCtrlDispatcherW`
/// refuses and `start` returns `Err`, so we report `false` and `main` falls
/// through to the foreground path.
pub fn try_run_service() -> bool {
    match service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        Ok(()) => true,
        Err(e) => {
            zpl_server::log!(
                "zpl-server: not running under the Windows SCM ({e}); continuing in the foreground"
            );
            false
        }
    }
}

/// Called by the SCM on a background thread once `try_run_service` registered us.
fn my_service_main(_arguments: Vec<OsString>) {
    if let Err(e) = run_service() {
        // No console under the SCM, so this is visible only when the service
        // is run from a console (e.g. for local testing).
        zpl_server::log!("zpl-server: service terminated with an error: {e}");
    }
}

fn run_service() -> Result<()> {
    // Bridge the SCM's Stop control (delivered on the control-handler thread)
    // to the tokio runtime below.
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            // Report current status. Must be accepted even as a no-op.
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            // Ask us to stop. Signal the runtime; ack immediately so the SCM
            // knows we accepted, then finish the graceful shutdown below.
            ServiceControl::Stop => {
                let _ = shutdown_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    // Register the control handler; the status handle reports state to the SCM.
    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;

    // The SCM owns the main thread and gives us a plain thread - no runtime -
    // so build our own here.
    let runtime = Runtime::new().map_err(Error::Winapi)?;
    runtime.block_on(async {
        // Report Running before serving so the SCM marks us started.
        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        // Same startup as the foreground path (reads the same env vars).
        let (tcp, http) = crate::start_server().await.map_err(Error::Winapi)?;

        // Serve until the SCM sends Stop. A disconnected channel (Err) also
        // means the handler is gone, i.e. shutdown - both outcomes stop now.
        let _ = shutdown_rx.recv();

        zpl_server::log!("zpl-server: service stopping");
        tcp.abort();
        http.abort();
        Ok(())
    })?;

    // Report Stopped to finish the lifecycle.
    status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    Ok(())
}
