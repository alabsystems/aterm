// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The controller's own SSH connections, made by the root LAN broker on macOS
//! (`docs/DESIGN-lan-broker-2026-11-05.md`): `ssh` is aterm's child, so macOS judges its connect
//! to a Mac on this LAN as aterm's and would ask its Local Network question. Every `ssh` the
//! controller runs (and the scan that reads a Mac's host key) names `ProxyCommand` with
//! `ProxyUseFdpass`: `aterm lan fdpass <host> <port> <seconds>` resolves and connects through
//! `aterm_uds::lan` (the broker for a LAN destination, in-process for any other) and hands the
//! connected socket to `ssh` over its standard output, so `ssh` dials nothing itself.

use super::inventory::Paths;
use crate::port::{Cmd, Ran, System};
use std::ffi::OsString;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The words `aterm lan fdpass` puts before the LAN broker's own when the broker cannot make the
/// connection (not installed, another protocol, refused): what [`super::ssh::classify`] reads as
/// [`super::ssh::Failure::Broker`].
pub const BROKER_SAID: &str = "aterm lan fdpass: ";

/// What `ProxyCommand` runs after aterm's executable.
const VERB: [&str; 2] = ["lan", "fdpass"];

/// The `-o` options that have `ssh` take its connection from `aterm lan fdpass` instead of
/// dialling, each connect bounded by `within` (as `ConnectTimeout` bounds one `ssh` makes).
#[must_use]
pub fn options(within: Duration) -> Vec<OsString> {
    vec![
        "-o".into(),
        format!("ProxyCommand={}", command(&own_executable(), within)).into(),
        "-o".into(),
        "ProxyUseFdpass=yes".into(),
    ]
}

/// This process's executable, the `aterm` the controller runs in; `/dev/null`, which no shell
/// can run, where the system cannot name it, so `ssh` fails rather than dialling by itself.
fn own_executable() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/dev/null"))
}

/// The `ProxyCommand` line: `ssh` takes the rest of its `-o` line as it stands, expands its `%`
/// tokens (`%h` the host, `%p` the port) and runs it with `/bin/sh -c exec …`, so the executable
/// is single-quoted for the shell and its own `%` doubled.
fn command(exe: &Path, within: Duration) -> String {
    let path = exe
        .to_string_lossy()
        .replace('%', "%%")
        .replace('\'', "'\\''");
    format!(
        "'{path}' {} {} %h %p {}",
        VERB[0],
        VERB[1],
        within.as_secs().max(1)
    )
}

/// `aterm lan fdpass <host> <port> <seconds>`'s work: connect to `host` on `port` within
/// `within` through `aterm_uds::lan` and pass the connection over `out` (`ssh`'s end of the
/// socket pair it gave as standard output), as `ProxyUseFdpass` expects: one byte with the
/// descriptor.
///
/// # Errors
/// The line `ssh` would have printed for the same failure (`Could not resolve hostname …`,
/// `connect to host … port …: Connection refused`, …), so [`super::ssh::classify`] reads it as
/// it reads `ssh`'s own, or [`BROKER_SAID`] with the broker's words; or why the connection could
/// not be passed.
pub fn fdpass(
    host: &str,
    port: u16,
    within: Duration,
    out: &aterm_uds::CtlStream,
) -> Result<(), String> {
    use aterm_uds::lan::{ConnectOptions, LanError, Target};
    let deadline = Instant::now() + within;
    let broker = |why: aterm_uds::lan::Unavailable| format!("{BROKER_SAID}{why}");
    let addrs = match host.parse::<IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => match aterm_uds::lan::resolve(host, within) {
            Ok(addrs) if !addrs.is_empty() => addrs,
            Ok(_) => {
                return Err(format!(
                    "ssh: Could not resolve hostname {host}: it resolved to no address"
                ));
            }
            Err(LanError::Unavailable(why)) => return Err(broker(why)),
            Err(LanError::Io(error)) => {
                return Err(format!("ssh: Could not resolve hostname {host}: {error}"));
            }
        },
    };
    let mut last = io::Error::from(io::ErrorKind::TimedOut);
    for ip in addrs {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            last = io::Error::from(io::ErrorKind::TimedOut);
            break;
        }
        match aterm_uds::lan::connect(
            &Target::Addr(SocketAddr::new(ip, port)),
            port,
            remaining,
            ConnectOptions::default(),
        ) {
            Ok((stream, _)) => {
                return aterm_uds::fdpass::send_with_fds(
                    out,
                    b"\0",
                    &[std::os::fd::AsFd::as_fd(&stream)],
                )
                .map(drop)
                .map_err(|error| {
                    format!("{BROKER_SAID}the connection could not be passed to ssh: {error}")
                });
            }
            Err(LanError::Unavailable(why)) => return Err(broker(why)),
            Err(LanError::Io(error)) => last = error,
        }
    }
    Err(format!(
        "ssh: connect to host {host} port {port}: {}",
        strerror(&last)
    ))
}

/// `error` as `ssh` words a failed connect (`strerror(3)`, without Rust's `(os error N)`).
fn strerror(error: &io::Error) -> String {
    if let Some(code) = error.raw_os_error() {
        let words = io::Error::from_raw_os_error(code).to_string();
        return words
            .split_once(" (os error")
            .map_or(words.clone(), |(words, _)| words.to_owned());
    }
    match error.kind() {
        io::ErrorKind::TimedOut => "Operation timed out".to_owned(),
        io::ErrorKind::HostUnreachable => "No route to host".to_owned(),
        io::ErrorKind::ConnectionRefused => "Connection refused".to_owned(),
        _ => error.to_string(),
    }
}

/// `aterm lan fdpass`'s arguments (`<host> <port> <seconds>`) read and run against this
/// process's standard output, its failure line on standard error.
///
/// # Errors
/// The arguments do not parse, or [`fdpass`]'s line.
pub fn serve(host: &str, port: &str, seconds: &str) -> Result<(), String> {
    let port: u16 = port
        .parse()
        .map_err(|_| format!("{BROKER_SAID}{port:?} is not a port"))?;
    let seconds: u64 = seconds
        .parse()
        .ok()
        .filter(|s| (1..=600).contains(s))
        .ok_or_else(|| format!("{BROKER_SAID}{seconds:?} is not 1 to 600 seconds"))?;
    let out = std::os::fd::AsFd::as_fd(&io::stdout())
        .try_clone_to_owned()
        .map_err(|error| format!("{BROKER_SAID}no standard output to pass on: {error}"))?;
    fdpass(
        host,
        port,
        Duration::from_secs(seconds),
        &aterm_uds::CtlStream::from(out),
    )
}

/// How long [`scan_host_key`] gives a Mac to show its host key.
pub const SCAN_WITHIN: Duration = Duration::from_secs(10);

/// The `ssh` that reads `address`'s `ssh-ed25519` host key on `port` within `within`, as
/// `ssh-keyscan` would, but through [`options`]: it accepts whatever key is offered into
/// `known` (`StrictHostKeyChecking=accept-new`, no other file read) and authenticates by no
/// method, so the Mac refuses it right after the key exchange that recorded the key.
#[must_use]
pub fn scan_cmd(address: &str, port: u16, within: Duration, known: &Path) -> Cmd {
    let mut cmd = Cmd::new(super::ssh::SSH)
        .args(["-T", "-F", "/dev/null"])
        .args(options(within));
    for option in [
        "BatchMode=yes".to_owned(),
        "StrictHostKeyChecking=accept-new".to_owned(),
        format!("UserKnownHostsFile={}", super::ssh::config_path(known)),
        "GlobalKnownHostsFile=/dev/null".to_owned(),
        "HashKnownHosts=no".to_owned(),
        "UpdateHostKeys=no".to_owned(),
        "HostKeyAlgorithms=ssh-ed25519".to_owned(),
        "PubkeyAuthentication=no".to_owned(),
        "PasswordAuthentication=no".to_owned(),
        "KbdInteractiveAuthentication=no".to_owned(),
        "GSSAPIAuthentication=no".to_owned(),
        "HostbasedAuthentication=no".to_owned(),
        "ControlMaster=no".to_owned(),
        "ControlPath=none".to_owned(),
        "ClearAllForwardings=yes".to_owned(),
        "LogLevel=ERROR".to_owned(),
    ] {
        cmd = cmd.arg("-o").arg(option);
    }
    cmd.arg("-p")
        .arg(port.to_string())
        .arg("--")
        .arg(format!("hostkey@{address}"))
        .arg("true")
        .budget(within + Duration::from_secs(20))
}

/// Where [`scan`] has `ssh` record a key it reads in `dir`: one file per address, port and
/// process, removed once read.
#[must_use]
pub fn scan_file(dir: &Path, address: &str, port: u16) -> PathBuf {
    let name: String = address
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("hostkey-{name}-{port}-{}", std::process::id()))
}

/// Read the host key `address` offers on `port` within `within` ([`scan_cmd`]): what `ssh`
/// recorded (a known_hosts line, `<host> ssh-ed25519 <base64>`, or nothing) and how it ran.
///
/// # Errors
/// `ssh` could not be run, or `dir` could not hold its file.
pub fn scan(
    sys: &dyn System,
    dir: &Path,
    address: &str,
    port: u16,
    within: Duration,
) -> Result<(String, Ran), String> {
    let known = scan_file(dir, address, port);
    let fresh = |what: &str| match sys.remove(&known) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{what} {}: {error}", known.display())),
    };
    fresh("cannot clear")?;
    let ran = sys
        .run(&scan_cmd(address, port, within, &known))
        .map_err(|e| format!("ssh (host key): {e}"))?;
    let recorded = match sys.read_file(&known) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("{}: {error}", known.display())),
    };
    fresh("cannot remove")?;
    Ok((recorded, ran))
}

/// The folder a scan outside enrollment records into ([`scan`]): `scan` under the controller's
/// folder, made if it is not there.
///
/// # Errors
/// It could not be made.
pub fn scan_dir(sys: &dyn System, paths: &Paths) -> Result<PathBuf, String> {
    let dir = paths.root.join("scan");
    sys.create_dir_all(&dir, 0o700)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// Script `sys` (a test's [`crate::port::Fake`]) to answer a [`scan`] of `address` on `port`
/// in `dir`: `key` recorded (a key blob) or nothing, and `ssh` ending as `ran`.
#[cfg(test)]
pub(crate) fn scanned(
    sys: &crate::port::Fake,
    dir: &Path,
    address: &str,
    port: u16,
    within: Duration,
    key: Option<&str>,
    ran: Ran,
) {
    let known = scan_file(dir, address, port);
    let line = scan_cmd(address, port, within, &known).display();
    if let Some(key) = key {
        let host = if port == 22 {
            address.to_owned()
        } else {
            format!("[{address}]:{port}")
        };
        sys.on_write(
            &line,
            &known,
            format!("{host} ssh-ed25519 {key}\n").as_bytes(),
        );
    }
    sys.on(&line, ran);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ssh` runs the proxy with `/bin/sh -c exec …` after expanding its own `%` tokens: a path
    /// with a blank, a quote or a `%` reaches the shell as one word and unexpanded.
    #[test]
    fn the_proxy_command_quotes_the_executable_for_the_shell_and_ssh() {
        assert_eq!(
            command(
                Path::new("/Applications/aterm (dev).app/Contents/MacOS/aterm"),
                Duration::from_secs(5)
            ),
            "'/Applications/aterm (dev).app/Contents/MacOS/aterm' lan fdpass %h %p 5"
        );
        assert_eq!(
            command(Path::new("/x/it's 100%/aterm"), Duration::from_millis(10)),
            "'/x/it'\\''s 100%%/aterm' lan fdpass %h %p 1"
        );
        let all: Vec<String> = options(Duration::from_secs(15))
            .iter()
            .map(|o| o.to_string_lossy().into_owned())
            .collect();
        assert_eq!(all[0], "-o");
        assert!(all[1].starts_with("ProxyCommand='"), "{all:?}");
        assert!(all[1].ends_with("' lan fdpass %h %p 15"), "{all:?}");
        assert_eq!(all[2..], ["-o", "ProxyUseFdpass=yes"]);
    }

    /// A connect is passed to `ssh` as `ProxyUseFdpass` takes it: one byte and the connected
    /// descriptor, which reaches the listener. A destination nobody listens on fails in the
    /// words `ssh` uses, so the controller's reading of `ssh` holds.
    #[test]
    fn a_connection_is_passed_on_and_a_failure_is_worded_as_ssh_words_it() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (ours, theirs) = aterm_uds::CtlStream::pair().unwrap();
        fdpass("127.0.0.1", port, Duration::from_secs(60), &ours).unwrap();
        let mut byte = [0u8; 1];
        let got = aterm_uds::fdpass::recv_with_fds(&theirs, &mut byte, 1).unwrap();
        assert_eq!((got.bytes, byte), (1, [0]));
        let mut passed = std::net::TcpStream::from(got.fds.into_iter().next().expect("a socket"));
        let (mut accepted, _) = listener.accept().unwrap();
        passed.write_all(b"SSH-2.0-x\r\n").unwrap();
        let mut heard = [0u8; 11];
        accepted.read_exact(&mut heard).unwrap();
        assert_eq!(&heard, b"SSH-2.0-x\r\n");

        // Nobody listens on the port the passed connection holds, and while it holds it no other
        // test can bind it, as a sibling running alongside could take the listener's own port
        // once dropped.
        let unheard = passed.local_addr().unwrap().port();
        let why = fdpass("127.0.0.1", unheard, Duration::from_secs(60), &ours).unwrap_err();
        assert_eq!(
            why,
            format!("ssh: connect to host 127.0.0.1 port {unheard}: Connection refused")
        );
        assert!(why.contains(super::super::ssh::CONNECTION_REFUSED), "{why}");
    }

    #[test]
    fn a_failure_without_an_errno_reads_as_ssh_words_it() {
        assert_eq!(
            strerror(&io::Error::from(io::ErrorKind::TimedOut)),
            "Operation timed out"
        );
        assert_eq!(
            strerror(&io::Error::from_raw_os_error(libc::EHOSTUNREACH)),
            "No route to host"
        );
    }

    /// The arguments `ProxyCommand` passes are checked before anything is dialled.
    #[test]
    fn arguments_that_do_not_parse_are_refused() {
        assert!(
            serve("h", "port", "5")
                .unwrap_err()
                .starts_with(BROKER_SAID)
        );
        assert!(serve("h", "22", "0").unwrap_err().starts_with(BROKER_SAID));
    }
}
