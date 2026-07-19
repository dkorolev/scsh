# Daemon HTTP connection model (keep-alive)

The daemon and every scsh HTTP client speak plain HTTP/1.1 over localhost TCP with
**persistent (keep-alive) connections**. This document is the contract: what stays open,
what closes, why the one retry in the client is safe, and how to verify all of it.

## Why keep-alive

Before 1.36.0 the daemon answered every request with `Connection: close`, and the CLI's
poster thread opened a fresh TCP connection for every event POST. Every closed TCP
connection parks a socket in TIME_WAIT for ~30 seconds (2×MSL), and the OS draws client
ports from a finite ephemeral range (macOS ≈16k ports, Linux ≈28k by default). A running
job posts constantly; a browser job page fetches in bursts; the test suite multiplies both.
Measured on macOS: **9,600+ sockets** parked in TIME_WAIT after back-to-back test-suite
runs, at which point unrelated `connect()` calls anywhere on the machine failed with
`EADDRNOTAVAIL`. After the change, a full CLI-suite run leaves ~55.

## The server contract

- `handle_connection` serves **many requests per connection**, strictly ping-pong
  (request, response, request, …). Clients here never pipeline; bytes read past the framed
  request would be discarded, not replayed.
- Every response carries `Content-Length` and a `Connection: keep-alive` or
  `Connection: close` label. There is no chunked encoding.
- The connection closes when:
  - the client closes it (a zero-byte read — `read_request` reports it as EOF rather than
    parsing an empty `GET /`);
  - the request carries a `close` token in its `Connection` header (parsed as a
    comma-separated token list, case-insensitive);
  - the request line says `HTTP/1.0` — such clients predate keep-alive and may frame the
    response by EOF;
  - nothing arrives for `KEEP_ALIVE_IDLE` (5 seconds) — the same value that has always
    bounded a slow request's reads.
- **WebSocket is the exception**: an upgrade request answers `101` with
  `Connection: Upgrade` and the socket becomes the WS session for its whole life. The
  upgrade check runs before any close/keep-alive logic and never passes through
  `write_response`.
- Dirty-state flags (store persistence, WS refresh) are set **per request** inside the
  connection loop — a keep-alive connection outlives its mutations, which must be visible
  immediately, not at connection end.
- The `403` non-loopback denial always closes.

## The clients

- **Browsers** reuse connections automatically now that the server stops hanging up; page
  fetch bursts ride a handful of sockets instead of one per request.
- **The poster thread** (`daemon/client.rs`, `DaemonConn`) — the high-frequency path that
  posts proc events and line batches for a running job — holds one reusable connection and
  reads responses by their `Content-Length` frame (reading to EOF would block forever on a
  live connection).
- **One-shot senders stay one-shot** and send an explicit `Connection: close`: session
  register/finish (`send_post`), `post_once` (e.g. prune), the daemon-alive and version
  probes in `daemon/paths.rs`. They are rare (a handful per run) and their read-to-EOF
  framing depends on the close.

## The retry, and why it cannot double-post

A post on a reused connection can race the daemon's 5-second idle close. The client
retries **once, on a fresh connection**, in exactly the cases where the daemon provably
never processed the request:

- **The write failed.** An incomplete request cannot be routed, and dropping our socket
  guarantees it never completes.
- **The read died before any response byte** with a disconnect error. The same race looks
  different per OS: macOS commonly reads a clean zero-byte FIN (`UnexpectedEof`), while
  Linux and Windows/WSL answer a write on a server-closed socket with an RST, surfacing as
  `ConnectionReset` (or `ConnectionAborted`/`BrokenPipe`).

Never retried: fresh-connection failures (the daemon is actually unreachable), timeouts
(`WouldBlock`/`TimedOut` — a slow daemon may still process the request), and any failure
after the first response byte (`read_keep_alive_response` demotes those to `InvalidData`,
so the retryable error kinds are only reachable with zero response bytes — the
classification is sound by construction, not convention).

Known residual window, accepted deliberately: a daemon that dies between reading a full
request and writing the first response byte is indistinguishable from the idle close, so
that one post could be applied twice across daemon generations. The idle-close race happens
on every >5s gap between posts; a daemon death inside a sub-millisecond handler is
vanishingly rare — and without the retry, every idle-close race would silently **lose** a
post instead.

## Portability notes

- Everything is `std`-only: no platform socket options, no `libc`. The per-OS differences
  are confined to which `ErrorKind` the stale-connection race surfaces as (see above), and
  the retry set covers all of them. WSL2 behaves as Linux.
- Read timeouts surface as `WouldBlock` on Unix and `TimedOut` on Windows; both are treated
  identically (connection ends server-side; no retry client-side).

## Verifying by hand

With a daemon running (`scsh daemon start`), from the repository root:

```console
# Two SEQUENTIAL requests over one nc connection — the sleep matters: the connection is
# ping-pong, so the second request goes out after the first response, never pipelined.
{ printf 'GET /api/v1/version HTTP/1.1\r\nHost: x\r\n\r\n'; sleep 1; \
  printf 'GET /api/v1/version HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n'; } \
  | nc 127.0.0.1 7274 | grep -c 'HTTP/1.1 200'    # prints 2

# curl reuses one connection for both URLs (verbose shows "Re-using existing connection"):
curl -sv http://127.0.0.1:7274/api/v1/version http://127.0.0.1:7274/api/v1/version 2>&1 \
  | grep -i 're-using'

# An HTTP/1.0 request closes by default:
printf 'GET /api/v1/version HTTP/1.0\r\nHost: x\r\n\r\n' | nc 127.0.0.1 7274 \
  | grep -i 'connection: close'

# Socket footprint after a full test run (expect double digits, not thousands):
netstat -an | grep -c TIME_WAIT
```

Nothing above starts anything that outlives the command; the daemon was already running and
stays as it was.

The automated coverage lives in `src/daemon/mod.rs`
(`daemon_keeps_a_connection_alive_across_requests_and_honors_close` — framed sequential
requests, explicit close, HTTP/1.0 default close), `src/daemon/server.rs`
(`connection_close_reads_the_header_as_a_token_list`), and `src/daemon/client.rs`
(connection reuse, the one safe stale retry, per-OS disconnect classification, mid-response
loss never retrying, close-labeled responses dropping the cached connection).
