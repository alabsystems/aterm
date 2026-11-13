// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

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
