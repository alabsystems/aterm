// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use super::read_main_ready;
use crate::Wake;
use crate::control::control_media::{Hops, MainHopError, MainPost, take_hop};

#[test]
fn one_dims_request_waits_for_a_coherent_read_of_the_live_grid() {
    let app = crate::App::headless_for_test();
    let term = app.front_terminal(crate::WindowId(0)).unwrap().term.clone();
    let mut held = Some(term.lock().unwrap());
    let hops: &'static Hops = Box::leak(Box::new(Hops::new()));
    let mut seen = 0;
    let mut post = MainPost::injected(
        |wake| {
            let Wake::ReadDims {
                session,
                term,
                reply,
            } = take_hop(wake)
            else {
                panic!("a dims read");
            };
            seen += 1;
            let snapshot = app.try_dims_snapshot(session, &term);
            if seen == 1 {
                assert!(snapshot.is_err(), "a locked grid supplies no sample");
                let mut guard = held.take().unwrap();
                guard.resize(31, 91);
                drop(guard);
            }
            reply.send(snapshot).unwrap();
            true
        },
        hops,
    );
    let snapshot = read_main_ready(
        &mut post,
        |reply| Wake::ReadDims {
            session: 0,
            term: term.clone(),
            reply,
        },
        |answer| Ok(answer.ok()),
        Instant::now(),
    )
    .expect("one request reads the grid after its holder releases it");
    drop(post);
    assert_eq!(seen, 2);
    assert_eq!((snapshot.rows, snapshot.cols), (31, 91));
    assert_eq!(hops.since_ns(), None, "no outstanding UI hop");
}

#[test]
fn an_expired_read_posts_no_new_ui_work() {
    let hops: &'static Hops = Box::leak(Box::new(Hops::new()));
    let mut post = MainPost::injected(|_| panic!("expired before posting"), hops);
    let result = read_main_ready::<u8, Option<u8>>(
        &mut post,
        |_| panic!("expired before building the event"),
        Ok,
        Instant::now() - Duration::from_secs(61),
    );
    assert_eq!(
        result,
        Err(MainHopError::Reason(super::MAIN_THREAD_REPLY_LATE))
    );
    assert_eq!(hops.since_ns(), None);
}

#[test]
fn a_dropped_read_is_a_refusal_instead_of_pending() {
    let hops: &'static Hops = Box::leak(Box::new(Hops::new()));
    let mut post = MainPost::injected(
        |wake| {
            drop(take_hop(wake));
            true
        },
        hops,
    );
    let term = std::sync::Arc::new(std::sync::Mutex::new(aterm_core::terminal::Terminal::new(
        24, 80,
    )));
    let result = read_main_ready(
        &mut post,
        |reply| Wake::ReadDims {
            session: 0,
            term: term.clone(),
            reply,
        },
        |answer| Ok(answer.ok()),
        Instant::now(),
    );
    assert_eq!(
        result.err(),
        Some(MainHopError::Reason("main-thread reply dropped"))
    );
    assert_eq!(hops.since_ns(), None);
}

#[test]
fn a_terminal_read_projection_refuses_without_reposting() {
    const REFUSAL: &str = "scripted terminal refusal";
    let hops: &'static Hops = Box::leak(Box::new(Hops::new()));
    let mut seen = 0;
    let mut post = MainPost::injected(
        |wake| {
            let Wake::ReadDims { reply, .. } = take_hop(wake) else {
                panic!("a dims read");
            };
            seen += 1;
            assert_eq!(seen, 1, "a terminal refusal cannot post another read");
            reply.send(Err(REFUSAL)).unwrap();
            true
        },
        hops,
    );
    let term = std::sync::Arc::new(std::sync::Mutex::new(aterm_core::terminal::Terminal::new(
        24, 80,
    )));
    let result = read_main_ready(
        &mut post,
        |reply| Wake::ReadDims {
            session: 0,
            term: term.clone(),
            reply,
        },
        |answer| answer.map(Some).map_err(MainHopError::Reason),
        Instant::now(),
    );
    drop(post);
    assert_eq!(result.err(), Some(MainHopError::Reason(REFUSAL)));
    assert_eq!(seen, 1);
    assert_eq!(hops.since_ns(), None);
}
