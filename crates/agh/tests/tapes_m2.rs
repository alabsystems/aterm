// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Tapes M2: credentialed tapes and the route, journal, fabric and git doors (WP10).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agh::config::Dirs;
use agh::fx::canon;
use agh::fx::tape::{TapeEndpoints, TapeMode};
use agh::fx::{DOOR_FABRIC, DOOR_GH, DOOR_GIT, DOOR_JOURNAL, DOOR_ROUTE};
use agh::git::GitRun;
use agh::identity::MapEnv;
use agh::notify::RecordingNotifier;
use agh::policy::{AnonymousWhy, ChainStep, Decision, Presence, Refusal, RefusalCode, Tier};
use agh::wire::Endpoints;
use agh::wire::scripted::ScriptedTransport;
use agh_fake::{AssetSpec, ReleaseSpec};
use astream_effects::EffectRecord;

use crate::common::{self, MAIN_SHA, run_cli};
use crate::tapes::over_the_fake::{Ran, World, arg, exchanges, read_tape};

const APP: &str = "fixture-dev/app";

/// The why of the error line on `stderr`.
fn why(stderr: &str) -> String {
    stderr
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .find_map(|line| line.get("error")?.get("why")?.as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("an error line with a why: {stderr}"))
}

/// Every `sub` the error lines on `stderr` name.
fn subs(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .filter_map(|line| line.get("error")?.get("sub")?.as_str().map(str::to_string))
        .collect()
}

/// `args` in a silent context over `endpoints`: its exit and its stderr.
fn refused(root: &Path, endpoints: Endpoints, args: &[&str]) -> (u8, String) {
    let transport = Arc::new(ScriptedTransport::new());
    let (mut ctx, captured) =
        agh::cli::Ctx::for_tests(root, transport.clone(), endpoints, MapEnv::new());
    let exit = run_cli(&mut ctx, args);
    assert!(
        transport.requests().is_empty(),
        "{args:?} sent {:?}",
        transport.requests()
    );
    (exit, captured.stderr_text())
}

#[test]
fn a_one_shot_recording_against_github_com_is_refused() {
    let dir = common::scratch("tapes-m2-one-shot");
    let t = common::tape_path(dir.path(), "t");
    let t_arg = arg(&t);
    let refs = "repos/alabsystems/aterm/git/refs";
    for (line, names) in [
        (
            vec!["release", "create", "alabsystems/aterm", "v1", "--draft"],
            "release create",
        ),
        (
            vec!["release", "ensure", "alabsystems/aterm", "v1"],
            "release ensure",
        ),
        (
            vec![
                "ref",
                "create",
                "alabsystems/aterm",
                "refs/tags/v1",
                MAIN_SHA,
            ],
            "ref create",
        ),
        (vec!["api", "-X", "POST", refs], "api"),
        (vec!["api", "--method", "post", refs], "api"),
        (vec!["api", "--method=Post", refs], "api"),
    ] {
        for dry_run in [false, true] {
            let mut args = vec!["--record", t_arg];
            args.extend(line.iter().copied());
            if dry_run {
                args.push("--dry-run");
            }
            let (exit, stderr) = refused(dir.path(), Endpoints::github_com(), &args);
            assert_eq!(exit, 2, "{args:?}: {stderr}");
            let why = why(&stderr);
            assert!(why.contains(names), "{args:?}: {why}");
            assert!(why.contains("github.com"), "{args:?}: {why}");
            assert!(!t.exists(), "{args:?}");
        }
    }
}

#[test]
fn provision_and_git_verbs_are_refused() {
    let dir = common::scratch("tapes-m2-refused");
    let t = common::tape_path(dir.path(), "t");
    let t_arg = arg(&t);
    for line in [
        &["auth", "import", "dev2", "--from-gh", "--tier", "dev"][..],
        &["auth", "git-credential", "get"][..],
        &["repo", "clone", APP, "d"][..],
        &["policy", "grant", "fixture-caller", "alabsystems/aterm"][..],
    ] {
        let mut args = vec!["--record", t_arg];
        args.extend(line.iter().copied());
        let (exit, stderr) = refused(dir.path(), Endpoints::github_com(), &args);
        assert_eq!(exit, 2, "{args:?}: {stderr}");
        assert!(!t.exists(), "{args:?}");
    }
    assert_eq!(
        std::fs::read_dir(dir.path())
            .expect("the scratch directory")
            .count(),
        0,
        "no file is made"
    );
}

#[test]
fn a_target_name_cannot_be_taped() {
    let dir = common::scratch("tapes-m2-target");
    let t = common::tape_path(dir.path(), "t");
    let t_arg = arg(&t);
    for args in [
        vec!["--record", t_arg, "repo", "view", "@channel"],
        vec!["--record", t_arg, "api", "-R", "@channel", "repos/x"],
        vec![
            "--record",
            t_arg,
            "--as",
            "dev",
            "cat",
            "@channel:README.md",
        ],
        vec!["--record", t_arg, "ls", "@channel"],
    ] {
        let (exit, stderr) = refused(dir.path(), Endpoints::github_com(), &args);
        assert_eq!(exit, 2, "{args:?}: {stderr}");
        assert!(why(&stderr).contains("@channel"), "{args:?}: {stderr}");
        assert!(!t.exists(), "{args:?}");
    }
}

#[test]
fn an_anonymous_tape_holds_no_route_record() {
    let w = World::new("tapes-m2-anonymous");
    w.fake.add_repo(
        "alabsystems/aterm",
        agh_fake::RepoSpec::public().push("fixture-release-bot"),
    );
    let t = w.tape("t");
    let recorded = w.live(&["--record", arg(&t), "repo", "view", "alabsystems/aterm"]);
    assert_eq!(recorded.exit, 0, "{}", recorded.stderr);
    let (header, records) = read_tape(&t);
    assert_eq!(header.mode, TapeMode::Anonymous);
    assert!(exchanges(&records, DOOR_ROUTE).is_empty(), "{records:?}");
    let replayed = w.offline(&["--replay", arg(&t)]);
    assert_eq!(replayed.exit, 0, "{}", replayed.stderr);
    assert_eq!(replayed.stdout, recorded.stdout);
    w.fake.assert_clean();
}

/// The sha256 of `bytes`, lowercase hex.
fn hex_of(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

#[test]
fn a_download_round_trips_by_digest() {
    use std::os::unix::fs::PermissionsExt as _;

    let w = World::new("tapes-m2-download");
    let bytes = b"the bytes of a.bin, a release asset".to_vec();
    let release = w
        .fake
        .add_release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    w.fake
        .add_asset(release, AssetSpec::uploaded("a.bin", &bytes));
    let d = w.root.path().join("d");
    std::fs::create_dir(&d).expect("the download directory");
    let t = w.tape("t");
    let line = [
        "release", "download", APP, "v1", "--asset", "a.bin", "--dir",
    ];
    let mut args = vec!["--record", arg(&t)];
    args.extend(line);
    args.push(arg(&d));
    let recorded = w.live(&args);
    assert_eq!(recorded.exit, 0, "{}", recorded.stderr);
    let got = std::fs::read(d.join("a.bin")).expect("the download");
    assert_eq!(got, bytes);

    let (header, records) = read_tape(&t);
    assert_eq!(header.mode, TapeMode::Credentialed);
    assert_eq!(header.endpoints, TapeEndpoints::Loopback);
    let hex = hex_of(&bytes);
    let sidecars = PathBuf::from(format!("{}.d", t.display()));
    let sidecar = sidecars.join(&hex);
    let mode = |p: &Path| std::fs::metadata(p).expect("there").permissions().mode() & 0o777;
    assert_eq!(mode(&sidecars), 0o700);
    assert_eq!(mode(&sidecar), 0o600);
    let body_file = format!("body-file sha256:{hex} {}", bytes.len());
    let streamed = exchanges(&records, DOOR_GH).into_iter().any(|record| {
        matches!(&record, EffectRecord::Exchange { reply, .. }
            if reply.split(|&b| b == b'\n').any(|line| line == body_file.as_bytes()))
    });
    assert!(streamed, "a reply holds {body_file}: {records:?}");

    let replay = |args: &[&str]| {
        std::fs::remove_file(d.join("a.bin")).ok();
        w.offline(args)
    };
    // A bare replay writes no file its tape names: the person types the line.
    let bare = replay(&["--replay", arg(&t)]);
    assert_eq!(bare.exit, 2, "{}", bare.stderr);
    assert!(
        why(&bare.stderr).contains("release download"),
        "{}",
        bare.stderr
    );
    assert!(!d.join("a.bin").exists(), "a bare replay wrote the asset");
    let mut again = vec!["--replay", arg(&t)];
    again.extend(line);
    again.push(arg(&d));
    let ran = replay(&again);
    assert_eq!(ran.exit, 0, "{}", ran.stderr);
    assert_eq!(ran.stdout, recorded.stdout);
    assert_eq!(std::fs::read(d.join("a.bin")).expect("replayed"), bytes);

    let mut longer = bytes.clone();
    longer.extend_from_slice(b"TRAILING");
    let mut other = bytes.clone();
    other[0] ^= 1;
    for (changed, body) in [("longer", longer), ("changed", other)] {
        std::fs::write(&sidecar, &body).expect("a body that is not the taped one");
        let ran = replay(&again);
        assert_eq!(ran.exit, 3, "{changed}: {}", ran.stderr);
        assert!(
            subs(&ran.stderr)
                .iter()
                .any(|s| s == "replay.stream_digest"),
            "{changed}: {}",
            ran.stderr
        );
    }

    std::fs::remove_file(&sidecar).expect("removed");
    let ran = replay(&again);
    assert_eq!(ran.exit, 3, "{}", ran.stderr);
    assert!(
        subs(&ran.stderr)
            .iter()
            .any(|s| s == "replay.stream_digest"),
        "{}",
        ran.stderr
    );
    w.fake.assert_clean();
}

#[test]
fn git_requests_are_canonical() {
    let slug = common::slug(APP);
    let dest = Path::new("dest-of-the-clone");
    let dir = Path::new("dir-of-the-fetch");
    let text = |run: &GitRun| String::from_utf8(canon::git_request(run)).expect("UTF-8");
    let runs = [
        (
            GitRun::new_clone(&slug, dest).with_depth(1),
            "git clone fixture-dev/app\ndepth 1".to_string(),
        ),
        (
            GitRun::ls_remote(&slug, Some("refs/tags/v1")),
            "git ls-remote fixture-dev/app\nref refs/tags/v1".to_string(),
        ),
        (
            GitRun::push_delete(&slug, "refs/tags/v1", MAIN_SHA),
            format!("git push-delete fixture-dev/app\nref refs/tags/v1\nexpect {MAIN_SHA}"),
        ),
        (
            GitRun::fetch(&slug, dir),
            "git fetch fixture-dev/app".to_string(),
        ),
    ];
    for (run, canonical) in runs {
        let got = text(&run);
        assert_eq!(got, canonical);
        assert!(!got.contains("dest-of-the-clone") && !got.contains("dir-of-the-fetch"));
    }
}

#[test]
fn route_replies_round_trip() {
    let round = |d: &Decision| canon::route_decode(&canon::route_reply(d)).expect("decodes");
    let sent = Decision::Send {
        profile: "dev".to_string(),
        tier: Tier::Dev,
    };
    assert_eq!(round(&sent), sent);
    for why in [
        AnonymousWhy::Asked,
        AnonymousWhy::ChainFellThrough,
        AnonymousWhy::NoConfig,
    ] {
        let anonymous = Decision::Anonymous { why };
        assert_eq!(round(&anonymous), anonymous);
    }
    let refusal = |presence: Presence| {
        Decision::Refuse(Refusal {
            code: RefusalCode::WriterAbsent,
            why: "the writer of fixture-dev has no credential".to_string(),
            next: "agh auth add dev --file ~/.secrets/dev --tier dev".to_string(),
            tried: vec![
                (ChainStep::Profile("dev".to_string()), Presence::Present),
                (ChainStep::Anonymous, presence),
            ],
        })
    };
    let absent = refusal(Presence::Absent);
    assert_eq!(round(&absent), absent);
    for slug in ["loose", "symlink", "malformed", "unsupported", "io"] {
        let unusable = refusal(Presence::Unusable(slug));
        assert_eq!(round(&unusable), unusable, "{slug}");
    }
    let unknown = canon::route_reply(&refusal(Presence::Unusable("loose_mode")));
    let mut junks = vec![unknown];
    junks.extend(
        [&b""[..], b"send dev", b"anonymous never", b"refuse nothing"]
            .iter()
            .map(|junk| junk.to_vec()),
    );
    for junk in &junks {
        let junk = junk.as_slice();
        let refused = canon::route_decode(junk).expect_err("not route_reply's");
        assert_eq!(
            refused.sub,
            agh::error::Sub::Replay(agh::error::Replay::Format),
            "{junk:?}"
        );
    }
}

/// `args` replayed offline as `World::offline` replays it, under the scratch root `root`
/// (no agh.toml, no token), which the caller keeps to look into after the run.
fn offline_under(w: &World, root: &Path, args: &[&str]) -> Ran {
    let transport = Arc::new(ScriptedTransport::new());
    let (mut ctx, captured) = agh::cli::Ctx::for_tests(
        root,
        transport.clone(),
        common::endpoints(&w.fake),
        common::env(),
    );
    let exit = run_cli(&mut ctx, args);
    assert!(
        transport.requests().is_empty(),
        "{args:?} reached the transport: {:?}",
        transport.requests()
    );
    Ran {
        exit,
        stdout: captured.stdout(),
        stderr: captured.stderr_text(),
    }
}

/// Every emit at `door`, in tape order.
fn emits(records: &[EffectRecord], door: &str) -> Vec<Vec<u8>> {
    records
        .iter()
        .filter_map(|record| match record {
            EffectRecord::Emit { door: d, bytes } if d == door => Some(bytes.clone()),
            _ => None,
        })
        .collect()
}

/// Every exchange at `door`, as its request and reply.
fn pairs(records: &[EffectRecord], door: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    exchanges(records, door)
        .into_iter()
        .filter_map(|record| match record {
            EffectRecord::Exchange { request, reply, .. } => Some((request, reply)),
            _ => None,
        })
        .collect()
}

/// `bytes` as UTF-8 text.
fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("UTF-8")
}

/// The journal emit `bytes` as its JSON object.
fn journal_json(bytes: &[u8]) -> aterm_json::Value {
    aterm_json::from_str(&text(bytes)).unwrap_or_else(|e| panic!("{e}: {}", text(bytes)))
}

/// The journal files under `root`'s scratch state directory.
fn journal_files(root: &Path) -> Vec<PathBuf> {
    let journal = Dirs::under(root).journal();
    let mut found = Vec::new();
    let mut dirs = vec![journal];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn a_credentialed_read_replays_with_no_agh_toml() {
    let w = World::new("tapes-m2-credentialed-read");
    let t = w.tape("t");
    let recorded = w.live(&["--record", arg(&t), "--as", "dev", "repo", "view", APP]);
    assert_eq!(recorded.exit, 0, "{}", recorded.stderr);
    let (header, records) = read_tape(&t);
    assert_eq!(header.mode, TapeMode::Credentialed);
    let tape = std::fs::read_to_string(&t).expect("the tape");
    let token = agh_fake::token("dev");
    assert!(!tape.contains(&token), "the tape holds the dev token");
    assert!(
        !tape.contains(&agh_fake::basic(&token)),
        "the tape holds the dev token's Basic form"
    );
    let routes = pairs(&records, DOOR_ROUTE);
    let (request, reply) = routes.first().expect("a route exchange");
    assert_eq!(
        text(request),
        format!("route repo {APP}\naccess read\nnamed literal {APP}\nanonymous 0\nas dev")
    );
    assert_eq!(text(reply), "send dev dev");

    let replayed = w.offline(&["--replay", arg(&t)]);
    assert_eq!(replayed.exit, 0, "{}", replayed.stderr);
    assert_eq!(replayed.stdout, recorded.stdout);
    w.fake.assert_clean();
}

#[test]
fn a_taped_write_journals_by_emit() {
    let w = World::new("tapes-m2-journal");
    let t = w.tape("t");
    let line = ["release", "create", APP, "v1", "--draft"];
    let mut args = vec!["--record", arg(&t)];
    args.extend(line);
    let recorded = w.live(&args);
    assert_eq!(recorded.exit, 0, "{}", recorded.stderr);
    let (header, records) = read_tape(&t);
    assert_eq!(header.mode, TapeMode::Credentialed);
    let journal = emits(&records, DOOR_JOURNAL);
    assert_eq!(journal.len(), 2, "{records:?}");
    let intent = journal_json(&journal[0]);
    assert_eq!(
        intent["type"],
        aterm_json::Value::from("intent"),
        "{intent}"
    );
    assert_eq!(
        intent["fingerprint"],
        aterm_json::Value::from("taped"),
        "{intent}"
    );
    let settled = journal_json(&journal[1]);
    assert_eq!(
        settled["type"],
        aterm_json::Value::from("settled"),
        "{settled}"
    );
    assert_eq!(settled["intent"], intent["id"], "{settled}");
    assert!(journal_files(w.root.path()).is_empty());

    let replay_root = common::scratch("tapes-m2-journal-replay");
    let replayed = offline_under(&w, replay_root.path(), &["--replay", arg(&t)]);
    assert_eq!(replayed.exit, 0, "{}", replayed.stderr);
    assert_eq!(replayed.stdout, recorded.stdout);
    assert!(journal_files(replay_root.path()).is_empty());

    let again = w.tape("again");
    let rerecorded = offline_under(
        &w,
        replay_root.path(),
        &["--replay", arg(&t), "--record", arg(&again)],
    );
    assert_eq!(rerecorded.exit, 0, "{}", rerecorded.stderr);
    let (_, rerecords) = read_tape(&again);
    assert_eq!(emits(&rerecords, DOOR_JOURNAL), journal);
    assert!(journal_files(replay_root.path()).is_empty());
    w.fake.assert_clean();
}

#[test]
fn an_upload_round_trips() {
    let w = World::new("tapes-m2-upload");
    w.fake
        .add_release(APP, ReleaseSpec::draft("v1", "2026-01-01T00:00:00Z"));
    let bytes = b"the bytes of a.bin, uploaded under a tape".to_vec();
    let a = common::file(w.root.path(), "a.bin", &bytes);
    let t = w.tape("t");
    let recorded = w.live(&["--record", arg(&t), "release", "upload", APP, "v1", arg(&a)]);
    assert_eq!(recorded.exit, 0, "{}", recorded.stderr);
    let (header, records) = read_tape(&t);
    assert_eq!(header.mode, TapeMode::Credentialed);
    let body = format!("body sha256:{} {}", hex_of(&bytes), bytes.len());
    let uploads: Vec<(Vec<u8>, Vec<u8>)> = pairs(&records, DOOR_GH)
        .into_iter()
        .filter(|(request, _)| text(request).lines().any(|l| l == body))
        .collect();
    assert_eq!(uploads.len(), 1, "one request holds {body}: {records:?}");
    let tape = std::fs::read(&t).expect("the tape");
    assert!(
        !tape.windows(bytes.len()).any(|window| window == bytes),
        "the tape holds the file's bytes"
    );

    let replayed = w.offline(&["--replay", arg(&t)]);
    assert_eq!(replayed.exit, 0, "{}", replayed.stderr);
    assert_eq!(replayed.stdout, recorded.stdout);
    w.fake.assert_clean();
}

#[test]
fn a_leased_delete_round_trips_through_the_git_door() {
    let w = World::new("tapes-m2-leased-delete");
    w.fake.set_tag(APP, "v1", MAIN_SHA);
    let t = w.tape("t");
    let line = [
        "ref",
        "delete",
        APP,
        "refs/tags/v1",
        "--expect",
        MAIN_SHA,
        "--confirm",
        "refs/tags/v1",
    ];
    let mut args = vec!["--record", arg(&t)];
    args.extend(line);
    let (ctx, captured) = common::ctx(w.root.path(), &w.fake, crate::git_runner::git_env());
    let mut ctx = ctx.with_git_helper(common::helper());
    let exit = run_cli(&mut ctx, &args);
    let stdout = captured.stdout();
    assert_eq!(exit, 0, "{}", captured.stderr_text());
    assert!(
        !w.fake
            .git_refs(APP)
            .iter()
            .any(|(name, _)| name == "refs/tags/v1"),
        "{:?}",
        w.fake.git_refs(APP)
    );

    let (header, records) = read_tape(&t);
    assert_eq!(header.mode, TapeMode::Credentialed);
    let git: Vec<String> = pairs(&records, DOOR_GIT)
        .iter()
        .map(|(request, _)| text(request))
        .collect();
    assert_eq!(
        git,
        [
            "git init-scratch".to_string(),
            format!("git push-delete {APP}\nref refs/tags/v1\nexpect {MAIN_SHA}"),
        ]
    );
    let scratch = records
        .iter()
        .position(|record| {
            matches!(record, EffectRecord::Exchange { door, request, .. }
                if door == DOOR_GIT && request.as_slice() == b"git init-scratch")
        })
        .expect("git init-scratch's exchange");
    let route = records[..scratch]
        .iter()
        .rposition(
            |record| matches!(record, EffectRecord::Exchange { door, .. } if door == DOOR_ROUTE),
        )
        .expect("the runner's route exchange");
    let between = &records[route + 1..scratch];
    assert!(
        matches!(between, [EffectRecord::Clock(_)]),
        "one clock record between the runner's route and git init-scratch: {between:?}"
    );

    let replayed = w.offline(&["--replay", arg(&t)]);
    assert_eq!(replayed.exit, 0, "{}", replayed.stderr);
    assert_eq!(replayed.stdout, stdout);
    w.fake.assert_clean();
}

#[test]
fn notify_is_taped_as_an_emit() {
    let w = World::new("tapes-m2-notify");
    let t = w.tape("t");
    let line = [
        "release", "create", APP, "v1", "--draft", "--notify", "--meta",
    ];
    let mut args = vec!["--record", arg(&t)];
    args.extend(line);
    let rec = Arc::new(RecordingNotifier::new());
    let (ctx, captured) = common::ctx(w.root.path(), &w.fake, common::env());
    let mut ctx = ctx.with_notifier(rec.clone());
    let exit = run_cli(&mut ctx, &args);
    let stderr = captured.stderr_text();
    assert_eq!(exit, 0, "{stderr}");
    assert!(rec.posted().is_empty(), "{:?}", rec.posted());

    let (_, records) = read_tape(&t);
    let journal = emits(&records, DOOR_JOURNAL);
    let intent = journal_json(journal.first().expect("the intent's emit"));
    let intent = intent["id"].as_str().expect("an intent id").to_string();
    let id = w
        .fake
        .releases_of(APP)
        .iter()
        .find(|release| release.tag == "v1")
        .expect("the draft")
        .id;
    let fabric = emits(&records, DOOR_FABRIC);
    assert_eq!(
        fabric,
        [format!(
            "@self post to=say:agh.write kind=note key=agh-{intent} -- v=1 repo={APP} tag=v1 \
             event=created id={id}"
        )
        .into_bytes()]
    );
    let receipt = stderr
        .lines()
        .find(|line| line.starts_with("agh: ok"))
        .unwrap_or_else(|| panic!("a receipt: {stderr}"));
    assert!(
        receipt
            .split(' ')
            .any(|word| word == "notify=skipped:taped"),
        "{receipt}"
    );

    let again = w.tape("again");
    let replay_root = common::scratch("tapes-m2-notify-replay");
    let rerecorded = offline_under(
        &w,
        replay_root.path(),
        &["--replay", arg(&t), "--record", arg(&again)],
    );
    assert_eq!(rerecorded.exit, 0, "{}", rerecorded.stderr);
    let (_, rerecords) = read_tape(&again);
    assert_eq!(emits(&rerecords, DOOR_FABRIC), fabric);
    w.fake.assert_clean();
}

/// `token` written as the vault writes `slot`'s file, under `root`'s scratch home: 0600 in a
/// 0700 `~/.secrets`.
fn vault_file(root: &Path, slot: agh::identity::vault::Slot, token: &str) {
    let path = slot.path(&Dirs::under(root).home);
    let dir = path.parent().expect("the vault directory");
    std::fs::create_dir_all(dir.parent().expect("the home")).expect("the home");
    let name = path.file_name().and_then(|n| n.to_str()).expect("a name");
    common::write_token(dir, name, token);
}

#[test]
fn a_credentialed_recording_settles_vault_adoption_off_the_tape() {
    use agh::identity::vault::Slot;

    let root = common::scratch("tapes-m2-vault");
    let fake = agh_fake::FakeGithub::start();
    fake.add_repo(APP, agh_fake::RepoSpec::public().push("fixture-dev-bot"));
    let dev = agh_fake::token("tapes-vault-dev");
    let release = agh_fake::token("tapes-vault-release");
    fake.add_token(&dev, "fixture-dev");
    fake.add_token(&release, "fixture-release-bot");
    vault_file(root.path(), Slot::Dev, &dev);
    vault_file(root.path(), Slot::Release, &release);
    assert!(!Dirs::under(root.path()).config.join("agh.toml").exists());
    let w = World { root, fake };
    let t = w.tape("t");
    let recorded = w.live(&["--record", arg(&t), "--as", "dev", "repo", "view", APP]);
    assert_eq!(recorded.exit, 0, "{}", recorded.stderr);
    let (header, records) = read_tape(&t);
    assert_eq!(header.mode, TapeMode::Credentialed);
    let routes: Vec<String> = pairs(&records, DOOR_ROUTE)
        .iter()
        .map(|(request, _)| text(request))
        .collect();
    assert!(
        routes
            .iter()
            .all(|request| !request.starts_with("route identity")),
        "adoption's login lookups are not the tape's: {routes:?}"
    );
    assert!(
        routes
            .first()
            .is_some_and(|request| request.starts_with(&format!("route repo {APP}\n"))),
        "{routes:?}"
    );
    let tape = std::fs::read_to_string(&t).expect("the tape");
    assert!(!tape.contains(&dev) && !tape.contains(&release));

    let replayed = w.offline(&["--replay", arg(&t)]);
    assert_eq!(replayed.exit, 0, "{}", replayed.stderr);
    assert_eq!(replayed.stdout, recorded.stdout);
}

#[test]
fn a_streamed_body_holding_a_token_is_not_taped() {
    let w = World::new("tapes-m2-download-token");
    let bytes = format!("TOKEN={}\n", agh_fake::token("in-an-asset")).into_bytes();
    let release = w
        .fake
        .add_release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    w.fake
        .add_asset(release, AssetSpec::uploaded("a.env", &bytes));
    let d = w.root.path().join("d");
    std::fs::create_dir(&d).expect("the download directory");
    let t = w.tape("t");
    let recorded = w.live(&[
        "--record",
        arg(&t),
        "release",
        "download",
        APP,
        "v1",
        "--asset",
        "a.env",
        "--dir",
        arg(&d),
    ]);
    assert_eq!(recorded.exit, 3, "{}", recorded.stderr);
    assert!(
        subs(&recorded.stderr)
            .iter()
            .any(|s| s == "replay.body_not_taped"),
        "{}",
        recorded.stderr
    );
    assert!(!t.exists(), "the tape is removed");
    assert!(
        !PathBuf::from(format!("{}.d", t.display())).exists(),
        "no body is left beside no tape"
    );
    assert!(!d.join("a.env").exists(), "nothing was downloaded");
}

#[test]
fn a_binary_body_holding_a_prefix_by_chance_round_trips() {
    let w = World::new("tapes-m2-download-binary");
    // High-bit bytes, as a compressed asset is, holding a token prefix and one word byte.
    let mut bytes: Vec<u8> = (0..4096u32)
        .map(|i| 0x80 | u8::try_from(i % 128).expect("under 128"))
        .collect();
    bytes.splice(2048..2048, [0xff, b'g', b'h', b's', b'_', b'7', 0xfe]);
    let release = w
        .fake
        .add_release(APP, ReleaseSpec::published("v1", "2026-01-01T00:00:00Z"));
    w.fake
        .add_asset(release, AssetSpec::uploaded("a.tar.gz", &bytes));
    let d = w.root.path().join("d");
    std::fs::create_dir(&d).expect("the download directory");
    let t = w.tape("t");
    let mut line = vec![
        "release", "download", APP, "v1", "--asset", "a.tar.gz", "--dir",
    ];
    line.push(arg(&d));
    let mut args = vec!["--record", arg(&t)];
    args.extend(line.iter().copied());
    let recorded = w.live(&args);
    assert_eq!(recorded.exit, 0, "{}", recorded.stderr);
    let sidecar = PathBuf::from(format!("{}.d", t.display())).join(hex_of(&bytes));
    assert_eq!(std::fs::read(&sidecar).expect("the sidecar"), bytes);

    std::fs::remove_file(d.join("a.tar.gz")).expect("the download removed");
    let mut again = vec!["--replay", arg(&t)];
    again.extend(line.iter().copied());
    let replayed = w.offline(&again);
    assert_eq!(replayed.exit, 0, "{}", replayed.stderr);
    assert_eq!(std::fs::read(d.join("a.tar.gz")).expect("replayed"), bytes);
    w.fake.assert_clean();
}

/// A world with the two vault files and no agh.toml: dev's token speaks as `fixture-dev`,
/// release's as `fixture-release-bot`.
fn vault_world(label: &str) -> World {
    use agh::identity::vault::Slot;

    let root = common::scratch(label);
    let fake = agh_fake::FakeGithub::start();
    fake.add_repo(APP, agh_fake::RepoSpec::public().push("fixture-dev-bot"));
    let dev = agh_fake::token(&format!("{label}-dev"));
    let release = agh_fake::token(&format!("{label}-release"));
    fake.add_token(&dev, "fixture-dev");
    fake.add_token(&release, "fixture-release-bot");
    vault_file(root.path(), Slot::Dev, &dev);
    vault_file(root.path(), Slot::Release, &release);
    assert!(!Dirs::under(root.path()).config.join("agh.toml").exists());
    World { root, fake }
}

#[test]
fn a_credentialed_recording_refuses_a_vault_token_rotated_to_another_login() {
    use agh::identity::vault::Slot;

    let w = vault_world("tapes-m2-vault-moved");
    let learned = w.live(&["whoami"]);
    assert_eq!(learned.exit, 0, "{}", learned.stderr);
    let other = agh_fake::token("tapes-m2-vault-other");
    w.fake.add_token(&other, "fixture-other");
    vault_file(w.root.path(), Slot::Dev, &other);
    let t = w.tape("t");
    for line in [
        &["--as", "release", "api", "/user"][..],
        &["release", "create", APP, "v9", "--draft"][..],
    ] {
        let live = w.live(line);
        assert_eq!(live.exit, 1, "{line:?} live: {}", live.stderr);
        let before = w.fake.seen().len();
        let mut args = vec!["--record", arg(&t)];
        args.extend(line.iter().copied());
        let recorded = w.live(&args);
        assert_eq!(recorded.exit, 1, "{line:?}: {}", recorded.stderr);
        let error = common::error_json(&recorded.stderr);
        assert_eq!(error["kind"], aterm_json::Value::from("refused"), "{error}");
        assert!(why(&recorded.stderr).contains("fixture-other"), "{error}");
        let sent: Vec<agh_fake::Seen> = w.fake.seen().split_off(before);
        assert!(
            sent.iter()
                .all(|seen| seen.as_login.as_deref() == Some("fixture-other")),
            "{line:?}: only the rotated token's login lookup is sent: {sent:?}"
        );
        assert!(!t.exists(), "{line:?}: a refused recording makes no tape");
    }
    assert!(w.fake.releases_of(APP).is_empty());
}

#[test]
fn a_credentialed_recording_keeps_its_timeout() {
    use agh::wire::Deadlines;
    use std::time::Duration;

    // The recording's client is built before `run_verb` sets `--timeout`; each request its
    // door is handed carries the backstop it was built with. `release wait` takes the timeout
    // as its whole wait, so its client has none of its own. Read off the door, with no clock.
    let w = World::new("tapes-m2-timeout");
    for (n, (line, backstop)) in [
        (
            &["--timeout", "1s", "repo", "view", APP][..],
            Duration::from_secs(1),
        ),
        (&["repo", "view", APP][..], Deadlines::JSON.backstop),
        (
            &["--timeout", "1s", "release", "wait", APP, "v1", "published"][..],
            Deadlines::JSON.backstop,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let t = w.tape(&format!("t{n}"));
        let transport = Arc::new(ScriptedTransport::new());
        transport.answer(401, &[], br#"{"message":"Bad credentials"}"#);
        let (mut ctx, captured) = agh::cli::Ctx::for_tests(
            w.root.path(),
            transport.clone(),
            common::endpoints(&w.fake),
            common::env(),
        );
        let mut args = vec!["--record", arg(&t), "--as", "dev"];
        args.extend(line.iter().copied());
        run_cli(&mut ctx, &args);
        let sent = ctx.fx.requested_deadlines();
        assert!(!sent.is_empty(), "{line:?}: {}", captured.stderr_text());
        assert!(
            sent.iter().all(|deadlines| deadlines.backstop == backstop),
            "{line:?}: {sent:?}"
        );
        assert!(
            transport
                .requests()
                .first()
                .is_some_and(|seen| seen.url.contains(APP)),
            "{line:?}: {:?}",
            transport.requests()
        );
    }
}

#[test]
fn a_command_line_holding_a_learned_credential_is_not_taped() {
    use agh::identity::vault::Slot;

    // A legacy classic token is 40 hex digits: no token shape, so only the learned credential
    // finds it in the command line.
    let legacy = |seed: &str| hex_of(seed.as_bytes())[..40].to_string();

    // agh.toml's profile: learned when the verb first speaks as it, after the header is on
    // disk, so `finish` refuses the tape and removes it.
    let w = World::new("tapes-m2-header-credential");
    let dev = legacy("tapes-m2-header-dev");
    w.fake.add_token(&dev, "fixture-dev-bot");
    common::write_token(&w.root.path().join("tokens"), "dev.token", &dev);
    let t = w.tape("t");
    let description = format!("description={dev}");
    let recorded = w.live(&[
        "--record",
        arg(&t),
        "--as",
        "dev",
        "api",
        "-X",
        "PATCH",
        &format!("repos/{APP}"),
        "-f",
        &description,
    ]);
    assert_eq!(recorded.exit, 3, "{}", recorded.stderr);
    assert!(!recorded.stderr.contains(&dev), "{}", recorded.stderr);
    assert!(!t.exists(), "the tape is removed");

    // A vault token: learned when adoption settles, before the tape exists, so no file is made
    // and nothing the line names is sent.
    let w = vault_world("tapes-m2-header-vault");
    let vaulted = legacy("tapes-m2-header-vault");
    w.fake.add_token(&vaulted, "fixture-dev");
    vault_file(w.root.path(), Slot::Dev, &vaulted);
    let t = w.tape("t");
    let description = format!("description={vaulted}");
    let before = w.fake.seen().len();
    let recorded = w.live(&[
        "--record",
        arg(&t),
        "--as",
        "dev",
        "api",
        "-X",
        "PATCH",
        &format!("repos/{APP}"),
        "-f",
        &description,
    ]);
    assert_eq!(recorded.exit, 3, "{}", recorded.stderr);
    assert_eq!(subs(&recorded.stderr), ["local"], "{}", recorded.stderr);
    assert!(!recorded.stderr.contains(&vaulted), "{}", recorded.stderr);
    assert!(!t.exists(), "no tape is made");
    assert!(
        w.fake
            .seen()
            .split_off(before)
            .iter()
            .all(|seen| seen.method == "GET"),
        "{:?}",
        w.fake.seen()
    );
}

/// A world whose agh.toml is the canonical one through `edit`, with the canonical tokens
/// (dev's file when `dev_file`).
fn world_with(label: &str, edit: &dyn Fn(String) -> String, dev_file: bool) -> World {
    let root = common::scratch(label);
    let fake = agh_fake::FakeGithub::start();
    common::canonical_tokens(root.path(), &fake, dev_file, true);
    let config = edit(common::canonical_config(root.path()));
    common::write_config(&Dirs::under(root.path()), &config);
    fake.add_repo(APP, agh_fake::RepoSpec::public().push("fixture-dev-bot"));
    World { root, fake }
}

/// The tape `t` of `recorded`, replayed offline, meets the recording's exit, stdout and
/// error subs, and no `replay.*` sub.
fn round_trips(w: &World, recorded: &Ran, t: &Path) {
    let replayed = w.offline(&["--replay", arg(t)]);
    assert!(
        subs(&replayed.stderr)
            .iter()
            .all(|s| !s.starts_with("replay.")),
        "recorded {}: {}\nreplayed {}: {}",
        recorded.exit,
        recorded.stderr,
        replayed.exit,
        replayed.stderr
    );
    assert_eq!(replayed.exit, recorded.exit, "{}", replayed.stderr);
    assert_eq!(replayed.stdout, recorded.stdout);
    assert_eq!(subs(&replayed.stderr), subs(&recorded.stderr));
}

#[test]
fn foreign_a_covers_all_repos_404_round_trips() {
    let w = world_with(
        "tapes-m2-covers",
        &|config| {
            config.replacen(
                "login = \"fixture-dev-bot\"\n",
                "login = \"fixture-dev-bot\"\ncovers_all_repos = true\n",
                1,
            )
        },
        true,
    );
    // A missing path, and a missing repository, which the flag makes NotFound and not
    // NotVisible.
    for (label, line) in [
        (
            "t",
            &["--as", "dev", "cat", "fixture-dev/app:missing.txt"][..],
        ),
        (
            "gone",
            &["--as", "dev", "repo", "view", "fixture-dev/gone"][..],
        ),
    ] {
        let t = w.tape(label);
        let untaped = w.live(line);
        let mut args = vec!["--record", arg(&t)];
        args.extend(line);
        let recorded = w.live(&args);
        assert_ne!(recorded.exit, 0, "{}", recorded.stderr);
        assert_eq!(
            (recorded.exit, subs(&recorded.stderr)),
            (untaped.exit, subs(&untaped.stderr)),
            "a recording answers as the run untaped: {}",
            recorded.stderr
        );
        round_trips(&w, &recorded, &t);
    }
}

#[test]
fn foreign_own_login_repo_list_round_trips() {
    let w = World::new("tapes-m2-own-login");
    w.fake.add_repo(
        "fixture-dev-bot/kept",
        agh_fake::RepoSpec::private().push("fixture-dev-bot"),
    );
    let t = w.tape("t");
    let line = [
        "--as",
        "dev",
        "repo",
        "list",
        "fixture-dev-bot",
        "--type",
        "private",
    ];
    let untaped = w.live(&line);
    assert_eq!(untaped.exit, 0, "{}", untaped.stderr);
    let listed = String::from_utf8_lossy(&untaped.stdout);
    assert!(listed.contains("fixture-dev-bot/kept"), "{listed}");
    let mut args = vec!["--record", arg(&t)];
    args.extend(line);
    let recorded = w.live(&args);
    assert_eq!(recorded.exit, 0, "{}", recorded.stderr);
    assert_eq!(
        recorded.stdout, untaped.stdout,
        "a recording lists as the run untaped"
    );
    round_trips(&w, &recorded, &t);
}

#[test]
fn foreign_drafts_unproven_round_trips() {
    let w = world_with("tapes-m2-drafts", &|config| config, false);
    let token = agh_fake::fine_grained_token("dev");
    w.fake.add_token(&token, "fixture-dev-bot");
    common::write_token(&w.root.path().join("tokens"), "dev.token", &token);
    let t = w.tape("t");
    let recorded = w.live(&[
        "--record",
        arg(&t),
        "release",
        "create",
        APP,
        "v1",
        "--draft",
    ]);
    assert!(
        subs(&recorded.stderr)
            .iter()
            .any(|s| s == "forbid.drafts_unproven"),
        "{}",
        recorded.stderr
    );
    round_trips(&w, &recorded, &t);
}

/// A profile that names a vault token file is told to re-add it with `--drafts`, or, where no
/// lane reads agh.toml, to remove it (design §6.10); its replay, whose router is empty, gives
/// the recording's step, read off the drafts fact.
#[test]
fn a_vault_profiles_drafts_step_round_trips() {
    let w = world_with(
        "tapes-m2-drafts-vault",
        &|config| config.replace("/tokens/dev.token\"", "/.secrets/gh_access_token\""),
        false,
    );
    let token = agh_fake::fine_grained_token("dev");
    w.fake.add_token(&token, "fixture-dev-bot");
    common::write_token(&w.root.path().join(".secrets"), "gh_access_token", &token);
    let t = w.tape("t");
    let recorded = w.live(&[
        "--record",
        arg(&t),
        "release",
        "create",
        APP,
        "v1",
        "--draft",
    ]);
    let next = |stderr: &str| {
        stderr
            .lines()
            .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
            .find_map(|line| line.get("error")?.get("next")?.as_str().map(str::to_string))
            .unwrap_or_else(|| panic!("an error line with a next: {stderr}"))
    };
    assert!(
        subs(&recorded.stderr)
            .iter()
            .any(|s| s == "forbid.drafts_unproven"),
        "{}",
        recorded.stderr
    );
    // The `--drafts` re-add comes first: a builder lanes' agh.toml names the vault files too,
    // and removing its profiles makes it say `vault = true`, which the lanes refuse. The
    // removal is the alternative for a file no lane reads, its names off the tape.
    let step = next(&recorded.stderr);
    assert!(
        step.starts_with("agh auth add dev --file PATH ") && step.contains(" --drafts;"),
        "{step}"
    );
    assert!(step.contains("agh auth remove dev"), "{step}");
    round_trips(&w, &recorded, &t);
    let replayed = w.offline(&["--replay", arg(&t)]);
    assert_eq!(next(&replayed.stderr), next(&recorded.stderr));
}

#[test]
fn foreign_an_env_profiles_git_run_round_trips() {
    let w = world_with(
        "tapes-m2-env-git",
        &|config| {
            config
                .lines()
                .map(|line| {
                    if line.starts_with("token_file") && line.ends_with("/tokens/dev.token\"") {
                        "env = \"AGH_TAPES_DEV\"".to_string()
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        },
        false,
    );
    w.fake.set_tag(APP, "v1", MAIN_SHA);
    let t = w.tape("t");
    let mut args = vec!["--record", arg(&t)];
    args.extend([
        "ref",
        "delete",
        APP,
        "refs/tags/v1",
        "--expect",
        MAIN_SHA,
        "--confirm",
        "refs/tags/v1",
    ]);
    let env = crate::git_runner::git_env().with("AGH_TAPES_DEV", &agh_fake::token("dev"));
    let (ctx, captured) = common::ctx(w.root.path(), &w.fake, env);
    let mut ctx = ctx.with_git_helper(common::helper());
    let exit = run_cli(&mut ctx, &args);
    let recorded = Ran {
        exit,
        stdout: captured.stdout(),
        stderr: captured.stderr_text(),
    };
    assert_eq!(recorded.exit, 1, "{}", recorded.stderr);
    assert!(
        why(&recorded.stderr).contains("token file"),
        "{}",
        recorded.stderr
    );
    round_trips(&w, &recorded, &t);
}
