// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLI front-end for `aterm fleet init|join|peers|allow|leave` over [`aterm_net::fleet_member`].

use std::ffi::OsString;
use std::io::{self, BufRead as _, IsTerminal as _, Read as _, Write as _};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_net::fleet::{
    DriveGrant, FleetError, FleetOp, FleetScope, FleetSecret, KEY_FILE, NodeIdentity, NodeKey,
};
use aterm_net::fleet_member::{
    self as member, Began, BookRead, BookUnread, CODE_FILE, Held, HeldBook, ISSUER_BOOK_FILE,
    JoinCode, Keyring, Leaving, NODE_FILE, NODE_KEY_FILE, PEERS_FILE, Peer, Recorded, Registration,
    Unreached, Unwritten, Via, target_of,
};
use aterm_uds::atomic_file::ConfigFile;
use aterm_uds::private_file::ANOTHER_LINK;

/// The whole `aterm fleet --help` (`fleet_cli` prints it), and each verb's `--help`.
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
                         the machines this one knows (its book and aterm.toml's
                         [[fleet.peers]] host = \"<host[:port]>\" entries), each
                         checked at once (3 s; --timeout up to 10): on a
                         member, its issuer's book of members too
  aterm fleet peers add <host[:port]> | rm <host[:port]|node>
  aterm fleet allow facts|content
  aterm fleet allow drive --from <host|node> [--from …]
                         what a peer may do here: read facts or content, or
                         (drive, granted here alone, never by a join) drive its
                         sessions from the nodes named, nobody else, and
                         every member reads at content; facts|content
                         takes it back
  aterm fleet leave      remove this machine's secret, join code, books and
                         [fleet] listen/allow/drive_from (its node key stays),
                         naming first each tab it holds on another Mac

Start a fleet with `init` on one machine and `join` on each other. --allow is
what a peer may read here: facts (programs, agent states, roles, task records
and models; no text a person or a program wrote) or content (the tab titles,
cwds, branches, prompts, an agent's newest words, and a shell's command line
and last rows as well); without --allow, either keeps the level [fleet] allow
names, else facts (a join into another fleet turns drive into content). --port
is this machine's fleet port: init's is 7100, join's the issuer's (the port its
code names), and either keeps the port [fleet] listen already names; a join
that would listen at the issuer's own address (a loopback fleet's, with no
--port) is refused, writing nothing, and one where another member's listener
proves itself writing nothing of the fleet (a first join's node key stays).
--host is the name the others dial this machine by: $HOSTNAME when set, else
on a Mac the HostName an administrator set, else its Bonjour name
(<LocalHostName>.local), else the OS host name; a member of a loopback fleet,
which listens there alone, at the loopback address. Machines reach each other
on loopback, private (10/8, 172.16/12, 192.168/16) or link-local (169.254/16)
IPv4 addresses only, the listener answering IPv4 alone (0.0.0.0:<port>): `init`
and `join` refuse any other --host, and an IPv6 one but ::1, and `peers add`
any such address and this machine's own listener, writing nothing.
`allow drive` is the level above content: the nodes it names, each proved by
the node key this machine's book binds, run `aterm ctl --host <this Mac> …`
here (spawn agent=<word> and await task; any session's status, text, screen,
result, turn, key, send and hold; close of a worker that node spawned; `aterm
help workers`), and nobody else does; no join and no remote call turns it on.
`init --rotate` mints the issuer a new secret, keeping the host and port its
code names unless --host or --port names others (every member joins again;
the book keeps each until then; no drive grant is kept, here or on a member
that joins again). Every write that leaves the level below drive (allow
facts|content, an init's or a join's --allow) removes every drive grant.
Read the fleet with `aterm ctl survey --fleet` on any machine of it (`--host
<h>` reads one machine): a member asks its issuer's book of members under the
same deadline, so it reads every member, as the issuer does. An issuer running
an older aterm shares no book: the read then says so and reaches only the
peers this machine knows (update aterm on the issuer). See `aterm help fleet`.
";

const PEER_CHECK_WITHIN: Duration = aterm_ctl::FLEET_WITHIN;

const PEER_CHECK_MAX: Duration = Duration::from_secs(10);

const JOIN_WITHIN: Duration = Duration::from_secs(10);

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

const NO_CODE_ARGUMENT: &str = "the join code is never an argument: every process on this \
     machine can read a command line (`ps`) and the shell's history keeps it. Pipe it in \
     (`aterm fleet join < code-file`), name its file (`--code-file F`), or run `aterm fleet \
     join` at a terminal and type it there (it is not shown)";

fn carries_code(word: &str) -> bool {
    word.to_ascii_lowercase().contains(member::CODE_PREFIX)
        || word
            .split(['=', '@'])
            .any(|part| JoinCode::parse(&format!("{}{part}@code:1", member::CODE_PREFIX)).is_ok())
}

fn no_code_in(args: &[String]) -> Result<(), Failure> {
    if args.iter().any(|arg| carries_code(arg)) {
        Err(Failure::Usage(NO_CODE_ARGUMENT.to_owned()))
    } else {
        Ok(())
    }
}

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
                if let Ok(ip) = value.parse::<IpAddr>()
                    && !aterm_net::fleet::is_fleet_source(ip)
                {
                    return Err(Failure::Usage(format!(
                        "--host {ip} is no LAN address (loopback, private, link-local), and a \
                         fleet is one LAN, so no machine of it dials it: name this machine by \
                         its address on this LAN, or by its name; nothing was written"
                    )));
                }
                // [`listen_on`] binds the loopback a loopback fleet names, else `0.0.0.0`.
                if let Ok(IpAddr::V6(ip)) = value.parse::<IpAddr>()
                    && !ip.is_loopback()
                {
                    return Err(Failure::Usage(format!(
                        "--host {ip} is an IPv6 address, and this machine's fleet listener \
                         answers IPv4 alone (0.0.0.0:<port>; a loopback fleet's, its loopback), \
                         so no machine of the fleet dials it there: name this machine by its \
                         IPv4 address on this LAN, or by its name; nothing was written"
                    )));
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
             nodes it names (`aterm fleet allow drive --from <host|node>`), never by an init or \
             a join"
                .to_owned(),
        )
    })
}

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

static UNSHOWN_TTY: std::sync::OnceLock<(i32, libc::termios)> = std::sync::OnceLock::new();

extern "C" fn restore_and_end(signal: libc::c_int) {
    if let Some((fd, saved)) = UNSHOWN_TTY.get() {
        // SAFETY: `fd` is the /dev/tty `read_unshown` holds open while this handler is
        // installed, and `saved` a termios `tcgetattr` filled.
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

fn read_unshown(prompt: &str) -> io::Result<String> {
    use std::os::fd::AsRawFd as _;
    struct Restore {
        fd: i32,
        saved: libc::termios,
        interrupt: Option<libc::sigaction>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: `fd` is the open /dev/tty this function holds for the guard's whole life,
            // and `saved` a termios `tcgetattr` filled.
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
    // SAFETY: an all-zero termios is a valid value of the plain C struct, and `tcgetattr`
    // overwrites it before it is read.
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is open, and `saved` is a writable termios.
    if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let interrupt = UNSHOWN_TTY
        .set((fd, saved))
        .is_ok()
        .then(|| {
            // SAFETY: `sigaction` is plain data for which all-zero is a valid value (an empty
            // mask, no flags).
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
    // SAFETY: `fd` is open, and `quiet` is a termios `tcgetattr` filled, then edited.
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

fn short(node: &str) -> &str {
    node.get(..8).unwrap_or(node)
}

/// Whether hosts `a` and `b` name one machine as a join reads them: the same word, or both the
/// loopback.
fn same_host(a: &str, b: &str) -> bool {
    let loopback = |host: &str| {
        host.eq_ignore_ascii_case("localhost")
            || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    };
    a.eq_ignore_ascii_case(b) || (loopback(a) && loopback(b))
}

fn listen_on(issuer_host: &str, port: u16) -> String {
    match issuer_host.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => member::host_port(&ip.to_string(), port),
        _ => format!("0.0.0.0:{port}"),
    }
}

fn private_state(places: &Places, out: &mut String) -> Result<(), Failure> {
    if let Some(was) = member::ensure_private_dir(&places.state).map_err(failed)? {
        out.push_str(&narrowed(places, was));
    }
    Ok(())
}

/// `verb`'s report once the state directory is private: one open to others is narrowed first,
/// and the line saying so goes under the report's first line, or out before `verb`'s failure
/// ([`say`]).
fn narrowing(
    places: &Places,
    verb: impl FnOnce() -> Result<String, Failure>,
) -> Result<String, Failure> {
    let mut out = String::new();
    private_state(places, &mut out)?;
    let mut report = verb().inspect_err(|_| say(&out))?;
    report.insert_str(report.find('\n').map_or(report.len(), |at| at + 1), &out);
    Ok(report)
}

#[cfg(test)]
thread_local! {
    static SAID: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// What a verb did before it failed, on stdout ahead of the failure on stderr.
fn say(out: &str) {
    #[cfg(test)]
    SAID.with(|said| said.borrow_mut().push_str(out));
    aterm_uds::stdout::print(format_args!("{out}"));
}

fn narrowed(places: &Places, was: u32) -> String {
    format!(
        "  state      {} was open to other users ({was:o}); it is 0700 now\n",
        places.state.display()
    )
}

fn init(places: &Places, args: &[String]) -> Result<String, Failure> {
    let opts = parse_opts(args, &["--allow", "--port", "--host", "--rotate"], false)?;
    // Before any read of the state, so no read a loose directory refuses decides this init.
    narrowing(places, || init_private(places, opts))
}

/// Whether `name` is in the state directory; a read that fails other than for its absence
/// refuses.
fn in_state(places: &Places, name: &str) -> Result<bool, Failure> {
    let path = places.state.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(failed(format!("{}: {e}", path.display()))),
    }
}

/// On an issuer, when it last wrote its join code (its `init`, or its latest `init --rotate`), in
/// ms since the epoch: `None` on a member, which holds none.
fn code_minted(places: &Places) -> Option<u64> {
    let written = std::fs::symlink_metadata(places.state.join(CODE_FILE))
        .and_then(|meta| meta.modified())
        .ok()?;
    let since = written.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(since.as_millis()).ok()
}

/// What [`init`]'s first write did.
enum Begun {
    /// An init again: its report.
    Again(String),
    /// The code and key of a new fleet, or a new secret, written.
    Minted(Minted),
}

struct Minted {
    code: JoinCode,
    written: Held,
    listen: String,
    ungranted: Option<String>,
}

fn init_private(places: &Places, opts: Opts) -> Result<String, Failure> {
    // The node first: its claim of the book's lock waits out another writer, and nothing of the
    // fleet is written yet.
    let keyed = matches!(member::read_node_identity(&places.state), Ok(Some(_)));
    let node = member::node_identity(&places.state)
        .map_err(failed)?
        .node()
        .to_owned();
    let code_path = places.state.join(CODE_FILE);
    at_step("key");
    let begun = member::write_held(&places.state, Began::Here, |book| {
        init_first(places, book, &opts, (&node, keyed))
    })
    .map_err(failed)??;
    let Minted {
        code,
        written,
        listen,
        ungranted,
    } = match begun {
        Begun::Again(report) => return Ok(report),
        Begun::Minted(minted) => minted,
    };
    let old_secret: Vec<Peer> = if opts.rotate {
        member::read_peers(&places.state)
            .map_err(failed)?
            .into_iter()
            .filter(|peer| peer.via == Via::Joined)
            .collect()
    } else {
        Vec::new()
    };
    at_step("config");
    let line = match member::write_held(&places.state, Began::Under(&written), |book| {
        let config = ConfigFile::open(&places.config).map_err(failed)?;
        let now = fleet_table(text_of(&config)?).map_err(failed)?;
        let allow = now.written_level(opts.allow, true);
        write_fleet(
            book,
            &config,
            &places.config,
            (&listen, allow),
            Some(minted_by(&opts)),
        )
    }) {
        Ok(line) => {
            let (line, gone) =
                line.map_err(|failure| unwritten_config(places, &failure_words(failure)))?;
            line + &gone.unwrap_or_default()
        }
        Err(Unwritten::Moved) => {
            return Err(failed(
                "the fleet's key this init wrote is no longer held here: an `aterm fleet leave`, \
                 or a join, came between its steps, so aterm.toml's [fleet] was not written; \
                 `aterm fleet init` makes this machine the issuer of a fleet again",
            ));
        }
        Err(Unwritten::Lock(why)) => return Err(unwritten_config(places, &why)),
    };
    let config_line = ungranted.unwrap_or(line);
    // A new key, a rotate's or a new fleet's (an aterm here may serve a fleet a leave just ended):
    // a member that joins next meets it.
    let served = FleetSecret::new(&code.secret)
        .map(|secret| serves_new_key(places, &secret))
        .unwrap_or_default();
    let mut out = format!(
        "aterm fleet init: this machine ({}, node {}) is the fleet's issuer{}\n",
        aterm_codec::tty::one_line(&code.host, 80),
        short(&node),
        if opts.rotate {
            " under a new secret: every member must join again"
        } else {
            ""
        }
    );
    out.push_str(&format!(
        "  key        {} (0600)\n  join code  {} (0600) — the fleet's secret: never printed, \
         as aterm exports screens\n  node key   {} (0600) — this machine's own: it proves \
         which node it is\n{config_line}{served}",
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
         content to let this fleet read that machine's tab titles, cwds, branches, prompts, \
         an agent's words and a shell's command line and rows: facts, the default, is \
         programs, agent states, roles, task records and models alone\n{FOLLOWS} \
         Members dial {}.\n",
        member::host_port(&code.host, code.port)
    ));
    Ok(out)
}

fn unread_key(places: &Places) -> Failure {
    let error = aterm_net::fleet::read_key_file(&places.state).err();
    let (cause, act) = error.as_ref().map_or_else(
        || (format!("{KEY_FILE} cannot be read"), "run this again"),
        |e| key_fault(places, e),
    );
    failed(format!(
        "{cause} ({}), so this init cannot tell which fleet this machine issues, and wrote \
         nothing: {act}{}",
        places.state.join(KEY_FILE).display(),
        if matches!(error, Some(aterm_net::fleet::KeyFileError::Malformed(_))) {
            ""
        } else {
            "; `aterm fleet leave` leaves the fleet"
        }
    ))
}

fn unwritten_config(places: &Places, why: &str) -> Failure {
    failed(format!(
        "{why}\n  This machine holds the fleet's key and its join code, but aterm.toml's [fleet] \
         was not written, so no member can reach it: `aterm fleet init` again writes it in {}",
        places.config.display()
    ))
}

/// [`init`]'s first write, from what it reads under the book's lock: an issuer's init again, or
/// the code and key of a new fleet (or, `--rotate`, a new secret), with the drive grants a new
/// fleet keeps none of taken out of aterm.toml before them.
fn init_first(
    places: &Places,
    book: &HeldBook<'_>,
    opts: &Opts,
    (node, keyed): (&str, bool),
) -> Result<Begun, Failure> {
    let config = ConfigFile::open(&places.config).map_err(failed)?;
    let table = fleet_table(text_of(&config)?).map_err(failed)?;
    let has_key = in_state(places, KEY_FILE)?;
    let code_path = places.state.join(CODE_FILE);
    let issues = in_state(places, CODE_FILE)?;
    if has_key && !issues {
        return Err(failed(
            "this machine is a member of a fleet it joined; `aterm fleet leave` first, then \
             `init` makes it the issuer of a new one",
        ));
    }
    // The fleet this machine issues is its code beside a key it reads: a leave stopped between
    // the two leaves the code alone.
    if opts.rotate {
        match book.held() {
            Held::Key(_) if issues => {}
            Held::Unreadable => return Err(unread_key(places)),
            Held::Key(_) | Held::Nothing => {
                return Err(Failure::Usage(
                    "`--rotate` mints a new secret for the fleet this machine issues, and it \
                     issues none (no join code, or none beside a fleet's key): `aterm fleet \
                     init` without it makes this machine the issuer of one"
                        .to_owned(),
                ));
            }
        }
    }
    if has_key && !opts.rotate {
        let listen = init_again_listen(places, book, (&config, &table), opts)?;
        return Ok(Begun::Again(format!(
            "aterm fleet init: this machine is already the fleet's issuer; its join code is in \
             {}\n{listen}{}  `--rotate` mints a new secret (every member joins again); `aterm \
             fleet leave` leaves the fleet\n",
            code_path.display(),
            if keyed {
                String::new()
            } else {
                format!(
                    "  node key   {} (0600) minted: each member binds it when it next joins; one \
                     that bound an older key runs `aterm fleet peers rm {}` there first\n",
                    places.state.join(NODE_KEY_FILE).display(),
                    short(node)
                )
            }
        )));
    }
    let issued = if opts.rotate && (opts.host.is_none() || opts.port.is_none()) {
        Some(issued_code(places, &code_path)?)
    } else {
        None
    };
    let host = match (opts.host.clone(), &issued) {
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
    let listen = listen_on(&host, port);
    // A new fleet keeps no drive grants (they named nodes of the fleet a leave stopped short of
    // aterm.toml left), and a new secret none either: a rotate is the remedy for a lost machine.
    // They go before the new key, so an init stopped here has written nothing of the fleet, and
    // the init again takes them out.
    let ungranted = if table.drive_from.is_empty() {
        None
    } else {
        let allow = table.written_level(opts.allow, true);
        let (line, gone) = write_fleet(
            book,
            &config,
            &places.config,
            (&listen, allow),
            Some(minted_by(opts)),
        )?;
        Some(line + &gone.unwrap_or_default())
    };
    let secret = member::mint_secret().map_err(failed)?;
    let written = FleetSecret::new(&secret).map_err(failed)?;
    let code = JoinCode { secret, host, port };
    book.write_code(&code)
        .map_err(|e| failed(format!("{CODE_FILE}: {e}")))?;
    at_step("code");
    book.write_key(&secret).map_err(|e| {
        failed(format!(
            "{KEY_FILE}: {e}\n  {}",
            if opts.rotate {
                "The join code holds the new secret and the key the old one, so no member can \
                 join with that code: `aterm fleet init --rotate` again mints both"
            } else {
                "The join code is written and the key is not, so this machine issues no fleet: \
                 `aterm fleet init` again mints both"
            }
        ))
    })?;
    Ok(Begun::Minted(Minted {
        code,
        written: Held::Key(written),
        listen,
        ungranted,
    }))
}

/// Why an init that mints takes the drive grants out: a new fleet, or a new secret.
const fn minted_by(opts: &Opts) -> Ungrant {
    if opts.rotate {
        Ungrant::NewSecret
    } else {
        Ungrant::OtherFleet
    }
}

/// An init again's `[fleet] listen`, written where aterm.toml has none on the port its join code names (an init or a rotate stopped before aterm.toml), at the level `--allow` names as the stopped init would, and the line saying so; refused, with nothing written, where the key cannot be read, the code is of another secret (a rotate stopped between its two writes) or cannot be read beside a flag, `--host` or `--port` names another than the code does, or `--allow` another level than `[fleet] allow` with nothing to write.
fn init_again_listen(
    places: &Places,
    book: &HeldBook<'_>,
    (config, table): (&ConfigFile, &FleetTable),
    opts: &Opts,
) -> Result<String, Failure> {
    let asked = opts.port.is_some() || opts.host.is_some() || opts.allow.is_some();
    let Held::Key(key) = book.held() else {
        return Err(unread_key(places));
    };
    let code = match member::read_join_code(&places.state) {
        Ok(code) => code,
        Err(why) if asked => {
            return Err(Failure::Usage(format!(
                "this machine is already the fleet's issuer, and its join code cannot be read \
                 ({why}), so this init cannot tell what --host, --port or --allow would change: \
                 `aterm fleet init --rotate --host <h> --port <n>` writes a new code (every \
                 member joins again), and `aterm fleet allow` sets the level"
            )));
        }
        Err(_) if table.listen.is_some() => return Ok(String::new()),
        Err(why) => {
            return Ok(format!(
                "  config     aterm.toml has no [fleet] listen, and the join code cannot be read \
                 ({why}): `aterm fleet init --rotate --host <h> --port <n>` writes both\n"
            ));
        }
    };
    if FleetSecret::new(&code.secret).ok().as_ref() != Some(key) {
        return Err(failed(format!(
            "the join code in {} is of another secret than the fleet's key in {} (as an `aterm \
             fleet init --rotate` stopped between its writes of the two leaves them), so no \
             member can join with that code, and this init wrote nothing: `aterm fleet init \
             --rotate` again mints both (every member joins again)",
            places.state.join(CODE_FILE).display(),
            places.state.join(KEY_FILE).display()
        )));
    }
    if opts.host.as_ref().is_some_and(|host| *host != code.host)
        || opts.port.is_some_and(|port| port != code.port)
    {
        return Err(Failure::Usage(format!(
            "this machine is already the fleet's issuer, and its join code names {}: an init \
             again writes [fleet] listen on that port, and another host or port takes \
             `--rotate` (every member joins again)",
            member::host_port(&code.host, code.port)
        )));
    }
    if table.listen_port() == Some(code.port) {
        // The code's own host and port, and the level [fleet] allow names, change nothing.
        return match opts.allow {
            Some(level) if table.level() != Some(level.word()) => Err(Failure::Usage(format!(
                "this machine is already the fleet's issuer: a level is set by `aterm fleet \
                 allow {}`, not `init`",
                level.word()
            ))),
            _ => Ok(String::new()),
        };
    }
    let line = write_fleet(
        book,
        config,
        &places.config,
        (
            &listen_on(&code.host, code.port),
            table.written_level(opts.allow, false),
        ),
        None,
    )?;
    Ok(format!(
        "{}             written: aterm.toml had no [fleet] listen on the port its join code \
         names (an init stopped before it), so no member could reach this machine\n{}",
        line.0,
        line.1.unwrap_or_default()
    ))
}

/// Whether the issuer `node` a join reached, proving `key`, is the one the book names under the
/// key held: its `init --rotate`, or its `leave` and `init` again.
fn renews(places: &Places, node: &str, key: Option<NodeKey>) -> bool {
    key.is_some()
        && member::read_peers(&places.state).is_ok_and(|book| {
            book.iter().any(|peer| {
                peer.via == Via::Issuer && peer.node.as_deref() == Some(node) && peer.key == key
            })
        })
}

/// A test's hook at each step a kill, or another verb, could come before: [`join`]'s (`lock` once it dialled, `key`, `drop`, `config`, `book`), [`init`]'s first write (`key`), its key's write past its code's (`code`) and its write of aterm.toml (`config`), a grant's write (`grant`) and a level's (`level`), a read's copy of the issuer's book (`keep`), and [`leave`]'s, under the book's lock, once it removed the fleet's files (`removed`) and once it edited aterm.toml (`unlisten`): a panic stops the verb there as a kill would, and a step reached inside the hook runs none.
#[cfg(test)]
type StepHook = Box<dyn FnMut(&str)>;

#[cfg(test)]
thread_local! {
    static AT_STEP: std::cell::RefCell<Option<StepHook>> =
        const { std::cell::RefCell::new(None) };
}

fn at_step(step: &str) {
    #[cfg(test)]
    AT_STEP.with(|hook| {
        if let Ok(mut hook) = hook.try_borrow_mut()
            && let Some(hook) = hook.as_mut()
        {
            hook(step);
        }
    });
    #[cfg(not(test))]
    let _ = step;
}

const FOLLOWS: &str = "An aterm running here follows [fleet] without a relaunch (`aterm ctl \
                       status` reads fleet=listening); one started later reads it at launch.";

fn issued_code(places: &Places, path: &Path) -> Result<JoinCode, Failure> {
    member::read_join_code(&places.state).map_err(|why| {
        failed(format!(
            "the join code in {} cannot be read ({why}), and it names the host and port \
                 members dial: name both with --host and --port to rotate",
            path.display()
        ))
    })
}

fn join(places: &Places, opts: &Opts, code_text: &str) -> Result<String, Failure> {
    let code = JoinCode::parse(code_text).map_err(failed)?;
    let secret = FleetSecret::new(&code.secret).map_err(failed)?;
    // A member of a loopback fleet listens on the loopback alone ([`listen_on`]): it is dialled
    // there.
    let host = match (opts.host.clone(), code.host.parse::<IpAddr>()) {
        (Some(host), _) => host,
        (None, Ok(ip)) if ip.is_loopback() => ip.to_string(),
        (None, _) => aterm_uds::process::lan_host_name()
            .filter(|host| member::is_host(host))
            .ok_or_else(|| {
                failed(
                    "this machine's host name is no host the issuer can dial it by; name one \
                     with --host",
                )
            })?,
    };
    let config = ConfigFile::open(&places.config).map_err(failed)?;
    let table = fleet_table(text_of(&config)?).map_err(failed)?;
    let port = opts
        .port
        .or_else(|| table.listen_port())
        .unwrap_or(code.port);
    // An issuer's own code is refused as its own fleet ([`not_an_issuer`]).
    let issues = !matches!(
        std::fs::symlink_metadata(places.state.join(CODE_FILE)),
        Err(e) if e.kind() == io::ErrorKind::NotFound
    );
    if !issues && port == code.port && same_host(&host, &code.host) {
        let at = member::host_port(&host, port);
        return Err(Failure::Usage(match opts.port {
            Some(named) => format!(
                "this machine would register and listen at {at}, the issuer's own address (the \
                 code names it): `--port {named}` is the issuer's own port there, so name \
                 another with `--port <n>`; nothing was written here"
            ),
            None => format!(
                "this machine would register and listen at {at}, the issuer's own address (the \
                 code names it): a member listens on a port of its own there, so name one with \
                 `--port <n>`; nothing was written here"
            ),
        }));
    }
    let mut out = String::new();
    // Before any read of the state, so no read a loose directory refuses decides this join.
    private_state(places, &mut out)?;
    let bound = not_an_issuer(places, &code, &secret)
        .and_then(|()| bound_issuer(places, &secret).map_err(failed))
        .inspect_err(|_| say(&out))?;
    // A join of another fleet, of one a join left unfinished, or held to no issuer keeps no
    // drive grants.
    let regrant = aterm_net::fleet::read_key_file(&places.state).ok().as_ref() != Some(&secret)
        || !names_issuer(places);
    if let Err(why) = with_fleet(
        text_of(&config)?,
        &listen_on(&code.host, port),
        table.written_level(opts.allow, regrant),
        regrant,
    ) {
        say(&out);
        return Err(failed(why));
    }
    let listened = table.listen_port().is_some();
    // The node key is minted before the dial, which proves it: it is the machine's own, kept
    // whatever the join comes to.
    let minted = matches!(member::read_node_identity(&places.state), Ok(None));
    let me = member::node_identity(&places.state)
        .map_err(failed)
        .inspect_err(|_| say(&out))?;
    let issuer_at = member::host_port(&code.host, code.port);
    let registration = Registration {
        node: me.node().to_owned(),
        host,
        port,
        version: aterm_types::version::app_version().to_owned(),
        key: Some(me.key()),
    };
    let keys = Keyring::read(&places.state);
    let (joined, at, issuer_key) = match register_with(
        &code,
        (&secret, &me),
        (&keys, bound.as_ref()),
        &registration,
    ) {
        Ok(registered) => registered,
        Err(why) => {
            say(&out);
            return Err(failed(not_recorded(
                &issuer_at,
                &why,
                (&places.state, minted),
            )));
        }
    };
    // Read before the key's write, as a leave reads it: holding another fleet's key, this machine
    // reaches none of the old fleet's Macs to hand a tab back. A rotate's fleet is the same Macs.
    let stranded = if matches!(
        aterm_net::fleet::read_key_file(&places.state),
        Ok(held) if held != secret
    ) && !renews(places, &joined.issuer.node, issuer_key)
    {
        held_lines(
            places,
            "now that it holds another fleet's key",
            Some("joins that fleet again"),
        )
    } else {
        String::new()
    };
    at_step("lock");
    let not_keyed = "the fleet's key was not written, and the book is as it was";
    // The join's first write, from what it reads again under the book's lock (a leave, an init
    // or a join may have come while it dialled). The marking, the key's write and the drop are
    // its one take, so no other edit comes between them (an older aterm's write drops every
    // mark).
    let Keyed {
        same_fleet,
        named,
        replaced,
        holds,
    } = member::write_held(&places.state, Began::Here, |book| {
        let held = book.held().clone();
        let same_fleet = held.secret() == Some(&secret);
        // The checks before the dial, made again: an init that came meanwhile makes this machine
        // an issuer, and a join of this fleet binds the issuer this join must have reached.
        if not_an_issuer(places, &code, &secret).is_err() {
            say(&out);
            return Err(failed(format!(
                "this machine came to issue a fleet while the join dialled (an `aterm fleet \
                 init` here wrote {}), so nothing of the code's fleet was written here: `aterm \
                 fleet leave` first — that fleet's members then hold a secret no issuer keeps — \
                 and join again\n  The issuer at {issuer_at} has recorded this machine (its \
                 `aterm fleet peers` lists it until `aterm fleet peers rm` there).",
                places.state.join(CODE_FILE).display()
            )));
        }
        if same_fleet
            && let Some((node, key)) = bound_issuer(places, &secret)
                .map_err(|why| unfinished(&out, &issuer_at, why, not_keyed))?
            && (node != joined.issuer.node || issuer_key != Some(key))
        {
            say(&out);
            return Err(failed(not_recorded(
                &issuer_at,
                &JoinFailed::OtherIssuer {
                    bound: node,
                    proved: issuer_key.map(|_| joined.issuer.node.clone()),
                },
                (&places.state, minted),
            )));
        }
        // Only a join of the fleet held here leaves the issuer it named.
        let named = same_fleet && names_issuer(places);
        // The issuer this machine binds, proving the key it bound, under another secret: its
        // `init --rotate`, or its `leave` and `init` again (both keep its node and key), which
        // this machine cannot tell apart.
        let renewed = !same_fleet && renews(places, &joined.issuer.node, issuer_key);
        let replaced = match held {
            Held::Nothing => None,
            Held::Key(_) if same_fleet => None,
            Held::Key(_) if renewed => Some(Replaced::Renewed),
            Held::Key(_) => Some(Replaced::Other),
            Held::Unreadable => Some(Replaced::Unusable(
                aterm_net::fleet::read_key_file(&places.state)
                    .err()
                    .map_or_else(String::new, |e| e.to_string()),
            )),
        };
        // The key's one write moves this machine to another fleet: the old fleet's entries are
        // marked as its own first, which no read under the key held sees.
        if !same_fleet {
            book.update(|book| {
                match held.secret().map(FleetSecret::mark) {
                    Some(mark) => {
                        for peer in book.iter_mut() {
                            if peer.via != Via::Manual && peer.fleet.is_none() {
                                peer.fleet = Some(mark);
                            }
                        }
                    }
                    None => book.retain(|peer| peer.via == Via::Manual),
                }
                Ok(())
            })
            .map_err(|why| unfinished(&out, &issuer_at, why, not_keyed))?;
        }
        at_step("key");
        book.write_key(&code.secret).map_err(|e| {
            unfinished(
                &out,
                &issuer_at,
                format!("{KEY_FILE}: {e}"),
                if held == Held::Nothing {
                    "the fleet's key was not written, so this machine holds no fleet's key it can \
                     read, as before"
                } else {
                    "the fleet's key was not written: this machine holds the fleet it held, its key \
                     and its book as they were"
                },
            )
        })?;
        let holds = holding(
            listened,
            (same_fleet, named),
            (!same_fleet || !named) && !table.drive_from.is_empty(),
        );
        at_step("drop");
        // The key is written, so this empty edit writes the book without the old issuer, which
        // an older aterm (it takes no mark) would read as the new key's.
        book.update(|_| Ok(()))
            .map_err(|why| unfinished(&out, &issuer_at, why, &holds))?;
        Ok(Keyed {
            same_fleet,
            named,
            replaced,
            holds,
        })
    })
    .map_err(|why| unfinished(&out, &issuer_at, why, not_keyed))??;
    if matches!(replaced, Some(Replaced::Other)) {
        out.push_str(&stranded);
    }
    let held = Held::Key(secret.clone());
    // aterm.toml before the book: until the old grants are out, the book names no issuer to
    // back them.
    at_step("config");
    let (config_line, allow, grants_gone, named) =
        match member::write_held(&places.state, Began::Under(&held), |book| {
            // The book's entry of the issuer, read again: one gone since backs no grant, and
            // aterm.toml as it is now gives the level.
            let named = named && names_issuer(places);
            let ungrant = match &replaced {
                Some(Replaced::Renewed) => Some(Ungrant::NewSecret),
                _ if !same_fleet => Some(Ungrant::OtherFleet),
                _ if !named => Some(Ungrant::Unbacked),
                _ => None,
            };
            let config = ConfigFile::open(&places.config).map_err(failed)?;
            let now = fleet_table(text_of(&config)?).map_err(failed)?;
            let allow = now.written_level(opts.allow, ungrant.is_some());
            let (line, gone) = write_fleet(
                book,
                &config,
                &places.config,
                (&listen_on(&code.host, port), allow),
                ungrant,
            )?;
            let grants_gone = gone.is_some();
            Ok((line + &gone.unwrap_or_default(), allow, grants_gone, named))
        }) {
            Ok(Ok(written)) => written,
            Ok(Err(failure)) => {
                return Err(unfinished(&out, &issuer_at, failure_words(failure), &holds));
            }
            Err(Unwritten::Moved) => return Err(left_between(&out, &issuer_at)),
            Err(Unwritten::Lock(why)) => return Err(unfinished(&out, &issuer_at, why, &holds)),
        };
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
        fleet: Some(secret.mark()),
    };
    at_step("book");
    let recorded = member::write_held(&places.state, Began::Under(&held), |book| {
        book.update(|book| {
            // aterm.toml kept the grants beside the book's entry of the issuer: one gone since (a
            // leave and a join stopped short came between) backs them no more.
            if named && !book.iter().any(|peer| peer.via == Via::Issuer) {
                return Ok(false);
            }
            book.retain(|peer| peer.via != Via::Issuer);
            member::record(book, issuer).map(|_| true)
        })
    });
    let recorded = match recorded {
        Ok(recorded) => recorded.map_err(Unwritten::Lock),
        Err(unwritten) => Err(unwritten),
    };
    match recorded {
        Ok(true) => {}
        Ok(false) | Err(Unwritten::Moved) => return Err(left_between(&out, &issuer_at)),
        Err(Unwritten::Lock(why)) => {
            return Err(unfinished(
                &format!("{out}{config_line}"),
                &issuer_at,
                why,
                &format!(
                    "the fleet's key is held here and aterm.toml listens for the fleet{}, but {}",
                    if grants_gone {
                        " with its drive grants taken out (no Mac drives this one)"
                    } else {
                        ""
                    },
                    if named {
                        "the book's entry of the issuer is as it was"
                    } else {
                        "the book does not name the issuer"
                    }
                ),
            ));
        }
    }
    out.insert_str(
        0,
        &format!(
            "aterm fleet join: this machine (node {}) holds the fleet's key{}\n  key        {} \
             (0600)\n  node key   {} (0600) — this machine's own, which the issuer bound its \
             node to\n{config_line}",
            short(&registration.node),
            match &replaced {
                None => String::new(),
                Some(Replaced::Other) => ", in place of another fleet's key it held".to_owned(),
                Some(Replaced::Renewed) =>
                    ", in place of the secret its issuer held before".to_owned(),
                Some(Replaced::Unusable(why)) if why.contains(ANOTHER_LINK) => format!(
                    ", in a new file in place of one it could not use ({why}): that other link \
                     still holds a copy of the secret the old file held, so remove it"
                ),
                Some(Replaced::Unusable(why)) => {
                    format!(", in place of a key file it could not use ({why})")
                }
            },
            places.state.join(KEY_FILE).display(),
            places.state.join(NODE_KEY_FILE).display(),
        ),
    );
    // The key a join wrote, where an aterm here may still serve another (one it replaced, or one a
    // leave just removed): a reader that comes next meets the new one.
    out.push_str(&serves_new_key(places, &secret));
    if allow == FleetScope::Facts.word() {
        out.push_str(
            "  allow      facts: the fleet reads programs, agent states, roles, task records and \
             models here, no tab title, cwd, prompt, agent's words or shell's command line; \
             `aterm fleet allow content` lets it\n",
        );
    }
    out.push_str(&format!(
        "  issuer     {} at {issuer_at} recorded this machine as {} ({} peer{} in its book){}\n\
         {FOLLOWS}\nOn a Mac, the local-network consent macOS asks for at aterm's first \
         connection into the LAN covers the survey's reads too.\n",
        aterm_codec::tty::one_line(&joined.issuer.host, 80),
        member::host_port(&registration.host, registration.port),
        joined.peers,
        if joined.peers == 1 { "" } else { "s" },
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

/// What a [`join`]'s key write found and did.
struct Keyed {
    /// The key held was the code's fleet's.
    same_fleet: bool,
    /// And the book named its issuer.
    named: bool,
    /// The key the write replaced, when it was no key of the code's fleet.
    replaced: Option<Replaced>,
    /// What a join stopped after the key holds ([`unfinished`]).
    holds: String,
}

/// The key file a join's write replaced ([`Keyed`]).
enum Replaced {
    /// Another fleet's key.
    Other,
    /// The secret its issuer held before, the issuer this machine binds proving the key it bound.
    Renewed,
    /// A key file it could not use, for this reason.
    Unusable(String),
}

/// What a join stopped after its key's write holds: aterm.toml as `listened` had it (beside the
/// fleet held, `same_fleet`), and the book's entry of the issuer (`named`) beside the grants
/// aterm.toml keeps (`grants`).
fn holding(listened: bool, (same_fleet, named): (bool, bool), grants: bool) -> String {
    let config = match (listened, same_fleet) {
        (false, _) => "aterm.toml has no [fleet] listen, so no aterm here listens for the fleet",
        (true, false) => {
            "aterm.toml's [fleet] is the one this machine had, which an aterm here serves this \
             fleet with"
        }
        (true, true) => "aterm.toml's [fleet] is as it was",
    };
    let book = match (named, grants) {
        (true, _) => "the book's entry of the issuer is as it was",
        (false, true) => {
            "the book does not name the issuer, so the drive grants aterm.toml keeps drive this \
             Mac for no one"
        }
        (false, false) => "the book does not name the issuer",
    };
    format!("the fleet's key is held here, but {config}, and {book}")
}

/// Why a join's registration did not happen ([`register_with`]).
#[derive(Debug)]
enum JoinFailed {
    /// Another node of the fleet proved itself at `place`, where this machine would register.
    PlaceHeld {
        /// The place, `host:port`.
        place: String,
        /// The node that listens there.
        node: String,
    },
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
    /// It was; the machine that answered refused, or answered no join (its words).
    Registration(String),
    /// It was; no answer to the registration came.
    Unanswered(String),
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
            Self::PlaceHeld { place, node } => write!(
                f,
                "node {} of this fleet listens at {place} now (its listener proved it)",
                short(node)
            ),
            Self::Registration(why) | Self::Unanswered(why) => f.write_str(why),
        }
    }
}

/// What a join that failed should do next ([`JoinFailed::advice`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Advice {
    /// Another node listens where this machine would register.
    PlaceHeld,
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
    /// An aterm answered at the code's address under another fleet's secret.
    OtherSecret,
    /// The issuer holds its share of connections from this machine's address already.
    Busy,
    /// The issuer proved itself and gave no answer to the registration.
    Unanswered,
    /// The registration names the issuer's own node: this machine carries its node files.
    IssuersNode,
    /// The issuer left or rotated its fleet while it served the join.
    MovedOn,
    /// The issuer answered and did not record this machine, for the reason it gave.
    Answered,
    /// Anything else, before any proof: the host and port the code names, and an aterm there.
    CheckTheIssuer,
}

impl JoinFailed {
    fn advice(&self) -> Advice {
        match self {
            Self::PlaceHeld { .. } => Advice::PlaceHeld,
            Self::Registration(words) if words.contains(NO_ISSUER) => Advice::NoIssuer,
            Self::Registration(words) if words.contains(member::BEFORE_KEYS) => Advice::BeforeKeys,
            Self::Registration(words) if words.contains(member::REBIND) => Advice::NodeBound,
            Self::Registration(words) if words.contains(member::OWN_NODE) => Advice::IssuersNode,
            Self::Registration(words) if words.contains(member::MOVED_ON) => Advice::MovedOn,
            Self::OtherKey(_) | Self::Unproved(_) => Advice::IssuerKey,
            Self::OtherIssuer { proved: None, .. } => Advice::BoundUnproved,
            Self::OtherIssuer { .. } => Advice::BoundIssuer,
            Self::Dial(Unreached::NotFleet(_)) => Advice::PastTheLan,
            Self::Dial(e) if e.not_permitted() => Advice::Sandbox,
            Self::Dial(e) if e.no_route() => Advice::LocalNetwork,
            Self::Dial(e) if e.no_network() => Advice::NoNetwork,
            Self::Dial(Unreached::Handshake(_, FleetError::ListenerWrongSecret)) => {
                Advice::OtherSecret
            }
            Self::Dial(Unreached::Handshake(_, FleetError::AddressBusy)) => Advice::Busy,
            Self::Unanswered(_) => Advice::Unanswered,
            Self::Registration(_) => Advice::Answered,
            Self::Dial(e) if !e.before_proof() => Advice::Answered,
            Self::Dial(_) => Advice::CheckTheIssuer,
        }
    }
}

fn not_recorded(issuer_at: &str, why: &JoinFailed, (state, minted): (&Path, bool)) -> String {
    let nothing = if minted {
        format!(
            "Nothing of the fleet was written here; this machine's node key ({NODE_FILE}, \
             {NODE_KEY_FILE}), minted for the join, is kept: it is the machine's own, which any \
             join reuses."
        )
    } else {
        "Nothing of the fleet was written here.".to_owned()
    };
    let nothing = nothing.as_str();
    match why.advice() {
        Advice::PlaceHeld => {
            let host = match why {
                JoinFailed::PlaceHeld { place, .. } => member::parse_host_port(place, None)
                    .map(|(host, _)| host)
                    .unwrap_or_default(),
                _ => String::new(),
            };
            // A port is this machine's to change only at a place that is its own; at another
            // machine's address the host is wrong, and a name it cannot place may be either.
            if this_machine_is(&host) {
                format!(
                    "this machine would register at a place another node of this fleet holds: \
                     {why}, so this machine's listener could never bind there\n  {nothing} A \
                     member listens at a place of its own: name a free port with `--port <n>`, \
                     and join again."
                )
            } else if host.parse::<IpAddr>().is_ok() {
                format!(
                    "this machine would register at a place another node of this fleet holds: \
                     {why}, and {} is no address of this machine, so the fleet would dial that \
                     node for it\n  {nothing} A member registers where it listens: name the \
                     address the others dial this machine by with `--host <h>`, and join again.",
                    aterm_codec::tty::one_line(&host, 60)
                )
            } else {
                format!(
                    "this machine would register at a place another node of this fleet holds: \
                     {why}, so this machine's listener could never bind there\n  {nothing} A \
                     member listens at a place of its own: name a free port with `--port <n>` \
                     (or with `--host <h>` the name the others dial this machine by, where that \
                     name is another machine's), and join again."
                )
            }
        }
        Advice::NoIssuer => format!(
            "this machine dialled {issuer_at}, a member of that fleet and not its issuer: it \
             holds no join code, so it records no members\n  {nothing} A join code comes from \
             the fleet's issuer, in the file its `aterm fleet init` names (`aterm fleet peers` on \
             {issuer_at} names that issuer): copy the code from there, and join again."
        ),
        Advice::PastTheLan => format!(
            "the issuer at {issuer_at} was not dialled: {why}\n  {nothing} A fleet's machines \
             reach each other on loopback, private (10/8, 172.16/12, 192.168/16) or link-local \
             (169.254/16) IPv4 addresses alone: on the issuer, `aterm fleet init --rotate --host \
             <its LAN address>` writes a code that names one."
        ),
        Advice::Sandbox => format!(
            "the issuer at {issuer_at} was not reached: {why}\n  {nothing} Run `aterm fleet join` \
             where it may open network connections (outside the sandbox), and join again."
        ),
        Advice::LocalNetwork => format!(
            "the issuer at {issuer_at} was not reached: {why}\n  {nothing} On a Mac that is \
             local-network privacy, which refuses an app's connections into the LAN until they \
             are allowed (System Settings › Privacy & Security › Local Network: aterm, or the \
             terminal this runs in); allow it, then join again."
        ),
        Advice::NoNetwork => format!(
            "the issuer at {issuer_at} was not reached: {why}\n  {nothing} This machine is not \
             on the issuer's network: join again once it is on the LAN (Wi-Fi or Ethernet), with \
             any full-tunnel VPN off."
        ),
        Advice::IssuerKey => format!(
            "the issuer at {issuer_at} is not the one this machine binds: {why}\n  {nothing} \
             Another machine answers at that host, or the issuer lost its node key (a \
             reinstall) or runs an older aterm (update it). If it is the issuer, `aterm fleet \
             peers rm <its node>` here takes the old binding out; then join again."
        ),
        Advice::BoundIssuer => format!(
            "the issuer at {issuer_at} is not the one this machine binds: {why}\n  {nothing} \
             This machine joined this fleet before, and a join again is answered by the issuer \
             it joined, under the node key it bound then. To move this machine to a new issuer \
             on purpose, run `aterm fleet leave` here, then `aterm fleet join` with the code."
        ),
        Advice::BoundUnproved => format!(
            "the issuer at {issuer_at} is not the one this machine binds: {why}\n  {nothing} \
             This machine joined this fleet before, and a join again is answered by the issuer \
             it joined, under the node key it bound then. If it is that issuer, it runs an \
             older aterm (update it there) or lost its node key (a reinstall: `aterm fleet \
             init` there mints one, and `aterm fleet peers rm <its node>` here then takes the \
             old binding out); join again after. To move this machine to a new issuer on \
             purpose, run `aterm fleet leave` here, then `aterm fleet join` with the code."
        ),
        Advice::NodeBound => format!(
            "the issuer at {issuer_at} did not record this machine: {why}\n  {nothing} The \
             issuer holds this machine's node id to a key this machine did not prove: a machine \
             that lost its node key, or another one using its node id. If this is that machine, \
             a person at the issuer takes its entry out as it says."
        ),
        Advice::BeforeKeys => format!(
            "the issuer at {issuer_at} did not record this machine: {why}\n  {nothing} The \
             issuer recorded this machine's node from another address before node keys came, \
             and binds its key only from there. If this machine moved since (a new address), \
             a person at the issuer takes its entry out as it says."
        ),
        Advice::OtherSecret => format!(
            "the issuer at {issuer_at} did not record this machine: {why}\n  {nothing} An aterm \
             answers there under another fleet's secret than the code's: the issuer rotated its \
             fleet since this code was written (`aterm fleet init --rotate`, after which every \
             member joins with the new code), or another fleet's Mac took that address. Copy the \
             issuer's current code (the file its `aterm fleet init` names), and join again."
        ),
        Advice::Busy => format!(
            "the issuer at {issuer_at} did not take the join now: {why}\n  {nothing} It holds as \
             many connections from this machine's address as it takes (other reads or joins from \
             here in flight): join again once they end."
        ),
        Advice::Unanswered => format!(
            "the issuer at {issuer_at} gave no answer to the registration: {why}\n  {nothing} \
             Its handshake at the code's address passed, so the code is right; then no answer \
             came: its aterm stopped or restarted its fleet listener mid-join, it is overloaded, \
             or this machine's socket failed (as said). If the registration reached it, it may \
             have recorded this machine: `aterm fleet join` again with the same code finishes \
             either way."
        ),
        Advice::IssuersNode => format!(
            "the issuer at {issuer_at} did not record this machine: {why}\n  {nothing} This \
             machine carries the issuer's node ({}, {}): no member joins under its issuer's node, \
             and nothing on the issuer changes that. Remove those two files here, and a join \
             mints this machine a node of its own; then join again.",
            state.join(NODE_FILE).display(),
            state.join(NODE_KEY_FILE).display()
        ),
        Advice::MovedOn => format!(
            "the issuer at {issuer_at} did not record this machine: {why}\n  {nothing} If it \
             rotated (`aterm fleet init --rotate`), copy the code it issues now (the file its \
             `aterm fleet init` names); if it left (`aterm fleet leave`), it issues none until \
             `aterm fleet init` runs there again: copy the code that writes. Then join with it."
        ),
        Advice::Answered => format!(
            "the issuer at {issuer_at} did not record this machine: {why}\n  {nothing} The \
             issuer answered, so the code's address and its aterm are right: what it said is the \
             cause. Once that is put right on the issuer (`aterm fleet peers` there lists its \
             book), join again."
        ),
        Advice::CheckTheIssuer => {
            let loopback = member::parse_host_port(issuer_at, None)
                .is_ok_and(|(host, _)| same_host(&host, "localhost"));
            format!(
                "the issuer at {issuer_at} did not record this machine: {why}\n  {nothing} \
                 Check the host and port the code names, start aterm on the issuer, and join \
                 again.{}",
                if loopback {
                    ""
                } else {
                    "\n  On a Mac, a first connection into the LAN asks to let aterm find \
                     devices on the local network: allow it, then join again."
                }
            )
        }
    }
}

/// Whether the book, read as the fleet whose key is held, names an issuer.
fn names_issuer(places: &Places) -> bool {
    member::read_peers(&places.state)
        .is_ok_and(|book| book.iter().any(|peer| peer.via == Via::Issuer))
}

fn unfinished(out: &str, issuer_at: &str, why: impl std::fmt::Display, holds: &str) -> Failure {
    say(out);
    failed(format!(
        "{why}\n  The issuer at {issuer_at} has recorded this machine (its `aterm fleet peers` \
         lists it), but the join did not finish here: {holds}. Run `aterm fleet join` again \
         with the same code to finish it."
    ))
}

fn left_between(out: &str, issuer_at: &str) -> Failure {
    say(out);
    failed(format!(
        "the fleet's key this join wrote is no longer held here, or the book names its issuer no \
         more: an `aterm fleet leave`, or a join of another code, came between its steps, so \
         nothing more of the fleet was recorded here\n  The issuer at {issuer_at} has recorded this machine (its `aterm \
         fleet peers` lists it until `aterm fleet peers rm` there); `aterm fleet join` with \
         the same code joins it again."
    ))
}

const NO_ISSUER: &str = "is no fleet's issuer";

fn not_an_issuer(places: &Places, code: &JoinCode, secret: &FleetSecret) -> Result<(), Failure> {
    let path = places.state.join(CODE_FILE);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        _ => {}
    }
    let own = member::read_join_code(&places.state);
    // init and `--rotate` write the key with the code's secret, so either names the fleet this
    // machine issues.
    let same = own.as_ref().is_ok_and(|own| own.secret == code.secret)
        || aterm_net::fleet::read_key_file(&places.state).ok().as_ref() == Some(secret);
    Err(failed(match own {
        _ if same => {
            "this machine is the issuer of the fleet that code is for: it does not join its \
             own fleet (`aterm fleet peers` lists its members)"
                .to_owned()
        }
        Ok(_) => format!(
            "this machine is the issuer of a fleet (its join code is {}): `aterm fleet leave` \
             first — that fleet's members then hold a secret no issuer keeps — and join again",
            path.display()
        ),
        Err(why) => format!(
            "this machine is the issuer of a fleet, and its join code {} cannot be read ({why}), \
             so this join cannot tell whether that code is its own fleet's: it dialled nothing; \
             once that file reads (0600, one link), join again",
            path.display()
        ),
    }))
}

/// The issuer a join of `secret`'s fleet must reach: the one the book read as that fleet's binds
/// ([`member::read_peers_as`]) while that fleet's key is held, or marks as that fleet's
/// (`fleet=`) while the key cannot be read; none with no key or another fleet's.
/// # Errors
/// The book cannot be read, or the key cannot be read beside an issuer's entry that names no
/// fleet (an older aterm's), so the join cannot tell whether it holds the code's fleet.
fn bound_issuer(
    places: &Places,
    secret: &FleetSecret,
) -> Result<Option<(String, NodeKey)>, String> {
    let book = member::read_peers_as(&places.state, secret.mark()).map_err(|e| {
        format!(
            "{e}: a join reads the book before it dials the issuer, so it dialled nothing and \
             nothing of the fleet was written here; once {} reads (0600, one link, a peer on \
             each line), or after `aterm fleet leave`, join again",
            places.state.join(PEERS_FILE).display()
        )
    })?;
    let Some(issuer) = book.into_iter().find(|peer| peer.via == Via::Issuer) else {
        return Ok(None);
    };
    let Some(bound) = issuer.node.zip(issuer.key) else {
        return Ok(None);
    };
    match aterm_net::fleet::read_key_file(&places.state) {
        Ok(held) if held == *secret => Ok(Some(bound)),
        Ok(_) | Err(aterm_net::fleet::KeyFileError::Missing) => Ok(None),
        Err(_) if issuer.fleet.is_some() => Ok(Some(bound)),
        Err(e) => {
            let (cause, act) = key_fault(places, &e);
            Err(format!(
                "{cause} ({}), and the book's issuer (node {}) names no fleet, so this join \
                 cannot tell whether the code is the fleet this machine holds, whose issuer a \
                 join again must reach: it dialled nothing, and nothing of the fleet was written \
                 here: {act}{}",
                places.state.join(KEY_FILE).display(),
                short(&bound.0),
                if matches!(e, aterm_net::fleet::KeyFileError::Malformed(_)) {
                    ""
                } else {
                    ", or `aterm fleet leave` and join again"
                }
            ))
        }
    }
}

fn register_with(
    code: &JoinCode,
    (secret, me): (&FleetSecret, &NodeIdentity),
    (keys, bound): (&Keyring, Option<&(String, NodeKey)>),
    registration: &Registration,
) -> Result<(member::Joined, IpAddr, Option<NodeKey>), JoinFailed> {
    // Only another node's proof there is evidence the place is taken: nothing answering, another
    // secret or this machine's own listener registers as before.
    if let Ok(there) = member::dial(
        &registration.host,
        None,
        registration.port,
        (secret, Some(me)),
        FleetOp::Book,
        PEER_CHECK_WITHIN,
    ) && let Some(other) = there.peer.filter(|proof| proof.node != me.node())
    {
        return Err(JoinFailed::PlaceHeld {
            place: member::host_port(&registration.host, registration.port),
            node: other.node,
        });
    }
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
    let joined =
        member::register_by(&mut reached, registration, deadline).map_err(|e| match e {
            member::Unregistered::Answered(why) => JoinFailed::Registration(why),
            member::Unregistered::Unanswered(why) => JoinFailed::Unanswered(why),
        })?;
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

fn peers(places: &Places, args: &[String]) -> Result<String, Failure> {
    match args {
        [] => narrowing(places, || list_peers(places, PEER_CHECK_WITHIN)),
        [flag, secs] if flag.as_str() == "--timeout" => {
            let within = check_within(secs)?;
            narrowing(places, || list_peers(places, within))
        }
        [add, target] if add.as_str() == "add" => {
            let (host, port) = member::parse_host_port(target, Some(member::DEFAULT_PORT))
                .map_err(|why| Failure::Usage(format!("add <host[:port]>: {why}")))?;
            let peer = manual_peer(host, port);
            let target = peer.target();
            unread_place(places, &peer)?;
            narrowing(places, || {
                let recorded =
                    member::update_peers(&places.state, |book| member::record(book, peer))
                        .map_err(failed)?;
                Ok(match recorded {
                    Recorded::Already => {
                        format!("aterm fleet peers: {target} is in the book already\n")
                    }
                    _ => format!(
                        "aterm fleet peers: added {target} to {}\n",
                        places.state.join(PEERS_FILE).display()
                    ),
                })
            })
        }
        [rm, what] if rm.as_str() == "rm" => narrowing(places, || {
            member::write_held(&places.state, Began::Here, |book| {
                remove_peer(places, book, what)
            })
            .map_err(failed)?
        }),
        _ => Err(Failure::Usage(
            "peers takes nothing, `--timeout <s>`, `add <host[:port]>` or `rm <host[:port]|node>`"
                .to_owned(),
        )),
    }
}

/// `peers add` of a place no read would take as a peer, refused before anything is written: an
/// address no fleet read dials, or this machine's own listener as a look tells it without a dial.
fn unread_place(places: &Places, peer: &Peer) -> Result<(), Failure> {
    let target = peer.target();
    if let Some(ip) = peer.addr
        && !aterm_net::fleet::is_fleet_source(ip)
    {
        return Err(Failure::Usage(format!(
            "add {target}: {ip} is no LAN address (loopback, private, link-local), and a fleet \
             is one LAN, so no read dials it: name the peer by its address on this LAN; \
             nothing was written"
        )));
    }
    // A fleet listener answers IPv4 alone ([`listen_on`]), as `init` and `join` refuse such a --host.
    if let Some(IpAddr::V6(ip)) = peer.addr
        && !ip.is_loopback()
    {
        return Err(Failure::Usage(format!(
            "add {target}: {ip} is an IPv6 address, and a fleet listener answers IPv4 alone \
             (0.0.0.0:<port>; a loopback fleet's, its loopback), so no read reaches a Mac there: \
             name the peer by its IPv4 address on this LAN, or by its name; nothing was written"
        )));
    }
    let config = ConfigFile::open(&places.config).map_err(failed)?;
    let table = fleet_table(text_of(&config)?).map_err(failed)?;
    if table.listen_port() == Some(peer.port) && this_machine_is(&peer.host) {
        return Err(Failure::Usage(format!(
            "add {target}: that is this machine's own fleet listener ([fleet] listen = {:?}), \
             which its own `aterm ctl survey` reads, so it is no peer: name another Mac's \
             host:port; nothing was written",
            table.listen.as_deref().unwrap_or_default()
        )));
    }
    Ok(())
}

/// Whether `host` names this machine without a dial: its loopback, an address on one of its
/// interfaces, or its own host name.
fn this_machine_is(host: &str) -> bool {
    if same_host(host, "localhost") {
        return true;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return aterm_uds::network::interfaces().is_ok_and(|up| {
            up.iter()
                .any(|interface| interface.address == ip.to_canonical())
        });
    }
    let bare = |name: &str| {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        name.strip_suffix(".local")
            .map_or(name.clone(), str::to_owned)
    };
    aterm_uds::process::lan_host_name().is_some_and(|own| bare(&own) == bare(host))
}

/// `peers rm`: the entry `what` names out of the book, with the copy of the issuer's book where
/// it is the issuer, and the drive grant of its node out of aterm.toml, in one take of the book's
/// lock.
fn remove_peer(places: &Places, book: &HeldBook<'_>, what: &str) -> Result<String, Failure> {
    let removed = match book.update(|peers| {
        let copies = issuer_copies(places, peers);
        member::remove(peers, &copies, what)
    }) {
        Ok(removed) => removed,
        Err(refused) => {
            return drop_copied_grant(places, book, what)?.ok_or_else(|| {
                let shown = aterm_codec::tty::one_line(what, 80);
                failed(match configured_entry(places, what) {
                    Some(entry) => format!(
                        "`{shown}` is no entry of this machine's book, which `peers rm` edits: {}",
                        in_aterm_toml(places, &entry)
                    ),
                    None => match names_this_machine(places, what)
                        .or_else(|| at_own_place(places, what))
                    {
                        Some(own) => format!(
                            "`{shown}` is this machine itself (node {}), which no book here lists: \
                             `peers rm` takes another Mac out of the book, and `aterm fleet \
                             leave` takes this one out of the fleet",
                            short(&own)
                        ),
                        None => refused,
                    },
                })
            });
        }
    };
    let copy_gone = removed.via == Via::Issuer && book.remove(ISSUER_BOOK_FILE).unwrap_or(false);
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
        && let Some(gone) = drop_grants(places, book, |word| DriveGrant::node_of(word) == *node)?
    {
        out.push_str(&gone.row());
    }
    match removed.via {
        Via::Joined => out.push_str(STILL_READS),
        Via::Issuer => out.push_str(NO_ISSUER_NAMED),
        _ => {}
    }
    Ok(out)
}

/// A member whose book names no issuer reads only the peers named here.
const NO_ISSUER_NAMED: &str = "  issuer     this machine names no issuer now, so it reads only the \
                               peers named here: `aterm fleet join` with the issuer's join code \
                               binds it again\n";

/// The `[[fleet.peers]]` entry `what` names, as `peers` lists it.
fn configured_entry(places: &Places, what: &str) -> Option<String> {
    let config = ConfigFile::open(&places.config).ok()?;
    let table = fleet_table(text_of(&config).ok()?).ok()?;
    table.peers.into_iter().find(|entry| {
        member::parse_host_port(entry, Some(member::DEFAULT_PORT)).is_ok_and(|(host, port)| {
            matches!(member::find(&[manual_peer(host, port)], what), Ok(Some(_)))
        })
    })
}

/// A removed node may keep the fleet's secret, which the book does not take back.
const STILL_READS: &str = "  reads      it may keep the fleet's secret (it does unless it left), and while it \
                           does it reads this fleet's surveys: only `aterm fleet init --rotate` \
                           on the issuer takes that back (every member joins again)\n";

/// `peers rm` of a node only this machine's copy of its issuer's book names.
/// # Errors
/// The book cannot be read, `what` names more than one copied peer, or aterm.toml cannot be
/// edited.
fn drop_copied_grant(
    places: &Places,
    held: &HeldBook<'_>,
    what: &str,
) -> Result<Option<String>, Failure> {
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
    let Some(gone) = drop_grants(places, held, |word| DriveGrant::node_of(word) == node)? else {
        return Ok(None);
    };
    Ok(Some(format!(
        "aterm fleet peers: {} ({}) stays in the fleet issuer's book, which this machine copies \
         (`aterm fleet peers rm` on the issuer takes it out); it drives this Mac no more\n{}",
        peer.target(),
        short(node),
        gone.row()
    )))
}

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

struct Row {
    peer: Peer,
    /// The `[[fleet.peers]]` entry it is from, not the book.
    configured: Option<String>,
    /// What it serves and which node answered, or why it was not reached.
    check: Option<Result<member::Checked, Unreached>>,
}

struct Listing {
    /// Every peer this machine knows, each checked.
    rows: Vec<Row>,
    /// Why nothing was checked, when it was not.
    unchecked: Option<String>,
    /// What the read of the issuer's book left this machine short of ([`book_note`]).
    note: Option<String>,
    /// `[fleet]` as aterm.toml says it once the read's own write is made.
    table: FleetTable,
    /// A line for each `[[fleet.peers]]` entry skipped.
    skipped: Vec<String>,
    /// Where this machine's own node proved itself at its `[fleet] listen` place, or why it did
    /// not ([`own_listener`]); none where it was not asked.
    listener: Option<Result<SocketAddr, Unreached>>,
    /// Why the read brought no book of this machine's issuer's ([`Missed::of_read`]).
    missed: Option<Missed>,
}

/// Every peer this machine knows ([`known_peers`]).
struct Known {
    /// Its book, the `[[fleet.peers]]` it does not hold already (each beside its entry).
    peers: Vec<(Peer, Option<String>)>,
    /// Each `[[fleet.peers]]` entry that is no host.
    skipped: Vec<(String, &'static str)>,
    /// `[fleet]` as aterm.toml says it.
    table: FleetTable,
    /// The keys its book and its whole copy bind ([`Keyring::of`]).
    keys: Keyring,
}

fn known_peers(places: &Places) -> Result<Known, Failure> {
    let config = ConfigFile::open(&places.config).map_err(failed)?;
    let table = fleet_table(text_of(&config)?).map_err(failed)?;
    let mut peers = member::read_peers(&places.state).map_err(failed)?;
    let copies = issuer_copies(places, &peers);
    let mut entries = vec![None; peers.len()];
    let mut skipped = Vec::new();
    for entry in &table.peers {
        match member::parse_host_port(entry, Some(member::DEFAULT_PORT)) {
            Ok((host, port)) => {
                let peer = manual_peer(host, port);
                if !peers.iter().any(|known| known.same_place(&peer)) {
                    peers.push(peer);
                    entries.push(Some(entry.clone()));
                }
            }
            Err(why) => skipped.push((entry.clone(), why)),
        }
    }
    let keys = Keyring::of(peers.iter().chain(&copies));
    member::with_copies(&mut peers, copies);
    let mut entries = entries.into_iter();
    let peers = peers
        .into_iter()
        .map(|peer| (peer, entries.next().flatten()))
        .collect();
    Ok(Known {
        peers,
        skipped,
        table,
        keys,
    })
}

fn issuer_copies(places: &Places, book: &[Peer]) -> Vec<Peer> {
    book.iter()
        .find(|peer| peer.via == Via::Issuer)
        .and_then(|issuer| issuer.node.as_deref())
        .map(|issuer| member::read_issuer_book(&places.state, issuer))
        .unwrap_or_default()
}

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
        fleet: None,
    }
}

fn check_peers(places: &Places, within: Duration) -> Result<Listing, Failure> {
    // The fleet the read is made under, read before the book it reads.
    let held = aterm_net::fleet::read_key_file(&places.state);
    if let Err(e) = &held
        && !matches!(e, aterm_net::fleet::KeyFileError::Missing)
    {
        // The book's entries of a fleet are read under its key: none would show.
        return Err(failed(format!(
            "no peer was checked, since the book is read as the fleet of the key in {}: {}",
            places.state.display(),
            no_key(places, e)
        )));
    }
    let Known {
        peers,
        skipped,
        mut table,
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
    let configured: Vec<(String, String)> = peers
        .iter()
        .filter_map(|(peer, entry)| Some((peer.target(), entry.clone()?)))
        .collect();
    let peers: Vec<Peer> = peers.into_iter().map(|(peer, _)| peer).collect();
    let row = |peer: Peer, check| Row {
        configured: configured
            .iter()
            .find(|(target, _)| peer.via == Via::Manual && *target == peer.target())
            .map(|(_, entry)| entry.clone()),
        peer,
        check,
    };
    let (rows, unchecked, note, (listener, missed)) = match held {
        Ok(secret) => {
            let own = own_node(places);
            let me = own_identity(places);
            let ((reads, book), listener) = std::thread::scope(|scope| {
                let probe = std::thread::Builder::new()
                    .name("aterm-fleet-self".to_owned())
                    .spawn_scoped(scope, || {
                        own_listener(&table, (&secret, me.as_ref()), within)
                    });
                let read = member::read_whole(
                    &peers,
                    &keys,
                    own.as_deref(),
                    (&secret, me.as_ref()),
                    within,
                    "survey facts",
                );
                let listener = match probe {
                    Ok(probe) => probe.join().ok().flatten(),
                    Err(_) => own_listener(&table, (&secret, me.as_ref()), within),
                };
                (read, listener)
            });
            let dropped = keep_book(places, &secret, &book, own.as_deref());
            let issuer_read = reads.iter().find(|read| read.peer.via == Via::Issuer);
            let missed = Missed::of_read(&book, issuer_read.map(|read| &read.read));
            // The read's own write lowered the level or took grants out: the header says the
            // table as it is now.
            if dropped.is_some()
                && let Ok(now) = ConfigFile::open(&places.config)
                    .map_err(failed)
                    .and_then(|config| fleet_table(text_of(&config)?).map_err(failed))
            {
                table = now;
            }
            let rows = reads
                .into_iter()
                .map(|read| {
                    row(
                        read.peer,
                        Some(read.read.map(|surveyed| surveyed.checked())),
                    )
                })
                .collect();
            (
                rows,
                None,
                joined_notes(book_note(&peers, &book), dropped),
                (listener, missed),
            )
        }
        Err(e) => (
            peers.into_iter().map(|peer| row(peer, None)).collect(),
            Some(no_key(places, &e)),
            None,
            (None, None),
        ),
    };
    Ok(Listing {
        rows,
        unchecked,
        note,
        table,
        skipped,
        listener,
        missed,
    })
}

/// Whether this machine's own node proves itself at its `[fleet] listen` place (a wildcard's at
/// the loopback), dialled as a peer dials it, within `within`: the evidence `peers`' header
/// rests a claim that this machine listens there on. None where `[fleet]` names no IP and port or
/// this machine holds no node key to be proved by.
fn own_listener(
    table: &FleetTable,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    within: Duration,
) -> Option<Result<SocketAddr, Unreached>> {
    let listen: SocketAddr = table.listen.as_deref()?.parse().ok()?;
    let me = me?;
    let ip = match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::from(std::net::Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::from(std::net::Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    let target = member::Target {
        host: ip.to_string(),
        addr: Some(ip),
        port: listen.port(),
        node: Some(me.node().to_owned()),
        key: Some(me.key()),
    };
    Some(member::proves_itself(
        &target,
        &Keyring::default(),
        (secret, me),
        within,
    ))
}

/// How long a verb that wrote the key waits for an aterm running here to serve it: its listener
/// reads the key every [`aterm_net::fleet::KEY_READ_EVERY`], and a second more.
const SERVES_NEW_KEY: Duration =
    aterm_net::fleet::KEY_READ_EVERY.saturating_add(Duration::from_secs(1));

/// One look of [`serves_new_key`]'s, and the pause between two: a listener on this machine
/// answers a handshake in milliseconds, and each look it refuses is a line of its log.
const PROBE_MIN: Duration = Duration::from_millis(500);

const PROBE_MAX: Duration = Duration::from_secs(1);

/// Once a verb wrote `secret` as the key an aterm running here serves, wait, at most
/// [`SERVES_NEW_KEY`], while its `[fleet] listen` place answers under another secret, or nothing
/// listens there and an aterm runs here ([`aterm_runs_here`]) to bind it, so a reader or a join
/// that comes next meets the new key; the line saying so where it still does. Nothing is waited
/// for where anything else answers there, or nothing does and no aterm runs here.
fn serves_new_key(places: &Places, secret: &FleetSecret) -> String {
    let table = ConfigFile::open(&places.config)
        .map_err(failed)
        .and_then(|config| fleet_table(text_of(&config)?).map_err(failed));
    let (Some(me), Ok(table)) = (own_identity(places), table) else {
        return String::new();
    };
    let end = Instant::now() + SERVES_NEW_KEY;
    let mut runs = None;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        let unserved = match own_listener(
            &table,
            (secret, Some(&me)),
            left.clamp(PROBE_MIN, PROBE_MAX),
        ) {
            Some(Err(Unreached::Handshake(at, FleetError::ListenerWrongSecret))) => format!(
                "{at} still answers under another secret {} s after the new key was written: an \
                 aterm here that has not read it (`aterm ctl status` here reads fleet=listening, \
                 or refused:<why>), or another fleet's listener holding that port \
                 (fleet=retrying then)",
                SERVES_NEW_KEY.as_secs()
            ),
            Some(Err(Unreached::Connect(at, e)))
                if e.kind() == io::ErrorKind::ConnectionRefused
                    && *runs.get_or_insert_with(aterm_runs_here) =>
            {
                format!(
                    "nothing listens on {at} {} s after the new key was written, though an aterm \
                     runs here: `aterm ctl status` here reads why (fleet=refused:<why>, or off \
                     where that aterm reads another aterm.toml)",
                    SERVES_NEW_KEY.as_secs()
                )
            }
            _ => return String::new(),
        };
        if Instant::now() >= end {
            return format!("  listener   {unserved}\n");
        }
        std::thread::sleep(PROBE_MIN.min(end.saturating_duration_since(Instant::now())));
    }
}

/// A test's answer to whether an aterm runs here ([`aterm_runs_here`]); with none, no aterm does.
#[cfg(test)]
type AtermHere = Box<dyn FnMut() -> bool>;

#[cfg(test)]
thread_local! {
    static ATERM_HERE: std::cell::RefCell<Option<AtermHere>> =
        const { std::cell::RefCell::new(None) };
}

/// Whether an aterm runs here, to serve `[fleet] listen` once it reads it: its control socket
/// answers ([`aterm_ctl::front_door_instance`]).
fn aterm_runs_here() -> bool {
    #[cfg(test)]
    {
        ATERM_HERE.with(|hook| hook.borrow_mut().as_mut().is_some_and(|hook| hook()))
    }
    #[cfg(not(test))]
    {
        aterm_ctl::front_door_instance().is_some()
    }
}

/// Whether a read's answer is this machine's: its own node, proved under its own key.
fn is_this_machine(node: Option<&str>, proven: bool, own: Option<&str>) -> bool {
    proven && own.is_some() && node == own
}

fn own_node(places: &Places) -> Option<String> {
    member::read_node_id(&places.state).ok().flatten()
}

/// This machine's own node, where `word` names it ([`member::names_node`]).
fn names_this_machine(places: &Places, word: &str) -> Option<String> {
    own_node(places).filter(|own| member::names_node(own, word))
}

/// This machine's own node, where `word` names its own listener without a dial: its `[fleet]
/// listen` port at its loopback, an interface address or its host name, as `peers add` refuses
/// that place ([`unread_place`]). A verb asks it once no list it reads names the word.
fn at_own_place(places: &Places, word: &str) -> Option<String> {
    let (host, port) = member::parse_host_port(word, Some(member::DEFAULT_PORT)).ok()?;
    let config = ConfigFile::open(&places.config).ok()?;
    let table = fleet_table(text_of(&config).ok()?).ok()?;
    (table.listen_port() == Some(port) && this_machine_is(&host))
        .then(|| own_node(places))
        .flatten()
}

fn own_identity(places: &Places) -> Option<NodeIdentity> {
    member::read_node_identity(&places.state).ok().flatten()
}

/// What `read`, a read under `secret`, leaves as the copy of the issuer's book, and the grants a
/// proven book backs no more taken out, in one take of the book's lock while that fleet is held
/// still ([`member::write_held`]): a read that ends after a leave, or after a join of another
/// fleet, keeps nothing of the fleet it read.
fn keep_book(
    places: &Places,
    secret: &FleetSecret,
    read: &BookRead,
    own: Option<&str>,
) -> Option<String> {
    let copy = match read {
        BookRead::Fetched(book) => Some((book.node.as_str(), member::copies_of(book, own))),
        BookRead::Refused(_) | BookRead::NoIssuer => None,
        BookRead::Unshared(_) | BookRead::Unread(_) => return None,
    };
    let proven = matches!(read, BookRead::Fetched(book) if book.proven);
    at_step("keep");
    member::write_held(
        &places.state,
        Began::Under(&Held::Key(secret.clone())),
        |book| {
            book.keep_issuer_book(
                copy.as_ref()
                    .map(|(issuer, copies)| (*issuer, copies.as_slice())),
            )
            .ok()?;
            proven.then(|| drop_unbacked_grants(places, book)).flatten()
        },
    )
    .ok()
    .flatten()
}

fn drop_unbacked_grants(places: &Places, held: &HeldBook<'_>) -> Option<String> {
    let keys = Keyring::read(&places.state);
    let gone = drop_grants(places, held, |word| {
        DriveGrant::from_word(word).is_some_and(|grant| keys.bound(&grant.node) != Some(grant.key))
    })
    .ok()??;
    Some(format!(
        "Its issuer's book binds {} this Mac let drive it no more, or to another key, so \
         {}.",
        if gone.picked == 1 {
            "a node".to_owned()
        } else {
            format!("{} nodes", gone.picked)
        },
        gone.clause()
    ))
}

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

fn shown_name(peer: &Peer) -> String {
    if peer.port == member::DEFAULT_PORT {
        peer.host.clone()
    } else {
        member::host_port(&peer.host, peer.port)
    }
}

fn list_peers(places: &Places, within: Duration) -> Result<String, Failure> {
    let listing = check_peers(places, within)?;
    let table = &listing.table;
    let own = own_node(places);
    let mut out = format!(
        "aterm fleet peers: {} known to this machine{}\n",
        listing.rows.len(),
        match (&listing.unchecked, &table.listen) {
            (Some(why), _) => format!(" — not checked: {why}"),
            (None, Some(listen)) => listens_words(places, &listing, listen, own.as_deref()),
            (None, None) => " (it has no [fleet] listen: no peer can read it)".to_owned(),
        }
    );
    out.extend(listing.skipped.iter().cloned());
    // A member that holds the key with no issuer named: its `peers rm` of the issuer, or a join
    // stopped before the book.
    let unnamed = listing.unchecked.is_none()
        && !in_state(places, CODE_FILE).unwrap_or(true)
        && !listing.rows.iter().any(|row| row.peer.via == Via::Issuer);
    if listing.rows.is_empty() {
        out.push_str(&format!("  none: {}\n", reads_no_peer(unnamed)));
        return Ok(out);
    }
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    let listed: Vec<Peer> = listing.rows.iter().map(|row| row.peer.clone()).collect();
    let minted = code_minted(places);
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
            Some(Ok(checked))
                if is_this_machine(checked.node.as_deref(), checked.proven, own.as_deref()) =>
            {
                format!(
                    "this machine itself, no peer: {}",
                    taken_out_by(places, row)
                )
            }
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
                aterm_codec::tty::one_line(
                    &unreached_words(peer, why, &listed, (own.as_deref(), minted)),
                    200,
                )
            }
        };
        out.push_str(&format!(
            "  {:<8}  {:<11}  {:<23}  {:<21}  {:<15}  {:<7}  {:<8}  {check}\n",
            peer.node.as_deref().map_or("-", short),
            if row.configured.is_some() {
                "config"
            } else {
                peer.via.word()
            },
            column(&peer.host, 23),
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
    if unnamed {
        out.push_str(
            "This machine holds the fleet's key and names no issuer, so it reads only the peers \
             named here: `aterm fleet join` with the issuer's join code binds it again.\n",
        );
    }
    Ok(out)
}

/// Why a machine whose book lists no peer reads no other, and what adds one: on a member whose
/// book names no issuer (`unnamed`: its `peers rm`, or a join stopped before the book), a join.
const fn reads_no_peer(unnamed: bool) -> &'static str {
    if unnamed {
        "this machine holds the fleet's key and names no issuer, so it reads no member: `aterm \
         fleet join` with the issuer's join code binds it again; `aterm fleet peers add <host>` \
         names a peer by hand"
    } else {
        "this machine's book lists no peer: a member is recorded when it runs `aterm fleet join`, \
         and `aterm fleet peers add <host>` names one by hand"
    }
}

/// What `peers`' header says of this machine's listener at `listen`, on evidence alone: its own
/// node proved there, another node proved there (and the act that moves it), or, with neither,
/// that it did not prove itself there and what may hold the place; a `listen` that is no
/// IP:port, which no listener takes, is named as that.
fn listens_words(places: &Places, listing: &Listing, listen: &str, own: Option<&str>) -> String {
    let table = &listing.table;
    let shown = aterm_codec::tty::one_line(listen, 60);
    let level = table.level().unwrap_or(FleetScope::Facts.word());
    let Ok(place) = listen.parse::<SocketAddr>() else {
        return format!(
            " ([fleet] listen is {shown}, no numeric IP and port, so no aterm here listens for \
             the fleet (`aterm ctl status` here reads fleet=refused:listen): write it as one, \
             `0.0.0.0:<port>` on a LAN, in {})",
            places.config.display()
        );
    };
    let specific = !place.ip().is_unspecified();
    let other = holder_of_listen(table, &listing.rows, own).or_else(|| match &listing.listener {
        Some(Err(Unreached::OtherNode { said, .. })) if specific && own != Some(said.as_str()) => {
            Some(said.clone())
        }
        _ => None,
    });
    if let Some(node) = other {
        return format!(
            " (its listener is not on {shown}: node {} answers there; {})",
            short(&node),
            member::moves_listener(&places.state)
        );
    }
    match &listing.listener {
        Some(Ok(_)) => format!(" (it listens on {shown}, serving {level})"),
        Some(Err(why)) => {
            let moves = member::moves_listener(&places.state);
            let held = format!("`aterm ctl status` here reads fleet=retrying then, and {moves}");
            let (seen, then) = match why {
                Unreached::Connect(at, e) if e.kind() == io::ErrorKind::ConnectionRefused => (
                    format!("nothing listens on {at}"),
                    "no aterm runs here, or it has not read [fleet] yet (`aterm ctl status` here \
                     says fleet=listening once it has)"
                        .to_owned(),
                ),
                Unreached::Handshake(at, FleetError::ListenerWrongSecret) => (
                    format!("{at} answers under another secret"),
                    format!(
                        "an aterm here that has not read this machine's key, or another fleet's \
                         listener holding that port ({held})"
                    ),
                ),
                Unreached::OtherNode { at, said, .. } => (
                    format!("node {} answers at {at}", short(said)),
                    format!(
                        "its listener holds {at}, so this machine's node is not reached there \
                         ({moves})"
                    ),
                ),
                why => (
                    why.to_string(),
                    format!("no aterm here serves it, or another program holds that port ({held})"),
                ),
            };
            format!(
                " ([fleet] listen is {shown}, serving {level}, but this machine's node did not \
                 prove itself there just now ({}): {then})",
                aterm_codec::tty::one_line(&seen, 160)
            )
        }
        None => format!(
            " ([fleet] listen is {shown}, serving {level}; this machine holds no node key to \
             prove itself there, so `aterm ctl status` says whether it listens)"
        ),
    }
}

/// `text` on one line, cut so that it and its `…` fit a column `width` characters wide.
fn column(text: &str, width: usize) -> String {
    (0..=width)
        .rev()
        .map(|cap| aterm_codec::tty::one_line(text, cap))
        .find(|shown| shown.chars().count() <= width)
        .unwrap_or_default()
}

/// The node a row proves at this machine's own `[fleet] listen` place, where its listener then is
/// not: a proven answer there, or another node's proof where the row's node was asked.
fn holder_of_listen(table: &FleetTable, rows: &[Row], own: Option<&str>) -> Option<String> {
    let listen: SocketAddr = table.listen.as_deref()?.parse().ok()?;
    let ours = |at: SocketAddr| {
        at.port() == listen.port()
            && (at.ip() == listen.ip() || wildcard_refused(listen.ip(), at.ip()))
            && (at.ip().is_loopback() || this_machine_is(&at.ip().to_string()))
    };
    let other = |node: &str| own != Some(node);
    rows.iter().find_map(|row| match row.check.as_ref()? {
        Ok(checked) if checked.proven && ours(checked.addr) => {
            checked.node.clone().filter(|node| other(node))
        }
        Err(Unreached::OtherNode { at, said, .. }) if ours(*at) && other(said) => {
            Some(said.clone())
        }
        _ => None,
    })
}

/// Whether a wildcard `listen` cannot bind beside a listener at `at`, an address it covers: on
/// Linux, which holds a listening port at every address it covers, and not on macOS, where std's
/// `SO_REUSEADDR` binds the wildcard beside the more specific place.
fn wildcard_refused(listen: IpAddr, at: IpAddr) -> bool {
    cfg!(target_os = "linux") && listen.is_unspecified() && (listen.is_ipv6() || at.is_ipv4())
}

/// Why `peer` was not read, with its remedies as run here, within a survey row's reason
/// ([`aterm_ctl::UNANSWERED_REASON_CHARS`], and [`aterm_ctl::UNANSWERED_ROOM`] beside an IPv6
/// place): a node at the place another node of the same host listens in rejoins on a port of
/// its own, a place of this machine its own node `own` answers at is named as this machine's,
/// a copy of the issuer's book is taken out on the issuer, and another secret is told as the
/// issuer that `minted` its code at that time sees it ([`code_minted`]), or as a member.
fn unreached_words(
    peer: &Peer,
    why: &Unreached,
    listed: &[Peer],
    (own, minted): (Option<&str>, Option<u64>),
) -> String {
    if let (Unreached::Handshake(at, FleetError::ListenerWrongSecret), Some(minted)) = (why, minted)
    {
        return match peer.seen_ms {
            Some(seen) if seen < minted => {
                format!(
                    "another secret at {at}: it joined before a rotate: it joins again with the new code"
                )
            }
            Some(_) => {
                format!(
                    "another secret at {at}: it left or joined another fleet: it joins again with this code"
                )
            }
            None => {
                format!(
                    "another secret at {at}: it holds another fleet's: it joins this fleet with this code"
                )
            }
        };
    }
    let Unreached::OtherNode { at, said, want } = why else {
        return why.to_string();
    };
    let rm = if peer.via == Via::IssuerBook {
        format!("the issuer's `peers rm {}`", short(want))
    } else {
        format!("`aterm fleet peers rm {}`", short(want))
    };
    // Two nodes registered by one name, or the loopback, are one machine's two listeners; a LAN
    // address another machine was given since names none.
    let one_machine = !peer
        .host
        .parse::<IpAddr>()
        .is_ok_and(|ip| !ip.is_loopback());
    let holds = listed.iter().any(|other| {
        other.node.as_deref() == Some(said.as_str())
            && other.addr == Some(at.ip())
            && other.port == at.port()
            && one_machine
            && same_host(&other.host, &peer.host)
    });
    let here = own == Some(said.as_str())
        && (at.ip().is_loopback() || this_machine_is(&at.ip().to_string()));
    match (holds || (here && one_machine), here) {
        (true, false) => format!(
            "node {} holds this place: `join --port <n>` on {}, or {rm}",
            short(said),
            short(want)
        ),
        (true, true) => format!(
            "this machine holds this place: `join --port <n>` on {}, or {rm}",
            short(want)
        ),
        // The row's label names the place, which an IPv6 address keeps whole beside it.
        (false, false) => format!(
            "node {} is here: {rm} if it left, or rejoin it",
            short(said)
        ),
        (false, true) => format!("this machine is here: `join` on {}, or {rm}", short(want)),
    }
}

/// What takes `row` out of the listing: `peers rm`, aterm.toml's edit, or `peers rm` on the issuer.
fn taken_out_by(places: &Places, row: &Row) -> String {
    match (&row.configured, row.peer.via) {
        (Some(entry), _) => in_aterm_toml(places, entry),
        (None, Via::IssuerBook) => "`aterm fleet peers rm` of it on the issuer takes it out of \
             the issuer's book, and this copy follows"
            .to_owned(),
        (None, _) => format!("`aterm fleet peers rm {}` takes it out", row.peer.target()),
    }
}

/// The edit that takes the `[[fleet.peers]]` entry `entry` out, which no verb makes.
fn in_aterm_toml(places: &Places, entry: &str) -> String {
    format!(
        "aterm.toml's `[[fleet.peers]] host = \"{}\"` names it: take that entry out of {}",
        aterm_codec::tty::one_line(entry, 60),
        places.config.display()
    )
}

fn age(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

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
    // Only drive reads the state: the book names the nodes that may drive.
    if level == aterm_net::fleet::DRIVE_LEVEL {
        narrowing(places, || set_level(places, &level, &from, &usage))
    } else {
        set_level(places, &level, &from, &usage)
    }
}

/// [`allow`]'s write of `level`, with the drive grants `from` names at drive. A grant is written by [`member::write_held`], under the fleet whose book named its nodes, each still bound to the key it names; facts or content names no fleet, so its write reads and writes aterm.toml alone, whose write takes the text it read or nothing (`ConfigFile::replace`): it lands before or after any other edit of `[fleet]`, a leave's or a join's included, as some serial order would, and no odd state directory or busy book blocks it.
fn set_level(
    places: &Places,
    level: &str,
    from: &[String],
    usage: &dyn Fn(&str) -> Failure,
) -> Result<String, Failure> {
    let drive = level == aterm_net::fleet::DRIVE_LEVEL;
    let word = if drive {
        if from.is_empty() {
            return Err(usage("drive names the nodes that may drive this Mac"));
        }
        aterm_net::fleet::DRIVE_LEVEL
    } else {
        let scope = FleetScope::from_word(level).ok_or_else(|| usage("no such level"))?;
        if !from.is_empty() {
            return Err(usage(
                "--from names who may drive, at the drive level alone",
            ));
        }
        scope.word()
    };
    let config = ConfigFile::open(&places.config).map_err(failed)?;
    in_a_fleet(text_of(&config)?)?;
    if !drive {
        at_step("level");
        return write_level(places, None, &config, (word, Vec::new()));
    }
    // The fleet the grant begins under, read before the book names its nodes: a missing key is a leave stopped short of aterm.toml.
    let key = places.state.join(KEY_FILE);
    let began = match aterm_net::fleet::read_key_file(&places.state) {
        Ok(secret) => Held::Key(secret),
        Err(aterm_net::fleet::KeyFileError::Missing) => {
            return Err(failed(format!(
                "this machine holds no fleet's key ({}), so nothing was granted: a grant names \
                 nodes of the fleet held here; `aterm fleet leave` finishes a leave that \
                 stopped short of aterm.toml",
                key.display()
            )));
        }
        Err(e) => {
            let (cause, act) = key_fault(places, &e);
            return Err(failed(format!(
                "{cause} ({}), so nothing was granted: {act}",
                key.display()
            )));
        }
    };
    let grants = drive_grants(places, from)?;
    at_step("grant");
    member::write_held(&places.state, Began::Under(&began), |book| {
        let keys = Keyring::read(&places.state);
        if let Some(gone) = grants
            .iter()
            .find(|grant| keys.bound(&grant.node) != Some(grant.key))
        {
            return Err(failed(format!(
                "this machine's book binds node {} to its key no more, so nothing was granted: \
                 `aterm fleet allow drive` again names the nodes it binds now",
                short(&gone.node)
            )));
        }
        let config = ConfigFile::open(&places.config).map_err(failed)?;
        let table = in_a_fleet(text_of(&config)?)?;
        // A list below drive grants nothing, so a grant starts it afresh.
        let mut nodes = if table.level() == Some(aterm_net::fleet::DRIVE_LEVEL) {
            table.drive_from
        } else {
            Vec::new()
        };
        for grant in &grants {
            nodes.retain(|word| DriveGrant::node_of(word) != grant.node);
            nodes.push(grant.word());
        }
        write_level(places, Some(book), &config, (word, nodes))
    })
    .map_err(|unwritten| {
        failed(format!(
            "{unwritten}, so nothing was granted: `aterm fleet allow drive` names the nodes of \
             the fleet this machine holds"
        ))
    })?
}

/// `[fleet]` as `text` says it, when it names a listen: this machine is in a fleet.
fn in_a_fleet(text: &str) -> Result<FleetTable, Failure> {
    let table = fleet_table(text).map_err(failed)?;
    if table.listen.is_none() {
        return Err(failed(
            "this machine is in no fleet ([fleet] listen is not set): `aterm fleet init` or \
             `aterm fleet join` first",
        ));
    }
    Ok(table)
}

/// Write `[fleet] allow = word` and `drive_from = nodes` over aterm.toml as `config` read it,
/// under the book's lock `held` where it is a grant's.
fn write_level(
    places: &Places,
    held: Option<&HeldBook<'_>>,
    config: &ConfigFile,
    (word, nodes): (&str, Vec<String>),
) -> Result<String, Failure> {
    let before = fleet_table(text_of(config)?).map_err(failed)?;
    let was = before.level();
    let drove = was == Some(aterm_net::fleet::DRIVE_LEVEL);
    let had = before.drive_from;
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
    let mut out = match with_level(text_of(config)?, word, &nodes).map_err(failed)? {
        None => format!(
            "aterm fleet allow: {said} in {} already\n",
            places.config.display()
        ),
        Some(text) => {
            let bak = match held {
                Some(held) => replace_held(held, config, &text)?,
                None => config
                    .lock()
                    .and_then(|edit| edit.replace(&text))
                    .map_err(failed)?,
            };
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
    if nodes.is_empty() && !had.is_empty() {
        // Members read at content still only where they did: a list a hand left at facts rises.
        let still = drove || was == Some(FleetScope::Content.word());
        out.push_str(&grants_removed(
            had.len(),
            &format!("allow is {word} now, below drive"),
            if still && word == FleetScope::Content.word() {
                CONTENT_LEFT
            } else {
                ""
            },
        ));
    }
    // A level below drive cuts drives only where it lowered drive or took a grant out.
    if !nodes.is_empty() {
        out.push_str(
            "  each node named, proving the node key it holds now, may drive this Mac's sessions \
             (`aterm ctl --host <this Mac> …` there); nobody else may, and a Mac that comes back \
             under another key drives nothing until it is named here again. The person here \
             wins: Take Over refuses every remote write.\n",
        );
        out.push_str(granted_reads(was));
    } else if drove || !had.is_empty() {
        out.push_str(
            "  no other Mac may drive this one's sessions: an aterm running here refuses each \
             new drive from now on, and within 3 s cuts the open ones and lifts the holds they \
             took\n",
        );
    }
    Ok(out)
}

/// What a grant leaves every member reading here, drive reading as content does, and the level
/// `was` before it that takes it back, ending every grant.
fn granted_reads(was: Option<&str>) -> &'static str {
    if was == Some(aterm_net::fleet::DRIVE_LEVEL) {
        "  reads      every member of the fleet reads this Mac at content, as drive reads: `aterm \
         fleet allow content` ends every grant, and `aterm fleet allow facts` lowers the reads \
         too\n"
    } else if was == Some(FleetScope::Content.word()) {
        "  reads      every member of the fleet reads this Mac at content still, as drive reads: \
         `aterm fleet allow content` takes it back, ending every grant\n"
    } else {
        "  reads      every member of the fleet now reads this Mac at content, not facts, as \
         drive reads (titles, cwds, prompts, an agent's words, a shell's lines): `aterm fleet \
         allow facts` takes it back, ending every grant\n"
    }
}

/// How a write that leaves the level at content as a grant ends says what every member reads.
const CONTENT_LEFT: &str = ", and every member of the fleet still reads it at content, which \
                            `aterm fleet allow facts` lowers";

/// The grant each of `words` names for `allow drive --from`.
/// # Errors
/// A word that names no peer, or a peer with no node or one bound to no key.
fn drive_grants(places: &Places, words: &[String]) -> Result<Vec<DriveGrant>, Failure> {
    if let Some((word, own)) = words
        .iter()
        .find_map(|word| Some((word, names_this_machine(places, word)?)))
    {
        return Err(failed(format!(
            "`{}` is this machine itself (node {}): `allow drive --from` names another Mac of the \
             fleet, and this one's own sessions are driven here with `aterm ctl`",
            aterm_codec::tty::one_line(word, 40),
            short(&own)
        )));
    }
    let missed = check_peers(places, PEER_CHECK_WITHIN)?.missed;
    let known: Vec<Peer> = known_peers(places)?
        .peers
        .into_iter()
        .map(|(peer, _)| peer)
        .collect();
    let issues = in_state(places, CODE_FILE)?;
    let mut nodes = Vec::new();
    for word in words {
        let shown = aterm_codec::tty::one_line(word, 40);
        let bound = bound_peer(places, &known, word)?;
        if matches!(bound, Bound::Nobody)
            && let Some(own) = at_own_place(places, word)
        {
            return Err(failed(format!(
                "`{shown}` is this machine itself (node {}): `allow drive --from` names another \
                 Mac of the fleet, and this one's own sessions are driven here with `aterm ctl`",
                short(&own)
            )));
        }
        let peer = match bound {
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
            Bound::Nobody if issues => {
                return Err(failed(format!(
                    "`{shown}` names no peer this machine's book records (`aterm fleet peers`)"
                )));
            }
            Bound::Nobody => {
                return Err(failed(match &missed {
                    Some(missed) => format!(
                        "`{shown}` names no peer this machine knows, and {}, so no book here says \
                         which Mac it is{} (`aterm fleet peers` says why)",
                        missed.why,
                        missed.again()
                    ),
                    None => format!(
                        "`{shown}` names no peer this machine knows, nor any its issuer's book \
                         names (`aterm fleet peers`)"
                    ),
                }));
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

/// `[fleet] drive_from` without the grants `gone` picks, in aterm.toml, and below drive without
/// the rest ([`with_level`]); nothing is written where `gone` picks none.
/// # Errors
/// Aterm.toml cannot be read, [`with_level`] refuses the edit, or the edit cannot be written.
fn drop_grants(
    places: &Places,
    held: &HeldBook<'_>,
    gone: impl Fn(&str) -> bool,
) -> Result<Option<Ungranted>, Failure> {
    let config = ConfigFile::open(&places.config).map_err(failed)?;
    let text = text_of(&config)?;
    let table = fleet_table(text).map_err(failed)?;
    let mut kept: Vec<String> = table
        .drive_from
        .iter()
        .filter(|word| !gone(word))
        .cloned()
        .collect();
    let picked = table.drive_from.len() - kept.len();
    if picked == 0 {
        return Ok(None);
    }
    let level = match table.level() {
        Some(aterm_net::fleet::DRIVE_LEVEL) if kept.is_empty() => FleetScope::Content.word(),
        Some(level) => level,
        None => table.allow.as_deref().unwrap_or(FleetScope::Facts.word()),
    };
    // A level below drive keeps no grant ([`with_level`]): the rest go with the ones picked.
    let swept = if level == aterm_net::fleet::DRIVE_LEVEL {
        0
    } else {
        std::mem::take(&mut kept).len()
    };
    let Some(edited) = with_level(text, level, &kept).map_err(failed)? else {
        return Ok(None);
    };
    let bak = replace_held(held, &config, &edited)?;
    let reads = if level == FleetScope::Content.word() {
        CONTENT_LEFT
    } else {
        ""
    };
    Ok(Some(Ungranted {
        picked,
        from: format!(
            "{}{}",
            places.config.display(),
            bak.map_or_else(String::new, |b| format!(
                " (previous saved as {})",
                b.display()
            ))
        ),
        left: match (kept.is_empty(), swept) {
            (false, _) => String::new(),
            (true, 0) => format!(
                ", and allow is {level}: no Mac drives this one until `aterm fleet allow drive \
                 --from <host|node>` grants again{reads}"
            ),
            (true, swept) => format!(
                ", with the {swept} more it listed, as allow is {level}, below drive: no Mac \
                 drives this one until `aterm fleet allow drive --from <host|node>` grants \
                 again{reads}"
            ),
        },
    }))
}

/// The grants a [`drop_grants`] took out: how many, the file (and its `.bak`), and what that
/// left this Mac.
struct Ungranted {
    picked: usize,
    from: String,
    left: String,
}

impl Ungranted {
    /// A report's row.
    fn row(&self) -> String {
        format!(
            "  drive      {} of [fleet] drive_from removed from {}{}\n",
            self.what(),
            self.from,
            self.left
        )
    }

    /// The same as a sentence's clause.
    fn clause(&self) -> String {
        format!(
            "{} of [fleet] drive_from {} removed from {}{}",
            self.what(),
            if self.picked == 1 { "was" } else { "were" },
            self.from,
            self.left
        )
    }

    fn what(&self) -> String {
        if self.picked == 1 {
            "its grant".to_owned()
        } else {
            format!("{} grants", self.picked)
        }
    }
}

fn leave(places: &Places, args: &[String]) -> Result<String, Failure> {
    if !args.is_empty() {
        return Err(Failure::Usage("leave takes nothing".to_owned()));
    }
    // Read before anything goes: once it has left, this machine reaches no Mac to hand a tab back,
    // and an issuer's fleet, which goes with it, takes no join again.
    let issues = in_state(places, CODE_FILE).unwrap_or(false);
    let holds = held_lines(
        places,
        "once it has left",
        (!issues).then_some("joins again"),
    );
    let mut out = String::new();
    let mut left = false;
    let mut issued = false;
    let (mut unkeyed, mut unlistened) = (false, false);
    let own = own_node(places);
    let mut issuer_named = false;
    // A leave's write: every other writer's step sees the whole leave or none of it.
    member::leave_held(&places.state, |leaving| -> Result<(), Failure> {
        if let Some(was) = leaving.narrowed() {
            out.push_str(&narrowed(places, was));
        }
        issuer_named = member::names_an_issuer(&places.state, own.as_deref());
        for (name, what) in [
            (KEY_FILE, "the fleet's secret"),
            (CODE_FILE, "the join code"),
            (PEERS_FILE, "the book of peers"),
            // Written by dev builds since 1b60ca3c4b; nothing reads it.
            (
                "fleet-peers-left",
                "what a join set aside of the fleet held before",
            ),
            (ISSUER_BOOK_FILE, "the copy of the issuer's book"),
        ] {
            let path = places.state.join(name);
            let had = leaving
                .remove(name)
                .map_err(|e| failed(format!("{}: {e}", path.display())))?;
            if had {
                out.push_str(&format!("  removed    {} ({what})\n", path.display()));
                left = true;
                issued |= name == CODE_FILE;
                unkeyed |= name == KEY_FILE;
            }
        }
        at_step("removed");
        let config = ConfigFile::open(&places.config).map_err(failed)?;
        let listened = fleet_table(text_of(&config)?)
            .map_err(failed)?
            .listen_port()
            .is_some();
        if let Some(text) = without_fleet(text_of(&config)?).map_err(failed)? {
            unlistened = listened;
            let bak = replace_held(leaving, &config, &text)?;
            out.push_str(&format!(
                "  config     [fleet] listen, allow and drive_from removed from {}{}\n",
                places.config.display(),
                bak.map_or_else(String::new, |b| format!(
                    " (previous saved as {})",
                    b.display()
                ))
            ));
            left = true;
        }
        at_step("unlisten");
        Ok(())
    })
    .map_err(failed)?
    .inspect_err(|_| say(&out))?;
    if !left {
        return Ok(format!(
            "aterm fleet leave: this machine is in no fleet\n{out}"
        ));
    }
    // A listener needs the key and `[fleet] listen` both. The issuer line goes only where this
    // book still names an issuer: one a `peers rm` took out here still lists this machine.
    let mut said: Vec<&str> = Vec::new();
    if unkeyed && unlistened {
        said.push("An aterm running here stops listening within 2 s (it watches the key).");
    }
    if issued {
        said.push(
            "This machine issued the fleet, and its fleet went with it: no machine can join it \
             or be rotated now. Run `aterm fleet init` on one machine and `aterm fleet join` on \
             each other to make a new one.",
        );
    } else if issuer_named {
        said.push(
            "The issuer is not told: its `aterm fleet peers` reads this machine as not \
             answering until `aterm fleet peers rm` there.",
        );
    }
    let mut out = format!("aterm fleet leave:\n{holds}{out}");
    if !said.is_empty() {
        out.push_str(&said.join(" "));
        out.push('\n');
    }
    Ok(out)
}

/// A report's line for each tab [`held_elsewhere`] finds this machine holding on another Mac,
/// which it reaches no more `once` (it has left, or holds another fleet's key): what lifts it
/// there, and this machine's own `hold off` once it `again` (joins that fleet again), where it
/// can.
fn held_lines(places: &Places, once: &str, again: Option<&str>) -> String {
    let own = own_node(places);
    held_elsewhere(places)
        .iter()
        .map(|(at, sid)| {
            format!(
                "  hold       {sid} on {at} stays held by this machine, which reaches that Mac no \
                 more {once}: {}{}\n",
                aterm_ctl::lifts_there(own.as_deref().unwrap_or("-")),
                again.map_or_else(String::new, |again| format!(
                    ", and so does `aterm ctl --host {at} hold {sid} off` here once this machine \
                     {again}"
                ))
            )
        })
        .collect()
}

/// The sessions other Macs' proven surveys say this machine holds (`held_from=` its node), each
/// beside the name a drive dials that Mac by.
fn held_elsewhere(places: &Places) -> Vec<(String, String)> {
    let (Ok(secret), Some(own)) = (
        aterm_net::fleet::read_key_file(&places.state),
        own_node(places),
    ) else {
        return Vec::new();
    };
    let Ok(known) = known_peers(places) else {
        return Vec::new();
    };
    let book: Vec<Peer> = known.peers.into_iter().map(|(peer, _)| peer).collect();
    raise_open_files();
    let (reads, _) = member::read_whole(
        &book,
        &known.keys,
        Some(&own),
        (&secret, own_identity(places).as_ref()),
        PEER_CHECK_WITHIN,
        "survey facts",
    );
    let mut held: Vec<(String, String)> = Vec::new();
    for read in reads {
        let Ok(surveyed) = read.read else {
            continue;
        };
        if !surveyed.proven || surveyed.node.as_deref() == Some(own.as_str()) {
            continue;
        }
        for row in surveyed
            .lines
            .iter()
            .filter(|line| line.starts_with("session "))
        {
            let field = |key: &str| {
                row.split(' ')
                    .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
            };
            // A peer's sid is printed only when it is one, as `survey` prints it.
            if field("held_from").is_some_and(|node| node.eq_ignore_ascii_case(&own))
                && let Some(sid) = field("sid").filter(|sid| {
                    sid.len() <= 64 && aterm_types::control_request::Sid::parse(sid).is_some()
                })
                && !held.iter().any(|(_, seen)| seen == sid)
            {
                held.push((
                    aterm_codec::tty::one_line(&shown_name(&read.peer), 80),
                    sid.to_owned(),
                ));
            }
        }
    }
    held
}

/// The fleet client of `aterm ctl survey --fleet | --host`
/// (`aterm_ctl::LocalVerbs::fleet_survey`).
/// # Errors
/// This machine holds no fleet key it can read, aterm.toml or the book cannot be read, or
/// `--host` names more than one peer, or neither a peer nor a host.
pub fn survey_fleet(ask: &aterm_ctl::FleetAsk<'_>) -> Result<aterm_ctl::FleetRead, String> {
    let places = Places::here().map_err(failure_words)?;
    survey_fleet_in(&places, ask)
}

/// The fleet client of `aterm ctl --host` (`aterm_ctl::LocalVerbs::fleet_drive`): a drive of
/// the peer `host` names.
/// # Errors
/// This machine is in no fleet or holds no node key, the book cannot be read, `host` names no
/// node the book records (or more than one), or the drive is refused.
pub fn drive_fleet(
    host: &str,
    within: Duration,
) -> Result<aterm_ctl::DriveLink, aterm_ctl::DriveRefused> {
    let places = Places::here().map_err(failure_words)?;
    drive_fleet_in(&places, host, within)
}

fn drive_fleet_in(
    places: &Places,
    host: &str,
    within: Duration,
) -> Result<aterm_ctl::DriveLink, aterm_ctl::DriveRefused> {
    let secret =
        aterm_net::fleet::read_key_file(&places.state).map_err(|e| aterm_ctl::DriveRefused {
            left: matches!(e, aterm_net::fleet::KeyFileError::Missing)
                .then(|| own_node(places).map(aterm_ctl::Left::Fleet))
                .flatten(),
            ..aterm_ctl::DriveRefused::once(no_key(places, &e))
        })?;
    let me = own_identity(places).ok_or(
        "this machine holds no node key it can read, so it proves no node and drives none: \
         `aterm fleet join` again mints it (on the fleet's issuer, `aterm fleet init`)",
    )?;
    if member::names_node(me.node(), host) {
        return Err(format!(
            "`{}` is this machine itself (node {}): its own sessions are driven here with `aterm \
             ctl`, never over the fleet",
            aterm_codec::tty::one_line(host, 80),
            short(me.node())
        )
        .into());
    }
    let deadline = (Instant::now() + within, within);
    let named = || -> Result<(Keyring, Vec<Peer>, Bound), aterm_ctl::DriveRefused> {
        let known = known_peers(places).map_err(failure_words)?;
        let book: Vec<Peer> = known.peers.into_iter().map(|(peer, _)| peer).collect();
        let bound = bound_peer(places, &book, host).map_err(failure_words)?;
        Ok((known.keys, book, bound))
    };
    let (mut keys, mut book, mut bound) = named()?;
    let mut unread = None;
    if matches!(bound, Bound::Nobody) {
        if let Some(own) = at_own_place(places, host) {
            return Err(format!(
                "`{}` is this machine itself (node {}): its own sessions are driven here with \
                 `aterm ctl`, never over the fleet",
                aterm_codec::tty::one_line(host, 80),
                short(&own)
            )
            .into());
        }
        let check = PEER_CHECK_WITHIN.min(within);
        match copy_again(places, &secret, Some(&me), (Instant::now() + check, check)) {
            Ok(true) => (keys, book, bound) = named()?,
            Ok(false) => {}
            Err(why) => unread = Some(why),
        }
    }
    let shown = aterm_codec::tty::one_line(host, 80);
    let peer = match bound {
        Bound::Node(peer) | Bound::Unbound(peer) => peer,
        Bound::ByHand => {
            return Err(format!(
                "`{shown}` is a peer named by hand, with no node: a drive goes to the node a \
                 join recorded, by its host, node id or address (`aterm fleet peers`)"
            )
            .into());
        }
        Bound::Nobody => {
            if let Some(missed) = unread {
                let why = format!(
                    "{}, so no book here says which Mac `{shown}` is: a drive goes only to a node \
                     a book holds",
                    missed.why
                );
                if missed.silent {
                    return Err(aterm_ctl::DriveRefused {
                        again: true,
                        left: None,
                        unserved: missed.unserved,
                        why: format!(
                            "{why}; try again once it answers (`aterm fleet peers` says why it \
                             does not)"
                        ),
                    });
                }
                return Err(aterm_ctl::DriveRefused {
                    left: own_node(places).map(aterm_ctl::Left::Book),
                    ..aterm_ctl::DriveRefused::once(format!(
                        "{why} (`aterm fleet peers` says why it is not read)"
                    ))
                });
            }
            return Err(aterm_ctl::DriveRefused {
                left: own_node(places).map(aterm_ctl::Left::Book),
                ..aterm_ctl::DriveRefused::once(format!(
                    "`{shown}` is no Mac this machine's book records (`aterm fleet peers`): a \
                     drive goes only to a node the book holds, named by its host, node id or \
                     the address and port it lists, never to whichever member answers at an \
                     address"
                ))
            });
        }
    };
    let label = shown_name(&peer);
    let driven =
        member::drive(&target_of(&peer), &keys, (&secret, &me), deadline).map_err(|why| {
            let said = unreached_words(&peer, &why, &book, (Some(me.node()), code_minted(places)));
            aterm_ctl::DriveRefused {
                again: drive_again(&why),
                left: None,
                unserved: unserved(&why),
                // The peer is named once: a reason that names its host already goes alone.
                why: if said.contains(&peer.host) {
                    said
                } else {
                    format!("{label}: {said}")
                },
            }
        })?;
    Ok(aterm_ctl::DriveLink {
        stream: Box::new(driven),
        label,
    })
}

/// Why a member's read of its issuer's book brought none ([`copy_again`], [`check_peers`]).
#[derive(Debug)]
struct Missed {
    /// Why, cause first.
    why: String,
    /// The issuer did not answer, so a read once it answers may bring the book: an issuer that
    /// answered with no book (an older aterm, none, not now, another node) is told once.
    silent: bool,
    /// The issuer took the connection and sent no fleet handshake in time ([`unserved`]).
    unserved: bool,
}

impl Missed {
    fn of(why: &BookUnread) -> Self {
        match why {
            BookUnread::Unreached(why) => Self::unreached(why),
            why => Self {
                why: format!("its issuer's book could not be read ({why})"),
                silent: false,
                unserved: false,
            },
        }
    }

    fn unreached(why: &Unreached) -> Self {
        match why {
            Unreached::Deadline(_, within) | Unreached::Lookup(_, within) => Self {
                why: format!(
                    "its issuer did not answer within {:.1}s",
                    within.as_secs_f64()
                ),
                silent: true,
                unserved: unserved(why),
            },
            why => Self {
                why: format!("its issuer's book could not be read ({why})"),
                silent: drive_again(why),
                unserved: false,
            },
        }
    }

    /// What `read` brought short of the issuer's book, judged by `issuer`, the issuer's own
    /// survey in that read: `None` where it brought the book or names no issuer.
    fn of_read(
        read: &BookRead,
        issuer: Option<&Result<member::Surveyed, Unreached>>,
    ) -> Option<Self> {
        match read {
            BookRead::NoIssuer | BookRead::Fetched(_) => None,
            BookRead::Refused(refused) => Some(Self::of(refused)),
            BookRead::Unread(why) | BookRead::Unshared(why) => Some(match issuer {
                Some(Err(unreached)) => Self::unreached(unreached),
                _ => Self {
                    why: format!("its issuer's book could not be read ({why})"),
                    silent: false,
                    unserved: false,
                },
            }),
        }
    }

    /// How a refusal that says no book here names the word ends: try again once the issuer
    /// answers, where it did not.
    const fn again(&self) -> &'static str {
        if self.silent {
            ": try again once it answers"
        } else {
            ""
        }
    }
}

/// On a member, its issuer's book read afresh (op `book`, by `deadline`: a drive's
/// [`PEER_CHECK_WITHIN`], as `allow drive` reads it, or a share of a survey's own) and kept as the
/// copy, as a drive's admission keeps one: whether a proven one was kept. A copy holds no member
/// that joined since this machine's last read.
/// # Errors
/// The issuer's book could not be read: why.
fn copy_again(
    places: &Places,
    secret: &FleetSecret,
    me: Option<&NodeIdentity>,
    deadline: (Instant, Duration),
) -> Result<bool, Missed> {
    let own = own_node(places);
    let Some(issuer) = member::read_peers(&places.state).ok().and_then(|book| {
        book.into_iter()
            .find(|peer| peer.via == Via::Issuer && peer.node.is_some() && peer.node != own)
    }) else {
        return Ok(false);
    };
    let book = member::fetch_book(&target_of(&issuer), (secret, me), deadline)
        .map_err(|why| Missed::of(&why))?;
    let copies = member::copies_of(&book, own.as_deref());
    Ok(book.proven
        && member::keep_issuer_book(&places.state, secret, Some((&book.node, &copies)))
            .unwrap_or(false))
}

fn drive_again(why: &Unreached) -> bool {
    match why {
        Unreached::Handshake(_, aterm_net::fleet::FleetError::AddressBusy) => true,
        _ if why.hung_up() => true,
        Unreached::Connect(_, e) => matches!(
            e.kind(),
            io::ErrorKind::ConnectionRefused
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
        ),
        _ => false,
    }
}

/// Whether the aterm dialled took the connection and sent no fleet handshake in time (stopped,
/// frozen or overloaded), so nothing was asked of it: exit 2 ([`aterm_ctl::DriveRefused`],
/// [`aterm_ctl::PeerFault::Unserved`]).
const fn unserved(why: &Unreached) -> bool {
    matches!(why, Unreached::Deadline(member::Stage::Handshake, _))
}

fn failure_words(failure: Failure) -> String {
    match failure {
        Failure::Usage(why) | Failure::Failed(why) => why,
    }
}

fn survey_fleet_in(
    places: &Places,
    ask: &aterm_ctl::FleetAsk<'_>,
) -> Result<aterm_ctl::FleetRead, String> {
    let secret = aterm_net::fleet::read_key_file(&places.state).map_err(|e| no_key(places, &e))?;
    let known = known_peers(places).map_err(failure_words)?;
    let mut book: Vec<Peer> = known.peers.into_iter().map(|(peer, _)| peer).collect();
    let me = own_identity(places);
    if let Some(what) = ask.host {
        if let Some(own) = names_this_machine(places, what) {
            return Err(format!(
                "`{}` is this machine itself (node {}): `aterm ctl survey` reads its own sessions",
                aterm_codec::tty::one_line(what, 80),
                short(&own)
            ));
        }
        let deadline = (Instant::now() + ask.within, ask.within);
        let mut keys = known.keys;
        let mut bound = member::bound_peers(&places.state);
        let mut unread = None;
        // A member's copy holds no member that joined since its last read: the issuer's book is
        // read in half the deadline at most, so a silent issuer leaves the rest for the host.
        if member::find(&bound, what)?.is_none() && member::find(&book, what)?.is_none() {
            if let Some(own) = at_own_place(places, what) {
                return Err(format!(
                    "`{}` is this machine itself (node {}): `aterm ctl survey` reads its own \
                     sessions",
                    aterm_codec::tty::one_line(what, 80),
                    short(&own)
                ));
            }
            let check = PEER_CHECK_WITHIN.min(ask.within / 2);
            match copy_again(
                places,
                &secret,
                me.as_ref(),
                (Instant::now() + check, check),
            ) {
                Ok(true) => {
                    let known = known_peers(places).map_err(failure_words)?;
                    keys = known.keys;
                    book = known.peers.into_iter().map(|(peer, _)| peer).collect();
                    bound = member::bound_peers(&places.state);
                }
                Ok(false) => {}
                Err(why) => unread = Some(why),
            }
        }
        let keys = &keys;
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
        let read = member::survey_one_by(&target_of(&peer), keys, (&secret, me.as_ref()), deadline);
        let mut row = peer_survey(
            &peer,
            read,
            (
                &book,
                me.as_ref().map(NodeIdentity::node),
                code_minted(places),
            ),
        );
        // The word was dialled as a host only because no book here could say what it is: the row
        // keeps what that dial found whole, led by the issuer's silence where the row has room,
        // and the note says the rest.
        let note = match (&unread, &mut row.answer) {
            (Some(missed), Err(found)) => {
                let lead = if missed.silent {
                    "issuer silent, as a host: "
                } else {
                    "no issuer's book, as a host: "
                };
                let n = |s: &str| s.chars().count();
                if n(lead) + n(found) + n(&row.label) <= aterm_ctl::UNANSWERED_ROOM {
                    found.insert_str(0, lead);
                }
                Some(format!(
                    "{}, so no book here names `{}`, and it was read as a host{}",
                    missed.why,
                    aterm_codec::tty::one_line(what, 80),
                    if missed.silent {
                        ": try again once the issuer answers (`aterm fleet peers` says why it \
                         does not)"
                    } else {
                        " (`aterm fleet peers` says why the book is not read)"
                    }
                ))
            }
            _ => None,
        };
        return Ok(aterm_ctl::FleetRead {
            peers: vec![row],
            note,
            alone: None,
        });
    }
    // A book that lists no peer is read by no dial: say why, as `peers` does.
    let alone = (book.is_empty() && known.skipped.is_empty()).then(|| {
        format!(
            "survey --fleet read no other machine: {}",
            reads_no_peer(!in_state(places, CODE_FILE).unwrap_or(false))
        )
    });
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
    let dropped = keep_book(places, &secret, &book_read, own.as_deref());
    // A place this machine's own node answered at is this machine, which its own survey reads.
    let (entries, answers): (Vec<Peer>, Vec<_>) = reads
        .into_iter()
        .filter(|read| {
            !matches!(&read.read, Ok(surveyed)
                if is_this_machine(surveyed.node.as_deref(), surveyed.proven, own.as_deref()))
        })
        .map(|read| (read.peer, read.read))
        .unzip();
    let mut peers: Vec<aterm_ctl::PeerSurvey> = entries
        .iter()
        .zip(once_per_node(&entries, answers))
        .filter_map(|(peer, read)| {
            Some(peer_survey(
                peer,
                read?,
                (&entries, own.as_deref(), code_minted(places)),
            ))
        })
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
                node: None,
            }),
    );
    Ok(aterm_ctl::FleetRead {
        peers,
        note: joined_notes(book_note(&book, &book_read), dropped),
        alone,
    })
}

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
    } else if unserved(why) {
        PeerFault::Unserved
    } else {
        PeerFault::Other
    }
}

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

fn peer_survey(
    peer: &Peer,
    read: Result<member::Surveyed, Unreached>,
    (listed, own, minted): (&[Peer], Option<&str>, Option<u64>),
) -> aterm_ctl::PeerSurvey {
    let name = shown_name(peer);
    aterm_ctl::PeerSurvey {
        label: match &read {
            Ok(surveyed) if surveyed.addr.ip().to_string() != peer.host => {
                format!("{name} at {}", surveyed.addr.ip())
            }
            _ => name,
        },
        node: peer.node.clone(),
        fault: read
            .as_ref()
            .err()
            .map_or(aterm_ctl::PeerFault::Other, fault_of),
        unproven: matches!(&read, Ok(surveyed) if !surveyed.proven),
        answer: read
            .map(|surveyed| surveyed.lines)
            .map_err(|e| unreached_words(peer, &e, listed, (own, minted))),
    }
}

/// Why this machine reads and drives no peer, and what puts it right, within a survey row's
/// reason ([`aterm_ctl::UNANSWERED_REASON_CHARS`]).
fn no_key(places: &Places, error: &aterm_net::fleet::KeyFileError) -> String {
    let (cause, act) = key_fault(places, error);
    format!("{cause}: {act}")
}

/// What is wrong with the key file, and the act that puts it right ([`no_key`]): a key that
/// holds no secret is replaced by a join, or left and joined again where the book names an
/// issuer a join cannot tell is the code's ([`bound_issuer`]), or on its issuer made again.
fn key_fault(places: &Places, error: &aterm_net::fleet::KeyFileError) -> (String, &'static str) {
    use aterm_net::fleet::KeyFileError as E;
    use aterm_uds::private_file::{ACL_OPEN, DIR_OPEN, FILE_OPEN};
    match error {
        E::Missing => (
            "this machine is in no fleet".to_owned(),
            "`aterm fleet init` on one machine, `aterm fleet join` on each other",
        ),
        E::NotPrivate(ANOTHER_LINK) => (
            format!("{KEY_FILE} has another hard link, a second copy of the fleet's secret"),
            "remove that link, run this again",
        ),
        E::NotPrivate(FILE_OPEN) => (
            format!("{KEY_FILE} is open to other users"),
            "`chmod 600` it, then run this again",
        ),
        E::NotPrivate(DIR_OPEN) => (
            "aterm's state folder is open to other users".to_owned(),
            "`chmod 700` it, then run this again",
        ),
        E::NotPrivate(ACL_OPEN) => (
            format!("{KEY_FILE} or its folder has an access list (ACL) for others"),
            "`chmod -N` it, run this again",
        ),
        E::Malformed(_) if in_state(places, CODE_FILE).unwrap_or(false) => {
            (error.to_string(), "`aterm fleet leave`, `init` again")
        }
        E::Malformed(_) if joins_over(places) => {
            (error.to_string(), "`aterm fleet join` replaces it")
        }
        E::Malformed(_) => (error.to_string(), "`aterm fleet leave`, join again"),
        other => (other.to_string(), "run this again once it reads"),
    }
}

/// Whether a join writes its key over one that holds no secret: the book reads, and names no
/// issuer bound by an entry of no fleet ([`bound_issuer`] refuses beside one).
fn joins_over(places: &Places) -> bool {
    member::read_peers(&places.state).is_ok_and(|book| {
        !book.iter().any(|peer| {
            peer.via == Via::Issuer
                && peer.fleet.is_none()
                && peer.node.is_some()
                && peer.key.is_some()
        })
    })
}

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
    // SAFETY: `setrlimit` reads the struct this frame owns; a soft limit at or under the hard one
    // is all it is asked.
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

    fn level(&self) -> Option<&'static str> {
        let word = self.allow.as_deref()?.to_ascii_lowercase();
        if word == aterm_net::fleet::DRIVE_LEVEL {
            return Some(aterm_net::fleet::DRIVE_LEVEL);
        }
        FleetScope::from_word(&word).map(FleetScope::word)
    }

    /// The level a write of `[fleet]` sets: `allow` (`--allow`), else this table's, else facts,
    /// with drive lowered to content where `ungrant` takes the grants out.
    fn written_level(&self, allow: Option<FleetScope>, ungrant: bool) -> &'static str {
        match allow
            .map(FleetScope::word)
            .or_else(|| self.level())
            .unwrap_or(FleetScope::Facts.word())
        {
            aterm_net::fleet::DRIVE_LEVEL if ungrant => FleetScope::Content.word(),
            allow => allow,
        }
    }
}

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

/// `text` with `[fleet] listen` and `allow` set, and `drive_from` out where `ungrant` takes it or
/// `allow` is below drive.
/// # Errors
/// `text` is not TOML, `fleet` is no table, or the edit would not read back as exactly this
/// change.
fn with_fleet(
    text: &str,
    listen: &str,
    allow: &str,
    ungrant: bool,
) -> Result<Option<String>, String> {
    use aterm_toml::edit::{DocumentMut, Item, Table, Value};
    // A level below drive keeps no grant.
    let ungrant = ungrant || allow != aterm_net::fleet::DRIVE_LEVEL;
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

/// `text` with `[fleet] allow` set to `level` and `drive_from` to `drive_from`, none below drive.
/// # Errors
/// As [`with_fleet`], and where `text` has no `[fleet]` table.
fn with_level(text: &str, level: &str, drive_from: &[String]) -> Result<Option<String>, String> {
    use aterm_toml::edit::{Array, DocumentMut, Item, Value};
    let drive_from = if level == aterm_net::fleet::DRIVE_LEVEL {
        drive_from
    } else {
        &[]
    };
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

/// `text` without `[fleet] listen`, `allow` and `drive_from`, and without a `[fleet]` they
/// leave empty.
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

fn text_of(config: &ConfigFile) -> Result<&str, Failure> {
    Ok(config.text().map_err(failed)?.unwrap_or(""))
}

/// What an edit of aterm.toml's `[fleet]` is made beside ([`replace_held`]): the book's lock, or a leave's hold.
trait Beside {}

impl Beside for HeldBook<'_> {}

impl Beside for Leaving<'_> {}

/// Replace aterm.toml as `config` read it with `text`, beside `_held` ([`member::write_held`], [`member::leave_held`]), and aterm.toml's write lock after the book's: every edit of `[fleet]` but a level's ([`set_level`]).
fn replace_held(
    _held: &impl Beside,
    config: &ConfigFile,
    text: &str,
) -> Result<Option<PathBuf>, Failure> {
    config
        .lock()
        .and_then(|edit| edit.replace(text))
        .map_err(failed)
}

/// Why a write of `[fleet]` ([`write_fleet`]) takes the drive grants out at any level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ungrant {
    /// They named the nodes of another fleet than the one written.
    OtherFleet,
    /// The book names no issuer of the fleet held to back them.
    Unbacked,
    /// The fleet's secret is new (the issuer's `init --rotate`, or its `leave` and `init`
    /// again), which removes every grant.
    NewSecret,
}

/// The line saying `n` drive grants were removed, as `why` says, and what that `left`.
fn grants_removed(n: usize, why: &str, left: &str) -> String {
    format!(
        "  drive      {} of [fleet] drive_from removed, as {why}: no Mac drives this one \
         until `aterm fleet allow drive --from <host|node>` grants again{left}\n",
        if n == 1 {
            "its grant".to_owned()
        } else {
            format!("its {n} grants")
        }
    )
}

/// Write `[fleet] listen` and `allow` beside `held`, with the drive grants out where `ungrant`
/// says why or `allow` is below drive: the line saying so, and the one saying grants went.
fn write_fleet(
    held: &HeldBook<'_>,
    config: &ConfigFile,
    path: &Path,
    (listen, allow): (&str, &str),
    ungrant: Option<Ungrant>,
) -> Result<(String, Option<String>), Failure> {
    let text = text_of(config)?;
    let before = fleet_table(text).map_err(failed)?;
    let had = before.drive_from.len();
    // Drive lowered to content leaves every member reading at content.
    let left = if before.level() == Some(aterm_net::fleet::DRIVE_LEVEL)
        && allow == FleetScope::Content.word()
    {
        CONTENT_LEFT
    } else {
        ""
    };
    let said = format!("[fleet] listen = \"{listen}\", allow = \"{allow}\"");
    let out = match with_fleet(text, listen, allow, ungrant.is_some()).map_err(failed)? {
        None => format!("  config     {said} in {} already\n", path.display()),
        Some(text) => {
            let bak = replace_held(held, config, &text)?;
            format!(
                "  config     {said} in {}{}\n",
                path.display(),
                bak.map_or_else(String::new, |b| format!(
                    " (previous saved as {})",
                    b.display()
                ))
            )
        }
    };
    let gone =
        (had > 0 && (ungrant.is_some() || allow != aterm_net::fleet::DRIVE_LEVEL)).then(|| {
            grants_removed(
                had,
                &match ungrant {
                    Some(Ungrant::OtherFleet) => {
                        "they named the nodes of the fleet this machine held before".to_owned()
                    }
                    Some(Ungrant::Unbacked) => {
                        "the book names no issuer of this fleet to back them".to_owned()
                    }
                    Some(Ungrant::NewSecret) => {
                        "the fleet's secret is new, and a new secret removes every grant".to_owned()
                    }
                    None => format!("allow is {allow} now, below drive"),
                },
                left,
            )
        });
    Ok((out, gone))
}

#[cfg(test)]
#[path = "fleet_member_tests.rs"]
mod tests;
