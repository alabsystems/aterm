// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Fabric posts: the notifier and `--notify` over a test socket (WP9).

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use agh::config::Dirs;
use agh::fx::Fx;
use agh::github::releases::{
    DraftRequest, MakeLatest, PublishGuard, PublishPatch, Releases, Replace, UploadFile,
};
use agh::identity::MapEnv;
use agh::journal::JournalRecord;
use agh::model::ReleaseObject;
use agh::notify::{Notice, Notifier, Posted, RecordingNotifier, SessionNotifier, Topic};
use agh::policy::{Permit, RouteRequest, RouteTarget};
use agh::retry::RetryPolicy;
use agh::wire::http::HttpTransport;
use agh::write::{Deleted, Effect, Ensured, Outcome};
use agh::{Github, GithubBuilder, How, IntentId};
use agh_fake::{FakeGithub, Fault, Match, ReleaseSpec};
use aterm_json::Value;

use crate::common::{self, HANG, SIM_START, slug};

const SID: &str = "s-0123456789abcdef";

/// What the test broker does after it has read one line past `AUTH`.
enum Reply {
    /// Write this line and its `\n`.
    Line(String),
    /// Write these bytes as they are.
    Raw(Vec<u8>),
}

/// A broker on a scratch socket: `{dir}/aterm-4242.sock`, its token beside it.
struct Broker {
    _dir: aterm_tempfile::TempDir,
    sock: PathBuf,
    token: String,
    listener: UnixListener,
}

impl Broker {
    /// A listener with its token file.
    fn new() -> Broker {
        let broker = Broker::without_token();
        aterm_uds::private_file::write_private(
            broker.sock.parent().expect("the socket's directory"),
            "aterm-4242.token",
            format!("{}\n", broker.token).as_bytes(),
        )
        .expect("the token file");
        broker
    }

    /// A listener with no token beside it.
    fn without_token() -> Broker {
        let dir = aterm_tempfile::Builder::new()
            .prefix("agh-n")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("a scratch directory under /tmp");
        let sock = dir.path().join("aterm-4242.sock");
        let listener = UnixListener::bind(&sock).expect("the test socket");
        Broker {
            sock,
            token: aterm_uds::rand::hex_token::<16>().expect("a token"),
            listener,
            _dir: dir,
        }
    }

    /// The notifier of `SID` on this socket, each phase bounded by the hang detector: this
    /// broker answers, so a serve thread a loaded machine runs late is no timeout.
    fn notifier(&self) -> SessionNotifier {
        SessionNotifier::at_socket(&self.sock, SID).with_bound_for_tests(HANG)
    }

    /// Accept one connection on a thread: record each line, answer nothing to the first
    /// (`AUTH`), and answer each later one with the next reply. With `close_after_auth`, the
    /// connection closes once `AUTH` is read. The thread ends at the stream's end, or after
    /// 60 s of silence (the hang detector), and sends what it heard; `heard` waits for that
    /// at most 60 s, so a notifier that never connects fails the test instead of hanging it.
    fn serve(&self, replies: Vec<Reply>, close_after_auth: bool) -> Receiver<Vec<String>> {
        let listener = self.listener.try_clone().expect("the listener");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("one connection");
            // macOS refuses the options (EINVAL) once the peer has closed, and a read or write
            // of a closed peer's stream cannot block, so that refusal needs no detector.
            let _ = stream.set_read_timeout(Some(HANG));
            let _ = stream.set_write_timeout(Some(HANG));
            let mut writer = stream.try_clone().expect("the stream");
            let mut reader = BufReader::new(stream);
            let mut replies = replies.into_iter();
            let mut heard = Vec::new();
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                heard.push(line.trim_end_matches('\n').to_string());
                if heard.len() == 1 {
                    if close_after_auth {
                        break;
                    }
                    continue;
                }
                let bytes = match replies.next() {
                    Some(Reply::Line(text)) => format!("{text}\n").into_bytes(),
                    Some(Reply::Raw(bytes)) => bytes,
                    None => continue,
                };
                let _ = writer.write_all(&bytes);
            }
            let _ = tx.send(heard);
        });
        rx
    }

    /// The notifier connected once and wrote nothing. `post` has returned, so its connection
    /// waits in the backlog already, closed: nothing here blocks.
    fn connected_silently(&self) {
        let (mut stream, _) = aterm_uds::accept(
            &self.listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("the notifier's connection");
        // As in `serve`: refused once the peer has closed, when a read cannot block.
        let _ = stream.set_read_timeout(Some(HANG));
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).expect("the stream's end");
        assert!(bytes.is_empty(), "{}", String::from_utf8_lossy(&bytes));
    }

    /// No connection is waiting: the notifier never connected.
    fn never_connected(&self) {
        let err = aterm_uds::accept(
            &self.listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect_err("no connection");
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
    }
}

fn line(text: &str) -> Reply {
    Reply::Line(text.to_string())
}

const CONNECTED: &str = "OK schema=1 sid=s-0123456789abcdef fabric=connected";

fn notice() -> Notice {
    Notice::converged(
        &slug("fixture-dev/app"),
        "v1",
        "uploaded",
        Some(1),
        Some(5000),
    )
}

fn post(notifier: &SessionNotifier, notice: &Notice) -> Posted {
    notifier.post(&Fx::simulated(SIM_START), notice)
}

/// What the broker heard, within the 60 s hang detector.
fn heard(rx: Receiver<Vec<String>>) -> Vec<String> {
    match rx.recv_timeout(HANG) {
        Ok(heard) => heard,
        Err(RecvTimeoutError::Timeout) => panic!("hang: the broker heard no end in 60 s"),
        Err(RecvTimeoutError::Disconnected) => panic!("the broker thread panicked"),
    }
}

/// The broker's key rule: 1–64 bytes of `[A-Za-z0-9._:-]`.
fn broker_key(key: &str) -> bool {
    (1..=64).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn hashed(bytes: &[u8]) -> String {
    let digest = aterm_digest::Sha256::digest(bytes);
    format!("agh-{}", &aterm_codec::hex::encode_lower(&digest)[..16])
}

#[test]
fn a_post_runs_the_six_steps() {
    let broker = Broker::new();
    let notice = notice();
    let handle = broker.serve(vec![line(CONNECTED), line("OK 17")], false);
    assert_eq!(post(&broker.notifier(), &notice), Posted::Sent("17".into()));
    assert_eq!(
        heard(handle),
        [
            format!("AUTH {}", broker.token),
            format!("@{SID} status"),
            notice.line(SID),
        ]
    );
}

#[test]
fn a_disconnected_fabric_is_skipped_and_posts_nothing() {
    let broker = Broker::new();
    let handle = broker.serve(
        vec![line("OK schema=1 sid=s-0123456789abcdef fabric=absent")],
        false,
    );
    assert_eq!(
        post(&broker.notifier(), &notice()),
        Posted::Skipped("fabric=absent".into())
    );
    assert_eq!(heard(handle).len(), 2);

    for status in [
        "OK schema=1 sid=s-0123456789abcdef",
        "OK schema=1 sid=s-0123456789abcdef fabric=Connected",
        "OK schema=1 sid=s-0123456789abcdef fabric=connected,",
    ] {
        let broker = Broker::new();
        let handle = broker.serve(vec![line(status)], false);
        assert_eq!(
            post(&broker.notifier(), &notice()),
            Posted::Skipped("fabric=unknown".into()),
            "{status}"
        );
        assert_eq!(heard(handle).len(), 2);
    }
}

#[test]
fn an_err_reply_is_failed() {
    let broker = Broker::new();
    let handle = broker.serve(
        vec![line(CONNECTED), line("ERR halted: the broker is draining")],
        false,
    );
    assert_eq!(
        post(&broker.notifier(), &notice()),
        Posted::Failed("halted".into())
    );
    assert_eq!(heard(handle).len(), 3);
}

#[test]
fn a_bad_token_is_failed() {
    let broker = Broker::new();
    let handle = broker.serve(vec![line("ERR auth: bad token")], false);
    let posted = post(&broker.notifier(), &notice());
    assert_eq!(posted, Posted::Failed("auth".into()));
    assert_eq!(heard(handle).len(), 2);
    let receipt = posted.receipt();
    assert_eq!(receipt, "failed:auth");
    assert!(!receipt.contains(&broker.token));
    assert!(!broker.token.contains(&receipt));
}

#[test]
fn no_socket_is_failed() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("agh-n")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("a scratch directory under /tmp");
    let sock = dir.path().join("aterm-4242.sock");
    let notifier = SessionNotifier::at_socket(&sock, SID);
    assert_eq!(
        post(&notifier, &notice()),
        Posted::Failed("no-socket".into())
    );
}

#[test]
fn a_missing_token_is_failed() {
    let broker = Broker::without_token();
    assert_eq!(
        post(&broker.notifier(), &notice()),
        Posted::Failed("no-token".into())
    );
    broker.connected_silently();
}

#[test]
fn a_silent_server_times_out() {
    let broker = Broker::new();
    let handle = broker.serve(Vec::new(), false);
    let notifier = broker
        .notifier()
        .with_bound_for_tests(Duration::from_millis(200));
    assert_eq!(post(&notifier, &notice()), Posted::Failed("timeout".into()));
    assert_eq!(heard(handle).len(), 2);
    // The run has paid its bound: a later post answers the same, connecting nowhere.
    assert_eq!(post(&notifier, &notice()), Posted::Failed("timeout".into()));
    broker.never_connected();
}

#[test]
fn a_line_break_or_bad_key_is_never_sent() {
    let broker = Broker::new();
    // A constructor encodes a write's event, so only a field set by hand can break a line.
    let encoded = Notice::converged(&slug("fixture-dev/app"), "v1", "uploaded\nx", Some(1), None);
    assert!(!encoded.line(SID).contains('\n'), "{}", encoded.line(SID));
    let mut broken = notice();
    broken.event = "uploaded\nx".to_string();
    assert_eq!(
        post(&broker.notifier(), &broken),
        Posted::Failed("framing".into())
    );
    let mut keyed = notice();
    keyed.key = "agh-a b".to_string();
    assert_eq!(
        post(&broker.notifier(), &keyed),
        Posted::Failed("key".into())
    );
    broker.never_connected();
}

#[test]
fn an_oversized_or_unframed_reply_is_failed() {
    let broker = Broker::new();
    let mut big = vec![b'a'; (1 << 20) + 1];
    big.push(b'\n');
    let handle = broker.serve(vec![Reply::Raw(big)], false);
    assert_eq!(
        post(&broker.notifier(), &notice()),
        Posted::Failed("oversized".into())
    );
    heard(handle);

    let broker = Broker::new();
    let handle = broker.serve(
        vec![line(
            "OK schema=1 sid=s-0123456789abcdef fabric=connected\r",
        )],
        false,
    );
    assert_eq!(
        post(&broker.notifier(), &notice()),
        Posted::Failed("framing".into())
    );
    heard(handle);

    let broker = Broker::new();
    let handle = broker.serve(Vec::new(), true);
    assert_eq!(
        post(&broker.notifier(), &notice()),
        Posted::Failed("closed".into())
    );
    assert_eq!(heard(handle).len(), 1);

    let broker = Broker::new();
    let tag = "t".repeat(4097);
    let big = Notice::converged(&slug("fixture-dev/app"), &tag, "uploaded", Some(1), None);
    assert_eq!(
        post(&broker.notifier(), &big),
        Posted::Failed("oversized".into())
    );
    broker.never_connected();
}

#[test]
fn no_session_and_mux_are_skipped() {
    let cases = [
        (MapEnv::new(), "no-session"),
        (
            MapEnv::new().with("ATERM_PARENT_SESSION_ID", "S-01"),
            "no-session",
        ),
        (
            MapEnv::new()
                .with("ATERM_PARENT_SESSION_ID", "s-01")
                .with("ATERM_MUX", "1"),
            "mux",
        ),
        (
            MapEnv::new()
                .with("ATERM_PARENT_SESSION_ID", "s-01")
                .with("ATERM_MUX_BASE", "|")
                .with("TMUX", "/tmp/tmux-501/default,1,0"),
            "mux",
        ),
        (
            MapEnv::new()
                .with("ATERM_PARENT_SESSION_ID", "s-01")
                .with("TERM", "screen-256color"),
            "mux",
        ),
    ];
    // Every case is pinned to a socket no one listens on, so a skip that regressed fails as
    // `no-socket` and never reaches a running aterm's.
    let dir = aterm_tempfile::Builder::new()
        .prefix("agh-n")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("a scratch directory under /tmp");
    let dead = dir.path().join("aterm-4242.sock");
    for (env, why) in cases {
        let notifier = SessionNotifier::from_env(&env).with_socket_for_tests(&dead);
        assert_eq!(
            post(&notifier, &notice()),
            Posted::Skipped(why.into()),
            "{env:?}"
        );
    }
}

/// aterm sets `ATERM_MUX_BASE` in every session (`|` outside any multiplexer), so a tab's
/// env posts; so do a check turned off and a base no multiplexer marks.
#[test]
fn a_tab_shaped_env_posts() {
    let tab = || MapEnv::new().with("ATERM_PARENT_SESSION_ID", SID);
    let envs = [
        tab().with("ATERM_MUX_BASE", "|"),
        tab()
            .with("ATERM_MUX", "off")
            .with("TMUX", "/tmp/tmux-501/default,1,0"),
        tab().with("ATERM_MUX_BASE", "x"),
    ];
    for env in envs {
        let broker = Broker::new();
        let handle = broker.serve(vec![line(CONNECTED), line("OK 18")], false);
        let notifier = SessionNotifier::from_env(&env)
            .with_socket_for_tests(&broker.sock)
            .with_bound_for_tests(HANG);
        assert_eq!(
            post(&notifier, &notice()),
            Posted::Sent("18".into()),
            "{env:?}"
        );
        assert_eq!(heard(handle).len(), 3);
    }
}

#[test]
fn keys_are_stable_and_distinct() {
    let wait = Notice::wait(
        &slug("fixture-dev/app"),
        "v1",
        &["published"],
        Some(2),
        Some(5000),
    );
    assert_eq!(
        wait.key,
        hashed(b"agh.release\x00fixture-dev/app\x00v1\x00published\x002\x005000")
    );
    // The same release's publish hashes the same fields on the other topic: the bridge pins
    // a key per session whatever its topic, so the two keys must differ.
    assert_ne!(
        wait.key,
        Notice::converged(
            &slug("fixture-dev/app"),
            "v1",
            "published",
            Some(2),
            Some(5000)
        )
        .key
    );
    // The same tag cut again (a new release id) is a new post.
    let recut = Notice::wait(
        &slug("fixture-dev/app"),
        "v1",
        &["published"],
        Some(2),
        Some(5001),
    );
    assert_ne!(recut.key, wait.key);
    let upper = Notice::wait(
        &slug("Fixture-Dev/App"),
        "v1",
        &["published"],
        Some(2),
        Some(5000),
    );
    assert_eq!(upper.key, wait.key);

    let release = slug("fixture-dev/app");
    let one = Notice::converged(&release, "v1", "uploaded", Some(1), Some(5000));
    let two = Notice::converged(&release, "v1", "uploaded", Some(2), Some(5000));
    assert_ne!(one.key, two.key);

    // Upload b.bin, then clobber it with new bytes: the count stays 2, but the rename leaves
    // a new asset id, so the two `uploaded` notices are two posts. A rerun that finds the
    // same asset keeps its key.
    let uploaded = Notice::converged_for(&release, "v1", "uploaded", Some(2), Some(5000), 71);
    let clobbered = Notice::converged_for(&release, "v1", "uploaded", Some(2), Some(5000), 72);
    let rerun = Notice::converged_for(&release, "v1", "uploaded", Some(2), Some(5000), 71);
    assert_ne!(uploaded.key, clobbered.key);
    assert_eq!(uploaded.key, rerun.key);
    assert_eq!(
        uploaded.key,
        hashed(b"fixture-dev/app\x00v1\x00uploaded\x002\x005000\x0071")
    );
    assert_eq!(uploaded.body(), two.body());

    let intent = IntentId::parse("0-0123456789ab").expect("an intent id");
    let created = Notice::intent(&release, "v1", "created", &intent, Some(5000));
    assert_eq!(created.key, concat!("agh-", "0-0123456789ab"));

    for key in [
        &wait.key,
        &recut.key,
        &upper.key,
        &one.key,
        &two.key,
        &uploaded.key,
        &clobbered.key,
        &created.key,
    ] {
        assert!(broker_key(key), "{key}");
    }
}

#[test]
fn bodies_and_lines_are_exact() {
    let notice = notice();
    assert_eq!(notice.topic, Topic::Write);
    let body = "v=1 repo=fixture-dev/app tag=v1 event=uploaded n=1 id=5000";
    assert_eq!(notice.body(), body);
    assert_eq!(
        notice.line(SID),
        format!(
            "@s-0123456789abcdef post to=say:agh.write kind=note key={} -- {body}",
            notice.key
        )
    );
    let wait = Notice::wait(&slug("fixture-dev/app"), "v1", &["published"], None, None);
    assert_eq!(wait.topic, Topic::Release);
    assert!(
        wait.line(SID)
            .starts_with("@s-0123456789abcdef post to=say:agh.release kind=note key=agh-"),
        "{}",
        wait.line(SID)
    );
}

#[test]
fn tags_and_conditions_are_percent_encoded() {
    let repo = slug("fixture-dev/app");
    let odd = Notice::wait(&repo, "a b=c%", &["published"], None, None);
    assert!(odd.body().contains(" tag=a%20b%3Dc%25 "), "{}", odd.body());

    let plus = Notice::wait(&repo, "v1", &["published", "asset=a+b.bin"], None, None);
    assert!(
        plus.body().ends_with(" event=published+asset=a%2Bb.bin"),
        "{}",
        plus.body()
    );
    let count = Notice::wait(&repo, "v1", &["published", "assets=2"], None, None);
    assert!(
        count.body().ends_with(" event=published+assets=2"),
        "{}",
        count.body()
    );

    let token = agh_fake::token("t");
    let secret = Notice::wait(&repo, &token, &["published"], None, None);
    for text in [secret.body(), secret.line(SID)] {
        assert!(text.contains(" tag=[redacted] "), "{text}");
        assert!(!text.contains(&token));
    }

    // A blank is encoded `%20`, so the redactor's `bearer` rule must see the tag first.
    let bearer = Notice::wait(
        &repo,
        "bearer abcdefgh",
        &["asset=bearer abcdefgh"],
        None,
        None,
    );
    let body = bearer.body();
    assert!(body.contains(" tag=bearer%20[redacted] "), "{body}");
    assert!(body.ends_with(" event=asset=bearer%20[redacted]"), "{body}");
    assert!(!body.contains("abcdefgh"), "{body}");
    let plain = Notice::wait(&repo, "bearer abcdefgh", &["published"], None, None);
    assert_eq!(
        plain.key,
        hashed(b"agh.release\x00fixture-dev/app\x00bearer abcdefgh\x00published\x00\x00"),
        "the key is the unredacted tag's"
    );

    // Each word is redacted on its own, so no rule reads across the blank between two.
    let intent = IntentId::parse("0-0123456789ab").expect("an intent id");
    let created = Notice::intent(&repo, "bearer", "created", &intent, Some(5000));
    assert_eq!(
        created.body(),
        "v=1 repo=fixture-dev/app tag=bearer event=created id=5000"
    );
    let basic = Notice::wait(&repo, "basic", &["published", "assets=2"], Some(2), None);
    assert!(
        basic.body().contains(" event=published+assets=2 "),
        "{}",
        basic.body()
    );
    let ending = Notice::wait(&repo, "v2-bearer", &["published"], None, None);
    assert!(
        ending.body().ends_with(" tag=v2-bearer event=published"),
        "{}",
        ending.body()
    );
    for owner in ["fixture-dev/bearer", "fixture-dev/qbasic"] {
        let tag = if owner.ends_with("qbasic") {
            "v20261101rc1"
        } else {
            "v1.0.0"
        };
        let uploaded = Notice::converged(&slug(owner), tag, "uploaded", Some(1), Some(5000));
        assert_eq!(
            uploaded.body(),
            format!("v=1 repo={owner} tag={tag} event=uploaded n=1 id=5000")
        );
    }
}

// ---- The posts after a write (WP5's `settled` calls, WP6's `--notify`) ----

const APP: &str = "fixture-dev/app";
/// When the fixtures' releases were made.
const AT: &str = "2026-10-01T00:00:00Z";
/// agh-fake's 422 body: the tag is taken.
const BODY_A: &str = r#"{"message":"Validation Failed","errors":[{"resource":"Release","code":"already_exists","field":"tag_name"}]}"#;

/// A writer over agh-fake, built as `common::writer` builds it plus `.notifier(rec)`.
struct Writes {
    root: aterm_tempfile::TempDir,
    fake: FakeGithub,
    gh: Github,
    rec: Arc<RecordingNotifier>,
}

impl Writes {
    fn new(label: &str) -> Writes {
        let root = common::scratch(label);
        let fake = FakeGithub::start();
        common::canonical_repos(&fake);
        common::canonical_tokens(root.path(), &fake, true, true);
        let (dirs, router) = common::file_router(root.path(), common::WRITERS);
        let rec = Arc::new(RecordingNotifier::new());
        let gh = GithubBuilder::new(router)
            .transport(Arc::new(HttpTransport::direct(Arc::new(common::env()))))
            .endpoints(common::endpoints(&fake))
            .env(Arc::new(common::env()))
            .fx(Arc::new(Fx::simulated(SIM_START)))
            .caller("test")
            .retry(RetryPolicy::NONE)
            .journal(dirs.clone())
            .notifier(rec.clone())
            .build();
        Writes {
            root,
            fake,
            gh,
            rec,
        }
    }

    /// The releases under `How::write()`, posting when `notify`.
    fn releases(&self, notify: bool) -> Releases<'_> {
        self.gh
            .releases(&slug(APP))
            .how(How::write().notify(notify))
    }

    fn permit(&self) -> Permit {
        common::permit(&self.gh, &RouteRequest::write(RouteTarget::Repo(slug(APP))))
    }

    fn create(&self, tag: &str, notify: bool) -> Outcome<Ensured<ReleaseObject>> {
        let journal = self.gh.journal(&slug(APP)).expect("the journal");
        self.releases(notify)
            .create(&self.permit(), &DraftRequest::new(tag), &journal)
            .expect("an outcome")
    }

    /// The id of the journal's first intent for `tag`.
    fn intent_of(&self, tag: &str) -> IntentId {
        let journal = self.gh.journal(&slug(APP)).expect("the journal");
        let (records, _) = journal.read().expect("the journal reads");
        records
            .into_iter()
            .find_map(|(_, record)| match record {
                JournalRecord::Intent(intent) if intent.key == tag => Some(intent.id),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no intent for {tag}"))
    }

    fn upload(&self, release: &ReleaseObject, name: &str, bytes: &[u8]) {
        let path = common::file(self.root.path(), name, bytes);
        let file = UploadFile::open(&path, Some(name)).expect("the upload file");
        self.releases(true)
            .upload(&self.permit(), release, &file, Replace::Never)
            .expect("the upload");
    }

    fn post_fault(&self, fault: Fault) {
        self.fake.fault(
            Match::api("/repos/fixture-dev/app/releases").method("POST"),
            fault,
        );
    }
}

fn done(outcome: Outcome<Ensured<ReleaseObject>>, effect: Effect) -> ReleaseObject {
    let Outcome::Done(ensured) = outcome else {
        panic!("done, {effect:?}: {outcome:?}");
    };
    assert_eq!(ensured.effect, effect);
    ensured.object
}

#[test]
fn writes_post_once_after_done() {
    let w = Writes::new("notify-writes");
    let made = done(w.create("v1", true), Effect::Created);
    let intent = w.intent_of("v1");
    let posted = w.rec.posted();
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert_eq!(posted[0].topic, Topic::Write);
    assert_eq!(posted[0].event, "created");
    assert_eq!(posted[0].key, format!("agh-{intent}"));
    assert_eq!(posted[0].id, Some(made.id));

    let again = done(w.create("v1", true), Effect::Converged);
    assert_eq!(again.id, made.id);
    let posted = w.rec.posted();
    assert_eq!(posted.len(), 2, "{posted:?}");
    assert_eq!(posted[1].key, posted[0].key);

    done(w.create("v2", false), Effect::Created);
    assert_eq!(w.rec.posted().len(), 2, "How::write() alone posts nothing");

    w.post_fault(Fault::Status {
        status: 422,
        body: BODY_A.to_string(),
        headers: Vec::new(),
    });
    let refused = w.create("v3", true);
    assert!(matches!(refused, Outcome::Refused(_)), "{refused:?}");
    assert_eq!(w.rec.posted().len(), 2, "a refused create posts nothing");

    w.post_fault(Fault::Close);
    let lost = w.create("v4", true);
    assert!(matches!(lost, Outcome::Unknown { .. }), "{lost:?}");
    assert_eq!(w.rec.posted().len(), 2, "an unknown create posts nothing");

    w.upload(&made, "a.bin", b"a");
    w.upload(&made, "b.bin", b"b");
    let posted = w.rec.posted();
    let uploads: Vec<&Notice> = posted[2..].iter().collect();
    assert_eq!(uploads.len(), 2, "{posted:?}");
    for (notice, n) in uploads.iter().zip([1, 2]) {
        assert_eq!(notice.topic, Topic::Write);
        assert_eq!(notice.event, "uploaded");
        assert_eq!(notice.n, Some(n));
        assert_eq!(notice.id, Some(made.id));
    }
    assert_ne!(uploads[0].key, uploads[1].key);

    let patch = PublishPatch {
        prerelease: false,
        make_latest: MakeLatest::False,
    };
    w.releases(true)
        .publish(&w.permit(), made.id, &PublishGuard::default(), &patch)
        .expect("the publish");
    let posted = w.rec.posted();
    assert_eq!(posted.len(), 5, "{posted:?}");
    assert_eq!(posted[4].event, "published");
    assert_eq!(posted[4].n, Some(2));
    assert_eq!(posted[4].id, Some(made.id));

    let deleted = w
        .releases(true)
        .delete(&w.permit(), made.id)
        .expect("the delete");
    assert_eq!(deleted, Deleted::Removed);
    let posted = w.rec.posted();
    assert_eq!(posted.len(), 6, "{posted:?}");
    assert_eq!(posted[5].event, "deleted");
    assert_eq!(posted[5].n, None);
    assert_eq!(posted[5].id, Some(made.id));
    assert_eq!(posted[5].tag, "v1");
}

// ---- The CLI write verbs with `--notify` ----

/// A scratch root with the canonical agh.toml, both token files and the canonical repos.
struct Cli {
    root: aterm_tempfile::TempDir,
    fake: FakeGithub,
}

/// One command line's exit and streams.
struct Ran {
    exit: u8,
    stdout: String,
    stderr: String,
}

impl Ran {
    /// stdout's one JSON value.
    fn json(&self) -> Value {
        aterm_json::from_str(self.stdout.trim_end())
            .unwrap_or_else(|e| panic!("JSON on stdout ({e}): {} / {}", self.stdout, self.stderr))
    }

    /// Exit `exit`, or a panic naming both streams.
    fn exits(&self, exit: u8) -> &Ran {
        assert_eq!(
            self.exit, exit,
            "stdout {} / stderr {}",
            self.stdout, self.stderr
        );
        self
    }

    /// The `notify=` note of the receipt line, when it has one.
    fn notify_note(&self) -> Option<String> {
        self.stderr
            .lines()
            .filter(|line| line.starts_with("agh: ok"))
            .flat_map(|line| line.split(' '))
            .find_map(|word| word.strip_prefix("notify="))
            .map(str::to_string)
    }
}

impl Cli {
    fn new(label: &str) -> Cli {
        let root = common::scratch(label);
        let fake = FakeGithub::start();
        common::canonical_tokens(root.path(), &fake, true, true);
        common::write_config(
            &Dirs::under(root.path()),
            &common::canonical_config(root.path()),
        );
        common::canonical_repos(&fake);
        Cli { root, fake }
    }

    /// `args` under `notifier`.
    fn run_with(&self, notifier: Arc<dyn Notifier>, args: &[&str]) -> Ran {
        let (ctx, captured) = common::ctx(self.root.path(), &self.fake, common::env());
        let mut ctx = ctx.with_notifier(notifier);
        let exit = common::run_cli(&mut ctx, args);
        Ran {
            exit,
            stdout: captured.stdout_text(),
            stderr: captured.stderr_text(),
        }
    }

    /// `args` under a fresh recorder, and the recorder.
    fn run(&self, args: &[&str]) -> (Ran, Arc<RecordingNotifier>) {
        let rec = Arc::new(RecordingNotifier::new());
        let ran = self.run_with(rec.clone(), args);
        (ran, rec)
    }

    /// `bytes` as `{name}` under the root; its path as text.
    fn file(&self, name: &str, bytes: &[u8]) -> String {
        common::file(self.root.path(), name, bytes)
            .display()
            .to_string()
    }
}

#[test]
fn create_posts_created_with_its_intent_key() {
    let cli = Cli::new("notify-cli-create");
    let (ran, rec) = cli.run(&[
        "release", "create", APP, "v1", "--draft", "--notify", "--meta",
    ]);
    let report = ran.exits(0).json();
    let posted = rec.posted();
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert_eq!(posted[0].event, "created");
    let intent = report["intent"].as_str().expect("an intent");
    assert_eq!(posted[0].key, format!("agh-{intent}"));
    assert_eq!(report["id"], Value::from(posted[0].id.expect("an id")));
    assert_eq!(
        ran.notify_note().as_deref(),
        Some("sent:recorded"),
        "{}",
        ran.stderr
    );
}

#[test]
fn two_uploads_post_two_counts() {
    let cli = Cli::new("notify-cli-upload");
    cli.run(&["release", "create", APP, "v1", "--draft"])
        .0
        .exits(0);
    let a = cli.file("a.bin", b"a");
    let b = cli.file("b.bin", b"b");
    let (ran, rec) = cli.run(&["release", "upload", APP, "v1", &a, &b, "--notify"]);
    ran.exits(0);
    let posted = rec.posted();
    assert_eq!(posted.len(), 2, "{posted:?}");
    for (notice, n) in posted.iter().zip([1, 2]) {
        assert_eq!(notice.event, "uploaded");
        assert_eq!(notice.n, Some(n));
    }
}

#[test]
fn delete_posts_deleted() {
    let cli = Cli::new("notify-cli-delete");
    cli.run(&["release", "create", APP, "v1", "--draft"])
        .0
        .exits(0);
    let (ran, rec) = cli.run(&[
        "release",
        "delete",
        APP,
        "v1",
        "--confirm",
        "v1",
        "--notify",
    ]);
    ran.exits(0);
    let posted = rec.posted();
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert_eq!(posted[0].event, "deleted");
    assert_eq!(posted[0].n, None);
}

#[test]
fn a_refused_write_and_a_dry_run_post_nothing() {
    let cli = Cli::new("notify-cli-refused");
    cli.fake.add_release(APP, ReleaseSpec::published("v1", AT));
    let (ran, rec) = cli.run(&[
        "release", "create", APP, "v1", "--draft", "--notify", "--meta",
    ]);
    ran.exits(9);
    assert_eq!(
        common::error_json(&ran.stderr)["sub"],
        Value::from("already_exists")
    );
    assert!(rec.posted().is_empty(), "{:?}", rec.posted());
    // A refused write prints no `agh: ok` receipt, so the whole of stderr is read.
    assert!(!ran.stderr.contains("notify="), "{}", ran.stderr);

    // The dry run names a fresh tag, so it is one that would be created.
    let (ran, rec) = cli.run(&[
        "release",
        "create",
        APP,
        "v2",
        "--draft",
        "--dry-run",
        "--notify",
        "--meta",
    ]);
    ran.exits(0);
    assert!(rec.posted().is_empty(), "{:?}", rec.posted());
    // The dry run prints its receipt, and the receipt has no `notify=` note.
    assert!(
        ran.stderr.lines().any(|line| line.starts_with("agh: ok")),
        "{}",
        ran.stderr
    );
    assert_eq!(ran.notify_note(), None, "{}", ran.stderr);
}

#[test]
fn a_failed_post_keeps_exit_zero() {
    let cli = Cli::new("notify-cli-failed");
    let dir = aterm_tempfile::Builder::new()
        .prefix("agh-n")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("a scratch directory under /tmp");
    let sock = dir.path().join("aterm-4242.sock");
    let notifier = Arc::new(SessionNotifier::at_socket(&sock, SID));
    let ran = cli.run_with(
        notifier,
        &[
            "release", "create", APP, "v1", "--draft", "--notify", "--meta",
        ],
    );
    let report = ran.exits(0).json();
    assert_eq!(report["outcome"], Value::from("created"), "{report}");
    assert_eq!(
        ran.notify_note().as_deref(),
        Some("failed:no-socket"),
        "{}",
        ran.stderr
    );
}
