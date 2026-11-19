// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

/// This process's pid, boot and start, as its record names it.
fn own() -> (u32, String, u64) {
    let pid = std::process::id();
    let (boot, start) = crate::release_progress::identity(pid).unwrap();
    (pid, boot, start)
}

/// This process's [`Place`]; the calling test ends COULD NOT RUN on a host that names no machine
/// or pid namespace for it, where no record is written.
fn place() -> Place {
    here().unwrap_or_else(|| {
        aterm_spec::host::could_not_run("this host names no machine or pid namespace for a process")
    })
}

/// The wall second now: a record a test writes is written now.
fn wall() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// What the records under `state`'s drive folder say now ([`read`]).
fn reading_of(state: &Path) -> Reading {
    read(&drive_folder(state), wall())
}

/// A reading that tells `graphs` and names `spent`.
fn told(graphs: Vec<PathBuf>, spent: Vec<PathBuf>) -> Reading {
    Reading {
        graphs: Some(graphs),
        spent,
    }
}

/// A reading that cannot be told and names `spent`.
fn untold(spent: Vec<PathBuf>) -> Reading {
    Reading {
        graphs: None,
        spent,
    }
}

/// Write a record of `(pid, boot, start)` on this machine and in this pid namespace publishing
/// in `graphs` under `state`, at its own name.
fn record_of(state: &Path, pid: u32, boot: &str, start: u64, graphs: &[&Path]) -> PathBuf {
    record_at(state, pid, boot, start, graphs, &place())
}

/// [`record_of`] at `place`.
fn record_at(
    state: &Path,
    pid: u32,
    boot: &str,
    start: u64,
    graphs: &[&Path],
    place: &Place,
) -> PathBuf {
    let dir = drive_folder(state);
    std::fs::create_dir_all(&dir).unwrap();
    let graphs: Vec<String> = graphs
        .iter()
        .map(|graph| graph.to_str().unwrap().to_owned())
        .collect();
    let body = aterm_json::json!({"pid": pid, "boot": boot, "start": start,
        "host": place.host, "namespace": place.namespace, "graphs": graphs});
    let path = dir.join(name_of(pid, start));
    std::fs::write(&path, body.to_string()).unwrap();
    path
}

/// A record names this process by pid and start in a private folder, mode 0600, and is read
/// back as a running process's graphs; [`forget`] removes it, and no folder is no records.
/// NEGATIVE CONTROL: the same record once one of its graphs is no folder reads as `None`, so a
/// reader never takes a graph it cannot open for an empty one.
#[test]
fn a_record_names_this_process_and_is_read_back_while_it_runs() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    assert_eq!(reading_of(&state), told(Vec::new(), Vec::new()));
    let (one, two) = (dir.path().join("one"), dir.path().join("two"));
    for graph in [&one, &two] {
        std::fs::create_dir(graph).unwrap();
    }
    let path = record(&drive_folder(&state), &[one.clone(), two.clone()]).unwrap();
    let (pid, _, start) = own();
    assert_eq!(
        path,
        drive_folder(&state).join(format!("{pid}-{start}.json"))
    );
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode(&path), mode(&drive_folder(&state))), (0o600, 0o700));
    assert_eq!(
        reading_of(&state),
        told(vec![one.clone(), two.clone()], Vec::new())
    );
    std::fs::remove_dir(&two).unwrap();
    assert_eq!(reading_of(&state), untold(Vec::new()));
    std::fs::create_dir(&two).unwrap();
    forget(&path);
    assert!(!path.exists());
    assert_eq!(reading_of(&state), told(Vec::new(), Vec::new()));
}

/// A record of a process this kernel proves ended (this pid at another start) is not read for
/// its graphs, whatever they are, and is named among the dead; a writer's temporary beside the
/// records is not read at all.
/// NEGATIVE CONTROL: the same record at this process's own start is read as running, and its
/// graph that is no folder makes the whole reading `None`.
#[test]
fn a_dead_process_record_is_ignored_and_named_dead() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let (pid, boot, start) = own();
    let absent = dir.path().join("absent");
    let dead = record_of(&state, pid, &boot, start + 1, &[&absent]);
    std::fs::write(drive_folder(&state).join(".1-2.json.tmp.3.x"), "half").unwrap();
    assert_eq!(reading_of(&state), told(Vec::new(), vec![dead.clone()]));
    std::fs::remove_file(&dead).unwrap();
    record_of(&state, pid, &boot, start, &[&absent]);
    assert_eq!(reading_of(&state), untold(Vec::new()));
}

/// Anything the records do not say whole makes the reading `None`: a record that is no JSON, one
/// whose contents name another process than its name, one no regular file (a folder, a link), a
/// record of a process this kernel does not judge (pid 1), and a records folder that is no
/// folder.
/// NEGATIVE CONTROL: each case's folder read with that entry removed is a reading.
#[test]
fn a_record_that_does_not_read_whole_makes_the_records_unknown() {
    let (pid, boot, start) = own();
    let cases: [(&str, &dyn Fn(&Path) -> PathBuf); 5] = [
        ("not json", &|state| {
            let path = drive_folder(state).join("7-8.json");
            std::fs::write(&path, "{").unwrap();
            path
        }),
        ("another name", &|state| {
            let path = record_of(state, pid, &boot, start, &[]);
            let renamed = drive_folder(state).join(format!("{pid}-{}.json", start + 1));
            std::fs::rename(&path, &renamed).unwrap();
            renamed
        }),
        ("a folder", &|state| {
            let path = drive_folder(state).join("7-8.json");
            std::fs::create_dir(&path).unwrap();
            path
        }),
        ("a link", &|state| {
            let target = record_of(state, pid, &boot, start, &[]);
            let moved = state.join("elsewhere.json");
            std::fs::rename(&target, &moved).unwrap();
            let path = drive_folder(state).join(name_of(pid, start));
            std::os::unix::fs::symlink(&moved, &path).unwrap();
            path
        }),
        ("unjudged", &|state| record_of(state, 1, &boot, 5, &[])),
    ];
    for (what, make) in cases {
        let dir = aterm_tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(drive_folder(&state)).unwrap();
        let path = make(&state);
        assert_eq!(reading_of(&state), untold(Vec::new()), "{what}");
        if path.is_dir() && !path.is_symlink() {
            std::fs::remove_dir(&path).unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }
        assert_eq!(reading_of(&state), told(Vec::new(), Vec::new()), "{what}");
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(state.join("drive")).unwrap();
    std::fs::write(drive_folder(&state), "").unwrap();
    assert_eq!(reading_of(&state), untold(Vec::new()));
}

/// A graph holds a tab by an entry of its name, of any kind; none of a name that is no session,
/// and a graph that cannot be read says nothing.
/// NEGATIVE CONTROL: the entry removed, the same tab is not held.
#[test]
fn a_graph_holds_a_tab_by_its_entry() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let graph = dir.path().join("graph");
    std::fs::create_dir(&graph).unwrap();
    let tab = "s-0000000000000000000a";
    std::fs::write(graph.join(tab), "sock x\n").unwrap();
    assert_eq!(holds(&graph, tab), Some(true));
    assert_eq!(holds(&graph, "self"), None);
    std::fs::remove_file(graph.join(tab)).unwrap();
    assert_eq!(holds(&graph, tab), Some(false));
    let file = dir.path().join("file");
    std::fs::write(&file, "").unwrap();
    assert_eq!(holds(&file, tab), None);
}

/// Set the modification time of the record at `path` to wall second `at`.
fn written_at(path: &Path, at: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(at)))
        .unwrap();
}

/// A record in the format written before records named a place: no host, no namespace.
fn placeless_record(state: &Path, pid: u32, boot: &str, start: u64) -> PathBuf {
    let path = drive_folder(state).join(name_of(pid, start));
    std::fs::create_dir_all(drive_folder(state)).unwrap();
    let body = aterm_json::json!({"pid": pid, "boot": boot, "start": start, "graphs": []});
    std::fs::write(&path, body.to_string()).unwrap();
    path
}

/// The wall second of the entry at `path`'s own modification time (`lstat`, a link's own).
fn written(path: &Path) -> u64 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A record that cannot be told, whatever stops it, keeps the reading `None` while its entry
/// was written less than a week ago, and is not spent: one of another machine or another pid
/// namespace (a network home, a container that mounts this home) or one that names no place,
/// whose pid this kernel cannot judge though at another start it reads as ended here; one of a
/// process of this machine this kernel does not judge (pid 1); one of another process than its
/// name; one that is no JSON; one this process cannot read; and a link. Unwritten for a week by
/// its own time, each is abandoned: named spent and skipped, so the rest reads, and [`live`]
/// removes it (a link itself, not what it names).
/// NEGATIVE CONTROLS: one second short of the week each reading is still `None` and the entry
/// stays; the record of an ended process at this process's machine and namespace is spent by its
/// pid at any age.
#[test]
fn a_record_that_cannot_be_told_is_unknown_until_a_week_unwritten() {
    let week = super::super::upgrade_drive::STATE_KEEP_S;
    let (pid, boot, start) = own();
    let ours = place();
    let other_namespace = format!("{}9", ours.namespace.clone().unwrap_or_default());
    let other_machine = Place {
        host: format!("{}0", ours.host),
        namespace: ours.namespace.clone(),
    };
    let other_pid_namespace = Place {
        host: ours.host.clone(),
        namespace: Some(other_namespace),
    };
    let cases: [(&str, &dyn Fn(&Path) -> PathBuf); 8] = [
        ("another machine", &|state| {
            record_at(state, pid, &boot, start + 1, &[], &other_machine)
        }),
        ("another namespace", &|state| {
            record_at(state, pid, &boot, start + 1, &[], &other_pid_namespace)
        }),
        ("no place", &|state| {
            placeless_record(state, pid, &boot, start + 1)
        }),
        ("unjudged", &|state| record_of(state, 1, &boot, 5, &[])),
        ("another name", &|state| {
            let path = record_of(state, pid, &boot, start, &[]);
            let renamed = drive_folder(state).join(format!("{pid}-{}.json", start + 1));
            std::fs::rename(&path, &renamed).unwrap();
            renamed
        }),
        ("no json", &|state| {
            let path = drive_folder(state).join("7-8.json");
            std::fs::write(&path, "{").unwrap();
            path
        }),
        ("unreadable", &|state| {
            let path = record_of(state, pid, &boot, start, &[]);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            path
        }),
        ("a link", &|state| {
            let target = record_of(state, pid, &boot, start, &[]);
            let moved = state.join("elsewhere.json");
            std::fs::rename(&target, &moved).unwrap();
            let path = drive_folder(state).join(name_of(pid, start));
            std::os::unix::fs::symlink(&moved, &path).unwrap();
            path
        }),
    ];
    for (what, make) in cases {
        let dir = aterm_tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(drive_folder(&state)).unwrap();
        let folder = drive_folder(&state);
        let path = make(&state);
        let at = written(&path);
        assert_eq!(read(&folder, at + week - 1), untold(Vec::new()), "{what}");
        assert_eq!(live(&folder, at + week - 1), None, "{what}");
        assert!(
            path.symlink_metadata().is_ok(),
            "{what}: a fresh record was removed"
        );
        assert_eq!(
            read(&folder, at + week),
            told(Vec::new(), vec![path.clone()]),
            "{what}"
        );
        assert_eq!(live(&folder, at + week), Some(Vec::new()), "{what}");
        assert!(
            path.symlink_metadata().is_err(),
            "{what}: an abandoned record stayed"
        );
        let ended = record_at(&state, pid, &boot, start + 1, &[], &ours);
        assert_eq!(
            read(&folder, written(&ended)),
            told(Vec::new(), vec![ended]),
            "{what}"
        );
    }
}

/// No age abandons a record that can be told or an entry that is no file or link: this
/// process's own record ten weeks unwritten is read as running and not spent, and a folder named
/// as a record keeps the reading `None` and stays; a link's removal leaves what it names.
/// NEGATIVE CONTROL: a link in the same folder, which cannot be told, is removed once a week
/// unwritten (and not one second before), so the age reaches the reading.
#[test]
fn a_told_record_or_a_folder_is_never_abandoned() {
    let week = super::super::upgrade_drive::STATE_KEEP_S;
    let (pid, boot, start) = own();
    let dir = aterm_tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let folder = drive_folder(&state);
    let graph = dir.path().join("graph");
    std::fs::create_dir(&graph).unwrap();
    let running = record_of(&state, pid, &boot, start, &[&graph]);
    let at = written(&running);
    assert_eq!(
        read(&folder, at + 10 * week),
        told(vec![graph.clone()], Vec::new())
    );
    assert_eq!(live(&folder, at + 10 * week), Some(vec![graph]));
    assert!(running.is_file(), "a running process's record was removed");
    std::fs::remove_file(&running).unwrap();
    let folded = folder.join("7-8.json");
    std::fs::create_dir(&folded).unwrap();
    let at = written(&folded);
    assert_eq!(read(&folder, at + 10 * week), untold(Vec::new()));
    assert_eq!(live(&folder, at + 10 * week), None);
    assert!(folded.is_dir(), "a folder named as a record was removed");
    std::fs::remove_dir(&folded).unwrap();
    let target = dir.path().join("target.json");
    std::fs::write(&target, "{").unwrap();
    let link = folder.join("7-8.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let at = written(&link);
    assert_eq!(live(&folder, at + week - 1), None);
    assert!(link.symlink_metadata().is_ok(), "a fresh link was removed");
    assert_eq!(live(&folder, at + week), Some(Vec::new()));
    assert!(link.symlink_metadata().is_err(), "an abandoned link stayed");
    assert!(target.is_file(), "a link's removal removed what it names");
}

/// Every record is read, so a record this kernel proves ended is named spent, and [`live`]
/// removes it, though another record makes the reading `None`.
/// NEGATIVE CONTROL: the record that makes the reading `None` (another machine's, fresh) is not
/// spent and stays.
#[test]
fn an_ended_record_is_spent_though_another_makes_the_reading_unknown() {
    let (pid, boot, start) = own();
    let ours = place();
    let dir = aterm_tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let folder = drive_folder(&state);
    let ended = record_of(&state, pid, &boot, start + 1, &[]);
    let foreign = Place {
        host: format!("{}0", ours.host),
        namespace: ours.namespace,
    };
    let unknown = record_at(&state, pid + 1, &boot, start, &[], &foreign);
    assert_eq!(reading_of(&state), untold(vec![ended.clone()]));
    assert_eq!(live(&folder, wall()), None);
    assert!(!ended.exists(), "an ended record stayed");
    assert!(unknown.is_file(), "a fresh foreign record was removed");
}

/// [`refresh`] moves a record's modification time without changing its bytes, and refuses
/// anything but a regular file at the path, a link to one included.
/// NEGATIVE CONTROL: before the refresh the record reads as written at its old time.
#[test]
fn a_refresh_moves_the_time_of_a_record_and_nothing_else() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let path = record(&drive_folder(&state), &[]).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    written_at(&path, 1_700_000_000);
    let modified = |path: &Path| std::fs::metadata(path).unwrap().modified().unwrap();
    let old = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert_eq!(modified(&path), old);
    let new = old + Duration::from_secs(3_600);
    refresh(&path, new).unwrap();
    assert_eq!(
        (modified(&path), std::fs::read(&path).unwrap()),
        (new, bytes)
    );
    let link = drive_folder(&state).join("1-2.json");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(refresh(&link, old).is_err());
    assert_eq!(modified(&path), new);
}

/// The variable naming the harness state a child of
/// [`a_test_binary_records_under_its_own_state_root_whatever_the_env_names`] is given, in that
/// child only.
const FOLDERS_CHILD: &str = "ATERM_GRAPH_RECORDS_FOLDERS_CHILD";

/// In a test binary [`folders`] names only folders under its redirected state root, whatever
/// `$ATERM_HARNESS_STATE` names: this binary run again with that variable set to a scratch folder
/// records itself in every folder [`folders`] names, and nothing is written under the scratch
/// folder.
/// NEGATIVE CONTROL: in that child the CLI's resolver reads the variable as set, and the
/// resolver given it ([`folders_of`]) names a folder under the scratch folder, the one a
/// [`folders`] that read the variable would write in.
#[test]
fn a_test_binary_records_under_its_own_state_root_whatever_the_env_names() {
    const MARKER: &str = "graph-records-folders-child=recorded";
    if let Some(harness) = std::env::var_os(FOLDERS_CHILD) {
        let harness = PathBuf::from(harness);
        let root = aterm_types::dirs::state_dir().expect("a test binary's state root");
        assert_eq!(super::super::cli::harness_state_dir(), Ok(harness.clone()));
        assert!(folders_of(Some(&root), Some(&harness)).contains(&agent_folder(&harness)));
        let folders = folders();
        assert!(!folders.is_empty());
        for folder in &folders {
            assert!(folder.starts_with(&root), "{}", folder.display());
            forget(&record(folder, &[]).unwrap());
        }
        println!("{MARKER}");
        return;
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let harness = dir.path().join("harness");
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "harness::graph_records::tests::a_test_binary_records_under_its_own_state_root_whatever_the_env_names",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(FOLDERS_CHILD, &harness)
        .env("ATERM_HARNESS_STATE", &harness);
    let output = aterm_uds::deadline::output_while_process_progressing(
        &mut command,
        aterm_spec::wait::MUST_HAPPEN,
        "the records child",
    )
    .unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains(MARKER),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !harness.exists(),
        "a test binary wrote under the env's harness state"
    );
}

/// The records folders of an aterm state root and a harness state: the drive folder's, then the
/// default harness state's under that root, then the told harness state's, each named once.
/// NEGATIVE CONTROL: a told harness state that is the root's default one names its folder once,
/// and with no aterm state root only the told harness state's folder is named.
#[test]
fn the_records_folders_name_each_root_once() {
    let (root, harness) = (Path::new("/a/state"), Path::new("/h"));
    let default = super::super::cli::harness_state_in(root);
    assert_eq!(
        folders_of(Some(root), Some(harness)),
        [
            drive_folder(root),
            agent_folder(&default),
            agent_folder(harness)
        ]
    );
    assert_eq!(
        folders_of(Some(root), Some(&default)),
        [drive_folder(root), agent_folder(&default)]
    );
    assert_eq!(folders_of(None, Some(harness)), [agent_folder(harness)]);
}
