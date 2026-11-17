// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;

use aterm_net::fleet_member::name_record::ByKind;
use aterm_types::machine_name::Nickname;

use aterm_db::file::Renamed;

use super::{Queued, Written, note, offer, queue_rename, refusal, row_label, row_nick, write_with};

const ROW: &str = "mc-0123456789abcdef";

fn heron() -> Nickname {
    Nickname::parse("Heron").expect("a nickname")
}

/// The cells a rename to `m12 Heron` leaves in the row.
fn cells() -> Vec<(Renamed, String)> {
    vec![
        (Renamed::Label, "m12".to_owned()),
        (Renamed::Nickname, "Heron".to_owned()),
    ]
}

/// One write `write_with` asked for: the row, its cells and the author's word.
type Call = (String, Vec<(Renamed, String)>, String);

/// The write a door that queues it answers.
fn wi() -> Queued {
    Queued {
        id: "01ab".to_owned(),
        unflushed: None,
    }
}

/// Each write `write_with` asks for, and what it made of `answer`.
fn asked(
    row: Option<&str>,
    kind: ByKind,
    answer: Result<Option<Queued>, &str>,
) -> (Written, Vec<Call>) {
    let calls = RefCell::new(Vec::new());
    let written = write_with(row, &cells(), kind, |row, cells, by| {
        calls.borrow_mut().push((
            row.to_owned(),
            cells
                .iter()
                .map(|(cell, value)| (*cell, (*value).to_owned()))
                .collect(),
            by.to_owned(),
        ));
        answer.map_err(str::to_owned)
    });
    (written, calls.into_inner())
}

/// A rename's cells reach this Mac's own row, by its key, as one write, a person's act, and the
/// answer names the intent that holds them.
#[test]
fn a_rename_queues_its_cells_as_one_registry_row_write() {
    let (written, calls) = asked(Some(ROW), ByKind::Person, Ok(Some(wi())));
    assert_eq!(calls, [(ROW.to_owned(), cells(), "person".to_owned())]);
    assert_eq!(written, Written::Queued(wi()));
}

#[test]
fn an_answer_of_two_lines_is_one_note() {
    assert_eq!(
        note("queued 01ab: the store is busy\nnote: the outbox was not flushed\n"),
        "lab registry: queued 01ab: the store is busy; note: the outbox was not flushed"
    );
}

#[test]
fn a_row_that_says_it_already_is_unchanged() {
    let (written, calls) = asked(Some(ROW), ByKind::Person, Ok(None));
    assert_eq!(calls.len(), 1);
    assert_eq!(written, Written::Unchanged);
}

#[test]
fn a_program_rename_is_written_as_a_program_act() {
    for kind in [ByKind::Agent, ByKind::Auto] {
        let (_, calls) = asked(Some(ROW), kind, Ok(Some(wi())));
        assert_eq!(calls[0].2, "program", "{kind:?}");
    }
}

#[test]
fn no_row_of_this_mac_writes_nothing() {
    let (written, calls) = asked(None, ByKind::Person, Ok(Some(wi())));
    assert!(calls.is_empty(), "{calls:?}");
    assert_eq!(written, Written::Unbound);
}

#[test]
fn a_refused_write_says_why() {
    let why = "lab/machines: row mc-0123456789abcdef is not published yet";
    let (written, calls) = asked(Some(ROW), ByKind::Person, Err(why));
    assert_eq!(calls.len(), 1);
    assert_eq!(written, Written::Refused(why.to_owned()));
}

/// A rename of a name kept anywhere but this account's live state writes no registry row, and a
/// read of one follows none ([`row_nick`], [`row_label`]): on
/// 2026-11-15 a build without this rule let `name_cli`'s tests, renaming scratch names, queue
/// owner writes of the developer's own Mac's row, two of which were signed.
#[test]
fn a_rename_of_a_name_kept_elsewhere_writes_no_row() {
    // This binary's own state root is private (a test binary's scratch root): were it not, the
    // second case would name the live state and write the real row, so it must not run.
    assert!(aterm_types::dirs::runs_under_private_state_root());
    let scratch = aterm_tempfile::tempdir().expect("a scratch folder");
    for state in [
        scratch.path().to_path_buf(),
        aterm_types::dirs::state_dir().expect("a state folder"),
    ] {
        assert_eq!(
            queue_rename(&state, &cells(), ByKind::Person, None),
            Written::Unbound,
            "{}",
            state.display()
        );
        assert_eq!(offer(&state, &wi()), None, "{}", state.display());
        assert_eq!(refusal(&state, &cells()), None, "{}", state.display());
        assert_eq!(row_nick(&state), None, "{}", state.display());
        assert_eq!(row_label(&state), None, "{}", state.display());
        assert!(super::for_follow(&state).is_none(), "{}", state.display());
        assert!(super::for_number(&state).is_none(), "{}", state.display());
    }
}

/// The nickname follow of the seam a test installed.
fn follow_of(seam: &mut Option<super::Seam>) -> &mut super::Follow {
    seam.as_mut()
        .and_then(|seam| seam.follow.as_mut())
        .expect("a follow seam")
}

#[test]
fn automatic_follow_requires_admission_and_the_current_owning_number() {
    use crate::harness::{account_sync_error::SyncError, alab_roster::SourceGuard};
    use crate::machine_name::{file, follow_registry};
    use aterm_net::fleet_member::name_record::NickClass;
    use aterm_types::machine_name::{Class, Number};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    struct Admission(Arc<AtomicBool>);
    impl SourceGuard for Admission {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn same_source(&self, other: &dyn SourceGuard) -> bool {
            other
                .as_any()
                .downcast_ref::<Self>()
                .is_some_and(|other| Arc::ptr_eq(&self.0, &other.0))
        }
        fn with_admitted(
            &self,
            save: Box<dyn FnOnce() -> Result<(), SyncError> + '_>,
        ) -> Result<(), SyncError> {
            if !self.0.load(Ordering::Acquire) {
                return Err("the retained source no longer admits local writes".into());
            }
            save()
        }
    }

    let root = aterm_tempfile::tempdir().unwrap();
    let number = Number::new(85).unwrap();
    let mut held = file::MachineName::unnumbered(Nickname::parse("dog").unwrap(), 1);
    held.num = Some(number);
    held.class = Class::Chosen;
    held.nick_class = NickClass::Chosen;
    held.row = file::RowSeen::Saw(Some(held.nick.clone()));
    file::read_or_create(root.path(), || Ok(held.clone())).unwrap();
    let allowed = Arc::new(AtomicBool::new(false));
    let input = super::Follow {
        number: Some(number),
        nickname: Some(heron()),
        guard: Some(super::ReceiptGuard::new(Admission(allowed.clone()))),
        row: None,
    };
    let seam = super::Seam {
        follow: Some(input.clone()),
        ..super::Seam::default()
    };
    let previous = super::ROW.with(|row| row.replace(Some(seam)));
    struct Reset(Option<super::Seam>);
    impl Drop for Reset {
        fn drop(&mut self) {
            super::ROW.with(|row| row.replace(self.0.take()));
        }
    }
    let _reset = Reset(previous);

    assert_eq!(
        row_nick(root.path()),
        Some(Some(heron())),
        "history remains readable"
    );
    assert!(follow_registry(root.path(), 2).is_err());
    assert_eq!(
        file::read(root.path()).unwrap(),
        Some(held.clone()),
        "a refused source changes neither nickname, RowSeen nor history"
    );

    allowed.store(true, Ordering::Release);
    super::ROW.with(|row| follow_of(&mut row.borrow_mut()).number = Number::new(86));
    assert_eq!(follow_registry(root.path(), 3).unwrap(), None);
    assert_eq!(file::read(root.path()).unwrap(), Some(held.clone()));
    super::ROW.with(|row| follow_of(&mut row.borrow_mut()).number = Some(number));
    let followed = follow_registry(root.path(), 4).unwrap().unwrap();
    assert_eq!(followed.nick, heron());
    assert_eq!(followed.num, held.num);
    assert_eq!(followed.rev, held.rev + 1);
    assert_eq!(followed.row, file::RowSeen::Saw(Some(heron())));
    assert_eq!(followed.history.len(), held.history.len() + 1);
    assert_eq!(follow_registry(root.path(), 5).unwrap(), None);
}
