// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
use super::*;

// The unit fixtures resolve only local packages. Reuse one private Cargo home for a
// test thread's graph reads, outside its Git worktree and concurrent builds' cache lock.
pub(super) fn use_fixture_home(cmd: &mut aterm_uds::spawnfd::Command) -> Result<(), String> {
    thread_local! {
        static FIXTURE_CARGO_HOME: std::io::Result<aterm_tempfile::TempDir> = aterm_tempfile::tempdir();
    }
    FIXTURE_CARGO_HOME.with(|home| {
        let home = home
            .as_ref()
            .map_err(|e| format!("create graph fixture Cargo home: {e}"))?;
        cmd.env("CARGO_HOME", home.path());
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn failed_original_child_retains_both_streams_and_natural_exit() {
    let mut child = command("/bin/sh");
    child.args([
        "-c",
        "printf original-stdout; printf 'original-stderr\\377' >&2; exit 23",
    ]);
    let why = match capture(&mut child, Duration::from_secs(60), &mut || false) {
        Ok(_) => panic!("the failed producer must not authorize graph selection"),
        Err(why) => why,
    };
    for original in [
        "original-stdout",
        "original-stderr",
        "\\xff",
        "natural exit: exit status: 23",
        "exited unsuccessfully",
    ] {
        assert!(why.contains(original), "{why}");
    }
}

#[cfg(unix)]
#[test]
fn stopped_capture_keeps_original_prefixes_without_inventing_exit() {
    let mut child = command("/bin/sh");
    child.args([
        "-c",
        "printf before-stop; printf stderr-before-stop >&2; sleep 30",
    ]);
    let why = match capture_when(
        &mut child,
        Duration::from_secs(60),
        &mut |stdout, stderr| !stdout.is_empty() && !stderr.is_empty(),
    ) {
        Ok(_) => panic!("stopping an incomplete producer cannot authorize graph selection"),
        Err(why) => why,
    };
    for fact in [
        "collection was cancelled",
        "before-stop",
        "stderr-before-stop",
        "natural exit: not observed",
        "stream completion unproven",
    ] {
        assert!(why.contains(fact), "{why}");
    }
}

#[cfg(unix)]
#[test]
fn spawn_empty_and_unparsed_reads_are_distinct_refusals() {
    let world = aterm_tempfile::tempdir().expect("owned fixture");
    let root = world.path();
    let mut tools =
        crate::toolchain::Toolchain::discover(Some(root), root, OsStr::new(""), None, None);
    tools.refused = None;
    for (producer, expected) in [
        (
            root.join("absent-producer"),
            "creation/start was not observed",
        ),
        (
            Path::new("/usr/bin/true").to_owned(),
            "succeeded but stdout is empty",
        ),
        (
            Path::new("/bin/echo").to_owned(),
            "no parseable workspace members",
        ),
    ] {
        tools.targo = producer;
        let why = super::super::read_members(root, &tools, OsStr::new(""))
            .expect_err("no member evidence");
        assert!(why.contains(expected), "{why}");
        assert!(
            why.contains("stdout (") && why.contains("stderr ("),
            "{why}"
        );
    }
}

#[test]
fn graph_failure_authorizes_neither_a_workspace_scope_nor_a_composition() {
    use super::super::{Selection, cone, graph_value, meet};
    let failure = Selection::Unavailable("original graph cause".to_owned());
    assert_eq!(cone("main", &failure).unwrap_err(), "original graph cause");
    let widened = Selection::Widened("a deliberate policy widening".to_owned());
    assert_eq!(
        meet(widened, &[failure.clone()], &|_| panic!(
            "no runnable graph"
        )),
        failure
    );
    assert_eq!(
        meet(
            failure.clone(),
            &[Selection::Unavailable("later failure".to_owned())],
            &|_| panic!("no runnable graph")
        ),
        failure,
        "the original unavailable judgment is not replaced by a later read"
    );
    let held = std::cell::RefCell::new(None);
    assert_eq!(
        graph_value::<()>(&held, || Err("first original failure".to_owned())),
        None
    );
    assert_eq!(
        graph_value::<()>(&held, || panic!(
            "a failed graph is not re-run to reconstruct evidence"
        )),
        None
    );
    assert_eq!(held.into_inner().as_deref(), Some("first original failure"));
}

#[cfg(unix)]
#[test]
fn required_workspace_queries_refuse_empty_and_unparsed_original_outputs() {
    use super::super::{Member, Members, foreign_specs, read_path_packages};

    let world = aterm_tempfile::tempdir().expect("owned workspace fixture");
    let root = world.path();
    std::fs::write(
        root.join("Cargo.lock"),
        "[[package]]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .expect("owned locked package");
    let members = Members::new(vec![Member {
        name: "fixture".into(),
        dir: "crates/fixture".into(),
        has_lib: true,
    }]);
    let mut tools =
        crate::toolchain::Toolchain::discover(Some(root), root, OsStr::new(""), None, None);
    tools.refused = None;
    let observed: Vec<_> = ["/usr/bin/true", "/bin/echo"]
        .into_iter()
        .map(|producer| {
            tools.targo = Path::new(producer).to_owned();
            (
                producer,
                read_path_packages(root, &tools, OsStr::new(""), &members),
                foreign_specs(root, &tools, OsStr::new(""), &["fixture".into()]),
            )
        })
        .collect();
    assert!(
        observed
            .iter()
            .all(|(_, paths, foreign)| paths.is_err() && foreign.is_err()),
        "required workspace evidence must not become an empty path set or a foreign-package judgment: {observed:#?}"
    );
    for (producer, paths, foreign) in observed {
        let expected = if producer == "/usr/bin/true" {
            "succeeded but stdout is empty"
        } else {
            "no parseable workspace packages"
        };
        for why in [paths.unwrap_err(), foreign.unwrap_err()] {
            for fact in [
                expected,
                "natural exit: exit status: 0",
                "stdout (",
                "stderr (",
            ] {
                assert!(why.contains(fact), "{producer}: {why}");
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn member_only_workspace_and_absent_inverse_keep_distinct_valid_empty_sets() {
    use super::super::{
        Member, Members, Reach, invert, outside_graph, outside_packages, parse_tree_packages,
        required_workspace_text, tree_root_prefix,
    };

    let world = aterm_tempfile::tempdir().expect("owned workspace fixture");
    let root = world.path();
    let members = Members::new(vec![Member {
        name: "fixture".into(),
        dir: "crates/fixture".into(),
        has_lib: true,
    }]);
    let mut child = command("/bin/sh");
    child
        .args([
            "-c",
            "printf 'fixture v0.1.0 (%s)\\n' \"$1\"",
            "graph-producer",
        ])
        .arg(root.join("crates/fixture"));
    let read = capture(&mut child, Duration::from_secs(60), &mut || false)
        .expect("original successful member-only workspace producer");
    let text = required_workspace_text(&read).expect("the workspace names its member");
    assert_eq!(
        outside_packages(text, &tree_root_prefix(root), &members),
        Some(Vec::new()),
        "a valid workspace need not have outside path packages"
    );
    let lock = [("fixture".to_owned(), "0.1.0".to_owned())]
        .into_iter()
        .collect();
    assert!(outside_graph(&["fixture".into()], &lock, &parse_tree_packages(text)).is_empty());
    assert_eq!(
        invert(
            root,
            Path::new("/usr/bin/true"),
            OsStr::new(""),
            "fixture",
            &members,
        )
        .expect("an inverse tree may find no package on this host"),
        Reach::default()
    );
}
