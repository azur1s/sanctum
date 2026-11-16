//! A tiny HTTP server that hands out the pack. A plugin has no threads, so it
//! never blocks: `poll` runs every tick and moves each connection as far as
//! the sockets allow.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

/// Connections served at once. More than that wait in the listen queue.
const MAX_CONNECTIONS: usize = 64;
/// Polls without progress before a connection is dropped (30 seconds of ticks).
const IDLE_POLLS: u32 = 600;
const MAX_REQUEST: usize = 8192;

pub struct Server {
    listener: TcpListener,
    connections: Vec<Connection>,
}

struct Connection {
    stream: TcpStream,
    request: Vec<u8>,
    /// Set once the request has arrived.
    response: Option<Response>,
    idle: u32,
}

struct Response {
    head: Vec<u8>,
    body: Arc<Vec<u8>>,
    sent: usize,
}

impl Server {
    pub fn bind(port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, connections: Vec::new() })
    }

    /// `pack` is what a request arriving now gets; a download already under way
    /// keeps the bytes it started with.
    pub fn poll(&mut self, pack: Option<&Arc<Vec<u8>>>) {
        while self.connections.len() < MAX_CONNECTIONS {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if stream.set_nonblocking(true).is_ok() {
                        self.connections.push(Connection { stream, request: Vec::new(), response: None, idle: 0 });
                    }
                }
                Err(_) => break,
            }
        }
        self.connections.retain_mut(|connection| connection.advance(pack));
    }
}

impl Connection {
    /// False once the connection is finished or broken.
    fn advance(&mut self, pack: Option<&Arc<Vec<u8>>>) -> bool {
        self.idle += 1;
        if self.idle > IDLE_POLLS {
            return false;
        }
        if self.response.is_none() {
            let mut chunk = [0u8; 1024];
            loop {
                match self.stream.read(&mut chunk) {
                    Ok(0) => return false,
                    Ok(n) => {
                        self.request.extend_from_slice(&chunk[..n]);
                        self.idle = 0;
                        if self.request.windows(4).any(|w| w == b"\r\n\r\n") {
                            self.response = Some(respond(&self.request, pack));
                            break;
                        }
                        if self.request.len() > MAX_REQUEST {
                            return false;
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => return true,
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(_) => return false,
                }
            }
        }
        let Some(response) = &mut self.response else { return true };
        loop {
            let head = response.head.len();
            let rest = if response.sent < head { &response.head[response.sent..] } else { &response.body[response.sent - head..] };
            if rest.is_empty() {
                let _ = self.stream.flush();
                return false;
            }
            match self.stream.write(rest) {
                Ok(0) => return false,
                Ok(n) => {
                    response.sent += n;
                    self.idle = 0;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return true,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
    }
}

/// Every path gets the pack: the URL only carries the hash so clients refetch
/// when it changes.
fn respond(request: &[u8], pack: Option<&Arc<Vec<u8>>>) -> Response {
    let get = request.starts_with(b"GET ");
    let head_only = request.starts_with(b"HEAD ");
    let empty = || Arc::new(Vec::new());
    match pack {
        Some(pack) if get || head_only => Response {
            head: format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                pack.len()
            )
            .into_bytes(),
            body: if get { Arc::clone(pack) } else { empty() },
            sent: 0,
        },
        Some(_) => Response {
            head: b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            body: empty(),
            sent: 0,
        },
        None => Response {
            head: b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            body: empty(),
            sent: 0,
        },
    }
}
