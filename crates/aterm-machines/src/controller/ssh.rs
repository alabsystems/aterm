// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `/usr/bin/ssh` invocation with fleet keys, UUID-pinned host keys, and bounded fan-out.

use super::inventory::{Host, Paths};
use crate::port::{Cmd, Ran, RunFailed, System};
use std::ffi::OsString;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Path to Apple's OpenSSH client.
pub const SSH: &str = "/usr/bin/ssh";

/// Controller report row when Local Network privacy may block LAN access.
pub const LOCAL_NETWORK_ROW: &str = "controller: aterm has no Local Network access — System \
                                     Settings ▸ Privacy & Security ▸ Local Network";

/// OpenSSH stderr substring when a pinned host key does not match.
pub const HOST_KEY_CHANGED: &str = "REMOTE HOST IDENTIFICATION HAS CHANGED!";

/// OpenSSH stderr substring when no host key is pinned for the alias.
pub const HOST_KEY_UNKNOWN: &str = " host key is known for ";

/// OpenSSH final stderr line on host-key verification failure.
pub const HOST_KEY_FAILED: &str = "Host key verification failed.";

/// OpenSSH stderr substring when public-key authentication is denied.
pub const DENIED: &str = ": Permission denied (";

/// OpenSSH stderr substring when the remote port refuses the connection.
pub const CONNECTION_REFUSED: &str = ": Connection refused";

/// OpenSSH stderr substring when the connection attempt times out.
pub const CONNECT_TIMED_OUT: &str = ": Operation timed out";

/// OpenSSH stderr substring when the SSH banner exchange times out.
pub const BANNER_TIMED_OUT: &str = "Connection timed out during banner exchange";

/// OpenSSH stderr substring on `EHOSTUNREACH` (`No route to host`).
pub const NO_ROUTE: &str = ": No route to host";

/// OpenSSH stderr substring on `EHOSTDOWN` (`Host is down`).
pub const HOST_DOWN: &str = ": Host is down";

/// OpenSSH stderr substring when hostname resolution fails.
pub const UNRESOLVED: &str = "Could not resolve hostname";

/// SSH connection target for one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Admin account name.
    pub user: String,
    /// Host address or hostname.
    pub address: String,
    /// Host UUID used as `HostKeyAlias` (`None` before enrollment).
    pub alias: Option<String>,
}

impl Target {
    /// Builds a [`Target`] from an inventory [`Host`].
    #[must_use]
    pub fn of(host: &Host) -> Self {
        Self {
            user: host.user.clone(),
            address: host.address.clone(),
            alias: Some(host.uuid.clone()),
        }
    }

    /// Formats `user@address` for `ssh` after `--`.
    #[must_use]
    pub fn destination(&self) -> String {
        format!("{}@{}", self.user, self.address)
    }

    /// Validates that `user` and `address` are safe for `ssh`.
    ///
    /// # Errors
    /// Returns a human-readable refusal when `user` or `address` is empty or malformed.
    pub fn check(&self) -> Result<(), String> {
        let word_ok = |w: &str, extra: &[u8]| {
            !w.is_empty()
                && !w.starts_with('-')
                && w.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b) || extra.contains(&b))
        };
        if !word_ok(&self.user, b"") {
            return Err(format!("ssh: {:?} is not an account name", self.user));
        }
        if !word_ok(&self.address, b":%") {
            return Err(format!("ssh: {:?} is not a host address", self.address));
        }
        if let Some(alias) = &self.alias
            && !word_ok(alias, b"")
        {
            return Err(format!("ssh: {alias:?} is not a host key alias"));
        }
        Ok(())
    }
}

/// OpenSSH control-master mode for a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mux<'a> {
    /// No control master and no interactive prompts (`BatchMode=yes`).
    Off,
    /// Starts a control master at this socket path for interactive enrollment.
    Master(&'a Path),
    /// Connects through an existing control master at this socket path.
    Through(&'a Path),
}

/// Base OpenSSH `-o` options shared by all fleet connections.
#[must_use]
pub fn base_options(paths: &Paths, target: &Target) -> Vec<OsString> {
    options(paths, target, Mux::Off)
}

/// OpenSSH arguments for `target` under `mux`.
#[must_use]
pub fn options(paths: &Paths, target: &Target, mux: Mux<'_>) -> Vec<OsString> {
    build(paths, target, mux)
}

fn build(paths: &Paths, target: &Target, mux: Mux<'_>) -> Vec<OsString> {
    let mut out: Vec<OsString> = vec!["-F".into(), "/dev/null".into()];
    let mut set = |option: &str| {
        out.push("-o".into());
        out.push(option.into());
    };
    set("IdentitiesOnly=yes");
    if matches!(mux, Mux::Off) {
        set(&format!("IdentityFile={}", config_path(&paths.ssh_key())));
    }
    set(&format!(
        "UserKnownHostsFile={}",
        config_path(&paths.known_hosts())
    ));
    set("GlobalKnownHostsFile=/dev/null");
    if let Some(alias) = &target.alias {
        set(&format!("HostKeyAlias={alias}"));
    }
    set("StrictHostKeyChecking=yes");
    set("UpdateHostKeys=no");
    set(if matches!(mux, Mux::Master(_)) {
        "BatchMode=no"
    } else {
        "BatchMode=yes"
    });
    set("ConnectTimeout=5");
    set("ServerAliveInterval=15");
    set("ServerAliveCountMax=4");
    set("ForwardAgent=no");
    set("ForwardX11=no");
    set("ClearAllForwardings=yes");
    set("LogLevel=ERROR");
    match mux {
        Mux::Off => {
            set("ControlMaster=no");
            set("ControlPath=none");
        }
        Mux::Master(path) => {
            set("ControlMaster=yes");
            set(&format!("ControlPath={}", config_path(path)));
            set("ControlPersist=60");
            set("PubkeyAuthentication=no");
        }
        Mux::Through(path) => {
            set("ControlMaster=no");
            set(&format!("ControlPath={}", config_path(path)));
            set("PubkeyAuthentication=no");
            set("PasswordAuthentication=no");
            set("KbdInteractiveAuthentication=no");
        }
    }
    out
}

/// `path` as an `-o` value.
fn config_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('%', "%%");
    if text
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '#'))
    {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        text
    }
}

/// Builds the [`Cmd`] for a forced-command request on `target`.
///
/// # Errors
/// Returns [`Target::check`]'s refusal if the target is invalid.
pub fn request_cmd(paths: &Paths, target: &Target, command: &str) -> Result<Cmd, RunFailed> {
    target.check().map_err(RunFailed::Refused)?;
    if command.is_empty() || !command.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        return Err(RunFailed::Refused(format!(
            "ssh: {command:?} is not a one-line request"
        )));
    }
    let mut args: Vec<OsString> = vec!["-T".into()];
    args.extend(base_options(paths, target));
    args.push("--".into());
    args.push(target.destination().into());
    args.push(command.into());
    Ok(ssh_cmd(args))
}

/// Runs one forced-command request on `target` with `stdin`.
///
/// # Errors
/// Returns an error if the target is invalid or spawning `ssh` fails.
pub fn request(
    sys: &dyn System,
    paths: &Paths,
    target: &Target,
    command: &str,
    stdin: Option<&Path>,
    budget: Duration,
) -> Result<Ran, RunFailed> {
    let mut cmd = request_cmd(paths, target, command)?.budget(budget);
    if let Some(path) = stdin {
        cmd = cmd.stdin_file(path);
    }
    sys.run(&cmd)
}

/// Classification of an OpenSSH transport failure (exit status 255).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The host's key is not the one pinned for it.
    HostKeyChanged,
    /// No key is pinned for it.
    HostKeyUnknown,
    /// It refused the fleet key.
    Denied,
    /// Nothing listens on its SSH port.
    Refused,
    /// No answer: to the connect, to the SSH banner, or to the address at all (`Host is
    /// down`).
    TimedOut,
    /// No route to it (and, to a local address, maybe Local Network privacy: the controller
    /// says so once when every host fails this way).
    NoRoute,
    /// Its name does not resolve.
    Unresolved,
    /// Anything else: `ssh`'s first line, or that it printed nothing.
    Other(String),
}

impl Failure {
    /// Returns whether the failure means the host could not be reached.
    #[must_use]
    pub const fn unreachable(&self) -> bool {
        matches!(
            self,
            Self::Refused | Self::TimedOut | Self::NoRoute | Self::Unresolved
        )
    }

    /// Formats a single-line summary for a report row.
    #[must_use]
    pub fn why(&self) -> String {
        match self {
            Self::HostKeyChanged => "its host key is not the one pinned at enrollment (a \
                                     reinstalled Mac, or not the same Mac): check it at the \
                                     Mac; if it was erased or reinstalled, `aterm machines \
                                     unenroll <host> --unreachable`, then enroll it again"
                .into(),
            Self::HostKeyUnknown => {
                "no host key is pinned for it: `aterm machines enroll` it first".into()
            }
            Self::Denied => "it refused the fleet key (its authorized_keys line is gone): if \
                             it was erased or reinstalled, `aterm machines unenroll <host> \
                             --unreachable`, then enroll it again"
                .into(),
            Self::Refused => "nothing answers on its SSH port: Remote Login is off".into(),
            Self::TimedOut => "no answer: asleep, off, or not on this network".into(),
            Self::NoRoute => "no route to it".into(),
            Self::Unresolved => "its name does not resolve".into(),
            Self::Other(line) => line.clone(),
        }
    }

    /// Formats [`Failure::why`] for a single-host command, noting Local Network privacy when applicable.
    #[must_use]
    pub fn why_alone(&self, address: &str) -> String {
        match self {
            Self::NoRoute if is_local_address(address) => {
                format!("no route to it; if it is on this network, {LOCAL_NETWORK_ROW}")
            }
            other => other.why(),
        }
    }
}

/// Diagnostic message when `ssh` exits 255 with empty standard error.
pub const EMPTY_255: &str = "exit 255 and nothing said: the connection closed, or the command on the host died by \
     a signal";

/// Classifies an OpenSSH failure when `ran` exited with status 255.
#[must_use]
pub fn classify(ran: &Ran) -> Option<Failure> {
    if ran.status != Some(255) {
        return None;
    }
    let err = ran.err();
    let failure = if err.contains(HOST_KEY_CHANGED) {
        Failure::HostKeyChanged
    } else if err.contains(HOST_KEY_UNKNOWN) && err.contains(HOST_KEY_FAILED) {
        Failure::HostKeyUnknown
    } else if err.contains(DENIED) {
        Failure::Denied
    } else if err.contains(CONNECTION_REFUSED) {
        Failure::Refused
    } else if err.contains(CONNECT_TIMED_OUT)
        || err.contains(BANNER_TIMED_OUT)
        || err.contains(HOST_DOWN)
    {
        Failure::TimedOut
    } else if err.contains(NO_ROUTE) {
        Failure::NoRoute
    } else if err.contains(UNRESOLVED) {
        Failure::Unresolved
    } else {
        Failure::Other(
            err.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map_or_else(|| EMPTY_255.into(), str::to_owned),
        )
    };
    Some(failure)
}

/// Returns whether `address` is governed by macOS Local Network privacy.
#[must_use]
pub fn is_local_address(address: &str) -> bool {
    let name = address.trim_end_matches('.').to_ascii_lowercase();
    if name.ends_with(".local") {
        return true;
    }
    let bare = name.split('%').next().unwrap_or("");
    match bare.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_private() || v4.is_link_local(),
        Ok(IpAddr::V6(v6)) => {
            let first = v6.segments()[0];
            (first & 0xffc0) == 0xfe80
                || (first & 0xfe00) == 0xfc00
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_private() || v4.is_link_local())
        }
        Err(_) => false,
    }
}

/// Runs `work` across `items` with at most `parallel` concurrent workers, preserving input order.
pub fn fan_out<T: Sync, R: Send>(
    items: &[T],
    parallel: usize,
    work: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let workers = parallel.max(1).min(items.len());
    if workers <= 1 {
        return items.iter().map(work).collect();
    }
    let next = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else {
                        break;
                    };
                    let result = work(item);
                    slots
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)[i] = Some(result);
                }
            });
        }
    });
    slots
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .into_iter()
        .map(|r| r.expect("every index was taken by a worker that finished"))
        .collect()
}

/// Builds a [`Cmd`] for `/usr/bin/ssh` with `args`.
#[must_use]
pub fn ssh_cmd(args: Vec<OsString>) -> Cmd {
    Cmd::new(SSH).args(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::Fake;
    use std::path::PathBuf;
    use std::sync::{Condvar, Mutex};
    use std::time::Instant;

    const UUID: &str = "0A1B2C3D-0000-4000-8000-000000000007";

    fn paths() -> Paths {
        Paths {
            root: PathBuf::from("/ctl/machines"),
        }
    }

    fn target() -> Target {
        Target {
            user: "labadmin".into(),
            address: "lab-07.local".into(),
            alias: Some(UUID.into()),
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// The values of every `-o` in `args`.
    fn os(args: &[OsString]) -> Vec<String> {
        let args = strings(args);
        args.windows(2)
            .filter(|w| w[0] == "-o")
            .map(|w| w[1].clone())
            .collect()
    }

    #[test]
    fn the_base_options_build_every_connection_from_nothing() {
        let args = base_options(&paths(), &target());
        let all = strings(&args);
        assert_eq!(&all[..2], ["-F", "/dev/null"]);
        assert_eq!(all.len(), 2 + 2 * os(&args).len(), "{all:?}");
        assert_eq!(
            os(&args),
            [
                "IdentitiesOnly=yes",
                "IdentityFile=/ctl/machines/keys/ssh_ed25519",
                "UserKnownHostsFile=/ctl/machines/known_hosts",
                "GlobalKnownHostsFile=/dev/null",
                "HostKeyAlias=0A1B2C3D-0000-4000-8000-000000000007",
                "StrictHostKeyChecking=yes",
                "UpdateHostKeys=no",
                "BatchMode=yes",
                "ConnectTimeout=5",
                "ServerAliveInterval=15",
                "ServerAliveCountMax=4",
                "ForwardAgent=no",
                "ForwardX11=no",
                "ClearAllForwardings=yes",
                "LogLevel=ERROR",
                "ControlMaster=no",
                "ControlPath=none",
            ]
        );
    }

    #[test]
    fn no_option_is_given_twice_so_none_is_silently_ignored() {
        for mux in [
            Mux::Off,
            Mux::Master(Path::new("/c/ab12cd34")),
            Mux::Through(Path::new("/c/ab12cd34")),
        ] {
            let names: Vec<String> = os(&options(&paths(), &target(), mux))
                .into_iter()
                .map(|o| o.split('=').next().unwrap().to_ascii_lowercase())
                .collect();
            let mut unique = names.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(unique.len(), names.len(), "{mux:?}: {names:?}");
        }
    }

    #[test]
    fn before_enrollment_reads_the_uuid_no_alias_is_named() {
        let mut t = target();
        t.alias = None;
        assert!(
            !os(&base_options(&paths(), &t))
                .iter()
                .any(|o| o.starts_with("HostKeyAlias"))
        );
    }

    #[test]
    fn a_master_lets_a_person_type_the_password_and_a_rider_cannot_authenticate_alone() {
        let master = os(&options(
            &paths(),
            &target(),
            Mux::Master(Path::new("/c/ab12cd34")),
        ));
        for want in [
            "BatchMode=no",
            "ControlMaster=yes",
            "ControlPath=/c/ab12cd34",
            "ControlPersist=60",
            "PubkeyAuthentication=no",
        ] {
            assert!(master.iter().any(|o| o == want), "{want}: {master:?}");
        }
        let rider = options(&paths(), &target(), Mux::Through(Path::new("/c/ab12cd34")));
        let rider_os = os(&rider);
        for want in [
            "BatchMode=yes",
            "ControlMaster=no",
            "ControlPath=/c/ab12cd34",
            "PubkeyAuthentication=no",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
        ] {
            assert!(rider_os.iter().any(|o| o == want), "{want}: {rider_os:?}");
        }
        assert!(!rider_os.iter().any(|o| o.starts_with("IdentityFile")));
    }

    #[test]
    fn a_path_with_a_blank_or_a_percent_reaches_ssh_as_one_literal_path() {
        let odd = Paths {
            root: PathBuf::from("/Users//a b/100% lab/machines"),
        };
        let opts = os(&base_options(&odd, &target()));
        assert!(
            opts.iter()
                .any(|o| o == r#"IdentityFile="/Users//a b/100%% lab/machines/keys/ssh_ed25519""#),
            "{opts:?}"
        );
        assert!(
            opts.iter()
                .any(|o| o == r#"UserKnownHostsFile="/Users//a b/100%% lab/machines/known_hosts""#),
            "{opts:?}"
        );
        assert_eq!(config_path(Path::new("/c/ab12cd34")), "/c/ab12cd34");
        assert_eq!(config_path(Path::new(r#"/a"b"#)), r#""/a\"b""#);
    }

    #[test]
    fn a_request_is_ssh_t_with_the_base_options_then_the_destination_and_command() {
        let fake = Fake::root();
        let mut key = String::from("/usr/bin/ssh -T");
        for a in strings(&base_options(&paths(), &target())) {
            key.push(' ');
            key.push_str(&crate::port::shell_word(&a));
        }
        key.push_str(" -- labadmin@lab-07.local hello");
        fake.on(&key, Ran::ok("{\"schema\":1}\n"));
        let ran = request(
            &fake,
            &paths(),
            &target(),
            "hello",
            None,
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(ran.out(), "{\"schema\":1}\n");
        assert_eq!(fake.calls(), [key]);
    }

    #[test]
    fn a_request_an_ssh_could_misread_never_starts() {
        let fake = Fake::root();
        let mut t = target();
        t.address = "-oProxyCommand=evil".into();
        let refused = |r: Result<Ran, RunFailed>| matches!(r, Err(RunFailed::Refused(_)));
        assert!(refused(request(
            &fake,
            &paths(),
            &t,
            "hello",
            None,
            Duration::from_secs(1)
        )));
        t = target();
        t.user = "a b".into();
        assert!(refused(request(
            &fake,
            &paths(),
            &t,
            "hello",
            None,
            Duration::from_secs(1)
        )));
        assert!(refused(request(
            &fake,
            &paths(),
            &target(),
            "hello\nput x",
            None,
            Duration::from_secs(1)
        )));
        assert!(refused(request(
            &fake,
            &paths(),
            &target(),
            "",
            None,
            Duration::from_secs(1)
        )));
        assert!(fake.calls().is_empty());
    }

    /// `ssh`'s standard error, as measured in the VM (`\r\n` and all).
    fn ssh_said(stderr: &str) -> Ran {
        Ran::exit(255, stderr)
    }

    const MEASURED_CHANGED: &str = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\r\n\
@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\r\n\
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\r\n\
IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\r\n\
Someone could be eavesdropping on you right now (man-in-the-middle attack)!\r\n\
It is also possible that a host key has just been changed.\r\n\
The fingerprint for the ED25519 key sent by the remote host is\n\
SHA256:0Tdy7Goos0XGPRcvSTdM+yrBQzHigTDPivQLBq34e5Q.\r\n\
Please contact your system administrator.\r\n\
Add correct host key in /tmp/ctlm/kh_wrong to get rid of this message.\r\n\
Offending ED25519 key in /tmp/ctlm/kh_wrong:1\r\n\
Host key for 0a1b2c3d-0000-4000-8000-000000000001 has changed and you have requested strict checking.\r\n\
Host key verification failed.\r\n";

    #[test]
    fn each_measured_ssh_failure_is_told_apart() {
        let cases: &[(&str, Failure)] = &[
            (MEASURED_CHANGED, Failure::HostKeyChanged),
            (
                "No ED25519 host key is known for 0a1b2c3d-0000-4000-8000-000000000001 and you have requested strict checking.\r\nHost key verification failed.\r\n",
                Failure::HostKeyUnknown,
            ),
            (
                "admin@127.0.0.1: Permission denied (publickey,password,keyboard-interactive).\r\n",
                Failure::Denied,
            ),
            (
                "ssh: connect to host 127.0.0.1 port 1: Connection refused\r\n",
                Failure::Refused,
            ),
            (
                "ssh: connect to host 192.168.64.254 port 22: Operation timed out\r\n",
                Failure::TimedOut,
            ),
            (
                "Connection timed out during banner exchange\r\n",
                Failure::TimedOut,
            ),
            (
                "ssh: connect to host 192.168.64.254 port 22: Host is down\r\n",
                Failure::TimedOut,
            ),
            (
                "ssh: connect to host 0.0.0.1 port 22: No route to host\r\n",
                Failure::NoRoute,
            ),
            (
                "ssh: Could not resolve hostname no-such-mac-zz.local: nodename nor servname provided, or not known\r\n",
                Failure::Unresolved,
            ),
            (
                "hostname contains invalid characters\r\n",
                Failure::Other("hostname contains invalid characters".into()),
            ),
            ("", Failure::Other(EMPTY_255.into())),
        ];
        for (stderr, want) in cases {
            assert_eq!(classify(&ssh_said(stderr)).as_ref(), Some(want), "{stderr}");
        }
        assert_eq!(classify(&Ran::exit(2, "refused: busy run=7")), None);
        assert_eq!(classify(&Ran::ok("")), None);
    }

    #[test]
    fn only_a_failure_to_reach_the_host_counts_as_unreachable() {
        assert!(Failure::TimedOut.unreachable());
        assert!(Failure::Refused.unreachable());
        assert!(!Failure::HostKeyChanged.unreachable());
        assert!(!Failure::Denied.unreachable());
    }

    #[test]
    fn local_network_privacy_governs_on_link_and_private_addresses_only() {
        for local in [
            "10.0.0.7",
            "192.168.64.2",
            "172.20.1.1",
            "169.254.3.4",
            "lab-07.local",
            "Lab-07.LOCAL.",
            "fe80::1473:84d2:c39d:69b8%en0",
            "fd47:fcfc:1b69:b3db:8ce:a8e2:8815:1434",
        ] {
            assert!(is_local_address(local), "{local}");
        }
        for remote in ["8.8.8.8", "2001:db8::1", "lab.example.com", "localnet"] {
            assert!(!is_local_address(remote), "{remote}");
        }
    }

    #[test]
    fn one_host_with_no_route_on_the_lan_names_local_network_privacy() {
        assert!(
            Failure::NoRoute
                .why_alone("192.168.64.2")
                .ends_with(LOCAL_NETWORK_ROW)
        );
        assert_eq!(
            Failure::NoRoute.why_alone("8.8.8.8"),
            Failure::NoRoute.why()
        );
        assert_eq!(
            Failure::TimedOut.why_alone("192.168.64.2"),
            Failure::TimedOut.why()
        );
    }

    #[test]
    fn fan_out_keeps_input_order_whatever_order_work_finishes_in() {
        let items: Vec<u64> = (0..50).collect();
        let done = Mutex::new(vec![false; items.len()]);
        let changed = Condvar::new();
        let out = fan_out(&items, items.len(), |&i| {
            let since = Instant::now();
            let mut d = done.lock().unwrap();
            while !d[(i as usize + 1)..].iter().all(|x| *x) {
                assert!(since.elapsed() < Duration::from_secs(60), "hang detector");
                d = changed.wait_timeout(d, Duration::from_secs(1)).unwrap().0;
            }
            d[i as usize] = true;
            changed.notify_all();
            i * 10
        });
        assert_eq!(out, items.iter().map(|i| i * 10).collect::<Vec<_>>());
    }

    #[test]
    fn fan_out_never_runs_more_than_parallel_at_once_and_does_reach_it() {
        let items: Vec<usize> = (0..24).collect();
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let gate = Mutex::new(0_usize);
        let opened = Condvar::new();
        let out = fan_out(&items, 4, |&i| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            if i < 4 {
                let since = Instant::now();
                let mut g = gate.lock().unwrap();
                *g += 1;
                opened.notify_all();
                while *g < 4 {
                    assert!(since.elapsed() < Duration::from_secs(60), "hang detector");
                    g = opened.wait_timeout(g, Duration::from_secs(1)).unwrap().0;
                }
            }
            running.fetch_sub(1, Ordering::SeqCst);
            i
        });
        assert_eq!(out, items);
        assert_eq!(peak.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn fan_out_over_nothing_or_with_no_parallelism_still_answers() {
        assert!(fan_out(&[] as &[u8], 8, |x| *x).is_empty());
        assert_eq!(fan_out(&[1, 2, 3], 0, |x| x + 1), [2, 3, 4]);
    }
}
