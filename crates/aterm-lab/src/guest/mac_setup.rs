// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The lab person's synthetic password for an exact owned Mac Setup PTY. Native AX password
//! rules remain in `super::act`; this path neither selects a window nor posts HID input.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_agent::RelayClient;
use aterm_types::macos_setup::COMMAND_LINE_TOOLS_ARGS;
use aterm_uds::process::{Identity, read_identity, read_process_path};

use super::{Credentials, Out, app_pids};

#[path = "mac_setup_prompt.rs"]
mod mac_setup_prompt;

// clt_install::receipt's whole v3 record or the live v2 custody it still adopts.
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
    claim: ReceiptClaim,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ReceiptClaim {
    Current(aterm_types::clt_mark::Claim),
    Earlier(String),
}

impl ReceiptClaim {
    fn read(version: &str, line: &str) -> Option<Self> {
        match version {
            "v3" => aterm_types::clt_mark::Claim::from_receipt_line(line).map(Self::Current),
            "v2" => {
                let build = line.strip_prefix("claimed ")?;
                (!build.is_empty()
                    && build.len() <= aterm_types::clt_mark::BUILD_BYTES
                    && !build.contains(char::is_whitespace))
                .then(|| Self::Earlier(build.to_owned()))
            }
            _ => None,
        }
    }

    fn held_in(&self, record: &[u8]) -> bool {
        match self {
            Self::Current(claim) => {
                std::str::from_utf8(record).is_ok_and(|text| claim.is_held_in(text))
            }
            Self::Earlier(build) => record == format!("opened {build}\n").as_bytes(),
        }
    }
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
        let version = lines.next()?;
        let sid = lines.next()?;
        if aterm_types::control_request::Sid::parse(sid).is_none() {
            return None;
        }
        let pid: i32 = lines.next()?.parse().ok()?;
        let birth: u64 = lines.next()?.parse().ok()?;
        let claim_line = lines.next()?;
        let claim = ReceiptClaim::read(version, claim_line)?;
        if pid <= 1
            || birth == 0
            || lines.next() != Some("")
            || lines.next().is_some()
            || name != format!("{sid}-{pid}-{birth}")
            || value != format!("{version}\n{sid}\n{pid}\n{birth}\n{claim_line}\n")
        {
            return None;
        }
        let parent = mark.parent()?;
        let mark_name = mark.file_name()?.to_str()?;
        let claimed =
            aterm_uds::private_file::read_owned(parent, mark_name, aterm_types::clt_mark::BYTES)
                .ok()?;
        if !claim.held_in(&claimed) {
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
    if !header.starts_with("OK ")
        || header.split_whitespace().any(|word| word == "frame=open")
        || !mac_setup_prompt::shown(&text)
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
#[path = "mac_setup_tests.rs"]
mod tests;
