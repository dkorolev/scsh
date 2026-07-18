//! WebSocket upgrade, frames, and broadcast hub for the session browser.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::sha1::sha1_digest;

const WS_MAGIC: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub struct Hub {
  clients: Mutex<Vec<Arc<Mailbox>>>,
}

/// One client's outbound slots — latest-wins, never a queue. Every message the hub sends
/// is a full-state snapshot (a tick supersedes the previous tick; a cast-growth notice
/// carries the recording's current total, superseding earlier notices for the same proc),
/// so a reader that cannot keep up — a browser behind an SSH tunnel — coalesces to the
/// freshest state instead of replaying history: the old unbounded per-client channel
/// buffered every 500ms tick during a stall and the page fast-forwarded through the
/// backlog (uptime visibly racing) while the pipe carried nothing but stale snapshots.
/// The serve loop's blocking socket write is the only in-flight message, so a slow client
/// is never more than one message plus one socket buffer behind — no ack protocol needed.
pub struct Mailbox {
  state: Mutex<MailboxState>,
  ready: Condvar,
}

#[derive(Default)]
struct MailboxState {
  /// Latest full-state tick; replaced on every broadcast, never appended.
  tick: Option<String>,
  /// Latest cast-growth notice per `(session, proc)`; replaced per key.
  growth: BTreeMap<(String, usize), String>,
  /// Set by the serve loop on disconnect so the next broadcast prunes this client.
  closed: bool,
}

impl Mailbox {
  /// The next message to write: the tick first (the whole-page state, freshest wins),
  /// then growth notices in key order.
  fn take_next(state: &mut MailboxState) -> Option<String> {
    if let Some(tick) = state.tick.take() {
      return Some(tick);
    }
    let key = state.growth.keys().next().cloned()?;
    state.growth.remove(&key)
  }

  fn close(&self) {
    self.state.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
  }
}

impl Hub {
  pub fn new() -> Arc<Hub> {
    Arc::new(Hub { clients: Mutex::new(Vec::new()) })
  }

  pub fn subscribe(self: &Arc<Self>) -> Arc<Mailbox> {
    let mailbox = Arc::new(Mailbox { state: Mutex::new(MailboxState::default()), ready: Condvar::new() });
    self.clients.lock().unwrap().push(Arc::clone(&mailbox));
    mailbox
  }

  /// Replace every client's pending tick with this one.
  pub fn broadcast_tick(&self, msg: &str) {
    self.deliver(|state| state.tick = Some(msg.to_string()));
  }

  /// Replace every client's pending growth notice for this proc.
  pub fn broadcast_growth(&self, session: &str, proc_index: usize, msg: &str) {
    self.deliver(|state| {
      state.growth.insert((session.to_string(), proc_index), msg.to_string());
    });
  }

  fn deliver(&self, mut put: impl FnMut(&mut MailboxState)) {
    let mut clients = self.clients.lock().unwrap();
    clients.retain(|mailbox| {
      let mut state = mailbox.state.lock().unwrap_or_else(|e| e.into_inner());
      if state.closed {
        return false;
      }
      put(&mut state);
      mailbox.ready.notify_one();
      true
    });
  }

  /// How many subscribers are (or recently were) connected — dead ones linger only until
  /// the next broadcast prunes them. Lets the tick loop skip work nobody would receive.
  pub fn client_count(&self) -> usize {
    self.clients.lock().unwrap().len()
  }
}

pub fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
  headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

pub fn wants_upgrade(method: &str, path: &str, headers: &[(String, String)]) -> bool {
  method == "GET"
    && path == "/ws"
    && header_value(headers, "Upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
    && header_value(headers, "Sec-WebSocket-Key").is_some()
}

pub fn accept_handshake(stream: &mut TcpStream, headers: &[(String, String)]) -> std::io::Result<()> {
  let key = header_value(headers, "Sec-WebSocket-Key").unwrap_or_default();
  let mut accept_input = String::with_capacity(key.len() + WS_MAGIC.len());
  accept_input.push_str(key);
  accept_input.push_str(WS_MAGIC);
  let digest = sha1_digest(accept_input.as_bytes());
  let accept = base64_encode(&digest);
  let resp = format!(
    "HTTP/1.1 101 Switching Protocols\r\n\
Upgrade: websocket\r\n\
Connection: Upgrade\r\n\
Sec-WebSocket-Accept: {accept}\r\n\r\n"
  );
  stream.write_all(resp.as_bytes())
}

pub fn serve(mut stream: TcpStream, mailbox: Arc<Mailbox>) {
  stream.set_read_timeout(Some(POLL_READ_TIMEOUT)).ok();
  loop {
    match read_client_frame(&mut stream) {
      Ok(Some(())) => {}
      Ok(None) => {}
      Err(e) if e.kind() == std::io::ErrorKind::ConnectionAborted => break,
      Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
      Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
      Err(_) => break,
    }
    // Take the freshest pending message, waiting briefly to keep the read-poll cadence.
    // The state lock is released before the socket write: a slow write must never block
    // broadcasts — that is exactly when the mailbox needs to keep coalescing.
    let msg = {
      let mut state = mailbox.state.lock().unwrap_or_else(|e| e.into_inner());
      match Mailbox::take_next(&mut state) {
        Some(msg) => Some(msg),
        None => {
          let (mut state, _) =
            mailbox.ready.wait_timeout(state, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner());
          Mailbox::take_next(&mut state)
        }
      }
    };
    if let Some(msg) = msg {
      if write_text_frame(&mut stream, &msg).is_err() {
        break;
      }
    }
  }
  mailbox.close();
}

fn write_text_frame(stream: &mut TcpStream, payload: &str) -> std::io::Result<()> {
  let bytes = payload.as_bytes();
  let mut header = Vec::with_capacity(10);
  header.push(0x81); // FIN + text
  let len = bytes.len();
  if len < 126 {
    header.push(len as u8);
  } else if len <= 65535 {
    header.push(126);
    header.extend_from_slice(&(len as u16).to_be_bytes());
  } else {
    header.push(127);
    header.extend_from_slice(&(len as u64).to_be_bytes());
  }
  stream.write_all(&header)?;
  stream.write_all(bytes)?;
  Ok(())
}

const MAX_WS_PAYLOAD: usize = 512 * 1024;
const POLL_READ_TIMEOUT: Duration = Duration::from_millis(50);
const FRAME_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// `Ok(Some(()))` when a client frame was handled; `Ok(None)` on poll idle; `Err` on close/error.
fn read_client_frame(stream: &mut TcpStream) -> std::io::Result<Option<()>> {
  stream.set_read_timeout(Some(POLL_READ_TIMEOUT)).ok();
  let mut head = [0u8; 2];
  match stream.read(&mut head) {
    Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "closed")),
    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
      return Ok(None);
    }
    Err(e) => return Err(e),
    Ok(1) => {
      stream.set_read_timeout(Some(FRAME_READ_TIMEOUT)).ok();
      let mut extra = [0u8; 1];
      stream.read_exact(&mut extra)?;
      head[1] = extra[0];
    }
    Ok(2) => {
      stream.set_read_timeout(Some(FRAME_READ_TIMEOUT)).ok();
    }
    Ok(_) => unreachable!(),
  }

  let result = read_client_frame_body(stream, head);
  stream.set_read_timeout(Some(POLL_READ_TIMEOUT)).ok();
  result.map(Some)
}

fn read_client_frame_body(stream: &mut TcpStream, head: [u8; 2]) -> std::io::Result<()> {
  let opcode = head[0] & 0x0F;
  if opcode == 0x8 {
    return Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "close"));
  }

  let masked = head[1] & 0x80 != 0;
  if !masked {
    return Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "unmasked client frame"));
  }

  let mut len = (head[1] & 0x7F) as usize;
  if len == 126 {
    let mut ext = [0u8; 2];
    stream.read_exact(&mut ext)?;
    len = u16::from_be_bytes(ext) as usize;
  } else if len == 127 {
    let mut ext = [0u8; 8];
    stream.read_exact(&mut ext)?;
    len = u64::from_be_bytes(ext) as usize;
  }

  const MAX_CONTROL_PAYLOAD: usize = 125;
  if matches!(opcode, 0x9 | 0xA) && len > MAX_CONTROL_PAYLOAD {
    let mut mask = [0u8; 4];
    stream.read_exact(&mut mask)?;
    discard_payload(stream, len)?;
    return Ok(());
  }
  if opcode == 0x9 && len == 0 {
    return Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "zero-length ping"));
  }

  let mut mask = [0u8; 4];
  stream.read_exact(&mut mask)?;

  if len > MAX_WS_PAYLOAD {
    discard_payload(stream, len)?;
    return Ok(());
  }

  if len == 0 {
    return Ok(());
  }

  let mut payload = vec![0u8; len];
  stream.read_exact(&mut payload)?;
  for (i, b) in payload.iter_mut().enumerate() {
    *b ^= mask[i % 4];
  }
  if opcode == 0x9 {
    // Ping → pong with same payload (non-zero only; zero-length rejected above).
    let mut frame = Vec::with_capacity(2 + payload.len());
    frame.push(0x8A);
    frame.push(payload.len() as u8);
    frame.extend_from_slice(&payload);
    stream.write_all(&frame)?;
  }
  Ok(())
}

fn discard_payload(stream: &mut TcpStream, len: usize) -> std::io::Result<()> {
  let mut discard = [0u8; 4096];
  let mut remaining = len;
  while remaining > 0 {
    let chunk = remaining.min(discard.len());
    stream.read_exact(&mut discard[..chunk])?;
    remaining -= chunk;
  }
  Ok(())
}

#[cfg(test)]
fn read_client_frame_blocking(stream: &mut TcpStream) -> std::io::Result<()> {
  stream.set_read_timeout(None).ok();
  match read_client_frame(stream)? {
    Some(()) => Ok(()),
    None => Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "idle")),
  }
}

fn base64_encode(data: &[u8]) -> String {
  const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
  for chunk in data.chunks(3) {
    let b0 = chunk[0] as u32;
    let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
    let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
    let triple = (b0 << 16) | (b1 << 8) | b2;
    out.push(TABLE[((triple >> 18) & 63) as usize] as char);
    out.push(TABLE[((triple >> 12) & 63) as usize] as char);
    if chunk.len() > 1 {
      out.push(TABLE[((triple >> 6) & 63) as usize] as char);
    } else {
      out.push('=');
    }
    if chunk.len() > 2 {
      out.push(TABLE[(triple & 63) as usize] as char);
    } else {
      out.push('=');
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::{Read, Write};
  use std::net::TcpListener;
  use std::panic::{catch_unwind, AssertUnwindSafe};
  use std::sync::mpsc;
  use std::thread;
  use std::time::Duration;

  const TEST_TIMEOUT: Duration = Duration::from_secs(30);
  const READ_TIMEOUT: Duration = Duration::from_secs(5);

  /// Fail the test if `f` does not finish within `TEST_TIMEOUT` (prevents hung I/O loops).
  fn with_timeout(f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
      let _ = tx.send(catch_unwind(AssertUnwindSafe(f)));
    });
    match rx.recv_timeout(TEST_TIMEOUT) {
      Ok(Ok(())) => {}
      Ok(Err(payload)) => std::panic::resume_unwind(payload),
      Err(mpsc::RecvTimeoutError::Timeout) => {
        panic!("test timed out after {}s", TEST_TIMEOUT.as_secs());
      }
      Err(mpsc::RecvTimeoutError::Disconnected) => panic!("test thread exited without result"),
    }
  }

  #[test]
  fn mailbox_keeps_only_the_latest_tick() {
    let hub = Hub::new();
    let mailbox = hub.subscribe();
    hub.broadcast_tick("tick-1");
    hub.broadcast_tick("tick-2");
    hub.broadcast_tick("tick-3");
    let mut state = mailbox.state.lock().unwrap();
    assert_eq!(Mailbox::take_next(&mut state).as_deref(), Some("tick-3"), "a slow client skips stale ticks");
    assert_eq!(Mailbox::take_next(&mut state), None, "history is never queued");
  }

  #[test]
  fn mailbox_coalesces_growth_per_proc_and_sends_the_tick_first() {
    let hub = Hub::new();
    let mailbox = hub.subscribe();
    hub.broadcast_growth("sess01", 0, "growth-a-old");
    hub.broadcast_growth("sess01", 0, "growth-a-new");
    hub.broadcast_growth("sess01", 3, "growth-b");
    hub.broadcast_tick("tick");
    let mut state = mailbox.state.lock().unwrap();
    assert_eq!(Mailbox::take_next(&mut state).as_deref(), Some("tick"), "the whole-page tick goes out first");
    assert_eq!(
      Mailbox::take_next(&mut state).as_deref(),
      Some("growth-a-new"),
      "the same proc coalesces to its latest notice"
    );
    assert_eq!(Mailbox::take_next(&mut state).as_deref(), Some("growth-b"), "distinct procs each keep one");
    assert_eq!(Mailbox::take_next(&mut state), None);
  }

  #[test]
  fn closed_mailbox_is_pruned_on_the_next_broadcast() {
    let hub = Hub::new();
    let mailbox = hub.subscribe();
    assert_eq!(hub.client_count(), 1);
    mailbox.close();
    hub.broadcast_tick("tick");
    assert_eq!(hub.client_count(), 0, "a disconnected client is dropped, not accumulated");
  }

  #[test]
  fn accept_key_matches_rfc6455_example() {
    let mut input = String::from("dGhlIHNhbXBsZSBub25jZQ==");
    input.push_str(WS_MAGIC);
    let digest = sha1_digest(input.as_bytes());
    assert_eq!(base64_encode(&digest), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
  }

  #[test]
  fn websocket_handshake_roundtrip() {
    with_timeout(|| {
      let listener = TcpListener::bind("127.0.0.1:0").unwrap();
      let addr = listener.local_addr().unwrap();
      let key = "dGhlIHNhbXBsZSBub25jZQ==";
      let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
        let req = read_http(&mut stream);
        assert!(wants_upgrade("GET", "/ws", &req.headers));
        accept_handshake(&mut stream, &req.headers).unwrap();
        write_text_frame(&mut stream, r#"{"type":"tick"}"#).unwrap();
      });
      let mut client = TcpStream::connect(addr).unwrap();
      client.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
      client
        .write_all(
          format!(
            "GET /ws HTTP/1.1\r\n\
Host: 127.0.0.1:{port}\r\n\
Upgrade: websocket\r\n\
Connection: Upgrade\r\n\
Sec-WebSocket-Key: {key}\r\n\r\n",
            port = addr.port(),
            key = key
          )
          .as_bytes(),
        )
        .unwrap();
      let mut buf = Vec::new();
      let mut chunk = [0u8; 512];
      loop {
        let n = client.read(&mut chunk).unwrap();
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
          break;
        }
      }
      let text = String::from_utf8_lossy(&buf);
      assert!(text.contains("101 Switching Protocols"));
      assert!(text.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
      let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
      loop {
        let body = &buf[header_end..];
        if body.len() >= 2 && body[0] == 0x81 {
          let len = body[1] as usize;
          if len < 126 && body.len() >= 2 + len {
            break;
          }
        }
        let n = client.read(&mut chunk).unwrap();
        if n == 0 {
          panic!("connection closed before complete websocket frame");
        }
        buf.extend_from_slice(&chunk[..n]);
      }
      let frame = &buf[header_end..];
      let len = frame[1] as usize;
      let payload = &frame[2..2 + len];
      assert!(String::from_utf8_lossy(payload).contains("tick"));
      handle.join().unwrap();
    });
  }

  struct MiniRequest {
    headers: Vec<(String, String)>,
  }

  fn write_masked_frame(client: &mut TcpStream, opcode: u8, payload: &[u8]) {
    let mut frame = Vec::new();
    frame.push(0x80 | opcode);
    let len = payload.len();
    if len < 126 {
      frame.push(0x80 | len as u8);
    } else if len < 65536 {
      frame.push(0x80 | 126);
      frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
      panic!("write_masked_frame: payload too large for test helper");
    }
    let mask = [0x12, 0x34, 0x56, 0x78];
    frame.extend_from_slice(&mask);
    for (i, b) in payload.iter().enumerate() {
      frame.push(b ^ mask[i % 4]);
    }
    client.write_all(&frame).unwrap();
  }

  fn write_masked_extended_frame(client: &mut TcpStream, opcode: u8, len: usize) {
    let mut frame = Vec::new();
    frame.push(0x80 | opcode);
    frame.push(0x80 | 127);
    frame.extend_from_slice(&(len as u64).to_be_bytes());
    let mask = [0xAA, 0xBB, 0xCC, 0xDD];
    frame.extend_from_slice(&mask);
    let mut remaining = len;
    let mut i = 0usize;
    while remaining > 0 {
      let chunk = remaining.min(64);
      for _ in 0..chunk {
        frame.push((i as u8) ^ mask[i % 4]);
        i += 1;
      }
      remaining -= chunk;
    }
    client.write_all(&frame).unwrap();
  }

  #[test]
  fn read_client_frame_completes_segmented_ping() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
      let (mut server, _) = listener.accept().unwrap();
      server.set_read_timeout(Some(POLL_READ_TIMEOUT)).ok();
      read_client_frame(&mut server).unwrap();
    });
    let mut client = TcpStream::connect(addr).unwrap();
    let mut frame = Vec::new();
    frame.push(0x89);
    frame.push(0x84);
    frame.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]);
    for (i, b) in b"ping".iter().enumerate() {
      frame.push(b ^ [0x12, 0x34, 0x56, 0x78][i % 4]);
    }
    client.write_all(&frame[..1]).unwrap();
    thread::sleep(Duration::from_millis(60));
    client.write_all(&frame[1..]).unwrap();
    let mut pong = [0u8; 6];
    client.read_exact(&mut pong).unwrap();
    assert_eq!(&pong, &[0x8A, 4, b'p', b'i', b'n', b'g']);
    handle.join().unwrap();
  }

  #[test]
  fn read_client_frame_pong_replies_to_ping() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
      let (mut server, _) = listener.accept().unwrap();
      read_client_frame_blocking(&mut server).unwrap();
    });
    let mut client = TcpStream::connect(addr).unwrap();
    write_masked_frame(&mut client, 0x9, b"ping");
    let mut pong = [0u8; 6];
    client.read_exact(&mut pong).unwrap();
    assert_eq!(&pong, &[0x8A, 4, b'p', b'i', b'n', b'g']);
    handle.join().unwrap();
  }

  #[test]
  fn read_client_frame_discards_oversize_payload() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let oversize = MAX_WS_PAYLOAD + 1;
    let handle = thread::spawn(move || {
      let (mut server, _) = listener.accept().unwrap();
      read_client_frame_blocking(&mut server).unwrap();
      read_client_frame_blocking(&mut server).unwrap();
    });
    let mut client = TcpStream::connect(addr).unwrap();
    write_masked_extended_frame(&mut client, 0x1, oversize);
    write_masked_frame(&mut client, 0x9, b"ok");
    let mut pong = [0u8; 4];
    client.read_exact(&mut pong).unwrap();
    assert_eq!(pong[0], 0x8A);
    assert_eq!(pong[1], 2);
    assert_eq!(&pong[2..], b"ok");
    handle.join().unwrap();
  }

  #[test]
  fn read_client_frame_discards_oversize_control_ping() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let oversize_ping = vec![b'a'; 126];
    let handle = thread::spawn(move || {
      let (mut server, _) = listener.accept().unwrap();
      read_client_frame_blocking(&mut server).unwrap();
      read_client_frame_blocking(&mut server).unwrap();
    });
    let mut client = TcpStream::connect(addr).unwrap();
    write_masked_frame(&mut client, 0x9, &oversize_ping);
    write_masked_frame(&mut client, 0x9, b"ok");
    let mut pong = [0u8; 4];
    client.read_exact(&mut pong).unwrap();
    assert_eq!(pong[0], 0x8A);
    assert_eq!(pong[1], 2);
    assert_eq!(&pong[2..], b"ok");
    handle.join().unwrap();
  }

  fn read_http(stream: &mut TcpStream) -> MiniRequest {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
      let n = stream.read(&mut chunk).unwrap();
      buf.extend_from_slice(&chunk[..n]);
      if buf.windows(4).any(|w| w == b"\r\n\r\n") {
        break;
      }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut headers = Vec::new();
    for line in text.split("\r\n").skip(1) {
      if line.is_empty() {
        break;
      }
      if let Some((k, v)) = line.split_once(':') {
        headers.push((k.trim().to_string(), v.trim().to_string()));
      }
    }
    MiniRequest { headers }
  }
}
