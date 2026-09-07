//! Boot tests: drive the real `zpl-server` binary the way an operator
//! would - env-configured ports and archive dir, banner on stderr, working
//! TCP + HTTP surfaces, SIGINT shutdown. This is the only way to reach
//! `main.rs` (env parsing, `start_server`, the shutdown path); everything
//! TCP/HTTP-side is asserted again here end-to-end through the actual
//! process.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn test_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("zpl-boot-{tag}-{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Pick a free TCP port by binding an ephemeral listener and dropping it.
/// The server binds it right after; a collision is a flake, not a failure.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

struct Boot {
    child: Child,
    rx: Receiver<String>,
}

/// Spawn the server with env-configurable ports and a temp archive; the
/// stderr reader thread feeds `rx`, which the banner waiters drain.
fn spawn_server(tcp_port: u16, http_port: u16, dir: &PathBuf, token: Option<&str>) -> Boot {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_zpl-server"));
    cmd.env("ZPL_TCP_PORT", tcp_port.to_string())
        .env("ZPL_HTTP_PORT", http_port.to_string())
        .env("ZPL_ARCHIVE_DIR", dir)
        .env("ZPL_ARCHIVE_KEEP", "100")
        .env("ZPL_ARCHIVE_MAX_MB", "16")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(t) = token {
        cmd.env("ZPL_AUTH_TOKEN", t);
    }
    let mut child = cmd.spawn().expect("spawn zpl-server");
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stderr).lines();
        while let Some(Ok(line)) = lines.next() {
            let _ = tx.send(line);
        }
    });
    Boot { child, rx }
}

/// Wait (up to 15 s) for the startup banner on stderr.
fn wait_banner(boot: &Boot) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match boot.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) if line.contains("tcp on") && line.contains("http on") => return line,
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => assert!(
                Instant::now() < deadline,
                "server must print its startup banner on stderr"
            ),
            Err(RecvTimeoutError::Disconnected) => panic!("server died before the banner"),
        }
    }
}

/// The banner reports the effective settings (config reads exercised).
#[test]
fn banner_reports_effective_settings() {
    let dir = test_dir("banner");
    let _keep = Cleanup(dir.clone());
    let tcp = free_port();
    let http = free_port();
    let boot = spawn_server(tcp, http, &dir, None);
    let line = wait_banner(&boot);
    assert!(line.contains(&format!("tcp on {tcp}")), "{line}");
    assert!(line.contains(&format!("http on {http}")), "{line}");
    assert!(
        line.contains("api auth=off"),
        "no token -> auth off: {line}"
    );
    assert!(
        line.contains("keep=100"),
        "ZPL_ARCHIVE_KEEP applied: {line}"
    );
    assert!(
        line.contains("max=16 MiB"),
        "ZPL_ARCHIVE_MAX_MB applied: {line}"
    );
    stop(&boot);
}

fn stop(boot: &Boot) {
    // Best-effort cleanup so a failed assertion can't leak a child process.
    let _ = Command::new("kill")
        .args(["-KILL", &boot.child.id().to_string()])
        .status();
}

/// Full cycle through the real binary: archive a label over TCP, read it
/// back over HTTP, then SIGINT and confirm a clean exit (the shutdown path
/// in main.rs: ctrl-c handler, task aborts).
#[test]
fn end_to_end_label_http_and_shutdown() {
    let dir = test_dir("e2e");
    let _keep = Cleanup(dir.clone());
    let tcp = free_port();
    let http = free_port();
    let mut boot = spawn_server(tcp, http, &dir, None);
    let _ = wait_banner(&boot);

    // Send a label over the TCP port.
    let mut sock = TcpStream::connect(("127.0.0.1", tcp)).unwrap();
    sock.write_all(b"^XA^GB50,50,2,B^XZ").unwrap();
    drop(sock);

    // Poll the HTTP API until the label is archived and listed.
    let url = format!("http://127.0.0.1:{http}/api/labels");
    let deadline = Instant::now() + Duration::from_secs(10);
    let body = loop {
        if let Ok(b) = raw_get(&url) {
            if b.contains("\"id\":1") {
                break b;
            }
        }
        assert!(Instant::now() < deadline, "label never archived");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        body.contains("\"width\":832"),
        "row carries geometry: {body}"
    );

    // SIGINT: clean shutdown, process exits on its own.
    sigint(&boot);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match boot.child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => panic!("server did not exit after SIGINT"),
            Err(e) => panic!("wait: {e}"),
        }
    }
}

#[cfg(unix)]
fn sigint(boot: &Boot) {
    let status = Command::new("kill")
        .args(["-INT", &boot.child.id().to_string()])
        .status()
        .expect("kill -INT");
    assert!(status.success(), "kill -INT failed");
}

/// Minimal raw HTTP GET (no HTTP client in the dev-deps).
fn raw_get(url: &str) -> std::io::Result<String> {
    let hostport = url
        .strip_prefix("http://")
        .ok_or_else(|| std::io::Error::other("bad url"))?;
    let (host, port_path) = hostport.split_once(':').unwrap_or((hostport, "80"));
    let (port, path) = port_path
        .split_once('/')
        .map(|(p, pa)| (p, format!("/{pa}")))
        .unwrap_or((port_path, "/".into()));
    let mut stream = TcpStream::connect(format!("{host}:{port}"))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )?;
    let mut buf = String::new();
    BufReader::new(stream.take(1 << 20)).read_to_string(&mut buf)?;
    Ok(buf
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or(buf))
}

/// With ZPL_ARCHIVE_KEEP / ZPL_ARCHIVE_MAX_MB unset, boot must fall back to
/// the defaults (the env_parse Err arm) instead of failing.
#[test]
fn unset_retention_vars_fall_back_to_defaults() {
    let dir = test_dir("defaults");
    let _keep = Cleanup(dir.clone());
    let tcp = free_port();
    let http = free_port();
    // Spawn WITHOUT the two retention vars (spawn_server sets them).
    let mut child = Command::new(env!("CARGO_BIN_EXE_zpl-server"))
        .env("ZPL_TCP_PORT", tcp.to_string())
        .env("ZPL_HTTP_PORT", http.to_string())
        .env("ZPL_ARCHIVE_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stderr).lines();
        while let Some(Ok(line)) = lines.next() {
            let _ = tx.send(line);
        }
    });
    let line = {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(l) if l.contains("tcp on") && l.contains("http on") => break l,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => assert!(
                    Instant::now() < deadline,
                    "server must boot with default retention"
                ),
                Err(RecvTimeoutError::Disconnected) => panic!("server died"),
            }
        }
    };
    assert!(
        line.contains("keep=10000"),
        "default keep in banner: {line}"
    );
    assert!(
        line.contains("max=1024 MiB"),
        "default max in banner: {line}"
    );
    let _ = Command::new("kill")
        .args(["-KILL", &child.id().to_string()])
        .status();
    let _ = child.wait();
}

/// Invalid env values abort boot with the key named in the error message
/// (the env_parse/env_port failure branches of main.rs).
#[test]
fn invalid_env_reports_the_key_and_exits_nonzero() {
    let dir = test_dir("badenv");
    let _keep = Cleanup(dir.clone());

    let out = Command::new(env!("CARGO_BIN_EXE_zpl-server"))
        .env("ZPL_TCP_PORT", "not-a-port")
        .env("ZPL_HTTP_PORT", free_port().to_string())
        .env("ZPL_ARCHIVE_DIR", &dir)
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "bad ZPL_TCP_PORT must fail boot");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ZPL_TCP_PORT"),
        "error names the key: {stderr}"
    );

    let out = Command::new(env!("CARGO_BIN_EXE_zpl-server"))
        .env("ZPL_TCP_PORT", free_port().to_string())
        .env("ZPL_HTTP_PORT", free_port().to_string())
        .env("ZPL_ARCHIVE_DIR", &dir)
        .env("ZPL_ARCHIVE_KEEP", "lots")
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "bad ZPL_ARCHIVE_KEEP must fail boot");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ZPL_ARCHIVE_KEEP"),
        "error names the key: {stderr}"
    );
}

/// ZPL_AUTH_TOKEN turns auth on: the API 401s without a token while the
/// static UI stays public (the browser can reach the login form).
#[test]
fn auth_token_enables_api_gate() {
    let dir = test_dir("bootauth");
    let _keep = Cleanup(dir.clone());
    let tcp = free_port();
    let http = free_port();
    let boot = spawn_server(tcp, http, &dir, Some("sekret"));
    let line = wait_banner(&boot);
    assert!(line.contains("api auth=on"), "{line}");

    let body = raw_get(&format!("http://127.0.0.1:{http}/api/labels")).unwrap();
    assert!(body.contains("\"needs_token\":true"), "401 body: {body}");
    let ui = raw_get(&format!("http://127.0.0.1:{http}/")).unwrap();
    assert!(ui.contains("<!DOCTYPE html>"), "UI stays public");
    stop(&boot);
}
