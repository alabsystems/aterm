// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::release::probe::model::{Repository, Subject};
use std::collections::VecDeque;
use std::time::Duration;

fn journal() -> Journal {
    Journal {
        nonce: "12".repeat(16),
        subject: Subject {
            uid: 501,
            node: "34".repeat(16),
            member: "56".repeat(16),
            writer: "bc".repeat(32),
            incarnation: "78".repeat(16),
            network: "9a".repeat(16),
            authority: "cd".repeat(32),
            genesis: "de".repeat(32),
            vault: "ef".repeat(32),
        },
        repositories: vec![Repository::new("fixture-owner".into())],
        created_ms: 1,
        updated_ms: 1,
        artifact: Some(Artifact {
            sha256: "ab".repeat(32),
            executable_sha256: "cd".repeat(32),
            bytes: 100,
            signing_identity: "CD".repeat(20),
            team: aterm_update_core::pins::APPLE_TEAM_ID.into(),
            executed: true,
        }),
        submission: None,
        apple_accepted: false,
        pending: None,
        cleanup_only: false,
    }
}

#[derive(Default)]
struct Disk {
    saves: Vec<Journal>,
    fail: bool,
}

impl Persistence for Disk {
    fn save(&mut self, journal: &Journal) -> Result<()> {
        if self.fail {
            return Err(Error::new("disk unavailable"));
        }
        self.saves.push(journal.clone());
        Ok(())
    }
}

#[derive(Default)]
struct Fake {
    performed: Vec<Action>,
    observed: Vec<Action>,
    answers: VecDeque<Outcome>,
    revoked: bool,
    historical: bool,
    final_checks: usize,
}

impl Host for Fake {
    fn authorize(&mut self, _: &Journal, _: Instant) -> Result<()> {
        if self.revoked {
            Err(Error::new("revoked"))
        } else {
            Ok(())
        }
    }
    fn perform(&mut self, j: &Journal, action: Action, _: Instant) -> Result<Outcome> {
        assert_eq!(j.pending, Some(action));
        self.performed.push(action);
        Ok(self.answers.pop_front().unwrap())
    }
    fn observe(&mut self, _: &Journal, action: Action, _: Instant) -> Result<Outcome> {
        self.observed.push(action);
        Ok(self.answers.pop_front().unwrap())
    }
    fn notary(&mut self, _: &Journal, _: Instant) -> Result<Outcome> {
        Ok(self.answers.pop_front().unwrap())
    }
    fn final_check(&mut self, _: &Journal, _: bool, _: Instant) -> Result<Finality> {
        self.final_checks += 1;
        Ok(if self.historical {
            Finality::Historical
        } else {
            Finality::Fresh
        })
    }
    fn now_ms(&self) -> u64 {
        2
    }
}

fn run(j: &mut Journal, host: &mut Fake, disk: &mut Disk) -> Result<Progress> {
    step(j, host, disk, Instant::now() + Duration::from_secs(60))
}

#[test]
fn lost_submit_response_is_read_back_without_another_submission() {
    let mut j = journal();
    let mut host = Fake::default();
    host.answers.extend([
        Outcome::Pending("response lost"),
        Outcome::Applied(Receipt::Submitted(
            "01234567-89ab-cdef-0123-456789abcdef".into(),
        )),
    ]);
    let mut disk = Disk::default();
    assert_eq!(
        run(&mut j, &mut host, &mut disk).unwrap(),
        Progress::Pending("response lost")
    );
    assert_eq!(disk.saves.last().unwrap().pending, Some(Action::Submit));
    assert_eq!(
        run(&mut j, &mut host, &mut disk).unwrap(),
        Progress::Advanced
    );
    assert_eq!(host.performed, [Action::Submit]);
    assert_eq!(host.observed, [Action::Submit]);
    assert_eq!(
        j.submission.as_deref(),
        Some("01234567-89ab-cdef-0123-456789abcdef")
    );
    assert_eq!(j.next(), Next::PollNotary);
}

#[test]
fn an_empty_readback_does_not_prove_an_unknown_post_never_started() {
    let mut j = journal();
    j.pending = Some(Action::Submit);
    let mut host = Fake::default();
    host.answers
        .push_back(Outcome::NotStarted("not listed yet"));
    let mut disk = Disk::default();
    assert!(run(&mut j, &mut host, &mut disk).is_err());
    assert_eq!(j.pending, Some(Action::Submit));
    assert!(host.performed.is_empty());
    assert!(disk.saves.is_empty());
}

#[test]
fn only_explicit_unsigned_cleanup_can_retire_a_pending_sign_without_signing() {
    for cleanup_only in [false, true] {
        let mut j = journal();
        j.artifact = None;
        j.pending = Some(Action::Sign);
        j.cleanup_only = cleanup_only;
        let mut host = Fake::default();
        host.answers
            .push_back(Outcome::Applied(Receipt::UnsignedRetained));
        let mut disk = Disk::default();
        if cleanup_only {
            assert_eq!(
                run(&mut j, &mut host, &mut disk).unwrap(),
                Progress::Advanced
            );
            assert!(j.artifact.is_none() && j.pending.is_none());
            assert_eq!(run(&mut j, &mut host, &mut disk).unwrap(), Progress::Clean);
            assert_eq!(host.final_checks, 1);
        } else {
            assert!(run(&mut j, &mut host, &mut disk).is_err());
            assert_eq!(j.pending, Some(Action::Sign));
            assert!(disk.saves.is_empty());
        }
        assert!(host.performed.is_empty());
        assert_eq!(host.observed, [Action::Sign]);
    }
}

#[test]
fn failed_intent_persistence_performs_no_external_action() {
    let mut j = journal();
    let mut host = Fake::default();
    let mut disk = Disk {
        fail: true,
        ..Default::default()
    };
    assert!(run(&mut j, &mut host, &mut disk).is_err());
    assert_eq!(j.pending, None);
    assert!(host.performed.is_empty());
}

#[test]
fn revoked_authority_stops_both_new_and_uncertain_actions() {
    for pending in [None, Some(Action::Submit)] {
        let mut j = journal();
        j.pending = pending;
        let mut host = Fake {
            revoked: true,
            ..Default::default()
        };
        let mut disk = Disk::default();
        assert!(run(&mut j, &mut host, &mut disk).is_err());
        assert!(host.performed.is_empty());
        assert!(host.observed.is_empty());
        assert!(disk.saves.is_empty());
    }
}

#[test]
fn apple_acceptance_of_another_archive_cannot_advance() {
    let mut j = journal();
    j.submission = Some("01234567-89ab-cdef-0123-456789abcdef".into());
    let mut host = Fake::default();
    host.answers.push_back(Outcome::Applied(Receipt::Accepted {
        submission: "01234567-89ab-cdef-0123-456789abcdef".into(),
        sha256: "ef".repeat(32),
    }));
    let mut disk = Disk::default();
    assert!(run(&mut j, &mut host, &mut disk).is_err());
    assert!(!j.apple_accepted);
    assert!(disk.saves.is_empty());
}

#[test]
fn mismatched_uploaded_bytes_leave_readback_unproven() {
    let mut j = journal();
    j.submission = Some("01234567-89ab-cdef-0123-456789abcdef".into());
    j.apple_accepted = true;
    let repo = &mut j.repositories[0];
    repo.commit = Some("de".repeat(20));
    repo.principal = Some("fixture-writer".into());
    repo.tag_owned = true;
    repo.release = Some(1);
    repo.asset = Some(2);
    let mut host = Fake::default();
    host.answers.push_back(Outcome::Applied(Receipt::Readback {
        sha256: "ab".repeat(32),
        bytes: 101,
    }));
    let mut disk = Disk::default();
    assert!(run(&mut j, &mut host, &mut disk).is_err());
    assert!(!j.repositories[0].verified);
    assert_eq!(j.pending, Some(Action::Readback(0)));
}

#[test]
fn completed_journal_flags_cannot_substitute_for_fresh_live_witnesses() {
    let mut journal = journal();
    journal.submission = Some("01234567-89ab-cdef-0123-456789abcdef".into());
    journal.apple_accepted = true;
    let repo = &mut journal.repositories[0];
    repo.commit = Some("de".repeat(20));
    repo.principal = Some("fixture-writer".into());
    repo.tag_owned = true;
    repo.release = Some(1);
    repo.asset = Some(2);
    repo.verified = true;
    repo.asset_absent = true;
    repo.release_absent = true;
    repo.tag_absent = true;
    let mut disk = Disk::default();
    let mut historical = Fake {
        historical: true,
        ..Fake::default()
    };
    assert_eq!(
        run(&mut journal, &mut historical, &mut disk).unwrap(),
        Progress::HistoricalComplete
    );
    assert_eq!(historical.final_checks, 1);
    assert!(historical.performed.is_empty());
    let mut fresh = Fake::default();
    assert_eq!(
        run(&mut journal, &mut fresh, &mut disk).unwrap(),
        Progress::Complete
    );
    assert_eq!(fresh.final_checks, 1);
    assert!(disk.saves.is_empty());
}
