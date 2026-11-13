// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Write as _;
fn pair() -> (Events, CtlStream) {
    let (host, client) = CtlStream::pair().unwrap();
    let binding = Binding {
        session: "s-a".into(),
        attempt: [7; 16],
    };
    (Events::new(host.into(), binding).unwrap(), client)
}
#[test]
fn cancellation_preserves_partial_frame_without_blocking_another_tick() {
    let (mut events, mut client) = pair();
    let cancel = Cancel {
        binding: events.binding.clone(),
    }
    .encode()
    .unwrap();
    let header = (cancel.len() as u32).to_be_bytes();
    client.write_all(&header[..2]).unwrap();
    assert!(!events.cancelled());
    client.write_all(&header[2..]).unwrap();
    client.write_all(&cancel[..3]).unwrap();
    assert!(!events.cancelled());
    client.write_all(&cancel[3..]).unwrap();
    assert!(events.cancelled());
}
#[test]
fn disconnect_and_oversized_header_withdraw_only_the_controller() {
    let (mut events, client) = pair();
    drop(client);
    assert!(events.cancelled());
    let (mut events, mut client) = pair();
    client.write_all(&u32::MAX.to_be_bytes()).unwrap();
    assert!(events.cancelled());
    assert!(events.input.len() <= 256);
}
#[test]
fn exact_cancel_keeps_actual_terminal_observable_but_replay_refuses() {
    let (mut events, mut client) = pair();
    let payload = Cancel {
        binding: events.binding.clone(),
    }
    .encode()
    .unwrap();
    client
        .write_all(&(payload.len() as u32).to_be_bytes())
        .unwrap();
    client.write_all(&payload).unwrap();
    assert!(events.cancelled());
    assert!(!events.failed);
    events
        .emit(Event::Finished {
            outcome: aterm_types::resource_admission::Outcome::Signal(15),
            cause: aterm_types::resource_admission::CompletionCause::Cancelled,
            runtime_ns: 1,
        })
        .unwrap();
    let bytes = aterm_ctl::resource_admission_transport::read_frame(
        &client,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .unwrap();
    assert!(matches!(
        EventFrame::decode(&bytes, &events.binding).unwrap().event,
        Event::Finished {
            cause: aterm_types::resource_admission::CompletionCause::Cancelled,
            ..
        }
    ));
    client.write_all(&[0]).unwrap();
    assert!(events.cancelled());
    assert!(events.failed);
    assert!(events.emit(Event::Queued).is_err());
}
