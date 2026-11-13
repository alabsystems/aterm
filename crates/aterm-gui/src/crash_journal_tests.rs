// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_spec::wait::MUST_HAPPEN;

pub(crate) use super::fixtures::layout;
use super::{
    BootClaim, DeathClass, Evidence, JournalHeader, JournalId, JournalOwner, Lane,
    MIN_WRITE_INTERVAL, Note, OwnerWords, PROBATION, PanesWithoutFolder, QuitLayout, Settled,
    TITLE_WRITE_INTERVAL, claim_at_boot, classify_death, decode, encode,
};
use crate::crash_signal::{MarkerOwner, markers};
use crate::restore::{self, RestoreManifest};

pub(crate) fn scratch(tag: &str) -> aterm_tempfile::TempDir {
    let root = crate::test_scratch::process_root("aterm-cj-");
    std::fs::create_dir_all(&root).expect("the journals' root");
    aterm_tempfile::Builder::new()
        .prefix(tag)
        .tempdir_in(&root)
        .expect("scratch dir")
}

pub(crate) fn cwds(manifest: &RestoreManifest) -> Vec<String> {
    manifest
        .windows
        .iter()
        .flat_map(|window| &window.restored_tabs)
        .filter_map(|tab| match &tab.root {
            restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(leaf),
            } => leaf.cwd.clone(),
            _ => None,
        })
        .collect()
}

pub(crate) fn reaped_pid() -> u32 {
    let mut child = aterm_uds::spawnfd::command("/usr/bin/true")
        .spawn()
        .expect("spawn /usr/bin/true");
    let pid = child.id();
    child.wait().expect("reap");
    pid
}

pub(crate) fn marker(logs: &Path, id: JournalId) -> PathBuf {
    let path = logs.join(markers::file_name(id.pid, id.nanos, MarkerOwner::Other));
    std::fs::write(&path, b"").unwrap();
    path
}

fn evidence(logs: &Path) -> Evidence<'_> {
    Evidence {
        log_dir: Some(logs),
        swept: &[],
    }
}

fn run(dir: &Path, logs: &Path, manifest: &RestoreManifest, probation: bool) -> JournalOwner {
    let id = JournalId::now(reaped_pid());
    marker(logs, id);
    let owner = JournalOwner::create(dir, id).expect("armed");
    owner
        .publish(manifest, &JournalHeader::now(id.pid, probation))
        .expect("published");
    owner
}

fn run_said(
    dir: &Path,
    logs: &Path,
    manifest: &RestoreManifest,
    programs: Option<Vec<super::LeafProgram>>,
) -> JournalOwner {
    let id = JournalId::now(reaped_pid());
    marker(logs, id);
    let owner = JournalOwner::create(dir, id).expect("armed");
    let mut header = JournalHeader::now(id.pid, false);
    header.programs = programs;
    owner.publish(manifest, &header).expect("published");
    owner
}

fn older_header(pid: u32) -> JournalHeader {
    JournalHeader {
        build: "0.98.0 (1790607299)".to_string(),
        stamps_quit: false,
        ..JournalHeader::now(pid, false)
    }
}

fn run_by_older_build(
    dir: &Path,
    logs: &Path,
    manifest: &RestoreManifest,
    at: Option<u64>,
) -> JournalOwner {
    let id = JournalId::now(reaped_pid());
    marker(logs, id);
    let owner = JournalOwner::create(dir, id).expect("armed");
    let mut header = older_header(id.pid);
    header.written_unix_ms = at.unwrap_or(header.written_unix_ms);
    owner.publish(manifest, &header).expect("published");
    owner
}

fn vim_in(window: u32, tab: u32) -> super::LeafProgram {
    super::LeafProgram {
        window,
        tab,
        program: "vim".to_string(),
        agent: false,
        again: false,
    }
}

#[test]
fn every_pane_of_the_restored_tree_is_counted_split_and_legacy() {
    use restore::{PaneLayout, RestoredSplitTree, SplitKind};
    let reopened = |manifest: RestoreManifest| super::Reopened {
        manifest,
        class: DeathClass::Killed,
        sources: vec![JournalId { pid: 9, nanos: 9 }],
        writers: vec![9],
        handed: Vec::new(),
        programs: None,
        second_chance: false,
    };
    let gone = |dir: &str| dir.starts_with("/gone");
    let none = |_: &str| false;

    let mut split = layout(&[("/here", "zsh"), ("/gone/a", "zsh"), ("/here/too", "zsh")]);
    let tabs = &mut split.windows[0].restored_tabs;
    let second = tabs.remove(1).root;
    let first = tabs[0].root.clone();
    tabs[0].root = RestoredSplitTree::Split {
        axis: SplitKind::Vertical,
        ratio: 0.5,
        first: Box::new(first),
        second: Box::new(second),
    };
    let split = reopened(split);
    assert_eq!(split.counts(), (1, 2));
    let counted = |gone| PanesWithoutFolder { gone, unasked: 0 };
    assert_eq!(
        split.panes_without_folder(gone),
        counted(1),
        "the split's second pane"
    );
    assert_eq!(split.panes_without_folder(none), counted(0));

    let leg = |cwd: &str| PaneLayout::Leaf {
        cwd: Some(cwd.to_string()),
        title: "zsh".to_string(),
        focused: false,
        local_id: None,
    };
    let mirror = vec![
        PaneLayout::Split {
            dir: SplitKind::Horizontal,
            ratio: 0.5,
            first: Box::new(leg("/here")),
            second: Box::new(leg("/gone/b")),
        },
        leg("/gone/c"),
    ];
    let mut legacy = layout(&[]);
    legacy.windows[0].tabs = mirror.clone();
    let legacy = reopened(legacy);
    assert_eq!(
        legacy.panes_without_folder(gone),
        counted(2),
        "the legacy mirror"
    );
    assert_eq!(legacy.panes_without_folder(none), counted(0));

    let mut both = layout(&[("/here", "zsh"), ("/here/too", "zsh")]);
    both.windows[0].tabs = mirror;
    assert_eq!(
        reopened(both).panes_without_folder(gone),
        counted(0),
        "canonical tabs win; the mirror is not counted beside them"
    );
}

#[test]
fn only_an_agent_with_a_conversation_is_resumed_and_only_when_relaunching() {
    let claude = |session: Option<&str>| restore::AgentRestore {
        claude_auth: None,
        word: String::new(),
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: session.map(str::to_string),
        cwd: "/work/b".into(),
        version: None,
        home: None,
        login: None,
        run: None,
        cause: None,
        tab: None,
    };
    let reopened = |agent: Option<restore::AgentRestore>| {
        let mut manifest = layout(&[("/work/a", "zsh"), ("/work/b", "claude")]);
        manifest.fill_agents(&|id| (id == 1).then(|| agent.clone()).flatten());
        super::Reopened {
            manifest,
            class: DeathClass::Killed,
            sources: Vec::new(),
            writers: Vec::new(),
            handed: Vec::new(),
            programs: None,
            second_chance: false,
        }
    };
    const CONVERSATION: &str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";
    let with = reopened(Some(claude(Some(CONVERSATION))));
    assert_eq!(with.resumed(true), vec![(0, 1)]);
    assert!(with.resumed(false).is_empty(), "the relaunch off");
    assert!(reopened(Some(claude(None))).resumed(true).is_empty());
    assert!(reopened(None).resumed(true).is_empty());
    use crate::message_reporters::journal_reopened_message;
    let agent_in = |tab: u32| super::LeafProgram {
        window: 0,
        tab,
        program: "claude".to_string(),
        agent: true,
        again: false,
    };
    let mut manifest = layout(&[("/work/a", "zsh"), ("/work/b", "claude")]);
    manifest.fill_agents(&|id| (id == 1).then(|| claude(Some(CONVERSATION))));
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    die(run_said(
        dir.path(),
        logs.path(),
        &manifest,
        Some(vec![agent_in(1)]),
    ));
    let only = claim_here(dir.path(), logs.path())
        .reopened
        .expect("reopened");
    let claude_only = |agent: &restore::AgentRestore| match agent.agent() {
        Some(aterm_agent::harness::upgrade::Agent::Claude) => true,
        Some(
            aterm_agent::harness::upgrade::Agent::Codex
            | aterm_agent::harness::upgrade::Agent::Agy
            | aterm_agent::harness::upgrade::Agent::Muse,
        )
        | None => false,
    };
    assert_eq!(only.relaunched_by(claude_only).resumed(true), vec![(0, 1)]);
    let switched_off = only.relaunched_by(|_| false);
    assert!(switched_off.resumed(true).is_empty());
    let row = journal_reopened_message(
        &switched_off,
        None,
        None,
        None,
        true,
        PanesWithoutFolder::NONE,
        0,
    );
    assert_eq!(row.title, "Tabs restored, claude lost");
    assert_eq!(only.resumed(true), vec![(0, 1)], "the layout itself kept");
    let codex = restore::AgentRestore {
        claude_auth: None,
        word: String::new(),
        pid: 4343,
        program: "/opt/codex/bin/codex".into(),
        argv: vec!["/opt/codex/bin/codex".into()],
        session: Some("019a0b1c-2d3e-7f40-8a5b-6c7d8e9f0a1b".into()),
        cwd: "/work/c".into(),
        version: None,
        run: Some(restore::RunRestore::Codex(restore::CodexRestore {
            home: "/h/.codex".into(),
            home_env: None,
            embedded: true,
            started_ms: 0,
            alab: Some(restore::AlabRestore {
                argv: vec![
                    "/opt/codex/bin/codex".into(),
                    "resume".into(),
                    "019a0b1c-2d3e-7f40-8a5b-6c7d8e9f0a1b".into(),
                ],
                session: "019a0b1c-2d3e-7f40-8a5b-6c7d8e9f0a1b".into(),
                codex_home: None,
                codex_auth_path: None,
                cwd: None,
            }),
            carried: None,
        })),
        ..claude(None)
    };
    let mut manifest = layout(&[
        ("/work/a", "zsh"),
        ("/work/b", "claude"),
        ("/work/c", "codex"),
    ]);
    manifest.fill_agents(&|id| match id {
        1 => Some(claude(Some(CONVERSATION))),
        2 => Some(codex.clone()),
        _ => None,
    });
    let codex_in = |tab: u32| super::LeafProgram {
        program: "codex".to_string(),
        ..agent_in(tab)
    };
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    die(run_said(
        dir.path(),
        logs.path(),
        &manifest,
        Some(vec![agent_in(1), codex_in(2)]),
    ));
    let mixed = claim_here(dir.path(), logs.path())
        .reopened
        .expect("reopened");
    assert_eq!(mixed.resumed(true), vec![(0, 1), (0, 2)], "both on");
    let mut vendor = codex.clone();
    if let Some(restore::RunRestore::Codex(run)) = vendor.run.as_mut() {
        run.alab = None;
    }
    assert!(codex.resumes() && !vendor.resumes());
    let told = mixed.relaunched_by(claude_only);
    assert_eq!(told.resumed(true), vec![(0, 1)]);
    let row = journal_reopened_message(&told, None, None, None, true, PanesWithoutFolder::NONE, 0);
    assert_eq!(row.title, "Tabs restored, codex lost");
}

#[test]
fn merged_journals_rebase_their_programs_and_one_silent_writer_is_unknown() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    die(run_said(
        dir.path(),
        logs.path(),
        &layout(&[("/a", "vim")]),
        Some(vec![vim_in(0, 0)]),
    ));
    die(run_said(
        dir.path(),
        logs.path(),
        &layout(&[("/b", "vim")]),
        Some(vec![vim_in(0, 0)]),
    ));
    let merged = claim_here(dir.path(), logs.path())
        .reopened
        .expect("reopened");
    assert_eq!(merged.programs, Some(vec![vim_in(0, 0), vim_in(1, 0)]));
    assert_eq!(merged.lost().map(|lost| lost.tabs), Some(2));

    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    die(run_said(
        dir.path(),
        logs.path(),
        &layout(&[("/a", "zsh")]),
        Some(Vec::new()),
    ));
    die(run_said(
        dir.path(),
        logs.path(),
        &layout(&[("/b", "zsh")]),
        None,
    ));
    let merged = claim_here(dir.path(), logs.path())
        .reopened
        .expect("reopened");
    assert_eq!(merged.programs, None);
    assert_eq!(merged.lost(), None);
}

#[test]
fn a_planted_program_name_reads_as_unnamed() {
    let mut header = JournalHeader::now(1, false);
    header.programs = Some(vec![super::LeafProgram {
        window: 0,
        tab: 0,
        program: "s-1234 `rm -rf`".to_string(),
        agent: false,
        again: false,
    }]);
    let text = encode(&layout(&[("/a", "zsh")]), &header).unwrap();
    let (read, _) = decode(&text).unwrap();
    assert_eq!(read.programs.unwrap()[0].program, "");
}

pub(crate) fn die(owner: JournalOwner) {
    drop(owner);
}

pub(crate) fn await_released(lock: &Path) {
    aterm_spec::wait::until_within(MUST_HAPPEN, "the dead lane's journal lock released", || {
        (markers::probe(lock) != markers::Owner::Live).then_some(())
    });
}

fn claim_here(dir: &Path, logs: &Path) -> BootClaim {
    claim_at_boot(
        dir,
        &evidence(logs),
        std::process::id(),
        QuitLayout::default(),
    )
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn names_round_trip_and_nothing_else_parses() {
    let id = JournalId {
        pid: 4242,
        nanos: 1_790_309_523_499_676_000,
    };
    assert_eq!(id.file_name(), "journal-4242-1790309523499676000.toml");
    assert_eq!(JournalId::parse(&id.file_name()), Some(id));
    assert_eq!(JournalId::parse_lock(&id.lock_name()), Some(id));
    for stranger in [
        "journal-4242-7.lock",
        "journal-x-7.toml",
        "journal-4242-.toml",
        ".journal-4242-7.toml",
        "session.toml",
        "journal-4242-7.toml.seen",
        "journal-4242-7.unreadable",
    ] {
        assert_eq!(JournalId::parse(stranger), None, "{stranger}");
    }
}

#[test]
fn a_journal_is_a_session_toml_with_a_header() {
    let manifest = layout(&[("/work/a", "vim"), ("/work/b", "zsh")]);
    let marked = super::LeafProgram {
        window: 0,
        tab: 1,
        program: "claude".to_string(),
        agent: true,
        again: true,
    };
    for programs in [None, Some(Vec::new()), Some(vec![vim_in(0, 0), marked])] {
        let mut header = JournalHeader::now(7, true);
        header.programs = programs;
        let text = encode(&manifest, &header).unwrap();
        assert_eq!(RestoreManifest::from_toml(&text), Some(manifest.clone()));
        assert_eq!(decode(&text).unwrap(), (header, manifest.clone()));
    }
    assert!(
        decode(&RestoreManifest::new(Vec::new()).to_toml().unwrap()).is_err(),
        "a plain session.toml has no header and is no journal"
    );
}

/// The image and its entry are only ordered, and the boot claim's folder takes one plain
/// `fsync(2)`, aterm-durable's (`primitive::fsync`), which astream-sys counts `plain` in
/// `flush_tally`: the seam counts exactly this thread's one, and the process-wide tally
/// moves by at least it (this binary's other tests flush meanwhile, so only a bound reads
/// there; aterm-durable's `tests/flush_tally.rs` holds the seam's count to the tally's
/// exactly). A folder flush that called the OS itself, as this one did through
/// `libc::fsync`, moves neither.
#[test]
fn a_journal_image_is_only_ordered_and_the_boot_claim_untouched() {
    use aterm_durable::{Flush, seam::Recording};
    let (dir, logs) = (scratch("cj-flush"), scratch("cj-flush-logs"));
    let id = JournalId::now(reaped_pid());
    marker(logs.path(), id);
    let owner = JournalOwner::create(dir.path(), id).expect("armed");
    let recording = Recording::start();
    owner
        .publish(
            &layout(&[("/work/a", "zsh")]),
            &JournalHeader::now(id.pid, false),
        )
        .expect("published");
    assert_eq!(
        recording.take(),
        [Flush::Ordered, Flush::Ordered],
        "the image and its entry"
    );
    assert_eq!(
        recording.take_plain(),
        0,
        "a published image takes no plain flush"
    );
    die(owner);
    let before = astream_sys::flush_tally();
    let claim = claim_here(dir.path(), logs.path());
    let after = astream_sys::flush_tally();
    assert!(claim.reopened.is_some(), "the dead run's desk reopens");
    assert_eq!(
        recording.take(),
        [],
        "the claim's entries take the plain fsync(2), not an aterm-durable arm"
    );
    assert_eq!(
        recording.take_plain(),
        1,
        "the claim's folder: one plain fsync(2), aterm-durable's"
    );
    assert!(
        after.plain > before.plain,
        "astream-sys counted the claim's folder flush: {before:?} -> {after:?}"
    );
}

#[test]
fn a_killed_runs_journal_reopens_once() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let manifest = layout(&[("/work/a", "vim"), ("/work/b", "zsh")]);
    let owner = run(dir.path(), logs.path(), &manifest, false);
    let id = owner.id();
    die(owner); // SIGKILL: no retire, the lock released by the kernel
    let claim = claim_here(dir.path(), logs.path());
    let reopened = claim.reopened.expect("reopened");
    assert_eq!(reopened.class, DeathClass::Killed);
    assert_eq!(reopened.sources, vec![id]);
    assert_eq!(cwds(&reopened.manifest), vec!["/work/a", "/work/b"]);
    assert_eq!(reopened.counts(), (1, 2));
    assert!(claim.notes.is_empty());
    assert!(
        names(dir.path()).is_empty(),
        "the journal, its lock and the claim are gone: {:?}",
        names(dir.path())
    );
    assert_eq!(
        claim_at_boot(dir.path(), &evidence(logs.path()), 1, QuitLayout::default()),
        BootClaim::default()
    );
}

#[test]
fn a_dead_owners_lock_is_free_while_a_child_forked_under_it_has_yet_to_exec() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let owner = run(dir.path(), logs.path(), &layout(&[("/a", "zsh")]), false);
    let id = owner.id();
    let child = crate::parked_fork::ParkedChild::fork();
    die(owner);
    let claim = claim_here(dir.path(), logs.path());
    assert!(
        child.still_parked(),
        "the claim ran while the child had yet to exec"
    );
    child.release();
    assert_eq!(claim.taken, vec![id], "the dead run's journal is taken");
}

#[test]
fn a_clean_end_is_never_reopened() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let owner = run(dir.path(), logs.path(), &layout(&[("/q", "zsh")]), false);
    owner.retire().unwrap();
    assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
    assert_eq!(claim_here(dir.path(), logs.path()), BootClaim::default());

    let owner = run(dir.path(), logs.path(), &layout(&[("/h", "zsh")]), false);
    let id = owner.id();
    for entry in std::fs::read_dir(logs.path()).unwrap().flatten() {
        std::fs::remove_file(entry.path()).unwrap();
    }
    die(owner);
    let claim = claim_here(dir.path(), logs.path());
    assert_eq!(claim.reopened, None);
    assert_eq!(claim.taken, vec![id]);
    assert_eq!(claim.set_aside.len(), 1, "{:?}", claim.set_aside);
    assert!(names(dir.path()).is_empty());
}

#[test]
fn a_second_instance_never_steals_a_journal() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let live = run(dir.path(), logs.path(), &layout(&[("/live", "zsh")]), false);
    for _ in 0..2 {
        let claim = claim_here(dir.path(), logs.path());
        assert!(claim.taken.is_empty(), "{claim:?}");
    }
    assert!(live.path().exists());
    die(live);
    let first = claim_here(dir.path(), logs.path());
    let second = claim_here(dir.path(), logs.path());
    assert_eq!(first.taken.len(), 1, "control: a dead owner's is taken");
    assert!(first.reopened.is_some());
    assert!(second.taken.is_empty() && second.reopened.is_none());

    die(run(
        dir.path(),
        logs.path(),
        &layout(&[("/race", "zsh")]),
        false,
    ));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let racers: Vec<_> = (0..2)
        .map(|_| {
            let (dir, logs) = (dir.path().to_path_buf(), logs.path().to_path_buf());
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                claim_here(&dir, &logs).taken.len()
            })
        })
        .collect();
    let taken: usize = racers.into_iter().map(|r| r.join().unwrap()).sum();
    assert_eq!(taken, 1, "one image, one taker");
}

#[test]
fn another_builds_journal_reopens() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let id = JournalId::now(reaped_pid());
    marker(logs.path(), id);
    die(JournalOwner::create(dir.path(), id).unwrap());
    let older = format!(
        "schema = 1\n\n[[windows]]\nrows = 30\ncols = 100\nactive_tab = 0\n\n\
         [[windows.tabs]]\nkind = \"leaf\"\ncwd = \"/from/0.80\"\ntitle = \"zsh\"\n\
         focused = true\n\n\
         [journal]\npid = {}\nbuild = \"0.80.0 (1234)\"\nwritten_unix_ms = 1\n",
        id.pid
    );
    std::fs::write(dir.path().join(id.file_name()), older).unwrap();
    let claim = claim_here(dir.path(), logs.path());
    let reopened = claim.reopened.expect("an older build's journal reopens");
    assert_eq!(reopened.manifest.windows[0].rows, 30);
    assert_eq!(
        reopened.manifest.first_leaf_cwd().as_deref(),
        Some("/from/0.80"),
        "{:?}",
        reopened.manifest
    );
}

#[test]
fn an_unreadable_journal_is_said_and_never_followed() {
    use std::os::unix::fs::PermissionsExt as _;
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let owner = run(dir.path(), logs.path(), &layout(&[("/torn", "zsh")]), false);
    let (id, path) = (owner.id(), owner.path());
    die(owner);
    let torn: &[u8] = b"schema = 2\n[[windows]\n";
    std::fs::write(&path, torn).unwrap();
    let older: Vec<String> = (1..=super::unix::KEEP_UNREADABLE as u128)
        .map(|nanos| JournalId { pid: 1, nanos }.unreadable_name())
        .collect();
    for name in &older {
        std::fs::write(dir.path().join(name), torn).unwrap();
    }
    let claim = claim_here(dir.path(), logs.path());
    assert_eq!(claim.reopened, None);
    let kept = match &claim.notes[..] {
        [
            Note::Unreadable {
                id: noted,
                class: DeathClass::Killed,
                path: Some(path),
                ..
            },
        ] if *noted == id => path.clone(),
        notes => panic!("{notes:?}"),
    };
    assert_eq!(
        std::fs::read(&kept).ok().as_deref(),
        Some(torn),
        "the row names the image, kept aside: {}",
        kept.display()
    );
    assert_eq!(
        std::fs::metadata(&kept).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let mut aside = older[1..].to_vec();
    aside.push(id.unreadable_name());
    aside.sort();
    let listed = |dir: &Path| -> Vec<String> {
        names(dir)
            .into_iter()
            .filter(|name| name.ends_with(".unreadable"))
            .collect()
    };
    assert_eq!(listed(dir.path()), aside);
    let again = claim_here(dir.path(), logs.path());
    assert!(
        again.taken.is_empty() && again.notes.is_empty(),
        "{again:?}"
    );
    assert_eq!(listed(dir.path()), aside, "no claim takes or sweeps one");

    let secret = scratch("cj-secret");
    let target = secret.path().join("elsewhere.toml");
    std::fs::write(
        &target,
        encode(
            &layout(&[("/secret", "zsh")]),
            &JournalHeader::now(1, false),
        )
        .unwrap(),
    )
    .unwrap();
    let owner = run(dir.path(), logs.path(), &layout(&[("/x", "zsh")]), false);
    let path = owner.path();
    die(owner);
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let claim = claim_here(dir.path(), logs.path());
    assert_eq!(claim.reopened, None, "a link is never followed");
    assert!(matches!(
        &claim.notes[..],
        [Note::Unreadable { path: None, .. }]
    ));
    assert!(target.exists(), "the link's target is untouched");
    assert!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_type().unwrap().is_symlink()),
        "a link is never kept: {:?}",
        names(dir.path())
    );
    let theirs = secret.path().join("notes.txt");
    std::fs::write(&theirs, b"token = \"their-private-line\n").unwrap();
    std::fs::set_permissions(&theirs, std::fs::Permissions::from_mode(0o644)).unwrap();
    let owner = run(dir.path(), logs.path(), &layout(&[("/y", "zsh")]), false);
    let path = owner.path();
    die(owner);
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&theirs, &path).unwrap();
    let claim = claim_here(dir.path(), logs.path());
    let [
        Note::Unreadable {
            path: None, error, ..
        },
    ] = &claim.notes[..]
    else {
        panic!("{:?}", claim.notes);
    };
    assert!(
        !error.contains("their-private-line"),
        "the row quotes the linked file: {error}"
    );
    assert_eq!(
        std::fs::metadata(&theirs).unwrap().permissions().mode() & 0o777,
        0o644,
        "the other name's file is untouched"
    );
    assert_eq!(listed(dir.path()), aside, "a hard link is never kept");
}

#[test]
fn a_relapse_inside_probation_is_skipped_and_said() {
    use super::LeafProgram;
    let python = || {
        Some(vec![LeafProgram {
            window: 0,
            tab: 1,
            program: "Python".to_string(),
            agent: false,
            again: false,
        }])
    };
    let relapse = |second_chance: bool, banner: &[u8]| {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let id = JournalId::now(reaped_pid());
        std::fs::write(marker(logs.path(), id), banner).unwrap();
        let owner = JournalOwner::create(dir.path(), id).expect("armed");
        let mut header = JournalHeader::now(id.pid, true).with_second_chance(second_chance);
        header.programs = python();
        owner
            .publish(&layout(&[("/a", "zsh"), ("/b", "python3")]), &header)
            .expect("published");
        die(owner);
        let claim = claim_here(dir.path(), logs.path());
        (claim, dir, logs)
    };

    let (claim, _dir, _logs) = relapse(false, b"");
    let reopened = claim
        .reopened
        .expect("a kill is no evidence the layout stops aterm");
    assert!(reopened.second_chance, "{reopened:?}");
    assert!(claim.notes.is_empty(), "{:?}", claim.notes);

    let (claim, _dir, _logs) = relapse(true, b"");
    assert_eq!(claim.reopened, None);
    match &claim.notes[..] {
        [
            Note::Relapsed {
                class,
                windows,
                tabs,
                lost: Some(lost),
                ..
            },
        ] => {
            assert_eq!(*class, DeathClass::Killed);
            assert_eq!((*windows, *tabs), (1, 2));
            assert_eq!(lost.named, ["Python"]);
        }
        other => panic!("{other:?}"),
    }

    let (claim, _dir, _logs) = relapse(false, b"aterm: fatal signal 11");
    assert_eq!(claim.reopened, None);
    assert!(
        matches!(
            &claim.notes[..],
            [Note::Relapsed {
                class: DeathClass::Signal,
                ..
            }]
        ),
        "{:?}",
        claim.notes
    );

    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let owner = run(
        dir.path(),
        logs.path(),
        &layout(&[("/settled", "zsh")]),
        true,
    );
    owner
        .publish(
            &layout(&[("/settled", "zsh")]),
            &JournalHeader::now(owner.id().pid, false),
        )
        .unwrap();
    die(owner);
    let settled = claim_here(dir.path(), logs.path())
        .reopened
        .expect("control");
    assert!(!settled.second_chance);
}

#[test]
fn the_header_marks_are_additive() {
    let text = encode(
        &layout(&[("/a", "zsh")]),
        &JournalHeader::now(7, true).with_second_chance(true),
    )
    .unwrap();
    assert!(text.contains("second_chance = true"), "{text}");
    let (header, _) = decode(&text).unwrap();
    assert!(header.probation && header.second_chance);
    let old = text.replace("second_chance = true\n", "");
    assert!(!old.contains("second_chance"));
    let (header, _) = decode(&old).unwrap();
    assert!(header.probation && !header.second_chance);
    let plain = encode(&layout(&[("/a", "zsh")]), &JournalHeader::now(7, false)).unwrap();
    assert!(!plain.contains("second_chance"), "{plain}");

    assert!(plain.contains("stamps_quit = true"), "{plain}");
    assert!(decode(&plain).expect("decoded").0.stamps_quit);
    let theirs = encode(&layout(&[("/a", "zsh")]), &older_header(7)).expect("encoded");
    assert!(!theirs.contains("stamps_quit"), "{theirs}");
    assert!(!decode(&theirs).expect("decoded").0.stamps_quit);
}

#[test]
fn a_quit_layout_does_not_set_a_crashed_desk_aside() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let desk: Vec<RestoreManifest> = (0..5)
        .map(|i| layout(&[(format!("/w{i}").as_str(), "zsh")]))
        .collect();
    let desk = RestoreManifest::new(desk.into_iter().flat_map(|m| m.windows).collect());
    let crashed = run(dir.path(), logs.path(), &desk, false);
    let crashed_id = crashed.id();
    die(crashed);
    let claim = claim_at_boot(
        dir.path(),
        &evidence(logs.path()),
        1,
        QuitLayout::of(Some(&layout(&[("/quit", "zsh")])), None, None),
    );
    let reopened = claim.reopened.expect("the crashed desk comes back");
    assert_eq!(reopened.sources, vec![crashed_id]);
    assert_eq!(
        cwds(&reopened.manifest),
        vec!["/w0", "/w1", "/w2", "/w3", "/w4"]
    );
    assert!(names(dir.path()).is_empty());
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    die(run(
        dir.path(),
        logs.path(),
        &layout(&[("/a", "zsh")]),
        false,
    ));
    let claim = claim_at_boot(
        dir.path(),
        &evidence(logs.path()),
        1,
        QuitLayout {
            windows: crate::restore::MAX_WINDOWS,
            run: None,
            also: Vec::new(),
            shape: vec![2; crate::restore::MAX_WINDOWS],
            written_unix_ms: None,
        },
    );
    assert_eq!(claim.reopened, None);
    assert_eq!(claim.set_aside.len(), 1, "{:?}", claim.set_aside);
}

#[test]
fn every_crashed_runs_windows_come_back_newest_first() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let older = run(
        dir.path(),
        logs.path(),
        &layout(&[("/older", "zsh")]),
        false,
    );
    let newer = run(
        dir.path(),
        logs.path(),
        &layout(&[("/newer", "zsh")]),
        false,
    );
    let (older_id, newer_id) = (older.id(), newer.id());
    die(older);
    die(newer);
    let reopened = claim_here(dir.path(), logs.path()).reopened.unwrap();
    assert_eq!(reopened.sources, vec![newer_id, older_id]);
    assert_eq!(cwds(&reopened.manifest), vec!["/newer", "/older"]);
    assert_ne!(newer_id.pid, older_id.pid);
    assert_eq!(reopened.writers, vec![newer_id.pid, older_id.pid]);
}

#[test]
fn the_death_class_is_read_from_the_runs_own_evidence() {
    let logs = scratch("cj-logs");
    let id = JournalId::now(reaped_pid());
    let none = Evidence {
        log_dir: Some(logs.path()),
        swept: &[],
    };
    assert_eq!(
        classify_death(&none, id, None),
        None,
        "a clean exit leaves nothing"
    );

    let later = logs
        .path()
        .join(markers::file_name(id.pid, id.nanos + 1, MarkerOwner::App));
    std::fs::write(&later, b"").unwrap();
    assert_eq!(
        classify_death(&none, id, None),
        None,
        "a later process's marker"
    );
    std::fs::remove_file(&later).unwrap();

    let own = marker(logs.path(), id);
    assert_eq!(classify_death(&none, id, None), Some(DeathClass::Killed));
    std::fs::write(&own, b"aterm: fatal signal 11").unwrap();
    assert_eq!(classify_death(&none, id, None), Some(DeathClass::Signal));
    std::fs::remove_file(&own).unwrap();

    file_clock_catches_up(logs.path());
    let report = logs.path().join(format!("crash-{}.log", id.pid));
    std::fs::write(&report, b"aterm-gui crashed").unwrap();
    assert_eq!(classify_death(&none, id, None), Some(DeathClass::Panic));
    let before = JournalId {
        pid: id.pid,
        nanos: super::unix_nanos(std::time::SystemTime::now()) + 60_000_000_000,
    };
    assert_eq!(
        classify_death(&none, before, None),
        None,
        "a report older than the journal is not its run's"
    );
    std::fs::remove_file(&report).unwrap();

    let swept = [markers::MarkerName {
        pid: id.pid,
        nanos: id.nanos - 5,
        owner: MarkerOwner::Other,
    }];
    let with_sweep = Evidence {
        log_dir: Some(logs.path()),
        swept: &swept,
    };
    assert_eq!(
        classify_death(&with_sweep, id, None),
        Some(DeathClass::Killed)
    );
}

fn file_clock_catches_up(dir: &Path) {
    let now = std::time::SystemTime::now();
    let probe = dir.join(".file-clock-probe");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        std::fs::write(&probe, b"").unwrap();
        if std::fs::metadata(&probe).unwrap().modified().unwrap() >= now {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the file clock did not reach the wall clock in a minute"
        );
        aterm_uds::deadline::poll_yield();
    }
    std::fs::remove_file(&probe).unwrap();
}

fn panic_report(logs: &Path, pid: u32, main_thread: bool) -> PathBuf {
    file_clock_catches_up(logs);
    let path = logs.join(format!("crash-{pid}.log"));
    let mark = if main_thread {
        crate::logging::MAIN_THREAD_PANIC
    } else {
        ""
    };
    std::fs::write(
        &path,
        format!("aterm-gui 0.1.0 crashed at unix 1.000{mark}\npanicked at x\n"),
    )
    .unwrap();
    path
}

#[test]
fn a_main_thread_panic_whose_report_another_launch_read_first_still_reopens() {
    for main_thread in [true, false] {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let id = JournalId::now(reaped_pid());
        let owner = JournalOwner::create(dir.path(), id).expect("armed");
        owner
            .publish(
                &layout(&[("/a", "zsh"), ("/b", "claude")]),
                &JournalHeader::now(id.pid, false),
            )
            .expect("published");
        die(owner);
        panic_report(logs.path(), id.pid, main_thread);
        assert!(
            crate::logging::take_crash_evidence_in(logs.path(), None).is_some(),
            "another launch reads the report first"
        );
        assert!(
            logs.path()
                .join(format!("crash-{}.log.seen", id.pid))
                .exists(),
            "PRECONDITION: consumed"
        );
        let claim = claim_here(dir.path(), logs.path());
        if main_thread {
            let reopened = claim.reopened.expect("the crashed desk comes back");
            assert_eq!(reopened.class, DeathClass::Panic);
            assert_eq!(cwds(&reopened.manifest), vec!["/a", "/b"]);
        } else {
            assert_eq!(claim.reopened, None, "a worker's panic says nothing");
            assert_eq!(claim.set_aside.len(), 1, "{:?}", claim.set_aside);
        }
    }
}

#[test]
fn a_handed_off_journal_is_not_reopened_by_a_later_run_of_the_same_pid() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let pid = reaped_pid();
    let stale = JournalId::now(pid);
    let owner = JournalOwner::create(dir.path(), stale).expect("armed");
    owner
        .publish(
            &layout(&[("/stale", "zsh")]),
            &JournalHeader::now(pid, false),
        )
        .expect("published");
    die(owner); // the hand-off: clean, the marker removed, the image left
    let mut live = JournalId::now(pid);
    while live.nanos <= stale.nanos {
        live = JournalId::now(pid);
    }
    let owner = JournalOwner::create(dir.path(), live).expect("armed");
    owner
        .publish(
            &layout(&[("/live", "zsh")]),
            &JournalHeader::now(pid, false),
        )
        .expect("published");
    die(owner);
    panic_report(logs.path(), pid, true);
    let claim = claim_here(dir.path(), logs.path());
    let reopened = claim.reopened.expect("the crashed run comes back");
    assert_eq!(reopened.class, DeathClass::Panic);
    assert_eq!(reopened.sources, vec![live]);
    assert_eq!(cwds(&reopened.manifest), vec!["/live"]);
    assert!(
        claim.set_aside.iter().any(|(id, _)| *id == stale),
        "{:?}",
        claim.set_aside
    );
    let logs = scratch("cj-logs");
    let id = JournalId::now(reaped_pid());
    let later = logs
        .path()
        .join(markers::file_name(id.pid, id.nanos + 1, MarkerOwner::App));
    std::fs::write(&later, b"").unwrap();
    panic_report(logs.path(), id.pid, true);
    assert_eq!(
        classify_death(&evidence(logs.path()), id, None),
        None,
        "a report written after a later run of the pid started is that run's"
    );
}

#[test]
fn a_later_run_swept_at_install_bounds_a_pid_only_report() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let stale = JournalId::now(reaped_pid());
    let owner = JournalOwner::create(dir.path(), stale).expect("armed");
    owner
        .publish(
            &layout(&[("/stale", "zsh")]),
            &JournalHeader::now(stale.pid, false),
        )
        .expect("published");
    die(owner); // the hand-off: clean, the marker removed, the image left
    panic_report(logs.path(), stale.pid, false);
    let swept = [markers::MarkerName {
        pid: stale.pid,
        nanos: stale.nanos + 1,
        owner: MarkerOwner::Other,
    }];
    let claim = claim_at_boot(
        dir.path(),
        &Evidence {
            log_dir: Some(logs.path()),
            swept: &swept,
        },
        std::process::id(),
        QuitLayout::default(),
    );
    assert_eq!(claim.reopened, None, "the later run's report is its own");
    assert_eq!(
        claim
            .set_aside
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        vec![stale]
    );
    assert_eq!(
        classify_death(&evidence(logs.path()), stale, None),
        Some(DeathClass::Panic)
    );
}

#[test]
fn a_report_that_names_its_run_is_that_runs_only() {
    for (named, reopens) in [(false, false), (true, true)] {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let stale = JournalId::now(reaped_pid());
        let owner = JournalOwner::create(dir.path(), stale).expect("armed");
        owner
            .publish(
                &layout(&[("/stale", "zsh")]),
                &JournalHeader::now(stale.pid, false),
            )
            .expect("published");
        die(owner);
        let run = if named { stale.nanos } else { stale.nanos + 1 };
        crate::logging::write_crash_report(
            &logs.path().join(format!("crash-{}.log", stale.pid)),
            true,
            Some(run),
            &"panicked at x",
            &"bt",
        )
        .expect("the report");
        assert!(
            crate::logging::take_crash_evidence_in(logs.path(), None).is_some(),
            "another launch reads the report first"
        );
        let claim = claim_here(dir.path(), logs.path());
        assert_eq!(
            claim.reopened.as_ref().map(|reopened| reopened.class),
            reopens.then_some(DeathClass::Panic),
            "a report of run {run} for the journal of run {}",
            stale.nanos
        );
    }
}

#[test]
fn a_successor_crash_before_its_first_image_reopens_the_predecessors_layout() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let parent = JournalId::now(reaped_pid());
    let owner = JournalOwner::create(dir.path(), parent).expect("armed");
    owner
        .publish(
            &layout(&[("/a", "zsh"), ("/b", "claude")]),
            &JournalHeader::now(parent.pid, false),
        )
        .expect("published");
    die(owner); // the hand-off: `clean_exit_now` removed its marker
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
    lane.set_predecessor(parent.pid, None);
    let own = lane.journal_id();
    marker(logs.path(), own); // the successor's run, armed at its start
    assert_eq!(
        lane.adopt_predecessor(true),
        Some(parent),
        "taken at Commit"
    );
    assert_eq!(lane.adopt_predecessor(true), None, "once");
    assert_eq!(
        names(dir.path()),
        vec![own.lock_name(), own.file_name()],
        "the parent's journal is this run's now, and its dead lock is gone"
    );
    drop(lane);
    await_released(&dir.path().join(own.lock_name()));
    let claim = claim_here(dir.path(), logs.path());
    let reopened = claim.reopened.expect("the desk comes back");
    assert_eq!(reopened.class, DeathClass::Killed);
    assert_eq!(reopened.sources, vec![own]);
    assert_eq!(cwds(&reopened.manifest), vec!["/a", "/b"]);
}

#[test]
fn a_committed_successor_that_dies_before_its_commit_wake_still_reopens_the_desk() {
    use std::sync::atomic::AtomicBool;
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let parent = JournalId::now(reaped_pid());
    let owner = JournalOwner::create(dir.path(), parent).expect("armed");
    owner
        .publish(
            &layout(&[("/a", "zsh"), ("/b", "claude")]),
            &JournalHeader::now(parent.pid, false),
        )
        .expect("published");
    die(owner); // the hand-off: `clean_exit_now` removed its marker
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
    lane.set_predecessor(parent.pid, Some(parent.nanos));
    let own = lane.journal_id();
    marker(logs.path(), own); // the successor's run, armed at its start
    let journal = lane.take_over_at_commit(true);
    assert!(
        journal.is_some(),
        "a successor's lane prepares its take-over"
    );
    let gate = crate::spawn::DeferredReaderGate::closed();
    let reader = {
        let gate = gate.clone();
        let image = dir.path().join(own.file_name());
        std::thread::spawn(move || {
            assert!(gate.wait_until_released(&AtomicBool::new(false)));
            image.exists()
        })
    };
    crate::release_committed_readers(journal, &gate);
    assert!(
        reader.join().expect("the reader"),
        "a released reader runs only once the journal is this run's"
    );
    drop(lane);
    await_released(&dir.path().join(own.lock_name()));
    let claim = claim_here(dir.path(), logs.path());
    let reopened = claim.reopened.expect("the desk comes back");
    assert_eq!(reopened.class, DeathClass::Killed);
    assert_eq!(reopened.sources, vec![own]);
    assert_eq!(cwds(&reopened.manifest), vec!["/a", "/b"]);
    assert!(claim.set_aside.is_empty(), "{:?}", claim.set_aside);
}

#[test]
fn the_commit_wake_installs_the_take_over_its_waiter_ran() {
    for waiter in [true, false] {
        let dir = scratch("cj-dir");
        let parent = JournalId::now(reaped_pid());
        let owner = JournalOwner::create(dir.path(), parent).expect("armed");
        owner
            .publish(
                &layout(&[("/a", "zsh")]),
                &JournalHeader::now(parent.pid, false),
            )
            .expect("published");
        die(owner);
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        lane.set_predecessor(parent.pid, Some(parent.nanos));
        let own = lane.journal_id();
        if waiter {
            lane.take_over_at_commit(true).expect("prepared").run(|| {});
            assert_eq!(
                names(dir.path()),
                vec![own.lock_name(), own.file_name()],
                "the waiter took it over"
            );
        }
        assert_eq!(
            lane.adopt_predecessor(true),
            Some(parent),
            "waiter={waiter}"
        );
        assert_eq!(lane.adopt_predecessor(true), None, "once");
        assert_eq!(
            names(dir.path()),
            vec![own.lock_name(), own.file_name()],
            "waiter={waiter}"
        );
        assert_eq!(
            markers::probe(&dir.path().join(own.lock_name())),
            markers::Owner::Live,
            "waiter={waiter}: the writer holds this run's journal"
        );
        lane.retire(MUST_HAPPEN).expect("retired");
        assert!(names(dir.path()).is_empty(), "waiter={waiter}");
    }
}

fn a_parents_journal(dir: &Path, desk: &RestoreManifest) -> (JournalId, JournalOwner) {
    let parent = JournalId::now(reaped_pid());
    let owner = JournalOwner::create(dir, parent).expect("armed");
    owner
        .publish(desk, &JournalHeader::now(parent.pid, false))
        .expect("published");
    (parent, owner)
}

#[test]
fn the_waiter_releases_the_readers_once_the_rename_stands() {
    let dir = scratch("cj-dir");
    let (parent, owner) = a_parents_journal(dir.path(), &layout(&[("/a", "zsh")]));
    die(owner);
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
    lane.set_predecessor(parent.pid, Some(parent.nanos));
    let own = lane.journal_id();
    let journal = lane.take_over_at_commit(true).expect("prepared");
    let slot = journal.slot.clone();
    let held = slot
        .taken_over
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let gate = crate::spawn::DeferredReaderGate::closed();
    let waiter = {
        let gate = gate.clone();
        std::thread::spawn(move || crate::release_committed_readers(Some(journal), &gate))
    };
    aterm_spec::wait::until_within(
        MUST_HAPPEN,
        "the readers released while the take-over's result waits",
        || gate.is_released().then_some(()),
    );
    assert!(
        dir.path().join(own.file_name()).exists(),
        "the rename came first"
    );
    drop(held);
    waiter.join().expect("the waiter");
    assert_eq!(lane.adopt_predecessor(true), Some(parent));
    lane.retire(MUST_HAPPEN).expect("retired");
    assert!(names(dir.path()).is_empty());

    let dir = scratch("cj-dir");
    let (parent, owner) = a_parents_journal(dir.path(), &layout(&[("/a", "zsh")]));
    die(owner);
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
    lane.set_predecessor(parent.pid, Some(parent.nanos));
    let own = lane.journal_id();
    let mut seen = None;
    lane.take_over_at_commit(true)
        .expect("prepared")
        .run(|| seen = Some(names(dir.path())));
    let mut expected = vec![parent.lock_name(), own.lock_name(), own.file_name()];
    expected.sort();
    assert_eq!(seen, Some(expected), "renamed, its parent's lock not swept");
    assert_eq!(names(dir.path()), vec![own.lock_name(), own.file_name()]);
}

#[test]
fn the_parents_lock_goes_once_its_exit_lets_it_go() {
    for first_image in [true, false] {
        let dir = scratch("cj-dir");
        let (parent, owner) = a_parents_journal(dir.path(), &layout(&[("/a", "zsh")]));
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        lane.set_predecessor(parent.pid, Some(parent.nanos));
        let own = lane.journal_id();
        lane.take_over_at_commit(true).expect("prepared").run(|| {});
        let mut held = vec![parent.lock_name(), own.lock_name(), own.file_name()];
        held.sort();
        assert_eq!(names(dir.path()), held, "a lock still held stays");
        die(owner); // its `_exit` completes
        assert_eq!(lane.adopt_predecessor(true), Some(parent));
        if first_image {
            assert!(lane.offer(layout(&[("/b", "zsh")]), Vec::new(), Instant::now()));
            let settled = vec![own.lock_name(), own.file_name()];
            aterm_spec::wait::until_within(
                MUST_HAPPEN,
                "the writer clears the parent's lock",
                || (names(dir.path()) == settled).then_some(()),
            );
        }
        lane.retire(MUST_HAPPEN).expect("retired");
        assert!(
            names(dir.path()).is_empty(),
            "first_image={first_image}: {:?}",
            names(dir.path())
        );
    }
}

#[test]
fn the_taken_over_image_is_named_until_the_writer_replaces_it() {
    let dir = scratch("cj-dir");
    let (parent, owner) = a_parents_journal(dir.path(), &layout(&[("/a", "zsh")]));
    die(owner);
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
    assert_eq!(
        lane.inherited_image_parent(),
        None,
        "nothing taken over yet"
    );
    lane.set_predecessor(parent.pid, Some(parent.nanos));
    assert_eq!(lane.adopt_predecessor(true), Some(parent));
    assert_eq!(lane.inherited_image_parent(), Some(parent.pid));
    assert!(lane.offer(layout(&[("/b", "zsh")]), Vec::new(), Instant::now()));
    aterm_spec::wait::until_within(
        MUST_HAPPEN,
        "the writer replaces the parent's image",
        || lane.inherited_image_parent().is_none().then_some(()),
    );
    lane.retire(MUST_HAPPEN).expect("retired");

    let fresh = scratch("cj-fresh");
    let fresh = Lane::new(Some(fresh.path().to_path_buf()), reaped_pid());
    assert_eq!(fresh.inherited_image_parent(), None, "no successor");
}

#[test]
fn a_successors_own_fact_about_a_tab_ends_what_it_carries_for_good() {
    use super::{HandedSession, Runs};
    let record = |session: &str| restore::AgentRestore {
        claude_auth: None,
        word: String::new(),
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some(session.into()),
        cwd: "/b".into(),
        version: Some("2.1.283".into()),
        home: None,
        login: None,
        run: None,
        cause: None,
        tab: None,
    };
    let successor = || {
        let dir = scratch("cj-dir");
        let mut desk = layout(&[("/a", "zsh"), ("/b", "claude")]);
        desk.fill_agents(&|id| (id == 1).then(|| record("C-parent")));
        let (parent, owner) = a_parents_journal(dir.path(), &desk);
        die(owner);
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        lane.set_predecessor(parent.pid, Some(parent.nanos));
        assert_eq!(lane.adopt_predecessor(true), Some(parent));
        (dir, lane)
    };
    let capture = |lane: &mut Lane, host: Option<&str>, runs: Runs, busy: bool| {
        let mut manifest = layout(&[("/own-b", "claude"), ("/own-a", "zsh")]);
        if busy {
            let restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(leaf),
            } = &mut manifest.windows[0].restored_tabs[0].root
            else {
                unreachable!("a terminal leaf")
            };
            leaf.cwd = None;
            leaf.title.clear();
        }
        manifest.fill_agents(&|id| (id == 0).then_some(host).flatten().map(record));
        lane.carry_forward(&mut manifest, &|id| {
            (id <= 1).then_some(HandedSession {
                was: 1 - id,
                runs: if id == 0 { runs } else { Runs::Other },
                keyed: None,
            })
        });
        let restore::RestoredSplitTree::Leaf {
            view: restore::RestoredView::Terminal(leaf),
        } = &manifest.windows[0].restored_tabs[0].root
        else {
            unreachable!("a terminal leaf")
        };
        let seen = (
            leaf.agent.as_ref().and_then(|agent| agent.session.clone()),
            leaf.cwd.clone(),
        );
        lane.offer(manifest, Vec::new(), Instant::now());
        seen
    };
    let claude = Runs::Agent(aterm_phase::Program::Claude);
    for (fact, host, runs) in [
        ("its host's record", Some("D-own"), claude),
        ("the shell at its prompt", None, Runs::Other),
    ] {
        let (_dir, mut lane) = successor();
        let first = capture(&mut lane, host, runs, false);
        assert_eq!(first.0, host.map(str::to_string), "{fact}: this run's own");
        assert_eq!(
            capture(&mut lane, None, claude, false).0,
            None,
            "{fact}: the parent's conversation is not named again"
        );
    }

    let (_dir, mut lane) = successor();
    for runs in [Runs::Unread, claude] {
        assert_eq!(
            capture(&mut lane, None, runs, false).0.as_deref(),
            Some("C-parent"),
            "{runs:?}: the parent's record stands"
        );
    }

    let (_dir, mut lane) = successor();
    assert_eq!(
        capture(&mut lane, None, Runs::Unread, true).1.as_deref(),
        Some("/b"),
        "a busy first capture keeps the parent's folder"
    );
    lane.withdraw();
    assert_eq!(
        capture(&mut lane, None, Runs::Unread, true).1,
        None,
        "after a withdraw, not the parent's folder"
    );
}

#[test]
fn a_parents_leaf_of_an_agent_with_no_lane_is_no_agent_a_status_read_names() {
    use super::Runs;
    use aterm_phase::Program;
    let record = |word: &str| restore::AgentRestore {
        claude_auth: None,
        word: word.into(),
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        program: format!("/opt/{word}/bin/{word}"),
        argv: vec![format!("/opt/{word}/bin/{word}")],
        session: Some("C-parent".into()),
        cwd: "/b".into(),
        version: None,
        home: None,
        login: None,
        run: None,
        cause: None,
        tab: None,
    };
    {
        let word = "opencode";
        let leaf = record(word);
        assert!(leaf.agent().is_none(), "{word}: no agent this build knows");
        for runs in [
            Runs::Agent(Program::Agy),
            Runs::Agent(Program::Claude),
            Runs::Other,
        ] {
            assert!(!runs.may_be(&leaf), "{word} under {runs:?}");
        }
        assert!(Runs::Unread.may_be(&leaf), "{word}: unread, it stands");
    }
    let codex = restore::AgentRestore {
        run: Some(restore::RunRestore::Codex(restore::CodexRestore {
            home: "/h/.codex".into(),
            home_env: None,
            embedded: false,
            started_ms: 0,
            alab: None,
            carried: None,
        })),
        ..record("codex")
    };
    assert!(Runs::Agent(Program::Codex).may_be(&codex));
    assert!(Runs::Agent(Program::Claude).may_be(&record("")));
    let muse = restore::AgentRestore {
        session: None,
        run: Some(restore::RunRestore::Muse(restore::MuseRestore {
            session: Some("01a148f8-9301-7752-96db-fef456f88074".into()),
            workspace: Some("/b".into()),
            data: "/h/.local/share".into(),
            env: Vec::new(),
        })),
        ..record("muse")
    };
    assert!(Runs::Agent(Program::Muse).may_be(&muse));
    assert!(!Runs::Agent(Program::Claude).may_be(&muse));
    assert_eq!(record("muse").agent(), None);
    assert!(!Runs::Agent(Program::Muse).may_be(&record("muse")));
    let agy = restore::AgentRestore {
        session: None,
        run: Some(restore::RunRestore::Agy(restore::AgyRestore {
            conversation: Some("eaeec4fb-4e43-47da-b46f-8a10c0fee186".into()),
            config: "/h/.gemini/antigravity-cli".into(),
            env: Vec::new(),
        })),
        ..record("agy")
    };
    assert!(Runs::Agent(Program::Agy).may_be(&agy));
    assert!(!Runs::Agent(Program::Claude).may_be(&agy));
    assert_eq!(record("agy").agent(), None);
    assert!(!Runs::Agent(Program::Agy).may_be(&record("agy")));
    let gemini = record("gemini");
    assert!(gemini.retired(), "the retired Gemini CLI's leaf");
    assert_eq!(gemini.agent(), None, "is no agent this build hosts");
    for runs in [
        Runs::Agent(Program::Agy),
        Runs::Agent(Program::Claude),
        Runs::Other,
    ] {
        assert!(!runs.may_be(&gemini), "gemini under {runs:?}");
    }
    assert!(Runs::Unread.may_be(&gemini), "gemini: unread, it stands");
}

#[test]
fn a_take_over_never_takes_an_older_run_of_the_parents_pid() {
    let setup = || {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let pid = reaped_pid();
        let older = JournalId::now(pid);
        marker(logs.path(), older); // its crash evidence: a kill
        let owner = JournalOwner::create(dir.path(), older).expect("armed");
        owner
            .publish(
                &layout(&[("/older", "zsh")]),
                &JournalHeader::now(pid, false),
            )
            .expect("published");
        die(owner);
        (dir, logs, older)
    };
    let (dir, logs, older) = setup();
    let born = older.nanos + 10_000_000_000;
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
    lane.set_predecessor(older.pid, Some(born));
    let own = lane.journal_id();
    assert_eq!(lane.adopt_predecessor(true), None, "not the parent run's");
    drop(lane);
    await_released(&dir.path().join(own.lock_name()));
    let claim = claim_here(dir.path(), logs.path());
    let reopened = claim.reopened.expect("the older desk is still offered");
    assert_eq!(reopened.sources, vec![older]);
    assert_eq!(cwds(&reopened.manifest), vec!["/older"]);

    let (dir, _logs, older) = setup();
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
    lane.set_predecessor(older.pid, None);
    assert_eq!(
        lane.adopt_predecessor(true),
        Some(older),
        "the pid alone takes the older desk"
    );
}

#[test]
fn a_taken_over_image_names_the_parent_as_its_leaves_writer() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let parent = JournalId::now(reaped_pid());
    let owner = JournalOwner::create(dir.path(), parent).expect("armed");
    owner
        .publish(
            &layout(&[("/a", "zsh"), ("/b", "zsh")]),
            &JournalHeader::now(parent.pid, false),
        )
        .expect("published");
    die(owner);
    let successor = reaped_pid();
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), successor);
    lane.set_predecessor(parent.pid, Some(parent.nanos));
    let own = lane.journal_id();
    marker(logs.path(), own);
    assert_eq!(lane.adopt_predecessor(true), Some(parent));
    drop(lane);
    await_released(&dir.path().join(own.lock_name()));
    let reopened = claim_here(dir.path(), logs.path())
        .reopened
        .expect("the desk comes back");
    assert_eq!(reopened.sources, vec![own], "judged as the successor's run");
    assert_eq!(
        reopened.writers,
        vec![parent.pid],
        "its leaves are the parent's"
    );
    assert_eq!(
        JournalHeader {
            pid: 0,
            ..JournalHeader::now(1, false)
        }
        .image_writer(own),
        own.pid
    );
}

#[test]
fn a_quit_cut_short_before_its_journal_went_opens_its_desk_once() {
    let setup = |stamped: bool| {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let desk = layout(&[("/desk", "zsh"), ("/agent", "claude")]);
        let quitting = if stamped {
            run(dir.path(), logs.path(), &desk, false)
        } else {
            run_by_older_build(dir.path(), logs.path(), &desk, None)
        };
        let id = quitting.id();
        let session = dir.path().join("session.toml");
        let stem = id.stem();
        restore::write_quit_to(&session, &desk, stamped.then_some(stem.as_str()))
            .expect("the quit's layout");
        die(quitting); // killed between the layout write and the retire
        let other = run(
            dir.path(),
            logs.path(),
            &layout(&[("/other", "zsh")]),
            false,
        );
        let other_id = other.id();
        die(other);
        let taken = restore::take_quit_from(&session).expect("the quit layout");
        let quit = QuitLayout::of(
            Some(&taken.manifest),
            taken.run.as_deref().and_then(JournalId::parse_stem),
            taken.written_unix_ms,
        );
        assert!(quit.written_unix_ms.is_some(), "the layout is dated");
        let claim = claim_at_boot(dir.path(), &evidence(logs.path()), 1, quit.clone());
        (claim, quit, id, other_id)
    };
    let (claim, quit, id, other) = setup(true);
    assert_eq!(quit.run, Some(id), "the quit names its run");
    let reopened = claim.reopened.expect("the other desk comes back");
    assert_eq!(
        reopened.sources,
        vec![other],
        "the quit's own desk opens once"
    );
    assert_eq!(cwds(&reopened.manifest), vec!["/other"]);
    assert!(claim.set_aside.iter().any(|(set, _)| *set == id));
    assert!(claim.taken.contains(&id), "and it is gone, single-use");

    let (claim, quit, id, other) = setup(false);
    assert_eq!(quit.run, None);
    assert_eq!(quit.shape, vec![2]);
    let reopened = claim.reopened.expect("the other desk comes back");
    assert_eq!(
        reopened.sources,
        vec![other],
        "an older build's quit, cut short, opens its desk once too"
    );
    assert!(claim.set_aside.iter().any(|(set, _)| *set == id));
    assert!(claim.taken.contains(&id), "and it is gone, single-use");

    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let quitting = run_by_older_build(
        dir.path(),
        logs.path(),
        &layout(&[("/desk", "zsh"), ("/agent", "claude")]),
        None,
    );
    let id = quitting.id();
    die(quitting);
    let blind = QuitLayout {
        windows: 1,
        run: None,
        also: Vec::new(),
        shape: vec![3],
        written_unix_ms: None,
    };
    let reopened = claim_at_boot(dir.path(), &evidence(logs.path()), 1, blind)
        .reopened
        .expect("reopened");
    assert_eq!(reopened.sources, vec![id], "the doubled desk");

    let newer_desk = |dated: bool| {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let desk = layout(&[("/desk", "zsh"), ("/agent", "claude")]);
        let quit_at = std::time::SystemTime::now() - Duration::from_secs(3 * 3600);
        let quit_ms = u64::try_from(
            quit_at
                .duration_since(std::time::UNIX_EPOCH)
                .expect("after the epoch")
                .as_millis(),
        )
        .expect("fits");
        let owner = run_by_older_build(dir.path(), logs.path(), &desk, Some(quit_ms + 900));
        let quitting = owner.id();
        let session = dir.path().join("session.toml");
        restore::write_quit_to(&session, &desk, None).expect("the quit's layout");
        std::fs::File::options()
            .write(true)
            .open(&session)
            .and_then(|file| file.set_modified(quit_at))
            .expect("dated hours ago");
        die(owner);
        let crashed = run_by_older_build(
            dir.path(),
            logs.path(),
            &layout(&[("/work", "zsh"), ("/work", "claude")]),
            None,
        );
        let crashed_id = crashed.id();
        die(crashed);
        let taken = restore::take_quit_from(&session).expect("the quit layout");
        assert_eq!(taken.run, None, "an older build's quit names no run");
        assert_eq!(
            taken.written_unix_ms.map(|ms| ms / 1000),
            Some(quit_ms / 1000),
            "dated by its file"
        );
        let quit = QuitLayout::of(
            Some(&taken.manifest),
            None,
            taken.written_unix_ms.filter(|_| dated),
        );
        let claim = claim_at_boot(dir.path(), &evidence(logs.path()), 1, quit);
        (claim, quitting, crashed_id)
    };
    let (claim, quitting, crashed) = newer_desk(true);
    let reopened = claim.reopened.expect("the newer desk comes back");
    assert_eq!(reopened.sources, vec![crashed], "behind the quit, once");
    assert_eq!(cwds(&reopened.manifest), vec!["/work", "/work"]);
    assert!(
        claim.set_aside.iter().any(|(set, _)| *set == quitting),
        "the quit's own journal is still set aside: {:?}",
        claim.set_aside
    );

    let (claim, quitting, crashed) = newer_desk(false);
    assert_eq!(claim.reopened, None, "the control: the newer desk is lost");
    for id in [quitting, crashed] {
        assert!(claim.set_aside.iter().any(|(set, _)| *set == id));
    }
}

#[test]
fn a_quit_beside_another_instances_quit_keeps_both_desks_once() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let session = dir.path().join("session.toml");
    let a_desk = RestoreManifest::new(
        layout(&[("/a/one", "zsh")])
            .windows
            .into_iter()
            .chain(layout(&[("/a/two", "zsh")]).windows)
            .collect(),
    );
    let b_desk = layout(&[("/b/one", "zsh")]);
    let a = run(dir.path(), logs.path(), &a_desk, false);
    let b = run(dir.path(), logs.path(), &b_desk, false);
    let (a_id, b_id) = (a.id(), b.id());
    restore::write_quit_to(&session, &a_desk, Some(&a_id.stem())).expect("A's layout");
    die(a);
    restore::write_quit_to(&session, &b_desk, Some(&b_id.stem())).expect("B's update");
    restore::unwrite_quit_to(&session, Some(&b_id.stem())).expect("taken back");
    let waiting = std::fs::read_to_string(&session).expect("A's desk waits");
    assert_eq!(restore::quit_run_of(&waiting), Some(a_id.stem()));
    restore::write_quit_to(&session, &b_desk, Some(&b_id.stem())).expect("B's layout");
    b.retire().expect("B retired");

    let taken = restore::take_quit_from(&session).expect("the quit layout");
    assert_eq!(
        cwds(&taken.manifest),
        ["/b/one", "/a/one", "/a/two"],
        "both desks, the later quit's first"
    );
    let quit = QuitLayout::of(
        Some(&taken.manifest),
        taken.run.as_deref().and_then(JournalId::parse_stem),
        taken.written_unix_ms,
    )
    .holding(&taken.also);
    let claim = claim_at_boot(dir.path(), &evidence(logs.path()), 1, quit);
    assert_eq!(claim.taken, vec![a_id]);
    assert_eq!(
        claim.reopened, None,
        "A's journal is its quit's, whose desk is in the layout: never a second A"
    );

    restore::write_quit_to(&session, &a_desk, None).expect("unstamped");
    restore::write_quit_to(&session, &b_desk, None).expect("unstamped");
    assert_eq!(
        cwds(&restore::take_from(&session).expect("the layout")),
        ["/b/one"]
    );
}

#[test]
fn a_desk_whose_build_stamps_its_quits_is_never_an_older_quits_own() {
    let now_ms = || {
        u64::try_from(
            aterm_time::since_epoch()
                .expect("after the epoch")
                .as_millis(),
        )
        .expect("fits")
    };
    let scenario = |marked: bool| {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let idle_since = now_ms() - 3_600_000;
        let driver_desk = layout(&[("/work", "zsh"), ("/work", "claude")]);
        let driver = JournalId::now(reaped_pid());
        marker(logs.path(), driver);
        let owner = JournalOwner::create(dir.path(), driver).expect("armed");
        let header = if marked {
            JournalHeader::now(driver.pid, false)
        } else {
            older_header(driver.pid)
        };
        owner
            .publish(
                &driver_desk,
                &JournalHeader {
                    written_unix_ms: idle_since,
                    ..header
                },
            )
            .expect("published");
        let desk = layout(&[("/desk", "zsh"), ("/agent", "claude")]);
        let older = run_by_older_build(dir.path(), logs.path(), &desk, None);
        let quitting = older.id();
        let session = dir.path().join("session.toml");
        restore::write_quit_to(&session, &desk, None).expect("the older quit's layout");
        die(older); // cut short before its journal went
        die(owner); // then the daily driver is killed
        let taken = restore::take_quit_from(&session).expect("the quit layout");
        assert_eq!(taken.run, None, "an older build's quit names no run");
        let quit = QuitLayout::of(Some(&taken.manifest), None, taken.written_unix_ms);
        assert!(
            quit.written_unix_ms
                .is_some_and(|at| at > idle_since + super::QUIT_DATE_SLACK_MS),
            "the driver's image is older than the quit layout"
        );
        assert_eq!(
            quit.shape,
            super::desk_shape(&driver_desk),
            "and has its shape"
        );
        let claim = claim_at_boot(dir.path(), &evidence(logs.path()), 1, quit);
        (claim, quitting, driver)
    };
    let (claim, quitting, driver) = scenario(true);
    let reopened = claim.reopened.expect("the driver's desk comes back");
    assert_eq!(reopened.sources, vec![driver], "behind the quit, once");
    assert_eq!(cwds(&reopened.manifest), vec!["/work", "/work"]);
    assert!(
        claim.set_aside.iter().any(|(set, _)| *set == quitting),
        "the older quit's own journal is still set aside: {:?}",
        claim.set_aside
    );

    let (claim, quitting, driver) = scenario(false);
    assert_eq!(
        claim.reopened, None,
        "the control: the driver's desk is lost"
    );
    for id in [quitting, driver] {
        assert!(claim.set_aside.iter().any(|(set, _)| *set == id));
        assert!(claim.taken.contains(&id), "taken, so gone");
    }
}

#[test]
fn a_taken_over_journal_is_rewritten_and_retired_as_the_successor_s_own() {
    let dir = scratch("cj-dir");
    let parent = JournalId::now(reaped_pid());
    let owner = JournalOwner::create(dir.path(), parent).expect("armed");
    owner
        .publish(
            &layout(&[("/a", "zsh")]),
            &JournalHeader::now(parent.pid, false),
        )
        .expect("published");
    die(owner);
    let mut off = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
    off.set_predecessor(parent.pid, None);
    assert_eq!(off.adopt_predecessor(false), None, "restore_session off");
    let mut none = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
    none.set_predecessor(reaped_pid(), None);
    assert_eq!(none.adopt_predecessor(true), None, "a parent with no image");
    assert!(none.retire(MUST_HAPPEN).is_ok());
    assert_eq!(
        names(dir.path()),
        vec![parent.lock_name(), parent.file_name()],
        "neither touched the parent's journal, and the second removed its own lock"
    );

    let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
    lane.set_predecessor(parent.pid, None);
    let own = lane.journal_id();
    assert_eq!(lane.adopt_predecessor(true), Some(parent));
    assert!(lane.offer(layout(&[("/mine", "zsh")]), Vec::new(), Instant::now()));
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let text = std::fs::read_to_string(dir.path().join(own.file_name())).unwrap();
        if cwds(&decode(&text).unwrap().1) == ["/mine"] {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the successor's image replaces it"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(lane.retire(MUST_HAPPEN).is_ok());
    assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
}

#[test]
fn a_crash_whose_evidence_another_launch_read_first_still_reopens() {
    for (banner, class) in [
        (&b""[..], DeathClass::Killed),
        (&b"aterm: fatal signal 11"[..], DeathClass::Signal),
    ] {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let start = super::unix_nanos(std::time::SystemTime::now());
        let id = JournalId::named(reaped_pid(), Some(start));
        assert_eq!(id.nanos, start);
        let own = logs
            .path()
            .join(markers::file_name(id.pid, start, MarkerOwner::App));
        std::fs::write(&own, banner).unwrap();
        let owner = JournalOwner::create(dir.path(), id).expect("armed");
        owner
            .publish(
                &layout(&[("/survived", "zsh")]),
                &JournalHeader::now(id.pid, false),
            )
            .expect("published");
        die(owner);
        let read = if banner.is_empty() {
            crate::logging::take_kill_evidence_in(logs.path(), std::process::id(), None).is_some()
        } else {
            crate::logging::take_crash_evidence_in(logs.path(), None).is_some()
        };
        assert!(
            read && !own.exists(),
            "control: the evidence was read first"
        );
        let claim = claim_here(dir.path(), logs.path());
        let reopened = claim
            .reopened
            .unwrap_or_else(|| panic!("{class:?}: set aside as {:?}", claim.set_aside));
        assert_eq!(reopened.class, class);
        assert_eq!(cwds(&reopened.manifest), vec!["/survived"]);
    }

    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let id = JournalId::now(reaped_pid());
    let earlier = logs.path().join(format!(
        "{}.seen",
        markers::file_name(id.pid, id.nanos - 7, MarkerOwner::App)
    ));
    std::fs::write(&earlier, b"").unwrap();
    let owner = JournalOwner::create(dir.path(), id).expect("armed");
    owner
        .publish(
            &layout(&[("/handed", "zsh")]),
            &JournalHeader::now(id.pid, false),
        )
        .expect("published");
    die(owner); // the hand-off's `_exit`: its own marker went with it
    let claim = claim_here(dir.path(), logs.path());
    assert_eq!(claim.reopened, None, "an earlier run's consumed marker");
    assert_eq!(claim.taken, vec![id]);
}

#[test]
fn a_killed_runs_consumed_marker_survives_the_prune_until_its_claim() {
    let three_days_ago =
        std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 24 * 3600);
    let killed_desk = |dir: &Path, logs: &Path| {
        let id = JournalId::now(reaped_pid());
        let marker = logs.join(markers::file_name(id.pid, id.nanos, MarkerOwner::App));
        std::fs::write(&marker, b"").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&marker)
            .and_then(|file| file.set_modified(three_days_ago))
            .expect("dated at the run's start");
        let owner = JournalOwner::create(dir, id).expect("armed");
        owner
            .publish(
                &layout(&[("/a", "zsh")]),
                &JournalHeader::now(id.pid, false),
            )
            .expect("published");
        die(owner);
        (id, marker)
    };
    let reports_since = |logs: &Path| {
        for n in 0..crate::logging::KEEP_SEEN_CRASH_REPORTS {
            std::fs::write(
                logs.join(format!("crash-{}.log.seen", 4_000_000 + n)),
                b"thread 'worker' panicked",
            )
            .unwrap();
        }
    };
    for prune_between in [false, true] {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let (id, marker) = killed_desk(dir.path(), logs.path());
        reports_since(logs.path());
        assert!(
            crate::logging::take_kill_evidence_in(
                logs.path(),
                std::process::id(),
                Some(dir.path())
            )
            .is_some(),
            "PRECONDITION: the successor reads the killed run's marker"
        );
        let seen = marker.with_file_name(format!(
            "{}.seen",
            marker.file_name().unwrap().to_string_lossy()
        ));
        assert!(seen.exists(), "PRECONDITION: consumed, not removed");
        if prune_between {
            let _ = crate::logging::take_crash_evidence_in(logs.path(), Some(dir.path()));
            assert!(
                seen.exists(),
                "THE PRUNE KEPT THE MARKER OF A RUN WHOSE JOURNAL WAITS"
            );
        }
        let claim = claim_here(dir.path(), logs.path());
        let reopened = claim.reopened.unwrap_or_else(|| {
            panic!(
                "prune between = {prune_between}: set aside as {:?}",
                claim.set_aside
            )
        });
        assert_eq!(reopened.class, DeathClass::Killed);
        assert_eq!(cwds(&reopened.manifest), vec!["/a"]);
        assert_eq!(claim.taken, vec![id]);
        if prune_between {
            let _ = crate::logging::take_crash_evidence_in(logs.path(), Some(dir.path()));
            assert!(
                !seen.exists(),
                "once claimed, the marker ages out as before"
            );
        }
    }
}

#[test]
fn a_claim_clears_the_dead_writers_leftovers() {
    let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
    let dead_id = JournalId::now(reaped_pid());
    die(JournalOwner::create(dir.path(), dead_id).unwrap());
    let temp = dir.path().join(format!("{}9.tmp", dead_id.temp_prefix()));
    std::fs::write(&temp, b"half").unwrap();
    let claim_file = dir
        .path()
        .join(format!(".journal-1-1.toml.claimed-{}-3.tmp", reaped_pid()));
    std::fs::write(&claim_file, b"x").unwrap();
    let live = JournalOwner::create(dir.path(), JournalId::now(std::process::id())).unwrap();
    let live_temp = dir.path().join(format!("{}9.tmp", live.id().temp_prefix()));
    std::fs::write(&live_temp, b"half").unwrap();

    let _ = claim_here(dir.path(), logs.path());
    assert!(!temp.exists() && !claim_file.exists());
    assert!(!dir.path().join(dead_id.lock_name()).exists());
    assert!(live_temp.exists(), "a live writer's temporary stays");
    assert!(dir.path().join(live.id().lock_name()).exists());
}

#[test]
fn a_rewrite_never_leaves_the_name_without_a_whole_image() {
    let dir = scratch("cj-noloss");
    let owner = JournalOwner::create(dir.path(), JournalId::now(std::process::id())).unwrap();
    owner
        .publish(&layout(&[("/0", "zsh")]), &JournalHeader::now(1, false))
        .unwrap();
    let path = owner.path();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (path, stop) = (path.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut reads = 0usize;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {reads}: the name is empty ({error})"));
                decode(&text).unwrap_or_else(|error| panic!("read {reads}: torn ({error})"));
                reads += 1;
            }
            reads
        })
    };
    for rewrite in 1..=400 {
        let cwd = format!("/{rewrite}");
        owner
            .publish(
                &layout(&[(cwd.as_str(), "zsh")]),
                &JournalHeader::now(1, false),
            )
            .unwrap();
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let reads = reader.join().expect("every read found a whole image");
    assert!(reads > 0, "the reader raced the writer");
    owner.retire().unwrap();
}

#[test]
fn an_image_is_private_and_whole() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("cj-dir");
    let owner = JournalOwner::create(dir.path(), JournalId::now(std::process::id())).unwrap();
    owner
        .publish(&layout(&[("/p", "zsh")]), &JournalHeader::now(1, false))
        .unwrap();
    let mode = std::fs::metadata(owner.path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(names(dir.path()).len(), 2, "{:?}", names(dir.path()));
    let path = owner.path();
    let ordinary = super::unix::read_image(&path).expect("the sole-named image reads");
    let image = std::fs::read(&path).unwrap();
    let other = dir.path().join("another-name.toml");
    std::fs::hard_link(&path, &other).unwrap();
    for name in [&path, &other] {
        assert!(
            super::unix::read_image(name).is_err(),
            "a second name must not admit another file as a journal"
        );
    }
    assert_eq!(std::fs::read(&other).unwrap(), image);
    std::fs::remove_file(&other).unwrap();
    assert_eq!(
        super::unix::read_image(&path).expect("the sole name reads again"),
        ordinary
    );
    owner.retire().unwrap();
}

#[test]
fn the_lane_writes_only_changes_at_the_bounded_rate() {
    let dir = scratch("cj-lane");
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
    let t0 = Instant::now();
    assert!(lane.capture_due(t0), "the first capture is due");
    assert!(lane.offer(layout(&[("/a", "zsh")]), Vec::new(), t0));
    assert!(!lane.capture_due(t0), "idle: nothing is due");
    assert_eq!(lane.next_wake(t0), None, "and nothing is woken for");

    lane.note_activity();
    let t1 = t0 + Duration::from_millis(500);
    assert!(!lane.capture_due(t1), "inside the interval");
    assert_eq!(
        lane.next_wake(t1),
        Some(t0 + MIN_WRITE_INTERVAL),
        "the trailing edge"
    );
    let t2 = t0 + MIN_WRITE_INTERVAL;
    assert!(lane.capture_due(t2));
    assert!(
        !lane.offer(layout(&[("/a", "zsh")]), Vec::new(), t2),
        "an unchanged capture is no write"
    );
    lane.note_activity();
    let t3 = t2 + MIN_WRITE_INTERVAL;
    let mut busy = layout(&[("/a", "zsh")]);
    if let restore::RestoredSplitTree::Leaf {
        view: restore::RestoredView::Terminal(leaf),
    } = &mut busy.windows[0].restored_tabs[0].root
    {
        leaf.cwd = None;
        leaf.title.clear();
    }
    assert!(
        !lane.offer(busy, Vec::new(), t3),
        "a busy engine's empty leaf keeps its last cwd and title"
    );
    lane.note_activity();
    let t4 = t3 + MIN_WRITE_INTERVAL;
    assert!(lane.offer(layout(&[("/a", "zsh"), ("/b", "zsh")]), Vec::new(), t4));
    assert_eq!(lane.writes, 2);
    lane.retire(MUST_HAPPEN).unwrap();

    let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
    lane.begin_probation(t0, false);
    assert!(lane.offer(layout(&[("/p", "zsh")]), Vec::new(), t0));
    assert_eq!(lane.next_wake(t0), Some(t0 + PROBATION));
    assert!(!lane.capture_due(t0 + Duration::from_secs(30)));
    assert!(lane.capture_due(t0 + PROBATION), "the settle is due");
    assert!(
        lane.offer(layout(&[("/p", "zsh")]), Vec::new(), t0 + PROBATION),
        "the same layout, rewritten without the mark"
    );
    assert_eq!(lane.next_wake(t0 + PROBATION), None);
    lane.retire(MUST_HAPPEN).unwrap();
    assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
}

#[test]
fn a_capture_activity_prompted_owes_one_trailing_settle() {
    let dir = scratch("cj-settle");
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
    let t0 = Instant::now();
    assert!(lane.offer(layout(&[("/a", "claude")]), Vec::new(), t0));
    assert_eq!(lane.next_wake(t0), None, "the first capture owes none");
    assert!(!lane.capture_due(t0 + MIN_WRITE_INTERVAL));

    lane.note_activity();
    let t1 = t0 + MIN_WRITE_INTERVAL;
    assert!(lane.capture_due(t1));
    assert!(!lane.offer(layout(&[("/a", "claude")]), Vec::new(), t1));
    let t2 = t1 + MIN_WRITE_INTERVAL;
    assert!(
        !lane.capture_due(t1 + Duration::from_millis(500)),
        "the interval stands"
    );
    assert_eq!(lane.next_wake(t1), Some(t2), "the settle is woken for");
    assert!(lane.capture_due(t2), "the settle is due");
    assert!(
        lane.offer(layout(&[("/a", "claude"), ("/b", "zsh")]), Vec::new(), t2),
        "what moved since is handed over"
    );
    assert_eq!(lane.writes, 2);
    assert_eq!(lane.next_wake(t2), None, "the settle owes none");
    assert!(!lane.capture_due(t2 + MIN_WRITE_INTERVAL), "idle again");

    let mut at = t2 + MIN_WRITE_INTERVAL;
    for _ in 0..3 {
        lane.note_activity();
        assert!(lane.capture_due(at));
        assert!(!lane.capture_due(at - Duration::from_millis(1)));
        lane.offer(layout(&[("/a", "claude"), ("/b", "zsh")]), Vec::new(), at);
        at += MIN_WRITE_INTERVAL;
    }
    assert!(lane.capture_due(at), "its settle");
    lane.offer(layout(&[("/a", "claude"), ("/b", "zsh")]), Vec::new(), at);
    assert_eq!(lane.next_wake(at), None, "and none after it");
    lane.retire(MUST_HAPPEN).unwrap();
}

#[test]
fn a_word_about_typing_is_captured_at_once_inside_the_write_interval() {
    let dir = scratch("cj-owner");
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
    let words = |autopilot: Option<&str>, land_at: Option<i64>, holds: &[&str]| OwnerWords {
        autopilot: autopilot.map(str::to_string),
        land_at,
        holds: holds.iter().map(|h| h.to_string()).collect(),
    };
    let with = |tab: usize, w: &OwnerWords| {
        let mut m = layout(&[("/a", "claude"), ("/b", "zsh")]);
        for (index, other) in m.windows[0].restored_tabs.iter_mut().enumerate() {
            if let restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(leaf),
            } = &mut other.root
                && index == tab
            {
                leaf.autopilot.clone_from(&w.autopilot);
                leaf.land_at = w.land_at;
                leaf.holds.clone_from(&w.holds);
            }
        }
        m
    };
    let none = OwnerWords::default();
    let t0 = Instant::now();
    assert!(lane.offer(with(0, &none), Vec::new(), t0));

    let t1 = t0 + Duration::from_millis(800);
    lane.note_activity();
    assert!(!lane.capture_due(t1), "plain activity: the interval stands");
    assert_eq!(lane.next_wake(t1), Some(t0 + MIN_WRITE_INTERVAL));
    assert!(
        !lane.note_owner_words(0, &none),
        "words the last image carries owe nothing"
    );
    let land = words(Some("land"), None, &[]);
    assert!(
        !lane.note_owner_words(7, &land),
        "a session the last image has no leaf for owes nothing"
    );
    assert!(!lane.capture_due(t1));

    assert!(lane.note_owner_words(0, &land));
    assert!(lane.capture_due(t1), "owed now, inside the interval");
    assert!(lane.offer(with(0, &land), Vec::new(), t1));
    assert_eq!(lane.writes, 2);
    assert!(!lane.note_owner_words(0, &land), "carried now");
    lane.note_activity();
    assert!(
        !lane.capture_due(t1 + Duration::from_millis(100)),
        "the capture that carried it cleared the debt"
    );

    let t2 = t1 + Duration::from_millis(300);
    let held = words(Some("land"), None, &["local review"]);
    assert!(lane.note_owner_words(0, &held));
    assert!(lane.capture_due(t2));
    assert!(lane.offer(with(0, &held), Vec::new(), t2));
    let t3 = t2 + Duration::from_millis(300);
    let off = words(Some("off"), None, &["local review"]);
    assert!(lane.note_owner_words(0, &off));
    assert!(lane.capture_due(t3));
    assert!(lane.offer(with(0, &off), Vec::new(), t3));
    let timed = words(Some("cruise"), Some(9), &[]);
    let t4 = t3 + MIN_WRITE_INTERVAL;
    assert!(lane.offer(with(0, &timed), Vec::new(), t4));
    let t5 = t4 + Duration::from_millis(300);
    let cancelled = words(Some("cruise"), None, &[]);
    assert!(lane.note_owner_words(0, &cancelled));
    assert!(lane.capture_due(t5));
    assert!(lane.offer(with(0, &cancelled), Vec::new(), t5));
    assert_eq!(lane.writes, 6);

    let journal = dir.path().join(lane.journal_id().file_name());
    let image = aterm_spec::wait::until_within(MUST_HAPPEN, "the image naming the cancel", || {
        std::fs::read_to_string(&journal)
            .ok()
            .and_then(|text| decode(&text).ok())
            .map(|(_, manifest)| manifest)
            .filter(|manifest| OwnerWords::carried(manifest, 0) == Some(cancelled.clone()))
    });
    assert_eq!(
        OwnerWords::carried(&image, 1),
        Some(none.clone()),
        "the other tab's leaf carries no word"
    );
    lane.retire(MUST_HAPPEN).unwrap();

    let mut inert = Lane::inert();
    assert!(!inert.note_owner_words(0, &land));
    assert!(!inert.capture_due(t1));
}

fn image_on_disk(
    dir: &Path,
    lane: &mut Lane,
    what: &str,
    ok: impl Fn(&RestoreManifest) -> bool,
) -> RestoreManifest {
    let journal = dir.join(lane.journal_id().file_name());
    aterm_spec::wait::until_within(MUST_HAPPEN, what, || {
        std::fs::read_to_string(&journal)
            .ok()
            .and_then(|text| decode(&text).ok())
            .map(|(_, manifest)| manifest)
            .filter(|manifest| ok(manifest))
    })
}

fn held_layout(holds: &[&str]) -> RestoreManifest {
    let mut m = layout(&[("/a", "claude")]);
    if let restore::RestoredSplitTree::Leaf {
        view: restore::RestoredView::Terminal(leaf),
    } = &mut m.windows[0].restored_tabs[0].root
    {
        leaf.holds = holds.iter().map(|h| h.to_string()).collect();
    }
    m
}

fn read_only(dir: &Path, read_only: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if read_only { 0o500 } else { 0o700 };
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

#[test]
fn an_image_the_writer_could_not_publish_is_handed_again() {
    let dir = scratch("cj-failed");
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
    let t0 = Instant::now();
    assert!(lane.offer(held_layout(&[]), Vec::new(), t0));
    image_on_disk(dir.path(), &mut lane, "the first image", |_| true);

    read_only(dir.path(), true);
    let t1 = t0 + MIN_WRITE_INTERVAL;
    assert!(lane.offer(held_layout(&["local review"]), Vec::new(), t1));
    aterm_spec::wait::until_within(MUST_HAPPEN, "the writer's failure heard", || {
        lane.poll_writer();
        lane.unwritten.then_some(())
    });
    read_only(dir.path(), false);
    assert!(
        !lane.capture_due(t1 + Duration::from_millis(500)),
        "inside the interval nothing is retried"
    );
    let t2 = t1 + MIN_WRITE_INTERVAL;
    assert!(lane.capture_due(t2), "owed again at the interval");
    assert!(
        lane.offer(held_layout(&["local review"]), Vec::new(), t2),
        "the same desk's image is handed again"
    );
    image_on_disk(dir.path(), &mut lane, "the hold's image", |manifest| {
        OwnerWords::carried(manifest, 0).is_some_and(|words| !words.holds.is_empty())
    });
    let t3 = t2 + MIN_WRITE_INTERVAL;
    lane.poll_writer();
    lane.note_activity();
    assert!(
        !lane.offer(held_layout(&["local review"]), Vec::new(), t3),
        "written, the same desk is handed nothing"
    );
    lane.retire(MUST_HAPPEN).unwrap();
}

#[test]
fn a_words_ticket_is_covered_once_an_image_on_disk_carries_it() {
    let dir = scratch("cj-ticket");
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
    let durable = lane.durable();
    let t0 = Instant::now();
    lane.begin_capture();
    assert!(lane.offer(held_layout(&[]), Vec::new(), t0));
    image_on_disk(dir.path(), &mut lane, "the first image", |_| true);

    let first = durable.ask().expect("an active lane hands out tickets");
    let t1 = t0 + Duration::from_millis(300);
    assert!(lane.capture_due(t1), "a ticket owes a capture now");
    lane.begin_capture();
    assert!(!lane.offer(held_layout(&[]), Vec::new(), t1), "nothing new");
    assert_eq!(durable.wait(first, MUST_HAPPEN), Settled::Durable);
    assert!(!lane.capture_due(t1), "carried: owed no more");

    read_only(dir.path(), true);
    let second = durable.ask().expect("a ticket");
    lane.begin_capture();
    assert!(lane.offer(held_layout(&["local -"]), Vec::new(), t1));
    assert_eq!(
        durable.wait(second, Duration::from_millis(50)),
        Settled::Late,
        "no image on disk carries it"
    );
    aterm_spec::wait::until_within(MUST_HAPPEN, "the writer's failure heard", || {
        lane.poll_writer();
        lane.unwritten.then_some(())
    });
    assert_eq!(durable.covered(), first, "still not covered");
    read_only(dir.path(), false);
    let t2 = t1 + MIN_WRITE_INTERVAL;
    assert!(lane.capture_due(t2));
    lane.begin_capture();
    assert!(lane.offer(held_layout(&["local -"]), Vec::new(), t2));
    assert_eq!(durable.wait(second, MUST_HAPPEN), Settled::Durable);
    image_on_disk(dir.path(), &mut lane, "the hold on disk", |manifest| {
        OwnerWords::carried(manifest, 0).is_some_and(|words| !words.holds.is_empty())
    });

    let third = durable.ask().expect("a ticket");
    lane.withdraw();
    assert_eq!(durable.wait(third, MUST_HAPPEN), Settled::Unjournaled);
    assert_eq!(durable.ask(), None, "and hands out no more");
    lane.retire(MUST_HAPPEN).unwrap();

    assert_eq!(Lane::inert().durable().ask(), None);
}

#[test]
fn a_program_starting_is_written_like_a_moved_folder() {
    let dir = scratch("cj-lane");
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
    let t0 = Instant::now();
    assert!(lane.offer(layout(&[("/a", "zsh")]), Vec::new(), t0));
    let t1 = t0 + MIN_WRITE_INTERVAL;
    assert!(
        lane.offer(layout(&[("/a", "vim")]), vec![vim_in(0, 0)], t1),
        "a program started: written though only the title moved"
    );
    let t2 = t1 + MIN_WRITE_INTERVAL;
    assert!(
        !lane.offer(layout(&[("/a", "vim")]), vec![vim_in(0, 0)], t2),
        "nothing moved"
    );
    let t3 = t2 + MIN_WRITE_INTERVAL;
    assert!(
        lane.offer(layout(&[("/a", "vim")]), Vec::new(), t3),
        "it ended: written"
    );
    lane.retire(MUST_HAPPEN).expect("retired");
}

/// A WORKING AGENT'S SPINNER WRITES NOTHING: its title's leading frame turns for as long as the
/// turn runs, the subject after it stands, and no capture of that is written or owed, however
/// long it spins. The turn's end (the frame gives way to the idle mark) and a subject that moves
/// are title changes, written at the title rate. Negative control: a lane that reads a turned
/// frame as a changed title (no `same_but_spinner_frames`) owes a write at every title interval.
#[test]
fn a_spinner_frame_that_turns_is_no_title_change() {
    let dir = scratch("cj-frame");
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
    let t0 = Instant::now();
    assert!(lane.offer(layout(&[("/a", "\u{25d0} build the fix")]), Vec::new(), t0));
    let frames = ["\u{25d3}", "\u{25d1}", "\u{25d2}", "\u{2810}", "\u{25d0}"];
    let mut t = t0;
    for round in 0..60 {
        t += MIN_WRITE_INTERVAL;
        lane.note_activity();
        assert!(lane.capture_due(t));
        let title = format!("{} build the fix", frames[round % frames.len()]);
        assert!(
            !lane.offer(layout(&[("/a", title.as_str())]), Vec::new(), t),
            "round {round}: {title:?} is the image already written"
        );
    }
    assert!(t - t0 > TITLE_WRITE_INTERVAL * 3);
    // Only the trailing capture after the last activity is owed: no title write.
    let settle = t + MIN_WRITE_INTERVAL;
    assert_eq!(lane.next_wake(t), Some(settle));
    assert!(!lane.offer(
        layout(&[("/a", "\u{25d3} build the fix")]),
        Vec::new(),
        settle
    ));
    assert_eq!(lane.next_wake(settle), None, "nothing is owed");

    // The turn ends: the frame gives way to the idle mark, a title change.
    let ended = settle + TITLE_WRITE_INTERVAL;
    lane.note_activity();
    assert!(lane.offer(
        layout(&[("/a", "\u{2733} build the fix")]),
        Vec::new(),
        ended
    ));
    // A subject that moves under a frame is one too, owed at the title rate.
    let moved = ended + MIN_WRITE_INTERVAL;
    lane.note_activity();
    assert!(!lane.offer(layout(&[("/a", "\u{25d0} land it")]), Vec::new(), moved));
    let due = ended + TITLE_WRITE_INTERVAL;
    assert!(lane.capture_due(due));
    assert!(lane.offer(layout(&[("/a", "\u{25d1} land it")]), Vec::new(), due));
    lane.retire(MUST_HAPPEN).unwrap();
}

#[test]
fn a_title_that_animates_is_written_at_the_title_rate() {
    let dir = scratch("cj-title");
    let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
    let t0 = Instant::now();
    assert!(lane.offer(layout(&[("/a", "spin 0")]), Vec::new(), t0));
    let mut t = t0;
    let mut written = 1;
    for frame in 1..=60 {
        t += MIN_WRITE_INTERVAL;
        lane.note_activity();
        assert!(lane.capture_due(t));
        let title = format!("spin {frame}");
        written += usize::from(lane.offer(layout(&[("/a", title.as_str())]), Vec::new(), t));
    }
    let spun = MIN_WRITE_INTERVAL * 60;
    assert_eq!(
        written as u128,
        1 + spun.as_millis() / TITLE_WRITE_INTERVAL.as_millis(),
        "{spun:?} of spinner: once per title interval"
    );

    t += MIN_WRITE_INTERVAL;
    lane.note_activity();
    assert!(lane.offer(layout(&[("/a", "spin 60"), ("/b", "zsh")]), Vec::new(), t));
    let published = t;

    t += MIN_WRITE_INTERVAL;
    lane.note_activity();
    assert!(!lane.offer(layout(&[("/a", "done"), ("/b", "zsh")]), Vec::new(), t));
    let settle = t + MIN_WRITE_INTERVAL;
    assert_eq!(lane.next_wake(t), Some(settle));
    assert!(!lane.offer(layout(&[("/a", "done"), ("/b", "zsh")]), Vec::new(), settle));
    assert!(
        !lane.capture_due(settle + MIN_WRITE_INTERVAL),
        "idle, and not yet due"
    );
    let due = published + TITLE_WRITE_INTERVAL;
    assert_eq!(lane.next_wake(settle), Some(due));
    assert!(lane.capture_due(due));
    assert!(lane.offer(layout(&[("/a", "done"), ("/b", "zsh")]), Vec::new(), due));
    assert_eq!(lane.next_wake(due), None);
    lane.retire(MUST_HAPPEN).unwrap();
}
