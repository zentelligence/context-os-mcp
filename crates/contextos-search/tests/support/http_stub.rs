//! A minimal, single-request HTTP/1.1 stub bound to an ephemeral loopback
//! port, used to test `OpenAiCompatible` without any real network access,
//! external process, or crate beyond the standard library. Hand-rolled
//! rather than pulling in an async HTTP server crate: `contextos-search`
//! and its tests stay free of a Tokio dependency, matching the rest of
//! this crate's synchronous design (see `src/embedding.rs`'s module
//! documentation).
//!
//! Shared across every integration test binary in this crate (see
//! `tests/support/mod.rs`); not every binary uses this helper, so an unused
//! item in one binary is expected rather than genuine dead code.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread::JoinHandle;
use std::time::Duration;

/// One HTTP request captured by [`HttpStub`].
#[derive(Clone, Debug, Default)]
pub struct CapturedRequest {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub body: Vec<u8>,
}

/// Scripted response for the one connection [`HttpStub`] accepts.
#[derive(Clone, Debug)]
pub enum StubResponse {
    /// Responds immediately with `status` and `body`.
    Immediate { status: u16, body: Vec<u8> },
    /// Sleeps for `delay` before responding, so a caller with a shorter
    /// request timeout observes a timeout rather than a slow success.
    Delayed {
        delay: Duration,
        status: u16,
        body: Vec<u8>,
    },
}

/// Accepts exactly one connection on an ephemeral loopback port, captures
/// the request, and writes the scripted [`StubResponse`].
pub struct HttpStub {
    addr: SocketAddr,
    handle: JoinHandle<std::io::Result<CapturedRequest>>,
}

impl HttpStub {
    /// Starts the stub on a background thread. Returns as soon as the
    /// listener is bound; no sleep is needed before connecting to it.
    pub fn start(response: StubResponse) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let handle = std::thread::spawn(move || serve_one(&listener, &response));
        Ok(Self { addr, handle })
    }

    /// Returns the full URL for `path` against this stub's loopback port.
    #[must_use]
    pub fn endpoint(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Waits for the stub's one request to complete and returns what it
    /// captured.
    pub fn join(self) -> std::io::Result<CapturedRequest> {
        match self.handle.join() {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::other("stub server thread panicked")),
        }
    }
}

fn serve_one(listener: &TcpListener, response: &StubResponse) -> std::io::Result<CapturedRequest> {
    let (mut stream, _) = listener.accept()?;
    let request = read_request(&mut stream)?;

    if let StubResponse::Delayed { delay, .. } = response {
        std::thread::sleep(*delay);
    }
    let (status, body) = match response {
        StubResponse::Immediate { status, body } | StubResponse::Delayed { status, body, .. } => {
            (*status, body.as_slice())
        }
    };
    write_response(&mut stream, status, body)?;
    Ok(request)
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<CapturedRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = find_header_terminator(&buffer) {
            break Some(end);
        }
        if buffer.len() > 64 * 1024 {
            break None;
        }
    };
    let Some(header_end) = header_end else {
        return Ok(CapturedRequest::default());
    };

    let header_text = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();

    let mut content_length = 0_usize;
    let mut authorization = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.to_owned());
            }
        }
    }

    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length.min(body.len()));

    Ok(CapturedRequest {
        method,
        path,
        authorization,
        body,
    })
}

fn find_header_terminator(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

fn write_response(stream: &mut TcpStream, status: u16, body: &[u8]) -> std::io::Result<()> {
    let reason = reason_phrase(status);
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    }
}
