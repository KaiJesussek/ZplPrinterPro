//! zpl-server binary: TCP print sessions plus the Task 21 HTTP API and
//! debounced state persistence in one process.
//!
//! On Windows the binary doubles as a Windows service: launched by the
//! Service Control Manager it serves and blocks until the SCM stops it;
//! launched any other way (double-click, `cargo run`, console) it runs in
//! the foreground exactly as before. Every other OS is unchanged.
#![forbid(unsafe_code)]

use std::sync::Arc;
use tokio::task::JoinHandle;

use zpl_server::archive::{Archive, DEFAULT_KEEP, DEFAULT_MAX_BYTES};
use zpl_server::http::{spawn_http, spawn_persistence};
use zpl_server::tcp::{spawn_tcp, AppState};

#[cfg(windows)]
mod service;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_port(key: &str, default: &str) -> std::io::Result<u16> {
    env_or(key, default)
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{key}: {e}")))
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> std::io::Result<T> {
    match std::env::var(key) {
        Ok(v) => v.parse().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{key}: '{v}'"))
        }),
        Err(_) => Ok(default),
    }
}

/// Bind the listeners and spawn the TCP, HTTP and persistence tasks.
///
/// Shared by the foreground path and the Windows service worker so both run
/// the exact same startup sequence and read the same environment variables.
/// Returns the two task handles the caller aborts on shutdown.
async fn start_server() -> std::io::Result<(JoinHandle<()>, JoinHandle<()>)> {
    let tcp_port = env_port("ZPL_TCP_PORT", "9100")?;
    let http_port = env_port("ZPL_HTTP_PORT", "8080")?;
    let archive_dir = env_or("ZPL_ARCHIVE_DIR", "./zpl-archive");
    let keep = env_parse("ZPL_ARCHIVE_KEEP", DEFAULT_KEEP)?;
    let max_bytes =
        env_parse("ZPL_ARCHIVE_MAX_MB", DEFAULT_MAX_BYTES / (1024 * 1024))? * (1024 * 1024);

    let archive = Archive::open_with(archive_dir.clone().into(), keep, max_bytes)?;
    let state = Arc::new(AppState::new(archive));
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", tcp_port)).await?;
    let tcp = spawn_tcp(listener, state.clone());
    let http = spawn_http(state.clone(), http_port).await?;
    spawn_persistence(state.clone());

    zpl_server::log!("zpl-server: tcp on {tcp_port}, http on {http_port} (archive at {archive_dir}, keep={keep}, max={} MiB); Ctrl-C to stop",
        max_bytes / (1024 * 1024));
    Ok((tcp, http))
}

/// Foreground entry: serve until Ctrl-C, then abort the spawned tasks.
async fn async_main() -> std::io::Result<()> {
    let (tcp, http) = start_server().await?;
    tokio::signal::ctrl_c().await?;
    zpl_server::log!("zpl-server: shutting down");
    tcp.abort();
    http.abort();
    Ok(())
}

fn main() -> std::io::Result<()> {
    // Windows only: when the Service Control Manager launches this binary,
    // serve as the service. `try_run_service` blocks until the SCM stops the
    // service and returns `true`, so we exit here. Launched any other way it
    // returns `false` and we fall through to the foreground path below. This
    // whole block is compiled out on every other OS.
    #[cfg(windows)]
    if service::try_run_service() {
        return Ok(());
    }

    // Foreground on every OS - identical to the previous behavior.
    tokio::runtime::Runtime::new()?.block_on(async_main())
}
