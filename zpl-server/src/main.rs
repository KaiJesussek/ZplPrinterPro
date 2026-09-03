//! zpl-server binary: TCP print sessions now; HTTP API from Task 21.
#![forbid(unsafe_code)]

use std::sync::Arc;

use zpl_server::archive::Archive;
use zpl_server::tcp::{spawn_tcp, AppState};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_port(key: &str, default: &str) -> std::io::Result<u16> {
    env_or(key, default)
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{key}: {e}")))
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let tcp_port = env_port("ZPL_TCP_PORT", "9100")?;
    let http_port = env_port("ZPL_HTTP_PORT", "8080")?; // bound in Task 21
    let archive_dir = env_or("ZPL_ARCHIVE_DIR", "./zpl-archive");

    let archive = Archive::open(archive_dir.clone().into())?;
    let state = Arc::new(AppState::new(archive));
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", tcp_port)).await?;
    let tcp = spawn_tcp(listener, state);

    eprintln!(
        "zpl-server: tcp on {tcp_port} (http on {http_port} from Task 21), archive at {archive_dir}; Ctrl-C to stop"
    );
    tokio::signal::ctrl_c().await?;
    eprintln!("zpl-server: shutting down");
    tcp.abort();
    Ok(())
}
