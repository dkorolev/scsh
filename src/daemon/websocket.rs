//! WebSocket upgrade, frames, and broadcast hub for the session browser.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::sha1::sha1_digest;

const WS_MAGIC: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub struct Hub {
  clients: Mutex<Vec<Sender<String>>>,
}

impl Hub {
  pub fn new() -> Arc<Hub> {
    Arc::new(Hub { clients: Mutex::new(Vec::new()) })
  }

  pub fn subscribe(self: &Arc<Self>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    self.clients.lock().unwrap().push(tx);
    rx
  }

  pub fn broadcast(&self, msg: String) {
    let mut clients = self.clients.lock().unwrap();
    clients.retain(|tx| tx.send(msg.clone()).is_ok());
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

pub fn serve(mut stream: TcpStream, rx: mpsc::Receiver<String>) {
  stream.set_read_timeout(Some(Duration::from_millis(50))).ok();
  loop {
    if let Err(e) = read_client_frame(&mut stream) {
      if e.kind() != std::io::ErrorKind::WouldBlock && e.kind() != std::io::ErrorKind::TimedOut {
        break;
      }
    }
    match rx.recv_timeout(Duration::from_millis(100)) {
      Ok(msg) => {
        if write_text_frame(&mut stream, &msg).is_err() {
          break;
        }
      }
      Err(RecvTimeoutError::Timeout) => {}
      Err(RecvTimeoutError::Disconnected) => break,
    }
  }
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

fn read_client_frame(stream: &mut TcpStream) -> std::io::Result<()> {
  let mut head = [0u8; 2];
  match stream.read(&mut head) {
    Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "closed")),
    Ok(1) => {
      let mut extra = [0u8; 1];
      stream.read_exact(&mut extra)?;
      head[1] = extra[0];
    }
    Ok(2) => {}
    Ok(_) => unreachable!(),
    Err(e) => return Err(e),
  }

  let opcode = head[0] & 0x0F;
  if opcode == 0x8 {
    return Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "close"));
  }

  let masked = head[1] & 0x80 != 0;
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

  let mut mask = [0u8; 4];
  if masked {
    stream.read_exact(&mut mask)?;
  }
  if len > MAX_WS_PAYLOAD {
    let mut discard = [0u8; 4096];
    let mut remaining = len;
    while remaining > 0 {
      let chunk = remaining.min(discard.len());
      stream.read_exact(&mut discard[..chunk])?;
      remaining -= chunk;
    }
    return Ok(());
  }
  const MAX_CONTROL_PAYLOAD: usize = 125;
  if matches!(opcode, 0x8 | 0x9 | 0xA) && len > MAX_CONTROL_PAYLOAD {
    let mut discard = vec![0u8; len];
    stream.read_exact(&mut discard)?;
    return Ok(());
  }
  if len > 0 {
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    if masked {
      for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask[i % 4];
      }
    }
    if opcode == 0x9 {
      // Ping → pong with same payload.
      let mut frame = Vec::with_capacity(2 + payload.len());
      frame.push(0x8A);
      frame.push(payload.len() as u8);
      frame.extend_from_slice(&payload);
      stream.write_all(&frame)?;
    }
  }
  Ok(())
}

fn base64_encode(data: &[u8]) -> String {
  const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
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
      while buf.len() <= header_end {
        let n = client.read(&mut chunk).unwrap();
        if n == 0 {
          break;
        }
        buf.extend_from_slice(&chunk[..n]);
      }
      let frame = &buf[header_end..];
      assert!(!frame.is_empty(), "expected websocket frame after handshake");
      assert_eq!(frame[0], 0x81);
      assert!(String::from_utf8_lossy(frame).contains("tick"));
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
    assert!(len < 126);
    frame.push(0x80 | len as u8);
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
  fn read_client_frame_pong_replies_to_ping() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
      let (mut server, _) = listener.accept().unwrap();
      read_client_frame(&mut server).unwrap();
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
      read_client_frame(&mut server).unwrap();
      read_client_frame(&mut server).unwrap();
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
