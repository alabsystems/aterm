// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PROVEN SELF-CLAIM ([`crate::self_claim`]) at the claim store and over a
//! real connection: the claim rule, the replacement it buys, the clear when
//! its process leaves, and — against a real process on a real pty, speaking
//! over a real socket served by the real lanes — the kernel's peer pid that
//! proves it, with a descendant and an inherited connection as the negative
//! controls.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use aterm_uds::CtlStream;

use super::control_lanes::{LaneLimits, LaneReader, LaneService, Lanes};
use super::{Scope, ServeDisposition, ServingFd, serving_peer_pid};
use crate::self_claim::Proven;
use crate::session_timeline::{
    ClaimRule, claim_rule, claim_supervisor_as, release_proven_if_gone, release_supervisor,
};

/// The proof of a process that runs: this test process, by its own pid and
/// kernel start.
fn running() -> Proven {
    let pid = std::process::id();
    Proven {
        pid,
        start: aterm_agent::harness::restart_stamp::kernel_start(pid).expect("our start"),
    }
}

/// A proof whose process is gone: this pid under a start it never had.
fn gone() -> Proven {
    Proven {
        start: "Thu Jan  1 00:00:00 1970".to_string(),
        ..running()
    }
}

/// The supervisor fields of a session's meta now: `(holder, proven)`.
fn shown(ctx: &crate::SessionCtx) -> (Option<String>, bool) {
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    let now = crate::metrics::now_us();
    (
        meta.live_supervisor(now).map(str::to_owned),
        meta.supervisor_proven(now),
    )
}

/// The `meta-change` payloads the session's timeline recorded, oldest first.
fn meta_changes(ctx: &crate::SessionCtx) -> Vec<String> {
    let reply = super::control_session::cmd_timeline(ctx, "");
    reply
        .lines()
        .filter_map(|l| {
            l.split_once(" kind=meta-change ")
                .map(|(_, p)| p.to_string())
        })
        .collect()
}

#[test]
fn the_claim_rule_lets_only_a_proof_replace_and_only_a_proof_renew_a_proof() {
    let host = "aterm-harness@77";
    let me = "codex-alab@500";
    // Nothing standing: anyone takes it.
    assert_eq!(claim_rule(None, false, me, false), ClaimRule::Take);
    assert_eq!(claim_rule(None, false, me, true), ClaimRule::Take);
    // A proof replaces the window host's claim…
    assert_eq!(
        claim_rule(Some((host, false)), false, me, true),
        ClaimRule::Replace
    );
    // …and a proven claim whose process is gone.
    assert_eq!(
        claim_rule(Some(("codex-alab@499", true)), true, me, true),
        ClaimRule::Replace
    );
    // The same proven holder renews with a proof.
    assert_eq!(
        claim_rule(Some((me, true)), false, me, true),
        ClaimRule::Renew
    );
    // NEGATIVE CONTROLS: an unproven request replaces nothing — not the
    // host's, not a dead proof's — and never renews a proof (a forgery of
    // the holder's spelling from a model's tool process);
    assert_eq!(
        claim_rule(Some((host, false)), false, me, false),
        ClaimRule::Busy
    );
    assert_eq!(
        claim_rule(Some(("codex-alab@499", true)), true, me, false),
        ClaimRule::Busy
    );
    assert_eq!(
        claim_rule(Some((me, true)), false, me, false),
        ClaimRule::Busy
    );
    // a proof replaces no other supervisor's claim, nor a proven claim whose
    // process runs.
    assert_eq!(
        claim_rule(Some(("watcher@elsewhere", false)), false, me, true),
        ClaimRule::Busy
    );
    assert_eq!(
        claim_rule(Some(("codex-alab@499", true)), false, me, true),
        ClaimRule::Busy
    );
    // An unproven claim's holder renews as before.
    assert_eq!(
        claim_rule(Some(("watcher", false)), false, "watcher", false),
        ClaimRule::Renew
    );
}

#[test]
fn a_proven_claim_replaces_the_hosts_and_a_dead_proofs_and_leaves_with_its_process() {
    let h = crate::session_store::test_handle_on(7501, -1);
    let ctx = h.ctx.as_ref();
    let now = crate::metrics::now_us();
    // The window host's loop holds the session.
    let host = format!("aterm-harness@{}", std::process::id());
    assert!(claim_supervisor_as(ctx, &host, None, None, None, now).is_ok());
    // An unproven self-claim is an ordinary foreign claim: busy.
    let me = format!("codex-alab@{}", std::process::id());
    assert_eq!(
        claim_supervisor_as(ctx, &me, Some(1), None, None, now),
        Err(host.clone())
    );
    assert_eq!(shown(ctx), (Some(host.clone()), false));
    // Proven: it replaces the host's, and says whose it replaced.
    let claimed = claim_supervisor_as(ctx, &me, Some(1), None, Some(running()), now)
        .expect("a proof replaces the host's claim");
    assert!(claimed.changed);
    assert_eq!(claimed.replaced.as_deref(), Some(host.as_str()));
    assert_eq!(shown(ctx), (Some(me.clone()), true));
    // A forgery of its spelling from another connection is refused and
    // moves nothing: the claim stays bound and proven.
    assert_eq!(
        claim_supervisor_as(ctx, &me, Some(2), None, None, now),
        Err(me.clone())
    );
    assert_eq!(shown(ctx), (Some(me.clone()), true));
    // The host's own claim cannot take it back.
    assert_eq!(
        claim_supervisor_as(ctx, &host, None, None, None, now),
        Err(me.clone())
    );
    // While its process leads the group in front, the claim stands…
    let pid = i32::try_from(std::process::id()).expect("a pid");
    assert!(!release_proven_if_gone(ctx, pid, now));
    assert_eq!(shown(ctx), (Some(me.clone()), true));
    // …and once another group leads, it is cleared and recorded.
    assert!(release_proven_if_gone(ctx, pid + 1, now));
    assert_eq!(shown(ctx), (None, false));
    assert_eq!(
        meta_changes(ctx).last().map(String::as_str),
        Some("field=supervisor value=-")
    );
    // A proven claim whose process is gone is replaced by a new proof, and
    // cleared even while its pid is the one in front.
    assert!(claim_supervisor_as(ctx, &me, Some(3), None, Some(gone()), now).is_ok());
    assert!(release_proven_if_gone(ctx, pid, now), "its process is gone");
    assert!(claim_supervisor_as(ctx, &me, Some(4), None, Some(gone()), now).is_ok());
    let other = "codex-alab@4242";
    let claimed = claim_supervisor_as(ctx, other, Some(5), None, Some(running()), now)
        .expect("a proof replaces a dead proof");
    assert_eq!(claimed.replaced.as_deref(), Some(me.as_str()));
    // An unproven claim is never cleared by the foreground's move.
    assert!(release_supervisor(ctx, None, now));
    assert!(claim_supervisor_as(ctx, "watcher", None, None, None, now).is_ok());
    assert!(!release_proven_if_gone(ctx, pid + 1, now));
    assert_eq!(shown(ctx), (Some("watcher".to_string()), false));
}

/// THE FENCE (§2): a write fenced `if-supervisor=<holder>` passes only while
/// that holder holds the live claim — the window host's, until a proven
/// self-claim replaces it; then the host's is refused naming the new holder,
/// and the self-claim's passes. NEGATIVE CONTROL: with no claim at all, a
/// fenced write is refused (`-`).
#[test]
fn a_fenced_write_lands_only_under_its_holders_claim() {
    use crate::session_timeline::supervisor_fence;
    let h = crate::session_store::test_handle_on(7504, -1);
    let ctx = h.ctx.as_ref();
    let now = crate::metrics::now_us();
    let host = "aterm-harness@1";
    assert_eq!(
        supervisor_fence(ctx, host, now).as_deref(),
        Some("ERR busy supervisor=-\n")
    );
    assert!(claim_supervisor_as(ctx, host, None, None, None, now).is_ok());
    assert_eq!(supervisor_fence(ctx, host, now), None);
    let me = format!("codex-alab@{}", std::process::id());
    assert!(claim_supervisor_as(ctx, &me, Some(1), None, Some(running()), now).is_ok());
    assert_eq!(
        supervisor_fence(ctx, host, now),
        Some(format!("ERR busy supervisor={me}\n")),
        "the replaced loop's write is refused"
    );
    assert_eq!(supervisor_fence(ctx, &me, now), None);
}

/// THE FENCE HOLDS ACROSS THE WRITE (§2): a write the host's fence admitted
/// and that is still in flight keeps a proven replacement from being
/// answered until it lands, and once the replacement is answered every
/// write under the host's fence is refused. NEGATIVE CONTROL: with no write
/// in flight the replacement is answered at once.
#[test]
fn a_replacement_waits_for_the_writes_the_replaced_fence_admitted() {
    use crate::session_timeline::FencedWrite;
    let h = crate::session_store::test_handle_on(7506, -1);
    let ctx = Arc::clone(&h.ctx);
    let now = crate::metrics::now_us();
    let host = "aterm-harness@1";
    let me = format!("codex-alab@{}", std::process::id());
    assert!(claim_supervisor_as(&ctx, host, None, None, None, now).is_ok());
    let writing = FencedWrite::enter(&ctx, host, now).expect("the host holds the claim");
    let (answered_tx, answered) = std::sync::mpsc::channel();
    let claimant = {
        let ctx = Arc::clone(&ctx);
        let me = me.clone();
        std::thread::spawn(move || {
            let claimed = claim_supervisor_as(&ctx, &me, Some(1), None, Some(running()), now);
            answered_tx.send(claimed.map(|c| c.replaced)).expect("send");
        })
    };
    // The claim moved (a new write reads the new holder)…
    let moved = Instant::now();
    while shown(&ctx).0.as_deref() != Some(me.as_str()) {
        assert!(
            moved.elapsed() < std::time::Duration::from_secs(60),
            "hang: the claim never moved"
        );
        std::thread::yield_now();
    }
    assert!(FencedWrite::enter(&ctx, host, now).is_err());
    // …but the replacement is not answered while the admitted write stands.
    assert!(
        answered.try_recv().is_err(),
        "answered under a write in flight"
    );
    drop(writing);
    assert_eq!(
        answered
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("hang: never answered"),
        Ok(Some(host.to_string()))
    );
    claimant.join().expect("claimant");
    assert_eq!(
        FencedWrite::enter(&ctx, host, now).err().as_deref(),
        Some(format!("ERR busy supervisor={me}\n").as_str())
    );
    // NEGATIVE CONTROL: nothing in flight, answered at once.
    assert!(release_supervisor(&ctx, None, now));
    assert!(claim_supervisor_as(&ctx, host, None, None, None, now).is_ok());
    assert!(claim_supervisor_as(&ctx, &me, Some(2), None, Some(running()), now).is_ok());
}

/// `meta set harness_act <act>` (§5): only the live PROVEN self-claim's
/// holder records an act, on its claim's connection or on one the kernel
/// proves again; it is one `harness-act` row and `status last_act=`.
/// NEGATIVE CONTROLS: no claim, an unproven claim's own connection, and a
/// proven claim's other connection with no proof are each `ERR denied` and
/// record nothing.
#[test]
fn only_a_proven_claims_holder_records_an_act() {
    use super::control_session::cmd_meta_harness_act;
    let h = crate::session_store::test_handle_on(7505, -1);
    let ctx = h.ctx.as_ref();
    let now = crate::metrics::now_us();
    let no_proof = |_: &str| -> Option<Proven> { None };
    let proof = |_: &str| -> Option<Proven> { Some(running()) };
    let acts = |ctx: &crate::SessionCtx| {
        let timeline = ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner);
        let rows: Vec<String> = timeline
            .since(None)
            .filter(|e| e.kind == crate::session_timeline::HARNESS_ACT)
            .map(|e| e.payload.clone())
            .collect();
        (rows, timeline.last_act().map(str::to_owned))
    };
    let denied = Some(("ERR denied\n".to_string(), false));
    let ok = Some(("OK\n".to_string(), true));
    // Not this form: falls through to `meta`.
    assert_eq!(
        cmd_meta_harness_act(ctx, Some(1), &proof, "set title x"),
        None
    );
    assert_eq!(cmd_meta_harness_act(ctx, Some(1), &proof, ""), None);
    // No claim.
    assert_eq!(
        cmd_meta_harness_act(ctx, Some(1), &proof, "set harness_act continue"),
        denied
    );
    // An unproven claim, on its own connection.
    let me = format!("codex-alab@{}", std::process::id());
    assert!(claim_supervisor_as(ctx, &me, Some(1), None, None, now).is_ok());
    assert_eq!(
        cmd_meta_harness_act(ctx, Some(1), &proof, "set harness_act continue"),
        denied
    );
    assert_eq!(acts(ctx), (Vec::new(), None));
    // Proven, bound to connection 2.
    assert!(release_supervisor(ctx, None, now));
    assert!(claim_supervisor_as(ctx, &me, Some(2), None, Some(running()), now).is_ok());
    assert_eq!(
        cmd_meta_harness_act(ctx, Some(2), &no_proof, "set harness_act continue"),
        ok
    );
    // Another connection: only with the kernel's proof again.
    assert_eq!(
        cmd_meta_harness_act(ctx, Some(3), &no_proof, "set harness_act approve"),
        denied
    );
    assert_eq!(
        cmd_meta_harness_act(ctx, Some(3), &proof, "set harness_act approve"),
        ok
    );
    assert_eq!(
        acts(ctx),
        (
            vec![
                format!("act=continue holder={me}"),
                format!("act=approve holder={me}")
            ],
            Some("approve".to_string())
        )
    );
    let before = acts(ctx);
    // The pid alone is not identity: a reproof under another kernel start
    // cannot speak for the standing claim.
    let reused = |_: &str| Some(gone());
    assert_eq!(
        cmd_meta_harness_act(ctx, Some(3), &reused, "set harness_act continue"),
        denied
    );
    // A release and new claim can land during the off-lock kernel look.
    // Even the same holder/pid/start cannot authorize the older request.
    let moved = |_: &str| {
        assert!(release_supervisor(ctx, None, now));
        assert!(claim_supervisor_as(ctx, &me, Some(2), None, Some(running()), now).is_ok());
        Some(running())
    };
    assert_eq!(
        cmd_meta_harness_act(ctx, Some(3), &moved, "set harness_act continue"),
        denied
    );
    assert_eq!(acts(ctx), before, "a stale act records nothing");
    // Usage: no act, two, a token no owner key could be, and `unset`.
    let usage = Some(("ERR usage: meta set harness_act <act>\n".to_string(), false));
    for rest in [
        "set harness_act",
        "set harness_act a b",
        "set harness_act a=b?",
        "unset harness_act",
    ] {
        assert_eq!(
            cmd_meta_harness_act(ctx, Some(2), &proof, rest),
            usage,
            "{rest}"
        );
    }
}

#[test]
fn a_proven_claim_is_never_carried_across_an_update() {
    let h = crate::session_store::test_handle_on(7502, -1);
    let ctx = h.ctx.as_ref();
    let now = crate::metrics::now_us();
    let me = format!("codex-alab@{}", std::process::id());
    let ttl = Some(now + 60_000_000);
    assert!(claim_supervisor_as(ctx, &me, None, ttl, Some(running()), now).is_ok());
    let meta = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(
        meta.handoff_supervisor(now, "aterm-harness@1"),
        (None, false)
    );
    // Negative control: the same lease unproven is carried.
    assert!(release_supervisor(ctx, None, now));
    assert!(claim_supervisor_as(ctx, &me, None, ttl, None, now).is_ok());
    let meta = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert!(meta.handoff_supervisor(now, "aterm-harness@1").0.is_some());
}

/// The peer pid is read only on a direct connection: the lanes mark theirs,
/// the inherited bridge's `socketpair` is never one.
#[test]
fn the_peer_pid_is_read_only_on_a_direct_connection() {
    let (_client, server) = CtlStream::pair().expect("socket pair");
    let serving = ServingFd::enter(&server);
    assert_eq!(
        serving_peer_pid(),
        None,
        "an undirected connection proves nothing"
    );
    let direct = super::DirectPeer::enter();
    assert_eq!(serving_peer_pid(), Some(std::process::id()));
    drop(direct);
    drop(serving);
    assert_eq!(serving_peer_pid(), None);
}

/// A lane service that serves `meta …` against one session exactly as the
/// dispatch's `meta` arm does: [`super::control_session::cmd_meta_supervisor`]
/// with the proof read from the connection and the session's PTY master
/// ([`super::prove_self_claim_by`]), and every other `meta` as a bare read.
/// `alab_image` is the image fact: the real one
/// ([`crate::self_claim::runs_alab_build`]), or a stand-in that takes the
/// claimant's `perl` for an ALab build.
struct MetaService {
    ctx: Arc<crate::SessionCtx>,
    master: i32,
    alab_image: fn(u32) -> bool,
    replies: Mutex<Vec<String>>,
}

impl LaneService for MetaService {
    fn authenticate(
        &self,
        _stream: &CtlStream,
        reader: &mut LaneReader<'_>,
    ) -> Option<(Scope, Option<String>)> {
        (super::read_request_line(reader)? == "AUTH ok").then_some((Scope::Owner, None))
    }

    fn serve_line(
        &self,
        line: String,
        scope: Scope,
        _stream: &CtlStream,
        _reader: &mut LaneReader<'_>,
        out: &mut Vec<u8>,
    ) -> Option<ServeDisposition> {
        let rest = line.strip_prefix("meta ").unwrap_or("");
        let master = self.master;
        let alab_image = self.alab_image;
        let prove = |holder: &str| super::prove_self_claim_by(holder, master, &alab_image);
        let reply = super::control_session::cmd_meta_supervisor(
            &self.ctx,
            scope,
            super::serving_connection(),
            &prove,
            rest,
        )
        .map_or_else(|| "ERR usage\n".to_string(), |(reply, _)| reply);
        self.replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(reply.clone());
        out.extend_from_slice(reply.as_bytes());
        None
    }

    fn is_wait(&self, _line: &str, _scope: Scope) -> bool {
        false
    }

    fn subscribe(&self, _line: String, _scope: Scope, _stream: CtlStream) {}

    fn release(&self, id: u64) {
        let _ = release_supervisor(&self.ctx, Some(id), crate::metrics::now_us());
    }
}

/// The claimant on a real pty: `perl`, its own session leader (so it leads
/// the pty's foreground group), connecting to `sock` and claiming
/// `codex-alab@<pid>` — its own pid (`self`, and `ttl` with a lease), or,
/// from a FORKED child (a descendant, as a model's tool process is), its
/// parent's (`child`). It prints the reply and waits for its stdin to close
/// with the connection open, so a connection-bound claim stands meanwhile.
#[cfg(target_os = "macos")]
const CLAIMANT: &str = r#"
use IO::Socket::UNIX;
my ($sock, $mode) = @ARGV;
my $pid = $$;
my $ttl = $mode eq "ttl" ? " ttl=60000" : "";
my $s;
my $claim = sub {
    $s = IO::Socket::UNIX->new(Peer => $sock) or die "connect: $!";
    print $s "AUTH ok\nmeta set supervisor codex-alab\@$pid$ttl\n";
    my $reply = <$s>;
    print "REPLY $reply";
};
if ($mode eq "child") {
    my $kid = fork();
    if ($kid == 0) { $claim->(); print "DONE\n"; my $hold = <STDIN>; exit 0; }
    waitpid($kid, 0);
} else {
    $claim->();
    print "DONE\n";
    my $hold = <STDIN>;
}
"#;

/// Own the claimant's process group and master, including on an assertion's
/// unwind. Only this fixture's newly created group is ever killed.
#[cfg(target_os = "macos")]
struct Claimant {
    child: std::process::Child,
    master: Option<std::os::fd::OwnedFd>,
    cleaned: bool,
}

#[cfg(target_os = "macos")]
impl Claimant {
    fn kill_group(&self) {
        // SAFETY: pre_exec made this fixture's freshly spawned child its own
        // session and group leader before spawn returned. Signal that group
        // alone, including the forked negative-control child.
        if let Ok(pid) = i32::try_from(self.child.id()) {
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }

    fn kill_and_wait(&mut self) {
        self.kill_group();
        self.master.take();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the killed claimant exits",
            || self.child.try_wait().expect("wait for the claimant"),
        );
        self.cleaned = true;
    }
}

#[cfg(target_os = "macos")]
impl Drop for Claimant {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        self.kill_group();
        self.master.take();
        let _ = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(10),
            || match self.child.try_wait() {
                Ok(None) => None,
                Ok(Some(_)) | Err(_) => Some(()),
            },
        );
    }
}

/// A claimant spawned on a fresh pty ([`CLAIMANT`]).
#[cfg(target_os = "macos")]
fn claimant(sock: &std::path::Path, mode: &str) -> Claimant {
    use aterm_uds::spawnfd::InheritNothing as _;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: both descriptors are fresh and owned by this fixture.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    use std::os::fd::AsRawFd as _;
    aterm_pty::set_nonblocking(master.as_raw_fd(), true).expect("nonblocking master");
    let stdio = || std::process::Stdio::from(slave.try_clone().expect("dup slave"));
    // std's Command on purpose: the pty child needs `pre_exec` (setsid +
    // TIOCSCTTY), which spawnfd lacks; the strip first, so it holds its
    // terminal and nothing else of this process.
    let mut program = std::process::Command::new("/usr/bin/perl");
    program
        .inherit_nothing()
        .env_clear()
        .env("HOME", sock.parent().expect("scratch socket directory"))
        .args(["-e", CLAIMANT])
        .arg(sock)
        .arg(mode)
        .stdin(stdio())
        .stdout(stdio())
        .stderr(stdio());
    // SAFETY: runs in the forked child before exec, after stdio is in place:
    // `setsid` and `ioctl(TIOCSCTTY)` are async-signal-safe and touch nothing
    // of the parent's.
    unsafe {
        program.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY.into(), 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = program.spawn().expect("spawn the claimant");
    drop(slave);
    Claimant {
        child,
        master: Some(master),
        cleaned: false,
    }
}

/// Read the pty until `DONE` (the claimant printed its reply), with a hang
/// detector; the text read.
#[cfg(target_os = "macos")]
fn read_until_done(master: i32) -> String {
    let mut text = Vec::new();
    let mut buf = [0u8; 512];
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the claimant reports DONE",
        || {
            // SAFETY: reads into a local buffer from this fixture's open,
            // nonblocking master, so the outer hang detector can always run.
            let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                text.extend_from_slice(&buf[..usize::try_from(n).unwrap_or(0)]);
            } else if n < 0 {
                let error = std::io::Error::last_os_error();
                assert!(
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ),
                    "read claimant: {error}"
                );
            } else {
                panic!("the claimant closed before DONE: {text:?}");
            }
            String::from_utf8_lossy(&text)
                .contains("DONE")
                .then_some(())
        },
    );
    String::from_utf8_lossy(&text).into_owned()
}

/// THE PROOF OVER A REAL CONNECTION: a real process that leads a real pty's
/// foreground group connects to a real socket served by the real lanes and
/// claims under its own pid — proven (`supervisor_proven=1`), by the kernel's
/// peer pid of that connection, the live `tcgetpgrp` of the master and its
/// image (here a stand-in that takes `perl` for an ALab build).
/// NEGATIVE CONTROLS: the same claim made from its forked child — a
/// descendant, as a model's tool process is of the TUI — names the leader's
/// pid but is not its connection's peer; with the REAL image fact, `perl`
/// is no ALab build; and a `ttl=` lease is never proven. Each is an ordinary
/// claim, unproven.
#[cfg(target_os = "macos")]
#[test]
fn a_claim_is_proven_by_its_connections_peer_leading_the_foreground() {
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let sock = dir.path().join("c.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let stand_in: fn(u32) -> bool = |_| true;
    let real: fn(u32) -> bool = crate::self_claim::runs_alab_build;
    for (mode, alab_image, want_proven) in [
        ("self", stand_in, true),
        ("child", stand_in, false),
        ("self", real, false),
        ("ttl", stand_in, false),
    ] {
        let mut claimant = claimant(&sock, mode);
        let pid = claimant.child.id();
        use std::os::fd::AsRawFd as _;
        let master = claimant.master.as_ref().expect("open master").as_raw_fd();
        let handle = crate::session_store::test_handle_on(7503, master);
        let service = Arc::new(MetaService {
            ctx: Arc::clone(&handle.ctx),
            master,
            alab_image,
            replies: Mutex::new(Vec::new()),
        });
        let lanes = Lanes::new(LaneLimits {
            rpc: 1,
            wait: 0,
            inline: 0,
            open: 4,
        });
        assert_eq!(lanes.start(), 1, "the request lane starts");
        lanes.publish(service.clone());
        let (stream, _) = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the claimant connects",
            || match listener.accept() {
                Ok(connection) => Some(connection),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
                Err(error) => panic!("accept claimant: {error}"),
            },
        );
        stream.set_nonblocking(false).expect("blocking lane stream");
        assert!(lanes.admit(stream).is_ok(), "admitted");
        let said = read_until_done(master);
        let reply = service
            .replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .first()
            .cloned();
        assert_eq!(reply.as_deref(), Some("OK\n"), "{mode}: {said}");
        let holder = format!("codex-alab@{pid}");
        assert_eq!(
            shown(&handle.ctx),
            (Some(holder.clone()), want_proven),
            "{mode}: {said}"
        );
        // `meta` prints it.
        let meta = super::control_session::cmd_meta(
            &handle.term,
            &crate::session_store::new_store(),
            7503,
            &handle.ctx,
            "",
        )
        .0;
        assert!(
            meta.contains(&format!(
                " supervisor={} supervisor_proven={} ",
                crate::control::pct_encode(&holder),
                u8::from(want_proven)
            )),
            "{meta}"
        );
        // End only this fixture's process group, then reap under the hang detector.
        claimant.kill_and_wait();
        if want_proven {
            // Its process gone, the proven claim is cleared: by its
            // connection's end, or at the sweep's next look, whichever is
            // first.
            let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while shown(&handle.ctx) != (None, false) {
                let _ = release_proven_if_gone(&handle.ctx, -1, crate::metrics::now_us());
                assert!(
                    Instant::now() < deadline,
                    "hang: the claim was never cleared"
                );
                std::thread::yield_now();
            }
        }
    }
}
