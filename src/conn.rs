//! Talking to dinod: the socket `dino` uses (DINO_HOME aware), one request at a time per
//! connection. dino-tui never starts dinod; it says how to.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use dino_core::ipc::{self, Request, Response};

pub fn socket() -> PathBuf {
    ipc::socket_path()
}

/// What to say when dinod isn't there.
pub fn not_running() -> String {
    format!("dinod isn't running (nothing at {}).\nStart it with `dino daemon &`, or open the Dino app, which starts it. dino-tui doesn't start it for you.", socket().display())
}

pub struct Conn(pub UnixStream);

impl Conn {
    pub fn open() -> io::Result<Conn> {
        UnixStream::connect(socket()).map(Conn)
    }

    pub fn request(&mut self, req: &Request) -> io::Result<Response> {
        ipc::write_json(&mut self.0, req)?;
        let (_, payload) = ipc::read_frame(&mut self.0)?;
        serde_json::from_slice(&payload).map_err(io::Error::other)
    }

    /// A request and its answer as plain JSON: for what the pinned dino-core can't say or would
    /// lose (a newer dinod's settings fields, a request newer than the pin).
    pub fn raw(&mut self, req: &serde_json::Value) -> io::Result<serde_json::Value> {
        ipc::write_json(&mut self.0, req)?;
        let (_, payload) = ipc::read_frame(&mut self.0)?;
        serde_json::from_slice(&payload).map_err(io::Error::other)
    }
}

/// A one-off request on a connection of its own.
pub fn request(req: &Request) -> io::Result<Response> {
    Conn::open()?.request(req)
}

pub fn raw(req: &serde_json::Value) -> io::Result<serde_json::Value> {
    Conn::open()?.raw(req)
}

/// dinod's answer to a request it doesn't know: an older dinod than the feature needs.
pub fn unknown_request(message: &str) -> bool {
    message.starts_with("bad request: unknown variant")
}

/// A raw answer's error, if it is one.
pub fn raw_error(v: &serde_json::Value) -> Option<String> {
    (v["type"] == "error").then(|| v["message"].as_str().unwrap_or("dinod said no").to_string())
}
