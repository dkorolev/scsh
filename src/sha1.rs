//! Minimal SHA-1 (FIPS 180-1) for WebSocket handshake accept keys — std-only.

/// SHA-1 digest of `data`.
pub fn sha1_digest(data: &[u8]) -> [u8; 20] {
  let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
  let bit_len = (data.len() as u64).wrapping_mul(8);
  let mut msg = data.to_vec();
  msg.push(0x80);
  while msg.len() % 64 != 56 {
    msg.push(0);
  }
  msg.extend_from_slice(&bit_len.to_be_bytes());

  for block in msg.chunks_exact(64) {
    let mut w = [0u32; 80];
    for (i, word) in w.iter_mut().enumerate().take(16) {
      let j = i * 4;
      *word = u32::from_be_bytes([block[j], block[j + 1], block[j + 2], block[j + 3]]);
    }
    for i in 16..80 {
      w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }

    let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
    for (i, &wi) in w.iter().enumerate() {
      let (f, k) = if i < 20 {
        ((b & c) | ((!b) & d), 0x5A827999)
      } else if i < 40 {
        (b ^ c ^ d, 0x6ED9EBA1)
      } else if i < 60 {
        ((b & c) | (b & d) | (c & d), 0x8F1BBCDC)
      } else {
        (b ^ c ^ d, 0xCA62C1D6)
      };
      let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
      e = d;
      d = c;
      c = b.rotate_left(30);
      b = a;
      a = temp;
    }
    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
  }

  let mut out = [0u8; 20];
  for (i, word) in h.iter().enumerate() {
    out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sha1_empty_known_vector() {
    let d = sha1_digest(b"");
    assert_eq!(hex(&d), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
  }

  #[test]
  fn sha1_abc_known_vector() {
    let d = sha1_digest(b"abc");
    assert_eq!(hex(&d), "a9993e364706816aba3e25717850c26c9cd0d89d");
  }

  fn hex(d: &[u8; 20]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
  }
}
