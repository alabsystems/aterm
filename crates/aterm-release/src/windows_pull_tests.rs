// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::buildplan::windows::{Attester, Handoff};
use crate::ledger::GitCli;
use aterm_winship::attest;
use aterm_winship::handoff::{Lane, Receipt, fixture};
use std::fs;
use std::process::Output;

const VERSION: &str = "0.100.0";
const BUILD: u64 = 100;
const BUILDER: &str = "win-builder-1";
/// 2026-11-15T00:00:00Z: the clock every decision here is read at.
const NOW: u64 = 1_794_700_800;
/// Unused by a stock-lane receipt; `fixture::receipt` names it for the Trust lane.
const TRUST_COMMIT: &str = "29ebb403de7a102850d6cb10a62d5cc3baf723ed";

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("aterm-windows-pull-")
        .tempdir()
        .unwrap()
}

/// `git -C dir args`, hermetic, under a fixed identity; at `when` (epoch seconds), if given.
fn git_at(dir: &Path, args: &[&str], when: Option<u64>) -> Output {
    let mut git = aterm_uds::fixture_git::git();
    git.args([
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "uploadpack.allowAnySHA1InWant=true",
        "-C",
    ])
    .arg(dir)
    .args(args);
    if let Some(when) = when {
        let date = format!("@{when} +0000");
        git.env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date);
    }
    git.output().unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_at(dir, args, None);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Origin, a bare repository, as the pull lane reads and writes it, and the release clone that
/// fetches from it.
struct Bare {
    origin: PathBuf,
    clone: PathBuf,
}

impl Origin for Bare {
    fn get(&self, reference: &str) -> Result<Option<String>, String> {
        let oid = git(
            &self.origin,
            &["for-each-ref", "--format=%(objectname)", reference],
        );
        Ok((!oid.is_empty()).then_some(oid))
    }

    fn fetch(&self, oid: &str) -> Result<(), String> {
        let origin = self.origin.to_str().unwrap();
        let out = git_at(&self.clone, &["fetch", "-q", origin, oid], None);
        out.status
            .success()
            .then_some(())
            .ok_or_else(|| String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn write(&self, reference: &str, before: Option<&str>, after: &str) -> Result<bool, String> {
        let lease = format!("--force-with-lease={reference}:{}", before.unwrap_or(""));
        let target = format!("{after}:{reference}");
        let origin = self.origin.to_str().unwrap();
        let out = git_at(&self.clone, &["push", "-q", &lease, origin, &target], None);
        if out.status.success() {
            return Ok(true);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("stale info") {
            Ok(false)
        } else {
            Err(stderr.into_owned())
        }
    }

    fn delete(&self, reference: &str, before: &str) -> Result<bool, String> {
        Ok(
            git_at(&self.origin, &["update-ref", "-d", reference, before], None)
                .status
                .success(),
        )
    }
}

/// A builder's key and the pin of it, as a release commit's builders file says it.
struct Key {
    pkcs8: Vec<u8>,
    public_key: String,
}

impl Key {
    fn new() -> Self {
        let pkcs8 = attest::generate().unwrap();
        let public_key = attest::public_key(&pkcs8).unwrap();
        Self { pkcs8, public_key }
    }

    fn pin(&self, id: &str, not_after: &str) -> String {
        format!(
            "[[builder]]\nid = \"{id}\"\npublic_key = \"{}\"\nnot_after = \"{not_after}\"\n",
            self.public_key
        )
    }
}

/// A release clone and its origin, with the release commit C made in the clone.
struct World {
    dir: aterm_tempfile::TempDir,
    bare: Bare,
    key: Key,
    commit: String,
    release: PathBuf,
}

impl World {
    /// C pins `builders` (no builders file when `None`).
    fn with(builders: Option<&str>) -> Self {
        let dir = scratch();
        let origin = dir.path().join("origin.git");
        let clone = dir.path().join("clone");
        fs::create_dir_all(&clone).unwrap();
        git(dir.path(), &["init", "-q", "--bare", "origin.git"]);
        git(&clone, &["init", "-q"]);
        for (key, value) in [
            ("user.name", "Release"),
            ("user.email", "release@example.invalid"),
            ("commit.gpgsign", "false"),
        ] {
            git(&clone, &["config", key, value]);
        }
        let pin = clone.join(aterm_winship::handoff::STOCK_PIN_FILE);
        fs::create_dir_all(pin.parent().unwrap()).unwrap();
        fs::write(&pin, "pub const STOCK_PIN: &str = \"1.97.1\";\n").unwrap();
        if let Some(builders) = builders {
            let file = clone.join(pull::BUILDERS_FILE);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, builders).unwrap();
        }
        git(&clone, &["add", "."]);
        git(&clone, &["commit", "-qm", "the release commit"]);
        let commit = git(&clone, &["rev-parse", "HEAD"]);
        let release = dir.path().join("aterm-release");
        fs::create_dir_all(&release).unwrap();
        Self {
            dir,
            bare: Bare { origin, clone },
            key: Key::new(),
            commit,
            release,
        }
    }

    /// C pins this world's builder, admitted through 2099.
    fn new() -> Self {
        let key = Key::new();
        let mut world = Self::with(Some(&key.pin(BUILDER, "2099-12-31")));
        world.key = key;
        world
    }

    fn runner(&self) -> GitCli {
        GitCli::new(&self.bare.clone)
    }

    fn ask(&self) -> Ask {
        Ask {
            version: VERSION.to_string(),
            build: BUILD,
            commit: self.commit.clone(),
        }
    }

    fn sources(&self) -> Sources {
        Sources::at(&self.runner(), &self.commit).unwrap()
    }

    /// The receipt a Windows build of C makes of `zip`.
    fn receipt(&self, zip: &[u8]) -> Receipt {
        let sources = self.sources();
        let cut = Cut {
            version: VERSION,
            build: BUILD,
            commit: &self.commit,
            source_tree: &sources.tree,
        };
        fixture::receipt(zip, &cut, Lane::Stock, TRUST_COMMIT)
    }

    /// A pair of C attested by `pkcs8` as `builder`, split as `release-serve` pushes it, in a
    /// fresh directory: its path and the zip's name. `tamper` runs on the split directory.
    fn handoff_files(&self, pkcs8: &[u8], builder: &str, tamper: impl FnOnce(&Path)) -> PathBuf {
        let zip = fixture::portable();
        let receipt = self.receipt(&zip);
        let carry = self.dir.path().join(format!("carry-{}", rand_name()));
        let parts = self.dir.path().join(format!("parts-{}", rand_name()));
        fs::create_dir_all(&carry).unwrap();
        fs::create_dir_all(&parts).unwrap();
        fixture::carry(&carry, &zip, &receipt).unwrap();
        attest::write_beside(&carry, &receipt.asset, pkcs8, builder).unwrap();
        pull::split(&carry.join(&receipt.asset), &parts, pull::PART_MAX_BYTES).unwrap();
        for name in [
            aterm_winship::handoff::receipt_name(&receipt.asset),
            pull::attestation_name(&receipt.asset),
        ] {
            fs::copy(carry.join(&name), parts.join(&name)).unwrap();
        }
        tamper(&parts);
        parts
    }

    /// A parentless commit in origin holding each file of `files` at its name; at `when`.
    fn origin_commit(&self, files: &Path, when: u64) -> String {
        let origin = &self.bare.origin;
        let mut names: Vec<_> = fs::read_dir(files)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        let mut tree = String::new();
        for name in names {
            let path = files.join(&name);
            let blob = git(origin, &["hash-object", "-w", path.to_str().unwrap()]);
            tree.push_str(&format!("100644 blob {blob}\t{name}\0"));
        }
        let mktree = GitCli::new(origin)
            .git_input(&["mktree", "-z"], tree.as_bytes())
            .unwrap()
            .stdout_utf8()
            .trim()
            .to_string();
        let out = git_at(
            origin,
            &["commit-tree", &mktree, "-m", "pushed by release-serve"],
            Some(when),
        );
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Origin's handoff ref for C, its files those of `files`.
    fn hand_off(&self, files: &Path) {
        let commit = self.origin_commit(files, NOW);
        git(
            &self.bare.origin,
            &["update-ref", &self.ask().handoff_ref(), &commit],
        );
    }

    /// Origin's heartbeat: `builder` at `at`, committed at `committed`.
    fn heartbeat(&self, at: u64, committed: u64) {
        self.heartbeat_text(&alive(BUILDER, at).to_toml().unwrap(), committed);
    }

    /// Origin's heartbeat commit holding `text` as its record, committed at `committed`.
    fn heartbeat_text(&self, text: &str, committed: u64) {
        let dir = self.dir.path().join(format!("alive-{}", rand_name()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(pull::ALIVE_FILE), text).unwrap();
        let commit = self.origin_commit(&dir, committed);
        git(&self.bare.origin, &["update-ref", pull::ALIVE_REF, &commit]);
    }

    /// What origin holds for C's pull, judged on [`NOW`]'s day, put together under the release
    /// directory.
    fn look(&self) -> Found {
        self.look_on(&pull::date_of(NOW))
    }

    /// What origin holds for C's pull, judged on `day`.
    fn look_on(&self, day: &str) -> Found {
        let ask = self.ask();
        look(
            &self.bare,
            &self.runner(),
            &ask,
            &ask.directory(&self.release),
            day,
        )
        .unwrap()
    }

    fn refused(&self) -> Refused {
        match self.look() {
            Found::Handoff(Err(refused)) => refused,
            other => panic!("{other:?}"),
        }
    }
}

/// A name no other fixture directory of this world takes.
fn rand_name() -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed).to_string()
}

fn requested() -> Pull {
    Pull {
        request: Some("c".repeat(40)),
        ..Pull::new(BUILD, &"a".repeat(40), NOW - 600)
    }
}

const ON: Config = Config {
    on: true,
    wait: WAIT_SECS,
    alive: ALIVE_SECS,
};

/// `builder`'s heartbeat record at `at`, of no request.
fn alive(builder: &str, at: u64) -> Alive {
    Alive {
        builder: builder.to_string(),
        at: aterm_winship::pointer::rfc3339(at),
        request: None,
        passed_over: None,
    }
}

fn beat(at: u64, committed: u64) -> Heartbeat {
    Heartbeat {
        alive: alive(BUILDER, at),
        committed,
        pinned: Ok(()),
    }
}

fn absent(alive: Result<Option<Heartbeat>, Unread>) -> Result<Found, String> {
    Ok(Found::Absent(alive))
}

fn unread(why: &str, transient: bool) -> Unread {
    Unread {
        why: why.to_string(),
        transient,
    }
}

/// THE CONFIGURATION IS THE RELEASE COMMIT'S `publish/config.sh`: its flat lines turn the lane
/// on and bound its wait and heartbeat; a missing or unreadable number is the default, and a
/// file that does not say `"1"` leaves the lane off.
/// NEGATIVE CONTROL: a lane read on by default asked a builder of every release whose commit
/// predates it.
#[test]
fn the_lane_is_what_the_release_commits_config_says() {
    assert_eq!(Config::read(""), Config::OFF);
    let config = Config::read(
        "PUMP_PUBLISH_DEFAULT=\"1\"\nWINDOWS_PULL_DEFAULT=\"1\"\n\
         WINDOWS_PULL_WAIT_DEFAULT=\"600\"\nWINDOWS_PULL_ALIVE_DEFAULT=\"120\"\n",
    );
    assert_eq!(
        config,
        Config {
            on: true,
            wait: 600,
            alive: 120
        }
    );
    for off in [
        "WINDOWS_PULL_DEFAULT=\"0\"",
        "WINDOWS_PULL_DEFAULT=\"\"",
        "# WINDOWS_PULL_DEFAULT=\"1\"",
    ] {
        assert!(!Config::read(off).on, "{off}");
    }
    let defaults = Config::read(
        "WINDOWS_PULL_DEFAULT=\"1\"\nWINDOWS_PULL_WAIT_DEFAULT=\"soon\"\n\
         WINDOWS_PULL_ALIVE_DEFAULT=\"-5\"\n",
    );
    assert_eq!((defaults.wait, defaults.alive), (WAIT_SECS, ALIVE_SECS));
}

/// A FRESH HEARTBEAT WAITS UNTIL THE WAIT FROM THE REQUEST RUNS OUT, AND NO LONGER: with no
/// handoff on origin, a builder whose heartbeat (its record's time and its commit's, either
/// way) is within the bound makes the run wait until the request plus the wait; a stale one, a
/// clock that runs ahead past the bound, none at all, one that is no heartbeat, a request never
/// written, or a wait run out goes on without a Windows zip, never loudly.
/// NEGATIVE CONTROL: a heartbeat read by its record alone kept a release waiting on a builder
/// whose pushes stopped while its clock ran on.
#[test]
fn a_fresh_heartbeat_waits_until_the_wait_runs_out_and_no_longer() {
    let pull = requested();
    let fresh = beat(NOW - 60, NOW - 60);
    assert_eq!(
        decide(absent(Ok(Some(fresh.clone()))), NOW, &pull, &ON),
        Decision::Wait {
            until: pull.requested_at + WAIT_SECS,
            why: decide_why(&pull, &fresh)
        }
    );
    let skipped = |found, now, pull: &Pull| match decide(found, now, pull, &ON) {
        Decision::Skip { why, loud: false } => why,
        other => panic!("{other:?}"),
    };
    let why = skipped(absent(Ok(Some(beat(NOW - 4000, NOW - 4000)))), NOW, &pull);
    assert!(
        why.contains("not within 1800 s") && why.contains(BUILDER),
        "{why}"
    );
    // Its record fresh, its commit not (or a clock far ahead): no builder is serving.
    skipped(absent(Ok(Some(beat(NOW - 60, NOW - 4000)))), NOW, &pull);
    skipped(absent(Ok(Some(beat(NOW + 4000, NOW + 4000)))), NOW, &pull);
    assert!(skipped(absent(Ok(None)), NOW, &pull).contains("no heartbeat"));
    assert!(skipped(absent(Err(unread("no record", false))), NOW, &pull).contains("no record"));
    let unrequested = Pull {
        request: None,
        unrequested: Some("push refused".into()),
        ..pull.clone()
    };
    assert!(skipped(absent(Ok(Some(fresh.clone()))), NOW, &unrequested).contains("push refused"));
    // The wait from the request: up to its last second the run waits, then goes on.
    let deadline = pull.requested_at + WAIT_SECS;
    let at = |now: u64| beat(now - 60, now - 60);
    assert!(matches!(
        decide(absent(Ok(Some(at(deadline - 1)))), deadline - 1, &pull, &ON),
        Decision::Wait { .. }
    ));
    let why = skipped(absent(Ok(Some(at(deadline)))), deadline, &pull);
    assert!(
        why.contains(WAIT_KEY) && why.contains("still serving"),
        "{why}"
    );
}

fn decide_why(pull: &Pull, beat: &Heartbeat) -> String {
    format!(
        "builder {} is serving (heartbeat {} s ago) and has not handed off build {} yet, {} s \
         after the request",
        beat.alive.builder,
        beat.age(NOW),
        pull.build,
        NOW - pull.requested_at
    )
}

/// A HANDOFF IS DECIDED BY ITS ADMISSION ALONE: admitted, it is declared whatever the
/// heartbeat; refused, the release goes on without it, loudly only for a signature that does
/// not verify.
#[test]
fn a_handoff_is_decided_by_its_admission() {
    let pull = requested();
    let refused = |why: &str, bad_signature| {
        Ok(Found::Handoff(Err(Refused {
            why: why.to_string(),
            bad_signature,
            transient: false,
        })))
    };
    assert_eq!(
        decide(refused("BAD SIGNATURE: forged", true), NOW, &pull, &ON),
        Decision::Skip {
            why: "the handoff on origin is refused: BAD SIGNATURE: forged".into(),
            loud: true
        }
    );
    assert!(matches!(
        decide(refused("a part is missing", false), NOW, &pull, &ON),
        Decision::Skip { loud: false, .. }
    ));
}

/// THE REQUEST IS A COMMIT OVER THE RELEASE COMMIT: origin's request ref names a commit whose
/// one parent is C and whose tree is the request file alone, naming the version, build, C and
/// C's tree; asking again for the same reservation changes nothing, and a new reservation's
/// request replaces it (deleted as it was read, then written: a ref moves to no unrelated
/// commit by a plain write).
/// NEGATIVE CONTROL: a request rewritten in place was refused by the native push as a
/// non-fast-forward, and the builder kept building the withdrawn reservation.
#[test]
fn the_request_is_a_commit_over_the_release_commit() {
    let w = World::new();
    let runner = w.runner();
    let oid = write_request(&w.bare, &runner, VERSION, BUILD, &w.commit, NOW).unwrap();
    assert_eq!(w.bare.get(pull::REQUEST_REF).unwrap(), Some(oid.clone()));
    let origin = &w.bare.origin;
    assert_eq!(
        git(origin, &["rev-list", "--parents", "-n", "1", &oid]),
        format!("{oid} {}", w.commit),
        "one parent, C, which the push carried to origin"
    );
    assert_eq!(
        git(origin, &["ls-tree", "--name-only", &oid]),
        pull::REQUEST_FILE
    );
    let request = Request::parse(&git(
        origin,
        &["show", &format!("{oid}:{}", pull::REQUEST_FILE)],
    ))
    .unwrap();
    assert_eq!(
        request,
        Request {
            version: VERSION.to_string(),
            build: BUILD,
            commit: w.commit.clone(),
            tree: git(&w.bare.clone, &["rev-parse", "HEAD^{tree}"]),
            reserved_at: aterm_winship::pointer::rfc3339(NOW),
        }
    );
    replace_request(&w.bare, &oid).unwrap();
    assert_eq!(w.bare.get(pull::REQUEST_REF).unwrap(), Some(oid.clone()));
    let again = write_request(&w.bare, &runner, VERSION, BUILD + 1, &w.commit, NOW + 1).unwrap();
    assert_ne!(again, oid);
    assert_eq!(w.bare.get(pull::REQUEST_REF).unwrap(), Some(again));
    // A request is a release's: a version that is none is refused before anything is written.
    assert!(write_request(&w.bare, &runner, "next", BUILD, &w.commit, NOW).is_err());
}

/// A HANDOFF ATTESTED BY A PINNED BUILDER IS ADMITTED, AND THE CUT ADMITS IT THE SAME: the
/// handoff ref's parts put back together in a private directory, the pair held to C's pins and
/// to the attestation of the builder C pins; the directory then holds exactly the zip, its
/// receipt and the attestation, and the sign step's own admission (`--windows-attested`) takes
/// it with no digest typed, freezing the builder with the zip.
/// NEGATIVE CONTROL: a pulled pair admitted by its own receipt alone shipped whatever a writer
/// of origin's refs pushed.
#[test]
fn a_handoff_attested_by_a_pinned_builder_is_admitted() {
    let w = World::new();
    w.hand_off(&w.handoff_files(&w.key.pkcs8, BUILDER, |_| {}));
    w.heartbeat(NOW - 4000, NOW - 4000);
    let pulled = match w.look() {
        Found::Handoff(Ok(pulled)) => pulled,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        (pulled.builder.as_str(), pulled.public_key.as_str()),
        (BUILDER, w.key.public_key.as_str())
    );
    let asset = pulled.asset.clone();
    let mut held: Vec<_> = fs::read_dir(&pulled.directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    held.sort();
    let mut want = vec![
        asset.clone(),
        pull::attestation_name(&asset),
        aterm_winship::handoff::receipt_name(&asset),
    ];
    want.sort();
    assert_eq!(held, want, "the pair and its attestation, nothing else");
    assert_eq!(
        fs::read(pulled.directory.join(&asset)).unwrap(),
        fixture::portable()
    );
    // Admitted whatever the heartbeat says (stale here): the handoff is there.
    assert_eq!(
        decide(Ok(w.look()), NOW, &requested(), &ON),
        Decision::Admit(match w.look() {
            Found::Handoff(Ok(pulled)) => pulled,
            other => panic!("{other:?}"),
        })
    );
    // The cut's admission of the declared directory.
    let dist = scratch();
    let mut handoff = Handoff::declared(&pulled.directory, None, true).unwrap();
    handoff
        .import(dist.path(), VERSION, BUILD, &w.commit, &w.sources())
        .unwrap();
    assert_eq!(
        handoff.attester,
        Some(Attester {
            builder: BUILDER.to_string(),
            public_key: w.key.public_key.clone(),
        })
    );
    assert!(dist.path().join(&asset).exists(), "the zip is staged");
    handoff
        .validate_identity(VERSION, BUILD, &w.commit, true)
        .unwrap();
    // A journal that froze an attested zip and lost its attester is refused.
    let mut lost = handoff.clone();
    lost.attester = None;
    assert!(
        lost.validate_identity(VERSION, BUILD, &w.commit, true)
            .unwrap_err()
            .contains("builder")
    );
    // And the same directory declared without an anchor is no pair at all to the cut.
    let mut bare = Handoff::declared(&pulled.directory, None, false).unwrap();
    assert!(
        bare.import(scratch().path(), VERSION, BUILD, &w.commit, &w.sources())
            .is_err()
    );
}

/// EVERY WAY A HANDOFF FAILS LEAVES THE RELEASE MAC-ONLY, NEVER REFUSED: a part missing, a
/// part that is not what the table says, a table whose digest is another, a signature by
/// another key (loud: BAD SIGNATURE), a builder C does not pin, one whose pin has expired, and
/// a C with no builders file. Each is a refused handoff, which step 7 decides to go on
/// without.
/// NEGATIVE CONTROL: a forged or broken handoff stopped the pump's release, or shipped.
#[test]
fn every_way_a_handoff_fails_leaves_the_release_mac_only() {
    let w = World::new();
    let asset = aterm_winship::handoff::TARGET.asset_name(VERSION);
    let part = pull::part_name(&asset, 0);
    let refused_with = |w: &World, files: PathBuf| {
        let _ = git_at(
            &w.bare.origin,
            &["update-ref", "-d", &w.ask().handoff_ref()],
            None,
        );
        w.hand_off(&files);
        let refused = w.refused();
        assert!(matches!(
            decide(
                Ok(Found::Handoff(Err(refused.clone()))),
                NOW,
                &requested(),
                &ON
            ),
            Decision::Skip { .. }
        ));
        refused
    };
    let missing = refused_with(
        &w,
        w.handoff_files(&w.key.pkcs8, BUILDER, |dir| {
            fs::remove_file(dir.join(&part)).unwrap();
        }),
    );
    assert!(
        missing.why.contains(&part) && !missing.bad_signature,
        "{missing:?}"
    );
    let changed = refused_with(
        &w,
        w.handoff_files(&w.key.pkcs8, BUILDER, |dir| {
            let mut bytes = fs::read(dir.join(&part)).unwrap();
            let last = bytes.len() - 1;
            bytes[last] ^= 1;
            fs::write(dir.join(&part), bytes).unwrap();
        }),
    );
    assert!(changed.why.contains("SHA-256"), "{changed:?}");
    let another_digest = refused_with(
        &w,
        w.handoff_files(&w.key.pkcs8, BUILDER, |dir| {
            let table = fs::read_to_string(dir.join(pull::PARTS_FILE)).unwrap();
            let parts = pull::Parts::parse(&table).unwrap();
            let forged = table.replace(&parts.sha256, &"0".repeat(64));
            fs::write(dir.join(pull::PARTS_FILE), forged).unwrap();
        }),
    );
    assert!(another_digest.why.contains("SHA-256"), "{another_digest:?}");
    let forger = Key::new();
    let forged = refused_with(&w, w.handoff_files(&forger.pkcs8, BUILDER, |_| {}));
    assert!(
        forged.bad_signature && forged.why.contains("BAD SIGNATURE"),
        "{forged:?}"
    );
    assert!(matches!(
        decide(Ok(Found::Handoff(Err(forged))), NOW, &requested(), &ON),
        Decision::Skip { loud: true, .. }
    ));
    let stranger = refused_with(&w, w.handoff_files(&w.key.pkcs8, "win-builder-9", |_| {}));
    assert!(
        stranger.why.contains("unknown builder") && !stranger.bad_signature,
        "{stranger:?}"
    );

    let expired = {
        let key = Key::new();
        let mut world = World::with(Some(&key.pin(BUILDER, "2026-01-31")));
        world.key = key;
        world
    };
    let files = expired.handoff_files(&expired.key.pkcs8, BUILDER, |_| {});
    let lapsed = refused_with(&expired, files);
    assert!(lapsed.why.contains("expired builder"), "{lapsed:?}");

    let unpinned = World::with(None);
    let files = unpinned.handoff_files(&unpinned.key.pkcs8, BUILDER, |_| {});
    let none = refused_with(&unpinned, files);
    assert!(none.why.contains(pull::BUILDERS_FILE), "{none:?}");
}

/// THE HEARTBEAT ON ORIGIN IS READ BY ITS RECORD AND ITS COMMIT: with no handoff, a builder
/// that pushed a fresh heartbeat keeps the release waiting, and one whose last push is stale
/// does not.
#[test]
fn the_heartbeat_on_origin_is_read_by_its_record_and_its_commit() {
    let w = World::new();
    assert_eq!(w.look(), Found::Absent(Ok(None)));
    w.heartbeat(NOW - 30, NOW - 30);
    let Found::Absent(Ok(Some(fresh))) = w.look() else {
        panic!("a heartbeat");
    };
    assert_eq!(
        (fresh.alive.builder.as_str(), fresh.committed),
        (BUILDER, NOW - 30)
    );
    assert_eq!(fresh.pinned, Ok(()), "C pins this builder");
    assert!(matches!(
        decide(Ok(w.look()), NOW, &requested(), &ON),
        Decision::Wait { .. }
    ));
    w.heartbeat(NOW - 30, NOW - 7200);
    assert!(matches!(
        decide(Ok(w.look()), NOW, &requested(), &ON),
        Decision::Skip { loud: false, .. }
    ));
}

/// WHAT ORIGIN DID NOT GIVE IS NO ANSWER: origin unreadable, a handoff or heartbeat whose fetch
/// failed, or a folder this Mac could not make keeps the run waiting, as for a builder at work,
/// until the wait from the request runs out, and only then goes on without a Windows zip;
/// what origin gave and is refused for what it holds, and a request never written, are
/// decided at once.
/// NEGATIVE CONTROL: one failed read of origin journaled the reservation Mac-only for good,
/// though the builder handed off a minute later.
#[test]
fn what_origin_did_not_give_waits_until_the_wait_runs_out() {
    let pull = requested();
    let deadline = pull.requested_at + WAIT_SECS;
    let refused = |transient| {
        Ok(Found::Handoff(Err(Refused {
            why: "the fetch failed".into(),
            bad_signature: false,
            transient,
        })))
    };
    let not_had = || {
        [
            Err("origin is down".to_string()),
            refused(true),
            absent(Err(unread("the fetch failed", true))),
        ]
    };
    for found in not_had() {
        match decide(found.clone(), NOW, &pull, &ON) {
            Decision::Wait { until, why } => {
                assert_eq!(until, deadline, "{found:?}");
                assert!(why.contains("a later run reads origin again"), "{why}");
            }
            other => panic!("{found:?}: {other:?}"),
        }
    }
    for found in not_had() {
        match decide(found.clone(), deadline, &pull, &ON) {
            Decision::Skip { why, loud: false } => {
                assert!(why.contains("has run out"), "{why}");
            }
            other => panic!("{found:?}: {other:?}"),
        }
    }
    // Refused for what it holds, or no heartbeat at all: decided now.
    assert!(matches!(
        decide(refused(false), NOW, &pull, &ON),
        Decision::Skip { loud: false, .. }
    ));
    assert!(matches!(
        decide(absent(Err(unread("no record", false))), NOW, &pull, &ON),
        Decision::Skip { loud: false, .. }
    ));
    // A request never written: nothing to wait for.
    let unrequested = Pull {
        request: None,
        unrequested: Some("push refused".into()),
        ..pull.clone()
    };
    assert!(matches!(
        decide(Err("origin is down".into()), NOW, &unrequested, &ON),
        Decision::Skip { loud: false, .. }
    ));
}

/// ONLY A BUILDER THAT CAN ANSWER THIS REQUEST IS WAITED ON: a fresh heartbeat of a builder the
/// release commit does not pin, or pins expired, or one whose heartbeat names this request
/// passed over (its shape refused, its builder unpinned, its builds spent), goes on without a
/// Windows zip at once; one that passed another request over is still waited on.
/// NEGATIVE CONTROL: a release waited the whole wait on a builder that could never hand off a
/// pair it would admit, or that had already given up on its request.
#[test]
fn only_a_builder_that_can_answer_the_request_is_waited_on() {
    let skipped = |found: Found, pull: &Pull| match decide(Ok(found), NOW, pull, &ON) {
        Decision::Skip { why, loud: false } => why,
        other => panic!("{other:?}"),
    };
    let pull = requested();
    // Another builder serves than C pins, or C's pin of this one has expired.
    let other = {
        let key = Key::new();
        let mut world = World::with(Some(&key.pin("win-builder-2", "2099-12-31")));
        world.key = key;
        world
    };
    other.heartbeat(NOW - 30, NOW - 30);
    let why = skipped(other.look(), &pull);
    assert!(
        why.contains("does not admit its attestation") && why.contains("unknown builder"),
        "{why}"
    );
    let expired = {
        let key = Key::new();
        let mut world = World::with(Some(&key.pin(BUILDER, "2026-01-31")));
        world.key = key;
        world
    };
    expired.heartbeat(NOW - 30, NOW - 30);
    let why = skipped(expired.look(), &pull);
    assert!(why.contains("expired builder"), "{why}");
    let unpinned = World::with(None);
    unpinned.heartbeat(NOW - 30, NOW - 30);
    let why = skipped(unpinned.look(), &pull);
    assert!(why.contains(pull::BUILDERS_FILE), "{why}");
    // A builder C pins that passed this very request over.
    let w = World::new();
    let request = pull.request.clone().unwrap();
    for over in [
        aterm_winship::pull::PassedOver::Refused,
        aterm_winship::pull::PassedOver::Unpinned,
        aterm_winship::pull::PassedOver::Spent,
    ] {
        let record = Alive {
            request: Some(request.clone()),
            passed_over: Some(over),
            ..alive(BUILDER, NOW - 30)
        };
        w.heartbeat_text(&record.to_toml().unwrap(), NOW - 30);
        let why = skipped(w.look(), &pull);
        assert!(
            why.contains("passed this release's request over") && why.contains(over.words()),
            "{why}"
        );
    }
    // Another request passed over, or this one at work: the builder is waited on.
    for record in [
        Alive {
            request: Some("d".repeat(40)),
            passed_over: Some(aterm_winship::pull::PassedOver::Spent),
            ..alive(BUILDER, NOW - 30)
        },
        Alive {
            request: Some(request.clone()),
            ..alive(BUILDER, NOW - 30)
        },
    ] {
        w.heartbeat_text(&record.to_toml().unwrap(), NOW - 30);
        assert!(matches!(
            decide(Ok(w.look()), NOW, &pull, &ON),
            Decision::Wait { .. }
        ));
    }
}

/// WHAT ORIGIN'S REFS SAY REACHES THE RELEASE ESCAPED: a reason carrying terminal escapes or
/// other control characters (a heartbeat or handoff that does not read, quoted back by the
/// parser; a transport's words) is decided with every control character escaped, and a
/// heartbeat whose builder is no builder id is no heartbeat.
/// NEGATIVE CONTROL: a writer of origin's refs put an OSC sequence into the pump's output and
/// its journal through a builder name.
#[test]
fn what_origin_says_reaches_the_release_escaped() {
    let pull = requested();
    let deadline = pull.requested_at + WAIT_SECS;
    let hostile = "x\u{1b}]0;owned\u{7}\u{1b}[2J\r\nNOW";
    for (found, now) in [
        (Err(hostile.to_string()), NOW),
        (Err(hostile.to_string()), deadline),
        (absent(Err(unread(hostile, false))), NOW),
        (
            Ok(Found::Handoff(Err(Refused {
                why: hostile.into(),
                bad_signature: true,
                transient: false,
            }))),
            NOW,
        ),
    ] {
        let why = match decide(found, now, &pull, &ON) {
            Decision::Skip { why, .. } | Decision::Wait { why, .. } => why,
            other => panic!("{other:?}"),
        };
        assert!(!why.chars().any(char::is_control), "{why:?}");
        assert!(why.contains("x\\u{1b}]0;owned\\u{7}"), "{why}");
    }
    let w = World::new();
    w.heartbeat_text(
        &format!(
            "builder = \"win\\u001b[31m\"\nat = \"{}\"\n",
            aterm_winship::pointer::rfc3339(NOW - 30)
        ),
        NOW - 30,
    );
    let Found::Absent(Err(unread)) = w.look() else {
        panic!("no heartbeat");
    };
    assert!(!unread.transient, "{unread:?}");
    let why = match decide(Ok(Found::Absent(Err(unread))), NOW, &pull, &ON) {
        Decision::Skip { why, loud: false } => why,
        other => panic!("{other:?}"),
    };
    assert!(!why.chars().any(char::is_control), "{why:?}");
}

/// A PAIR ADMITTED IS JUDGED ON ITS ADMISSION DAY: the pull admits an attestation of a builder
/// C pins unexpired on the day it looks, and the cut that declares the pair with that day
/// (`--windows-attested`, from the journal's `admitted_on`) admits it again on any later day,
/// past the builder's `not_after` included, and freezes the day with the zip; declared without
/// it, the cut judges the attestation on its own day. A day that is no date is refused, and
/// the folder a lost pair leaves is named for what it lacks.
/// NEGATIVE CONTROL: a `cut --sign` retried the next UTC day refused the pair the pull admitted
/// the day before, when the builder's pin ended between them.
#[test]
fn a_pair_admitted_is_judged_on_its_admission_day() {
    let key = Key::new();
    let mut w = World::with(Some(&key.pin(BUILDER, "2026-01-31")));
    w.key = key;
    w.hand_off(&w.handoff_files(&w.key.pkcs8, BUILDER, |_| {}));
    // Admitted on its last day; refused on the next.
    let pulled = match w.look_on("2026-01-31") {
        Found::Handoff(Ok(pulled)) => pulled,
        other => panic!("{other:?}"),
    };
    assert!(lacks(&pulled.directory, VERSION).is_empty());
    match w.look_on("2026-02-01") {
        Found::Handoff(Err(refused)) => {
            assert!(refused.why.contains("expired builder"), "{refused:?}");
        }
        other => panic!("{other:?}"),
    }
    let pulled = match w.look_on("2026-01-31") {
        Found::Handoff(Ok(pulled)) => pulled,
        other => panic!("{other:?}"),
    };
    // The cut, today (past 2026-01-31), with the admission day: admitted, the day frozen.
    let dist = scratch();
    let mut handoff = Handoff::declared(&pulled.directory, None, true)
        .unwrap()
        .attested_on(Some("2026-01-31"))
        .unwrap();
    handoff
        .import(dist.path(), VERSION, BUILD, &w.commit, &w.sources())
        .unwrap();
    assert_eq!(handoff.attested_on.as_deref(), Some("2026-01-31"));
    handoff
        .validate_identity(VERSION, BUILD, &w.commit, true)
        .unwrap();
    // A resume of that frozen handoff, its journal read back, admits it the same.
    let mut resumed: Handoff =
        aterm_toml::from_str(&aterm_toml::to_string(&handoff).unwrap()).unwrap();
    resumed
        .import(scratch().path(), VERSION, BUILD, &w.commit, &w.sources())
        .unwrap();
    // A journal that froze the attested zip and lost its day is refused.
    let mut lost = handoff.clone();
    lost.attested_on = None;
    assert!(
        lost.validate_identity(VERSION, BUILD, &w.commit, true)
            .unwrap_err()
            .contains("day")
    );
    // Without the day, judged on today's: the builder's pin has ended.
    let mut today = Handoff::declared(&pulled.directory, None, true).unwrap();
    let err = today
        .import(scratch().path(), VERSION, BUILD, &w.commit, &w.sources())
        .unwrap_err();
    assert!(err.contains("expired builder"), "{err}");
    // A day that is no date; and a digest-bound handoff takes none.
    assert!(
        Handoff::declared(&pulled.directory, None, true)
            .unwrap()
            .attested_on(Some("soon"))
            .is_err()
    );
    let carried = Handoff::declared(&pulled.directory, Some(&"a".repeat(64)), false)
        .unwrap()
        .attested_on(Some("2026-01-31"))
        .unwrap();
    assert_eq!(carried.attested_on, None);
    // What a folder something emptied lacks, by name.
    let asset = pulled.asset.clone();
    fs::remove_file(pulled.directory.join(&asset)).unwrap();
    fs::remove_file(pulled.directory.join(pull::attestation_name(&asset))).unwrap();
    assert_eq!(
        lacks(&pulled.directory, VERSION),
        vec![asset.clone(), pull::attestation_name(&asset)]
    );
}
