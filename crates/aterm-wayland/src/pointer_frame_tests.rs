// Copyright (c) 2018 Victor Berger
// SPDX-License-Identifier: MIT
//
// Derived from smithay-client-toolkit 0.19.2 src/seat/pointer/mod.rs (the
// `wl_pointer` event handling: a frame's grouping and its axis merge) and
// modified by the aterm project in 2026. See ../LICENSE-MIT for the upstream
// MIT license.

use super::*;

const A: u32 = 1;
const B: u32 = 2;

fn enter(f: &mut Frames<u32>, s: u32) -> Option<Vec<PointerEvent<u32>>> {
    f.feed(Raw::Enter {
        serial: 10,
        surface: Some(s),
        x: 1.0,
        y: 2.0,
    })
}

#[test]
fn a_frame_hands_out_its_events_together() {
    let mut f = Frames::new(5);
    assert_eq!(enter(&mut f, A), None);
    assert_eq!(
        f.feed(Raw::Motion {
            time: 7,
            x: 3.0,
            y: 4.0
        }),
        None
    );
    let frame = f.feed(Raw::Frame).expect("a frame");
    assert_eq!(frame.len(), 2);
    assert_eq!(frame[0].kind, PointerKind::Enter { serial: 10 });
    assert_eq!(frame[0].position, (1.0, 2.0));
    assert_eq!(frame[1].kind, PointerKind::Motion { time: 7 });
    assert_eq!(frame[1].position, (3.0, 4.0));
    assert_eq!(f.feed(Raw::Frame), None, "an empty frame is nothing");
    assert_eq!(f.latest_enter(), Some(10));
}

#[test]
fn before_version_five_each_event_is_its_own() {
    let mut f = Frames::new(4);
    let got = enter(&mut f, A).expect("at once");
    assert_eq!(got.len(), 1);
    let got = f
        .feed(Raw::Button {
            serial: 11,
            time: 8,
            button: 0x110,
            state: PRESSED,
        })
        .expect("at once");
    assert_eq!(
        got[0].kind,
        PointerKind::Press {
            time: 8,
            button: 0x110,
            serial: 11
        }
    );
    assert_eq!(f.latest_button(), Some(11));
}

#[test]
fn a_frame_s_axis_events_merge_into_one_scroll() {
    let mut f = Frames::new(5);
    enter(&mut f, A);
    f.feed(Raw::Frame);
    f.feed(Raw::AxisSource(0));
    f.feed(Raw::Axis {
        time: 9,
        axis: VERTICAL,
        value: 15.0,
    });
    f.feed(Raw::AxisDiscrete {
        axis: VERTICAL,
        discrete: 1,
    });
    f.feed(Raw::Axis {
        time: 9,
        axis: HORIZONTAL,
        value: -3.0,
    });
    f.feed(Raw::AxisStop {
        time: 9,
        axis: HORIZONTAL,
    });
    let frame = f.feed(Raw::Frame).expect("a frame");
    assert_eq!(frame.len(), 1, "{frame:?}");
    assert_eq!(
        frame[0].kind,
        PointerKind::Axis {
            time: 9,
            horizontal: AxisScroll {
                absolute: -3.0,
                discrete: 0,
                stop: true
            },
            vertical: AxisScroll {
                absolute: 15.0,
                discrete: 1,
                stop: false
            },
            source: Some(0),
        }
    );
}

#[test]
fn a_leave_names_the_surface_left_and_events_after_it_are_dropped() {
    let mut f = Frames::new(5);
    enter(&mut f, A);
    f.feed(Raw::Leave {
        serial: 12,
        surface: Some(A),
    });
    f.feed(Raw::Motion {
        time: 1,
        x: 0.0,
        y: 0.0,
    });
    let frame = f.feed(Raw::Frame).expect("a frame");
    assert_eq!(frame.len(), 2, "the motion after the leave is dropped");
    assert_eq!(frame[1].kind, PointerKind::Leave { serial: 12 });
    assert_eq!(frame[1].surface, A);
    assert_eq!(f.surface(), None);
}

#[test]
fn a_leave_of_a_destroyed_surface_leaves_the_one_entered() {
    let mut f = Frames::new(5);
    enter(&mut f, B);
    f.feed(Raw::Leave {
        serial: 13,
        surface: None,
    });
    let frame = f.feed(Raw::Frame).expect("a frame");
    assert_eq!(frame[1].surface, B);
    assert_eq!(f.surface(), None);
}

#[test]
fn unknown_values_are_dropped_not_guessed() {
    let mut f = Frames::new(5);
    enter(&mut f, A);
    f.feed(Raw::Axis {
        time: 1,
        axis: 7,
        value: 1.0,
    });
    f.feed(Raw::AxisSource(9));
    f.feed(Raw::Button {
        serial: 14,
        time: 1,
        button: 0x110,
        state: 5,
    });
    let frame = f.feed(Raw::Frame).expect("the enter");
    assert_eq!(frame.len(), 1, "{frame:?}");
}

#[test]
fn axis_discrete_merge_saturates_instead_of_overflowing() {
    let mut f = Frames::new(5);
    enter(&mut f, A);
    f.feed(Raw::AxisDiscrete {
        axis: VERTICAL,
        discrete: i32::MAX,
    });
    f.feed(Raw::AxisDiscrete {
        axis: VERTICAL,
        discrete: 2,
    });
    let frame = f.feed(Raw::Frame).expect("axis frame");
    assert_eq!(
        frame[1].kind,
        PointerKind::Axis {
            time: 0,
            horizontal: AxisScroll::default(),
            vertical: AxisScroll {
                absolute: 0.0,
                discrete: i32::MAX,
                stop: false,
            },
            source: None,
        }
    );
}
