// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn git_in(dir: &Path, args: &[&str]) {
    let out = crate::ledger::testing::git()
        .arg("-C")
        .arg(dir)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git -C {} {args:?}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn ledger(rows: &[(&str, &str)]) -> String {
    let rows: Vec<String> = rows
        .iter()
        .map(|(sha, at)| format!("\"{sha}\":{{\"verified_at\":\"{at}\"}}"))
        .collect();
    format!("{{\"mappings\":{{\"aterm\":{{{}}}}}}}\n", rows.join(","))
}

/// A checkout at `dir` of a new bare origin `origin` holding `text` as its ledger.
fn engine(dir: &Path, origin: &Path, text: &str) {
    git_in(
        dir.parent().unwrap(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            &origin.display().to_string(),
        ],
    );
    crate::ledger::testing::init(
        dir,
        &[
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
        ],
    );
    std::fs::write(dir.join("mappings.json"), text).unwrap();
    git_in(dir, &["add", "mappings.json"]);
    git_in(dir, &["commit", "-q", "-m", "ledger"]);
    git_in(
        dir,
        &["remote", "add", "origin", &origin.display().to_string()],
    );
    git_in(dir, &["push", "-q", "-u", "origin", "main"]);
}

/// A CUT, STATUS OR YANK RUN BY HAND READS THE LEDGER ON THE ENGINE'S ORIGIN, AND A RELEASE'S
/// CHILD THE ENGINE IT WAS HANDED: with no `PUBLICATION_ENGINE`, the newest aterm row is the
/// one on the main of the origin the clone's own engine names (a release re-points it at each
/// sync), else `~/publication`'s, though each checkout is behind it; `PUBLICATION_ENGINE`'s
/// ledger is read at its HEAD (the release's synced engine, its own unpushed row included),
/// never a row left uncommitted there; the rows the clone's engine committed and could not
/// push count as recorded where its own origin is the one read, and not where it names none
/// and `~/publication`'s is; the origin is reached only through the caller's native `open`;
/// with no engine naming an origin, nothing is read. NEGATIVE CONTROL: the checkouts' own
/// ledgers name an older row, which a reader of their working trees (as before) would release.
#[test]
fn a_reader_by_hand_reads_the_origins_ledger_and_a_releases_child_its_engines() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (old, new, third) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
    let (home, origin) = (root.join("home"), root.join("publication.git"));
    std::fs::create_dir(&home).unwrap();
    let shared = home.join("publication");
    engine(&shared, &origin, &ledger(&[(&old, "2026-11-01T00:00:00Z")]));
    let peer = root.join("peer");
    git_in(
        &root,
        &["clone", "-q", &origin.display().to_string(), "peer"],
    );
    std::fs::write(
        peer.join("mappings.json"),
        ledger(&[
            (&old, "2026-11-01T00:00:00Z"),
            (&new, "2026-11-07T00:00:00Z"),
        ]),
    )
    .unwrap();
    git_in(
        &peer,
        &[
            "-c",
            "user.name=P",
            "-c",
            "user.email=p@example.invalid",
            "commit",
            "-q",
            "-am",
            "row",
        ],
    );
    git_in(&peer, &["push", "-q", "origin", "HEAD:main"]);
    let repo = root.join("aterm");
    crate::ledger::testing::init(&repo, &[]);
    let var = |engine: Option<&Path>| {
        let (home, engine) = (home.clone(), engine.map(Path::to_path_buf));
        move |name: &str| match name {
            "HOME" => Some(home.clone().into_os_string()),
            "PUBLICATION_ENGINE" => engine.clone().map(PathBuf::into_os_string),
            _ => None,
        }
    };
    let in_place = |dir: &Path| {
        newest_published_source(&std::fs::read_to_string(dir.join("mappings.json")).unwrap())
            .unwrap()
            .commit
    };

    let open = crate::github_remote::engine_fixture;
    let by_hand = published_source_in(&repo, &var(None), &open).unwrap();
    assert_eq!(by_hand.commit, new, "the origin's newest row");
    assert_eq!(
        in_place(&shared),
        old,
        "NEGATIVE CONTROL: the checkout's own is older"
    );
    // The origin is reached only through `open`'s native authority: one that refuses reads
    // nothing, though the origin is a readable repository the fixture's `open` just read.
    let refused = published_source_in(&repo, &var(None), &|_, _| {
        Err("no admitted identity for it".into())
    })
    .unwrap_err()
    .to_string();
    assert!(refused.contains("no admitted identity for it"), "{refused}");
    let child = published_source_in(&repo, &var(Some(&shared)), &open).unwrap();
    assert_eq!(
        child.commit, old,
        "PUBLICATION_ENGINE's ledger, at its HEAD"
    );
    // A row left uncommitted in the named engine's working tree is no row of the engine.
    std::fs::write(
        shared.join("mappings.json"),
        ledger(&[
            (&old, "2026-11-01T00:00:00Z"),
            (&third, "2026-11-08T00:00:00Z"),
        ]),
    )
    .unwrap();
    assert_eq!(
        published_source_in(&repo, &var(Some(&shared)), &open)
            .unwrap()
            .commit,
        old,
        "the named engine's HEAD, not its working tree"
    );
    assert_eq!(
        in_place(&shared),
        third,
        "NEGATIVE CONTROL: its working tree holds the uncommitted row"
    );

    // The clone's own engine names the origin a reader by hand reads.
    let copy = aterm_verify::engine::engine_copy(&aterm_verify::engine::release_dir(&repo));
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    engine(
        &copy,
        &root.join("elsewhere.git"),
        &ledger(&[(&third, "2026-11-02T00:00:00Z")]),
    );
    assert_eq!(
        published_source_in(&repo, &var(None), &open)
            .unwrap()
            .commit,
        third
    );
    // A row the clone's engine committed and could not push is as recorded as the origin's.
    let fourth = "d".repeat(40);
    std::fs::write(
        copy.join("mappings.json"),
        ledger(&[
            (&third, "2026-11-02T00:00:00Z"),
            (&fourth, "2026-11-03T00:00:00Z"),
        ]),
    )
    .unwrap();
    git_in(&copy, &["commit", "-q", "-am", "unpushed row"]);
    assert_eq!(
        published_source_in(&repo, &var(None), &open)
            .unwrap()
            .commit,
        fourth
    );
    assert_eq!(
        ledger_with_unpushed(&ledger(&[(&third, "2026-11-02T00:00:00Z")]), "not json"),
        ledger(&[(&third, "2026-11-02T00:00:00Z")]),
        "an unreadable unpushed ledger adds nothing"
    );
    // A copy whose remote section is gone keeps the main it last fetched and the row it holds
    // unpushed, which is that origin's record, never the one `~/publication` names.
    let fifth = "e".repeat(40);
    std::fs::write(
        copy.join("mappings.json"),
        ledger(&[
            (&third, "2026-11-02T00:00:00Z"),
            (&fourth, "2026-11-03T00:00:00Z"),
            (&fifth, "2026-11-09T00:00:00Z"),
        ]),
    )
    .unwrap();
    git_in(&copy, &["commit", "-q", "-am", "unpushed row"]);
    let elsewhere = root.join("elsewhere.git").display().to_string();
    git_in(&copy, &["config", "--remove-section", "remote.origin"]);
    assert_eq!(
        published_source_in(&repo, &var(None), &open)
            .unwrap()
            .commit,
        new,
        "~/publication's origin, without the copy's row"
    );
    git_in(&copy, &["remote", "add", "origin", &elsewhere]);
    assert_eq!(
        published_source_in(&repo, &var(None), &open)
            .unwrap()
            .commit,
        fifth,
        "NEGATIVE CONTROL: the copy's own origin, with its row"
    );

    // No engine names an origin: nothing is read, and the refusal says where it looked.
    std::fs::remove_dir_all(&copy).unwrap();
    std::fs::remove_dir_all(&shared).unwrap();
    let none = published_source_in(&repo, &var(None), &open)
        .unwrap_err()
        .to_string();
    assert!(none.contains("names its origin"), "{none}");
    assert!(none.contains(&shared.display().to_string()), "{none}");
}

/// Where [`an_inherited_git_dir_cannot_answer_for_the_copys_unpushed_rows`]'s re-run writes
/// what it read.
const GIT_DIR_PROBE: &str = "ATERM_RELEASE_TEST_UNPUSHED_GIT_DIR_PROBE";

/// A COPY'S UNPUSHED ROWS ARE READ FROM THE COPY, WHATEVER `GIT_DIR` THE RELEASE INHERITED:
/// [`unpushed_ledger`] reads the repository [`crate::github_remote::origin_slug`] judged, so a
/// release started where `GIT_DIR` names another repository (a hook, an editor) counts only
/// the copy's own unpushed row. The test binary is re-run with that `GIT_DIR` kept
/// ([`crate::ledger::testing::CHOSEN_GIT_ENV`]). NEGATIVE CONTROL: in that re-run, a plain
/// `git -C <copy>` reads the other repository's ledger.
#[test]
fn an_inherited_git_dir_cannot_answer_for_the_copys_unpushed_rows() {
    if let Some(root) = std::env::var_os(GIT_DIR_PROBE).map(PathBuf::from) {
        let run: aterm_verify::engine::RunGit<'_> = &|cmd: &mut Command| {
            aterm_uds::deadline::output_within(cmd, aterm_spec::wait::MUST_HAPPEN)
                .map_err(|e| std::io::Error::other(e.to_string()))
        };
        let copy = root.join("copy");
        let read = unpushed_ledger(&copy, run).unwrap_or_default();
        let plain = command(aterm_uds::os_path!("git"))
            .arg("-C")
            .arg(&copy)
            .args(["show", "HEAD:mappings.json"])
            .output()
            .unwrap();
        std::fs::write(root.join("read"), read).unwrap();
        std::fs::write(root.join("plain"), plain.stdout).unwrap();
        return;
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let unpushed = |dir: &Path, text: &str| {
        engine(dir, &dir.with_extension("git"), &ledger(&[]));
        std::fs::write(dir.join("mappings.json"), text).unwrap();
        git_in(dir, &["commit", "-q", "-am", "unpushed"]);
    };
    let own = ledger(&[(&"a".repeat(40), "2026-11-08T00:00:00Z")]);
    let other = ledger(&[(&"b".repeat(40), "2026-11-08T00:00:00Z")]);
    unpushed(&root.join("copy"), &own);
    unpushed(&root.join("other"), &other);
    let mut child = command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "gates::ledger_reader_tests::an_inherited_git_dir_cannot_answer_for_the_copys_unpushed_rows",
            "--test-threads=1",
        ])
        .env(GIT_DIR_PROBE, &root)
        .env("GIT_DIR", root.join("other/.git"))
        .env(crate::ledger::testing::CHOSEN_GIT_ENV, "1")
        .stdin(aterm_uds::spawnfd::Stdio::null());
    let ran =
        aterm_uds::deadline::output_within(&mut child, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert_eq!(std::fs::read_to_string(root.join("read")).unwrap(), own);
    assert_eq!(
        std::fs::read_to_string(root.join("plain")).unwrap(),
        other,
        "NEGATIVE CONTROL: the inherited GIT_DIR answers a plain read"
    );
}
