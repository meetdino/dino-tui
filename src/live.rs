//! dinod's live data, by long-poll: `StateChange { seen }` answers once what a client shows
//! differs (or after a few seconds), `OutputRate { seen }` once the rate does (or after a minute).
//! Each waits on a connection of its own, on a thread of its own, so the loop sleeps until dinod
//! answers.

use std::time::Duration;

use dino_core::ipc::{Request, Response};

use crate::app::Event;
use crate::conn::{self, Conn};
use crate::wake::Waker;

pub fn start(waker: Waker<Event>) {
    let w = waker.clone();
    std::thread::Builder::new().name("state".into()).spawn(move || state(w)).expect("a thread");
    std::thread::Builder::new().name("rate".into()).spawn(move || rate(waker)).expect("a thread");
}

/// A connection, once dinod is there: right away, else retried every second (only while it's gone).
fn connect(waker: &Waker<Event>, lost: &mut bool) -> Conn {
    loop {
        match Conn::open() {
            Ok(c) => return c,
            Err(_) => {
                if !*lost {
                    *lost = true;
                    waker.send(Event::Lost);
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn state(waker: Waker<Event>) {
    let mut lost = false;
    loop {
        let mut c = connect(&waker, &mut lost);
        let mut seen = None;
        loop {
            match c.request(&Request::StateChange { seen }) {
                Ok(resp @ Response::State { .. }) => {
                    lost = false;
                    if let Response::State { version, .. } = &resp {
                        seen = *version;
                    }
                    waker.send(Event::State(Box::new(resp)));
                }
                // A dinod from before `StateChange`: ask `State` (it answers at once), and wait in
                // between as `StateChange` would at most.
                Ok(Response::Error { message }) if conn::unknown_request(&message) => match c.request(&Request::State) {
                    Ok(resp) => {
                        waker.send(Event::State(Box::new(resp)));
                        std::thread::sleep(Duration::from_secs(2));
                    }
                    Err(_) => break,
                },
                Ok(_) => std::thread::sleep(Duration::from_secs(1)),
                Err(_) => break,
            }
        }
    }
}

fn rate(waker: Waker<Event>) {
    let mut lost = false;
    loop {
        let mut c = connect(&waker, &mut lost);
        let mut seen: Option<u32> = None;
        loop {
            match c.request(&Request::OutputRate { seen }) {
                Ok(Response::OutputRate { tokens_per_second }) => {
                    seen = Some(tokens_per_second);
                    waker.send(Event::Rate(Some(tokens_per_second)));
                }
                // A dinod from before `OutputRate` (0.1.11): the rate stays hidden, and nothing
                // asks again.
                Ok(Response::Error { message }) if conn::unknown_request(&message) => {
                    waker.send(Event::Rate(None));
                    return;
                }
                Ok(_) => std::thread::sleep(Duration::from_secs(5)),
                Err(_) => break,
            }
        }
    }
}
