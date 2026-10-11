//! The event loop's one wait: poll(2) on the terminal, dinod's stream when attached, and a wake
//! socket that dinod's answers (from the long-poll threads), size changes (SIGWINCH) and finished
//! requests write a byte to. Nothing ticks while nothing happens.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// Sends events to the loop and wakes it.
pub struct Waker<E> {
    tx: Sender<E>,
    bell: Arc<UnixStream>,
}

impl<E> Clone for Waker<E> {
    fn clone(&self) -> Self {
        Waker { tx: self.tx.clone(), bell: self.bell.clone() }
    }
}

impl<E> Waker<E> {
    pub fn send(&self, ev: E) {
        if self.tx.send(ev).is_ok() {
            self.wake();
        }
    }

    /// The writing end, for a thread that only needs to wake the loop.
    pub fn bell(&self) -> Arc<UnixStream> {
        self.bell.clone()
    }

    pub fn wake(&self) {
        // Non-blocking: a full socket already says "look".
        let _ = (&*self.bell).write(&[1]);
    }
}

/// The loop's side: the events, and the socket to poll.
pub struct Wake<E> {
    pub rx: Receiver<E>,
    reader: UnixStream,
}

impl<E> Wake<E> {
    pub fn fd(&self) -> RawFd {
        self.reader.as_raw_fd()
    }

    /// Empty the socket (the events stay in `rx`).
    pub fn drain(&mut self) {
        let mut buf = [0u8; 256];
        while matches!(self.reader.read(&mut buf), Ok(n) if n > 0) {}
    }
}

pub fn new<E>() -> io::Result<(Waker<E>, Wake<E>)> {
    let (reader, writer) = UnixStream::pair()?;
    reader.set_nonblocking(true)?;
    writer.set_nonblocking(true)?;
    // A size change wakes the loop too; signal-hook's registry is the one crossterm uses, so both
    // see it.
    signal_hook::low_level::pipe::register(signal_hook::consts::SIGWINCH, writer.try_clone()?)?;
    let (tx, rx) = channel();
    Ok((Waker { tx, bell: Arc::new(writer) }, Wake { rx, reader }))
}

/// poll(2) on `fds` (events POLLIN) for up to `timeout` (forever: `None`). Which are readable;
/// none when a signal cut it short.
pub fn poll(fds: &[RawFd], timeout: Option<Duration>) -> io::Result<Vec<bool>> {
    let mut set: Vec<libc::pollfd> = fds.iter().map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
    let ms = timeout.map_or(-1, |t| t.as_millis().min(i32::MAX as u128) as i32);
    let n = unsafe { libc::poll(set.as_mut_ptr(), set.len() as libc::nfds_t, ms) };
    if n < 0 {
        let e = io::Error::last_os_error();
        return if e.kind() == io::ErrorKind::Interrupted { Ok(vec![false; fds.len()]) } else { Err(e) };
    }
    Ok(set.iter().map(|p| p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0).collect())
}
