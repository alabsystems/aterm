// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLI front-end for `aterm fleet init|join|peers|allow|leave` over [`aterm_net::fleet_member`].
//!
//! Manages issuer keys, node-key registration (`op=join` using
//! [`aterm_uds::process::lan_host_name`]), peer roster probing, and drive grants in `aterm.toml`.

use std::ffi::OsString;
use std::io::{self, BufRead as _, IsTerminal as _, Read as _, Write as _};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_net::fleet::{
    DriveGrant, FleetOp, FleetScope, FleetSecret, KEY_FILE, NodeIdentity, NodeKey,
};
use aterm_net::fleet_member::{
    self as member, BookRead, BookUnread, CODE_FILE, ISSUER_BOOK_FILE, JoinCode, Keyring,
    NODE_KEY_FILE, PEERS_FILE, Peer, Recorded, Registration, Unreached, Via, target_of,
};
use aterm_uds::atomic_file::ConfigFile;

/// `aterm fleet --help`'s membership half (the dispatcher prints it).
pub(crate) const USAGE: &str = "\
aterm fleet — this machine's membership in a fleet: the aterms of one LAN that
hold one secret, each reading the others at the level they allow

USAGE:
  aterm fleet init [--allow facts|content] [--port N] [--host H] [--rotate]
                         make this machine the fleet's issuer: mint its secret,
                         write the join code to a 0600 file and print that
                         file's path (never the code), set [fleet] listen/allow
  aterm fleet join [--allow facts|content] [--port N] [--host H] [--code-file F]
                         on each other machine: join with that code, read from
                         stdin when piped, from F, or typed at the terminal
                         unshown; never an argument
  aterm fleet peers [--timeout S]
                         the machines this one knows, each checked at once
                         (3 s; --timeout up to 10): on a member, its issuer's
                         book of members too
  aterm fleet peers add <host[:port]> | rm <host[:port]|node>
  aterm fleet allow facts|content
  aterm fleet allow drive --from <host|node> [--from …]
                         what a peer may do here: read facts or content, or
                         (drive, on this Mac alone) drive its sessions from the
                         nodes named, nobody else; facts|content takes it back
  aterm fleet leave      remove this machine's secret, code, book and [fleet] keys

Start a fleet with `init` on one machine and `join` on each other. --allow is
what a peer may read here: facts (programs, agent states, roles, task records
and models; no text a person or a program wrote, the default) or content (the
tab titles, cwds, prompts, an agent's newest words as well). --port is this
machine's fleet port: init's is 7100, join's the issuer's (the port its code
names), and either keeps the port [fleet] listen already names. --host is the
name the others dial this machine by (on a Mac the HostName an administrator
set, else its Bonjour name, <LocalHostName>.local). Machines reach each other
on loopback, private (10/8, 172.16/12, 192.168/16, fc00::/7) or link-local
addresses only.
`allow drive` is the level above content: the nodes it names, each proved by
the node key this machine's book binds, run `aterm ctl --host <this Mac> …`
here (sessions' status, text, turns, holds and workers; `aterm help workers`),
and nobody else does; no join and no remote call turns it on.
`init --rotate` mints the issuer a new secret (every member joins again; the
book keeps each until then). Read the fleet with `aterm ctl survey --fleet`
(or `--host <h>`) on any machine of it: a member asks its issuer's book of
members under the same deadline, so it reads every member, as the issuer
does. An issuer running an older aterm shares no book: the read then says so
and reaches only the peers this machine knows (update aterm on the issuer).
See `aterm help fleet`.
";

/// The deadline on one peer in `peers`, unless `--timeout` names another:
/// the survey's per-peer deadline.
const PEER_CHECK_WITHIN: Duration = aterm_ctl::FLEET_WITHIN;

/// The longest `peers --timeout`: the longest the survey's `--timeout` takes.
const PEER_CHECK_MAX: Duration = Duration::from_secs(10);

/// The deadline on the join's dial and registration: a person is waiting, and
/// a `.local` name can take a moment to resolve.
const JOIN_WITHIN: Duration = Duration::from_secs(10);

/// The most a code file or piped code is read: a code is under 100 bytes.
const CODE_READ_MAX: u64 = 4096;

/// Run one membership verb.
pub(crate) fn main_entry(verb: &str, args: &[OsString]) -> ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Err(Failure::Usage(why)) = no_code_in(&args) {
        eprintln!("aterm fleet {verb}: {why}");
        return ExitCode::from(2);
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        return aterm_uds::stdout::answer_exit(&format!("aterm fleet {verb}"), USAGE, 0);
    }
    let ran = Places::here().and_then(|places| match verb {
        "init" => init(&places, &args),
        "join" => parse_join(&args).and_then(|opts| {
            let (code, from) = read_code(opts.code_file.as_deref())?;
            join(&places, &opts, &code).map(|report| report + from.left_behind())
        }),
        "peers" => peers(&places, &args),
        "allow" => allow(&places, &args),
        "leave" => leave(&places, &args),
        _ => Err(Failure::Usage(format!("no such command `{verb}`"))),
    });
    match ran {
        Ok(report) => aterm_uds::stdout::answer_exit(&format!("aterm fleet {verb}"), &report, 0),
        Err(Failure::Usage(why)) => {
            eprintln!("aterm fleet {verb}: {why}\n(`aterm fleet --help` lists the commands)");
            ExitCode::from(2)
        }
        Err(Failure::Failed(why)) => {
            eprintln!("aterm fleet {verb}: {why}");
            ExitCode::from(1)
        }
    }
}

/// Why a verb did not run (`2`) or did not finish (`1`).
#[derive(Debug)]
enum Failure {
    Usage(String),
    Failed(String),
}

fn failed(why: impl std::fmt::Display) -> Failure {
    Failure::Failed(why.to_string())
}

/// Where this machine keeps its fleet: the state directory and aterm.toml.
struct Places {
    state: PathBuf,
    config: PathBuf,
}

impl Places {
    fn here() -> Result<Self, Failure> {
        let state = aterm_types::dirs::state_dir()
            .ok_or_else(|| failed("no state directory: $HOME is not set"))?;
        let config = aterm_types::dirs::aterm_config_path()
            .ok_or_else(|| failed("no config path: neither $XDG_CONFIG_HOME nor $HOME is set"))?;
        Ok(Self { state, config })
    }
}

/// What `init` and `join` take.
#[derive(Debug, Default)]
struct Opts {
    allow: Option<FleetScope>,
    port: Option<u16>,
    host: Option<String>,
    rotate: bool,
    code_file: Option<PathBuf>,
}

/// Why a join code is never an argument: said whenever one is offered.
const NO_CODE_ARGUMENT: &str = "the join code is never an argument: every process on this \
     machine can read a command line (`ps`) and the shell's history keeps it. Pipe it in \
     (`aterm fleet join < code-file`), name its file (`--code-file F`), or run `aterm fleet \
     join` at a terminal and type it there (it is not shown)";

/// Whether `word` carries a join code's secret: `atf1-` in any case, or the
/// characters a code holds before its `@` — the whole word, or what follows a
/// flag's `=` or precedes an `@` — which is a code pasted without its `atf1-`.
fn carries_code(word: &str) -> bool {
    word.to_ascii_lowercase().contains(member::CODE_PREFIX)
        || word
            .split(['=', '@'])
            .any(|part| JoinCode::parse(&format!("{}{part}@code:1", member::CODE_PREFIX)).is_ok())
}

/// `Err` with the reason a code is never an argument when any of `args` —
/// a flag, a flag's value, or a word of its own — carries one.
fn no_code_in(args: &[String]) -> Result<(), Failure> {
    if args.iter().any(|arg| carries_code(arg)) {
        Err(Failure::Usage(NO_CODE_ARGUMENT.to_owned()))
    } else {
        Ok(())
    }
}

/// Read `args` as the flags `takes` names (`--allow`, `--port`, `--host`,
/// `--rotate`, `--code-file`), each `--flag value` or `--flag=value`.
fn parse_opts(args: &[String], takes: &[&str], code_words: bool) -> Result<Opts, Failure> {
    no_code_in(args)?;
    let mut opts = Opts::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        if !takes.contains(&flag) {
            return Err(Failure::Usage(if flag.starts_with('-') {
                format!("unknown flag `{}`", aterm_codec::tty::one_line(flag, 40))
            } else if code_words {
                NO_CODE_ARGUMENT.to_owned()
            } else {
                format!("unexpected `{}`", aterm_codec::tty::one_line(flag, 40))
            }));
        }
        if flag == "--rotate" {
            opts.rotate = true;
            continue;
        }
        let value = match inline {
            Some(value) => value,
            None => rest
                .next()
                .cloned()
                .ok_or_else(|| Failure::Usage(format!("{flag} takes a value")))?,
        };
        match flag {
            "--allow" => opts.allow = Some(allow_of(&value)?),
            "--port" => {
                opts.port = Some(
                    value
                        .parse::<u16>()
                        .ok()
                        .filter(|port| *port != 0)
                        .ok_or_else(|| Failure::Usage("--port is 1 to 65535".to_owned()))?,
                );
            }
            "--host" => {
                if !member::is_host(&value) {
                    return Err(Failure::Usage(
                        "--host is an address or a name of letters, digits, `.`, `-` and `_`"
                            .to_owned(),
                    ));
                }
                opts.host = Some(value);
            }
            _ => opts.code_file = Some(PathBuf::from(value)),
        }
    }
    Ok(opts)
}

fn allow_of(word: &str) -> Result<FleetScope, Failure> {
    FleetScope::from_word(&word.to_ascii_lowercase()).ok_or_else(|| {
        Failure::Usage(
            "--allow is `facts` or `content`: driving this Mac is granted on it alone, to the \
             nodes it names (`aterm fleet allow drive --from <host|node>`), never by a join"
                .to_owned(),
        )
    })
}

/// `join`'s flags: every other argument — a code above all — is refused.
fn parse_join(args: &[String]) -> Result<Opts, Failure> {
    parse_opts(args, &["--allow", "--port", "--host", "--code-file"], true)
}

/// Where [`read_code`] read the join code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodeFrom {
    /// `--code-file`.
    File,
    /// Stdin, not a terminal: `aterm fleet join < code-file`, mostly.
    Piped,
    /// Typed at the terminal, unshown.
    Typed,
}

impl CodeFrom {
    /// What a join that read its code here says last: a code that came from
    /// a file — named, or piped in — left a copy of the fleet's secret
    /// wherever it was dropped (Downloads, a USB stick, a synced folder),
    /// which `init` told the person to make.
    const fn left_behind(self) -> &'static str {
        match self {
            Self::File | Self::Piped => {
                "The join code is the fleet's secret: now that this machine has joined, delete \
                 the file it came from and every copy of it (Downloads, a USB stick, a synced \
                 folder).\n"
            }
            Self::Typed => "",
        }
    }
}

/// The join code as text: the first line of `--code-file`, else of stdin when
/// it is not a terminal, else typed at the terminal with echo off; and where
/// it came from.
fn read_code(code_file: Option<&Path>) -> Result<(String, CodeFrom), Failure> {
    let mut text = String::new();
    let from = if let Some(path) = code_file {
        std::fs::File::open(path)
            .and_then(|file| file.take(CODE_READ_MAX).read_to_string(&mut text))
            .map_err(|e| failed(format!("the --code-file file: {e}")))?;
        CodeFrom::File
    } else if !io::stdin().is_terminal() {
        io::stdin()
            .lock()
            .take(CODE_READ_MAX)
            .read_to_string(&mut text)
            .map_err(|e| failed(format!("reading the code on stdin: {e}")))?;
        CodeFrom::Piped
    } else {
        text = read_unshown("Join code (not shown): ")
            .map_err(|e| failed(format!("reading the code at the terminal: {e}")))?;
        CodeFrom::Typed
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|code| (code.to_owned(), from))
        .ok_or_else(|| failed("no join code came: pipe it in, or name its file with --code-file"))
}

/// The terminal and the settings it had before [`read_unshown`] turned its
/// echo off: what a Ctrl-C at that prompt puts back ([`restore_and_end`]).
static UNSHOWN_TTY: std::sync::OnceLock<(i32, libc::termios)> = std::sync::OnceLock::new();

/// SIGINT while the code is typed unshown.
extern "C" fn restore_and_end(signal: libc::c_int) {
    if let Some((fd, saved)) = UNSHOWN_TTY.get() {
        // SAFETY: `fd` is the /dev/tty `read_unshown` holds open while this
        // handler is installed, and `saved` a termios `tcgetattr` filled.
        unsafe {
            libc::tcsetattr(*fd, libc::TCSAFLUSH, saved);
        }
    }
    // SAFETY: `sigaction` is plain data for which all-zero is a valid value (an empty mask, no
    // flags).
    let mut default: libc::sigaction = unsafe { std::mem::zeroed() };
    default.sa_sigaction = libc::SIG_DFL;
    // SAFETY: The default disposition, for the signal this handler took.
    unsafe {
        libc::sigaction(signal, &default, std::ptr::null_mut());
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "SIGINT, no core signal: the person's Ctrl-C ends the process as it would have"
    )]
    // SAFETY: `raise` is async-signal-safe; the disposition is the default.
    unsafe {
        libc::raise(signal);
    }
}

/// One line typed at `/dev/tty` with echo off (the newline still echoed), the
/// terminal's own settings put back however this returns — and on a Ctrl-C,
/// which ends the process with no unwind, by a SIGINT handler installed for
/// the read ([`restore_and_end`]).
fn read_unshown(prompt: &str) -> io::Result<String> {
    use std::os::fd::AsRawFd as _;
    /// Puts the terminal's settings and SIGINT's disposition back when
    /// dropped.
    struct Restore {
        fd: i32,
        saved: libc::termios,
        interrupt: Option<libc::sigaction>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: `fd` is the open /dev/tty this function holds for the
            // guard's whole life, and `saved` a termios `tcgetattr` filled.
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSAFLUSH, &self.saved);
            }
            if let Some(was) = &self.interrupt {
                // SAFETY: Puts back the disposition `read_unshown` replaced.
                unsafe {
                    libc::sigaction(libc::SIGINT, was, std::ptr::null_mut());
                }
            }
        }
    }
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    let fd = tty.as_raw_fd();
    // SAFETY: an all-zero termios is a valid value of the plain C struct, and
    // `tcgetattr` overwrites it before it is read.
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is open, and `saved` is a writable termios.
    if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let interrupt = UNSHOWN_TTY
        .set((fd, saved))
        .is_ok()
        .then(|| {
            // SAFETY: `sigaction` is plain data for which all-zero is a valid
            // value (an empty mask, no flags).
            let mut restoring: libc::sigaction = unsafe { std::mem::zeroed() };
            restoring.sa_sigaction =
                restore_and_end as extern "C" fn(libc::c_int) as libc::sighandler_t;
            // SAFETY: As above.
            let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: A valid disposition for SIGINT; `was` receives the one it replaces.
            (unsafe { libc::sigaction(libc::SIGINT, &restoring, &mut was) } == 0).then_some(was)
        })
        .flatten();
    let mut quiet = saved;
    quiet.c_lflag &= !libc::ECHO;
    quiet.c_lflag |= libc::ECHONL;
    let restore = Restore {
        fd,
        saved,
        interrupt,
    };
    // SAFETY: `fd` is open, and `quiet` a termios `tcgetattr` filled, edited.
    if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &quiet) } != 0 {
        return Err(io::Error::last_os_error());
    }
    tty.write_all(prompt.as_bytes())?;
    tty.flush()?;
    let mut line = String::new();
    io::BufReader::new(&tty)
        .take(CODE_READ_MAX)
        .read_line(&mut line)?;
    drop(restore);
    Ok(line)
}

/// A node id's first eight digits, as rows and reports show it.
fn short(node: &str) -> &str {
    node.get(..8).unwrap_or(node)
}

/// The `listen` address for a fleet port: every interface, or the loopback
/// alone when the issuer is a loopback address (a fleet of instances on one
/// machine, as `crates/aterm/tests/fleet_live_headless.rs` runs).
fn listen_on(issuer_host: &str, port: u16) -> String {
    match issuer_host.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => member::host_port(&ip.to_string(), port),
        _ => format!("0.0.0.0:{port}"),
    }
}

/// The state directory made private, and a line saying so when it was not.
fn private_state(places: &Places, out: &mut String) -> Result<(), Failure> {
    if let Some(was) = member::ensure_private_dir(&places.state).map_err(failed)? {
        out.push_str(&format!(
            "  state      {} was open to other users ({was:o}); it is 0700 now\n",
            places.state.display()
        ));
    }
    Ok(())
}

/// `aterm fleet init`: this machine becomes the fleet's issuer.
fn init(places: &Places, args: &[String]) -> Result<String, Failure> {
    let opts = parse_opts(args, &["--allow", "--port", "--host", "--rotate"], false)?;
    let mut out = String::new();
    let config = ConfigFile::open(&places.config).map_err(failed)?;
    let table = fleet_table(text_of(&config)?).map_err(failed)?;
    let has_key = std::fs::symlink_metadata(places.state.join(KEY_FILE)).is_ok();
    let code_path = places.state.join(CODE_FILE);
    let issues = std::fs::symlink_metadata(&code_path).is_ok();
    if has_key && !issues {
        return Err(failed(
            "this machine is a member of a fleet it joined; `aterm fleet leave` first, then \
             `init` makes it the issuer of a new one",
        ));
    }
    if opts.rotate && !issues {
        return Err(Failure::Usage(
            "`--rotate` mints a new secret for the fleet this machine issues, and it issues \
             none: `aterm fleet init` without it makes this machine the issuer of one"
                .to_owned(),
        ));
    }
    if has_key && !opts.rotate {
        if code_path.exists()
            && (opts.port.is_some() || opts.host.is_some() || opts.allow.is_some())
        {
            return Err(Failure::Usage(format!(
                "this machine is already the fleet's issuer, and changes nothing without \
                 `--rotate`: --host and --port are in its join code, so a new one takes \
                 `--rotate` (every member joins again); --allow alone is [fleet] allow in {}",
                places.config.display()
            )));
        }
        let keyed = matches!(member::read_node_identity(&places.state), Ok(Some(_)));
        let me = member::node_identity(&places.state).map_err(failed)?;
        return Ok(format!(
            "aterm fleet init: this machine is already the fleet's issuer; its join code is in \
             {}\n{}  `--rotate` mints a new secret (every member joins again); `aterm fleet \
             leave` leaves the fleet\n",
            code_path.display(),
            if keyed {
                String::new()
            } else {
                format!(
                    "  node key   {} (0600) minted: each member binds it when it next joins; one \
                     that bound an older key runs `aterm fleet peers rm {}` there first\n",
                    places.state.join(NODE_KEY_FILE).display(),
                    short(me.node())
                )
            }
        ));
    }
    let issued = if opts.rotate && (opts.host.is_none() || opts.port.is_none()) {
        Some(issued_code(places, &code_path)?)
    } else {
        None
    };
    private_state(places, &mut out)?;
    let host = match (opts.host, &issued) {
        (Some(host), _) => host,
        (None, Some(issued)) => issued.host.clone(),
        (None, None) => aterm_uds::process::lan_host_name()
            .filter(|host| member::is_host(host))
            .ok_or_else(|| {
                failed(
                    "this machine's host name is no host a member can dial; name one with --host",
                )
            })?,
    };
    let port = opts
        .port
        .or_else(|| issued.as_ref().map(|issued| issued.port))
        .or_else(|| table.listen_port())
        .unwrap_or(member::DEFAULT_PORT);
    let allow = opts
        .allow
        .map(FleetScope::word)
        .or_else(|| table.level())
        .unwrap_or(FleetScope::Facts.word());
    let secret = member::mint_secret().map_err(failed)?;
    let code = JoinCode { secret, host, port };
    member::write_private(
        &places.state,
        CODE_FILE,
        format!("{}\n", code.text()).as_bytes(),
    )
    .map_err(|e| failed(format!("{CODE_FILE}: {e}")))?;
    member::write_key_file(&places.state, &secret)
        .map_err(|e| failed(format!("{KEY_FILE}: {e}")))?;
    let node = member::node_identity(&places.state)
        .map_err(failed)?
        .node()
        .to_owned();
    let old_secret: Vec<Peer> = if opts.rotate {
        member::read_peers(&places.state)
            .map_err(failed)?
            .into_iter()
            .filter(|peer| peer.via == Via::Joined)
            .collect()
    } else {
        Vec::new()
    };
    let listen = listen_on(&code.host, port);
    let config_line = write_fleet(&config, &places.config, (&listen, allow), false)?;
    out.insert_str(
        0,
        &format!(
            "aterm fleet init: this machine ({}, node {}) is the fleet's issuer{}\n",
            aterm_codec::tty::one_line(&code.host, 80),
            short(&node),
            if opts.rotate {
                " under a new secret: every member must join again"
            } else {
                ""
            }
        ),
    );
    out.push_str(&format!(
        "  key        {} (0600)\n  join code  {} (0600) — the fleet's secret: never printed, \
         as aterm exports screens\n  node key   {} (0600) — this machine's own: it proves \
         which node it is\n{config_line}",
        places.state.join(KEY_FILE).display(),
        code_path.display(),
        places.state.join(NODE_KEY_FILE).display(),
    ));
    if !old_secret.is_empty() {
        out.push_str(&format!(
            "  members    {} in the book hold the old secret until each joins again with the \
             new code (`aterm fleet peers` reads each as holding another secret until then); \
             `aterm fleet peers rm` the machine that was lost:\n",
            old_secret.len()
        ));
        for peer in &old_secret {
            out.push_str(&format!(
                "             {}  {}  {}\n",
                peer.node.as_deref().map_or("-", short),
                aterm_codec::tty::one_line(&peer.host, 40),
                peer.target()
            ));
        }
    }
    out.push_str(&format!(
        "On each other machine, with that file copied there (it is the secret: AirDrop, scp, \
         a USB stick — not a chat; delete each copy once that machine has joined):\n  aterm \
         fleet join --code-file <file>    (or: aterm fleet join < <file>)\n  add --allow \
         content to let this fleet read that machine's tab titles, cwds, prompts and an \
         agent's words: facts, the default, is programs, states and roles alone\n{FOLLOWS} \
         Members dial {}.\n",
        member::host_port(&code.host, port)
    ));
    Ok(out)
}

/// What `init` and `join` say of the aterm that serves `[fleet]`: one may or
/// may not run here as they write it.
const FOLLOWS: &str = "An aterm running here follows [fleet] without a relaunch (`aterm ctl \
                       status` reads fleet=listening); one started later reads it at launch.";

/// The join code this machine issued, at `path`, for a rotation that keeps
/// its host and port: refused, failing closed, when it cannot be read.
fn issued_code(places: &Places, path: &Path) -> Result<JoinCode, Failure> {
    member::read_join_code(&places.state).map_err(|why| {
        failed(format!(
            "the join code in {} cannot be read ({why}), and it names the host and port \
                 members dial: name both with --host and --port to rotate",
            path.display()
        ))
    })
}

/// `aterm fleet join` with the code `code_text`.
fn join(places: &Places, opts: &Opts, code_text: &str) -> Result<String, Failure> {
    let code = JoinCode::parse(code_text).map_err(failed)?;
    let secret = FleetSecret::new(&code.secret).map_err(failed)?;
    not_an_issuer(places, &code)?;
    let host = match opts.host.clone() {
        Some(host) => host,
        None => aterm_uds::process::lan_host_name()
            .filter(|host| member::is_host(host))
            .ok_or_else(|| {
                failed(
                    "this machine's host name is no host the issuer can dial it by; name one \
                     with --host",
                )
            })?,
    };
    let regrant = aterm_net::fleet::read_key_file(&places.state).ok().as_ref() != Some(&secret);
    let (port, allow, grants_gone) = {
        let config = ConfigFile::open(&places.config).map_err(failed)?;
        let text = text_of(&config)?;
        let table = fleet_table(text).map_err(failed)?;
        let port = opts
            .port
            .or_else(|| table.listen_port())
            .unwrap_or(code.port);
        let allow = match opts
            .allow
            .map(FleetScope::word)
            .or_else(|| table.level())
            .unwrap_or(FleetScope::Facts.word())
        {
            aterm_net::fleet::DRIVE_LEVEL if regrant => FleetScope::Content.word(),
            allow => allow,
        };
        with_fleet(text, &listen_on(&code.host, port), allow, regrant).map_err(failed)?;
        (port, allow, regrant && !table.drive_from.is_empty())
    };
    let mut out = String::new();
    private_state(places, &mut out)?;
    let me = member::node_identity(&places.state).map_err(failed)?;
    let issuer_at = member::host_port(&code.host, code.port);
    let registration = Registration {
        node: me.node().to_owned(),
        host,
        port,
        version: aterm_types::version::app_version().to_owned(),
        key: Some(me.key()),
    };
    let keys = Keyring::read(&places.state);
    let bound = (!regrant).then(|| bound_issuer(places)).flatten();
    let (joined, at, issuer_key) = match register_with(
        &code,
        (&secret, &me),
        (&keys, bound.as_ref()),
        &registration,
    ) {
        Ok(registered) => registered,
        Err(why) => {
            aterm_uds::stdout::print(format_args!("{out}"));
            return Err(failed(not_recorded(&issuer_at, &why)));
        }
    };
    let held = aterm_net::fleet::read_key_file(&places.state).ok();
    let same_fleet = held.as_ref() == Some(&secret);
    let replaced = std::fs::symlink_metadata(places.state.join(KEY_FILE)).is_ok() && !same_fleet;
    let taken_out: Vec<Peer> = if same_fleet {
        Vec::new()
    } else {
        member::update_peers(&places.state, |book| {
            let (kept, gone) = std::mem::take(book)
                .into_iter()
                .partition(|peer| peer.via == Via::Manual);
            *book = kept;
            Ok(gone)
        })
        .map_err(|why| {
            unfinished(
                &out,
                &issuer_at,
                why,
                "the fleet's key was not written, and the book is as it was",
            )
        })?
    };
    if let Err(e) = member::write_key_file(&places.state, &code.secret) {
        let put_back = member::update_peers(&places.state, |book| {
            let missing: Vec<Peer> = taken_out
                .iter()
                .filter(|peer| !book.contains(peer))
                .cloned()
                .collect();
            book.extend(missing);
            Ok(())
        });
        let holds = match put_back {
            Ok(()) => "the fleet's key was not written, and the book is as it was".to_owned(),
            Err(why) => format!(
                "the fleet's key was not written, and the book no longer names the issuer or \
                 the members of the fleet this machine held, which could not be put back \
                 ({why})"
            ),
        };
        return Err(unfinished(
            &out,
            &issuer_at,
            format!("{KEY_FILE}: {e}"),
            &holds,
        ));
    }
    let issuer = Peer {
        node: Some(joined.issuer.node.clone()),
        key: issuer_key,
        key_from: None,
        host: joined.issuer.host.clone(),
        addr: Some(at),
        port: code.port,
        version: Some(joined.issuer.version.clone()),
        seen_ms: aterm_time::unix_millis(),
        via: Via::Issuer,
    };
    member::update_peers(&places.state, |book| {
        book.retain(|peer| peer.via != Via::Issuer);
        member::record(book, issuer)
    })
    .map_err(|why| {
        unfinished(
            &out,
            &issuer_at,
            why,
            "the fleet's key is held here, but the book does not name the issuer and aterm.toml \
             has no [fleet] listen",
        )
    })?;
    let config_line = ConfigFile::open(&places.config)
        .map_err(failed)
        .and_then(|config| {
            write_fleet(
                &config,
                &places.config,
                (&listen_on(&code.host, port), allow),
                regrant,
            )
        })
        .map_err(|failure| {
            let (Failure::Usage(why) | Failure::Failed(why)) = failure;
            unfinished(
                &out,
                &issuer_at,
                why,
                "the fleet's key is held here and the book names the issuer, but aterm.toml has \
                 no [fleet] listen, so no aterm here listens for the fleet",
            )
        })?;
    out.insert_str(
        0,
        &format!(
            "aterm fleet join: this machine (node {}) holds the fleet's key{}\n  key        {} \
             (0600)\n  node key   {} (0600) — this machine's own, which the issuer bound its \
             node to\n{config_line}",
            short(&registration.node),
            if replaced {
                ", in place of the one it held"
            } else {
                ""
            },
            places.state.join(KEY_FILE).display(),
            places.state.join(NODE_KEY_FILE).display(),
        ),
    );
    if allow == FleetScope::Facts.word() {
        out.push_str(
            "  allow      facts: the fleet reads programs, states and roles here, no tab title, \
             cwd, prompt or agent's words; [fleet] allow = \"content\" in aterm.toml lets it\n",
        );
    }
    if grants_gone {
        out.push_str(
            "  drive      no Mac drives this one: its drive grants named the nodes of the fleet \
             it held before, so none is kept (`aterm fleet allow drive --from <host|node>` \
             grants again)\n",
        );
    }
    out.push_str(&format!(
        "  issuer     {} at {issuer_at} recorded this machine ({} peers in its book){}\n\
         {FOLLOWS}\nOn a Mac this was aterm's first connection into the LAN: the \
         local-network consent macOS asked for (if it did) covers the survey's reads \
         too.\n",
        aterm_codec::tty::one_line(&joined.issuer.host, 80),
        joined.peers,
        if issuer_key.is_some() {
            ", and proved its node key, which this machine binds it to"
        } else {
            "; it proved no node key, so its node is its claim here: join again here once it \
             has one (`aterm fleet init` there mints it, after an update if its aterm is \
             older; a join before then binds nothing)"
        },
    ));
    Ok(out)
}

/// Why a join's registration did not happen ([`register_with`]).
#[derive(Debug)]
enum JoinFailed {
    /// The issuer was not reached and proved.
    Dial(Unreached),
    /// It proved this node, which this machine binds to another key.
    OtherKey(String),
    /// It answered as this node, which this machine binds to a key, and proved none.
    Unproved(String),
    /// A join again of the fleet this machine holds, whose book binds its issuer `bound` to a key.
    OtherIssuer {
        /// The issuer this machine's book binds.
        bound: String,
        /// The node the machine that answered proved, when it proved one.
        proved: Option<String>,
    },
    /// It was; the exchange failed, or the issuer refused (its words).
    Registration(String),
}

impl std::fmt::Display for JoinFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dial(e) => write!(f, "{e}"),
            Self::OtherKey(node) => write!(
                f,
                "it proved node {} under another key than this machine binds it to",
                short(node)
            ),
            Self::Unproved(node) => write!(
                f,
                "it answered as node {}, which this machine binds to a node key, and proved none",
                short(node)
            ),
            Self::OtherIssuer {
                bound,
                proved: Some(node),
            } => write!(
                f,
                "another node answered for the issuer this Mac binds (node {}): it proved node {}",
                short(bound),
                short(node)
            ),
            Self::OtherIssuer {
                bound,
                proved: None,
            } => write!(
                f,
                "the machine that answered for the issuer this Mac binds (node {}) proved no node",
                short(bound)
            ),
            Self::Registration(why) => f.write_str(why),
        }
    }
}

/// What a join that failed should do next ([`JoinFailed::advice`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Advice {
    /// The code names a machine that issues no fleet now.
    NoIssuer,
    /// The issuer's address is past the LAN, so it was never dialled.
    PastTheLan,
    /// This process may not connect at all (a sandbox, a firewall).
    Sandbox,
    /// No route to the issuer's host.
    LocalNetwork,
    /// No route to the issuer's network.
    NoNetwork,
    /// The machine that answered claims a node this machine binds to a key.
    IssuerKey,
    /// A join again of the fleet this machine holds reached another node than the issuer it binds.
    BoundIssuer,
    /// A join again of the fleet this machine holds reached a machine that proved no node.
    BoundUnproved,
    /// The issuer binds this machine's node id to a node key it did not prove.
    NodeBound,
    /// The issuer recorded this machine's node from another address before keys came.
    BeforeKeys,
    /// Anything else: the host and port the code names, and an aterm there.
    CheckTheIssuer,
}

impl JoinFailed {
    /// The advice this failure takes, by its kind: the first that fits, in
    /// this order.
    fn advice(&self) -> Advice {
        match self {
            Self::Registration(words) if words.contains(NO_ISSUER) => Advice::NoIssuer,
            Self::Registration(words) if words.contains(member::BEFORE_KEYS) => Advice::BeforeKeys,
            Self::Registration(words) if words.contains(member::REBIND) => Advice::NodeBound,
            Self::OtherKey(_) | Self::Unproved(_) => Advice::IssuerKey,
            Self::OtherIssuer { proved: None, .. } => Advice::BoundUnproved,
            Self::OtherIssuer { .. } => Advice::BoundIssuer,
            Self::Dial(Unreached::NotFleet(_)) => Advice::PastTheLan,
            Self::Dial(e) if e.not_permitted() => Advice::Sandbox,
            Self::Dial(e) if e.no_route() => Advice::LocalNetwork,
            Self::Dial(e) if e.no_network() => Advice::NoNetwork,
            _ => Advice::CheckTheIssuer,
        }
    }
}

/// What `join` says when the issuer at `issuer_at` did not record this
/// machine, `why`'s [`Advice`] deciding what to do next: nothing of the
/// fleet was written here in every case.
fn not_recorded(issuer_at: &str, why: &JoinFailed) -> String {
    const NOTHING: &str = "Nothing of the fleet was written here.";
    match why.advice() {
        Advice::NoIssuer => format!(
            "{issuer_at} did not record this machine: {why}\n  {NOTHING} The code names a \
             machine that issues no fleet now — a member, or one that left — so no aterm there \
             will record this one. Copy the code from the machine whose `aterm fleet init` \
             wrote it (its `aterm fleet peers` lists the members), and join again."
        ),
        Advice::PastTheLan => format!(
            "the issuer at {issuer_at} was not dialled: {why}\n  {NOTHING} A fleet's machines \
             reach each other on loopback, private (10/8, 172.16/12, 192.168/16, fc00::/7) or \
             link-local addresses alone: on the issuer, `aterm fleet init --rotate --host \
             <its LAN address>` writes a code that names one."
        ),
        Advice::Sandbox => format!(
            "the issuer at {issuer_at} was not reached: {why}\n  {NOTHING} Run `aterm fleet join` \
             where it may open network connections (outside the sandbox), and join again."
        ),
        Advice::LocalNetwork => format!(
            "the issuer at {issuer_at} was not reached: {why}\n  {NOTHING} On a Mac that is \
             local-network privacy, which refuses an app's connections into the LAN until they \
             are allowed (System Settings › Privacy & Security › Local Network: aterm, or the \
             terminal this runs in); allow it, then join again."
        ),
        Advice::NoNetwork => format!(
            "the issuer at {issuer_at} was not reached: {why}\n  {NOTHING} This machine is not \
             on the issuer's network: join again once it is on the LAN (Wi-Fi or Ethernet), with \
             any full-tunnel VPN off."
        ),
        Advice::IssuerKey => format!(
            "the issuer at {issuer_at} is not the one this machine binds: {why}\n  {NOTHING} \
             Another machine answers at that host, or the issuer lost its node key (a \
             reinstall) or runs an older aterm (update it). If it is the issuer, `aterm fleet \
             peers rm <its node>` here takes the old binding out; then join again."
        ),
        Advice::BoundIssuer => format!(
            "the issuer at {issuer_at} is not the one this machine binds: {why}\n  {NOTHING} \
             This machine joined this fleet before, and a join again is answered by the issuer \
             it joined, under the node key it bound then. To move this machine to a new issuer \
             on purpose, run `aterm fleet leave` here, then `aterm fleet join` with the code."
        ),
        Advice::BoundUnproved => format!(
            "the issuer at {issuer_at} is not the one this machine binds: {why}\n  {NOTHING} \
             This machine joined this fleet before, and a join again is answered by the issuer \
             it joined, under the node key it bound then. If it is that issuer, it runs an \
             older aterm (update it there) or lost its node key (a reinstall: `aterm fleet \
             init` there mints one, and `aterm fleet peers rm <its node>` here then takes the \
             old binding out); join again after. To move this machine to a new issuer on \
             purpose, run `aterm fleet leave` here, then `aterm fleet join` with the code."
        ),
        Advice::NodeBound => format!(
            "the issuer at {issuer_at} did not record this machine: {why}\n  {NOTHING} The \
             issuer holds this machine's node id to a key this machine did not prove: a machine \
             that lost its node key, or another one using its node id. If this is that machine, \
             a person at the issuer takes its entry out as it says."
        ),
        Advice::BeforeKeys => format!(
            "the issuer at {issuer_at} did not record this machine: {why}\n  {NOTHING} The \
             issuer recorded this machine's node from another address before node keys came, \
             and binds its key only from there. If this machine moved since (a new address), \
             a person at the issuer takes its entry out as it says."
        ),
        Advice::CheckTheIssuer => format!(
            "the issuer at {issuer_at} did not record this machine: {why}\n  {NOTHING} Check \
             the host and port the code names, start aterm on the issuer, and join again.\n  On \
             a Mac, a first connection into the LAN asks to let aterm find devices on the local \
             network: allow it, then join again."
        ),
    }
}

/// A JOIN THE ISSUER RECORDED THAT DID NOT FINISH HERE ([`join`]): `why` a
/// write after the issuer's answer failed, that the issuer at `issuer_at`
/// lists this machine now, what this machine `holds` of the fleet, and that
/// a join again with the same code finishes it (a join again of the fleet it
/// holds writes what is missing).
fn unfinished(out: &str, issuer_at: &str, why: impl std::fmt::Display, holds: &str) -> Failure {
    aterm_uds::stdout::print(format_args!("{out}"));
    failed(format!(
        "{why}\n  The issuer at {issuer_at} has recorded this machine (its `aterm fleet peers` \
         lists it), but the join did not finish here: {holds}. Run `aterm fleet join` again \
         with the same code to finish it."
    ))
}

/// The words an issuer's refusal of a `join` carries when it holds no join
/// code ([`member::NOT_THE_ISSUER`]'s, and an older aterm's).
const NO_ISSUER: &str = "is no fleet's issuer";

/// `Ok` unless this machine is a fleet's issuer — it holds a join code
/// ([`CODE_FILE`]) — which never joins: not its own fleet, and not another
/// one, which would throw away its fleet's secret and code, a fleet nobody
/// could join again.
fn not_an_issuer(places: &Places, code: &JoinCode) -> Result<(), Failure> {
    let path = places.state.join(CODE_FILE);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        _ => {}
    }
    let own = member::read_join_code(&places.state).ok();
    Err(failed(match own {
        Some(own) if own.secret == code.secret => {
            "this machine is the issuer of the fleet that code is for: it does not join its \
             own fleet (`aterm fleet peers` lists its members)"
                .to_owned()
        }
        _ => format!(
            "this machine is the issuer of a fleet (its join code is {}): `aterm fleet leave` \
             first — that fleet's members then hold a secret no issuer keeps — and join again",
            path.display()
        ),
    }))
}

/// The issuer this machine's book binds a key to — its first `via=issuer`
/// entry's node, with the key its join bound — or `None` when it binds none
/// (no issuer, one from before node keys, a book that cannot be read): what
/// a join again of the same fleet must reach ([`register_with`]).
fn bound_issuer(places: &Places) -> Option<(String, NodeKey)> {
    member::read_peers(&places.state)
        .ok()?
        .into_iter()
        .find(|peer| peer.via == Via::Issuer)
        .and_then(|issuer| issuer.node.zip(issuer.key))
}

/// Dial the issuer `code` names for a `join`, proving `me`, and register;
/// the issuer's answer, the address it answered at, and the node key it
/// proved when it proved the node it answered as.
fn register_with(
    code: &JoinCode,
    (secret, me): (&FleetSecret, &NodeIdentity),
    (keys, bound): (&Keyring, Option<&(String, NodeKey)>),
    registration: &Registration,
) -> Result<(member::Joined, IpAddr, Option<NodeKey>), JoinFailed> {
    let started = Instant::now();
    let mut reached = member::dial(
        &code.host,
        None,
        code.port,
        (secret, Some(me)),
        FleetOp::Join,
        JOIN_WITHIN,
    )
    .map_err(JoinFailed::Dial)?;
    if let Some(proof) = &reached.peer
        && keys.bound(&proof.node).is_some_and(|key| key != proof.key)
    {
        return Err(JoinFailed::OtherKey(proof.node.clone()));
    }
    if let Some((node, key)) = bound
        && reached
            .peer
            .as_ref()
            .is_none_or(|proof| proof.node != *node || proof.key != *key)
    {
        return Err(JoinFailed::OtherIssuer {
            bound: node.clone(),
            proved: reached.peer.as_ref().map(|proof| proof.node.clone()),
        });
    }
    let deadline = (started + JOIN_WITHIN).max(Instant::now() + Duration::from_secs(1));
    let joined = member::register_by(&mut reached, registration, deadline)
        .map_err(JoinFailed::Registration)?;
    let key = match &reached.peer {
        Some(proof) if proof.node == joined.issuer.node => Some(proof.key),
        Some(proof) => {
            return Err(JoinFailed::Registration(format!(
                "the issuer answered as node {} and proved node {}",
                short(&joined.issuer.node),
                short(&proof.node)
            )));
        }
        None if keys.bound(&joined.issuer.node).is_some() => {
            return Err(JoinFailed::Unproved(joined.issuer.node));
        }
        None => None,
    };
    Ok((joined, reached.addr.ip(), key))
}

/// `aterm fleet peers [--timeout <s> | add <host[:port]> | rm <host[:port]|node>]`.
fn peers(places: &Places, args: &[String]) -> Result<String, Failure> {
    match args {
        [] => list_peers(places, PEER_CHECK_WITHIN),
        [flag, secs] if flag.as_str() == "--timeout" => list_peers(places, check_within(secs)?),
        [add, target] if add.as_str() == "add" => {
            let (host, port) = member::parse_host_port(target, Some(member::DEFAULT_PORT))
                .map_err(|why| Failure::Usage(format!("add <host[:port]>: {why}")))?;
            let peer = manual_peer(host, port);
            let target = peer.target();
            let mut out = String::new();
            private_state(places, &mut out)?;
            let recorded = member::update_peers(&places.state, |book| member::record(book, peer))
                .map_err(failed)?;
            out.push_str(&match recorded {
                Recorded::Already => {
                    format!("aterm fleet peers: {target} is in the book already\n")
                }
                _ => format!(
                    "aterm fleet peers: added {target} to {}\n",
                    places.state.join(PEERS_FILE).display()
                ),
            });
            Ok(out)
        }
        [rm, what] if rm.as_str() == "rm" => {
            let removed = match member::update_peers(&places.state, |book| {
                let copies = issuer_copies(places, book);
                member::remove(book, &copies, what)
            }) {
                Ok(removed) => removed,
                Err(refused) => {
                    return drop_copied_grant(places, what)?.ok_or_else(|| failed(refused));
                }
            };
            let copy_gone = removed.via == Via::Issuer
                && member::remove_private(&places.state, ISSUER_BOOK_FILE).unwrap_or(false);
            let mut out = format!(
                "aterm fleet peers: removed {} ({}){}\n",
                removed.target(),
                removed.node.as_deref().map_or("named by hand", short),
                if copy_gone {
                    ", and this machine's copy of its book"
                } else {
                    ""
                }
            );
            if let Some(node) = &removed.node
                && let Some(line) = drop_grants(places, |word| DriveGrant::node_of(word) == *node)?
            {
                out.push_str(&line);
            }
            Ok(out)
        }
        _ => Err(Failure::Usage(
            "peers takes nothing, `--timeout <s>`, `add <host[:port]>` or `rm <host[:port]|node>`"
                .to_owned(),
        )),
    }
}

/// `peers rm` of a node only this machine's copy of its issuer's book names.
///
/// # Errors
/// The book cannot be read, `what` names more than one copied peer, or aterm.toml cannot be edited.
fn drop_copied_grant(places: &Places, what: &str) -> Result<Option<String>, Failure> {
    let book = member::read_peers(&places.state).map_err(failed)?;
    if !matches!(member::find(&book, what), Ok(None)) {
        return Ok(None);
    }
    let copies = issuer_copies(places, &book);
    let Some(peer) = member::find(&copies, what)
        .map_err(Failure::Usage)?
        .and_then(|at| copies.get(at))
    else {
        return Ok(None);
    };
    let Some(node) = peer.node.as_deref() else {
        return Ok(None);
    };
    let Some(line) = drop_grants(places, |word| DriveGrant::node_of(word) == node)? else {
        return Ok(None);
    };
    Ok(Some(format!(
        "aterm fleet peers: {} ({}) stays in the fleet issuer's book, which this machine copies \
         (`aterm fleet peers rm` on the issuer takes it out); it drives this Mac no more\n{line}",
        peer.target(),
        short(node)
    )))
}

/// `peers --timeout`'s seconds: more than none, at most [`PEER_CHECK_MAX`].
fn check_within(secs: &str) -> Result<Duration, Failure> {
    secs.parse::<f64>()
        .ok()
        .filter(|s| *s > 0.0 && *s <= PEER_CHECK_MAX.as_secs_f64())
        .map(Duration::from_secs_f64)
        .ok_or_else(|| {
            Failure::Usage(format!(
                "--timeout takes seconds, more than 0 and at most {}",
                PEER_CHECK_MAX.as_secs()
            ))
        })
}

/// One row `peers` lists: a book entry, or a `[[fleet.peers]]` one, and what
/// its check came to.
struct Row {
    peer: Peer,
    /// From aterm.toml, not the book.
    configured: bool,
    /// What it serves and which node answered, or why it was not reached.
    check: Option<Result<member::Checked, Unreached>>,
}

/// What `aterm fleet peers` found, before it is said.
struct Listing {
    /// Every peer this machine knows, each checked.
    rows: Vec<Row>,
    /// Why nothing was checked, when it was not.
    unchecked: Option<String>,
    /// What the read of the issuer's book left this machine short of ([`book_note`]).
    note: Option<String>,
    /// `[fleet]` as aterm.toml says it.
    table: FleetTable,
    /// A line for each `[[fleet.peers]]` entry skipped.
    skipped: Vec<String>,
}

/// Every peer this machine knows ([`known_peers`]).
struct Known {
    /// Its book, the `[[fleet.peers]]` it does not hold already.
    peers: Vec<(Peer, bool)>,
    /// Each `[[fleet.peers]]` entry that is no host.
    skipped: Vec<(String, &'static str)>,
    /// `[fleet]` as aterm.toml says it.
    table: FleetTable,
    /// The keys its book and its whole copy bind ([`Keyring::of`]).
    keys: Keyring,
}

/// Every peer this machine knows: its book, then the `[[fleet.peers]]` it
/// does not hold already — at that port, by that address or that host name
/// ([`Peer::same_place`]: `lab-mini` for the `lab-mini.local` a Mac joined
/// as, an address for the one a member registered from) — then its copy of
/// its issuer's book ([`issuer_copies`], [`member::with_copies`]).
fn known_peers(places: &Places) -> Result<Known, Failure> {
    let config = ConfigFile::open(&places.config).map_err(failed)?;
    let table = fleet_table(text_of(&config)?).map_err(failed)?;
    let mut peers = member::read_peers(&places.state).map_err(failed)?;
    let copies = issuer_copies(places, &peers);
    let booked = peers.len();
    let mut skipped = Vec::new();
    for entry in &table.peers {
        match member::parse_host_port(entry, Some(member::DEFAULT_PORT)) {
            Ok((host, port)) => {
                let peer = manual_peer(host, port);
                if !peers.iter().any(|known| known.same_place(&peer)) {
                    peers.push(peer);
                }
            }
            Err(why) => skipped.push((entry.clone(), why)),
        }
    }
    let configured = booked..peers.len();
    let keys = Keyring::of(peers.iter().chain(&copies));
    member::with_copies(&mut peers, copies);
    let peers = peers
        .into_iter()
        .enumerate()
        .map(|(at, peer)| (peer, configured.contains(&at)))
        .collect();
    Ok(Known {
        peers,
        skipped,
        table,
        keys,
    })
}

/// This machine's copy of the book of the issuer `book` names
/// ([`member::read_issuer_book`]): none when the book names no issuer, or the
/// copy is another node's.
fn issuer_copies(places: &Places, book: &[Peer]) -> Vec<Peer> {
    book.iter()
        .find(|peer| peer.via == Via::Issuer)
        .and_then(|issuer| issuer.node.as_deref())
        .map(|issuer| member::read_issuer_book(&places.state, issuer))
        .unwrap_or_default()
}

/// A peer a person named, at `host` and `port`.
fn manual_peer(host: String, port: u16) -> Peer {
    Peer {
        node: None,
        key: None,
        key_from: None,
        addr: host.parse().ok(),
        host,
        port,
        version: None,
        seen_ms: None,
        via: Via::Manual,
    }
}

/// Every peer this machine knows ([`known_peers`]) and, on a member, every
/// one its issuer's book adds, each checked at once within `within`
/// ([`member::read_whole`] with `survey facts`); the book kept
/// ([`keep_book`]).
fn check_peers(places: &Places, within: Duration) -> Result<Listing, Failure> {
    let Known {
        peers,
        skipped,
        table,
        keys,
    } = known_peers(places)?;
    let skipped = skipped
        .iter()
        .map(|(entry, why)| {
            format!(
                "  [[fleet.peers]] host = {:?} is skipped: {why}\n",
                aterm_codec::tty::one_line(entry, 60)
            )
        })
        .collect();
    let configured: Vec<String> = peers
        .iter()
        .filter(|(_, configured)| *configured)
        .map(|(peer, _)| peer.target())
        .collect();
    let peers: Vec<Peer> = peers.into_iter().map(|(peer, _)| peer).collect();
    let row = |peer: Peer, check| Row {
        configured: peer.via == Via::Manual && configured.contains(&peer.target()),
        peer,
        check,
    };
    let (rows, unchecked, note) = match aterm_net::fleet::read_key_file(&places.state) {
        Ok(secret) => {
            let own = own_node(places);
            let me = own_identity(places);
            let (reads, book) = member::read_whole(
                &peers,
                &keys,
                own.as_deref(),
                (&secret, me.as_ref()),
                within,
                "survey facts",
            );
            let dropped = keep_book(places, &book, own.as_deref());
            let rows = reads
                .into_iter()
                .map(|read| {
                    row(
                        read.peer,
                        Some(read.read.map(|surveyed| surveyed.checked())),
                    )
                })
                .collect();
            (rows, None, joined_notes(book_note(&peers, &book), dropped))
        }
        Err(e) => (
            peers.into_iter().map(|peer| row(peer, None)).collect(),
            Some(no_key(&e)),
            None,
        ),
    };
    Ok(Listing {
        rows,
        unchecked,
        note,
        table,
        skipped,
    })
}

/// This machine's node id, when it has one: a member's own registration in
/// its issuer's book is left out of its copy.
fn own_node(places: &Places) -> Option<String> {
    member::read_node_id(&places.state).ok().flatten()
}

/// This machine's node and its key, which its reads prove to each peer:
/// `None` (each read then proves none) when it holds no node key — an
/// `init` or `join` from before keys came — or one it cannot read.
fn own_identity(places: &Places) -> Option<NodeIdentity> {
    member::read_node_identity(&places.state).ok().flatten()
}

/// Keep what a read made of the issuer's book `read` as this machine's copy
/// of it ([`ISSUER_BOOK_FILE`]), best effort: a book that came replaces the
/// copy whole ([`member::copies_of`], [`member::write_issuer_book`]); the
/// issuer answering that it shares none, or a book that names no issuer
/// now, takes it away; and a read that brought no answer leaves it — so a
/// member whose issuer sleeps still reads the members its last read named.
fn keep_book(places: &Places, read: &BookRead, own: Option<&str>) -> Option<String> {
    let _ = match read {
        BookRead::Fetched(book) => {
            let kept =
                member::write_issuer_book(&places.state, &book.node, &member::copies_of(book, own));
            if kept.is_ok() && book.proven {
                return drop_unbacked_grants(places);
            }
            kept
        }
        BookRead::Refused(_) | BookRead::NoIssuer => {
            member::remove_private(&places.state, ISSUER_BOOK_FILE).map(|_| ())
        }
        BookRead::Unshared(_) | BookRead::Unread(_) => Ok(()),
    };
    None
}

/// THE GRANTS A FRESH BOOK NO LONGER BACKS, taken out of `[fleet]
/// drive_from` ([`drop_grants`]), best effort: each whose node this
/// machine's book and its copy of its issuer's book, just kept, bind no
/// more ([`Keyring::read`], what the listener admits a driver by), or bind
/// to another key than the grant holds.
fn drop_unbacked_grants(places: &Places) -> Option<String> {
    let keys = Keyring::read(&places.state);
    let line = drop_grants(places, |word| {
        DriveGrant::from_word(word).is_some_and(|grant| keys.bound(&grant.node) != Some(grant.key))
    })
    .ok()??;
    Some(format!(
        "Its issuer's book binds a node this Mac let drive it no more, or to another key: {}",
        line.trim()
    ))
}

/// Two closing notes of a read, as one.
fn joined_notes(first: Option<String>, then: Option<String>) -> Option<String> {
    match (first, then) {
        (Some(first), Some(then)) => Some(format!("{first}\n{then}")),
        (first, then) => first.or(then),
    }
}

/// What a read is short of its issuer's book ([`book_short`]).
#[derive(Debug, PartialEq, Eq)]
enum Short<'a> {
    /// The issuer runs an older aterm, which shares no book.
    Older,
    /// The issuer shares no book, for this reason.
    NoBook(&'a str),
    /// The issuer answered its survey and its book did not come, for this reason.
    Unshared {
        /// Why no book came.
        why: &'a str,
        /// Whether the read held a copy of the issuer's last book.
        copies: bool,
    },
}

/// What the read of `peers` (this machine's, its copy of the issuer's book
/// among them) is short of its issuer's book, by what came of it (`read`):
/// `None` when the book came, or the issuer was not reached (its own row
/// says why), or there is no issuer.
fn book_short<'a>(peers: &[Peer], read: &'a BookRead) -> Option<Short<'a>> {
    match read {
        BookRead::Refused(BookUnread::Older(_)) => Some(Short::Older),
        BookRead::Refused(BookUnread::NoBook { why, .. }) => Some(Short::NoBook(why)),
        BookRead::Unshared(why) => Some(Short::Unshared {
            why,
            copies: peers.iter().any(|peer| peer.via == Via::IssuerBook),
        }),
        _ => None,
    }
}

/// The line a read of `peers` ends with when it is short of its issuer's
/// book ([`book_short`]), naming the issuer.
fn book_note(peers: &[Peer], read: &BookRead) -> Option<String> {
    let issuer = peers.iter().find(|peer| peer.via == Via::Issuer)?;
    let name = aterm_codec::tty::one_line(&shown_name(issuer), 60);
    Some(match book_short(peers, read)? {
        Short::Older => format!(
            "This machine read only the peers it knows: its issuer {name} runs an older aterm, \
             which shares no book of the fleet's members (update aterm there)."
        ),
        Short::NoBook(why) => format!(
            "This machine read only the peers it knows: its issuer {name} shares no book of the \
             fleet's members: {}",
            aterm_codec::tty::one_line(why, 120)
        ),
        Short::Unshared { why, copies } => format!(
            "This machine's issuer {name} answered, but its book of the fleet's members did not \
             come ({}): {}",
            aterm_codec::tty::one_line(why, 120),
            if copies {
                "the read asked the members its last read of that book named."
            } else {
                "this machine holds no earlier read of that book, so the read reached only \
                 the peers it knows."
            }
        ),
    })
}

/// How a peer is named in a read: its host, with `:port` where that is not
/// the fleet's default.
fn shown_name(peer: &Peer) -> String {
    if peer.port == member::DEFAULT_PORT {
        peer.host.clone()
    } else {
        member::host_port(&peer.host, peer.port)
    }
}

/// `aterm fleet peers`: every peer this machine knows, each checked at once
/// within `within`.
fn list_peers(places: &Places, within: Duration) -> Result<String, Failure> {
    let listing = check_peers(places, within)?;
    let table = &listing.table;
    let mut out = format!(
        "aterm fleet peers: {} known to this machine{}\n",
        listing.rows.len(),
        match (&listing.unchecked, &table.listen) {
            (Some(why), _) => format!(" — not checked: {why}"),
            (None, Some(listen)) => format!(
                " (it listens on {}, serving {})",
                aterm_codec::tty::one_line(listen, 60),
                table.level().unwrap_or(FleetScope::Facts.word())
            ),
            (None, None) => " (it has no [fleet] listen: no peer can read it)".to_owned(),
        }
    );
    out.extend(listing.skipped.iter().cloned());
    if listing.rows.is_empty() {
        out.push_str(
            "  none yet: a member is recorded when it runs `aterm fleet join`; \
             `aterm fleet peers add <host>` names one by hand\n",
        );
        return Ok(out);
    }
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    let mut no_route = 0;
    let mut unreached = 0;
    out.push_str(&format!(
        "  {:<8}  {:<11}  {:<23}  {:<21}  {:<15}  {:<7}  {:<8}  CHECK\n",
        "NODE", "VIA", "HOST", "ADDRESS", "VERSION", "JOINED", "KEY"
    ));
    for row in &listing.rows {
        let peer = &row.peer;
        let check = match &row.check {
            None => "-".to_owned(),
            Some(Ok(checked)) => match (&peer.node, &checked.node) {
                (None, Some(node)) => format!(
                    "ok, serves {} (node {}{})",
                    checked.scope.word(),
                    short(node),
                    if checked.proven { ", proven" } else { "" }
                ),
                _ => format!("ok, serves {}", checked.scope.word()),
            },
            Some(Err(why)) => {
                // A peer never dialled (a name that did not resolve) says
                // nothing of this machine's route either way.
                if why.dialled() {
                    unreached += 1;
                }
                if why.no_route() {
                    no_route += 1;
                }
                aterm_codec::tty::one_line(&why.to_string(), 200)
            }
        };
        out.push_str(&format!(
            "  {:<8}  {:<11}  {:<23}  {:<21}  {:<15}  {:<7}  {:<8}  {check}\n",
            peer.node.as_deref().map_or("-", short),
            if row.configured {
                "config"
            } else {
                peer.via.word()
            },
            aterm_codec::tty::one_line(&peer.host, 23),
            peer.target(),
            peer.version.as_deref().unwrap_or("-"),
            peer.seen_ms
                .map_or_else(|| "-".to_owned(), |ms| age(now_ms.saturating_sub(ms))),
            match (&peer.node, peer.key) {
                (_, Some(key)) => key.short(),
                (Some(_), None) => "unbound".to_owned(),
                (None, None) => "-".to_owned(),
            },
        ));
    }
    if no_route > 0 && no_route == unreached {
        out.push_str(
            "Every peer not reached had no route: on a Mac that is local-network privacy, \
             which refuses an app's connections into the LAN until they are allowed (System \
             Settings › Privacy & Security › Local Network: aterm, or the terminal this runs \
             in).\n",
        );
    }
    if let Some(note) = &listing.note {
        out.push_str(note);
        out.push('\n');
    }
    Ok(out)
}

/// An age as a person reads it: `40s`, `12m`, `3h`, `9d`.
fn age(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

/// `aterm fleet allow facts|content` and `aterm fleet allow drive --from
/// <host|node> [--from …]` (the step-in,
/// `docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5.1): what a peer may do
/// here, written to `[fleet] allow` and `drive_from` on this machine alone —
/// no join and no fleet op writes either.
fn allow(places: &Places, args: &[String]) -> Result<String, Failure> {
    let usage = |why: &str| {
        Failure::Usage(format!(
            "{why} (aterm fleet allow facts|content, or aterm fleet allow drive --from \
             <host|node>)"
        ))
    };
    let (level, rest) = args
        .split_first()
        .ok_or_else(|| usage("allow names a level"))?;
    let level = level.to_ascii_lowercase();
    let mut from: Vec<String> = Vec::new();
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        match arg.strip_prefix("--from=") {
            Some(word) => from.push(word.to_owned()),
            None if arg == "--from" => from.push(
                rest.next()
                    .cloned()
                    .ok_or_else(|| usage("--from takes a host or node"))?,
            ),
            None => {
                return Err(usage(&format!(
                    "unexpected `{}`",
                    aterm_codec::tty::one_line(arg, 40)
                )));
            }
        }
    }
    let config = ConfigFile::open(&places.config).map_err(failed)?;
    let table = fleet_table(text_of(&config)?).map_err(failed)?;
    if table.listen.is_none() {
        return Err(failed(
            "this machine is in no fleet ([fleet] listen is not set): `aterm fleet init` or \
             `aterm fleet join` first",
        ));
    }
    let (word, nodes, config) = if level == aterm_net::fleet::DRIVE_LEVEL {
        if from.is_empty() {
            return Err(usage("drive names the nodes that may drive this Mac"));
        }
        let grants = drive_grants(places, &from)?;
        let config = ConfigFile::open(&places.config).map_err(failed)?;
        let mut nodes = fleet_table(text_of(&config)?).map_err(failed)?.drive_from;
        for grant in grants {
            nodes.retain(|word| DriveGrant::node_of(word) != grant.node);
            nodes.push(grant.word());
        }
        (aterm_net::fleet::DRIVE_LEVEL, nodes, config)
    } else {
        let scope = FleetScope::from_word(&level).ok_or_else(|| usage("no such level"))?;
        if !from.is_empty() {
            return Err(usage(
                "--from names who may drive, at the drive level alone",
            ));
        }
        (scope.word(), Vec::new(), config)
    };
    let said = if nodes.is_empty() {
        format!("[fleet] allow = \"{word}\"")
    } else {
        format!(
            "[fleet] allow = \"{word}\", drive_from = [{}]",
            nodes
                .iter()
                .map(|node| format!("\"{node}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let mut out = match with_level(text_of(&config)?, word, &nodes).map_err(failed)? {
        None => format!(
            "aterm fleet allow: {said} in {} already\n",
            places.config.display()
        ),
        Some(text) => {
            let bak = config
                .lock()
                .and_then(|edit| edit.replace(&text))
                .map_err(failed)?;
            format!(
                "aterm fleet allow: {said} in {}{}\n",
                places.config.display(),
                bak.map_or_else(String::new, |b| format!(
                    " (previous saved as {})",
                    b.display()
                ))
            )
        }
    };
    out.push_str(if nodes.is_empty() {
        "  no other Mac may drive this one's sessions; the running aterm follows at once\n"
    } else {
        "  each node named, proving the node key it holds now, may drive this Mac's sessions \
         (`aterm ctl --host <this Mac> …` there); nobody else may, and a Mac that comes back \
         under another key drives nothing until it is named here again. The person here wins: \
         Take Over refuses every remote write. `aterm fleet allow content` takes it back.\n"
    });
    Ok(out)
}

/// The grant each of `words` names for `allow drive --from`.
///
/// # Errors
/// A word that names no peer, or a peer with no node or one bound to no key.
fn drive_grants(places: &Places, words: &[String]) -> Result<Vec<DriveGrant>, Failure> {
    check_peers(places, PEER_CHECK_WITHIN)?;
    let known: Vec<Peer> = known_peers(places)?
        .peers
        .into_iter()
        .map(|(peer, _)| peer)
        .collect();
    let mut nodes = Vec::new();
    for word in words {
        let shown = aterm_codec::tty::one_line(word, 40);
        let peer = match bound_peer(places, &known, word)? {
            Bound::Node(peer) => peer,
            Bound::Unbound(peer) => {
                return Err(failed(format!(
                    "`{shown}` is node {}, which this machine binds no key to: it is bound when \
                     that machine joins the issuer again, and only then may it drive",
                    short(peer.node.as_deref().unwrap_or("-"))
                )));
            }
            Bound::ByHand => {
                return Err(failed(format!(
                    "`{shown}` is a peer named by hand, with no node: a Mac that may drive is \
                     named by the node its join recorded"
                )));
            }
            Bound::Nobody => {
                return Err(failed(format!(
                    "`{shown}` names no peer this machine knows, nor any its issuer's book names \
                     (`aterm fleet peers`)"
                )));
            }
        };
        if let (Some(node), Some(key)) = (peer.node, peer.key) {
            nodes.push(DriveGrant { node, key });
        }
    }
    Ok(nodes)
}

/// What a word names among the peers a drive can name ([`bound_peer`]).
#[derive(Debug)]
enum Bound {
    /// The peer of a node this machine binds a key to: its node and that
    /// key are set.
    Node(Peer),
    /// The peer of a node this machine binds no key to: its node is set.
    Unbound(Peer),
    /// A peer named by hand, with no node.
    ByHand,
    /// No peer.
    Nobody,
}

/// The node `what` names, for a drive or its grant.
///
/// # Errors
/// `what` names more than one peer of either list.
fn bound_peer(places: &Places, known: &[Peer], what: &str) -> Result<Bound, Failure> {
    let bound = member::bound_peers(&places.state);
    if let Some(at) = member::find(&bound, what).map_err(Failure::Usage)? {
        return Ok(Bound::Node(bound[at].clone()));
    }
    Ok(match member::find(known, what).map_err(Failure::Usage)? {
        None => Bound::Nobody,
        Some(at) => match &known[at].node {
            None => {
                let stead: Vec<&Peer> = bound
                    .iter()
                    .filter(|peer| peer.same_place(&known[at]))
                    .collect();
                match stead.as_slice() {
                    [] => Bound::ByHand,
                    [peer] => Bound::Node((*peer).clone()),
                    many => {
                        return Err(Failure::Usage(format!(
                            "`{}` is a peer named by hand at the place of {} nodes ({}): name \
                             one by its node id",
                            aterm_codec::tty::one_line(what, 80),
                            many.len(),
                            many.iter()
                                .map(|peer| short(peer.node.as_deref().unwrap_or("-")))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )));
                    }
                }
            }
            Some(node) => bound
                .iter()
                .find(|peer| peer.node.as_ref() == Some(node))
                .map_or_else(
                    || Bound::Unbound(known[at].clone()),
                    |peer| Bound::Node(peer.clone()),
                ),
        },
    })
}

/// `[fleet] drive_from` without the grants `gone` picks, in aterm.toml.
///
/// # Errors
/// Aterm.toml cannot be read, or the edit cannot be written.
fn drop_grants(places: &Places, gone: impl Fn(&str) -> bool) -> Result<Option<String>, Failure> {
    let config = ConfigFile::open(&places.config).map_err(failed)?;
    let text = text_of(&config)?;
    let table = fleet_table(text).map_err(failed)?;
    let kept: Vec<String> = table
        .drive_from
        .iter()
        .filter(|word| !gone(word))
        .cloned()
        .collect();
    if kept.len() == table.drive_from.len() {
        return Ok(None);
    }
    let level = match table.level() {
        Some(aterm_net::fleet::DRIVE_LEVEL) if kept.is_empty() => FleetScope::Content.word(),
        Some(level) => level,
        None => table.allow.as_deref().unwrap_or(FleetScope::Facts.word()),
    };
    let Some(edited) = with_level(text, level, &kept).map_err(failed)? else {
        return Ok(None);
    };
    let bak = config
        .lock()
        .and_then(|edit| edit.replace(&edited))
        .map_err(failed)?;
    Ok(Some(format!(
        "  drive      {} of [fleet] drive_from removed from {}{}{}\n",
        if table.drive_from.len() - kept.len() == 1 {
            "its grant".to_owned()
        } else {
            format!("{} grants", table.drive_from.len() - kept.len())
        },
        places.config.display(),
        if kept.is_empty() && level == FleetScope::Content.word() {
            ", and allow is content: no Mac drives this one"
        } else {
            ""
        },
        bak.map_or_else(String::new, |b| format!(
            " (previous saved as {})",
            b.display()
        ))
    )))
}

/// `aterm fleet leave`.
fn leave(places: &Places, args: &[String]) -> Result<String, Failure> {
    if !args.is_empty() {
        return Err(Failure::Usage("leave takes nothing".to_owned()));
    }
    let mut out = String::from("aterm fleet leave:\n");
    let mut issued = false;
    for (name, what) in [
        (KEY_FILE, "the fleet's secret"),
        (CODE_FILE, "the join code"),
        (PEERS_FILE, "the book of peers"),
        (ISSUER_BOOK_FILE, "the copy of the issuer's book"),
    ] {
        let path = places.state.join(name);
        let had = member::remove_private(&places.state, name)
            .map_err(|e| failed(format!("{}: {e}", path.display())))?;
        if had {
            out.push_str(&format!("  removed    {} ({what})\n", path.display()));
            issued |= name == CODE_FILE;
        }
    }
    let config = ConfigFile::open(&places.config).map_err(failed)?;
    match without_fleet(text_of(&config)?).map_err(failed)? {
        None => {}
        Some(text) => {
            let bak = config
                .lock()
                .and_then(|edit| edit.replace(&text))
                .map_err(failed)?;
            out.push_str(&format!(
                "  config     [fleet] listen, allow and drive_from removed from {}{}\n",
                places.config.display(),
                bak.map_or_else(String::new, |b| format!(
                    " (previous saved as {})",
                    b.display()
                ))
            ));
        }
    }
    if out.lines().count() == 1 {
        return Ok("aterm fleet leave: this machine is in no fleet\n".to_owned());
    }
    out.push_str("The running aterm stops listening within 2 s (it watches the key). ");
    out.push_str(if issued {
        "This machine issued the fleet, and its fleet went with it: no machine can join it \
         or be rotated now. Run `aterm fleet init` on one machine and `aterm fleet join` on \
         each other to make a new one.\n"
    } else {
        "The issuer is not told: its `aterm fleet peers` reads this machine as not answering \
         until `aterm fleet peers rm` there.\n"
    });
    Ok(out)
}

/// The fleet client of `aterm ctl survey --fleet | --host` (`aterm_ctl::LocalVerbs::fleet_survey`).
///
/// # Errors
/// This machine holds no fleet key it can read, aterm.toml or the book cannot be read.
pub fn survey_fleet(ask: &aterm_ctl::FleetAsk<'_>) -> Result<aterm_ctl::FleetRead, String> {
    let places = Places::here().map_err(failure_words)?;
    survey_fleet_in(&places, ask)
}

/// The step-IN's dial: the peer `host` names.
///
/// # Errors
/// This machine is in no fleet or holds no node key.
pub fn drive_fleet(
    host: &str,
    within: Duration,
) -> Result<aterm_ctl::DriveLink, aterm_ctl::DriveRefused> {
    let places = Places::here().map_err(failure_words)?;
    drive_fleet_in(&places, host, within)
}

/// [`drive_fleet`] from the machine whose state and aterm.toml `places` are.
fn drive_fleet_in(
    places: &Places,
    host: &str,
    within: Duration,
) -> Result<aterm_ctl::DriveLink, aterm_ctl::DriveRefused> {
    let secret = aterm_net::fleet::read_key_file(&places.state).map_err(|e| no_key(&e))?;
    let me = own_identity(places).ok_or(
        "this machine holds no node key it can read, so it proves no node and drives none: \
         `aterm fleet join` again mints it",
    )?;
    let known = known_peers(places).map_err(failure_words)?;
    let keys = known.keys;
    let book: Vec<Peer> = known.peers.into_iter().map(|(peer, _)| peer).collect();
    let shown = aterm_codec::tty::one_line(host, 80);
    let peer = match bound_peer(places, &book, host).map_err(failure_words)? {
        Bound::Node(peer) | Bound::Unbound(peer) => peer,
        Bound::ByHand => {
            return Err(format!(
                "`{shown}` is a peer named by hand, with no node: a drive goes to the node a \
                 join recorded, by its host, node id or address (`aterm fleet peers`)"
            )
            .into());
        }
        Bound::Nobody => {
            return Err(format!(
                "`{shown}` is no Mac this machine's book records (`aterm fleet peers`): a \
                 drive goes only to a node the book holds, named by its host, node id or the \
                 address and port it lists, never to whichever member answers at an address"
            )
            .into());
        }
    };
    let label = shown_name(&peer);
    let driven =
        member::drive(&target_of(&peer), &keys, (&secret, &me), within).map_err(|why| {
            aterm_ctl::DriveRefused {
                again: drive_again(&why),
                why: format!("{label}: {why}"),
            }
        })?;
    Ok(aterm_ctl::DriveLink {
        stream: Box::new(driven),
        label,
    })
}

/// Whether a drive dial that failed `why` may be served a moment later: the
/// peer's share for this machine's address was full (`NO capacity`), or
/// nothing listened at its port (its aterm restarting, or its listener
/// rebinding).
fn drive_again(why: &Unreached) -> bool {
    match why {
        Unreached::Handshake(_, aterm_net::fleet::FleetError::AddressBusy) => true,
        Unreached::Connect(_, e) => e.kind() == io::ErrorKind::ConnectionRefused,
        _ => false,
    }
}

/// What a [`Failure`] says.
fn failure_words(failure: Failure) -> String {
    match failure {
        Failure::Usage(why) | Failure::Failed(why) => why,
    }
}

/// [`survey_fleet`] of the machine whose state and aterm.toml `places` are.
fn survey_fleet_in(
    places: &Places,
    ask: &aterm_ctl::FleetAsk<'_>,
) -> Result<aterm_ctl::FleetRead, String> {
    let secret = aterm_net::fleet::read_key_file(&places.state).map_err(|e| no_key(&e))?;
    let known = known_peers(places).map_err(failure_words)?;
    let mut book: Vec<Peer> = known.peers.into_iter().map(|(peer, _)| peer).collect();
    let me = own_identity(places);
    if let Some(what) = ask.host {
        let keys = &known.keys;
        let mut bound = member::bound_peers(&places.state);
        let peer = match member::find(&bound, what)? {
            Some(at) => bound.swap_remove(at),
            None => match member::find(&book, what)? {
                Some(at) => book.swap_remove(at),
                None => {
                    let (host, port) = member::parse_host_port(what, Some(member::DEFAULT_PORT))
                        .map_err(|why| {
                            format!(
                                "`{}` is no peer this machine knows (`aterm fleet peers`), and \
                                 no host: {why}",
                                aterm_codec::tty::one_line(what, 80)
                            )
                        })?;
                    manual_peer(host, port)
                }
            },
        };
        raise_open_files();
        let read = member::survey_all(
            &[target_of(&peer)],
            keys,
            (&secret, me.as_ref()),
            ask.within,
        );
        return Ok(aterm_ctl::FleetRead {
            peers: read
                .into_iter()
                .map(|read| peer_survey(&peer, read))
                .collect(),
            note: None,
        });
    }
    raise_open_files();
    let own = own_node(places);
    let (reads, book_read) = member::read_whole(
        &book,
        &known.keys,
        own.as_deref(),
        (&secret, me.as_ref()),
        ask.within,
        "survey",
    );
    let dropped = keep_book(places, &book_read, own.as_deref());
    let (entries, answers): (Vec<Peer>, Vec<_>) =
        reads.into_iter().map(|read| (read.peer, read.read)).unzip();
    let mut peers: Vec<aterm_ctl::PeerSurvey> = entries
        .iter()
        .zip(once_per_node(&entries, answers))
        .filter_map(|(peer, read)| Some(peer_survey(peer, read?)))
        .collect();
    peers.extend(
        known
            .skipped
            .into_iter()
            .map(|(entry, why)| aterm_ctl::PeerSurvey {
                label: format!("[[fleet.peers]] {}", aterm_codec::tty::one_line(&entry, 40)),
                answer: Err(format!("it is no host, so it was not asked: {why}")),
                fault: aterm_ctl::PeerFault::NotDialled,
                unproven: false,
            }),
    );
    Ok(aterm_ctl::FleetRead {
        peers,
        note: joined_notes(book_note(&book, &book_read), dropped),
    })
}

/// What kind of failure `why` is, as the fleet read's closing note reads
/// the failures of every peer together (`aterm_ctl::PeerFault`).
fn fault_of(why: &Unreached) -> aterm_ctl::PeerFault {
    use aterm_ctl::PeerFault;
    if !why.dialled() {
        PeerFault::NotDialled
    } else if why.no_route() {
        PeerFault::NoRoute
    } else if why.no_network() {
        PeerFault::NoNetwork
    } else if why.not_permitted() {
        PeerFault::NotPermitted
    } else {
        PeerFault::Other
    }
}

/// ONE MACHINE, ONE BLOCK: `read`, one answer for each of `entries`, with
/// every answer from a node another answer is kept for taken out (`None`,
/// in its place) — two entries that name one machine (a person's
/// `[[fleet.peers]]` or `peers add` by another name than it joined with, a
/// book entry by name beside one by address) are read once, never twice for
/// the budget and the count of machines.
fn once_per_node(
    entries: &[Peer],
    read: Vec<Result<member::Surveyed, Unreached>>,
) -> Vec<Option<Result<member::Surveyed, Unreached>>> {
    let node_of = |answer: &Result<member::Surveyed, Unreached>| match answer {
        Ok(member::Surveyed {
            node: Some(node),
            proven,
            ..
        }) => Some((node.clone(), *proven)),
        _ => None,
    };
    let checked = |at: usize, node: &str| {
        entries
            .get(at)
            .is_some_and(|entry| entry.node.as_deref() == Some(node))
    };
    let same_failure = |ours: &Unreached, theirs: &Unreached| {
        ours.at().is_some() && ours.at() == theirs.at() && ours.to_string() == theirs.to_string()
    };
    let folded: Vec<bool> = entries
        .iter()
        .zip(&read)
        .map(|(by_hand, ours)| match ours {
            Err(ours) if by_hand.node.is_none() => {
                entries.iter().zip(&read).any(|(entry, theirs)| {
                    entry.node.is_some()
                        && entry.same_place(by_hand)
                        && matches!(theirs, Err(theirs) if same_failure(ours, theirs))
                })
            }
            _ => false,
        })
        .collect();
    let mut kept: Vec<((String, bool), usize)> = Vec::new();
    for (at, answer) in read.iter().enumerate() {
        let Some(named) = node_of(answer) else {
            continue;
        };
        match kept.iter_mut().find(|(seen, _)| *seen == named) {
            None => kept.push((named, at)),
            Some(((node, _), first)) if !checked(*first, node) && checked(at, node) => {
                *first = at;
            }
            Some(_) => {}
        }
    }
    read.into_iter()
        .enumerate()
        .map(|(at, answer)| {
            let other = node_of(&answer).is_some_and(|named| !kept.contains(&(named, at)));
            (!other && folded.get(at) != Some(&true)).then_some(answer)
        })
        .collect()
}

/// One peer's answer as the survey prints it: labelled with where it came
/// from — the name dialled and the address that answered, which a reply's
/// own `host=` never stands for — and unproven when it proved no node key
/// this machine binds ([`member::Surveyed::proven`]).
fn peer_survey(peer: &Peer, read: Result<member::Surveyed, Unreached>) -> aterm_ctl::PeerSurvey {
    let name = shown_name(peer);
    aterm_ctl::PeerSurvey {
        label: match &read {
            Ok(surveyed) if surveyed.addr.ip().to_string() != peer.host => {
                format!("{name} at {}", surveyed.addr.ip())
            }
            _ => name,
        },
        fault: read
            .as_ref()
            .err()
            .map_or(aterm_ctl::PeerFault::Other, fault_of),
        unproven: matches!(&read, Ok(surveyed) if !surveyed.proven),
        answer: read
            .map(|surveyed| surveyed.lines)
            .map_err(|e| e.to_string()),
    }
}

/// Why a fleet read asked no peer, the key being `error`: short enough that
/// the survey's row for the fleet (160 characters) keeps its remedy.
fn no_key(error: &aterm_net::fleet::KeyFileError) -> String {
    match error {
        aterm_net::fleet::KeyFileError::Missing => "this machine is in no fleet: `aterm fleet \
             init` on one machine, `aterm fleet join` on each other"
            .to_owned(),
        other => format!("its fleet key cannot be used: {other}"),
    }
}

/// Raise this process's soft `RLIMIT_NOFILE` toward its hard limit (at most
/// 1024), best effort: a reader asking [`member::AT_ONCE`] peers at once,
/// each with its name lookup's sockets, from a shell whose limit is a
/// Dock-launched app's 256.
fn raise_open_files() {
    const WANT: libc::rlim_t = 1024;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` writes the struct this frame owns, of the type the call takes.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return;
    }
    let want = WANT.min(limit.rlim_max);
    if limit.rlim_cur >= want {
        return;
    }
    limit.rlim_cur = want;
    // SAFETY: `setrlimit` reads the struct this frame owns; a soft limit at
    // or under the hard one is all it is asked.
    let _ = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) };
}

/// `[fleet]` as aterm.toml says it now.
#[derive(Debug, Default, PartialEq, Eq)]
struct FleetTable {
    listen: Option<String>,
    allow: Option<String>,
    /// Each `[[fleet.peers]]` entry's `host`.
    peers: Vec<String>,
    /// `drive_from`: the nodes that may drive this Mac at `allow = "drive"`.
    drive_from: Vec<String>,
}

impl FleetTable {
    fn listen_port(&self) -> Option<u16> {
        self.listen
            .as_deref()?
            .parse::<std::net::SocketAddr>()
            .ok()
            .map(|addr| addr.port())
    }

    /// The level `allow` names — `facts`, `content` or `drive` — or `None`
    /// for none of them.
    fn level(&self) -> Option<&'static str> {
        let word = self.allow.as_deref()?.to_ascii_lowercase();
        if word == aterm_net::fleet::DRIVE_LEVEL {
            return Some(aterm_net::fleet::DRIVE_LEVEL);
        }
        FleetScope::from_word(&word).map(FleetScope::word)
    }
}

/// Read `[fleet]` from aterm.toml's `text` with the parser the app loads it
/// with.
fn fleet_table(text: &str) -> Result<FleetTable, String> {
    let doc: aterm_toml::Table = aterm_toml::from_str(text)
        .map_err(|e| format!("aterm.toml is not valid TOML, so it is left as it was: {e}"))?;
    let Some(fleet) = doc.get("fleet") else {
        return Ok(FleetTable::default());
    };
    let fleet = fleet
        .as_table()
        .ok_or("aterm.toml's `fleet` is no table; edit it by hand")?;
    let string = |key: &str| {
        fleet
            .get(key)
            .and_then(aterm_toml::Value::as_str)
            .map(str::to_owned)
    };
    let peers = fleet
        .get("peers")
        .and_then(aterm_toml::Value::as_array)
        .map(|peers| {
            peers
                .iter()
                .filter_map(|peer| peer.get("host")?.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let drive_from = fleet
        .get("drive_from")
        .and_then(aterm_toml::Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| node.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Ok(FleetTable {
        listen: string("listen"),
        allow: string("allow"),
        peers,
        drive_from,
    })
}

/// `text` with `[fleet] listen` and `allow` set.
///
/// # Errors
/// `text` is not TOML, `fleet` is no table, or the edit would not read back as exactly this change.
fn with_fleet(
    text: &str,
    listen: &str,
    allow: &str,
    ungrant: bool,
) -> Result<Option<String>, String> {
    use aterm_toml::edit::{DocumentMut, Item, Table, Value};
    let now = fleet_table(text)?;
    if now.listen.as_deref() == Some(listen)
        && now.allow.as_deref() == Some(allow)
        && (!ungrant || now.drive_from.is_empty())
    {
        return Ok(None);
    }
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("aterm.toml is not valid TOML, so it is left as it was: {e}"))?;
    let root = doc.as_table_mut();
    if root.get("fleet").is_none_or(Item::is_none) {
        root.insert("fleet", Item::Table(Table::new()));
    }
    let fleet = root
        .get_mut("fleet")
        .and_then(Item::as_table_like_mut)
        .ok_or("aterm.toml's `fleet` is no table; edit it by hand")?;
    fleet.insert("listen", Item::Value(Value::from(listen)));
    fleet.insert("allow", Item::Value(Value::from(allow)));
    if ungrant {
        fleet.remove("drive_from");
    }
    let edited = doc.to_string();
    reads_back(text, &edited, |fleet| {
        fleet.insert(
            "listen".to_owned(),
            aterm_toml::Value::String(listen.to_owned()),
        );
        fleet.insert(
            "allow".to_owned(),
            aterm_toml::Value::String(allow.to_owned()),
        );
        if ungrant {
            fleet.remove("drive_from");
        }
    })?;
    Ok(Some(edited))
}

/// `text` with `[fleet] allow` set to `level` and `drive_from` to `drive_from`.
///
/// # Errors
/// As [`with_fleet`].
fn with_level(text: &str, level: &str, drive_from: &[String]) -> Result<Option<String>, String> {
    use aterm_toml::edit::{Array, DocumentMut, Item, Value};
    let now = fleet_table(text)?;
    if now.allow.as_deref() == Some(level) && now.drive_from == drive_from {
        return Ok(None);
    }
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("aterm.toml is not valid TOML, so it is left as it was: {e}"))?;
    let fleet = doc
        .as_table_mut()
        .get_mut("fleet")
        .and_then(Item::as_table_like_mut)
        .ok_or("aterm.toml has no [fleet] table: this machine is in no fleet")?;
    fleet.insert("allow", Item::Value(Value::from(level)));
    if drive_from.is_empty() {
        fleet.remove("drive_from");
    } else {
        let nodes: Array = drive_from.iter().map(String::as_str).collect();
        fleet.insert("drive_from", Item::Value(Value::from(nodes)));
    }
    let edited = doc.to_string();
    reads_back(text, &edited, |fleet| {
        fleet.insert(
            "allow".to_owned(),
            aterm_toml::Value::String(level.to_owned()),
        );
        if drive_from.is_empty() {
            fleet.remove("drive_from");
        } else {
            fleet.insert(
                "drive_from".to_owned(),
                aterm_toml::Value::Array(
                    drive_from
                        .iter()
                        .map(|node| aterm_toml::Value::String(node.clone()))
                        .collect(),
                ),
            );
        }
    })?;
    Ok(Some(edited))
}

/// `text` with `[fleet] listen`.
///
/// # Errors
/// As [`with_fleet`].
fn without_fleet(text: &str) -> Result<Option<String>, String> {
    use aterm_toml::edit::DocumentMut;
    let now = fleet_table(text)?;
    if now.listen.is_none() && now.allow.is_none() && now.drive_from.is_empty() {
        return Ok(None);
    }
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("aterm.toml is not valid TOML, so it is left as it was: {e}"))?;
    doc.remove_path(&["fleet", "listen"]);
    doc.remove_path(&["fleet", "allow"]);
    doc.remove_path(&["fleet", "drive_from"]);
    if doc
        .get("fleet")
        .and_then(aterm_toml::edit::Item::as_table_like)
        .is_some_and(|fleet| fleet.is_empty())
    {
        doc.remove("fleet");
    }
    let edited = doc.to_string();
    reads_back(text, &edited, |fleet| {
        fleet.remove("listen");
        fleet.remove("allow");
        fleet.remove("drive_from");
    })?;
    Ok(Some(edited))
}

/// `edited` read back through the parser the app loads aterm.toml with, held
/// to what the edit meant: `text`'s meaning with `fleet` applied to its
/// `[fleet]` table (an emptied table counts as none).
fn reads_back(
    text: &str,
    edited: &str,
    fleet: impl FnOnce(&mut aterm_toml::Table),
) -> Result<(), String> {
    use aterm_toml::Value;
    let mut want: aterm_toml::Table =
        aterm_toml::from_str(text).map_err(|e| format!("not valid TOML: {e}"))?;
    let after: aterm_toml::Table = aterm_toml::from_str(edited)
        .map_err(|_| "the edited aterm.toml would not be valid TOML".to_owned())?;
    let mut table = match want.remove("fleet") {
        Some(Value::Table(table)) => table,
        _ => aterm_toml::Table::new(),
    };
    fleet(&mut table);
    if !table.is_empty() {
        want.insert("fleet".to_owned(), Value::Table(table));
    }
    if want == after {
        Ok(())
    } else {
        Err("the edited aterm.toml would not read back as meant; set [fleet] by hand".to_owned())
    }
}

/// aterm.toml's text as `config` read it: `""` when there was no file.
fn text_of(config: &ConfigFile) -> Result<&str, Failure> {
    Ok(config.text().map_err(failed)?.unwrap_or(""))
}

/// Write `[fleet] listen` and `allow` to aterm.toml at `path` (and take
/// `drive_from` out where `ungrant`, [`with_fleet`]); the report line says
/// what changed.
fn write_fleet(
    config: &ConfigFile,
    path: &Path,
    (listen, allow): (&str, &str),
    ungrant: bool,
) -> Result<String, Failure> {
    let said = format!("[fleet] listen = \"{listen}\", allow = \"{allow}\"");
    match with_fleet(text_of(config)?, listen, allow, ungrant).map_err(failed)? {
        None => Ok(format!(
            "  config     {said} in {} already\n",
            path.display()
        )),
        Some(text) => {
            let bak = config
                .lock()
                .and_then(|edit| edit.replace(&text))
                .map_err(failed)?;
            Ok(format!(
                "  config     {said} in {}{}\n",
                path.display(),
                bak.map_or_else(String::new, |b| format!(
                    " (previous saved as {})",
                    b.display()
                ))
            ))
        }
    }
}

#[cfg(test)]
#[path = "fleet_member_tests.rs"]
mod tests;
