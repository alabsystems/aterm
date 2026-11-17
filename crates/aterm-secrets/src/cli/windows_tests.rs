// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::run_at;
use crate::cli::{Command, Fill};
use crate::client::{self, Membership, Paths};
use crate::text::{Audience, Document};
use std::process::ExitCode;

/// What a Windows member does with its vault, through the verbs it runs (`run_at`, as `aterm
/// secrets` runs them, each at a scratch home): before any join `status` answers and `get` has
/// nothing to read; a join of a vault at a path, pinned to its signer, asks to join and changes
/// nothing else; a sync before an admin lets the PC in says it is not in, and the first sync
/// after reads each entry in place, which `ls` and `status` then answer for; a write is refused,
/// exit 2, and the vault stays as it was. NEGATIVE CONTROL (run on Windows, 2026-11-16): with
/// `run_at` refusing `sync` as it refuses a write, the first sync answers 2 and this test fails.
#[test]
fn a_windows_member_joins_a_path_vault_and_reads_it_in_place_once_let_in() {
    let root = aterm_tempfile::tempdir().expect("scratch root");
    let vault = root.path().join("lab.vault");
    let admin = Paths::for_home(&root.path().join("admin"));
    let created =
        client::create(&admin, vault.to_str().expect("utf-8"), None, Some("admin")).expect("vault");
    let publish = |change: &dyn Fn(&mut Document)| {
        let opened = client::admin_open(&admin).expect("admin opens");
        let mut doc = opened.doc.clone();
        change(&mut doc);
        client::admin_publish(&admin, &opened, &doc).expect("publish");
    };
    publish(&|doc| {
        doc.set_entry(&Audience::Everyone, "token", b"the-vaults-token")
            .expect("set");
    });
    let pc = Paths::for_home(&root.path().join("pc"));
    let live = || Ok(pc.clone());
    let run = |command| run_at(command, &live);

    assert_eq!(run(Command::Status).expect("status"), ExitCode::SUCCESS);
    assert!(
        run(Command::Get("token".into())).is_err(),
        "no vault, no entry"
    );
    let source = format!("{}#{}", vault.display(), created.signer);
    let joined = run(Command::Join {
        source,
        name: Some("pc".into()),
        agent: true,
        wait: false,
        password_stdin: false,
    })
    .expect("join");
    assert_eq!(joined, ExitCode::SUCCESS);
    let asked = client::status(&pc).expect("status");
    assert!(matches!(asked.membership, Membership::Pending));
    assert_eq!(
        run(Command::Sync { diagnostic: false }).expect("sync"),
        ExitCode::FAILURE,
        "a PC nobody let in yet is not in the vault"
    );
    assert!(super::member::get_value(&pc, "token").is_err());

    let recipient = asked.machine.expect("a key").0.to_bech32();
    publish(&|doc| doc.add_machine("pc", &recipient).expect("admit"));
    assert_eq!(
        run(Command::Sync { diagnostic: false }).expect("sync"),
        ExitCode::SUCCESS
    );
    assert_eq!(
        super::member::get_value(&pc, "token")
            .expect("read in place")
            .as_slice(),
        b"the-vaults-token"
    );
    assert_eq!(client::registered_names(&pc).expect("names"), ["token"]);
    assert_eq!(
        run(Command::Ls {
            describe: false,
            json: true
        })
        .expect("ls"),
        ExitCode::SUCCESS
    );
    assert_eq!(run(Command::Status).expect("status"), ExitCode::SUCCESS);
    assert_eq!(
        run(Command::Sync { diagnostic: true }).expect("sync"),
        ExitCode::SUCCESS
    );

    let version = std::fs::read(&vault).expect("the vault");
    for write in [
        Command::Edit,
        Command::Set {
            name: "token".into(),
            hosts: Vec::new(),
            file: None,
            fill: Fill::default(),
        },
        Command::Unset {
            name: "token".into(),
            hosts: Vec::new(),
        },
        Command::Watch,
        Command::Serve(Vec::new()),
    ] {
        assert_eq!(run(write).expect("refused"), ExitCode::from(2));
    }
    assert_eq!(
        std::fs::read(&vault).expect("the vault"),
        version,
        "a refused verb changes nothing"
    );
}
