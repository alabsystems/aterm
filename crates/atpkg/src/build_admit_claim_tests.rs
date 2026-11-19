// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The public claim on a stand-in seat: a stop a look's own check sees.

use std::cell::RefCell;
use std::os::fd::OwnedFd;

use aterm_capacity::{Admit, Fixed, Machine, Pace, Procs, Reading};

use super::*;

/// `base`, but its first read that names a volume closes the status reader, as a caller that
/// went away does. The claim's own read of the machine names none; the look's names its waiter's
/// working directory. `rows` keeps what the ledger held then: the waiter's row, unheld, once the
/// read is the look's.
struct ReaderGoesInALook<'a> {
    base: Fixed,
    reader: RefCell<Option<std::io::PipeReader>>,
    ledger: &'a Ledger,
    rows: RefCell<Option<Vec<(u32, bool)>>>,
}

impl Machine for ReaderGoesInALook<'_> {
    fn read(&self, path: Option<&Path>) -> Reading {
        if path.is_some()
            && let Some(reader) = self.reader.borrow_mut().take()
        {
            let rows = self.ledger.list().expect("the ledger during the look");
            *self.rows.borrow_mut() = Some(
                rows.into_iter()
                    .map(|row| (row.pid, row.held.is_some()))
                    .collect(),
            );
            drop(reader);
        }
        self.base.read(path)
    }

    fn procs(&self) -> Option<Procs> {
        self.base.procs()
    }
}

/// A claim whose status reader goes away inside a look, after the check before that look let it
/// go on, is ended by the look's own check after its read: it says `atpkg claim: wait ended
/// before the command started` and exits 75, as a stop seen between looks does, and leaves no
/// waiter. NEGATIVE CONTROL, by hand: with the look's stop answered as main answered it
/// (`Err(io::ErrorKind::Interrupted.into())` for the `PollStopped` arm), the claim said
/// `atpkg claim: operation interrupted`.
#[test]
fn a_claim_whose_status_reader_goes_in_a_look_says_its_wait_ended_and_exits_75() {
    let root = aterm_tempfile::Builder::new()
        .prefix("atpkg-claim-stop-in-a-look-")
        .tempdir()
        .expect("a scratch root");
    let ledger = Ledger::at(root.path().join("ledger"));
    let (reader, writer) = aterm_uds::pipe().expect("the status pipe");
    let machine = ReaderGoesInALook {
        base: Fixed::new(Reading::of_gib(32, 28, 8)),
        reader: RefCell::new(Some(reader)),
        ledger: &ledger,
        rows: RefCell::new(None),
    };
    let seat = Seat {
        ledger: Some(ledger.clone()),
        machine: &machine,
        own: None,
        verify: None,
        mode: Admit::Wait,
        pace: Pace::default(),
        pid: std::process::id(),
        marker: None,
    };
    // A program that is not there: a claim the stop failed to end could only fail its exec,
    // never replace this test binary.
    let absent = root.path().join("absent-program");
    let request = PublicClaim {
        what: "claim stopped in a look".into(),
        key: "claim-stop-in-a-look-v1".into(),
        checkout: root.path().to_path_buf(),
        rank: Rank::Ordinary,
        cores: 1,
        mem_bytes: Some(0),
        alone: false,
        flock: None,
        no_wait: false,
        since_ms: None,
        wait_ms: None,
        status: File::from(OwnedFd::from(writer)),
        command: vec![absent.into()],
    };
    let (code, said) = claimed(ClaimCommand::Exec(request), &seat);
    assert_eq!(
        *machine.rows.borrow(),
        Some(vec![(std::process::id(), false)]),
        "the reader went in the look: the waiter's row was laid and unheld"
    );
    assert_eq!(
        said.as_deref(),
        Some("atpkg claim: wait ended before the command started"),
        "the claim says its wait ended"
    );
    assert_eq!(code, ExitCode::from(75), "and exits 75, starting nothing");
    assert!(
        ledger.list().expect("the ledger after").is_empty(),
        "the stopped claim left no waiter"
    );
}
