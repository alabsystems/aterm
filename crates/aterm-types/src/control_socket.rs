// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure lifecycle decisions for the introspection control socket, shared by the server (`aterm-gui`) and the client (`aterm-ctl`).

use std::path::Path;

/// Filename of the `latest` symlink in the socket directory: points at the newest instance's `aterm-<pid>.sock`, so clients with no flags reach it.
pub const LATEST_SOCK_FILE: &str = "aterm.sock";

/// The legacy shared token filename.
pub const SIBLING_TOKEN_FILE: &str = "aterm.token";

/// Prefix that keeps an oddly-named socket's token off the two reserved names,
/// [`SIBLING_TOKEN_FILE`] and `aterm-<pid>.token`: appending `.token` to `aterm` or `aterm-4242`
/// would yield them, so a name not ending in `.sock` is prefixed.
const EXPLICIT_TOKEN_PREFIX: &str = "aterm-sock-";

/// What the host should do about the control socket, decided from its launch flags by [`socket_directive`].
#[derive(Debug, PartialEq, Eq)]
pub enum SocketDirective {
    /// Bind the per-instance default (`aterm-<pid>.sock` in the per-user dir) and maintain the `latest` symlink.
    PerInstance,
    /// Bind exactly this caller-supplied path; no symlink is maintained.
    Explicit(String),
    /// Do not bind a control socket at all.
    Disabled,
}

/// Decide the socket disposition from the `--control-sock` value and the `--no-control-sock` switch (`None` = not given; the switch reads as [`env_flag_engaged`]).
#[must_use]
pub fn socket_directive(
    control_sock: Option<&str>,
    no_control_sock: Option<&str>,
) -> SocketDirective {
    if env_flag_engaged(no_control_sock) {
        return SocketDirective::Disabled;
    }
    match control_sock {
        Some(v) if v == "0" || v.eq_ignore_ascii_case("off") => SocketDirective::Disabled,
        Some("") | None => SocketDirective::PerInstance,
        Some(v) => SocketDirective::Explicit(v.to_string()),
    }
}

/// The one reading of a boolean `ATERM_NO_*` / veto value (`None` = unset): engaged only by a non-empty value other than `"0"`.
#[must_use]
pub fn env_flag_engaged(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// The per-instance socket filename for `pid`: `aterm-<pid>.sock`.
#[must_use]
pub fn instance_sock_name(pid: u32) -> String {
    let mut name = String::from("aterm-");
    name.push_str(&pid.to_string());
    name.push_str(".sock");
    name
}

/// The per-instance token filename for `pid`: `aterm-<pid>.token`.
#[must_use]
pub fn instance_token_name(pid: u32) -> String {
    let mut name = String::from("aterm-");
    name.push_str(&pid.to_string());
    name.push_str(".token");
    name
}

/// Parse the owning pid out of a per-instance filename (`aterm-<pid>.sock` or `aterm-<pid>.token`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn instance_pid(name: &str) -> Option<u32> {
    let stem = name
        .strip_suffix(".sock")
        .or_else(|| name.strip_suffix(".token"))?;
    let pid = stem.strip_prefix("aterm-")?;
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

/// The pid a socket filename encodes, for the `aterm-<pid>.sock` spelling only. [`instance_pid`]
/// also accepts `.token` for the stale sweep; the token rule must not, or a socket path ending
/// `.token` would name itself and the server would write its token over its own socket.
fn sock_name_pid(sock_name: &str) -> Option<u32> {
    if !sock_name.ends_with(".sock") {
        return None;
    }
    instance_pid(sock_name)
}

/// The token filename that authenticates the socket named `sock_name`: the name the server writes beside it and the first a client reads.
#[must_use]
pub fn token_name_for_sock(sock_name: &str) -> String {
    if let Some(pid) = sock_name_pid(sock_name) {
        return instance_token_name(pid);
    }
    let mut name = String::new();
    if !sock_name.ends_with(".sock") {
        name.push_str(EXPLICIT_TOKEN_PREFIX);
    }
    name.push_str(sock_name);
    name.push_str(".token");
    name
}

/// Every token filename a client may read for the socket named `sock_name`, in order.
#[must_use]
pub fn token_names_for_sock(sock_name: &str) -> Vec<String> {
    let mut names = Vec::with_capacity(2);
    names.push(token_name_for_sock(sock_name));
    if sock_name_pid(sock_name).is_none() {
        names.push(SIBLING_TOKEN_FILE.to_string());
    }
    names
}

/// The `sock <abs-path>` line of a discovery graph entry (`<dir>/graph/<sid>`), which the server writes and both its sibling forward and `aterm-ctl`'s self-location read: one parser, so the ends cannot drift.
#[must_use]
pub fn graph_entry_sock(body: &str) -> Option<String> {
    body.lines()
        .find_map(|l| l.strip_prefix("sock "))
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
}

/// One spelling per socket file, however a path arrived at it: the key both the writer and the reader of the discovery graph use.
#[must_use]
pub fn socket_key(path: &str) -> String {
    let stripped = path.strip_prefix(r"\\?\").unwrap_or(path);
    let drive = stripped.len() >= 2
        && stripped.as_bytes()[1] == b':'
        && stripped.as_bytes()[0].is_ascii_alphabetic();
    let text = if drive { stripped } else { path };
    if cfg!(windows) {
        text.to_ascii_lowercase()
    } else {
        text.to_string()
    }
}

/// The `pid <n>` line of a discovery graph entry: the pid of the instance that wrote it, so `aterm-ctl`'s `instances`/`ls` can report a pid for an explicit-`--control-sock` instance whose socket name carries none.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn graph_entry_pid(body: &str) -> Option<u32> {
    let raw = body.lines().find_map(|l| l.strip_prefix("pid "))?.trim();
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}

/// From a socket-directory listing, the per-instance files whose owning pid is dead — stale leftovers of crashed instances, safe to remove.
#[must_use]
pub fn stale_instance_files(names: &[&str], pid_alive: &dyn Fn(u32) -> bool) -> Vec<String> {
    names
        .iter()
        .filter(|n| matches!(instance_pid(n), Some(pid) if !pid_alive(pid)))
        .map(|n| (*n).to_string())
        .collect()
}

/// One entry of a socket-directory listing as the launch sweep sees it: its name, its byte length, and how long ago it was last written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SockFileFacts<'a> {
    /// The bare filename (`aterm-4242.sock`, `private.sock`, `images`, …).
    pub name: &'a str,
    /// Byte length.
    pub len: u64,
    /// Seconds since the file was last written; `0` when the clock cannot say.
    pub age_secs: u64,
}

/// How long a zero-length socket file must sit untouched before the launch sweep probes it: one day, which keeps an instance between its `bind` and first `accept` (milliseconds) out of reach.
pub const STALE_SOCK_MIN_AGE_SECS: u64 = 24 * 60 * 60;

/// From a socket-directory listing, the zero-length `*.sock` files at least `min_age_secs` old that nothing listens on: leftovers of killed instances, safe to remove.
#[must_use]
pub fn stale_socket_files(
    files: &[SockFileFacts<'_>],
    min_age_secs: u64,
    aliased: Option<&str>,
    pid_alive: &dyn Fn(u32) -> bool,
    listening: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    files
        .iter()
        .filter(|f| {
            f.name.ends_with(".sock")
                && f.name != LATEST_SOCK_FILE
                && Some(f.name) != aliased
                && f.len == 0
                && f.age_secs >= min_age_secs
                && match instance_pid(f.name) {
                    Some(pid) if !pid_alive(pid) => true,
                    _ => !listening(f.name),
                }
        })
        .map(|f| f.name.to_string())
        .collect()
}

/// Whether a `latest` symlink target (relative or absolute) designates the instance socket of `pid` — i.e. the link belongs to that instance and may be removed on its exit.
#[must_use]
pub fn symlink_targets_pid(target: &str, pid: u32) -> bool {
    Path::new(target)
        .file_name()
        .is_some_and(|f| f == instance_sock_name(pid).as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two spellings Windows hands the discovery graph for one socket key the same; a UNC
    /// verbatim path and a unix path pass untouched.
    #[test]
    fn socket_key_collapses_the_verbatim_drive_spelling_and_nothing_else() {
        let plain = r"C:\Users\u\AppData\Local\Temp\aterm\aterm-16172.sock";
        let verbatim = r"\\?\C:\Users\u\AppData\Local\Temp\aterm\aterm-16172.sock";
        assert_eq!(socket_key(plain), socket_key(verbatim));
        assert_ne!(
            socket_key(plain),
            socket_key(r"C:\Users\u\AppData\Local\Temp\aterm\aterm-16173.sock")
        );
        assert!(
            socket_key(r"\\?\UNC\server\share\aterm.sock")
                .to_ascii_lowercase()
                .starts_with(r"\\?\unc\")
        );
        let unix = "/var/folders/x/aterm/aterm-42.sock";
        assert_eq!(
            socket_key(unix),
            if cfg!(windows) {
                unix.to_ascii_lowercase()
            } else {
                unix.to_string()
            }
        );
        assert_eq!(
            socket_key(r"C:\A\aterm-1.sock") == socket_key(r"c:\a\aterm-1.sock"),
            cfg!(windows)
        );
    }

    #[test]
    fn socket_directive_reads_the_path_and_the_kill_switch() {
        let explicit = |p: &str| SocketDirective::Explicit(p.to_string());
        for (path, kill, want) in [
            (Some("0"), None, SocketDirective::Disabled),
            (Some("off"), None, SocketDirective::Disabled),
            (Some("OFF"), None, SocketDirective::Disabled),
            (None, Some("1"), SocketDirective::Disabled),
            (None, Some("yes"), SocketDirective::Disabled),
            (Some("/tmp/a.sock"), Some("1"), SocketDirective::Disabled),
            (None, None, SocketDirective::PerInstance),
            (Some(""), None, SocketDirective::PerInstance),
            (None, Some("0"), SocketDirective::PerInstance),
            (None, Some(""), SocketDirective::PerInstance),
            (Some("/tmp/a.sock"), None, explicit("/tmp/a.sock")),
            (Some("/tmp/off"), None, explicit("/tmp/off")),
        ] {
            assert_eq!(socket_directive(path, kill), want, "{path:?} {kill:?}");
        }
    }

    #[test]
    fn instance_names_roundtrip_through_pid_parse() {
        assert_eq!(instance_sock_name(42), "aterm-42.sock");
        assert_eq!(instance_token_name(42), "aterm-42.token");
        assert_eq!(instance_pid("aterm-42.sock"), Some(42));
        assert_eq!(instance_pid("aterm-42.token"), Some(42));
    }

    #[test]
    fn instance_pid_rejects_fixed_and_malformed_names() {
        assert_eq!(instance_pid(LATEST_SOCK_FILE), None);
        assert_eq!(instance_pid(SIBLING_TOKEN_FILE), None);
        assert_eq!(instance_pid("aterm-.sock"), None);
        assert_eq!(instance_pid("aterm-+5.sock"), None);
        assert_eq!(instance_pid("aterm-42.sock.tmp"), None);
        assert_eq!(instance_pid("other-42.sock"), None);
    }

    #[test]
    fn graph_entry_sock_parses_the_shared_on_disk_format() {
        assert_eq!(
            graph_entry_sock("sock /d/aterm-7.sock\nnonce abcd\n").as_deref(),
            Some("/d/aterm-7.sock")
        );
        assert_eq!(graph_entry_sock("nonce abcd\n"), None);
        assert_eq!(graph_entry_sock("sock \n"), None);
        assert_eq!(graph_entry_sock(""), None);
        assert_eq!(
            graph_entry_sock("sock   /a/b.sock  \n").as_deref(),
            Some("/a/b.sock")
        );
    }

    #[test]
    fn graph_entry_pid_parses_hosting_pid_and_fails_closed() {
        assert_eq!(
            graph_entry_pid("sock /tmp/app.sock\nnonce abcd\npid 4242\n"),
            Some(4242)
        );
        assert_eq!(graph_entry_pid("sock /tmp/app.sock\nnonce abcd\n"), None);
        assert_eq!(graph_entry_pid("pid \n"), None);
        assert_eq!(graph_entry_pid("pid 12x\n"), None);
        assert_eq!(graph_entry_pid("pid +7\n"), None);
    }

    #[test]
    fn token_choice_follows_symlink_target() {
        assert_eq!(token_name_for_sock("aterm-7.sock"), "aterm-7.token");
        assert_eq!(token_name_for_sock("aterm-1.sock"), "aterm-1.token");
        assert_eq!(
            token_name_for_sock("aterm-4294967295.sock"),
            "aterm-4294967295.token"
        );
        assert_eq!(token_name_for_sock("aterm-01.sock"), "aterm-1.token");
        assert_eq!(
            token_name_for_sock("aterm-99999999999.sock"),
            "aterm-99999999999.sock.token"
        );
    }

    /// Two explicit sockets in one directory never name one token file.
    #[test]
    fn explicit_sockets_get_their_own_token_each() {
        assert_eq!(token_name_for_sock("a.sock"), "a.sock.token");
        assert_eq!(token_name_for_sock("b.sock"), "b.sock.token");
        assert_ne!(token_name_for_sock("a.sock"), token_name_for_sock("b.sock"));
        assert_eq!(token_name_for_sock("aterm.sock"), "aterm.sock.token");
        assert_ne!(token_name_for_sock("aterm.sock"), SIBLING_TOKEN_FILE);
    }

    /// Odd names stay off the two reserved names: a bare `.token` append would give `aterm` the
    /// legacy shared file and `aterm-4242` pid 4242's.
    #[test]
    fn odd_socket_names_never_land_on_a_reserved_token_name() {
        for (sock, token) in [
            ("ctl", "aterm-sock-ctl.token"),
            ("aterm", "aterm-sock-aterm.token"),
            ("aterm-4242", "aterm-sock-aterm-4242.token"),
            ("aterm-42.token", "aterm-sock-aterm-42.token.token"),
            ("a.b.c.sock", "a.b.c.sock.token"),
            ("\u{65e5}\u{672c}.sock", "\u{65e5}\u{672c}.sock.token"),
            ("aterm-.sock", "aterm-.sock.token"),
            ("aterm-+1.sock", "aterm-+1.sock.token"),
            ("", "aterm-sock-.token"),
        ] {
            assert_eq!(token_name_for_sock(sock), token, "for {sock:?}");
            assert_ne!(token_name_for_sock(sock), SIBLING_TOKEN_FILE);
            assert_eq!(
                instance_pid(&token_name_for_sock(sock)),
                None,
                "{sock:?} must not derive a per-instance token name"
            );
        }
    }

    /// Distinct socket filenames derive distinct token filenames.
    #[test]
    fn the_token_name_rule_is_injective_over_socket_names() {
        let names = [
            "aterm-1.sock",
            "aterm-2.sock",
            "a.sock",
            "b.sock",
            "aterm.sock",
            "aterm",
            "aterm-1",
            "aterm-1.token",
            "ctl",
            "",
        ];
        let mut tokens: Vec<String> = names.iter().map(|n| token_name_for_sock(n)).collect();
        tokens.sort();
        let count = tokens.len();
        tokens.dedup();
        assert_eq!(tokens.len(), count, "two socket names shared a token name");
    }

    /// A client tries the per-socket file first, and the legacy shared name only for an
    /// explicit socket.
    #[test]
    fn read_candidates_put_the_per_socket_token_first() {
        assert_eq!(token_names_for_sock("aterm-7.sock"), vec!["aterm-7.token"]);
        assert_eq!(
            token_names_for_sock("a.sock"),
            vec!["a.sock.token".to_string(), SIBLING_TOKEN_FILE.to_string()]
        );
        assert_eq!(
            token_names_for_sock("ctl"),
            vec![
                "aterm-sock-ctl.token".to_string(),
                SIBLING_TOKEN_FILE.to_string()
            ]
        );
        for n in ["aterm-7.sock", "a.sock", "ctl", ""] {
            assert_eq!(token_names_for_sock(n)[0], token_name_for_sock(n));
        }
    }

    #[test]
    fn stale_sweep_removes_only_dead_instances() {
        let names = [
            "aterm-100.sock",
            "aterm-100.token",
            "aterm-200.sock",
            "aterm-200.token",
            "aterm.sock",
            "aterm.token",
            "images",
        ];
        let alive = |pid: u32| pid == 200;
        let stale = stale_instance_files(&names, &alive);
        assert_eq!(stale, vec!["aterm-100.sock", "aterm-100.token"]);
        assert!(stale_instance_files(&names, &|_| true).is_empty());
    }

    /// A listing entry a day-plus old, as the launch sweep sees it.
    fn aged(name: &str, len: u64) -> SockFileFacts<'_> {
        SockFileFacts {
            name,
            len,
            age_secs: STALE_SOCK_MIN_AGE_SECS + 1,
        }
    }

    /// A probe that records which files it was asked about, so a test can prove a verdict was
    /// reached without dialling (dead pid, young file).
    fn recording_probe(
        answer: bool,
    ) -> (
        std::rc::Rc<std::cell::RefCell<Vec<String>>>,
        impl Fn(&str) -> bool,
    ) {
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = asked.clone();
        (asked, move |name: &str| {
            seen.borrow_mut().push(name.to_string());
            answer
        })
    }

    /// A zero-byte `aterm-<pid>.sock` of a dead pid is stale on its name alone, never dialled.
    #[test]
    fn aged_empty_sock_of_a_dead_pid_is_stale_without_a_probe() {
        let files = [aged("aterm-100.sock", 0), aged("aterm-200.sock", 0)];
        let (asked, probe) = recording_probe(true);
        let stale = stale_socket_files(
            &files,
            STALE_SOCK_MIN_AGE_SECS,
            None,
            &|pid| pid == 200,
            &probe,
        );
        assert_eq!(stale, vec!["aterm-100.sock"]);
        assert_eq!(*asked.borrow(), vec!["aterm-200.sock"]);
    }

    /// A recycled pid reads alive, so only the probe can tell; a file nothing listens on is
    /// stale.
    #[test]
    fn aged_empty_sock_nobody_listens_on_is_stale_even_when_its_pid_is_alive() {
        let files = [aged("aterm-100.sock", 0)];
        let stale =
            stale_socket_files(&files, STALE_SOCK_MIN_AGE_SECS, None, &|_| true, &|_| false);
        assert_eq!(stale, vec!["aterm-100.sock"]);
    }

    /// A socket with a live listener is never swept, whatever its age or name.
    #[test]
    fn a_live_listener_is_never_stale() {
        let files = [aged("aterm-100.sock", 0), aged("private.sock", 0)];
        assert!(
            stale_socket_files(&files, STALE_SOCK_MIN_AGE_SECS, None, &|_| true, &|_| true)
                .is_empty()
        );
        let stale =
            stale_socket_files(&files, STALE_SOCK_MIN_AGE_SECS, None, &|_| false, &|_| true);
        assert_eq!(stale, vec!["aterm-100.sock"]);
    }

    /// A file younger than the margin is left alone unprobed: it may be an
    /// instance between `bind` and `accept`.
    #[test]
    fn young_socks_are_left_alone_and_not_probed() {
        let young = SockFileFacts {
            name: "aterm-100.sock",
            len: 0,
            age_secs: STALE_SOCK_MIN_AGE_SECS - 1,
        };
        let (asked, probe) = recording_probe(false);
        assert!(
            stale_socket_files(&[young], STALE_SOCK_MIN_AGE_SECS, None, &|_| false, &probe)
                .is_empty()
        );
        assert!(asked.borrow().is_empty(), "a young file is not dialled");
        let at_margin = SockFileFacts {
            age_secs: STALE_SOCK_MIN_AGE_SECS,
            ..young
        };
        assert_eq!(
            stale_socket_files(
                &[at_margin],
                STALE_SOCK_MIN_AGE_SECS,
                None,
                &|_| false,
                &probe
            ),
            vec!["aterm-100.sock"]
        );
    }

    /// Only a zero-length `*.sock` is a candidate, and never the `latest` alias (excluded by
    /// name).
    #[test]
    fn only_zero_length_sock_files_other_than_the_latest_alias_are_candidates() {
        let files = [
            aged(LATEST_SOCK_FILE, 16),
            aged(LATEST_SOCK_FILE, 0),
            aged("aterm-100.token", 0),
            aged("aterm-100.sock.tmp", 0),
            aged("images", 0),
            aged("private.sock", 5),
            aged("private.sock", 0),
        ];
        let (asked, probe) = recording_probe(false);
        let stale = stale_socket_files(&files, STALE_SOCK_MIN_AGE_SECS, None, &|_| false, &probe);
        assert_eq!(stale, vec!["private.sock"]);
        assert_eq!(*asked.borrow(), vec!["private.sock"]);
    }

    /// The socket the `latest` alias names is kept, unprobed, until the alias moves on; an
    /// equally dead and old neighbour goes.
    #[test]
    fn the_socket_the_alias_names_is_kept_until_the_alias_moves_on() {
        let files = [aged("aterm-100.sock", 0), aged("aterm-200.sock", 0)];
        let (asked, probe) = recording_probe(false);
        let stale = stale_socket_files(
            &files,
            STALE_SOCK_MIN_AGE_SECS,
            Some("aterm-100.sock"),
            &|_| false,
            &probe,
        );
        assert_eq!(stale, vec!["aterm-200.sock"]);
        assert!(asked.borrow().is_empty(), "neither needed a dial");
        let stale = stale_socket_files(
            &files,
            STALE_SOCK_MIN_AGE_SECS,
            Some("aterm-300.sock"),
            &|_| false,
            &probe,
        );
        assert_eq!(stale, vec!["aterm-100.sock", "aterm-200.sock"]);
    }

    #[test]
    fn symlink_ownership_matches_pid_in_target() {
        assert!(symlink_targets_pid("aterm-42.sock", 42));
        assert!(symlink_targets_pid(
            "/run/user/1000/aterm/aterm-42.sock",
            42
        ));
        assert!(!symlink_targets_pid("aterm-42.sock", 43));
        assert!(!symlink_targets_pid("aterm.sock", 42));
        assert!(!symlink_targets_pid("", 42));
    }
}
