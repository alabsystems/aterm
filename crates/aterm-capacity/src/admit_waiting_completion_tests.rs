// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopDuring {
    Nap,
    TurnNap,
    Resize,
    Poll,
}

/// The first look waits; its wait line makes the next look fit. The caller's stop is
/// true for exactly one observation: in the nap, in the already granted claim's resize, or
/// inside the next look, after that look took the ledger's turn.
fn check_transient_stop(during: StopDuring) {
    use std::cell::Cell;

    struct Resize<'a>(Option<&'a Cell<bool>>);

    impl Relief for Resize<'_> {
        fn relieve(&mut self, _: &Shortfall, _: &mut dyn FnMut(&str), _: &dyn Fn() -> bool) {}

        fn holders(&mut self, _: &Shortfall, _: &dyn Fn() -> bool) -> Option<String> {
            None
        }

        fn resized(&mut self, _: &mut dyn FnMut(&str), stopped: &dyn Fn() -> bool) -> Option<u64> {
            if let Some(request) = self.0 {
                request.set(true);
                assert!(stopped(), "the resize saw the caller's stop");
            }
            None
        }
    }

    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let machine = crate::Fixed::new(Reading::of_gib(48, 6, 14));
    let request = Cell::new(false);
    // The turn tries counted when the stop was armed: the stop comes once a later look has
    // tried the ledger's turn ([`Ledger::turn_tries`]).
    let armed = Cell::new(None::<u64>);
    let mut resize = Resize((during == StopDuring::Resize).then_some(&request));
    let mut turn = (during == StopDuring::TurnNap).then(|| ledger.hold_turn().unwrap().unwrap());
    let mode = if turn.is_some() {
        Admit::Now
    } else {
        Admit::Wait
    };
    let mut waited = false;
    let result = admit_waiting(
        &ledger,
        &machine,
        Need::new("wait then fit").mem_gib(16).margin(1),
        mode,
        Pace {
            poll: Duration::ZERO,
            line_every: Duration::ZERO,
        },
        &mut resize,
        &mut |_| {
            if !waited {
                waited = true;
                drop(turn.take());
                machine.set(Reading::of_gib(48, 40, 14));
                request.set(matches!(during, StopDuring::Nap | StopDuring::TurnNap));
                if during == StopDuring::Poll {
                    armed.set(Some(ledger.turn_tries()));
                }
            }
        },
        &|| {
            request.replace(false)
                || (armed.get().is_some_and(|tries| ledger.turn_tries() > tries)
                    && armed.take().is_some()
                    && {
                        // The turn is a lock on an open file: this process's second take of
                        // it is refused while the look holds it.
                        assert!(
                            matches!(ledger.hold_turn(), Ok(Err(_))),
                            "the one-shot stop came while the look held the ledger's turn"
                        );
                        true
                    })
        },
    );
    assert!(waited);
    let Err(Error::Stopped {
        signal: None,
        short: Some(short),
    }) = result
    else {
        panic!("a stop during {during:?} must end admission, got {result:?}");
    };
    if during == StopDuring::TurnNap {
        assert!(matches!(short.shorts.as_slice(), [Short::Turn { .. }]));
    } else {
        assert!(
            short
                .shorts
                .iter()
                .any(|s| matches!(s, Short::Memory { .. }))
        );
    }
    assert!(ledger.list().unwrap().is_empty());
    assert!(
        ledger.hold_turn().unwrap().is_ok(),
        "the stopped wait let the ledger's turn go"
    );
}

#[test]
fn a_one_shot_stop_in_a_nap_ends_the_wait() {
    check_transient_stop(StopDuring::Nap);
}

#[test]
fn a_one_shot_stop_in_a_contended_now_nap_ends_the_wait() {
    check_transient_stop(StopDuring::TurnNap);
}

#[test]
fn a_one_shot_stop_in_a_post_grant_resize_drops_the_claim() {
    check_transient_stop(StopDuring::Resize);
}

/// A caller's stop seen once, at a look's ask after the look, while the look holds the ledger's
/// turn, ends the wait as `Stopped` and lets the turn go: the look answers `PollStopped`, and the
/// wait ends with the stop it kept, never as a ledger failure. The stop says so once, where
/// `tests::a_caller_stop_first_seen_by_a_looks_own_check_ends_the_wait_as_stopped`'s says so from
/// its ask on. NEGATIVE CONTROLS, observed: with the look's stop answered as
/// `Error::Ledger(Interrupted)`, as it was, this ends `` a stop during Poll must end admission,
/// got Err(Ledger(Kind(Interrupted))) ``; with the look's turn let go before its ask after the
/// look, `` the one-shot stop came while the look held the ledger's turn ``.
#[test]
fn a_one_shot_stop_inside_a_look_under_its_turn_ends_the_wait() {
    check_transient_stop(StopDuring::Poll);
}

/// A caller stop observed by an interruptible holder scan wins over the refusal that scan
/// was preparing. Negative control: without the check after `holders`, this is `Unmet`.
#[test]
fn a_stop_during_unmet_holder_discovery_is_reported_as_stopped() {
    struct StopsInHolders<'a>(&'a AtomicBool);

    impl Relief for StopsInHolders<'_> {
        fn relieve(
            &mut self,
            _short: &Shortfall,
            _say: &mut dyn FnMut(&str),
            _stopped: &dyn Fn() -> bool,
        ) {
        }

        fn holders(&mut self, _short: &Shortfall, _stopped: &dyn Fn() -> bool) -> Option<String> {
            self.0.store(true, Ordering::SeqCst);
            Some("holder scan stopped".to_owned())
        }
    }

    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let mut reading = Reading::of_gib(48, 40, 14);
    reading.volume = Some(Volume {
        mount: PathBuf::from("/vol"),
        total: 40 * GIB,
        free: 20 * GIB,
        floor: 4 * GIB,
        room: 2 * GIB,
    });
    let machine = crate::Fixed::new(reading);
    let stopped = AtomicBool::new(false);
    let mut relief = StopsInHolders(&stopped);
    let got = admit_waiting(
        &ledger,
        &machine,
        Need::new("verify").disk(27 * GIB, "/vol"),
        Admit::Wait,
        Pace::default(),
        &mut relief,
        &mut |_| {},
        &|| stopped.load(Ordering::SeqCst),
    );

    let Err(Error::Stopped {
        signal: None,
        short: Some(short),
    }) = &got
    else {
        panic!("the stop during holder discovery wins, got {got:?}");
    };
    assert!(short.shorts.iter().any(Short::unservable), "{short}");
    assert!(ledger.list().unwrap().is_empty(), "the stopped claim left");
}
