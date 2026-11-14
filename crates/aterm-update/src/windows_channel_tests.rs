// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release channel's check against agh-fake's loopback GitHub (web, API and CDN origins,
//! read through the platform's curl), its chain signed with test keys: a good release stages;
//! each tampered link refuses, says which, and stages nothing.

use std::cell::Cell;
use std::time::Duration;

use agh_fake::{AssetSpec, FakeGithub, ReleaseSpec, RepoSpec};
use aterm_update_core::github_rest::Origins;
use ring::signature::{Ed25519KeyPair, KeyPair};

use super::*;
use crate::windows::{Origin, Running, Standing};

const OWNER: &str = "fixture-dev";
const REPO: &str = "aterm";
const VERSION: &str = "0.11.0";
const TAG: &str = "v0.11.0";
const BUILD: u64 = 11;
const COMMIT_FULL: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const COMMIT: &str = "bbbbbbbbbbbb";
/// 2026-11-12T00:00:00Z, inside the fixture roster's validity.
const NOW: i64 = 1_794_441_600;
/// A read's patience where the fixture delivers: a hang detector, never a budget.
const DELIVERS: Duration = Duration::from_secs(60);
const PUBLISHER: &str = "Fixture Publisher";
/// The login that may push to the fixture repository: a reader with its token is listed drafts.
const PUSHER: &str = "fixture-dev-bot";

fn key(seed: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap()
}

fn public(key: &Ed25519KeyPair) -> String {
    aterm_codec::base64::encode(key.public_key().as_ref()).unwrap()
}

/// The paper master the fixture roster is signed by: the one anchor.
fn master() -> Ed25519KeyPair {
    key(11)
}

/// The rostered machine that signs the fixture appcast.
fn machine() -> Ed25519KeyPair {
    key(7)
}

/// A signer that must not be asked: no publisher is pinned.
fn unasked(image: &Path) -> Result<String, String> {
    panic!(
        "no publisher is pinned, so no signature is read ({})",
        image.display()
    )
}

/// The start of a PE32+ x86-64 image declaring `subsystem`, as a linker lays it out.
fn image(subsystem: Subsystem) -> Vec<u8> {
    let mut bytes = vec![0; 1024];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&0x80_u32.to_le_bytes());
    let pe = 0x80;
    bytes[pe..pe + 4].copy_from_slice(b"PE\0\0");
    bytes[pe + 4..pe + 6].copy_from_slice(&0x8664_u16.to_le_bytes());
    bytes[pe + 6..pe + 8].copy_from_slice(&3_u16.to_le_bytes());
    bytes[pe + 20..pe + 22].copy_from_slice(&240_u16.to_le_bytes());
    bytes[pe + 22..pe + 24].copy_from_slice(&0x0022_u16.to_le_bytes());
    let optional = pe + 24;
    bytes[optional..optional + 2].copy_from_slice(&0x20b_u16.to_le_bytes());
    bytes[optional + 68..optional + 70].copy_from_slice(&subsystem.code().to_le_bytes());
    bytes
}

fn build_record(build: u64) -> String {
    build_record_of(VERSION, COMMIT_FULL, build)
}

fn build_record_of(version: &str, commit_full: &str, build: u64) -> String {
    let commit = &commit_full[..12];
    format!(
        r#"{{"schema":1,"version":"{version}","commit":"{commit}","commit_full":"{commit_full}","build_number":{build},"arch":"x64","toolchain":"stock 1.97.1","dirty":false,"built_at":"2026-11-11T23:23:04Z"}}"#
    )
}

/// The portable folder as `build.ps1 -Zip` lays it out under `aterm/`.
fn portable() -> Vec<(String, Vec<u8>)> {
    portable_of(build_record(BUILD))
}

/// The portable folder whose `BUILD.json` is `record`.
fn portable_of(record: String) -> Vec<(String, Vec<u8>)> {
    let mut console = image(Subsystem::Console);
    console.extend(std::iter::repeat_n(0x90, 5000));
    [
        ("aterm/", Vec::new()),
        ("aterm/aterm.exe", console),
        ("aterm/aterm-gui.exe", image(Subsystem::Windowed)),
        ("aterm/install.ps1", b"# the fixture's installer\n".to_vec()),
        ("aterm/BUILD.json", record.into_bytes()),
        ("aterm/licenses/", Vec::new()),
        ("aterm/licenses/MIT.txt", b"MIT".to_vec()),
    ]
    .into_iter()
    .map(|(name, data)| (name.to_string(), data))
    .collect()
}

/// A stored (uncompressed) zip of `members`, as a DOS host writes one.
fn zip(members: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in members {
        let offset = out.len() as u32;
        let fixed = |into: &mut Vec<u8>| {
            into.extend_from_slice(&20_u16.to_le_bytes());
            into.extend_from_slice(&0_u16.to_le_bytes());
            into.extend_from_slice(&0_u16.to_le_bytes());
            into.extend_from_slice(&[0; 8]);
            into.extend_from_slice(&(data.len() as u32).to_le_bytes());
            into.extend_from_slice(&(data.len() as u32).to_le_bytes());
            into.extend_from_slice(&(name.len() as u16).to_le_bytes());
            into.extend_from_slice(&0_u16.to_le_bytes());
        };
        out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        fixed(&mut out);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        central.extend_from_slice(&20_u16.to_le_bytes());
        fixed(&mut central);
        central.extend_from_slice(&[0; 10]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out
}

fn sha256(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

/// One release as the channel publishes it: its roster, appcast and zip, each signature made
/// over the bytes beside it.
#[derive(Clone)]
struct Release {
    tag: String,
    /// The version its Windows zip is named for.
    version: String,
    roster: Vec<u8>,
    roster_sig: Vec<u8>,
    appcast: String,
    appcast_sig: Vec<u8>,
    zip: Vec<u8>,
    /// Whether the release carries its zip as an asset (a Mac-only one does not).
    windows: bool,
}

/// How the fixture GitHub publishes a release.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Published,
    Prerelease,
    Draft,
}

/// agh-fake publishing each of `releases`, each created a minute after the one before it, with
/// the one at `latest` the repository's latest.
fn serve_all(releases: &[(&Release, Kind)], latest: usize) -> FakeGithub {
    let fake = FakeGithub::start();
    let slug = format!("{OWNER}/{REPO}");
    fake.add_repo(&slug, RepoSpec::public().push(PUSHER));
    let mut ids = Vec::new();
    for (minute, (release, kind)) in releases.iter().enumerate() {
        let created = format!(
            "2026-11-{:02}T{:02}:{:02}:00Z",
            1 + minute / (24 * 60),
            minute / 60 % 24,
            minute % 60
        );
        let spec = match kind {
            Kind::Published => ReleaseSpec::published(&release.tag, &created),
            Kind::Prerelease => ReleaseSpec::published(&release.tag, &created).prerelease(true),
            Kind::Draft => ReleaseSpec::draft(&release.tag, &created),
        };
        let id = fake.add_release(&slug, spec);
        let windows = format!("aterm-{}-windows-x86_64.zip", release.version);
        let mut assets = vec![
            (APPCAST, release.appcast.as_bytes()),
            (APPCAST_SIG, release.appcast_sig.as_slice()),
            (ROSTER_ASSET, release.roster.as_slice()),
            (ROSTER_SIG_ASSET, release.roster_sig.as_slice()),
        ];
        if release.windows {
            assets.push((windows.as_str(), release.zip.as_slice()));
        }
        for (name, bytes) in assets {
            fake.add_asset(id, AssetSpec::uploaded(name, bytes));
        }
        ids.push(id);
    }
    fake.set_latest(&slug, Some(ids[latest]));
    fake
}

fn roster(seq: u64) -> (Vec<u8>, Vec<u8>) {
    roster_of(seq, &[("fixture", &machine())], &[])
}

/// A roster of sequence `seq` listing `machines` and revoking `revoked`, signed by the master.
fn roster_of(
    seq: u64,
    machines: &[(&str, &Ed25519KeyPair)],
    revoked: &[&str],
) -> (Vec<u8>, Vec<u8>) {
    let roster = aterm_update_core::Roster {
        schema: 1,
        roster_seq: seq,
        valid_until: "2099-01-01T00:00:00Z".into(),
        machines: machines
            .iter()
            .map(|(id, key)| aterm_update_core::Machine {
                id: (*id).into(),
                pubkey: public(key),
                added_at: String::new(),
                not_after: None,
            })
            .collect(),
        revoked: revoked.iter().map(|id| (*id).to_string()).collect(),
    };
    let bytes = roster.to_toml().unwrap().into_bytes();
    let signature = master().sign(&bytes).as_ref().to_vec();
    (bytes, signature)
}

impl Release {
    /// Build 11 of `COMMIT`, its zip the portable folder.
    fn good() -> Self {
        Self::of(zip(&portable()))
    }

    /// Build 11 whose zip is `zip`, signed as it is.
    fn of(zip: Vec<u8>) -> Self {
        Self::of_build(VERSION, BUILD, COMMIT_FULL, zip)
    }

    /// Aterm `version` (build `build` of `commit_full`) under its tag, whose zip is `zip`, signed
    /// as it is.
    fn of_build(version: &str, build: u64, commit_full: &str, zip: Vec<u8>) -> Self {
        let (roster, roster_sig) = roster(1);
        let mut release = Self {
            tag: format!("v{version}"),
            version: version.to_string(),
            roster,
            roster_sig,
            appcast: String::new(),
            appcast_sig: Vec::new(),
            zip,
            windows: true,
        };
        release.appcast = format!(
            "schema = 1\nmachine_id = \"fixture\"\nroster_seq = 1\nversion = \"{version}\"\n\
             build_number = {build}\ncommit = \"{commit_full}\"\ndmg = \"aterm-{version}.dmg\"\n\
             sha256 = \"{}\"\nwindows_x86_64 = \"aterm-{version}-windows-x86_64.zip\"\n\
             windows_x86_64_sha256 = \"{}\"\nwindows_x86_64_size = {}\n",
            "0".repeat(64),
            sha256(&release.zip),
            release.zip.len(),
        );
        release.resign();
        release
    }

    fn resign(&mut self) {
        self.appcast_sig = machine().sign(self.appcast.as_bytes()).as_ref().to_vec();
    }

    /// The appcast with `from` replaced by `to`, signed again by the rostered machine.
    fn alter(&mut self, from: &str, to: &str) {
        assert!(self.appcast.contains(from), "{from:?} in {}", self.appcast);
        self.appcast = self.appcast.replace(from, to);
        self.resign();
    }

    /// The appcast signed by machine `id`'s `key` instead, under `roster` (sequence `seq`).
    fn signed_by(&mut self, id: &str, key: &Ed25519KeyPair, seq: u64, roster: (Vec<u8>, Vec<u8>)) {
        let from = "machine_id = \"fixture\"\nroster_seq = 1\n";
        assert!(self.appcast.contains(from), "{}", self.appcast);
        self.appcast = self.appcast.replace(
            from,
            &format!("machine_id = \"{id}\"\nroster_seq = {seq}\n"),
        );
        self.appcast_sig = key.sign(self.appcast.as_bytes()).as_ref().to_vec();
        (self.roster, self.roster_sig) = roster;
    }

    /// agh-fake publishing this release alone, as the repository's latest.
    fn serve(&self) -> FakeGithub {
        serve_all(&[(self, Kind::Published)], 0)
    }
}

fn reader(fake: &FakeGithub) -> Reader {
    let p = fake.ports();
    Reader::loopback(
        Origins::loopback(p.api, p.uploads, p.web, p.cdn),
        None,
        DELIVERS,
    )
}

fn source() -> Source {
    Source {
        owner: OWNER.to_string(),
        repo: REPO.to_string(),
    }
}

/// The build on this machine the fixture release beats.
const OLDER_HERE: Have = Have {
    installed: 10,
    offered: 0,
};

/// One check of `release`, served fresh, under the fixture master and `publisher`.
fn run(
    release: &Release,
    lane: &Lane,
    have: Have,
    publisher: &str,
    signer: Signer<'_>,
) -> Result<Checked, String> {
    let fake = release.serve();
    let master = public(&master());
    let masters = [master.as_str()];
    let pins = Pins {
        masters: &masters,
        publisher,
    };
    check(lane, &reader(&fake), &source(), pins, have, NOW, signer)
}

/// A lane in its own scratch folder.
fn scratch_lane(label: &str) -> (crate::ScratchDir, Lane) {
    let dir = crate::scratch_dir(label);
    let lane = Lane::at(dir.path().join("channel"));
    (dir, lane)
}

/// What the lane's folder holds, its lock and record aside.
fn left(lane: &Lane) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(lane.root())
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name != LOCK && name != STATE)
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn assert_nothing_staged(lane: &Lane, what: &str) {
    assert_eq!(lane.read_pointer(), Ok(None), "{what}: no pointer");
    assert_eq!(left(lane), Vec::<String>::new(), "{what}: nothing left");
}

#[test]
fn a_signed_release_s_zip_is_verified_unpacked_and_staged() {
    let (dir, lane) = scratch_lane("stage");
    let release = Release::good();
    let checked = run(&release, &lane, OLDER_HERE, "", &unasked).unwrap();
    let Checked::Staged(staged) = checked else {
        panic!("not staged: {checked:?}");
    };
    assert_eq!(staged.build_number, BUILD);
    assert_eq!(staged.version, VERSION);
    assert_eq!(staged.commit, COMMIT);
    assert_eq!(staged.dir, lane.dist(COMMIT));
    assert_eq!(staged.smoke, Smoke::NotRun);
    assert_eq!(lane.read_pointer(), Ok(Some(staged.clone())));
    // The folder is the zip's `aterm/`, its top-level folder dropped.
    for (name, data) in portable() {
        let Some(relative) = name
            .strip_prefix(ZIP_ROOT)
            .filter(|rest| !rest.ends_with('/') && !rest.is_empty())
        else {
            continue;
        };
        assert_eq!(
            fs::read(staged.dir.join(relative)).unwrap(),
            data,
            "{relative}"
        );
    }
    assert!(staged.installer().is_file());
    assert_eq!(
        left(&lane),
        [
            COMMIT.to_string(),
            format!("{COMMIT}{RECEIPT_SUFFIX}"),
            POINTER.to_string()
        ],
        "the folder, its receipt and the pointer; no download or unpack in flight"
    );
    lane.verify_stage(&staged, "", &unasked).unwrap();
    let state = lane.load().unwrap();
    assert_eq!(state.roster_floor, 1);
    assert_eq!(state.checked_unix, NOW);
    assert!(
        state.outcome.contains("verified and staged"),
        "{}",
        state.outcome
    );

    // Checked again, the stage stands and the zip is not fetched again.
    let fake = release.serve();
    let master = public(&master());
    let masters = [master.as_str()];
    let pins = Pins {
        masters: &masters,
        publisher: "",
    };
    let again = check(
        &lane,
        &reader(&fake),
        &source(),
        pins,
        OLDER_HERE,
        NOW,
        &unasked,
    );
    assert_eq!(again, Ok(Checked::AlreadyStaged(staged.clone())));
    assert!(
        !fake
            .seen()
            .iter()
            .any(|seen| seen.path_and_query.contains("windows-x86_64.zip")),
        "a verified stage is not downloaded again"
    );

    // A window weighs it: with no local build, the channel's build is what is offered.
    let running = Running {
        version: "0.10.0",
        commit: "aaaaaaaaaaaa",
        build_number: 10,
    };
    let no_local = dir.path().join("no-local-latest.json");
    let here = crate::windows::look_with(running, None, &no_local, Some((&lane, true)));
    assert_eq!(here.origin, Origin::Channel);
    assert_eq!(here.standing, Standing::Newer(staged.clone()));
    assert_eq!(here.pointer, lane.pointer());
    let lines = crate::windows::status_lines(running, &here).join("\n");
    assert!(
        lines.contains("came from the release channel, verified"),
        "{lines}"
    );
    assert!(
        lines.contains("release channel, checked 2026-11-12T00:00:00Z"),
        "{lines}"
    );
    let wire = crate::windows::control_line(running, &here);
    assert!(wire.contains(" channel=on origin=channel "), "{wire}");
    assert!(wire.contains(" staged=newer "), "{wire}");
}

/// A zip of the portable folder with `change` made to its members, in a release signed as it is.
fn with_members(change: impl FnOnce(&mut Vec<(String, Vec<u8>)>)) -> Release {
    let mut members = portable();
    change(&mut members);
    Release::of(zip(&members))
}

fn set(members: &mut [(String, Vec<u8>)], name: &str, data: Vec<u8>) {
    members
        .iter_mut()
        .find(|(member, _)| member == name)
        .unwrap()
        .1 = data;
}

#[test]
fn each_tampered_link_refuses_says_which_and_stages_nothing() {
    let good = Release::good();
    let size = good.zip.len();
    let short_by_one = format!("is {size} bytes, not the {}", size + 1);
    let cases: Vec<(&str, Release, &str)> = vec![
        (
            "a roster another key signed",
            Release {
                roster_sig: key(12).sign(&good.roster).as_ref().to_vec(),
                ..good.clone()
            },
            "machine roster signature refused",
        ),
        (
            "a roster changed after it was signed",
            Release {
                roster: String::from_utf8(good.roster.clone())
                    .unwrap()
                    .replace("2099", "2098")
                    .into_bytes(),
                ..good.clone()
            },
            "machine roster signature refused",
        ),
        (
            "an appcast a machine off the roster signed",
            Release {
                appcast_sig: key(13).sign(good.appcast.as_bytes()).as_ref().to_vec(),
                ..good.clone()
            },
            "appcast signature refused",
        ),
        (
            "an appcast changed after it was signed",
            Release {
                appcast: good
                    .appcast
                    .replace("build_number = 11", "build_number = 12"),
                ..good.clone()
            },
            "appcast signature refused",
        ),
        (
            "an appcast that names another machine",
            {
                let mut release = good.clone();
                release.alter("machine_id = \"fixture\"", "machine_id = \"m99\"");
                release
            },
            "appcast signer attribution refused",
        ),
        (
            "a zip whose SHA-256 is not the signed one",
            {
                let mut release = good.clone();
                release.alter(&sha256(&good.zip), &sha256(b"another zip"));
                release
            },
            "SHA-256 differs from the signed appcast",
        ),
        (
            "a zip shorter than the signed size",
            {
                let mut release = good.clone();
                release.alter(
                    &format!("windows_x86_64_size = {size}"),
                    &format!("windows_x86_64_size = {}", size + 1),
                );
                release
            },
            &short_by_one,
        ),
        (
            "a zip longer than the signed size",
            {
                let mut release = good.clone();
                release.alter(
                    &format!("windows_x86_64_size = {size}"),
                    &format!("windows_x86_64_size = {}", size - 1),
                );
                release
            },
            "could not download aterm-0.11.0-windows-x86_64.zip",
        ),
        (
            "a zip whose images are swapped",
            with_members(|members| {
                set(members, "aterm/aterm.exe", image(Subsystem::Windowed));
                set(members, "aterm/aterm-gui.exe", image(Subsystem::Console));
            }),
            "declares subsystem 2, not Console",
        ),
        (
            "a zip with no installer",
            with_members(|members| members.retain(|(name, _)| name != "aterm/install.ps1")),
            "carries no install.ps1",
        ),
        (
            "a zip whose build record names another build",
            with_members(|members| {
                set(members, "aterm/BUILD.json", build_record(12).into_bytes());
            }),
            "BUILD.json names aterm 0.11.0 (commit bbbbbbbbbbbb, build 12)",
        ),
        (
            "a zip with a member outside its folder",
            with_members(|members| members.push(("evil.txt".to_string(), b"x".to_vec()))),
            "outside the top-level aterm/ folder",
        ),
        (
            "an appcast of another version under this tag",
            {
                let mut release = good.clone();
                release.alter(&format!("version = \"{VERSION}\""), "version = \"0.12.0\"");
                release.alter(&format!("aterm-{VERSION}-windows"), "aterm-0.12.0-windows");
                release
            },
            "signs aterm 0.12.0, not the release v0.11.0",
        ),
    ];
    for (what, release, expect) in cases {
        let (_dir, lane) = scratch_lane("tamper");
        let why = run(&release, &lane, OLDER_HERE, "", &unasked).expect_err(what);
        assert!(why.contains(expect), "{what}: {why}");
        assert_nothing_staged(&lane, what);
        assert_eq!(
            lane.load().unwrap().outcome,
            why,
            "{what}: the refusal is the lane's last word"
        );
    }
}

#[test]
fn a_build_that_does_not_beat_this_machine_or_its_floors_is_not_staged() {
    let release = Release::good();
    for (what, have) in [
        (
            "the installed build",
            Have {
                installed: BUILD,
                offered: 0,
            },
        ),
        (
            "a newer installed build",
            Have {
                installed: BUILD + 1,
                offered: 0,
            },
        ),
        (
            "the local build loop's offer",
            Have {
                installed: 10,
                offered: BUILD,
            },
        ),
    ] {
        let (_dir, lane) = scratch_lane("downgrade");
        let checked = run(&release, &lane, have, "", &unasked);
        assert_eq!(
            checked,
            Ok(Checked::NotNewer {
                tag: TAG.to_string(),
                build: BUILD,
                have: have.installed.max(have.offered),
            }),
            "{what}"
        );
        assert_nothing_staged(&lane, what);
    }

    let floors: [(&str, State, &str); 3] = [
        (
            "a roster older than one already admitted",
            State {
                roster_floor: 2,
                ..State::default()
            },
            "machine roster admission refused: Rollback",
        ),
        (
            "a build below the signed minimum",
            State {
                min_build: BUILD + 1,
                ..State::default()
            },
            "below the signed minimum build 12",
        ),
        (
            "a signer an admitted roster revoked",
            State {
                revoked_machines: vec!["fixture".to_string()],
                ..State::default()
            },
            "its signer fixture is revoked",
        ),
    ];
    for (what, state, expect) in floors {
        let (_dir, lane) = scratch_lane("floor");
        fs::create_dir_all(lane.root()).unwrap();
        lane.save(&state).unwrap();
        let why = run(&release, &lane, OLDER_HERE, "", &unasked).expect_err(what);
        assert!(why.contains(expect), "{what}: {why}");
        assert_nothing_staged(&lane, what);
    }
}

#[test]
fn an_admitted_roster_raises_the_floor_even_when_its_appcast_is_refused() {
    let (_dir, lane) = scratch_lane("floor-holds");
    let mut release = Release::good();
    (release.roster, release.roster_sig) = roster(3);
    release.appcast_sig = key(13).sign(release.appcast.as_bytes()).as_ref().to_vec();
    let why = run(&release, &lane, OLDER_HERE, "", &unasked).unwrap_err();
    assert!(why.contains("appcast signature refused"), "{why}");
    assert_eq!(lane.load().unwrap().roster_floor, 3);
    // So the roster of sequence 1 the good release carries is a replay now.
    let why = run(&Release::good(), &lane, OLDER_HERE, "", &unasked).unwrap_err();
    assert!(why.contains("Rollback"), "{why}");
    assert_nothing_staged(&lane, "after the replay");
}

/// The next release, aterm 0.12.0 (build 12) under `tag`, with `extra` appcast lines and no
/// Windows zip, signed by machine `id`'s `key` under `roster` (sequence `seq`).
fn next_release(
    tag: &str,
    extra: &str,
    (id, key): (&str, &Ed25519KeyPair),
    seq: u64,
    roster: (Vec<u8>, Vec<u8>),
) -> Release {
    let mut next = Release::good();
    next.tag = tag.to_string();
    next.version = "0.12.0".to_string();
    next.windows = false;
    next.appcast = format!(
        "schema = 1\nmachine_id = \"{id}\"\nroster_seq = {seq}\nversion = \"0.12.0\"\n\
         build_number = 12\n{extra}commit = \"{}\"\ndmg = \"aterm-0.12.0.dmg\"\n\
         sha256 = \"{}\"\n",
        "c".repeat(40),
        "0".repeat(64)
    );
    next.appcast_sig = key.sign(next.appcast.as_bytes()).as_ref().to_vec();
    (next.roster, next.roster_sig) = roster;
    next
}

/// A later check's floors withdraw a stage they no longer admit, whatever that check comes to:
/// a release that yanks its build, or whose roster revokes the machine that signed it, withdraws
/// it when that release carries no Windows zip and when its appcast is refused.
#[test]
fn a_newer_signed_floor_or_a_revoked_signer_withdraws_the_stage() {
    let second = key(8);
    let revoking = || roster_of(2, &[("second", &second)], &["fixture"]);
    let no_zip: Result<Checked, String> = Ok(Checked::NoAsset {
        tag: "v0.12.0".to_string(),
        why: "carries no x86_64-pc-windows-msvc zip, and no release before it does among the 1 \
              the channel lists on 1 page of 100 (none of them is a published vMAJOR.MINOR.PATCH \
              release older than it)"
            .to_string(),
    });
    let mistagged: Result<Checked, String> = Err(
        "appcast refused: it signs aterm 0.12.0, not the release v0.13.0 it was read from"
            .to_string(),
    );
    let cases: [(&str, Release, Result<Checked, String>); 4] = [
        (
            "a release that yanks build 11",
            next_release(
                "v0.12.0",
                "min_build = 12\n",
                ("fixture", &machine()),
                1,
                roster(1),
            ),
            no_zip.clone(),
        ),
        (
            "a refused release that yanks build 11",
            next_release(
                "v0.13.0",
                "min_build = 12\n",
                ("fixture", &machine()),
                1,
                roster(1),
            ),
            mistagged.clone(),
        ),
        (
            "a release whose roster revokes the stage's signer",
            next_release("v0.12.0", "", ("second", &second), 2, revoking()),
            no_zip,
        ),
        (
            "a refused release whose roster revokes the stage's signer",
            next_release("v0.13.0", "", ("second", &second), 2, revoking()),
            mistagged,
        ),
    ];
    for (what, next, expect) in cases {
        let (_dir, lane) = scratch_lane("withdraw");
        let Ok(Checked::Staged(staged)) = run(&Release::good(), &lane, OLDER_HERE, "", &unasked)
        else {
            panic!("the fixture release stages");
        };
        assert_eq!(
            run(&next, &lane, OLDER_HERE, "", &unasked),
            expect,
            "{what}"
        );
        assert_nothing_staged(&lane, what);
        let why = crate::windows::before_install(&staged, Some(&lane), "", &unasked).unwrap_err();
        assert!(why.contains("no longer stages"), "{what}: {why}");
    }
}

/// Floors recorded after a stage (a check that admitted them and stopped before it withdrew the
/// stage) make no window offer it and its folder refuse to install.
#[test]
fn a_stage_its_lane_s_floors_no_longer_admit_is_neither_offered_nor_installed() {
    let (dir, lane) = scratch_lane("inadmissible");
    let Ok(Checked::Staged(staged)) = run(&Release::good(), &lane, OLDER_HERE, "", &unasked) else {
        panic!("the fixture release stages");
    };
    let running = Running {
        version: "0.10.0",
        commit: "aaaaaaaaaaaa",
        build_number: 10,
    };
    let no_local = dir.path().join("no-local-latest.json");
    let kept = lane.load().unwrap();
    for (what, state, expect) in [
        (
            "its signer revoked",
            State {
                revoked_machines: vec!["fixture".to_string()],
                ..kept.clone()
            },
            "signed by machine fixture, which the machine roster revoked",
        ),
        (
            "its build below the signed minimum",
            State {
                min_build: BUILD + 1,
                ..kept.clone()
            },
            "is below the signed minimum build 12",
        ),
    ] {
        lane.save(&state).unwrap();
        let why = lane.read_pointer().unwrap_err();
        assert!(why.contains(expect), "{what}: {why}");
        let here = crate::windows::look_with(running, None, &no_local, Some((&lane, true)));
        assert_eq!(here.standing, Standing::Absent, "{what}");
        assert_eq!(here.origin, Origin::Local, "{what}");
        let why = crate::windows::before_install(&staged, Some(&lane), "", &unasked).unwrap_err();
        assert!(why.contains(expect), "{what}: {why}");
    }
    lane.save(&kept).unwrap();
    assert_eq!(
        crate::windows::before_install(&staged, Some(&lane), "", &unasked),
        Ok(Origin::Channel)
    );
}

/// A stage stands as a release's only when that release signs the same zip from the same
/// machine: the same build re-published with another zip, or by another machine, is downloaded
/// and staged again, and a roster that then revokes the first machine leaves it standing.
#[test]
fn a_release_signed_again_with_another_zip_or_by_another_machine_is_staged_again() {
    let (_dir, lane) = scratch_lane("resigned");
    let Ok(Checked::Staged(first)) = run(&Release::good(), &lane, OLDER_HERE, "", &unasked) else {
        panic!("the fixture release stages");
    };
    let mit = first.dir.join("licenses").join("MIT.txt");
    let fixed = with_members(|members| {
        set(members, "aterm/licenses/MIT.txt", b"MIT, again".to_vec());
    });
    assert_eq!(
        run(&fixed, &lane, OLDER_HERE, "", &unasked),
        Ok(Checked::Staged(first.clone())),
        "another zip of the same build"
    );
    assert_eq!(fs::read(&mit).unwrap(), b"MIT, again");

    let second = key(8);
    let mut moved = fixed.clone();
    moved.signed_by(
        "second",
        &second,
        2,
        roster_of(2, &[("fixture", &machine()), ("second", &second)], &[]),
    );
    assert_eq!(
        run(&moved, &lane, OLDER_HERE, "", &unasked),
        Ok(Checked::Staged(first.clone())),
        "the same zip signed by another machine"
    );
    let mut state = lane.load().unwrap();
    state.revoked_machines.push("fixture".to_string());
    lane.save(&state).unwrap();
    assert_eq!(lane.read_pointer(), Ok(Some(first.clone())));
    assert_eq!(
        run(&moved, &lane, OLDER_HERE, "", &unasked),
        Ok(Checked::AlreadyStaged(first))
    );
}

/// A newer build's stage that cannot be recorded (here its receipt cannot be written) leaves the
/// stage before it offered and installable, and removes its own folder.
#[test]
fn a_stage_that_cannot_be_recorded_leaves_the_one_before_it_offered() {
    let (_dir, lane) = scratch_lane("unrecorded");
    let Ok(Checked::Staged(first)) = run(&Release::good(), &lane, OLDER_HERE, "", &unasked) else {
        panic!("the fixture release stages");
    };
    let newer_full = "c".repeat(40);
    let newer_commit = &newer_full[..12];
    let newer = Release::of_build(
        "0.12.0",
        BUILD + 1,
        &newer_full,
        zip(&portable_of(build_record_of(
            "0.12.0",
            &newer_full,
            BUILD + 1,
        ))),
    );
    // A folder where the newer stage's receipt goes: the receipt cannot land.
    let blocker = lane.receipt_path(newer_commit);
    fs::create_dir(&blocker).unwrap();
    let why = run(&newer, &lane, OLDER_HERE, "", &unasked).unwrap_err();
    assert!(why.contains("cannot write"), "{why}");
    assert_eq!(lane.read_pointer(), Ok(Some(first.clone())));
    assert_eq!(
        crate::windows::before_install(&first, Some(&lane), "", &unasked),
        Ok(Origin::Channel)
    );
    assert!(!lane.dist(newer_commit).exists(), "the unrecorded folder");

    // Once its receipt can be written, the newer build stages and the one before it goes.
    fs::remove_dir(&blocker).unwrap();
    let Ok(Checked::Staged(second)) = run(&newer, &lane, OLDER_HERE, "", &unasked) else {
        panic!("the newer release stages");
    };
    assert_eq!(second.build_number, BUILD + 1);
    assert_eq!(lane.read_pointer(), Ok(Some(second)));
    assert_eq!(
        left(&lane),
        [
            newer_commit.to_string(),
            format!("{newer_commit}{RECEIPT_SUFFIX}"),
            POINTER.to_string()
        ]
    );
}

/// Aterm `version`, build `build` of a commit of `c`s under `v{version}`, its zip the portable
/// folder of that build, signed by the fixture machine under the roster of sequence 1.
fn windows_release(version: &str, build: u64, c: char) -> Release {
    let full = c.to_string().repeat(40);
    let record = build_record_of(version, &full, build);
    Release::of_build(version, build, &full, zip(&portable_of(record)))
}

/// The Mac-only release cut after `VERSION`: aterm 0.12.0 (build 12) under v0.12.0 with `extra`
/// appcast lines and no Windows zip, signed by the fixture machine under `roster` (sequence
/// `seq`).
fn mac_only(extra: &str, seq: u64, roster: (Vec<u8>, Vec<u8>)) -> Release {
    next_release("v0.12.0", extra, ("fixture", &machine()), seq, roster)
}

/// One check of what `reader` reads, under the fixture master and no publisher.
fn check_through(reader: &Reader, lane: &Lane, have: Have) -> Result<Checked, String> {
    let master = public(&master());
    let masters = [master.as_str()];
    let pins = Pins {
        masters: &masters,
        publisher: "",
    };
    check(lane, reader, &source(), pins, have, NOW, &unasked)
}

/// Whether `fake` was asked for anything of the release of `tag` by its download route.
fn read_from(fake: &FakeGithub, tag: &str) -> bool {
    let route = format!("/{OWNER}/{REPO}/releases/download/{tag}/");
    fake.seen()
        .iter()
        .any(|seen| seen.path_and_query.starts_with(&route))
}

/// A Mac-only newest release (the pump's) leaves the newest release before it that carries a
/// Windows zip: that release is held to the chain under the newest one's roster and floors,
/// staged, and stands when checked again. Without the walk back the check came to `NoAsset`.
#[test]
fn a_mac_only_newest_release_stages_the_newest_windows_release_before_it() {
    let (_dir, lane) = scratch_lane("walk-back");
    fs::create_dir_all(lane.root()).unwrap();
    // An earlier check admitted the roster of sequence 2, so the Windows release's own roster
    // (sequence 1) is a replay: the walk authorizes its appcast under the newest release's.
    lane.save(&State {
        roster_floor: 2,
        ..State::default()
    })
    .unwrap();
    let windows = Release::good();
    let mac = mac_only("", 2, roster(2));
    let fake = serve_all(&[(&windows, Kind::Published), (&mac, Kind::Published)], 1);
    let checked = check_through(&reader(&fake), &lane, OLDER_HERE).unwrap();
    let Checked::Staged(staged) = checked else {
        panic!("not staged: {checked:?}");
    };
    assert_eq!(
        (
            staged.version.as_str(),
            staged.build_number,
            staged.commit.as_str()
        ),
        (VERSION, BUILD, COMMIT)
    );
    let pointer = lane.read_pointer_record().unwrap().unwrap();
    assert_eq!(pointer.tag, TAG);
    assert_eq!(pointer.machine_id, "fixture");
    lane.verify_stage(&staged, "", &unasked).unwrap();
    assert!(
        !fake.seen().iter().any(|seen| seen
            .path_and_query
            .ends_with(&format!("/{TAG}/{ROSTER_ASSET}"))),
        "the Windows release's own roster is never read"
    );
    assert_eq!(lane.load().unwrap().roster_floor, 2);

    assert_eq!(
        check_through(&reader(&fake), &lane, OLDER_HERE),
        Ok(Checked::AlreadyStaged(staged.clone())),
        "checked again, the stage stands"
    );
    let installed = Have {
        installed: BUILD,
        offered: 0,
    };
    let (_dir, here) = scratch_lane("walk-back-installed");
    let checked = check_through(&reader(&fake), &here, installed);
    assert_eq!(
        checked,
        Ok(Checked::NotNewer {
            tag: TAG.to_string(),
            build: BUILD,
            have: BUILD,
        })
    );
    assert!(
        checked.unwrap().line().starts_with(
            "the newest release for this machine, v0.11.0 (build 11), is not newer than build 11"
        ),
        "the line names the release weighed"
    );
}

/// The newest release's signed minimum build binds the release the walk finds: one below it is
/// refused, naming the release and the link, and nothing is staged.
#[test]
fn a_windows_release_below_the_newest_release_s_minimum_build_is_refused() {
    let (_dir, lane) = scratch_lane("walk-back-floor");
    let windows = Release::good();
    let mac = mac_only("min_build = 12\n", 1, roster(1));
    let fake = serve_all(&[(&windows, Kind::Published), (&mac, Kind::Published)], 1);
    let why = check_through(&reader(&fake), &lane, OLDER_HERE).unwrap_err();
    assert_eq!(
        why,
        "the newest release, v0.12.0, carries no x86_64-pc-windows-msvc zip, and the newest \
         before it that does, v0.11.0, is refused: aterm 0.11.0 (build 11) is below the signed \
         minimum build 12"
    );
    assert_nothing_staged(&lane, "below the floor");
    assert_eq!(lane.load().unwrap().min_build, 12);
    assert_eq!(lane.load().unwrap().outcome, why);
}

/// The release the walk finds is authorized under the newest release's roster and the lane's
/// revocations: a signer that roster revokes, or one an admitted roster revoked before, is
/// refused; the same release from a signer neither revoked stages.
#[test]
fn a_windows_release_whose_signer_is_revoked_is_refused() {
    let second = key(8);
    let both = || roster_of(2, &[("fixture", &machine()), ("second", &second)], &[]);
    let revoking = roster_of(2, &[("second", &second)], &["fixture"]);
    let cases: [(&str, Release, Vec<String>, Option<&str>); 3] = [
        (
            "the newest release's roster revokes its signer",
            next_release("v0.12.0", "", ("second", &second), 2, revoking),
            Vec::new(),
            Some("v0.11.0, is refused: appcast signature refused"),
        ),
        (
            "a roster admitted before revoked its signer",
            next_release("v0.12.0", "", ("second", &second), 2, both()),
            vec!["fixture".to_string()],
            Some("v0.11.0, is refused: appcast refused: its signer fixture is revoked"),
        ),
        (
            "its signer revoked by no roster",
            next_release("v0.12.0", "", ("second", &second), 2, both()),
            Vec::new(),
            None,
        ),
    ];
    for (what, mac, revoked, refused) in cases {
        let (_dir, lane) = scratch_lane("walk-back-revoked");
        fs::create_dir_all(lane.root()).unwrap();
        lane.save(&State {
            revoked_machines: revoked,
            ..State::default()
        })
        .unwrap();
        let windows = Release::good();
        let fake = serve_all(&[(&windows, Kind::Published), (&mac, Kind::Published)], 1);
        let checked = check_through(&reader(&fake), &lane, OLDER_HERE);
        match refused {
            Some(expect) => {
                let why = checked.expect_err(what);
                assert!(why.contains(expect), "{what}: {why}");
                assert_nothing_staged(&lane, what);
            }
            None => assert!(
                matches!(&checked, Ok(Checked::Staged(staged)) if staged.build_number == BUILD),
                "{what}: {checked:?}"
            ),
        }
    }
}

/// When no release the walk looks at carries this machine's zip (a Mac-only one, or one whose
/// zip is not named for its own version), the check comes to `NoAsset`, naming the newest
/// release and what it looked at, and reads no earlier release's appcast.
#[test]
fn no_windows_release_in_the_walk_is_no_asset_naming_what_it_looked_at() {
    let (_dir, lane) = scratch_lane("walk-back-none");
    let mac11 = next_release("v0.11.0", "", ("fixture", &machine()), 1, roster(1));
    let mut misnamed = windows_release("0.10.0", 10, 'a');
    misnamed.version = "0.9.0".to_string();
    let mac = mac_only("", 1, roster(1));
    let fake = serve_all(
        &[
            (&misnamed, Kind::Published),
            (&mac11, Kind::Published),
            (&mac, Kind::Published),
        ],
        2,
    );
    let checked = check_through(&reader(&fake), &lane, OLDER_HERE);
    let expect = Checked::NoAsset {
        tag: "v0.12.0".to_string(),
        why: "carries no x86_64-pc-windows-msvc zip, and no release before it does among the 3 \
              the channel lists on 1 page of 100 (looked at v0.11.0, v0.10.0)"
            .to_string(),
    };
    assert_eq!(checked, Ok(expect.clone()));
    assert_nothing_staged(&lane, "no Windows release");
    assert_eq!(lane.load().unwrap().outcome, expect.line());
    assert!(
        expect
            .line()
            .starts_with("the newest release, v0.12.0, carries no"),
        "{}",
        expect.line()
    );
    assert!(!read_from(&fake, "v0.11.0"), "no zip listed, nothing read");
    assert!(
        !read_from(&fake, "v0.10.0"),
        "no zip of its version, nothing read"
    );
}

/// The walk weighs releases by version, never by where the listing puts them, and skips drafts,
/// prereleases and tags that are not `vMAJOR.MINOR.PATCH`, whatever they carry: the newest
/// proper release before the Mac-only one is staged, and no skipped release is read.
#[test]
fn the_walk_skips_drafts_prereleases_and_other_tags_and_weighs_versions_not_listing_order() {
    let (_dir, lane) = scratch_lane("walk-back-skip");
    let older = windows_release("0.10.0", 10, 'a');
    let wanted = Release::good();
    let draft = windows_release("0.11.3", 14, 'd');
    let pre = windows_release("0.11.2", 13, 'e');
    let mut legacy = windows_release("0.11.5", 15, 'f');
    legacy.tag = "v0.11".to_string();
    let mut malformed = windows_release("0.11.6", 16, '9');
    malformed.tag = "v0.11.6-rc1".to_string();
    // Created after the release wanted, so listed above it, but of an older version.
    let hotfix = windows_release("0.10.5", 12, '8');
    let mac = mac_only("", 1, roster(1));
    let fake = serve_all(
        &[
            (&older, Kind::Published),
            (&wanted, Kind::Published),
            (&draft, Kind::Draft),
            (&pre, Kind::Prerelease),
            (&legacy, Kind::Published),
            (&malformed, Kind::Published),
            (&hotfix, Kind::Published),
            (&mac, Kind::Published),
        ],
        7,
    );
    // A reader that may push is listed the draft too, so its skip is the walk's.
    let secret = agh_fake::token("walk-back");
    fake.add_token(&secret, PUSHER);
    let pusher = || {
        let p = fake.ports();
        Reader::loopback(
            Origins::loopback(p.api, p.uploads, p.web, p.cdn),
            Some(aterm_update_core::github_rest::Token::new(secret.as_bytes()).unwrap()),
            DELIVERS,
        )
    };
    let listed = pusher().releases(OWNER, REPO, 30, 1).unwrap();
    let tags: Vec<&str> = listed.iter().map(|release| release.tag.as_str()).collect();
    assert_eq!(
        tags,
        [
            "v0.12.0",
            "v0.10.5",
            "v0.11.6-rc1",
            "v0.11",
            "v0.11.2",
            "v0.11.3",
            "v0.11.0",
            "v0.10.0"
        ],
        "the listing, newest first by creation"
    );
    assert!(listed.iter().any(|release| release.draft));
    fake.clear_seen();

    let checked = check_through(&pusher(), &lane, OLDER_HERE).unwrap();
    let Checked::Staged(staged) = checked else {
        panic!("not staged: {checked:?}");
    };
    assert_eq!(
        (staged.version.as_str(), staged.build_number),
        (VERSION, BUILD)
    );
    assert_eq!(lane.read_pointer_record().unwrap().unwrap().tag, TAG);
    for skipped in [
        "v0.11.3",
        "v0.11.2",
        "v0.11",
        "v0.11.6-rc1",
        "v0.10.5",
        "v0.10.0",
    ] {
        assert!(!read_from(&fake, skipped), "{skipped} is not read");
    }
}

/// `count` Mac-only releases the walk lists and never reads, `v0.10.{first}` upward, each created
/// after the one before it: the pump's releases between two Windows ones.
fn fillers(first: usize, count: usize) -> Vec<Release> {
    let base = mac_only("", 1, roster(1));
    (first..first + count)
        .map(|n| Release {
            tag: format!("v0.10.{n}"),
            ..base.clone()
        })
        .collect()
}

/// agh-fake publishing `releases`, oldest first, each published, the last the latest.
fn serve_published(releases: &[Release]) -> FakeGithub {
    let published: Vec<(&Release, Kind)> = releases.iter().map(|r| (r, Kind::Published)).collect();
    serve_all(&published, published.len() - 1)
}

/// The pages of the releases listing `fake` was asked for, by number, in order.
fn pages_read(fake: &FakeGithub) -> Vec<u32> {
    let listing = format!("/repos/{OWNER}/{REPO}/releases?per_page=100&page=");
    fake.seen()
        .iter()
        .filter_map(|seen| seen.path_and_query.strip_prefix(&listing)?.parse().ok())
        .collect()
}

/// The pump's releases push the last Windows release past the listing's first page: the walk
/// reads page after page until the one holding it, and then one more, and stages it; it reads
/// no further page, though the listing has one.
#[test]
fn a_windows_release_pages_back_is_staged_reading_one_page_past_it() {
    let (_dir, lane) = scratch_lane("walk-back-pages");
    // Newest first: page 1 is the latest and 99 fillers, page 2 99 fillers and the Windows
    // release, page 3 100 fillers, page 4 one more.
    let mut releases = fillers(0, 101);
    releases.push(Release::good());
    releases.extend(fillers(101, 99 + 99));
    releases.push(mac_only("", 1, roster(1)));
    let fake = serve_published(&releases);
    let checked = check_through(&reader(&fake), &lane, OLDER_HERE).unwrap();
    let Checked::Staged(staged) = checked else {
        panic!("not staged: {checked:?}");
    };
    assert_eq!(
        (staged.version.as_str(), staged.build_number),
        (VERSION, BUILD)
    );
    assert_eq!(lane.read_pointer_record().unwrap().unwrap().tag, TAG);
    assert_eq!(pages_read(&fake), [1, 2, 3]);
}

/// A Windows release created after an older-versioned one (a hotfix cut later) is listed above
/// it; when the release of the newer version is on the next page, the page the walk reads past
/// the first candidate finds it, and the newer version is staged, the hotfix never read.
#[test]
fn the_page_past_the_first_windows_release_is_weighed_too() {
    let (_dir, lane) = scratch_lane("walk-back-next-page");
    // Newest first: page 1 is the latest, 98 fillers and the hotfix; page 2 the release wanted.
    let mut releases = vec![Release::good(), windows_release("0.10.500", 12, '8')];
    releases.extend(fillers(0, 98));
    releases.push(mac_only("", 1, roster(1)));
    let fake = serve_published(&releases);
    let checked = check_through(&reader(&fake), &lane, OLDER_HERE).unwrap();
    let Checked::Staged(staged) = checked else {
        panic!("not staged: {checked:?}");
    };
    assert_eq!(
        (staged.version.as_str(), staged.build_number),
        (VERSION, BUILD)
    );
    assert_eq!(pages_read(&fake), [1, 2], "page 2 is the listing's last");
    assert!(!read_from(&fake, "v0.10.500"), "the hotfix is not read");
}

/// GitHub lists no release past its first 1000: with none of those carrying a zip, the walk
/// reads ten pages and comes to `NoAsset`, naming how much it read and the newest it looked at;
/// the Windows release past them is never read.
#[test]
fn the_walk_reads_no_further_than_github_lists() {
    let (_dir, lane) = scratch_lane("walk-back-bound");
    let mut releases = vec![Release::good()];
    releases.extend(fillers(0, 999));
    releases.push(mac_only("", 1, roster(1)));
    let fake = serve_published(&releases);
    let checked = check_through(&reader(&fake), &lane, OLDER_HERE);
    assert_eq!(
        checked,
        Ok(Checked::NoAsset {
            tag: "v0.12.0".to_string(),
            why: "carries no x86_64-pc-windows-msvc zip, and no release before it does among \
                  the 1000 the channel lists on 10 pages of 100 (looked at v0.10.998, v0.10.997, \
                  v0.10.996, v0.10.995, v0.10.994, v0.10.993, v0.10.992, v0.10.991, v0.10.990, \
                  v0.10.989 and 989 older)"
                .to_string(),
        })
    );
    assert_eq!(pages_read(&fake), (1..=10).collect::<Vec<u32>>());
    assert!(!read_from(&fake, TAG));
    assert_nothing_staged(&lane, "past the listing");
}

/// The release the walk finds is held to every link of the chain, each refusal naming it: the
/// release of another version than its tag, and one naming no source commit, are refused.
#[test]
fn a_windows_release_that_is_not_its_tag_s_or_names_no_commit_is_refused() {
    let mut mistagged = Release::good();
    mistagged.alter("0.11.0", "0.11.2");
    let mut no_commit = Release::good();
    no_commit.alter(COMMIT_FULL, "not-a-commit");
    let lead = "the newest release, v0.12.0, carries no x86_64-pc-windows-msvc zip, and the \
                newest before it that does, v0.11.0, is refused: ";
    for (windows, expect) in [
        (
            mistagged,
            "appcast refused: it signs aterm 0.11.2, not the release v0.11.0 it was read from",
        ),
        (
            no_commit,
            "appcast refused: it names no source commit and build",
        ),
    ] {
        let (_dir, lane) = scratch_lane("walk-back-chain");
        let fake = serve_published(&[windows, mac_only("", 1, roster(1))]);
        assert_eq!(
            check_through(&reader(&fake), &lane, OLDER_HERE),
            Err(format!("{lead}{expect}"))
        );
        assert_nothing_staged(&lane, expect);
    }
}

/// The walked release's own signed minimum build raises the lane's floor: met by its own build it
/// stages and is recorded; above the build already staged it withdraws that stage, even when the
/// walked release's zip then fails to download.
#[test]
fn the_walked_release_s_own_minimum_build_raises_the_floor() {
    let (_dir, lane) = scratch_lane("walk-back-own-floor");
    let mut windows = Release::good();
    windows.alter("build_number = 11\n", "build_number = 11\nmin_build = 11\n");
    let mac = mac_only("", 1, roster(1));
    let fake = serve_published(&[windows, mac.clone()]);
    let checked = check_through(&reader(&fake), &lane, OLDER_HERE);
    assert!(
        matches!(&checked, Ok(Checked::Staged(staged)) if staged.build_number == BUILD),
        "{checked:?}"
    );
    assert_eq!(lane.load().unwrap().min_build, BUILD);

    // Build 12, whose appcast yanks build 11, served with a zip one byte longer than it signs.
    let mut newer = windows_release("0.11.1", 12, 'c');
    newer.alter("build_number = 12\n", "build_number = 12\nmin_build = 12\n");
    newer.zip.push(0);
    let fake = serve_published(&[newer, mac]);
    let why = check_through(&reader(&fake), &lane, OLDER_HERE).unwrap_err();
    assert!(why.contains("aterm-0.11.1-windows-x86_64.zip"), "{why}");
    assert_eq!(lane.load().unwrap().min_build, 12);
    assert_nothing_staged(&lane, "the yanked stage");
}

#[test]
fn authenticode_is_asked_only_when_a_publisher_is_pinned() {
    // Pinned, and both images signed by the pinned publisher: staged, each image asked once.
    let (_dir, lane) = scratch_lane("authenticode");
    let asked = Cell::new(0);
    let root = lane.root().to_path_buf();
    let signed_by = |who: &'static str| {
        let (asked, root) = (&asked, &root);
        move |image: &Path| -> Result<String, String> {
            assert!(image.starts_with(root), "{}", image.display());
            asked.set(asked.get() + 1);
            Ok(who.to_string())
        }
    };
    let checked = run(
        &Release::good(),
        &lane,
        OLDER_HERE,
        PUBLISHER,
        &signed_by(PUBLISHER),
    )
    .unwrap();
    let Checked::Staged(staged) = checked else {
        panic!("not staged: {checked:?}");
    };
    assert_eq!(asked.get(), 2, "aterm.exe and aterm-gui.exe");
    lane.verify_stage(&staged, PUBLISHER, &signed_by(PUBLISHER))
        .unwrap();
    // Read back before its install, an image another publisher signed is refused.
    let why = lane
        .verify_stage(&staged, PUBLISHER, &signed_by("Someone Else"))
        .unwrap_err();
    assert!(
        why.contains("signed by \"Someone Else\", not the pinned publisher"),
        "{why}"
    );
    // Unpinned, the same folder reads back without asking.
    lane.verify_stage(&staged, "", &unasked).unwrap();

    // Pinned, and an image signed by another publisher, or not validly: nothing staged.
    let refusals: [(&str, Result<String, String>, &str); 2] = [
        (
            "another publisher",
            Ok("Someone Else".to_string()),
            "aterm.exe is signed by \"Someone Else\", not the pinned publisher \"Fixture Publisher\"",
        ),
        (
            "no valid signature",
            Err("its signature is NotSigned".to_string()),
            "aterm.exe's Authenticode signature refused: its signature is NotSigned",
        ),
    ];
    for (what, says, expect) in refusals {
        let (_dir, lane) = scratch_lane("authenticode-refused");
        let signer = |_: &Path| says.clone();
        let why = run(&Release::good(), &lane, OLDER_HERE, PUBLISHER, &signer).expect_err(what);
        assert!(why.contains(expect), "{what}: {why}");
        assert_nothing_staged(&lane, what);
    }
}

#[test]
fn a_staged_folder_is_read_back_against_its_receipt_before_its_install() {
    let (dir, lane) = scratch_lane("receipt");
    let Ok(Checked::Staged(staged)) = run(&Release::good(), &lane, OLDER_HERE, "", &unasked) else {
        panic!("the fixture release stages");
    };
    let before =
        |staged: &StagedBuild| crate::windows::before_install(staged, Some(&lane), "", &unasked);
    assert_eq!(before(&staged), Ok(Origin::Channel));

    let image = staged.dir.join("aterm.exe");
    let original = fs::read(&image).unwrap();
    let mut changed = original.clone();
    changed[600] ^= 1;
    fs::write(&image, &changed).unwrap();
    let why = before(&staged).unwrap_err();
    assert!(
        why.contains("is not installed: the staged aterm.exe changed since it was verified"),
        "{why}"
    );
    fs::write(&image, &original).unwrap();
    assert_eq!(before(&staged), Ok(Origin::Channel));

    fs::write(staged.dir.join("extra.dll"), b"planted").unwrap();
    let why = before(&staged).unwrap_err();
    assert!(
        why.contains("holds extra.dll, which the verified zip did not"),
        "{why}"
    );
    fs::remove_file(staged.dir.join("extra.dll")).unwrap();

    fs::create_dir(staged.dir.join("plugins")).unwrap();
    let why = before(&staged).unwrap_err();
    assert!(why.contains("holds the folder plugins"), "{why}");
    fs::remove_dir(staged.dir.join("plugins")).unwrap();

    fs::remove_file(staged.dir.join("licenses").join("MIT.txt")).unwrap();
    let why = before(&staged).unwrap_err();
    assert!(why.contains("the staged licenses/MIT.txt is gone"), "{why}");
    fs::write(staged.dir.join("licenses").join("MIT.txt"), b"MIT").unwrap();
    assert_eq!(before(&staged), Ok(Origin::Channel));

    // A build the pointer does not name is not the lane's stage.
    let other = StagedBuild {
        build_number: BUILD + 1,
        ..staged.clone()
    };
    let why = before(&other).unwrap_err();
    assert!(
        why.contains("stages aterm 0.11.0 (commit bbbbbbbbbbbb, build 11) now, not"),
        "{why}"
    );
    // A receipt of another zip is no receipt of this stage.
    let receipt_path = lane.receipt_path(COMMIT);
    let original_receipt = fs::read(&receipt_path).unwrap();
    let mut receipt: Receipt = aterm_json::from_slice(&original_receipt).unwrap();
    receipt.zip_sha256 = sha256(b"another zip");
    fs::write(&receipt_path, aterm_json::to_string(&receipt).unwrap()).unwrap();
    let why = before(&staged).unwrap_err();
    assert!(why.contains("receipt does not name"), "{why}");
    fs::write(&receipt_path, &original_receipt).unwrap();
    assert_eq!(before(&staged), Ok(Origin::Channel));

    // A folder that is not the lane's is the local build loop's, judged as before.
    let local = StagedBuild {
        dir: dir.path().join("dist").join(COMMIT),
        ..staged.clone()
    };
    assert_eq!(
        crate::windows::before_install(&local, Some(&lane), "", &unasked),
        Ok(Origin::Local)
    );
    assert_eq!(
        crate::windows::before_install(&staged, None, "", &unasked),
        Ok(Origin::Local)
    );
}

/// While an install holds the lane, a check stages nothing, prunes nothing and leaves the last
/// check's record as it was; once the hold ends, the lane checks again.
#[test]
fn a_held_lane_is_not_checked_until_its_holder_lets_go() {
    let (_dir, lane) = scratch_lane("held");
    let Ok(Checked::Staged(staged)) = run(&Release::good(), &lane, OLDER_HERE, "", &unasked) else {
        panic!("the fixture release stages");
    };
    let before = lane.load().unwrap();
    let held = lane.hold().unwrap();
    let why = run(&Release::good(), &lane, OLDER_HERE, "", &unasked).unwrap_err();
    assert_eq!(
        why,
        "another aterm is checking or installing the release channel's build right now"
    );
    assert_eq!(
        lane.load().unwrap(),
        before,
        "the refused check records nothing"
    );
    assert_eq!(lane.read_pointer(), Ok(Some(staged.clone())));
    drop(held);
    assert_eq!(
        run(&Release::good(), &lane, OLDER_HERE, "", &unasked),
        Ok(Checked::AlreadyStaged(staged))
    );
}

#[test]
fn an_automatic_check_is_due_half_an_hour_after_the_last() {
    let (_dir, lane) = scratch_lane("due");
    assert!(lane.due(NOW), "never checked");
    fs::create_dir_all(lane.root()).unwrap();
    lane.save(&State {
        checked_unix: NOW,
        ..State::default()
    })
    .unwrap();
    let every = i64::try_from(CHECK_EVERY.as_secs()).unwrap();
    assert!(!lane.due(NOW + 60));
    assert!(!lane.due(NOW + every - 1));
    assert!(lane.due(NOW + every));
    assert!(
        lane.due(NOW - 1),
        "a clock that stepped back past the last check"
    );
    fs::write(lane.root().join(STATE), b"not json").unwrap();
    assert!(
        lane.due(NOW + 60),
        "an unreadable record: the check says why"
    );
    let note = lane.note(false);
    assert!(note.outcome.contains("is unreadable"), "{note:?}");
    assert!(!note.automatic);
}

#[test]
fn get_authenticode_signature_s_answer_reads_as_a_signer_or_why_not() {
    assert_eq!(
        parse_signer("status=Valid\r\nsigner=Fixture Publisher\r\n"),
        Ok("Fixture Publisher".to_string())
    );
    assert_eq!(
        parse_signer("status=NotSigned\nsigner=\n"),
        Err("its signature is NotSigned".to_string())
    );
    assert_eq!(
        parse_signer("status=HashMismatch\nsigner=Fixture Publisher\n"),
        Err("its signature is HashMismatch".to_string())
    );
    assert!(parse_signer("status=Valid\nsigner=\n").is_err());
    assert!(parse_signer("").is_err());
    assert!(parse_signer("Get-AuthenticodeSignature : not recognized").is_err());
}

/// Set in the copy of this test binary that
/// [`windows_reads_a_signed_image_s_signer_and_an_unsigned_one_as_no_publisher_s`] starts under
/// a module path Windows PowerShell cannot load its security module from.
#[cfg(windows)]
const UNLOADABLE_MODULE_PATH: &str = "ATERM_TEST_UNLOADABLE_PSMODULEPATH";

/// The production reader against Windows' own judgement. The positive control first: Windows
/// PowerShell, which Microsoft signs, reads as signed by `Microsoft Windows`, so a reader that
/// cannot start PowerShell, whose script does not run or whose image never reaches it fails
/// here. Then a test binary nobody signed reads as Windows says it: not signed.
///
/// Both hold again in a copy of this test run as `aterm update check` typed at a PowerShell 7
/// prompt runs it: under a module path whose first folder holds a `Microsoft.PowerShell.Security`
/// Windows PowerShell cannot load (PowerShell 7 hands its children its own build of it there).
#[cfg(windows)]
#[test]
fn windows_reads_a_signed_image_s_signer_and_an_unsigned_one_as_no_publisher_s() {
    assert_eq!(
        authenticode_signer(&crate::windows::powershell()),
        Ok("Microsoft Windows".to_string())
    );
    let exe = std::env::current_exe().unwrap();
    assert_eq!(
        authenticode_signer(&exe),
        Err("its signature is NotSigned".to_string())
    );
    if std::env::var_os(UNLOADABLE_MODULE_PATH).is_some() {
        return;
    }
    let dir = crate::scratch_dir("psmodulepath");
    let module = dir.path().join("Microsoft.PowerShell.Security");
    fs::create_dir_all(&module).unwrap();
    fs::write(
        module.join("Microsoft.PowerShell.Security.psd1"),
        "@{ ModuleVersion = '7.0.0.0'; GUID = 'a94c8c7e-9810-47c0-b8af-65089c13a35a'; \
         PowerShellVersion = '7.0'; CompatiblePSEditions = @('Core'); \
         CmdletsToExport = @('Get-AuthenticodeSignature') }\r\n",
    )
    .unwrap();
    let mut path = dir.path().as_os_str().to_os_string();
    path.push(";");
    path.push(std::env::var_os("PSModulePath").unwrap_or_default());
    let name = "windows_channel::tests::\
                windows_reads_a_signed_image_s_signer_and_an_unsigned_one_as_no_publisher_s";
    let mut copy = aterm_uds::spawnfd::command(&exe);
    copy.args(["--exact", name, "--test-threads=1"])
        .env("PSModulePath", &path)
        .env(UNLOADABLE_MODULE_PATH, "1");
    let output = aterm_uds::deadline::output_within(&mut copy, Duration::from_secs(60))
        .expect("the copy under the unloadable module path ends");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success() && said.contains("test result: ok. 1 passed"),
        "{said}"
    );
}
