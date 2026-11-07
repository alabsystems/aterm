// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The allowed direct path's server (`aterm_uds::lan::direct`,
//! `docs/DESIGN-lan-broker-2026-11-05.md` §5): the broker's own [`connection`] served inside this
//! process over a socketpair, to this process alone, while no broker is installed and macOS's own
//! record says it allowed this code on the local network ([`crate::grant`]).
//!
//! A Mac updated from a build with no broker (0.158.0 and earlier) holds that allowance for aterm
//! and no broker until a person types the Mac password the broker's install needs; this keeps
//! its LAN meanwhile, raising no alert, since macOS asks only code it has no answer for. The
//! process makes its own LAN sockets and DNS-SD calls as it did before the broker. Each session
//! reads both when it opens, so once the broker answers, or macOS's answer for this account
//! changes, the next session goes to the broker or finds no path; a session already open (a
//! browse, an advert) runs until its client ends it.
//!
//! macOS judges a LAN use by the process's responsible app, which is not always aterm: an `aterm`
//! command typed in Terminal is Terminal's to answer for. So the path serves only a process whose
//! responsible app runs this same executable (the app, a tab's command, a helper it started) or
//! that is its own: any other would ask its app's question, which the broker never does.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use aterm_uds::CtlStream;

use crate::grant;
use crate::server::{Admission, Shared, connection};

/// This process's direct path: its code (read once) and its server (made once).
struct Direct {
    code: OnceLock<Result<grant::Code, String>>,
    shared: OnceLock<Arc<Shared>>,
    /// Where macOS's answers are read ([`grant::NEHELPER_PLIST`]; a test's fixture).
    plist: &'static str,
    /// Whether macOS answers this process's LAN use as `code`'s own
    /// ([`responsible_is_this_code`]; a test's stand-in).
    responsible: fn(&grant::Code) -> Result<(), String>,
    /// Where the server's connection lines go ([`quiet`]; a test's recorder).
    log: fn(std::fmt::Arguments<'_>),
}

/// The direct path's server log: nothing. Its one client is this process, which hears every
/// failure as the broker's frame and says it in its own words; the broker's log lines would
/// otherwise land on this process's stderr (a person's terminal, or `ssh`'s from `lan fdpass`).
fn quiet(_: std::fmt::Arguments<'_>) {}

/// Whether the app macOS holds responsible for this process (the one whose Local Network answer
/// it applies) is `code`'s executable, or this process itself.
///
/// # Errors
/// Another app's, in words, or a responsible app macOS did not name.
fn responsible_is_this_code(code: &grant::Code) -> Result<(), String> {
    let me = i32::try_from(std::process::id()).map_err(|_| "a pid past i32".to_owned())?;
    let responsible = aterm_containment::responsible_pid(me)
        .ok_or_else(|| "macOS did not name the app responsible for this process".to_owned())?;
    if responsible == me {
        return Ok(());
    }
    let path = pid_path(responsible).ok_or_else(|| {
        format!("the app responsible for this process (pid {responsible}) is gone")
    })?;
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    if path.to_string_lossy() == code.executable {
        Ok(())
    } else {
        Err(format!(
            "macOS answers this process's LAN use as {}'s (pid {responsible}), not aterm's",
            path.display()
        ))
    }
}

/// `pid`'s executable, from `proc_pidpath`.
fn pid_path(pid: i32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut buf = [0u8; 4 * 1024];
    let len = u32::try_from(buf.len()).ok()?;
    // SAFETY: `buf` is writable for `len` bytes, all `proc_pidpath` may write.
    let wrote = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), len) };
    let wrote = usize::try_from(wrote).ok().filter(|n| *n > 0)?;
    Some(std::ffi::OsStr::from_bytes(&buf[..wrote.min(buf.len())]).into())
}

impl Direct {
    fn code(&self) -> Result<&grant::Code, String> {
        self.code
            .get_or_init(grant::this_code)
            .as_ref()
            .map_err(Clone::clone)
    }

    fn shared(&self) -> &Arc<Shared> {
        // Admitted by uid: the one peer is this process, at the socketpair's other end.
        self.shared.get_or_init(|| {
            Arc::new(Shared::new(
                Admission::SameUid,
                crate::bonjour::system(),
                self.log,
            ))
        })
    }
}

impl aterm_uds::lan::direct::Direct for Direct {
    fn allowed(&self) -> Result<String, String> {
        let code = self.code()?;
        let bytes = grant::read_bounded(Path::new(self.plist))?;
        let allowed = grant::allowed(&bytes, code)?;
        (self.responsible)(code)?;
        Ok(allowed)
    }

    fn serve(&self, stream: CtlStream) {
        connection(self.shared(), &stream);
    }
}

static DIRECT: Direct = Direct {
    code: OnceLock::new(),
    shared: OnceLock::new(),
    plist: grant::NEHELPER_PLIST,
    responsible: responsible_is_this_code,
    log: quiet,
};

/// Give this process the allowed direct path (`aterm`'s start does, once). It reads nothing until
/// a LAN use finds no broker.
pub fn provide_direct() {
    // A second provision is the first's, already in place.
    let _ = aterm_uds::lan::direct::provide(&DIRECT);
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::Mutex;
    use std::time::Duration;

    use aterm_uds::lan::wire::Target;
    use aterm_uds::lan::{ConnectOptions, LanError, Seam, Unavailable, broker, direct, seam_here};

    use super::*;

    /// A hang detector, never a latency budget.
    const MUST_HAPPEN: Duration = Duration::from_secs(60);

    /// The account the fixtures' configuration is for.
    const GUEST_USER: &str = "E8D0C90E-068E-4C80-AC44-6815AE2AC56B";

    /// The direct path as this test binary's code sees it: macOS's record read from `plist` (a
    /// fixture), the code named as released 0.158.0's run by the fixtures' account, and macOS
    /// answering for this process as that code (the responsible app's stand-in).
    fn path_reading(plist: &'static str) -> &'static Direct {
        path_answered_as(plist, |_| Ok(()))
    }

    fn path_answered_as(
        plist: &'static str,
        responsible: fn(&grant::Code) -> Result<(), String>,
    ) -> &'static Direct {
        path_of(plist, responsible, GUEST_USER, quiet)
    }

    fn path_of(
        plist: &'static str,
        responsible: fn(&grant::Code) -> Result<(), String>,
        user: &str,
        log: fn(std::fmt::Arguments<'_>),
    ) -> &'static Direct {
        let direct = Box::leak(Box::new(Direct {
            code: OnceLock::new(),
            shared: OnceLock::new(),
            plist,
            responsible,
            log,
        }));
        let _ = direct.code.set(Ok(grant::Code {
            identifier: "com.aterm.aterm".into(),
            team: Some("A66A9P66Z7".into()),
            executable: "/Applications/aterm.app/Contents/MacOS/aterm".into(),
            user: user.into(),
        }));
        direct
    }

    /// The lines [`record`] heard: one test's server log.
    static RECORDED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn record(line: std::fmt::Arguments<'_>) {
        RECORDED.lock().unwrap().push(line.to_string());
    }

    fn fixture(name: &str) -> &'static str {
        let path = aterm_tempfile::manifest_dir()
            .join("src/fixtures")
            .join(name);
        Box::leak(path.to_string_lossy().into_owned().into_boxed_str())
    }

    /// No broker at the seam's socket, loopback classified as local network.
    fn no_broker(root: &aterm_tempfile::TempDir) -> Seam {
        Seam {
            loopback_is_local: true,
            socket: Some(root.path().join("absent").join("broker.sock")),
            ..Seam::default()
        }
    }

    /// With no broker installed and macOS's allowance for this code on record, the broker's
    /// protocol is served in-process: a hello says so, and a LAN connect comes back connected.
    #[test]
    fn no_broker_and_an_allowance_serve_this_process_itself() {
        let root = aterm_tempfile::tempdir().unwrap();
        let allowed = path_reading(fixture("nehelper-granted.plist"));
        let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = target.local_addr().unwrap();
        let (hello, mut stream) = seam_here(no_broker(&root), || {
            direct::seam_here(allowed, || {
                let hello = broker::hello().unwrap();
                let (stream, reached) = broker::connect(
                    &Target::Addr(addr),
                    addr.port(),
                    MUST_HAPPEN,
                    ConnectOptions::default(),
                )
                .unwrap();
                assert_eq!(reached, addr);
                (hello, stream)
            })
        });
        assert_eq!(hello.protocol, aterm_uds::lan::PROTOCOL);
        assert_eq!(
            hello.direct.as_deref(),
            Some("macOS allowed com.aterm.aterm (team A66A9P66Z7) on the local network")
        );
        let (mut far, _) = target.accept().unwrap();
        stream.write_all(b"ping").unwrap();
        let mut got = [0u8; 4];
        far.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
        far.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"ping");
    }

    /// NEGATIVE CONTROLS: the same process with no allowance on record (macOS asked and heard
    /// nothing; a dismissed question; an unreadable record), or one another app answers for, has
    /// no direct path and reads NotInstalled, as a build with none does; and a broker that answers
    /// is the broker, even beside an allowance: its answer stands.
    #[test]
    fn without_an_allowance_or_beside_a_broker_there_is_no_direct_path() {
        let root = aterm_tempfile::tempdir().unwrap();
        for plist in [
            fixture("nehelper-unanswered.plist"),
            fixture("nehelper-dismissed.plist"),
            Box::leak(
                root.path()
                    .join("missing.plist")
                    .to_string_lossy()
                    .into_owned()
                    .into_boxed_str(),
            ),
        ] {
            let reading = path_reading(plist);
            let got = seam_here(no_broker(&root), || {
                direct::seam_here(reading, broker::hello)
            });
            assert!(
                matches!(got, Err(LanError::Unavailable(Unavailable::NotInstalled))),
                "{plist}: {got:?}"
            );
        }
        // macOS's allowance on record, and this process answered for by another app (an `aterm`
        // typed in Terminal): no direct path, since that app's question is not aterm's.
        let terminal = path_answered_as(fixture("nehelper-granted.plist"), |_| {
            Err("macOS answers this process's LAN use as Terminal's".to_owned())
        });
        let got = seam_here(no_broker(&root), || {
            direct::seam_here(terminal, broker::hello)
        });
        assert!(
            matches!(got, Err(LanError::Unavailable(Unavailable::NotInstalled))),
            "{got:?}"
        );
        // macOS's allowance on record for another account of this Mac: this account's own
        // question is still unanswered, so no direct path.
        let other_account = path_of(
            fixture("nehelper-granted.plist"),
            |_| Ok(()),
            "5D773287-9015-4E7D-A8F3-7CE6088E62D5",
            quiet,
        );
        let got = seam_here(no_broker(&root), || {
            direct::seam_here(other_account, broker::hello)
        });
        assert!(
            matches!(got, Err(LanError::Unavailable(Unavailable::NotInstalled))),
            "{got:?}"
        );
        // A broker that speaks another protocol at the socket: Outdated, though macOS allowed
        // this code.
        let socket = root.path().join("run").join("broker.sock");
        let server =
            crate::server::Server::bind(&socket, Admission::SameUid, crate::bonjour::system())
                .unwrap();
        std::thread::spawn(move || {
            let _ = server.run();
        });
        let allowed = path_reading(fixture("nehelper-granted.plist"));
        let newer = Seam {
            socket: Some(socket),
            protocol: Some(aterm_uds::lan::PROTOCOL + 1),
            ..Seam::default()
        };
        let got = seam_here(newer, || direct::seam_here(allowed, broker::hello));
        assert!(
            matches!(
                got,
                Err(LanError::Unavailable(Unavailable::Outdated { .. }))
            ),
            "{got:?}"
        );
    }

    /// A request the direct path fails reaches its client as the broker's error, and the server's
    /// line for it goes to the path's own log, never the broker's stderr: the production path's
    /// log is [`quiet`], so nothing lands on the person's terminal. NEGATIVE CONTROL: with the
    /// connection's lines written by the broker's own log again, the recorder hears nothing.
    #[test]
    fn a_failed_direct_request_is_the_clients_alone_to_say() {
        let root = aterm_tempfile::tempdir().unwrap();
        let recorded = path_of(
            fixture("nehelper-granted.plist"),
            |_| Ok(()),
            GUEST_USER,
            record,
        );
        let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = closed.local_addr().unwrap();
        drop(closed);
        let got = seam_here(no_broker(&root), || {
            direct::seam_here(recorded, || {
                broker::connect(
                    &Target::Addr(addr),
                    addr.port(),
                    MUST_HAPPEN,
                    ConnectOptions::default(),
                )
            })
        });
        let Err(LanError::Io(error)) = got else {
            panic!("a refused connect is the act's error: {got:?}");
        };
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::ConnectionRefused,
            "{error}"
        );
        let lines = RECORDED.lock().unwrap().clone();
        assert!(
            lines.iter().any(|line| line.contains("refused")),
            "the server's line went to the path's log: {lines:?}"
        );
    }
}
