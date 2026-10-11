//! dinod's live data, by long-poll: `StateChange { seen }` answers once what a client shows
//! differs (or after a few seconds), `OutputRate { seen, seen_input }` once the output or new input
//! rate does (or after a minute).
//! Each waits on a connection of its own, on a thread of its own, so the loop sleeps until dinod
//! answers.

use std::time::Duration;

use dino_core::ipc::{Request, Response};

use crate::app::{Event, Rate};
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
        let (mut seen, mut seen_input): (Option<u32>, Option<u32>) = (None, None);
        // Whether this dinod says the input rate (meetdino/dino#216): its answers carry the field.
        // Until it's known, only the output is waited on, as an older dinod does anyway.
        let mut says_input = false;
        loop {
            let req = Request::OutputRate { seen, seen_input: if says_input { seen_input } else { None } };
            // Typed both ways, read as JSON in between: a field's absence is what tells an older dinod.
            let answer = serde_json::to_value(&req).map_err(std::io::Error::other).and_then(|v| c.raw(&v));
            match answer {
                Ok(v) if conn::raw_error(&v).is_some_and(|m| conn::unknown_request(&m)) => {
                    // A dinod from before `OutputRate` (0.1.11): the rate stays hidden, and nothing
                    // asks again.
                    waker.send(Event::Rate(None));
                    return;
                }
                Ok(v) => {
                    says_input = v.get("input_tokens_per_second").is_some();
                    match serde_json::from_value::<Response>(v) {
                        Ok(Response::OutputRate { tokens_per_second, input_tokens_per_second }) => {
                            seen = Some(tokens_per_second);
                            seen_input = Some(input_tokens_per_second);
                            waker.send(Event::Rate(Some(Rate { out: tokens_per_second, input: says_input.then_some(input_tokens_per_second) })));
                        }
                        _ => std::thread::sleep(Duration::from_secs(5)),
                    }
                }
                Err(_) => break,
            }
        }
    }
}
