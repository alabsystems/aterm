// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DRIVE LEASE'S TAKER, AS THE KERNEL NAMES IT (`Lease::Drive::holder_pid`,
//! `status hand_pid=`): a lease records the peer pid of the direct connection
//! its acquire was served on, never its `holder=` text. Against a real
//! process speaking over a real socket served by the real lanes, a lease
//! named `aterm-harness@<aterm's pid>` is attested as that process's, so the
//! presence layer no longer reads the forged name as aterm's own hand.

use std::sync::PoisonError;
#[cfg(target_os = "macos")]
use std::sync::{Arc, Mutex};

use aterm_uds::CtlStream;

use super::{ServingFd, serving_peer_pid};

/// The lease a session holds now, as `(holder, holder_pid)`.
fn drive_lease(ctx: &crate::SessionCtx) -> Option<(String, Option<u32>)> {
    match ctx
        .turn_lease
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        Some(crate::Lease::Drive {
            holder, holder_pid, ..
        }) => Some((holder.clone(), *holder_pid)),
        _ => None,
    }
}

/// The harness's own `Hand::take` connects from inside the aterm process
/// over a direct connection: the kernel names aterm itself. The same acquire
/// over the bridge's inherited connection (not a direct one) is attested by
/// nobody, and `lease status` then prints no `pid=`.
#[test]
fn a_lease_taken_over_a_direct_connection_records_its_kernel_peer() {
    let handle = crate::session_store::test_handle(7601);
    let own = crate::harness_host::holder();
    let (_client, server) = CtlStream::pair().expect("socket pair");
    {
        let _serving = ServingFd::enter(&server);
        let reply =
            super::control_session::cmd_lease(&handle.ctx, &format!("acquire holder={own} hard"));
        assert!(reply.starts_with("OK lease acquired"), "{reply}");
        assert_eq!(
            drive_lease(&handle.ctx),
            Some((own.clone(), None)),
            "an undirected connection attests nobody"
        );
        let status = super::control_session::cmd_lease(&handle.ctx, "status");
        assert!(!status.contains(" pid="), "{status}");
        let _direct = super::DirectPeer::enter();
        assert_eq!(serving_peer_pid(), Some(std::process::id()));
        // A renewal by the same holder re-reads the taker.
        let reply =
            super::control_session::cmd_lease(&handle.ctx, &format!("acquire holder={own} hard"));
        assert!(reply.starts_with("OK lease acquired"), "{reply}");
    }
    assert_eq!(
        drive_lease(&handle.ctx),
        Some((own.clone(), Some(std::process::id()))),
        "the harness's own hand is aterm's pid"
    );
    let status = super::control_session::cmd_lease(&handle.ctx, "status");
    assert!(
        status.contains(&format!(" pid={}\n", std::process::id())),
        "{status}"
    );
    assert!(crate::presence::attested_aterms_holder(
        &own,
        Some(std::process::id()),
        &own,
        std::process::id()
    ));
}

/// The ruling-313 judgement, pure. NEGATIVE CONTROL: the old name-only
/// judgement takes a forged lease (aterm's holder name, another process's
/// pid, or none attested) for aterm's own hand; the attested one refuses it.
#[test]
fn a_forged_holder_name_is_not_atermss_hand() {
    let me = std::process::id();
    let own = format!("aterm-harness@{me}");
    let name_only = |holder: &str| holder == own;
    for (pid, want) in [(Some(me), true), (Some(me + 1), false), (None, false)] {
        assert!(name_only(&own), "the old judgement trusts the name");
        assert_eq!(
            crate::presence::attested_aterms_holder(&own, pid, &own, me),
            want,
            "{pid:?}"
        );
    }
    assert!(!crate::presence::attested_aterms_holder(
        "drv-1",
        Some(me),
        &own,
        me
    ));
}

/// A lane service that serves `lease …` against one session exactly as the
/// dispatch's `lease` arm does ([`super::control_session::cmd_lease`]).
#[cfg(target_os = "macos")]
struct LeaseService {
    ctx: Arc<crate::SessionCtx>,
    replies: Mutex<Vec<String>>,
}

#[cfg(target_os = "macos")]
impl super::control_lanes::LaneService for LeaseService {
    fn authenticate(
        &self,
        _stream: &CtlStream,
        reader: &mut super::control_lanes::LaneReader<'_>,
    ) -> Option<(super::Scope, super::Principal, Option<String>)> {
        (super::read_request_line(reader)? == "AUTH ok").then_some((
            super::Scope::Owner,
            super::Principal::Instance,
            None,
        ))
    }

    fn serve_line(
        &self,
        line: String,
        _scope: super::Scope,
        _stream: &CtlStream,
        _reader: &mut super::control_lanes::LaneReader<'_>,
        out: &mut Vec<u8>,
    ) -> Option<super::ServeDisposition> {
        let rest = line.strip_prefix("lease ").unwrap_or("");
        let reply = super::control_session::cmd_lease(&self.ctx, rest);
        self.replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(reply.clone());
        out.extend_from_slice(reply.as_bytes());
        None
    }

    fn is_wait(&self, _line: &str, _scope: super::Scope) -> bool {
        false
    }

    fn subscribe(&self, _line: String, _scope: super::Scope, _stream: CtlStream) {}

    fn release(&self, _id: u64) {}

    fn has_live_claim(&self, id: u64) -> bool {
        let now = crate::metrics::now_us();
        self.ctx
            .turn_lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|lease| {
                matches!(lease, crate::Lease::Drive { conn: Some(owner), here: true, .. }
                    if *owner == id)
                    && lease.is_live(now)
            })
    }
}

/// The forger: `perl` connecting to `sock` and taking a hard lease under
/// aterm's own holder name, then reading its status, then closing.
#[cfg(target_os = "macos")]
const FORGER: &str = r#"
use IO::Socket::UNIX;
my ($sock, $holder) = @ARGV;
my $s = IO::Socket::UNIX->new(Peer => $sock) or die "connect: $!";
print $s "AUTH ok\nlease acquire holder=$holder hard\nlease status\n";
my $a = <$s>;
my $b = <$s>;
print "PID $$\n$a$b";
"#;

/// A LEASE TAKEN BY ANOTHER PROCESS NAMES THAT PROCESS, whatever its
/// `holder=` says: a real child connects to a real socket served by the real
/// lanes and acquires `holder=aterm-harness@<this process's pid>` — the
/// forgery any same-uid client can make — and the lease records the CHILD's
/// pid, so `status hand_pid=` and `lease status pid=` name it and the
/// presence layer reads it as somebody else's hand.
#[cfg(target_os = "macos")]
#[test]
fn a_lease_taken_by_another_process_names_it_whatever_its_holder_says() {
    use std::io::Read as _;
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let sock = dir.path().join("l.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let handle = crate::session_store::test_handle(7602);
    let service = Arc::new(LeaseService {
        ctx: Arc::clone(&handle.ctx),
        replies: Mutex::new(Vec::new()),
    });
    let lanes = super::control_lanes::Lanes::new(super::control_lanes::LaneLimits {
        rpc: 1,
        wait: 0,
        inline: 0,
        open: 4,
    });
    assert_eq!(lanes.start(), 1, "the request lane starts");
    lanes.publish(service.clone());
    let own = crate::harness_host::holder();
    let mut child = aterm_uds::spawnfd::command("/usr/bin/perl")
        .env_clear()
        .args(["-e", FORGER])
        .arg(&sock)
        .arg(&own)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the forger");
    let child_pid = child.id();
    let (stream, _) = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the forger connects",
        || match listener.accept() {
            Ok(connection) => Some(connection),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(error) => panic!("accept forger: {error}"),
        },
    );
    stream.set_nonblocking(false).expect("blocking lane stream");
    assert!(lanes.admit(stream).is_ok(), "admitted");
    let mut said = String::new();
    child
        .stdout
        .take()
        .expect("piped stdout")
        .read_to_string(&mut said)
        .expect("read the forger");
    let _ = child.wait();
    assert!(said.contains(&format!("PID {child_pid}\n")), "{said}");
    assert!(said.contains("OK lease acquired"), "{said}");
    assert!(
        said.contains(&format!(" pid={child_pid}\n")),
        "`lease status` names the taker: {said}"
    );
    assert_eq!(
        drive_lease(&handle.ctx),
        Some((own.clone(), Some(child_pid))),
        "the kernel's peer, not the holder text's pid"
    );
    assert_ne!(child_pid, std::process::id());
    assert!(
        !crate::presence::attested_aterms_holder(&own, Some(child_pid), &own, std::process::id()),
        "the forged name is somebody else's hand"
    );
}
