//! End-to-end host-query tests (Task 20): ~HS/~HQ/~WD/~HI/^HV over a real
//! TCP session. Query detection must go through the scanner (a `~HS` body
//! inside ^FD data is literal text, never a query), and every responder
//! must keep the socket open even when it chooses not to answer.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use zpl_server::archive::Archive;
use zpl_server::tcp::{spawn_tcp, AppState};

fn test_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("zpl-q-{tag}-{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn spawn(dir: PathBuf) -> (SocketAddr, Arc<AppState>) {
    let state = Arc::new(AppState::new(Archive::open(dir).unwrap()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    spawn_tcp(listener, state.clone());
    (addr, state)
}

/// Read until the response holds at least `n` STX-framed strings (or EOF).
async fn read_until_frames(sock: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    let deadline = Instant::now() + Duration::from_secs(3);
    while buf.iter().filter(|&&b| b == 0x02).count() < n {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {n} frame(s); got: {:?}",
            String::from_utf8_lossy(&buf)
        );
        match tokio::time::timeout(Duration::from_millis(100), sock.read(&mut chunk)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(k)) => buf.extend_from_slice(&chunk[..k]),
            _ => continue,
        }
    }
    buf
}

/// Read whatever arrives within `quiet` - asserting NOTHING is heard.
async fn assert_no_burst(sock: &mut TcpStream, quiet: Duration, what: &str) {
    let mut chunk = [0u8; 256];
    let got = match tokio::time::timeout(quiet, sock.read(&mut chunk)).await {
        Ok(Ok(k)) => k,
        _ => 0, // timed out or errored: nothing arrived
    };
    assert_eq!(got, 0, "{what} must produce no bytes on the socket");
}

fn frames(resp: &[u8]) -> usize {
    resp.iter().filter(|&&b| b == 0x02).count()
}

/// String 2's comma fields of the first frame in `resp`.
fn s2_fields(resp: &[u8]) -> Vec<String> {
    let l = resp.split(|&b| b == b'\n').nth(1).unwrap();
    l[1..l.len() - 2]
        .split(|&b| b == b',')
        .map(|f| String::from_utf8(f.to_vec()).unwrap())
        .collect()
}

fn s1_fields(resp: &[u8]) -> Vec<String> {
    let l = resp.split(|&b| b == b'\n').nth(0).unwrap();
    l[1..l.len() - 2]
        .split(|&b| b == b',')
        .map(|f| String::from_utf8(f.to_vec()).unwrap())
        .collect()
}

#[tokio::test]
async fn hv_returns_numbered_field_data_of_last_label() {
    let dir = test_dir("hv");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"^XA^FN1^FDHello^FS^XZ^HV1").await.unwrap();
    // Plain ^HV replies are unframed raw data: the field's text.
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut buf))
        .await
        .unwrap()
        .expect("^HV must produce a response");
    assert_eq!(&buf[..n], b"Hello", "default ^HV returns the field data");
    drop(sock);
}

#[tokio::test]
async fn hv_honors_byte_count_header_and_termination() {
    let dir = test_dir("hvfull");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    // ^HV1,3,<,>! -> n=3 bytes of "Hello", header "<", terminator ">!".
    sock.write_all(b"^XA^FN1^FDHello^FS^XZ^HV1,3,<,>!").await.unwrap();
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"<Hel>!");
    drop(sock);
}

#[tokio::test]
async fn hv_truncates_by_bytes_not_chars() {
    // Guide p.239: n = "the number of bytes to be returned". The field's
    // e (byte 0xE9 under the default codepage) is 2 UTF-8 bytes, so
    // ^HV1,1 must emit exactly ONE raw byte - a host sizing its receive
    // buffer to n must never over-read (multibyte split is fine).
    let dir = test_dir("hvbytes");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"^XA^FN1^FD\xE9^FS^XZ^HV1,1").await.unwrap();
    let mut buf = [0u8; 8];
    let n = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(n, 1, "exactly one byte for n=1");
    assert_eq!(buf[0], 0xC3, "the first UTF-8 byte of \u{00E9} (0xE9 \u{2192} U+00E9)");
    drop(sock);
}

#[tokio::test]
async fn hv_unknown_field_returns_empty_data() {
    let dir = test_dir("hvmiss");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"^XA^FN1^FDHello^FS^XZ^HV9").await.unwrap();
    // A never-defined field number returns no data (and no header/terminator
    // default), so the raw reply is zero bytes: assert silence.
    assert_no_burst(&mut sock, Duration::from_millis(400), "^HV for an unknown field").await;
    drop(sock);
}

#[tokio::test]
async fn labels_remaining_decrements_per_printed_copy() {
    let dir = test_dir("pq");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    // A ^PQ2 batch: 2 copies received at ^XZ; the completion consumes the
    // first printed copy, so 1 label remains when ~HS is answered.
    sock.write_all(b"^XA^GB50,50,2,B^FS^PQ2^XZ~HS").await.unwrap();
    let resp = read_until_frames(&mut sock, 1).await;
    let f = s2_fields(&resp);
    assert_eq!(f[8], "00000001", "uuuuuuuu = labels remaining in batch");
    drop(sock);
}

#[tokio::test]
async fn batch_remaining_carries_across_labels() {
    let dir = test_dir("pqcarry");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    // Two ^PQ2 labels: each completion adds 2 and consumes 1 -> 1 then 2.
    sock.write_all(b"^XA^GB50,50,2,B^FS^PQ2^XZ^XA^GB40,40,2,B^FS^PQ2^XZ~HS")
        .await
        .unwrap();
    let resp = read_until_frames(&mut sock, 1).await;
    let f = s2_fields(&resp);
    assert_eq!(f[8], "00000002", "remaining carries across the batch");
    drop(sock);
}

#[tokio::test]
async fn partial_format_flag_set_while_format_open() {
    let dir = test_dir("partial");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"^XA").await.unwrap(); // format opened, no fields yet
    tokio::time::sleep(Duration::from_millis(200)).await; // separate reads
    sock.write_all(b"~HS").await.unwrap();
    let resp = read_until_frames(&mut sock, 1).await;
    let f = s1_fields(&resp);
    assert_eq!(f[7], "1", "h = partial format flag");
    assert_eq!(f[6], "0", "g = comm diag off");
    sock.write_all(b"^XZ").await.unwrap(); // close cleanly
    drop(sock);
}

#[tokio::test]
async fn tilde_inside_fd_data_is_not_a_query() {
    let dir = test_dir("fdtilde");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    // The field body contains a literal "~HS": scanning inside field data
    // treats '~' as text, so the ONLY response must come from the trailing
    // real query. (The old raw byte-scan answered both - the regression
    // this test pins.)
    sock.write_all(b"^XA^FD~HS inside data^FS^XZ~HS").await.unwrap();
    let resp = read_until_frames(&mut sock, 1).await;
    // One ~HS response = three STX-framed lines; a spurious query from the
    // ^FD body would double the count to six.
    assert_eq!(frames(&resp), 3, "exactly one ~HS response, from the trailing query");
    assert!(resp.starts_with(b"\x02158,"), "the one response is a status string");
    drop(sock);
    wait_labels(&state, 1).await; // the label still archives normally
}

#[tokio::test]
async fn hq_od_reports_print_meters() {
    let dir = test_dir("hqod");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"^XA^GB50,50,2,B^XZ~HQOD").await.unwrap();
    let resp = read_until_frames(&mut sock, 1).await;
    let text = String::from_utf8_lossy(&resp);
    assert!(resp.starts_with(&[0x02]), "~HQ frames with STX");
    assert!(text.contains("PRINT METERS"));
    assert!(text.contains("TOTAL NONRESETTABLE: 1 \""));
    assert!(text.contains("USER RESETTABLE CNTR1: 0 \""));
    assert!(resp.ends_with(&[0x03]), "~HQ ends with ETX");
    drop(sock);
}

#[tokio::test]
async fn hq_sn_reports_serial_number() {
    let dir = test_dir("hqsn");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"~HQSN").await.unwrap();
    let resp = read_until_frames(&mut sock, 1).await;
    let text = String::from_utf8_lossy(&resp);
    assert!(text.contains("SERIAL NUMBER"));
    assert!(text.contains("ZPLDEMO0001"));
    drop(sock);
}

#[tokio::test]
async fn unknown_hq_query_is_ignored_socket_stays_open() {
    let dir = test_dir("hqzz");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"~HQZZ").await.unwrap();
    assert_no_burst(&mut sock, Duration::from_millis(400), "~HQZZ").await;
    // The socket is still alive: a follow-up query is answered.
    sock.write_all(b"~HS").await.unwrap();
    let resp = read_until_frames(&mut sock, 1).await;
    assert_eq!(frames(&resp), 3, "~HS answers with its three strings");
    drop(sock);
}

#[tokio::test]
async fn wd_dumps_identity_and_config() {
    let dir = test_dir("wd");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"~WD").await.unwrap();
    let resp = read_until_frames(&mut sock, 1).await;
    let text = String::from_utf8_lossy(&resp);
    assert!(resp.starts_with(&[0x02]));
    assert!(text.contains("ZPL II EMULATOR"));
    assert!(text.contains("V60.14.0"));
    drop(sock);
}

#[tokio::test]
async fn hi_reports_model_firmware_and_memory() {
    let dir = test_dir("hi");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"~HI").await.unwrap();
    let mut buf = [0u8; 128];
    let n = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"ZPL20,V60.14.0,8,4096KB,0\r\n");
    drop(sock);
}

async fn wait_labels(state: &AppState, n: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.counters.labels.load(Ordering::Relaxed) < n {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {n} archived label(s)"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
