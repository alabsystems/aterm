// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The public covering-claim wire. Admission and exec belong to build_admit.
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    os::{
        fd::{FromRawFd as _, RawFd},
        unix::{ffi::OsStrExt as _, fs::FileTypeExt as _},
    },
    path::PathBuf,
    process::ExitCode,
};

pub(crate) const USAGE: &str = "aterm pkg claim --capabilities\n\
    aterm pkg claim --what LABEL --key K --checkout DIR --rank ordinary|release \
    --cores N [--mem-bytes N] [--alone] [--flock-fd FD] \
    [--no-wait [--since-ms MS] | --wait-ms MS] --status-fd FD -- PROGRAM ARGS...\n\n\
    macOS: one covering account-ledger claim; the admitted command keeps its PID \
    and inherited descriptors. Status is one JSON line on a separate inherited \
    writable pipe/socket, then closed. No admission exits 75 without starting. \
    Memory defaults to the measured peak under K, then the ledger's cold figure; \
    an explicit --mem-bytes 0 remains zero. --flock-fd is an inherited regular \
    file, locked only by the admitting look and retained across exec.";

#[derive(Debug)]
pub(crate) enum ClaimCommand {
    Capabilities,
    Exec(PublicClaim),
}

#[derive(Debug)]
pub(crate) struct PublicClaim {
    pub(crate) what: String,
    pub(crate) key: String,
    pub(crate) checkout: PathBuf,
    pub(crate) rank: aterm_capacity::Rank,
    pub(crate) cores: u32,
    pub(crate) mem_bytes: Option<u64>,
    pub(crate) alone: bool,
    pub(crate) flock: Option<File>,
    pub(crate) no_wait: bool,
    pub(crate) since_ms: Option<u64>,
    pub(crate) wait_ms: Option<u64>,
    pub(crate) status: File,
    pub(crate) command: Vec<OsString>,
}

#[derive(Debug, PartialEq, Eq)]
enum Words {
    Capabilities,
    Exec(ClaimWords),
}

#[derive(Debug, PartialEq, Eq)]
struct ClaimWords {
    what: String,
    key: String,
    checkout: PathBuf,
    rank: aterm_capacity::Rank,
    cores: u32,
    mem_bytes: Option<u64>,
    alone: bool,
    flock_fd: Option<RawFd>,
    no_wait: bool,
    since_ms: Option<u64>,
    wait_ms: Option<u64>,
    status_fd: RawFd,
    command: Vec<OsString>,
}

fn number(value: &OsStr, flag: &str) -> Result<u64, String> {
    let value = value
        .to_str()
        .ok_or_else(|| format!("{flag} needs decimal digits"))?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("{flag} needs nonnegative decimal digits"));
    }
    value.parse().map_err(|_| format!("{flag} is out of range"))
}

fn text(value: &OsStr, flag: &str) -> Result<String, String> {
    let value = value
        .to_str()
        .ok_or_else(|| format!("{flag} needs UTF-8 text"))?;
    if value.is_empty() || value.contains('\0') {
        return Err(format!("{flag} needs nonempty text without NUL"));
    }
    Ok(value.to_owned())
}

fn once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("{flag} was supplied more than once"));
    }
    *slot = Some(value);
    Ok(())
}

fn parse_words(args: &[OsString]) -> Result<Words, String> {
    if args == [OsString::from("--capabilities")] {
        return Ok(Words::Capabilities);
    }
    let (mut what, mut key, mut checkout, mut rank, mut cores) = (None, None, None, None, None);
    let (mut memory, mut flock, mut since, mut wait, mut status) = (None, None, None, None, None);
    let (mut alone, mut no_wait, mut command) = (false, false, None);
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].to_str().ok_or("claim option is not UTF-8")?;
        index += 1;
        if flag == "--" {
            command = Some(args[index..].to_vec());
            break;
        }
        if matches!(flag, "--alone" | "--no-wait") {
            let selected = if flag == "--alone" {
                &mut alone
            } else {
                &mut no_wait
            };
            if *selected {
                return Err(format!("{flag} was supplied more than once"));
            }
            *selected = true;
            continue;
        }
        if !matches!(
            flag,
            "--what"
                | "--key"
                | "--checkout"
                | "--rank"
                | "--cores"
                | "--mem-bytes"
                | "--flock-fd"
                | "--since-ms"
                | "--wait-ms"
                | "--status-fd"
        ) {
            return Err(format!("unknown claim option {flag:?}"));
        }
        let value = args
            .get(index)
            .ok_or_else(|| format!("{flag} needs a value"))?;
        index += 1;
        match flag {
            "--what" => once(&mut what, text(value, flag)?, flag)?,
            "--key" => once(&mut key, text(value, flag)?, flag)?,
            "--checkout" => {
                if value.is_empty() || value.as_bytes().contains(&0) {
                    return Err("--checkout needs a nonempty path without NUL".into());
                }
                once(&mut checkout, PathBuf::from(value), flag)?;
            }
            "--rank" => {
                let value = match value.to_str() {
                    Some("ordinary") => aterm_capacity::Rank::Ordinary,
                    Some("release") => aterm_capacity::Rank::Release,
                    _ => return Err("--rank needs ordinary or release".into()),
                };
                once(&mut rank, value, flag)?;
            }
            "--cores" => {
                let value =
                    u32::try_from(number(value, flag)?).map_err(|_| "--cores is out of range")?;
                if value == 0 {
                    return Err("--cores must be positive".into());
                }
                once(&mut cores, value, flag)?;
            }
            "--mem-bytes" => once(&mut memory, number(value, flag)?, flag)?,
            "--since-ms" => once(&mut since, number(value, flag)?, flag)?,
            "--wait-ms" => once(&mut wait, number(value, flag)?, flag)?,
            "--flock-fd" | "--status-fd" => {
                let fd = RawFd::try_from(number(value, flag)?)
                    .map_err(|_| format!("{flag} is out of range"))?;
                if fd < 3 {
                    return Err(format!("{flag} must be at least 3"));
                }
                once(
                    if flag == "--flock-fd" {
                        &mut flock
                    } else {
                        &mut status
                    },
                    fd,
                    flag,
                )?;
            }
            _ => unreachable!(),
        }
    }
    if since.is_some() && !no_wait {
        return Err("--since-ms requires --no-wait".into());
    }
    if no_wait && wait.is_some() {
        return Err("--no-wait and --wait-ms are mutually exclusive".into());
    }
    let command = command.ok_or("claim needs -- PROGRAM ARGS...")?;
    if command.first().is_none_or(|program| program.is_empty())
        || command.iter().any(|word| word.as_bytes().contains(&0))
    {
        return Err("claim needs a nonempty PROGRAM and arguments without NUL".into());
    }
    let status_fd = status.ok_or("claim needs --status-fd")?;
    if flock == Some(status_fd) {
        return Err("status and flock descriptors must be distinct".into());
    }
    Ok(Words::Exec(ClaimWords {
        what: what.ok_or("claim needs --what")?,
        key: key.ok_or("claim needs --key")?,
        checkout: checkout.ok_or("claim needs --checkout")?,
        rank: rank.ok_or("claim needs --rank")?,
        cores: cores.ok_or("claim needs --cores")?,
        mem_bytes: memory,
        alone,
        flock_fd: flock,
        no_wait,
        since_ms: since,
        wait_ms: wait,
        status_fd,
        command,
    }))
}

fn inherited(fd: RawFd) -> io::Result<(std::fs::Metadata, i32)> {
    // Match spawnfd's inherited-socket check, but keep the ORIGINAL number:
    // status must close before exec and flock must survive at its passed slot.
    // SAFETY: both fcntls inspect a descriptor and touch no memory or ownership.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::FD_CLOEXEC != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "descriptor is close-on-exec, not a caller-transferred inherited original",
        ));
    }
    let metadata = aterm_uds::spawnfd::descriptor_metadata(fd)?;
    // SAFETY: F_GETFL reads flags for this validated descriptor only.
    let access = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if access < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((metadata, access & libc::O_ACCMODE))
}

fn validate_fds(words: &ClaimWords) -> io::Result<()> {
    let (status, mode) = inherited(words.status_fd)?;
    let status_kind = status.file_type();
    if !(status_kind.is_fifo() && mode == libc::O_WRONLY
        || status_kind.is_socket() && mode == libc::O_RDWR)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--status-fd must be an inherited write-only pipe or read/write socket",
        ));
    }
    if let Some(fd) = words.flock_fd {
        let (file, mode) = inherited(fd)?;
        if !file.is_file() || !matches!(mode, libc::O_RDONLY | libc::O_WRONLY | libc::O_RDWR) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--flock-fd must be an inherited regular file",
            ));
        }
    }
    Ok(())
}

fn parse(args: &[OsString]) -> Result<ClaimCommand, String> {
    let Words::Exec(words) = parse_words(args)? else {
        return Ok(ClaimCommand::Capabilities);
    };
    validate_fds(&words).map_err(|error| format!("claim inherited descriptors: {error}"))?;
    // SAFETY: this early exec-only entry owns the two explicitly transferred
    // originals. ALL argument/type/access/inheritance checks completed before
    // either is taken; they are distinct and no FD-allocating operation or
    // callback intervenes. Failure above leaves both caller numbers untouched.
    let status = unsafe { File::from_raw_fd(words.status_fd) };
    // SAFETY: the optional second original was validated together with status
    // and is distinct; this is its sole transfer, with no duplicate owner.
    let flock = words.flock_fd.map(|fd| unsafe { File::from_raw_fd(fd) });
    Ok(ClaimCommand::Exec(PublicClaim {
        what: words.what,
        key: words.key,
        checkout: words.checkout,
        rank: words.rank,
        cores: words.cores,
        mem_bytes: words.mem_bytes,
        alone: words.alone,
        flock,
        no_wait: words.no_wait,
        since_ms: words.since_ms,
        wait_ms: words.wait_ms,
        status,
        command: words.command,
    }))
}

fn help_command(args: &[OsString]) -> bool {
    matches!(args, [word] if matches!(word.to_str(), Some("--help" | "-h")))
}

pub(crate) fn command(args: &[OsString]) -> ExitCode {
    if help_command(args) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match parse(args) {
        Ok(parsed) => crate::build_admit::claim_exec(parsed),
        Err(error) => {
            eprintln!("atpkg claim: {error}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

pub(crate) fn raw_entry_required(_: &[String]) -> ExitCode {
    // The normal raw entry precedes String conversion. Never execute a
    // passthrough reconstructed from the generic CLI's lossy String route.
    eprintln!("atpkg claim: claim must be the directly named verb\n{USAGE}");
    ExitCode::from(2)
}

#[cfg(test)]
#[path = "claim_command_tests.rs"]
mod tests;
