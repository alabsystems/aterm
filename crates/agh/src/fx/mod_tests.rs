// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::Path;

use astream_effects::EffectRecord;

use super::child::{ChildOut, ChildSpec};
use super::tape::{TapeHeader, TapeMode};
use super::{DOOR_EXIT, DOOR_FABRIC, DOOR_GIT, DOOR_JOURNAL, DOOR_ROUTE, Fx, canon, read_tape};
use crate::error::{Replay, Sub};
use crate::notify::Posted;
use crate::policy::{Decision, RouteRequest, RouteTarget, Tier};
use crate::redact::Redactor;
use crate::slug::Slug;
use crate::wire::Endpoints;

#[cfg(unix)]
fn pipe_file(reader: &std::io::PipeReader) -> std::fs::File {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(super::NONBLOCK_OPEN_FLAG)
        .open(format!("/dev/fd/{}", reader.as_raw_fd()))
        .expect("nonblocking pipe")
}

#[cfg(unix)]
#[test]
fn a_scheduled_pipe_writer_is_read_to_its_end() {
    use std::io::Write as _;
    use std::time::Duration;
    let (reader, mut writer) = aterm_uds::pipe().expect("pipe");
    let file = pipe_file(&reader);
    writer.write_all(b"fixture tape").expect("whole tape");
    drop(writer);
    let got = super::read_pipe_at(
        &file,
        super::TAPE_STREAM_HANG,
        std::time::Instant::now() - Duration::from_secs(6),
    );
    assert_eq!(got.expect("the writer's complete answer"), b"fixture tape");
    let expired = super::read_pipe_at(
        &file,
        super::TAPE_STREAM_HANG,
        std::time::Instant::now() - Duration::from_secs(61),
    )
    .expect_err("a whole minute without a completed read ends the wait");
    assert_eq!(expired.kind(), std::io::ErrorKind::TimedOut);
}

#[cfg(unix)]
#[test]
fn a_pipe_writer_that_never_finishes_expires() {
    use std::io::{ErrorKind, Write as _};
    use std::time::Duration;

    let (reader, mut writer) = aterm_uds::pipe().expect("pipe");
    writer.write_all(b"partial tape").expect("partial input");
    let file = pipe_file(&reader);
    let error = super::read_pipe(&file, Duration::from_millis(10))
        .expect_err("an open writer cannot hold replay forever");
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    drop(writer);
    drop(reader);
}

fn scratch() -> aterm_tempfile::TempDir {
    crate::test_fixtures::scratch_dir("fx")
}

/// A streamed asset's GET, as `dev`: the request the streamed-body tests tape.
#[cfg(unix)]
fn asset_request() -> crate::wire::WireRequest<'static> {
    let mut req = crate::wire::WireRequest::new(
        crate::wire::Method::Get,
        "https://api.github.com/repos/o/r/releases/assets/1",
        vec![("Accept".to_string(), "application/octet-stream".to_string())],
        1 << 20,
        crate::wire::Deadlines::JSON,
    );
    req.as_profile = Some("dev");
    req
}

fn header(mode: TapeMode) -> TapeHeader {
    let argv: Vec<String> = ["repo", "view", "o/r"].map(String::from).to_vec();
    TapeHeader {
        mode,
        ..TapeHeader::for_run(&argv, &Endpoints::github_com())
    }
}

fn records(path: &Path) -> Vec<EffectRecord> {
    read_tape(path).expect("the tape reads").1
}

fn write(path: &Path, mode: TapeMode, records: &[EffectRecord]) {
    std::fs::write(path, super::tape::render(&header(mode), records)).expect("the tape");
}

fn exit() -> EffectRecord {
    EffectRecord::Emit {
        door: DOOR_EXIT.to_string(),
        bytes: b"0".to_vec(),
    }
}

fn route() -> RouteRequest {
    RouteRequest::read(RouteTarget::Repo(Slug::parse("o/r").expect("a slug")))
        .with_as(Some("dev".to_string()))
}

fn sent() -> Decision {
    Decision::Send {
        profile: "dev".to_string(),
        tier: Tier::Dev,
    }
}

/// A credentialed recording tapes each decision; its replay answers it, never deciding,
/// and another question is a request divergence.
#[test]
fn a_credentialed_tape_replays_the_route_without_deciding() {
    let dir = scratch();
    let path = dir.path().join("t.atape");
    let fx = Fx::record_with(&path, &header(TapeMode::Credentialed), Redactor::new())
        .expect("a recording");
    let mut decided = 0;
    let decision = fx
        .route(&route(), &mut || {
            decided += 1;
            sent()
        })
        .expect("decided");
    assert_eq!((decision, decided), (sent(), 1));
    fx.finish(0).expect("the tape is whole");
    assert_eq!(
        records(&path),
        [
            EffectRecord::Exchange {
                door: DOOR_ROUTE.to_string(),
                request: canon::route_request(&route()),
                reply: b"send dev dev".to_vec(),
            },
            exit()
        ]
    );

    let (replay, _) = Fx::replay(&path).expect("a replay");
    let never = &mut || -> Decision { panic!("a replay never decides") };
    assert_eq!(replay.route(&route(), never).expect("taped"), sent());
    replay.finish(0).expect("replayed whole");

    let (other, _) = Fx::replay(&path).expect("a replay");
    let refused = other
        .route(&route().with_as(None), never)
        .expect_err("another question");
    assert_eq!(refused.sub, Sub::Replay(Replay::RequestDiverged));

    let again = dir.path().join("again.atape");
    let (rerecord, _) = Fx::rerecord(&path, &again).expect("a rerecording");
    assert_eq!(rerecord.route(&route(), never).expect("taped"), sent());
    rerecord.finish(0).expect("rerecorded whole");
    assert_eq!(records(&again), records(&path));
}

/// An anonymous tape decides every time and tapes no route record, in every tape mode.
#[test]
fn an_anonymous_tape_never_tapes_the_route() {
    let dir = scratch();
    let path = dir.path().join("t.atape");
    let fx = Fx::record(&path, &header(TapeMode::Anonymous)).expect("a recording");
    assert_eq!(fx.route(&route(), &mut sent).expect("decided"), sent());
    fx.finish(0).expect("whole");
    assert_eq!(records(&path), [exit()]);
    let (replay, _) = Fx::replay(&path).expect("a replay");
    assert_eq!(replay.route(&route(), &mut sent).expect("decided"), sent());
    replay.finish(0).expect("replayed whole");
}

/// The journal's and the fabric's doors are emits under a tape: the journal's step is the
/// tape's own count from 0, the fabric's `send` never runs, and a replay compares both.
#[test]
fn the_journal_and_fabric_doors_emit_under_a_tape() {
    let dir = scratch();
    let path = dir.path().join("t.atape");
    let line = "@self post to=say:agh.write kind=note key=agh-1 -- v=1";
    let unsent = &mut || -> Posted { panic!("a tape never posts") };
    let fx = Fx::record(&path, &header(TapeMode::Anonymous)).expect("a recording");
    assert_eq!(fx.emit_journal(b"{\"intent\":1}").expect("taped"), Some(0));
    assert_eq!(fx.emit_journal(b"{\"outcome\":1}").expect("taped"), Some(1));
    assert_eq!(
        fx.emit_fabric(line, unsent).expect("taped"),
        Posted::Skipped("taped".to_string())
    );
    fx.finish(0).expect("whole");
    let emit = |door: &str, bytes: &[u8]| EffectRecord::Emit {
        door: door.to_string(),
        bytes: bytes.to_vec(),
    };
    assert_eq!(
        records(&path),
        [
            emit(DOOR_JOURNAL, b"{\"intent\":1}"),
            emit(DOOR_JOURNAL, b"{\"outcome\":1}"),
            emit(DOOR_FABRIC, line.as_bytes()),
            exit()
        ]
    );

    let (replay, _) = Fx::replay(&path).expect("a replay");
    assert_eq!(
        replay.emit_journal(b"{\"intent\":1}").expect("same"),
        Some(0)
    );
    assert_eq!(
        replay.emit_journal(b"{\"outcome\":1}").expect("same"),
        Some(1)
    );
    assert_eq!(
        replay.emit_fabric(line, unsent).expect("same"),
        Posted::Skipped("taped".to_string())
    );
    replay.finish(0).expect("replayed whole");

    let (other, _) = Fx::replay(&path).expect("a replay");
    let refused = other
        .emit_journal(b"{\"intent\":2}")
        .expect_err("another body");
    assert!(matches!(refused.sub, Sub::Replay(_)), "{refused:?}");

    let simulated = Fx::simulated(0);
    assert_eq!(simulated.emit_journal(b"x").expect("untaped"), None);
    let mut posted = 0;
    let answer = simulated
        .emit_fabric(line, &mut || {
            posted += 1;
            Posted::Sent("1".to_string())
        })
        .expect("sent");
    assert_eq!((answer, posted), (Posted::Sent("1".to_string()), 1));
}

/// A replayed git run is the tape's answer and runs nothing: the spec names the bare `git`,
/// which `child::check` would refuse. A recording that cannot run tapes its failure, which
/// replays as the same error.
#[test]
fn the_git_door_replays_with_nothing_run() {
    let dir = scratch();
    let path = dir.path().join("t.atape");
    let out = ChildOut {
        code: Some(0),
        stdout: b"0123\trefs/tags/v1\n".to_vec(),
        stderr: Vec::new(),
    };
    let canonical = b"git ls-remote o/r\nref refs/tags/v1";
    let exchange = EffectRecord::Exchange {
        door: DOOR_GIT.to_string(),
        request: canonical.to_vec(),
        reply: canon::git_reply(&out),
    };
    write(&path, TapeMode::Credentialed, &[exchange.clone(), exit()]);
    let bare = ChildSpec::new(Path::new("git"));

    let (replay, _) = Fx::replay(&path).expect("a replay");
    assert_eq!(replay.git(&bare, canonical).expect("taped"), out);
    replay.finish(0).expect("replayed whole");

    let (other, _) = Fx::replay(&path).expect("a replay");
    let refused = other
        .git(&bare, b"git ls-remote o/r")
        .expect_err("another run");
    assert_eq!(refused.sub, Sub::Replay(Replay::RequestDiverged));

    let again = dir.path().join("again.atape");
    let (rerecord, _) = Fx::rerecord(&path, &again).expect("a rerecording");
    assert_eq!(rerecord.git(&bare, canonical).expect("taped"), out);
    rerecord.finish(0).expect("rerecorded whole");
    assert_eq!(records(&again), [exchange, exit()]);

    let unrun = dir.path().join("unrun.atape");
    let fx = Fx::record(&unrun, &header(TapeMode::Credentialed)).expect("a recording");
    let refused = fx.git(&bare, canonical).expect_err("a relative git");
    assert_eq!(refused.sub, Sub::Local);
    fx.finish(1).expect("whole");
    let unrun_records = records(&unrun);
    let failed = EffectRecord::Exchange {
        door: DOOR_GIT.to_string(),
        request: canonical.to_vec(),
        reply: canon::git_failure(&refused),
    };
    assert_eq!(unrun_records.first(), Some(&failed), "{unrun_records:?}");
    let (replay, _) = Fx::replay(&unrun).expect("a replay");
    let replayed = replay.git(&bare, canonical).expect_err("the taped failure");
    assert_eq!(
        (
            replayed.kind,
            replayed.sub.clone(),
            replayed.side_effect,
            replayed.why.clone()
        ),
        (
            refused.kind,
            refused.sub.clone(),
            refused.side_effect,
            refused.why.clone()
        )
    );
    replay.finish(1).expect("replayed whole");
}

/// A routing fact whose answer holds a token shape is never taped: the recording refuses
/// it, latches, and discards the tape, whose fact would replay `[redacted]`.
#[test]
fn a_shaped_fact_discards_the_recording() {
    let dir = scratch();
    let path = dir.path().join("t.atape");
    let fx = Fx::record_with(&path, &header(TapeMode::Credentialed), Redactor::new())
        .expect("a recording");
    let token = agh_fake::token("fact");
    let refused = fx
        .fact("login", "dev", &mut || token.clone())
        .expect_err("a shaped answer");
    assert_eq!(refused.sub, Sub::Replay(Replay::StreamNotTaped));
    let mut plain = || "fixture-dev-bot".to_string();
    assert!(fx.fact("login", "dev", &mut plain).is_err(), "latched");
    assert!(fx.finish(0).is_err(), "a discarded tape is not whole");
    assert!(!path.exists(), "the tape is removed");
}

/// A child is never taped, in any tape mode.
#[test]
fn a_child_is_never_taped() {
    let dir = scratch();
    let path = dir.path().join("t.atape");
    let fx = Fx::record_with(&path, &header(TapeMode::Credentialed), Redactor::new())
        .expect("a recording");
    let refused = fx
        .child(&ChildSpec::new(Path::new("/bin/gh")))
        .expect_err("not taped");
    assert_eq!(refused.sub, Sub::Replay(Replay::StreamNotTaped));
}

/// A credentialed streamed answer is taped by its digest: a 0600 file in a 0700
/// `{tape}.d`, a `body-file` line on the tape, a replay that streams the same bytes, a
/// rerecording that copies the file, and `Replay::StreamDigest` for a file that is
/// missing or holds other bytes.
#[cfg(unix)]
#[test]
fn a_streamed_body_is_taped_by_its_digest() {
    use std::io::Read as _;
    use std::os::unix::fs::PermissionsExt as _;

    use crate::wire::scripted::ScriptedTransport;

    let e = Endpoints::github_com();
    let request = asset_request;
    let body = b"the bytes of one asset".to_vec();
    let hex = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&body));
    let read = |fx: &Fx, transport: &ScriptedTransport| {
        let mut answer = fx
            .gh_stream(transport, request())
            .expect("the door")
            .expect("an answer");
        let mut got = Vec::new();
        answer.body.read_to_end(&mut got).map(|_| got)
    };

    let dir = scratch();
    let path = dir.path().join("t.atape");
    let sidecars = super::tape::sidecar_dir(&path);
    // A partial file a killed recording left is in the way of nothing.
    aterm_uds::private_file::create_private_dir(&sidecars).expect("the body directory");
    std::fs::write(sidecars.join(".partial-0"), b"a killed run's").expect("a stale partial");
    let transport = ScriptedTransport::new();
    transport.answer(200, &[("Content-Type", "application/octet-stream")], &body);
    let fx = Fx::record_with(&path, &header(TapeMode::Credentialed), Redactor::new())
        .expect("a recording")
        .with_endpoints(&e);
    assert_eq!(read(&fx, &transport).expect("streams"), body);
    fx.finish(0).expect("whole");

    let mode = |p: &Path| std::fs::metadata(p).expect("there").permissions().mode() & 0o777;
    assert_eq!(mode(&sidecars), 0o700);
    assert_eq!(mode(&sidecars.join(&hex)), 0o600);
    assert_eq!(
        std::fs::read_dir(&sidecars).expect("the directory").count(),
        1,
        "no partial file is left"
    );
    let taped = records(&path);
    let Some(EffectRecord::Exchange { reply, .. }) = taped.first() else {
        panic!("an exchange first: {taped:?}");
    };
    let reply = String::from_utf8(reply.clone()).expect("UTF-8");
    assert!(
        reply.ends_with(&format!("\nbody-file sha256:{hex} {}", body.len())),
        "{reply}"
    );

    let silent = ScriptedTransport::new();
    let (replay, _) = Fx::replay(&path).expect("a replay");
    assert_eq!(
        read(&replay.with_endpoints(&e), &silent).expect("streams"),
        body
    );

    let again = dir.path().join("again.atape");
    let (rerecord, _) = Fx::rerecord(&path, &again).expect("a rerecording");
    let rerecord = rerecord.with_endpoints(&e);
    assert_eq!(read(&rerecord, &silent).expect("streams"), body);
    rerecord.finish(0).expect("rerecorded whole");
    assert_eq!(
        std::fs::read(super::tape::sidecar_dir(&again).join(&hex)).expect("the copy"),
        body
    );
    assert!(silent.requests().is_empty());

    let mut other = body.clone();
    other[0] ^= 1;
    std::fs::write(sidecars.join(&hex), &other).expect("a changed body");
    let (replay, _) = Fx::replay(&path).expect("a replay");
    let replay = replay.with_endpoints(&e);
    assert!(read(&replay, &silent).is_err(), "the read fails at its end");
    let refused = replay.finish(0).expect_err("the latched digest");
    assert_eq!(refused.sub, Sub::Replay(Replay::StreamDigest));

    std::fs::remove_file(sidecars.join(&hex)).expect("removed");
    let (replay, _) = Fx::replay(&path).expect("a replay");
    let refused = replay
        .with_endpoints(&e)
        .gh_stream(&silent, request())
        .expect_err("no body file");
    assert_eq!(refused.sub, Sub::Replay(Replay::StreamDigest));

    // A link in its place is not followed, even to the taped bytes.
    let elsewhere = dir.path().join("elsewhere");
    std::fs::write(&elsewhere, &body).expect("the bytes elsewhere");
    std::os::unix::fs::symlink(&elsewhere, sidecars.join(&hex)).expect("a link");
    let (replay, _) = Fx::replay(&path).expect("a replay");
    let refused = replay
        .with_endpoints(&e)
        .gh_stream(&silent, request())
        .expect_err("a link is no body file");
    assert_eq!(refused.sub, Sub::Replay(Replay::StreamDigest));
    assert!(silent.requests().is_empty());
}

/// A streamed body that holds a credential the run learned, or a token shape, is not
/// taped: the recording stops with `Replay::BodyNotTaped` before its caller reads a byte,
/// and removes its tape and every body file it stored, `{tape}.d` with them.
#[cfg(unix)]
#[test]
fn a_streamed_body_holding_a_credential_is_not_taped() {
    use crate::wire::scripted::ScriptedTransport;

    let e = Endpoints::github_com();
    let request = asset_request;
    let learned = b"a-learned-credential-of-no-shape".to_vec();
    let shaped = agh_fake::token("streamed-body");
    let mut far = vec![b'x'; 70_000];
    far.extend_from_slice(shaped.as_bytes());
    for (label, body) in [
        (
            "learned",
            [b"head ".as_slice(), learned.as_slice(), b" tail".as_slice()].concat(),
        ),
        ("a shape past the first read", far),
    ] {
        let dir = scratch();
        let path = dir.path().join("t.atape");
        let redactor = Redactor::new();
        redactor.learn(&learned);
        let transport = ScriptedTransport::new();
        transport.answer(
            200,
            &[("Content-Type", "application/octet-stream")],
            b"first",
        );
        transport.answer(200, &[("Content-Type", "application/octet-stream")], &body);
        let fx = Fx::record_with(&path, &header(TapeMode::Credentialed), redactor)
            .expect("a recording")
            .with_endpoints(&e);
        let mut first = fx
            .gh_stream(&transport, request())
            .expect("the door")
            .expect("an answer");
        let mut got = Vec::new();
        std::io::Read::read_to_end(&mut first.body, &mut got).expect("the first body");
        assert!(super::tape::sidecar_dir(&path).exists(), "{label}");
        let refused = fx.gh_stream(&transport, request()).expect_err("not taped");
        assert_eq!(refused.sub, Sub::Replay(Replay::BodyNotTaped), "{label}");
        assert!(fx.finish(0).is_err(), "{label}");
        assert!(!path.exists(), "{label}: the tape is removed");
        assert!(
            !super::tape::sidecar_dir(&path).exists(),
            "{label}: its bodies are removed"
        );
    }
}

/// An anonymous tape keeps M1's refusal of a streamed answer.
#[test]
fn an_anonymous_tape_never_streams() {
    use crate::wire::scripted::ScriptedTransport;
    use crate::wire::{Deadlines, Method, WireRequest};

    let dir = scratch();
    let path = dir.path().join("t.atape");
    let fx = Fx::record(&path, &header(TapeMode::Anonymous))
        .expect("a recording")
        .with_endpoints(&Endpoints::github_com());
    let transport = ScriptedTransport::new();
    let req = WireRequest::new(
        Method::Get,
        "https://api.github.com/repos/o/r/releases/assets/1",
        Vec::new(),
        1 << 20,
        Deadlines::JSON,
    );
    let refused = fx.gh_stream(&transport, req).expect_err("not taped");
    assert_eq!(refused.sub, Sub::Replay(Replay::StreamNotTaped));
    assert!(transport.requests().is_empty());
}
