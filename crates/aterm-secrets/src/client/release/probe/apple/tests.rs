// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::release::probe::model::{Artifact, Repository, Subject};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::os::unix::process::ExitStatusExt as _;

const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

#[test]
fn subprocess_diagnostics_keep_only_fixed_step_program_and_error_class() {
    let mut command = aterm_uds::spawnfd::command("/private/hidden/codesign");
    command.args(["--force", "--sign", "DO-NOT-PRINT-THIS"]);
    let error = aterm_uds::deadline::CompleteOutputError::Run(aterm_uds::deadline::RunError::Wait(
        std::io::Error::other("PRIVATE-STDERR"),
    ));
    let error = ToolError::observed(&command, &error, true);
    assert!(!error.not_started());
    assert_eq!(
        error.to_string(),
        "probe step=sign program=codesign class=wait"
    );
    let refused = ToolError::observed(
        &command,
        &aterm_uds::deadline::CompleteOutputError::Run(aterm_uds::deadline::RunError::Spawn(
            std::io::Error::other("PRIVATE-ENV"),
        )),
        false,
    );
    assert!(refused.not_started());
    assert_eq!(
        refused.to_string(),
        "probe step=sign program=codesign class=spawn"
    );
    let after = ToolError::observed(
        &command,
        &aterm_uds::deadline::CompleteOutputError::Run(aterm_uds::deadline::RunError::Spawn(
            std::io::Error::other("PRIVATE-READER"),
        )),
        true,
    );
    assert!(!after.not_started());
    assert_eq!(
        after.to_string(),
        "probe step=sign program=codesign class=reader-spawn"
    );
    let capture = ToolError::observed(
        &command,
        &aterm_uds::deadline::CompleteOutputError::Run(aterm_uds::deadline::RunError::Capture(
            std::io::Error::other("PRIVATE-CAPTURE-DIAGNOSTIC"),
        )),
        false,
    );
    assert!(
        !capture.not_started(),
        "a dispatched capture failure outranks a missing start observer"
    );
    assert_eq!(
        capture.to_string(),
        "probe step=sign program=codesign class=capture"
    );
}

struct Fake {
    replies: RefCell<VecDeque<std::result::Result<Output, ToolError>>>,
    commands: RefCell<Vec<Vec<String>>>,
}
impl Tool for Fake {
    fn run(&self, command: &mut Command, _: Instant) -> std::result::Result<Output, ToolError> {
        self.commands.borrow_mut().push(
            std::iter::once(command.get_program())
                .chain(command.get_args())
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
        );
        self.replies
            .borrow_mut()
            .pop_front()
            .expect("scripted native output")
    }
}
fn answer(value: Value) -> std::result::Result<Output, ToolError> {
    Ok(Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: aterm_json::to_vec(&value).unwrap(),
        stderr: Vec::new(),
    })
}
fn journal() -> Journal {
    Journal {
        nonce: "12".repeat(16),
        subject: Subject {
            uid: 501,
            node: "23".repeat(16),
            member: "34".repeat(16),
            writer: "45".repeat(32),
            incarnation: "56".repeat(16),
            network: "67".repeat(16),
            authority: "78".repeat(32),
            genesis: "89".repeat(32),
            vault: "9a".repeat(32),
        },
        repositories: vec![Repository::new("fixture".into())],
        created_ms: 1,
        updated_ms: 1,
        artifact: Some(Artifact {
            sha256: "ab".repeat(32),
            executable_sha256: "cd".repeat(32),
            bytes: 100,
            signing_identity: "ef".repeat(20),
            team: aterm_update_core::pins::APPLE_TEAM_ID.into(),
            executed: true,
        }),
        submission: Some(ID.into()),
        apple_accepted: false,
        pending: None,
        cleanup_only: false,
    }
}
fn notary<'a>(fake: &'a Fake, journal: &'a Journal) -> Notary<'a> {
    Notary {
        tool: fake,
        program: Path::new("/fixture/Xcode/Contents/Developer/usr/bin/notarytool"),
        profile: "fixture-profile",
        archive: Path::new("/private/aterm-probe-12121212121212121212121212121212.zip"),
        journal,
    }
}
fn fake(replies: Vec<std::result::Result<Output, ToolError>>) -> Fake {
    Fake {
        replies: RefCell::new(replies.into()),
        commands: RefCell::new(Vec::new()),
    }
}

#[test]
fn pre_spawn_refusal_can_retry_but_unknown_submission_only_observes() {
    let journal = journal();
    let fake = fake(vec![
        Err(ToolError::NotStarted),
        answer(aterm_json::json!({"id":ID})),
        Err(ToolError::Uncertain),
        answer(
            aterm_json::json!({"history":[{"id":ID,"name":"aterm-probe-12121212121212121212121212121212.zip"}]}),
        ),
    ]);
    let notary = notary(&fake, &journal);
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    assert!(matches!(
        notary.submit(deadline).unwrap(),
        Outcome::NotStarted(_)
    ));
    assert!(matches!(
        notary.submit(deadline).unwrap(),
        Outcome::Applied(Receipt::Submitted(_))
    ));
    assert!(matches!(
        notary.submit(deadline).unwrap(),
        Outcome::Pending(_)
    ));
    assert!(matches!(
        notary.observe_submission(deadline).unwrap(),
        Outcome::Applied(Receipt::Submitted(_))
    ));
    let commands = fake.commands.borrow();
    assert_eq!(
        commands
            .iter()
            .map(|args| args[1].as_str())
            .collect::<Vec<_>>(),
        ["submit", "submit", "submit", "history"]
    );
    assert!(
        commands
            .iter()
            .all(|args| args[0].ends_with("/usr/bin/notarytool"))
    );
}

#[test]
fn acceptance_requires_the_exact_submission_archive_and_hash() {
    let journal = journal();
    for hash in ["ab".repeat(32), "ff".repeat(32)] {
        let fake = fake(vec![
            answer(aterm_json::json!({"id":ID,"status":"Accepted"})),
            answer(
                aterm_json::json!({"jobId":ID,"status":"Accepted","statusCode":0,"sha256":hash,"archiveFilename":"aterm-probe-12121212121212121212121212121212.zip"}),
            ),
        ]);
        let result =
            notary(&fake, &journal).accepted(Instant::now() + std::time::Duration::from_secs(60));
        assert_eq!(result.is_ok(), hash == "ab".repeat(32));
    }
}

#[test]
fn duplicate_archive_history_is_not_an_owned_submission_receipt() {
    let journal = journal();
    let row =
        aterm_json::json!({"id":ID,"name":"aterm-probe-12121212121212121212121212121212.zip"});
    let fake = fake(vec![answer(
        aterm_json::json!({"history":[row.clone(),row]}),
    )]);
    assert!(
        notary(&fake, &journal)
            .observe_submission(Instant::now() + std::time::Duration::from_secs(60))
            .is_err()
    );
    assert_eq!(fake.commands.borrow().len(), 1);
}
