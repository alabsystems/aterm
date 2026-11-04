// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cooperative account-image entry. This protocol is separate from binary
//! upgrades and shell restarts: a native host must authorize the account move.
//! No command, credential path or environment assignment comes from its argv.

use super::native_act::{Binding, Image};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

pub const ENTRY: &str = "codex-exec-handoff";
pub const HANDOFF_ENV: &str = "ATERM_CODEX_EXEC_HANDOFF";
const PREFIX: &str = "OK codex-exec-handoff ";
const MAX_REPLY: usize = super::native_act::MAX_REQUEST * 2 + 256;

/// Exact native-selected exec command. Unlike a shell restart, argv may carry
/// newlines and is never rendered or reparsed as shell text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecCommand {
    /// The original kernel-observed argv[0], independent of the physical image.
    pub argv0: String,
    pub image: Image,
    pub program: String,
    pub argv: Vec<String>,
    pub auth_path: String,
    pub cwd: Option<String>,
}

impl ExecCommand {
    #[must_use]
    pub fn encode(&self) -> Option<String> {
        if !std::path::Path::new(&self.program).is_absolute()
            || !std::path::Path::new(&self.auth_path).is_absolute()
            || self.image.path.first() != Some(&b'/')
            || self.image.path.contains(&0)
            || self.argv0.is_empty()
            || self.argv0.contains('\0')
            || self.program.contains('\0')
            || self.auth_path.contains('\0')
            || self.argv.iter().any(|arg| arg.contains('\0'))
            || self
                .cwd
                .as_ref()
                .is_some_and(|cwd| !std::path::Path::new(cwd).is_absolute() || cwd.contains('\0'))
        {
            return None;
        }
        let encoded = aterm_json::to_string(&aterm_json::json!({
            "image_path":self.image.path, "image_device":self.image.device,
            "image_inode":self.image.inode, "program":self.program, "argv0":self.argv0,
            "argv":self.argv, "auth_path":self.auth_path, "cwd":self.cwd,
        }))
        .ok()?;
        (encoded.len() <= super::native_act::MAX_REQUEST).then_some(encoded)
    }

    #[must_use]
    pub fn parse(encoded: &str) -> Option<Self> {
        if encoded.len() > super::native_act::MAX_REQUEST {
            return None;
        }
        let value: aterm_json::Value = aterm_json::from_str(encoded).ok()?;
        let object = value.as_object()?;
        if object.len() != 8 {
            return None;
        }
        let command = Self {
            argv0: value.get("argv0")?.as_str()?.into(),
            image: Image {
                path: aterm_json::from_value(value.get("image_path")?.clone()).ok()?,
                device: value.get("image_device")?.as_u64()?,
                inode: value.get("image_inode")?.as_u64()?,
            },
            program: value.get("program")?.as_str()?.into(),
            argv: aterm_json::from_value(value.get("argv")?.clone()).ok()?,
            auth_path: value.get("auth_path")?.as_str()?.into(),
            cwd: match value.get("cwd")? {
                aterm_json::Value::Null => None,
                value => Some(value.as_str()?.into()),
            },
        };
        (command.encode().as_deref() == Some(encoded)).then_some(command)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub socket: String,
    pub session: String,
    pub binding: Binding,
}

fn session(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

fn hex(value: &str, size: usize) -> bool {
    value.len() == size && aterm_codec::hex::is_lower(value, size)
}

impl Invocation {
    /// The capability authenticates the retained operation, not arbitrary argv.
    #[must_use]
    pub fn arguments(&self) -> Option<Vec<OsString>> {
        if !session(&self.session)
            || !hex(&self.binding.id, 32)
            || !hex(&self.binding.capability, 64)
            || !std::path::Path::new(&self.socket).is_absolute()
            || self.socket.len() > super::native_act::MAX_PATH
            || self.socket.contains('\0')
        {
            return None;
        }
        Some(
            [
                ENTRY.to_owned(),
                "contract=1".to_owned(),
                format!(
                    "socket={}",
                    aterm_codec::hex::encode_lower(self.socket.as_bytes())
                ),
                format!("session={}", self.session),
                format!("id={}", self.binding.id),
                format!("cap={}", self.binding.capability),
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        )
    }

    fn parse(args: &[OsString]) -> Option<Self> {
        if args.len() != 6 || args.first()?.to_str()? != ENTRY {
            return None;
        }
        let mut fields = BTreeMap::new();
        for word in &args[1..] {
            let (key, value) = word.to_str()?.split_once('=')?;
            if fields.insert(key, value).is_some() {
                return None;
            }
        }
        if fields.remove("contract")? != "1" {
            return None;
        }
        let encoded = fields.remove("socket")?;
        if encoded.len() > super::native_act::MAX_PATH * 2 {
            return None;
        }
        let socket = String::from_utf8(aterm_codec::hex::decode_vec(encoded.as_bytes())?).ok()?;
        let value = Self {
            socket,
            session: fields.remove("session")?.to_owned(),
            binding: Binding {
                id: fields.remove("id")?.to_owned(),
                capability: fields.remove("cap")?.to_owned(),
            },
        };
        if !fields.is_empty() || value.arguments()?.as_slice() != args {
            return None;
        }
        Some(value)
    }

    fn request(&self, verb: &str) -> String {
        format!(
            "@{} {ENTRY} {verb} id={} cap={}",
            self.session, self.binding.id, self.binding.capability
        )
    }

    fn ticket(&self) -> String {
        format!("{}.{}", self.binding.id, self.binding.capability)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Launch { command: ExecCommand, deadline: u64 },
    Execute { deadline: u64 },
}

impl Reply {
    #[must_use]
    pub fn encode(&self) -> Option<String> {
        let (state, deadline, command) = match self {
            Self::Launch { command, deadline } => (
                "launch",
                deadline,
                format!(
                    " command={}",
                    aterm_codec::hex::encode_lower(command.encode()?.as_bytes())
                ),
            ),
            Self::Execute { deadline } => ("execute", deadline, String::new()),
        };
        if *deadline == 0 {
            return None;
        }
        let line = format!("{PREFIX}state={state} deadline_us={deadline}{command}\n");
        (line.len() <= MAX_REPLY).then_some(line)
    }

    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        reply(line)
    }
}

fn reply(line: &str) -> Option<Reply> {
    if line.len() > MAX_REPLY {
        return None;
    }
    let mut fields = BTreeMap::new();
    for word in line.strip_prefix(PREFIX)?.split_ascii_whitespace() {
        let (key, value) = word.split_once('=')?;
        if fields.insert(key, value).is_some() {
            return None;
        }
    }
    let state = fields.remove("state")?;
    let text = fields.remove("deadline_us")?;
    let deadline: u64 = text.parse().ok()?;
    if deadline == 0 || deadline.to_string() != text {
        return None;
    }
    let result = match state {
        "launch" => {
            let encoded = fields.remove("command")?;
            let decoded =
                String::from_utf8(aterm_codec::hex::decode_vec(encoded.as_bytes())?).ok()?;
            if aterm_codec::hex::encode_lower(decoded.as_bytes()) != encoded {
                return None;
            }
            Reply::Launch {
                command: ExecCommand::parse(&decoded)?,
                deadline,
            }
        }
        "execute" => Reply::Execute { deadline },
        _ => return None,
    };
    fields.is_empty().then_some(result)
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "native account handoff was not admitted",
    )
}

fn validate(command: &ExecCommand) -> io::Result<()> {
    // The command type has no shell, hook or arbitrary environment fields.
    if Image::at(std::path::Path::new(&command.program)).as_ref() != Some(&command.image)
        || !std::path::Path::new(&command.auth_path).is_absolute()
        || command.auth_path.contains('\0')
        || !std::fs::symlink_metadata(&command.auth_path).is_ok_and(|m| m.is_file())
    {
        return Err(denied());
    }
    if let Some(cwd) = &command.cwd
        && std::fs::canonicalize(cwd)? != std::env::current_dir()?.canonicalize()?
    {
        return Err(denied());
    }
    Ok(())
}

#[cfg(unix)]
fn run(invocation: Invocation) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut client = super::upgrade_drive::harness_client(&invocation.socket, None)?;
    // SAFETY: reading descriptor flags does not transfer its ownership.
    let flags = unsafe { libc::fcntl(client.transport().as_raw_fd(), libc::F_GETFD) };
    if flags < 0 || flags & libc::FD_CLOEXEC == 0 {
        return Err(denied());
    }
    let line = client.request_line(&invocation.request("attach"))?;
    let Some(Reply::Launch { command, deadline }) = reply(&line) else {
        return Err(denied());
    };
    validate(&command)?;
    let mut exec = aterm_uds::spawnfd::transparent(&command.program);
    exec.arg0(&command.argv0);
    exec.args(&command.argv);
    exec.env("CODEX_AUTH_PATH", &command.auth_path);
    exec.env(HANDOFF_ENV, invocation.ticket());
    let exec = exec.prepare_exec()?;
    let line = client.request_line(&invocation.request("execute"))?;
    if !matches!(reply(&line), Some(Reply::Execute { deadline: same }) if same == deadline) {
        return Err(denied());
    }
    // The original PID and inherited stdin/stdout/stderr survive. Failure is
    // terminal; no shell command, new ticket or fallback account is attempted.
    Err(exec.exec_until(deadline))
}

/// Recognized before ordinary harness setup; absent host support refuses.
pub(super) fn entry(args: &[OsString]) -> Option<ExitCode> {
    if args.first().is_none_or(|arg| arg != ENTRY) {
        return None;
    }
    #[cfg(unix)]
    if let Some(invocation) = Invocation::parse(args) {
        let _ = run(invocation);
    }
    Some(ExitCode::from(126))
}

#[cfg(test)]
#[path = "codex_exec_handoff_tests.rs"]
mod tests;

#[path = "codex_exec_source.rs"]
mod source;
pub use source::ExecSource;

#[path = "codex_exec_transaction.rs"]
pub mod transaction;

#[path = "codex_exec_tree.rs"]
pub mod tree;
