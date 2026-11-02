use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::ProtocolError;
use crate::auth::validate_callback;

#[derive(Debug)]
pub enum LoopbackError {
    Io(&'static str, io::Error),
    Protocol(ProtocolError),
    InvalidRequest,
    Timeout,
    Cancelled,
}

impl fmt::Display for LoopbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(operation, error) => write!(f, "OAuth loopback {operation} failed: {error}"),
            Self::Protocol(error) => error.fmt(f),
            Self::InvalidRequest => f.write_str("Invalid OAuth loopback HTTP request."),
            Self::Timeout => f.write_str("OAuth loopback callback timed out."),
            Self::Cancelled => f.write_str("OAuth loopback callback was cancelled."),
        }
    }
}

impl std::error::Error for LoopbackError {}

pub struct LoopbackListener {
    listener: TcpListener,
    redirect_uri: String,
}

impl LoopbackListener {
    pub fn bind() -> Result<Self, LoopbackError> {
        // Keep the OS-assigned port bound; never release it between discovery and listen.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|error| LoopbackError::Io("bind", error))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| LoopbackError::Io("configure", error))?;
        let port = listener
            .local_addr()
            .map_err(|error| LoopbackError::Io("address", error))?
            .port();
        Ok(Self {
            listener,
            redirect_uri: format!("http://127.0.0.1:{port}/"),
        })
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    pub fn wait_for_code(
        &self,
        state: &str,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> Result<String, LoopbackError> {
        if timeout.is_zero() || timeout > Duration::from_secs(300) || state.is_empty() {
            return Err(LoopbackError::InvalidRequest);
        }
        let deadline = Instant::now() + timeout;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(LoopbackError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(LoopbackError::Timeout);
            }
            let (mut stream, _) = match self.listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => return Err(LoopbackError::Io("accept", error)),
            };
            stream
                .set_nonblocking(false)
                .map_err(|error| LoopbackError::Io("configure stream", error))?;
            stream
                .set_read_timeout(Some(Duration::from_millis(100)))
                .map_err(|error| LoopbackError::Io("read timeout", error))?;
            stream
                .set_write_timeout(Some(Duration::from_millis(100)))
                .map_err(|error| LoopbackError::Io("write timeout", error))?;
            let target = read_target(&mut stream, deadline, cancelled)?;
            let parsed = url::Url::parse(&format!(
                "{}{}",
                self.redirect_uri.trim_end_matches('/'),
                target
            ))
            .map_err(|_| LoopbackError::InvalidRequest)?;
            if parsed.path() != "/" {
                reply(&mut stream, "404 Not Found", "Not an OAuth callback.")?;
                continue;
            }
            let result = validate_callback(parsed.query().unwrap_or_default(), state);
            if result.is_ok() {
                reply(
                    &mut stream,
                    "200 OK",
                    "Authorization callback verified. Return to VoicePrompt.",
                )?;
            } else {
                reply(
                    &mut stream,
                    "400 Bad Request",
                    "Authorization callback rejected.",
                )?;
            }
            return result.map_err(LoopbackError::Protocol);
        }
    }
}

fn read_target(
    stream: &mut TcpStream,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<String, LoopbackError> {
    let mut request = Vec::new();
    let mut buffer = [0; 512];
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(LoopbackError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(LoopbackError::Timeout);
        }
        let count = match stream.read(&mut buffer) {
            Ok(0) => return Err(LoopbackError::InvalidRequest),
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => return Err(LoopbackError::Io("read", error)),
        };
        request.extend_from_slice(&buffer[..count]);
        if request.len() > 8192 {
            return Err(LoopbackError::InvalidRequest);
        }
        if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            break;
        }
    }
    let text = std::str::from_utf8(&request).map_err(|_| LoopbackError::InvalidRequest)?;
    let mut fields = text
        .lines()
        .next()
        .ok_or(LoopbackError::InvalidRequest)?
        .split_whitespace();
    let method = fields.next();
    let target = fields.next().ok_or(LoopbackError::InvalidRequest)?;
    let version = fields.next();
    if method != Some("GET")
        || !target.starts_with('/')
        || target.starts_with("//")
        || !matches!(version, Some("HTTP/1.0" | "HTTP/1.1"))
        || fields.next().is_some()
    {
        return Err(LoopbackError::InvalidRequest);
    }
    Ok(target.to_owned())
}

fn reply(stream: &mut TcpStream, status: &str, body: &'static str) -> Result<(), LoopbackError> {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>VoicePrompt authorization callback</title></head><body><p>{body}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\nX-Content-Type-Options: nosniff\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|error| LoopbackError::Io("reply", error))
}
