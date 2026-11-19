// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! A write under a progress window whose HTTP transfer the window's stall bound ends
//! (`GIT_HTTP_LOW_SPEED_*`), over real git and curl to a smart-HTTP remote whose hook never
//! returns: the remote holds the connection open and answers nothing. git's HTTP transport polls
//! that socket every 50 ms, so the push's tree keeps using CPU and the tree's own window never
//! closes; the transfer's bound ends it. The remote serves each connection apart, as GitHub
//! does, so a re-read made meanwhile is answered.

use super::*;
use agh::journal::{IntentStore as _, JournalRecord, Settled};
use agh::policy::{RouteRequest, RouteTarget};
use agh_fake::git_backend::GitHttpBackend;
use std::os::unix::fs::PermissionsExt as _;

/// The window each test runs under: curl ends a transfer that moves less than a byte a second
/// for two seconds.
const WINDOW: Duration = Duration::from_secs(2);

/// A remote serving `source` as `fixture-dev/app`, whose hook `name` marks `held` once it runs
/// and then never returns: `pre-receive` holds the pack before any ref moves, `post-receive`
/// holds the answer of a write whose refs moved, as a response lost on its way back does.
fn parked(source: &Path, name: &str, held: &Path) -> GitHttpBackend {
    let backend = GitHttpBackend::start(
        "fixture-dev/app",
        source,
        &agh_fake::token("reader"),
        &agh_fake::token("dev"),
    )
    .expect("the private Git fixture");
    let hook = backend.repository().join("hooks").join(name);
    let sh = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
    std::fs::write(
        &hook,
        format!(
            "#!{}\n: > '{}'\nexec '{}' {}\n",
            sh.display(),
            held.display(),
            sleep.display(),
            aterm_spec::wait::PARKED_SECS
        ),
    )
    .expect("the hook");
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("its mode");
    backend
}

/// A client of `world` whose git reaches `backend`, on clock `fx`.
fn client_of(world: &World, backend: &GitHttpBackend, fx: &Arc<Fx>) -> Github {
    let ports = world.fake.ports();
    git_builder(
        world.router.clone(),
        Endpoints::loopback(ports.api, ports.uploads, backend.port(), ports.cdn),
        &world.dirs,
        git_env(),
        live_start(),
    )
    .fx(fx.clone())
    .build()
}

/// A source repository whose origin is the fixture's GitHub URL, and its main.
fn source_of(world: &World) -> (PathBuf, String) {
    let source = bare_repo(world.path(), "source");
    host_git(
        &source,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/fixture-dev/app.git",
        ],
    );
    let sha = host_git(&source, &["rev-parse", "main"]);
    (source, sha)
}

/// The write ran `git push` once, and `reads` re-reads (`ls-remote`) followed it.
fn pushed_once_then_read(children: &[ChildSpec], reads: usize) {
    let words = |child: &ChildSpec, word: &str| child.args.iter().any(|arg| arg == word);
    let args = || children.iter().map(|child| &child.args).collect::<Vec<_>>();
    let pushes: Vec<_> = children
        .iter()
        .enumerate()
        .filter(|(_, child)| words(child, "push"))
        .map(|(at, _)| at)
        .collect();
    let [pushed] = pushes[..] else {
        panic!("the write ran its push once: {:?}", args());
    };
    assert_eq!(
        children[pushed..]
            .iter()
            .filter(|child| words(child, "ls-remote"))
            .count(),
        reads,
        "re-reads after the push: {:?}",
        args()
    );
    for child in children {
        assert_eq!(
            env_of(child, "GIT_HTTP_LOW_SPEED_LIMIT").map(OsString::as_os_str),
            Some(std::ffi::OsStr::new("1")),
            "{child:?}"
        );
        assert_eq!(
            env_of(child, "GIT_HTTP_LOW_SPEED_TIME").map(OsString::as_os_str),
            Some(std::ffi::OsStr::new("2")),
            "{child:?}"
        );
    }
}

/// The settlements `records` holds, `intent` being the only intent they hold.
fn settled_of<'a>(records: &'a [(u64, JournalRecord)], intent: &agh::IntentId) -> Vec<&'a Settled> {
    let intents: Vec<_> = records
        .iter()
        .filter_map(|(_, record)| match record {
            JournalRecord::Intent(intent) => Some(intent.id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(intents, std::slice::from_ref(intent), "{records:?}");
    records
        .iter()
        .filter_map(|(_, record)| match record {
            JournalRecord::Settled {
                intent: settled,
                outcome,
            } if settled == intent => Some(outcome),
            _ => None,
        })
        .collect()
}

/// A WRITE UNDER A PROGRESS WINDOW ENDS ITSELF ON A REMOTE THAT NEVER ANSWERS, OF UNKNOWN
/// OUTCOME: the native push (`push_atomic_with_progress_window`, what `repo push` runs with no
/// `--timeout`) sends its pack to a remote whose `pre-receive` hook never returns,
/// so the remote holds the pack and the connection and answers nothing, and the transfer's
/// stall bound ends the push. agh re-reads the guarded refs once and finds them unchanged; that
/// cannot exclude a delayed commit of the pack the remote holds, so agh never pushes again: it
/// settles the intent of the write it journaled Unknown ([`agh::write::Outcome::Unknown`]), and
/// the journal's view of it stays open, refusing the repository's later atomic ref writes, until
/// it is reconciled. NEGATIVE CONTROLS (mutants): with no transfer bound on its children, the
/// push waits on the remote past the hang detector, as `verify land`'s main and journal writes
/// would; a stalled transfer that agh sends on through its receive-failure recovery after the
/// re-read (the `stalled` guard deleted) pushes again, and fails the one push: the test moves
/// agh's simulated clock to one pause short of [`agh::git::GIT_BUDGET`] once the hook holds the
/// pack, so the recovery makes its first repeat and its second re-read finds the patience spent
/// (two pushes and two re-reads in about two windows, not the ten minutes of repeats a silent
/// remote would take), while the receive closure's `map_err` keeps that recovery's outcome
/// Unknown and its intent open, so only the push count tells it apart; one returned before its
/// re-read fails the one re-read.
#[test]
fn a_progress_window_ends_a_push_the_remote_never_answers() {
    let world = reader_world("native-push-silent-remote");
    let (source, sha) = source_of(&world);
    let held = world.path().join("pack-held");
    let backend = parked(&source, "pre-receive", &held);
    let fx = Arc::new(Fx::simulated(live_start()));
    let gh = client_of(&world, &backend, &fx);
    let (tx, rx) = std::sync::mpsc::channel();
    let runner = std::thread::spawn(move || {
        let slug = common::slug("fixture-dev/app");
        let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(slug.clone())));
        let journal = gh.journal(&slug).expect("journal");
        let batch = push_batch(&[("refs/tags/held", ABSENT, &sha)]);
        let done = gh
            .refs(&slug)
            .push_atomic_with_progress_window(&permit, &source, &batch, &journal, WINDOW)
            .map(|outcome| match outcome {
                agh::write::Outcome::Unknown { intent, error } => {
                    Ok((intent, error.into_data().why))
                }
                other => Err(other.slug()),
            })
            .map_err(|error| error.into_data().why);
        let view = match &done {
            Ok(Ok((intent, _))) => journal.view(intent).expect("the journal"),
            _ => None,
        };
        let (records, _) = journal.read().expect("the journal");
        let _ = tx.send((done, view, records, gh.fx().children_run()));
    });
    aterm_spec::wait::until_while_descendants_work(
        "the remote's pre-receive hook ran: it holds the pack",
        || held.exists().then_some(()),
    );
    // One pause (the recovery's first, a second) short of the budget: a repeat would be made
    // once, and the patience spent at its re-read.
    fx.sleep(agh::git::GIT_BUDGET.saturating_sub(Duration::from_secs(1)))
        .expect("the simulated clock moves");
    let (done, view, records, children) = rx
        .recv_timeout(common::HANG)
        .expect("the push ended within the hang detector");
    runner.join().expect("the runner thread");
    let Ok(Ok((unknown, why))) = &done else {
        panic!("the remote answered nothing after it took the pack: {done:?} {records:?}");
    };
    assert!(
        why.contains("moved less than a byte a second for 2s")
            && why.contains("no re-read found it landed"),
        "the transfer's stall bound ended the push, and its re-read found it unlanded: {why}"
    );
    let settled = settled_of(&records, unknown);
    assert!(
        matches!(settled[..], [Settled::Unknown { .. }]),
        "agh settled the intent of the write it ended Unknown: {records:?}"
    );
    let view = view.expect("the journal views the intent");
    assert!(
        view.open && matches!(view.settled, Some(Settled::Unknown { .. })),
        "the intent stays open until it is reconciled: {view:?}"
    );
    pushed_once_then_read(&children, 1);
    drop(backend);
}

/// A STALLED PUSH THAT ITS RE-READ FINDS LANDED IS DONE, AND ITS INTENT CLOSES: the remote's
/// `post-receive` hook, which runs once the refs moved, never returns, so the remote holds the
/// answer of a write it took and the transfer's stall bound ends the push. agh re-reads the
/// guarded refs once, finds every requested ref at AFTER, which proves the write landed, and
/// returns it done, settling its intent Created: the journal holds nothing open, so the
/// repository's later atomic ref writes from this Mac (`verify land`'s record journal after its
/// push of main) are not refused. It pushed once. NEGATIVE CONTROL (mutant): a stalled transfer
/// returned before its re-read is settled Unknown, an intent left open over a write that landed.
#[test]
fn a_stalled_push_its_re_read_finds_landed_is_done_and_closes_its_intent() {
    let world = reader_world("native-push-answer-held");
    let (source, sha) = source_of(&world);
    let held = world.path().join("refs-moved");
    let backend = parked(&source, "post-receive", &held);
    let fx = Arc::new(Fx::simulated(live_start()));
    let gh = client_of(&world, &backend, &fx);
    let (tx, rx) = std::sync::mpsc::channel();
    let after = sha.clone();
    let runner = std::thread::spawn(move || {
        let slug = common::slug("fixture-dev/app");
        let permit = common::permit(&gh, &RouteRequest::write(RouteTarget::Repo(slug.clone())));
        let journal = gh.journal(&slug).expect("journal");
        let batch = push_batch(&[("refs/tags/held", ABSENT, &after)]);
        let done = gh
            .refs(&slug)
            .push_atomic_with_progress_window(&permit, &source, &batch, &journal, WINDOW)
            .map(|outcome| match outcome {
                agh::write::Outcome::Done(_) => Ok(()),
                agh::write::Outcome::Unknown { error, .. } => {
                    Err(format!("unknown: {}", error.into_data().why))
                }
                other => Err(other.slug().to_owned()),
            })
            .map_err(|error| error.into_data().why);
        let open = journal.open().expect("the journal");
        let (records, _) = journal.read().expect("the journal");
        let _ = tx.send((done, open, records, gh.fx().children_run()));
    });
    let (done, open, records, children) = rx
        .recv_timeout(common::HANG)
        .expect("the push ended within the hang detector");
    runner.join().expect("the runner thread");
    assert!(
        matches!(done, Ok(Ok(()))),
        "a re-read that finds every ref moved proves the write landed: {done:?} {records:?}"
    );
    assert!(
        held.exists(),
        "the remote moved the refs before it held its answer"
    );
    assert_eq!(
        host_git(backend.repository(), &["rev-parse", "refs/tags/held"]),
        sha
    );
    let intent = records
        .iter()
        .find_map(|(_, record)| match record {
            JournalRecord::Intent(intent) => Some(intent.id.clone()),
            _ => None,
        })
        .expect("the write journaled its intent");
    let settled = settled_of(&records, &intent);
    assert!(
        matches!(settled[..], [Settled::Created { .. }]),
        "agh settled the intent of the write it found landed Created: {records:?}"
    );
    assert!(open.is_empty(), "nothing stays open: {open:?}");
    pushed_once_then_read(&children, 1);
    drop(backend);
}

/// A STALLED LEASED DELETE THAT ITS RE-READ FINDS DONE IS WRITTEN: a leased delete (what
/// aterm-release's `delete_ref` runs under a 60-second window) opens no intent, so a stalled
/// transfer takes its receive-failure recovery as any other receive failure does. The remote's
/// `post-receive` hook, which runs once the ref is gone, never returns, and the transfer's stall
/// bound ends the push; agh re-reads the ref, finds it absent and returns the delete done
/// (`remote re-read confirmed ref absence`), so the release step converges instead of failing and
/// meeting a stale lease when run again. NEGATIVE CONTROL (mutant): a stalled transfer returned
/// before the re-read is an error of unknown outcome over a delete that landed.
#[test]
fn a_stalled_leased_delete_its_re_read_finds_done_is_written() {
    let world = reader_world("native-delete-answer-held");
    let (source, sha) = source_of(&world);
    host_git(&source, &["tag", "doomed", "main"]);
    let held = world.path().join("ref-deleted");
    let backend = parked(&source, "post-receive", &held);
    let fx = Arc::new(Fx::simulated(live_start()));
    let gh = client_of(&world, &backend, &fx);
    let (tx, rx) = std::sync::mpsc::channel();
    let runner = std::thread::spawn(move || {
        let slug = common::slug("fixture-dev/app");
        let run = GitRun::push_delete(&slug, "refs/tags/doomed", &sha).with_progress_window(WINDOW);
        let done = gh
            .run_git(&run)
            .map(|out| out.stdout)
            .map_err(|error| error.into_data());
        let _ = tx.send((done, gh.fx().children_run()));
    });
    let (done, children) = rx
        .recv_timeout(common::HANG)
        .expect("the delete ended within the hang detector");
    runner.join().expect("the runner thread");
    let stdout = done.unwrap_or_else(|error| {
        panic!(
            "a re-read that finds the ref absent proves the delete landed: {:?} {:?} {}",
            error.kind, error.side_effect, error.why
        )
    });
    assert!(
        stdout.contains("remote re-read confirmed ref absence"),
        "{stdout}"
    );
    assert!(
        held.exists(),
        "the remote deleted the ref before it held its answer"
    );
    assert_eq!(
        host_git(backend.repository(), &["tag", "--list", "doomed"]),
        ""
    );
    pushed_once_then_read(&children, 1);
    drop(backend);
}
