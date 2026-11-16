// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use super::*;

fn failed(why: &'static str) -> EventLoopError {
    EventLoopError::Os(os_error!(OsError::Misc(why)))
}

fn unanswered(why: &'static str) -> Result<EventLoop<()>, Unbuilt> {
    Err(Unbuilt { error: failed(why), reached: false })
}

fn reached(why: &'static str) -> Result<EventLoop<()>, Unbuilt> {
    Err(Unbuilt { error: failed(why), reached: true })
}

/// A loop built by a backend that answered (the headless one stands in for it here).
fn built() -> Result<EventLoop<()>, Unbuilt> {
    Ok(EventLoop::Headless(headless::EventLoop::new().expect("a loop")))
}

fn unnamed() -> EventLoopError {
    failed("no display server is named")
}

#[test]
fn under_the_fallback_display_servers_that_never_answered_give_the_headless_loop_and_each_why() {
    let loop_ = EventLoop::first_built(
        [unanswered("no compositor at wayland-9"), unanswered("no X server at :9")],
        true,
        unnamed,
    )
    .expect("the headless loop");
    assert!(matches!(loop_, EventLoop::Headless(_)));
    let why = loop_.headless_fallback().expect("why it runs without one");
    assert!(why.contains("wayland-9") && why.contains(":9"), "{why}");
}

#[test]
fn a_display_server_that_answered_and_still_failed_is_the_error_even_under_the_fallback() {
    let err =
        EventLoop::first_built([reached("the X server lacks XInputExtension 2")], true, unnamed)
            .err()
            .expect("the backend's own failure");
    assert!(err.to_string().contains("XInputExtension"), "{err}");
}

#[test]
fn without_the_fallback_a_display_server_that_never_answered_is_the_error() {
    assert!(EventLoop::first_built([unanswered("no X server at :9")], false, unnamed).is_err());
}

#[test]
fn with_nothing_named_the_fallback_says_so_and_without_it_that_is_the_error() {
    let loop_ = EventLoop::<()>::first_built([], true, unnamed).expect("the headless loop");
    assert!(loop_
        .headless_fallback()
        .is_some_and(|why| why.contains("no display server is named")));
    assert!(EventLoop::<()>::first_built([], false, unnamed).is_err());
}

#[test]
fn a_later_display_server_that_answers_is_used_and_no_fallback_is_said() {
    let loop_ =
        EventLoop::first_built([unanswered("no compositor at wayland-9"), built()], true, unnamed)
            .expect("the loop the second one built");
    assert_eq!(loop_.headless_fallback(), None);
}

#[test]
fn no_backend_after_a_built_one_is_tried() {
    let tried = std::iter::once(built())
        .chain(std::iter::from_fn(|| panic!("a backend after a built loop was tried")));
    assert!(EventLoop::first_built(tried, true, unnamed).is_ok());
}
