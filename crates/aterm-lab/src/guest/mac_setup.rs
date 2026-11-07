// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The lab person's synthetic password for an exact owned Mac Setup PTY. Native AX password
//! rules remain in `super::act`; this path neither selects a window nor posts HID input.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_agent::RelayClient;
use aterm_types::macos_setup::{COMMAND_LINE_TOOLS_ARGS, MAC_SETUP_PASSWORD_PROMPT};
use aterm_uds::process::{Identity, read_identity, read_process_path};

use super::{Credentials, Out, app_pids};

// clt_install::receipt's whole v2 record; unknown versions fail closed.
const RECEIPT_BYTES: usize = 256;

#[derive(Clone, Debug)]
struct Process {
    identity: Identity,
    image: PathBuf,
    argv: Vec<Vec<u8>>,
}

fn process(pid: i32) -> Option<Process> {
    let identity = read_identity(pid)?;
    let image = read_process_path(pid)?.canonicalize().ok()?;
    let argv = aterm_sysprobe::kproc::argv(u32::try_from(pid).ok()?)?;
    (read_identity(pid) == Some(identity)).then_some(Process {
        identity,
        image,
        argv,
    })
}

#[derive(Clone, Debug)]
struct Binding {
    name: String,
    text: Vec<u8>,
    sid: String,
    child: Identity,
    claim: String,
}

impl Binding {
    fn read(
        mark: &Path,
        name: &str,
        exe: &Path,
        probe: &impl Fn(i32) -> Option<Process>,
    ) -> Option<Self> {
        use std::os::unix::ffi::OsStrExt as _;
        let directory = children(mark)?;
        let text = aterm_uds::private_file::read_owned(&directory, name, RECEIPT_BYTES).ok()?;
        let value = std::str::from_utf8(&text).ok()?;
        let mut lines = value.split('\n');
        if lines.next()? != "v2" {
            return None;
        }
        let sid = lines.next()?;
        if sid.len() > 64 || aterm_types::control_request::Sid::parse(sid).is_none() {
            return None;
        }
        let pid: i32 = lines.next()?.parse().ok()?;
        let birth: u64 = lines.next()?.parse().ok()?;
        let claim = lines.next()?.strip_prefix("claimed ")?;
        if pid <= 1
            || birth == 0
            || claim.is_empty()
            || claim.len() > aterm_types::clt_mark::BUILD_BYTES
            || claim.contains(char::is_whitespace)
            || lines.next() != Some("")
            || lines.next().is_some()
            || name != format!("{sid}-{pid}-{birth}")
            || value != format!("v2\n{sid}\n{pid}\n{birth}\nclaimed {claim}\n")
        {
            return None;
        }
        let parent = mark.parent()?;
        let mark_name = mark.file_name()?.to_str()?;
        let claimed =
            aterm_uds::private_file::read_owned(parent, mark_name, aterm_types::clt_mark::BYTES)
                .ok()?;
        if claimed != format!("opened {claim}\n").as_bytes() {
            return None;
        }
        let child = probe(pid)?;
        if child.identity.pid != pid
            || child.identity.birth.0 != birth
            || child.image != exe
            || child.argv.len() != COMMAND_LINE_TOOLS_ARGS.len() + 1
            || child.argv[0] != exe.as_os_str().as_bytes()
            || !child.argv[1..]
                .iter()
                .zip(COMMAND_LINE_TOOLS_ARGS)
                .all(|(got, expected)| got == expected.as_bytes())
        {
            return None;
        }
        let sid = sid.to_owned();
        let claim = claim.to_owned();
        Some(Self {
            name: name.to_owned(),
            text,
            sid,
            child: child.identity,
            claim,
        })
    }

    fn current(&self, mark: &Path, exe: &Path) -> bool {
        Self::read(mark, &self.name, exe, &process).is_some_and(|now| {
            now.text == self.text && now.child == self.child && now.claim == self.claim
        })
    }
}

fn children(mark: &Path) -> Option<PathBuf> {
    let mut name = mark.file_name()?.to_os_string();
    name.push(".children");
    Some(mark.with_file_name(name))
}

fn binding(mark: &Path, exe: &Path) -> Option<Binding> {
    let directory = children(mark)?;
    aterm_uds::private_file::check_owned_dir(&directory).ok()?;
    let mut found = None;
    for entry in std::fs::read_dir(&directory).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Some(candidate) = Binding::read(mark, name, exe, &process) {
            if found.is_some() {
                return None;
            }
            found = Some(candidate);
        }
    }
    found
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Frame {
    generation: String,
    fingerprint: String,
    human: u64,
}

impl Frame {
    fn read(line: &str) -> Option<Self> {
        if line.split_whitespace().next()? != "OK" {
            return None;
        }
        let fields = line.split_whitespace().skip(1).collect::<Vec<_>>();
        let one = |name: &str| {
            let mut values = fields.iter().filter_map(|word| word.strip_prefix(name));
            let value = values.next()?;
            values.next().is_none().then_some(value)
        };
        let generation = one("gen=")?;
        let (epoch, seq) = generation.split_once('.')?;
        if epoch.is_empty()
            || seq.is_empty()
            || !epoch.bytes().all(|b| b.is_ascii_digit())
            || !seq.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let fingerprint = one("hash=")?;
        if fingerprint.len() != 16 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(Self {
            generation: generation.to_owned(),
            fingerprint: fingerprint.to_owned(),
            human: one("human_seq=")?.parse().ok()?,
        })
    }
}

/// Read a coherent prompt and keep the ordinary server-side input fences at the write.
/// No credential appears in argv or a diagnostic; unknown input outcomes are never retried.
fn answer<S: Read + Write>(
    client: &mut RelayClient<S>,
    target: &Binding,
    password: &str,
    current: impl Fn() -> bool,
    attempted: &mut BTreeSet<String>,
) -> std::io::Result<Option<u64>> {
    // `send --` preserves ordinary payload bytes, but request lines cannot hold CR/LF and its
    // leading separator trims whitespace. These are the existing VM's synthetic passwords.
    if password.is_empty()
        || password.starts_with(char::is_whitespace)
        || password.contains(['\r', '\n'])
        || attempted.contains(&target.name)
        || !current()
    {
        return Ok(None);
    }
    let status = format!("@{} status", target.sid);
    let before = client.request_line(&status)?;
    let Some(before) = Frame::read(&before) else {
        return Ok(None);
    };
    let (header, text) = client.request_counted(&format!("@{} text", target.sid))?;
    // Only the stable beginning is compared: the real saved frame soft-wraps the word
    // "Mac" inside the introduction. The dedicated child/receipt remains the authority.
    let warning = text
        .lines()
        .find(|line| !line.is_empty())
        .is_some_and(|line| {
            line.starts_with("aterm is installing Apple's Command Line Tools")
                || line.starts_with("aterm is finishing the part of this Mac's setup")
        });
    if !header.starts_with("OK ")
        || header.split_whitespace().any(|word| word == "frame=open")
        || !warning
        || !text
            .lines()
            .rev()
            .find(|line| !line.is_empty())
            .is_some_and(|line| line == MAC_SETUP_PASSWORD_PROMPT.trim_end())
    {
        return Ok(None);
    }
    let after = client.request_line(&status)?;
    if Frame::read(&after).as_ref() != Some(&before) || !current() {
        return Ok(None);
    }
    attempted.insert(target.name.clone());
    let request = format!(
        "@{} send if-gen={} if-fp={} if-human={} -- {}\\n",
        target.sid, before.generation, before.fingerprint, before.human, password,
    );
    // The dispatcher stamps full delivery as `OK seq=<u64>` after its guarded input reply.
    // Skips, duplicate writes and unknown metadata never become an acknowledged password.
    let reply = client.request_line(&request)?;
    Ok(reply.strip_prefix("OK seq=").and_then(|sequence| {
        (!sequence.is_empty() && sequence.bytes().all(|b| b.is_ascii_digit()))
            .then(|| sequence.parse().ok())
            .flatten()
    }))
}

#[derive(Default)]
pub(super) struct Person {
    attempted: BTreeSet<String>,
    known: BTreeMap<(u32, u64), bool>,
}

impl Person {
    pub(super) fn step(&mut self, out: &Out, who: &Credentials, exe: &str) {
        if who.password.is_empty() {
            return;
        }
        let Some(mark) = aterm_types::clt_mark::path() else {
            return;
        };
        let Ok(exe_path) = Path::new(exe).canonicalize() else {
            return;
        };
        let Some(target) = binding(&mark, &exe_path) else {
            return;
        };
        if self.attempted.contains(&target.name) {
            return;
        }
        let Some(apps) = app_pids(exe, &mut self.known) else {
            return;
        };
        let Ok(sock) = aterm_ctl::resolve_sock_for(Some(&target.sid)) else {
            return;
        };
        let Ok((mut stream, token)) = aterm_ctl::connect_then_read_token(&sock) else {
            return;
        };
        let Some(peer) = aterm_uds::peer::peer_audit_token(&stream) else {
            return;
        };
        if aterm_uds::peer::peer_uid(&stream) != Some(aterm_uds::peer::our_uid())
            || !apps.contains_key(&peer.pid())
        {
            return;
        }
        let Ok(gui_pid) = i32::try_from(peer.pid()) else {
            return;
        };
        let Some(gui) = read_identity(gui_pid) else {
            return;
        };
        if gui.idversion.and_then(|v| u32::try_from(v).ok()) != Some(peer.pid_version())
            || read_process_path(gui.pid)
                .and_then(|p| p.canonicalize().ok())
                .as_ref()
                != Some(&exe_path)
            || !aterm_sysprobe::kproc::argv(peer.pid())
                .is_some_and(|argv| argv.len() == 1 && argv[0] == exe.as_bytes())
        {
            return;
        }
        let Ok(token) = token else {
            return;
        };
        if stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .is_err()
            || writeln!(stream, "AUTH {token}").is_err()
        {
            return;
        }
        let mut client = RelayClient::new(stream).bounded(Duration::from_secs(2));
        let current = || read_identity(gui.pid) == Some(gui) && target.current(&mark, &exe_path);
        let sequence = answer(
            &mut client,
            &target,
            &who.password,
            current,
            &mut self.attempted,
        )
        .ok()
        .flatten();
        let delivered = sequence.is_some();
        if self.attempted.contains(&target.name) {
            out.line(
                "root-pty",
                &format!(
                    "sid={} pid={} birth={} delivered={} seq={}",
                    target.sid,
                    target.child.pid,
                    target.child.birth.0,
                    u8::from(delivered),
                    sequence.map_or_else(|| "-".to_owned(), |value| value.to_string()),
                ),
            );
            if delivered {
                // Existing typed record, explicitly a control-delivered PTY password, not a
                // native secure field or a claim that a posted HID key was measured.
                out.line(
                    "typed",
                    &format!(
                        "{} account {} - unwatched aterm-Mac-Setup-PTY(control)",
                        target.child.pid,
                        who.password.chars().count(),
                    ),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::Cursor;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::{Arc, Mutex};

    struct Wire {
        replies: Cursor<Vec<u8>>,
        sent: Arc<Mutex<Vec<u8>>>,
    }

    impl Read for Wire {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.replies.read(bytes)
        }
    }

    impl Write for Wire {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.sent.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn owned_mac_setup_password_requires_current_receipt_child_and_fenced_prompt() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join(aterm_types::clt_mark::FILE);
        let directory = children(&mark).unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let exe = root.path().join("aterm");
        let name = "s-0123456789abcdef0123-42-700";
        let text = b"v2\ns-0123456789abcdef0123\n42\n700\nclaimed 1.0.0+gfixture\n";
        aterm_uds::private_file::write_private(
            root.path(),
            aterm_types::clt_mark::FILE,
            b"opened 1.0.0+gfixture\n",
        )
        .unwrap();
        aterm_uds::private_file::write_private(&directory, name, text).unwrap();
        let child = Process {
            identity: Identity {
                pid: 42,
                birth: aterm_uds::process::Birth(700),
                idversion: Some(3),
            },
            image: exe.clone(),
            argv: std::iter::once(exe.to_str().unwrap())
                .chain(COMMAND_LINE_TOOLS_ARGS)
                .map(|arg| arg.as_bytes().to_vec())
                .collect(),
        };
        let admitted = || {
            Binding::read(&mark, name, &exe, &|pid| {
                (pid == child.identity.pid).then(|| child.clone())
            })
        };
        let target = admitted().expect("real owned whole receipt and exact child");
        for changed in ["pid", "birth", "argv", "image"] {
            let mut wrong = child.clone();
            match changed {
                "pid" => wrong.identity.pid += 1,
                "birth" => wrong.identity.birth.0 += 1,
                "argv" => wrong.argv = vec![b"aterm".to_vec(), b"shell".to_vec()],
                _ => wrong.image = root.path().join("another-app"),
            }
            assert!(Binding::read(&mark, name, &exe, &|_| Some(wrong.clone())).is_none());
        }
        aterm_uds::private_file::write_private(
            root.path(),
            aterm_types::clt_mark::FILE,
            b"opened another-owner\n",
        )
        .unwrap();
        assert!(
            admitted().is_none(),
            "a receipt is no authority over another claim"
        );
        aterm_uds::private_file::write_private(
            root.path(),
            aterm_types::clt_mark::FILE,
            b"opened 1.0.0+gfixture\n",
        )
        .unwrap();
        aterm_uds::private_file::write_private(
            &directory,
            name,
            b"v2\ns-different\n42\n700\nclaimed 1.0.0+gfixture\n",
        )
        .unwrap();
        assert!(
            admitted().is_none(),
            "the file name binds the whole SID/child record"
        );
        aterm_uds::private_file::write_private(&directory, name, text).unwrap();

        const STATUS: &str = "OK gen=1.2 hash=0123456789abcdef human_seq=7\n";
        let run = |after: &str, ack: &str, current: &dyn Fn() -> bool| {
            let sent = Arc::new(Mutex::new(Vec::new()));
            // The saved failed VM's actual display wrap, including "Ma" / "c", then
            // the prompt and an empty display row. This is not an idealized secure field.
            let replies = format!(
                "{STATUS}OK 5\naterm is installing Apple's Command Line Tools for building software, turning on\ndeveloper mode, and setting up its local network helper. macOS asks for your Ma\nc password once.\nMac password:\n\n{after}{ack}"
            );
            let mut client = RelayClient::new(Wire {
                replies: Cursor::new(replies.into_bytes()),
                sent: sent.clone(),
            });
            let mut attempted = BTreeSet::new();
            let delivered = answer(
                &mut client,
                &target,
                "nux-fixture-password",
                current,
                &mut attempted,
            )
            .unwrap();
            let before = sent.lock().unwrap().clone();
            if !attempted.is_empty() {
                assert!(
                    answer(
                        &mut client,
                        &target,
                        "nux-fixture-password",
                        current,
                        &mut attempted
                    )
                    .unwrap()
                    .is_none()
                );
                assert_eq!(
                    *sent.lock().unwrap(),
                    before,
                    "one attempt even after uncertain delivery"
                );
            }
            (delivered, String::from_utf8(before).unwrap())
        };
        let (delivered, sent) = run(STATUS, "OK seq=9\n", &|| true);
        assert_eq!(delivered, Some(9));
        assert!(sent.ends_with(
            "send if-gen=1.2 if-fp=0123456789abcdef if-human=7 -- nux-fixture-password\\n\n"
        ));
        let (delivered, sent) = run(
            "OK gen=1.3 hash=0123456789abcdef human_seq=7\n",
            "",
            &|| true,
        );
        assert!(delivered.is_none());
        assert!(
            !sent.contains(" send "),
            "a changed prompt frame gets no credential bytes"
        );
        let calls = Cell::new(0);
        let (delivered, sent) = run(STATUS, "", &|| {
            calls.set(calls.get() + 1);
            calls.get() == 1
        });
        assert!(delivered.is_none());
        assert!(
            !sent.contains(" send "),
            "retired child custody gets no credential bytes"
        );
        for ack in [
            "OK skipped reason=person seq=9\n",
            "ERR write failed\n",
            "OK seq=9 dup=1\n",
            "OK seq=9 seq=10\n",
            "OK seq=9 unknown=1\n",
            "OK seq=18446744073709551616\n",
            "OK\n",
        ] {
            let (delivered, _) = run(STATUS, ack, &|| true);
            assert!(delivered.is_none(), "no full-delivery receipt: {ack}");
        }
    }
}
