// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const DAY: u64 = 24 * 60 * 60;

/// A fixed present, far enough from the epoch for every age below.
fn present() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000)
}

fn ago(secs: u64) -> SystemTime {
    present() - Duration::from_secs(secs)
}

/// Set `path`'s last write to `at`.
fn written(path: &Path, at: SystemTime) {
    std::fs::File::open(path).unwrap().set_modified(at).unwrap();
}

/// A folder `dir` holding one file, both last written `at`.
fn folder(dir: &Path, file: &str, at: SystemTime) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(file), b"bytes").unwrap();
    written(&dir.join(file), at);
    written(dir, at);
}

/// A run's log folder under `state` named `name`, last written `at`, owned by `release` when
/// one is given.
fn run_logs(state: &Path, name: &str, at: SystemTime, release: Option<&Path>) -> PathBuf {
    let logs = state.join(".ubd/logs.noindex").join(name);
    std::fs::create_dir_all(&logs).unwrap();
    if let Some(release) = release {
        own(&logs, release);
        written(&logs.join(OWNER), at);
    }
    folder(&logs, "transcript.log", at);
    logs
}

/// A clone's release folder at `release` whose newest release began at `started`.
fn clone_with_release(release: &Path, started: SystemTime) {
    std::fs::create_dir_all(release).unwrap();
    crate::release::Journal {
        started: started
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        ..Default::default()
    }
    .save(&release.join("release.toml"))
    .unwrap();
}

fn left(dirs: &[&PathBuf]) -> Vec<bool> {
    dirs.iter().map(|dir| dir.is_dir()).collect()
}

/// A run's log folder goes once its clone's newest release began after it and nothing there
/// names it; the newest release's, the one `latest-run` names, the one the `running` claim
/// names, a day's, and another live clone's newest all stay. NEGATIVE CONTROL: the folder that
/// goes is the same age and shape as the one `latest-run` keeps.
#[test]
fn a_runs_logs_stay_while_a_release_names_them_and_go_after() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let (mine, other, gone) = (
        dir.path().join("mine/.git/aterm-release"),
        dir.path().join("other/.git/aterm-release"),
        dir.path().join("gone/.git/aterm-release"),
    );
    clone_with_release(&mine, ago(5 * DAY));
    clone_with_release(&other, ago(40 * DAY));
    let old = run_logs(&state, "20260101T000000-11", ago(9 * DAY), Some(&mine));
    let latest = run_logs(&state, "20260101T000001-12", ago(9 * DAY), Some(&mine));
    let running = run_logs(&state, "20260101T000002-13", ago(9 * DAY), Some(&mine));
    let newest = run_logs(&state, "20260105T000000-14", ago(4 * DAY), Some(&mine));
    let today = run_logs(&state, "20260109T000000-15", ago(3_600), Some(&gone));
    let others = run_logs(&state, "20251201T000000-16", ago(30 * DAY), Some(&other));
    let orphan = run_logs(&state, "20260102T000000-17", ago(9 * DAY), Some(&gone));
    std::fs::write(
        mine.join("latest-run"),
        latest.as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    std::fs::write(
        mine.join(crate::release::RUNNING_FILE),
        format!("pid 1\nstarted then\nlogs {}\n", running.display()),
    )
    .unwrap();

    let removed = sweep_logs(&state, &mine, present(), &|_| false);
    assert_eq!(removed, [old.clone(), orphan.clone()]);
    assert_eq!(
        left(&[&old, &latest, &running, &newest, &today, &others, &orphan]),
        [false, true, true, true, true, true, false]
    );
}

/// A folder that names no clone waits [`UNOWNED`], and then goes only when the sweeping clone
/// names it nowhere; a folder whose process still runs stays whatever its age; a clone whose
/// journal cannot be read keeps every folder of its own. NEGATIVE CONTROL: each kept folder
/// has a twin that differs in that one fact and goes.
#[test]
fn an_unowned_live_or_unreadable_runs_logs_stay() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let (mine, torn) = (
        dir.path().join("mine/.git/aterm-release"),
        dir.path().join("torn/.git/aterm-release"),
    );
    clone_with_release(&mine, ago(2 * DAY));
    std::fs::create_dir_all(&torn).unwrap();
    std::fs::write(torn.join("release.toml"), "not = [a journal").unwrap();
    let legacy = state.join("logs/20251101T000000-21");
    folder(&legacy, "transcript.log", ago(20 * DAY));
    let young = state.join("logs/20251220T000000-22");
    folder(&young, "transcript.log", ago(10 * DAY));
    let named = state.join("logs/20251101T000001-23");
    folder(&named, "transcript.log", ago(20 * DAY));
    std::fs::write(
        mine.join("latest-run"),
        named.as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    let live = run_logs(&state, "20260101T000000-77", ago(9 * DAY), Some(&mine));
    let dead = run_logs(&state, "20260101T000000-78", ago(9 * DAY), Some(&mine));
    let unread = run_logs(&state, "20260101T000000-31", ago(9 * DAY), Some(&torn));

    let removed = sweep_logs(&state, &mine, present(), &|pid| pid == 77);
    assert_eq!(removed, [dead.clone(), legacy.clone()]);
    assert_eq!(
        left(&[&legacy, &young, &named, &live, &dead, &unread]),
        [false, true, true, true, false, true]
    );
}

/// A frozen tool set goes when no record of the release folder names it; the sets the records
/// name, the set just frozen, a day's, and one a process still runs from (the remover's
/// refusal) stay, and an interrupted freeze's staging folder goes. NEGATIVE CONTROL: with a
/// record that cannot be read, nothing goes.
#[test]
fn a_frozen_tool_set_stays_while_a_record_names_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let release = dir.path().join("aterm-release");
    let tools = release.join(".ubd/tools");
    let set = |digit: &str, at| {
        let path = tools.join(digit.repeat(64));
        folder(&path, "aterm-release", at);
        path
    };
    let (cutter, tool, frozen) = (
        set("a", ago(9 * DAY)),
        set("b", ago(9 * DAY)),
        set("c", ago(9 * DAY)),
    );
    let (unnamed, used, fresh) = (
        set("d", ago(9 * DAY)),
        set("e", ago(9 * DAY)),
        set("f", ago(3_600)),
    );
    let stage = tools.join(".tools-x1.noindex");
    folder(&stage, "aterm-release", ago(9 * DAY));
    std::fs::write(
        release.join(".ubd/cutter.toml"),
        format!("path = \"{}\"\n", cutter.join("aterm-release").display()),
    )
    .unwrap();
    std::fs::write(
        release.join(".ubd/tool.toml"),
        format!("set_sha256 = \"{}\"\n", "b".repeat(64)),
    )
    .unwrap();
    let remove = |path: &Path| {
        if path == used {
            return Err("pid 7 still references this artifact".to_owned());
        }
        std::fs::remove_dir_all(path)
            .map(|()| true)
            .map_err(|e| e.to_string())
    };

    // A record that is a directory cannot be read: everything stays.
    std::fs::create_dir(release.join("torn.toml")).unwrap();
    assert!(sweep_tools(&release, &frozen, present(), &remove).is_empty());
    std::fs::remove_dir(release.join("torn.toml")).unwrap();

    let removed = sweep_tools(&release, &frozen, present(), &remove);
    assert_eq!(removed, [stage.clone(), unnamed.clone()]);
    assert_eq!(
        left(&[&cutter, &tool, &frozen, &unnamed, &used, &fresh, &stage]),
        [true, true, true, false, true, true, false]
    );
}

/// An earlier pump controller copy goes on the next install; the installed one, a day's and
/// one the remover refuses (an agent names it, or a process runs from it) stay. A cutter
/// checkout goes when the cutter cache it was made for is gone; one whose cache stands and one
/// with no record stay. NEGATIVE CONTROL: the copies and checkouts that stay are the same age
/// as those that go.
#[test]
fn earlier_controller_copies_and_orphaned_cutter_checkouts_go() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let controllers = state.join(".ubd/pump/controllers");
    let copy = |name: &str, at| {
        let path = controllers.join(name);
        folder(&path, "aterm-release", at);
        path
    };
    let (installed, earlier) = (copy("aa", ago(9 * DAY)), copy("bb", ago(9 * DAY)));
    let (running, fresh) = (copy("cc", ago(9 * DAY)), copy("dd", ago(3_600)));
    let refuse = |held: PathBuf| {
        move |path: &Path| {
            if path == held {
                return Err("pid 7 still references this artifact".to_owned());
            }
            std::fs::remove_dir_all(path)
                .map(|()| true)
                .map_err(|e| e.to_string())
        }
    };
    let removed = sweep_controllers(
        &controllers,
        &installed,
        present(),
        &refuse(running.clone()),
    );
    assert_eq!(removed, std::slice::from_ref(&earlier));
    assert_eq!(
        left(&[&installed, &earlier, &running, &fresh]),
        [true, false, true, true]
    );

    let cache = dir
        .path()
        .join("clone/.git/aterm-release/.ubd/cutter-cache");
    std::fs::create_dir_all(&cache).unwrap();
    let checkout = |key: &str, cache: Option<&Path>| {
        let path = state.join(format!(".ubd/cutter-{key}.noindex"));
        folder(&path, "Cargo.toml", ago(9 * DAY));
        if let Some(cache) = cache {
            own_cutter(&path, cache);
        }
        path
    };
    let alive = checkout("1", Some(&cache));
    let orphan = checkout("2", Some(&dir.path().join("gone/.ubd/cutter-cache")));
    let unrecorded = checkout("3", None);
    let removed = sweep_cutters(&state, present(), &refuse(PathBuf::new()));
    assert_eq!(removed, std::slice::from_ref(&orphan));
    assert_eq!(left(&[&alive, &orphan, &unrecorded]), [true, false, true]);
    assert!(!cutter_owner(&orphan).exists() && cutter_owner(&alive).exists());
}
