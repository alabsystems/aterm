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
    JoinCode, Keyring, Leaving, Leveling, NODE_FILE, NODE_KEY_FILE, PEERS_FILE, Peer, Recorded,
    Registration, Unreached, Unwritten, Via, target_of,
};
use aterm_uds::atomic_file::{ConfigFile, EditLanded};
use aterm_uds::private_file::ANOTHER_LINK;

/// The whole `aterm fleet --help` (`fleet_cli` prints it), and each verb's `--help`.
pub(crate) const USAGE: &str = "\
aterm fleet — this machine's membership in a fleet: the aterms of one LAN that
hold one secret, each reading the others at the level they allow

USAGE:
  aterm fleet init [--allow facts|content] [--port N] [--host H] [--rotate]
                         found a fleet on this machine: mint its secret,
                         write the join code to a 0600 file and print that
                         file's path (never the code), set [fleet] listen/allow
  aterm fleet join [--allow facts|content] [--port N] [--host H] [--at H[:P]]
                 [--code-file F]
                         on each other machine: join with that code (the
                         fleet's secret alone), read from stdin when piped,
                         from F, or typed at the terminal unshown; never an
                         argument. Any member records the join: the one --at
                         names, else the place an older aterm's code names,
                         else each member this machine's book holds, then
                         the members advertising _alab._tcp on this LAN
  aterm fleet peers [--timeout S]
                         the machines this one knows (its book and aterm.toml's
                         [[fleet.peers]] host = \"<host[:port]>\" entries), each
                         checked at once (3 s; --timeout up to 10): on a
                         member, the book of the member that answered its
                         join too
  aterm fleet peers add <host[:port]> | rm <host[:port]|node>
  aterm fleet allow facts|content
  aterm fleet allow drive --all | --from <host|node> [--from …]
                         what a peer may do here: read facts or content, or
                         (drive, granted here alone) drive its sessions from
                         every member or the nodes named, and every member
                         reads at content; facts|content takes it back
  aterm fleet publish    an ALab vault admin on the Mac that minted the fleet's
                         code publishes it to every current and future ALab
                         member
  aterm fleet sync [--on]
                         join the signed default ALab fleet, or its newer code
                         from the default fleet this machine is in, keeping
                         the level set here (a new secret's join takes named
                         drive grants out, as every join does); a first join
                         lets every member drive this machine, a vault
                         admin's too. A fleet joined by hand to
                         another code, one minted here, and a key it cannot
                         read are left as they are. leave's fleet-off record
                         keeps sync off until --on or a join here (an init
                         keeps it)
  aterm fleet census [--json] [--refresh]
                         read this machine's cached swarm census of its own
                         book (or compute it here): member labels,
                         versions, window liveness, sessions, warm/leased/walled
                         accounts, shortfall, converge checks and LAN stragglers
  aterm fleet leave      remove this machine's secret, join code, books and
                         [fleet] listen/allow/drive_from (its node key stays),
                         naming first each tab it holds on another Mac

Start a fleet with `init` on one machine and `join` on each other. --allow is
what a peer may read here: facts (programs, agent states, roles, task records
and models; no text a person or a program wrote) or content (the tab titles,
cwds, branches, prompts, an agent's newest words, and a shell's command line
and last rows as well); without --allow, either keeps the level [fleet] allow
names, else facts (a join into another fleet turns drive into content). --port
is this machine's fleet port: init's is 7100, join's the port of the member it
dials, and either keeps the port [fleet] listen already names; a join that
would listen at the place of the member it dials (a loopback fleet's, with no
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
help workers`), and nobody else does. `--all` instead admits every node the
issuer's book binds, under the key it binds. No plain join and no remote call
turns either grant on; `sync` applies the default ALab full-membership policy.
`init --rotate` mints the issuer a new secret, keeping the place [fleet]
listen names unless --host or --port names others (every member joins again;
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

#[cfg(test)]
thread_local! {
    static ISSUER_READ_WITHIN: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
    static PEER_READ_WITHIN: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

/// How long a verb's read of the fleet's listeners, and `peers`' status ask of the aterm here
/// ([`aterm_here`]), wait at most where the verb names no bound (`peers` with no `--timeout`,
/// [`held_elsewhere`], a join's check of its own place): [`PEER_CHECK_WITHIN`], or what a test
/// set: its hang detector for a read whose every listener serves in process, refuses at once or
/// is held silent ([`member::HeldSilent`]), and so answers or ends. A unit test sets it on its
/// thread; a test of the binary sets the dev seam [`PEER_WITHIN_SEAM`] (milliseconds) on a verb
/// whose every listener is an aterm on the same machine that serves or refuses at once, and whose
/// aterm here answers its status or runs nowhere, which a loaded machine can keep from answering
/// within [`PEER_CHECK_WITHIN`].
fn peer_read_within() -> Duration {
    #[cfg(test)]
    if let Some(within) = PEER_READ_WITHIN.get() {
        return within;
    }
    seamed_within(PEER_WITHIN_SEAM, PEER_CHECK_WITHIN)
}

/// How long a verb's read of the issuer's book before it names a peer ([`copy_first`],
/// [`check_peers`]) waits at most: [`PEER_CHECK_WITHIN`], or what a test set: its hang detector
/// for an issuer that serves, and so must answer, or none, which leaves a survey's whole deadline
/// to the host it dials. A unit test sets it on its thread; a test of the binary sets the dev seam
/// [`ISSUER_WITHIN_SEAM`] (milliseconds), as the issuer it dials is an aterm on the same machine,
/// which a loaded machine can keep from answering within [`PEER_CHECK_WITHIN`].
fn issuer_read_within() -> Duration {
    #[cfg(test)]
    if let Some(within) = ISSUER_READ_WITHIN.get() {
        return within;
    }
    seamed_within(ISSUER_WITHIN_SEAM, PEER_CHECK_WITHIN)
}

/// The milliseconds the dev seam `seam` names, or `or` where it names none.
fn seamed_within(seam: &str, or: Duration) -> Duration {
    aterm_types::dev_seam!(seam)
        .and_then(|ms| ms.to_str()?.trim().parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .map_or(or, Duration::from_millis)
}

/// The dev seam that sets [`issuer_read_within`] for a test of the binary.
const ISSUER_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_ISSUER_WITHIN_MS";

/// The dev seam that sets [`peer_read_within`] for a test of the binary.
const PEER_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_PEER_WITHIN_MS";

/// The dev seam that sets a fleet survey's per-peer deadline ([`survey_fleet`]) for a test of the
/// binary, in place of its `--timeout` or default: a survey whose every peer is an aterm on the
/// same machine that serves or refuses at once, which a loaded machine can keep from answering
/// within the most `--timeout` allows.
const SURVEY_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_SURVEY_WITHIN_MS";

const PEER_CHECK_MAX: Duration = Duration::from_secs(10);

/// How long a join's dial of its issuer, and its registration there, take at most.
#[cfg(not(test))]
const JOIN_WITHIN: Duration = Duration::from_secs(10);
/// The unit tests' issuers serve in process, so a join one answers late is a loaded machine, not
/// an issuer gone silent: their joins wait the hang detector.
#[cfg(test)]
const JOIN_WITHIN: Duration = aterm_spec::wait::MUST_HAPPEN;

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
        "publish" => publish(&places, &args),
        "sync" => sync(&places, &args),
        "census" => census(&places, &args),
        "leave" => leave(&places, &args),
        _ => Err(Failure::Usage(format!("no such command `{verb}`"))),
    });
    match ran {
        Ok(report) => {
            let report = with_final_newline(report);
            aterm_uds::stdout::answer_exit(&format!("aterm fleet {verb}"), &report, 0)
        }
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

fn with_final_newline(mut report: String) -> String {
    if !report.ends_with('\n') {
        report.push('\n');
    }
    report
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

impl Failure {
    /// Why, whichever it is.
    fn into_text(self) -> String {
        match self {
            Failure::Usage(why) | Failure::Failed(why) => why,
        }
    }
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
    /// `join --at <host[:port]>`: a member to dial first, since the code names none.
    at: Option<(String, u16)>,
    /// `fleet sync`'s join, made only while this Mac holds the fleet sync read here, which a
    /// leave's [`member::OFF_FILE`] stops too.
    synced: Option<Held>,
    /// Set when `join`'s issuer dial failed at connection or per-address capacity.
    saw_capacity: std::sync::atomic::AtomicBool,
    /// The `_alab._tcp` browse a join reads members' places from once none it knows answered
    /// ([`advertised_places`]); `None`: one of its own, started then.
    adverts: Option<std::sync::Arc<aterm_secrets::discover::Watch>>,
}

impl Opts {
    /// The level a join writes over `table`'s: `--allow`, else a sync's first join's content
    /// (until `grant_default` widens it to drive).
    fn level_over(&self, table: &FleetTable) -> Option<FleetScope> {
        self.allow.or_else(|| {
            (self.synced.is_some() && table.allow.is_none()).then_some(FleetScope::Content)
        })
    }
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
            "--at" => {
                opts.at = Some(
                    member::parse_host_port(&value, Some(member::default_port()))
                        .map_err(|why| Failure::Usage(format!("--at <host[:port]>: {why}")))?,
                );
            }
            _ => opts.code_file = Some(PathBuf::from(value)),
        }
    }
    Ok(opts)
}

fn allow_of(word: &str) -> Result<FleetScope, Failure> {
    FleetScope::from_word(&word.to_ascii_lowercase()).ok_or_else(|| {
        Failure::Usage(
            "--allow is `facts` or `content`: driving this Mac is granted on it alone, to every \
             member (`aterm fleet allow drive --all`) or the nodes it names (`--from \
             <host|node>`), never by an init or a join by hand"
                .to_owned(),
        )
    })
}

fn parse_join(args: &[String]) -> Result<Opts, Failure> {
    parse_opts(
        args,
        &["--allow", "--port", "--host", "--code-file", "--at"],
        true,
    )
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

/// The loopback address `[fleet] listen` names in `table`, if it names one ([`listen_on`]).
fn loopback_listen(table: &FleetTable) -> Option<String> {
    let (host, _) = member::parse_host_port(table.listen.as_deref()?, None).ok()?;
    host.parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
        .then_some(host)
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
    Minted(Box<Minted>),
}

struct Minted {
    code: JoinCode,
    /// Where this machine listens for the fleet: the code names no place, so the report does.
    place: (String, u16),
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
        place,
        written,
        listen,
        ungranted,
    } = match begun {
        Begun::Again(report) => return Ok(report),
        Begun::Minted(minted) => *minted,
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
    let unserved = FleetSecret::new(&code.secret)
        .ok()
        .and_then(|secret| serves_new_key(places, &secret));
    let mut out = format!(
        "aterm fleet init: this machine ({}, node {}) is the fleet's issuer{}\n",
        aterm_codec::tty::one_line(&place.0, 80),
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
         which node it is\n{config_line}{}",
        places.state.join(KEY_FILE).display(),
        code_path.display(),
        places.state.join(NODE_KEY_FILE).display(),
        unserved.as_deref().unwrap_or_default(),
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
         programs, agent states, roles, task records and models alone\n{}\n",
        follows(
            unserved.as_deref(),
            &format!(
                " The code names no member: a join finds one on this LAN, and `--at {}` names \
                 this one.",
                member::host_port(&place.0, place.1)
            )
        )
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
                     that bound an older key runs `aterm fleet peers rm {node}` there first\n",
                    places.state.join(NODE_KEY_FILE).display()
                )
            }
        )));
    }
    // A rotate keeps the place this machine listens at: a loopback fleet stays on the loopback.
    let kept = opts.rotate.then(|| loopback_listen(&table)).flatten();
    let host = match opts.host.clone().or(kept) {
        Some(host) => host,
        None => aterm_uds::process::lan_host_name()
            .filter(|host| member::is_host(host))
            .ok_or_else(|| {
                failed(
                    "this machine's host name is no host a member can dial; name one with --host",
                )
            })?,
    };
    let port = opts
        .port
        .or_else(|| table.listen_port())
        .unwrap_or_else(member::default_port);
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
    // The file names where this Mac listens after the secret, as every code did before F17: an
    // aterm before 0.147 reads no code without it (F20 drops it).
    let code = JoinCode {
        secret,
        hint: Some((host.clone(), port)),
    };
    // A person's init supersedes the default grant a sync's join owes.
    forget_owed_grant(places)?;
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
    Ok(Begun::Minted(Box::new(Minted {
        code,
        place: (host, port),
        written: Held::Key(written),
        listen,
        ungranted,
    })))
}

/// Why an init that mints takes the drive grants out: a new fleet, or a new secret.
const fn minted_by(opts: &Opts) -> Ungrant {
    if opts.rotate {
        Ungrant::NewSecret
    } else {
        Ungrant::OtherFleet
    }
}

/// An init again's `[fleet] listen`, written where aterm.toml has none (an init or a rotate stopped before aterm.toml) at the place `--host` and `--port` name, at the level `--allow` names as the stopped init would, and the line saying so; refused, with nothing written, where the key cannot be read, the code is of another secret (a rotate stopped between its two writes) or cannot be read beside a flag, `--host` or `--port` names another place than `[fleet] listen`, or `--allow` another level than `[fleet] allow` with nothing to write.
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
    let listening = table.listen_port();
    if opts
        .port
        .is_some_and(|port| Some(port) != listening && listening.is_some())
    {
        return Err(Failure::Usage(format!(
            "this machine is already the fleet's issuer, listening on port {}: another port is \
             aterm.toml's [fleet] listen to edit, not an init again",
            listening.unwrap_or_default()
        )));
    }
    if let (Some(host), Some(port), Some(listen)) = (&opts.host, listening, &table.listen)
        && listen_on(host, port) != *listen
    {
        return Err(Failure::Usage(format!(
            "this machine is already the fleet's issuer, listening at {listen}: `--host {}` \
             would listen at another place, which `aterm fleet init --rotate --host {0}` \
             writes (every member joins again)",
            aterm_codec::tty::one_line(host, 80)
        )));
    }
    if listening.is_some() {
        // The level [fleet] allow names changes nothing.
        return match opts.allow {
            Some(level) if table.level() != Some(level.word()) => Err(Failure::Usage(format!(
                "this machine is already the fleet's issuer: a level is set by `aterm fleet \
                 allow {}`, not `init`",
                level.word()
            ))),
            _ => Ok(String::new()),
        };
    }
    let host = opts
        .host
        .clone()
        .or_else(aterm_uds::process::lan_host_name)
        .unwrap_or_default();
    let line = write_fleet(
        book,
        config,
        &places.config,
        (
            &listen_on(&host, opts.port.unwrap_or_else(member::default_port)),
            table.written_level(opts.allow, false),
        ),
        None,
    )?;
    Ok(format!(
        "{}             written: aterm.toml had no [fleet] listen (an init stopped before it), \
         so no member could reach this machine\n{}",
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

/// A test's hook at each step a kill, or another verb, could come before: [`join`]'s (`lock` once it dialled, `key`, `drop`, `config`, `book`), [`init`]'s first write (`key`), its key's write past its code's (`code`) and its write of aterm.toml (`config`), a grant's write (`grant`) and a level's (`level`), a read's copy of the issuer's book (`keep`), and [`leave`]'s, under the book's lock, once it removed the fleet's files (`removed`), once it read aterm.toml (`config`) and once it edited it (`unlisten`): a panic stops the verb there as a kill would, and a step reached inside the hook runs none.
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

/// A report's closing words on an aterm here: [`FOLLOWS`], with `then` after it, unless the
/// verb's own wait saw no listener serve the key it wrote (`unserved`, [`serves_new_key`]), whose
/// line gives that cause and its remedy instead.
fn follows(unserved: Option<&str>, then: &str) -> String {
    match unserved {
        Some(_) => then.trim_start().to_owned(),
        None => format!("{FOLLOWS}{then}"),
    }
}

/// The places a join dials, in turn, until a member records it
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1, F17: any member admits a join, and the code
/// names none): `--at`, the place a code an older aterm wrote names, then each member the book in
/// the state directory holds, the one heard last first.
fn join_places(places: &Places, opts: &Opts, code: &JoinCode) -> Vec<(String, u16)> {
    let own = member::read_node_id(&places.state).ok().flatten();
    let mut book: Vec<Peer> = member::read_peers(&places.state)
        .unwrap_or_default()
        .into_iter()
        .filter(|peer| peer.via != Via::Manual && peer.node.is_some() && peer.node != own)
        .collect();
    book.sort_by_key(|peer| std::cmp::Reverse(peer.seen_ms));
    let mut dials: Vec<(String, u16)> = Vec::new();
    let known = opts
        .at
        .iter()
        .chain(&code.hint)
        .cloned()
        .chain(book.iter().map(|peer| {
            (
                peer.addr
                    .map_or_else(|| peer.host.clone(), |a| a.to_string()),
                peer.port,
            )
        }));
    for (host, port) in known {
        if !dials
            .iter()
            .any(|(held, at)| *at == port && same_host(held, &host))
        {
            dials.push((host, port));
        }
    }
    dials
}

/// How long a join listens for `_alab._tcp` adverts, and then resolves them.
const ADVERTS_WITHIN: Duration = Duration::from_secs(3);

/// The places the members on this LAN advertise (`_alab._tcp`, F17), less those in `known`:
/// `opts`' browse, else a fresh one given [`ADVERTS_WITHIN`] to hear them. None in a test or an
/// isolated run (`ATERM_STATE_HOME`, `CI`), so no lab's world ever dials the live fleet.
fn advertised_places(opts: &Opts, known: &[(String, u16)]) -> Vec<(String, u16)> {
    let watch = match opts.adverts.clone() {
        Some(watch) => watch,
        None if cfg!(test)
            || std::env::var_os("ATERM_STATE_HOME").is_some()
            || std::env::var_os("CI").is_some() =>
        {
            return Vec::new();
        }
        None => {
            let bell = std::sync::Arc::new(aterm_secrets::wake::Bell::default());
            let watch = aterm_secrets::discover::Watch::start_for(
                crate::node_service::ALAB_TCP_SERVICE,
                &bell,
                &[],
            );
            std::thread::sleep(ADVERTS_WITHIN);
            std::sync::Arc::new(watch)
        }
    };
    let mut found: Vec<(String, u16)> = Vec::new();
    for advert in watch.adverts(ADVERTS_WITHIN) {
        let place = (advert.host, advert.port);
        if !known
            .iter()
            .chain(&found)
            .any(|(host, port)| *port == place.1 && same_host(host, &place.0))
        {
            found.push(place);
        }
    }
    found
}

fn join(places: &Places, opts: &Opts, code_text: &str) -> Result<String, Failure> {
    let code = JoinCode::parse(code_text).map_err(failed)?;
    let secret = FleetSecret::new(&code.secret).map_err(failed)?;
    let mut dials = join_places(places, opts, &code);
    // With no place it knows, the members that advertise themselves are its places; with one, they
    // are dialled only once none of those answered.
    let browsed = dials.is_empty();
    if browsed {
        dials = advertised_places(opts, &[]);
    }
    let Some(first) = dials.first().cloned() else {
        // An issuer's own code names no member either: that it is this machine's own fleet's
        // is the refusal to give, once a loose state directory reads.
        let mut out = String::new();
        if in_state(places, CODE_FILE)? {
            private_state(places, &mut out)?;
            not_an_issuer(places, &code, &secret).inspect_err(|_| say(&out))?;
        }
        return Err(Failure::Usage(
            "the join code names no member, this Mac knows of none, and none advertises itself \
             on this LAN: name one with `--at <host[:port]>` (a member's `aterm fleet peers` \
             lists the fleet's places); nothing was written here"
                .to_owned(),
        ));
    };
    // A member of a loopback fleet listens on the loopback alone ([`listen_on`]): it is dialled
    // there.
    let host = match (opts.host.clone(), first.0.parse::<IpAddr>()) {
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
    // A fleet's members listen on one port: a join's is the member's it dials.
    let port = opts.port.or_else(|| table.listen_port()).unwrap_or(first.1);
    // An issuer's own code is refused as its own fleet ([`not_an_issuer`]).
    let issues = !matches!(
        std::fs::symlink_metadata(places.state.join(CODE_FILE)),
        Err(e) if e.kind() == io::ErrorKind::NotFound
    );
    if !issues && port == first.1 && same_host(&host, &first.0) {
        let at = member::host_port(&host, port);
        return Err(Failure::Usage(match opts.port {
            Some(named) => format!(
                "this machine would register and listen at {at}, the place of the member it \
                 dials: `--port {named}` is that member's own port there, so name another with \
                 `--port <n>`; nothing was written here"
            ),
            None => format!(
                "this machine would register and listen at {at}, the place of the member it \
                 dials: a member listens on a port of its own there, so name one with `--port \
                 <n>`; nothing was written here"
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
        &listen_on(&first.0, port),
        table.written_level(opts.level_over(&table), regrant),
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
    let issuer_at = member::host_port(&first.0, first.1);
    let registration = Registration {
        node: me.node().to_owned(),
        host,
        port,
        version: aterm_types::version::app_version().to_owned(),
        key: None,
        signed: None,
    }
    .signed_by(&me, aterm_time::unix_millis().unwrap_or_default());
    let keys = Keyring::read(&places.state);
    let (joined, at, issuer_key) = match register_with(
        &dials,
        (&secret, &me),
        (&keys, bound.as_ref()),
        &registration,
        &|| {
            if browsed {
                Vec::new()
            } else {
                advertised_places(opts, &dials)
            }
        },
    ) {
        Ok(registered) => registered,
        Err(why) => {
            if why.at_capacity() {
                opts.saw_capacity
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
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
    // A sync's join is made only while this machine holds the fleet the sync found it in.
    let began = opts.synced.as_ref().map_or(Began::Here, Began::Under);
    // The join's first write, from what it reads again under the book's lock (a leave, an init
    // or a join may have come while it dialled). The marking, the key's write and the drop are
    // its one take, so no other edit comes between them (an older aterm's write drops every
    // mark).
    let Keyed {
        same_fleet,
        named,
        replaced,
        holds,
        off,
    } = member::write_held(&places.state, began, |book| {
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
        if opts.synced.is_some() && in_state(places, member::OFF_FILE)? {
            say(&out);
            return Err(failed(format!(
                "fleet sync is off after aterm fleet leave, which came while it joined, so \
                 nothing of the default fleet was written here\n  The issuer at {issuer_at} has \
                 recorded this machine (its `aterm fleet peers` lists it until `aterm fleet peers \
                 rm` there)."
            )));
        }
        if same_fleet {
            bound_issuer(places, &secret)
                .map_err(|why| unfinished(&out, &issuer_at, why, not_keyed))?;
        }
        // A person's join supersedes the default grant a sync's join owes.
        if opts.synced.is_none() {
            book.remove(GRANT_OWED_FILE).map_err(|e| {
                unfinished(
                    &out,
                    &issuer_at,
                    format!("{}: {e}", places.state.join(GRANT_OWED_FILE).display()),
                    not_keyed,
                )
            })?;
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
        // A join ends a leave's opt-out of sync, in the key's take, where no leave comes between.
        let off_path = places.state.join(member::OFF_FILE);
        let off = match book.remove(member::OFF_FILE) {
            Ok(true) => Some(format!(
                "  off        {} removed (`aterm fleet leave` wrote it): fleet sync is on again\n",
                off_path.display()
            )),
            Ok(false) => None,
            Err(e) => Some(format!(
                "  off        {} could not be removed ({e}), so fleet sync stays off\n",
                off_path.display()
            )),
        };
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
            off,
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
    let (config_line, allow, grants_gone, named, granted) =
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
            // A sync's join owes the default grant where aterm.toml has a level it may replace, or
            // the content beside the record a pass stopped here left: the record goes first, so
            // a stop between the two writes leaves it for the next pass. A vault admin owes it as
            // every member does (docs/DESIGN-fluid-swarm-2026-10-28.md F2).
            let owed_content = in_state(places, GRANT_OWED_FILE)?
                && now.level() == Some(FleetScope::Content.word())
                && now.drive_from.is_empty();
            let owes = opts.synced.is_some() && (default_grant_stands(&now) || owed_content);
            if owes {
                owe_grant(places)?;
            }
            let allow = now.written_level(opts.level_over(&now), ungrant.is_some());
            let (line, gone) = write_fleet(
                book,
                &config,
                &places.config,
                (&listen_on(&first.0, port), allow),
                ungrant,
            )?;
            at_step("grant");
            let granted = if owes {
                grant_default(places, book)?
            } else {
                None
            };
            // A sync's record of the fleet it put this Mac in goes with the level it wrote.
            if opts.synced.is_some() {
                write_synced(places, &secret).map_err(failed)?;
            }
            let grants_gone = gone.is_some();
            Ok((
                line + &gone.unwrap_or_default(),
                allow,
                grants_gone,
                named,
                granted,
            ))
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
        port: joined.issuer.port,
        version: Some(joined.issuer.version.clone()),
        seen_ms: aterm_time::unix_millis(),
        via: Via::Issuer,
        fleet: Some(secret.mark()),
        signed: None,
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
    let unserved = serves_new_key(places, &secret);
    out.push_str(unserved.as_deref().unwrap_or_default());
    out.push_str(off.as_deref().unwrap_or_default());
    if allow == FleetScope::Facts.word() {
        out.push_str(
            "  allow      facts: the fleet reads programs, agent states, roles, task records and \
             models here, no tab title, cwd, prompt, agent's words or shell's command line; \
             `aterm fleet allow content` lets it\n",
        );
    }
    out.push_str(&format!(
        "  issuer     {} at {issuer_at} recorded this machine as {} ({} peer{} in its book){}\n\
         {}\n",
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
        follows(
            unserved.as_deref(),
            "\nOn a Mac, the local-network consent macOS asks for at aterm's first connection \
             into the LAN covers the survey's reads too."
        ),
    ));
    if let Some(granted) = granted {
        out.push('\n');
        out.push_str(&granted);
    }
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
    /// The line for the [`member::OFF_FILE`] it removed, or could not.
    off: Option<String>,
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
    /// It was; the machine that answered refused, or answered no join (its words).
    Registration(String),
    /// It was; no answer to the registration came.
    Unanswered(String),
}

impl JoinFailed {
    /// Whether the failure matches an overloaded issuer listener at connection or per-address
    /// capacity, so `sync.fleet` retries within minutes instead of waiting a full pass.
    fn at_capacity(&self) -> bool {
        match self {
            Self::Dial(e) => e.at_capacity(),
            Self::Unanswered(_) => true,
            Self::PlaceHeld { .. }
            | Self::OtherKey(_)
            | Self::Unproved(_)
            | Self::Registration(_) => false,
        }
    }
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
    /// The node this machine binds that answered as none or under another key, whole, as a
    /// member's `peers rm` takes its issuer while no copy of its book says which other members a
    /// shorter word names ([`named_here`]).
    fn bound_node(&self) -> &str {
        match self {
            Self::OtherKey(node) | Self::Unproved(node) => node,
            _ => "<its node>",
        }
    }

    fn advice(&self) -> Advice {
        match self {
            Self::PlaceHeld { .. } => Advice::PlaceHeld,
            Self::Registration(words) if words.contains(NO_ISSUER) => Advice::NoIssuer,
            Self::Registration(words) if words.contains(member::BEFORE_KEYS) => Advice::BeforeKeys,
            Self::Registration(words) if words.contains(member::REBIND) => Advice::NodeBound,
            Self::Registration(words) if words.contains(member::OWN_NODE) => Advice::IssuersNode,
            Self::Registration(words) if words.contains(member::MOVED_ON) => Advice::MovedOn,
            Self::OtherKey(_) | Self::Unproved(_) => Advice::IssuerKey,
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
             peers rm {}` here takes the old binding out; then join again.",
            why.bound_node()
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

/// The join's registration with the first of `dials` whose member records it (any member does,
/// F17): the answer, where it answered and the key it proved. One that answers another key than
/// the book binds its node to, proves no node the book binds a key to, or is an aterm before 0.147
/// that records no member, is passed over for the next; the first place's failure stands.
fn register_with(
    dials: &[(String, u16)],
    (secret, me): (&FleetSecret, &NodeIdentity),
    (keys, _bound): (&Keyring, Option<&(String, NodeKey)>),
    registration: &Registration,
    more: &dyn Fn() -> Vec<(String, u16)>,
) -> Result<(member::Joined, IpAddr, Option<NodeKey>), JoinFailed> {
    // Only another node's proof there is evidence the place is taken: nothing answering, another
    // secret or this machine's own listener registers as before.
    if let Ok(there) = member::dial(
        &registration.host,
        None,
        registration.port,
        (secret, Some(me)),
        FleetOp::Book,
        peer_read_within(),
    ) && let Some(other) = there.peer.filter(|proof| proof.node != me.node())
    {
        return Err(JoinFailed::PlaceHeld {
            place: member::host_port(&registration.host, registration.port),
            node: other.node,
        });
    }
    let started = Instant::now();
    // The first place's failure is the one the report names: the place named, or heard last.
    let mut first = None;
    // The places it knows, then, once none of them answered, those members advertise.
    let mut advertised = None;
    let mut known = dials.iter().cloned();
    while let Some((host, port)) = known
        .next()
        .or_else(|| advertised.get_or_insert_with(|| more().into_iter()).next())
    {
        let left = (started + JOIN_WITHIN).saturating_duration_since(Instant::now());
        if left.is_zero() && first.is_some() {
            break;
        }
        match register_at(
            (host.as_str(), port),
            (secret, me),
            keys,
            registration,
            left,
        ) {
            Ok(registered) => return Ok(registered),
            // An aterm before 0.147 that holds no join code records no member, another may.
            Err(refused @ JoinFailed::Registration(_))
                if matches!(refused.advice(), Advice::NoIssuer) =>
            {
                first.get_or_insert(refused);
            }
            // A member's own words about this Mac hold wherever it is asked.
            Err(refused @ JoinFailed::Registration(_)) => return Err(refused),
            Err(why) => {
                first.get_or_insert(why);
            }
        }
    }
    Err(first.unwrap_or_else(|| JoinFailed::Unanswered("no member was dialled".to_owned())))
}

fn register_at(
    (host, port): (&str, u16),
    (secret, me): (&FleetSecret, &NodeIdentity),
    keys: &Keyring,
    registration: &Registration,
    within: Duration,
) -> Result<(member::Joined, IpAddr, Option<NodeKey>), JoinFailed> {
    let started = Instant::now();
    let within = within.max(Duration::from_secs(1));
    let mut reached = member::dial(host, None, port, (secret, Some(me)), FleetOp::Join, within)
        .map_err(JoinFailed::Dial)?;
    if let Some(proof) = &reached.peer
        && keys.bound(&proof.node).is_some_and(|key| key != proof.key)
    {
        return Err(JoinFailed::OtherKey(proof.node.clone()));
    }
    let deadline = (started + within).max(Instant::now() + Duration::from_secs(1));
    let joined =
        member::register_by(&mut reached, registration, deadline).map_err(|e| match e {
            member::Unregistered::Answered(why) => JoinFailed::Registration(why),
            member::Unregistered::Unanswered(why) => JoinFailed::Unanswered(why),
        })?;
    let key = match &reached.peer {
        Some(proof) if proof.node == joined.issuer.node => Some(proof.key),
        Some(proof) => {
            return Err(JoinFailed::Registration(format!(
                "the member answered as node {} and proved node {}",
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
        [] => narrowing(places, || list_peers(places, peer_read_within())),
        [flag, secs] if flag.as_str() == "--timeout" => {
            let within = check_within(secs)?;
            narrowing(places, || list_peers(places, within))
        }
        [add, target] if add.as_str() == "add" => {
            let (host, port) = member::parse_host_port(target, Some(member::default_port()))
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
            // A member reads its issuer's book before the lock, which no dial holds; what that
            // read missed stands only where the lock finds the fleet and issuer it was made under.
            let began = ReadUnder::now(places, Held::read(&places.state), true);
            let secret = began.held.secret().cloned();
            let within = issuer_read_within();
            let check = (Instant::now() + within, within);
            let me = own_identity(places);
            let (read, mut unread) =
                copy_first(places, (secret.as_ref(), me.as_ref()), what, check);
            // A word no book here names may name a member that joined since the copy, as a drive
            // reads the issuer's book again for one.
            if !read
                && let Some(secret) = &secret
                && names_nobody(places, what)
                && let Err(why) = copy_again(places, secret, me.as_ref(), check)
            {
                unread = Some(why);
            }
            at_step("rm");
            member::write_held(&places.state, Began::Here, |book| {
                let moved = ReadUnder::now(places, book.held().clone(), true) != began;
                remove_peer(places, book, what, unread.as_ref().filter(|_| !moved))
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
/// lock (`unread`: why the read of the issuer's book before it brought none).
fn remove_peer(
    places: &Places,
    book: &HeldBook<'_>,
    what: &str,
    unread: Option<&Missed>,
) -> Result<String, Failure> {
    this_machine_alone(places, what).map_err(failed)?;
    let listed = member::read_peers(&places.state).map_err(failed)?;
    let found = named_here(places, &listed, what, unread).map_err(failed)?;
    // Every node that drives here before: the entry's own, and on a member each node only the copy
    // of the issuer's book, which goes with the issuer, binds.
    let drove = driving(places);
    let removed = match book.update(|peers| {
        let others = others_listed(places, peers);
        member::remove(peers, &others, what)
    }) {
        Ok(removed) => removed,
        Err(refused) => {
            return drop_copied_grant(places, book, what, unread)?.ok_or_else(|| {
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
                        None => match unread.filter(|_| found.is_none()) {
                            Some(missed) => format!(
                                "{}, and nothing was removed{}{} (`aterm fleet peers` says why)",
                                unnamed_unread(what, missed),
                                granted_node(places, what).map_or_else(String::new, |node| {
                                    format!(" (node {}'s drive grant stays)", short(&node))
                                }),
                                missed.again()
                            ),
                            None => refused,
                        },
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
    let still = driving(places);
    let undriving: Vec<&str> = drove
        .iter()
        .filter(|node| !still.contains(node))
        .map(String::as_str)
        .collect();
    if !undriving.is_empty() {
        out.push_str(&undriven(&undriving));
    }
    match removed.via {
        Via::Joined => out.push_str(STILL_READS),
        Via::Issuer => out.push_str(NO_ISSUER_NAMED),
        _ => {}
    }
    Ok(out)
}

/// Whether a grant in aterm.toml lets `node` drive this Mac under the key this Mac's books bind it
/// to now ([`DriveGrant::admitting`], as the listener's admission asks).
fn drives_here(places: &Places, node: &str) -> bool {
    let grants = granted_here(places);
    !grants.is_empty()
        && Keyring::read(&places.state)
            .bound(node)
            .is_some_and(|key| DriveGrant::admitting(&grants, node, key).is_some())
}

/// Every node [`drives_here`] lets drive this Mac now, among the nodes its books bind.
fn driving(places: &Places) -> Vec<String> {
    let grants = granted_here(places);
    if grants.is_empty() {
        return Vec::new();
    }
    let keys = Keyring::read(&places.state);
    member::bound_peers(&places.state)
        .into_iter()
        .filter_map(|peer| peer.node)
        .filter(|node| {
            keys.bound(node)
                .is_some_and(|key| DriveGrant::admitting(&grants, node, key).is_some())
        })
        .collect()
}

/// The drive grants aterm.toml names, none below drive.
fn granted_here(places: &Places) -> Vec<DriveGrant> {
    ConfigFile::open(&places.config)
        .ok()
        .and_then(|config| fleet_table(text_of(&config).ok()?).ok())
        .filter(|table| table.level() == Some(aterm_net::fleet::DRIVE_LEVEL))
        .map(|table| {
            table
                .drive_from
                .iter()
                .filter_map(|word| DriveGrant::from_word(word))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `what` names no peer here ([`named_here`]), nor this machine.
fn names_nobody(places: &Places, what: &str) -> bool {
    member::read_peers(&places.state)
        .is_ok_and(|book| matches!(named_here(places, &book, what, None), Ok(None)))
        && names_this_machine(places, what).is_none()
        && at_own_place(places, what).is_none()
}

/// How a verb says `word` names no peer here, cause first: on the issuer, none its book records;
/// on a member, none it knows, while the issuer's book was not read (`missed`), or names none, or
/// with no issuer named whose book could.
fn names_no_peer(places: &Places, word: &str, missed: Option<&Missed>) -> String {
    let shown = aterm_codec::tty::one_line(word, 40);
    match missed {
        _ if in_state(places, CODE_FILE).unwrap_or(false) => {
            format!("`{shown}` names no peer this machine's book records (`aterm fleet peers`)")
        }
        Some(missed) => format!(
            "{}{} (`aterm fleet peers` says why)",
            unnamed_unread(word, missed),
            missed.again()
        ),
        None if !names_issuer(places) => format!(
            "`{shown}` names no peer this machine knows, and its book names no issuer whose book \
             could (`aterm fleet join` with the issuer's join code binds one)"
        ),
        None => format!(
            "`{shown}` names no peer this machine knows, nor any its issuer's book names (`aterm \
             fleet peers`)"
        ),
    }
}

/// The silence of this machine's issuer a refusal of `word` waits on: `missed` where it was
/// silent and `word` names the issuer, whose book may say which Mac it is a moment later.
fn silent_issuer<'a>(
    places: &Places,
    word: &str,
    missed: Option<&'a Missed>,
) -> Option<&'a Missed> {
    missed.filter(|missed| {
        missed.silent
            && member::read_peers(&places.state)
                .is_ok_and(|book| loosely_named_issuer(places, &book, word).is_some())
    })
}

/// How a verb says `what` names no peer here while its issuer's book, which may name it, was not
/// read (`missed`), cause first.
fn unnamed_unread(what: &str, missed: &Missed) -> String {
    format!(
        "`{}` names no peer this machine knows, and {}, so no book here says which Mac it is",
        aterm_codec::tty::one_line(what, 40),
        missed.why
    )
}

/// The node of the drive grant in aterm.toml that `what` names, where one does.
fn granted_node(places: &Places, what: &str) -> Option<String> {
    let config = ConfigFile::open(&places.config).ok()?;
    let table = fleet_table(text_of(&config).ok()?).ok()?;
    table
        .drive_from
        .iter()
        .map(|word| DriveGrant::node_of(word))
        .find(|node| member::names_node(node, what))
}

/// A member whose book names no issuer reads only the peers named here.
const NO_ISSUER_NAMED: &str = "  issuer     this machine names no issuer now, so it reads only the \
                               peers named here: `aterm fleet join` with the issuer's join code \
                               binds it again\n";

/// The `[[fleet.peers]]` entry `what` names, as `peers` lists it.
fn configured_entry(places: &Places, what: &str) -> Option<String> {
    configured(places)
        .into_iter()
        .find(|(_, peer)| matches!(member::find(std::slice::from_ref(peer), what), Ok(Some(_))))
        .map(|(entry, _)| entry)
}

/// Each `[[fleet.peers]]` entry of aterm.toml that is a host, beside the peer it names; none
/// where aterm.toml cannot be read.
fn configured(places: &Places) -> Vec<(String, Peer)> {
    let Some(table) = ConfigFile::open(&places.config)
        .ok()
        .and_then(|config| fleet_table(text_of(&config).ok()?).ok())
    else {
        return Vec::new();
    };
    table
        .peers
        .into_iter()
        .filter_map(|entry| {
            let (host, port) =
                member::parse_host_port(&entry, Some(member::default_port())).ok()?;
            Some((entry, manual_peer(host, port)))
        })
        .collect()
}

/// The peers beside `book` that a word may name ([`member::named`]): the whole copy of the
/// issuer's book, then the `[[fleet.peers]]` entries.
fn others_listed(places: &Places, book: &[Peer]) -> Vec<Peer> {
    let mut others = issuer_copies(places, book);
    others.extend(configured(places).into_iter().map(|(_, peer)| peer));
    others
}

/// The one peer `what` names in `book` and beside it ([`others_listed`], [`member::named`]), for
/// every verb that takes a peer's word; a word that names the issuer other than by its whole node
/// id or its address and port is refused before any count while no copy is held
/// ([`uncopied_issuer`]) or the read of its book before it brought none to keep (`unread`: why,
/// [`copy_first`]).
/// # Errors
/// `what` names more than one peer, or names the issuer otherwise while no book here says which
/// other members it names.
fn named_here(
    places: &Places,
    book: &[Peer],
    what: &str,
    unread: Option<&Missed>,
) -> Result<Option<Peer>, String> {
    // Before any count: one read off a copy the issuer could not confirm may miss a member.
    if let Some(issuer) = loosely_named_issuer(places, book, what)
        && (unread.is_some() || uncopied_issuer(places, book).is_some())
    {
        return Err(format!(
            "no book here says which members `{}` names beside this machine's issuer ({} at {}), \
             since {}: name it by its node id ({}) or its address and port ({}){}",
            aterm_codec::tty::one_line(what, 80),
            short(issuer.node.as_deref().unwrap_or("-")),
            issuer.target(),
            unread.map_or(
                "this machine holds no copy of its issuer's book",
                |missed| { missed.why.as_str() }
            ),
            issuer.node.as_deref().unwrap_or("-"),
            issuer.target(),
            if unread.is_some_and(|missed| missed.silent) {
                ", or try again once it answers"
            } else {
                ""
            }
        ));
    }
    let others = others_listed(places, book);
    let identities = roster_identities(places);
    Ok(member::named_with(book, &others, &identities, what)?.cloned())
}

/// Whether `what` names `peer` as no other peer can be named: its whole node id, or its address
/// and port.
fn names_exactly(peer: &Peer, what: &str) -> bool {
    let what = what.trim();
    peer.node
        .as_deref()
        .is_some_and(|node| node.eq_ignore_ascii_case(what))
        || peer.target().eq_ignore_ascii_case(what)
}

/// The issuer `book` names, on a member that holds no copy of that issuer's book
/// ([`member::holds_issuer_book`]): since its join, or since the copy went.
fn uncopied_issuer<'a>(places: &Places, book: &'a [Peer]) -> Option<&'a Peer> {
    let own = own_node(places);
    book.iter()
        .find(|peer| peer.via == Via::Issuer)
        .filter(|issuer| {
            issuer.node.as_deref().is_some_and(|node| {
                own.as_deref() != Some(node) && !member::holds_issuer_book(&places.state, node)
            })
        })
}

/// The issuer `book` names on a member where `what` answers to it, alone or beside other peers,
/// other than by its whole node id or its address and port ([`names_exactly`]), its short names
/// among them ([`member::all_short_named`]): a copy holds no member that joined since this
/// machine's last read, which such a word may name too.
fn loosely_named_issuer<'a>(places: &Places, book: &'a [Peer], what: &str) -> Option<&'a Peer> {
    let own = own_node(places);
    let identities = roster_identities(places);
    book.iter()
        .find(|peer| peer.via == Via::Issuer)
        .filter(|issuer| {
            let alone = std::slice::from_ref(*issuer);
            issuer.node.is_some()
                && issuer.node != own
                && (matches!(member::find_with(alone, &identities, what), Ok(Some(_)))
                    || !member::all_short_named(alone, &[], what).is_empty())
                && !names_exactly(issuer, what)
        })
}

/// On a member that holds no copy of its issuer's book ([`uncopied_issuer`]), or whose word
/// `what` names that issuer loosely ([`loosely_named_issuer`]), that book read afresh and kept as
/// the copy first ([`copy_again`], by `deadline`), as `allow drive` reads it, so the word is read
/// over every member: whether it was read, and why it brought none to keep (`secret`: none where
/// this machine holds no fleet key).
fn copy_first(
    places: &Places,
    (secret, me): (Option<&FleetSecret>, Option<&NodeIdentity>),
    what: &str,
    deadline: (Instant, Duration),
) -> (bool, Option<Missed>) {
    let wanted = member::read_peers(&places.state).is_ok_and(|book| {
        uncopied_issuer(places, &book).is_some()
            || loosely_named_issuer(places, &book, what).is_some()
    });
    if !wanted {
        return (false, None);
    }
    let unkept = |why: &str| Missed {
        why: why.to_owned(),
        silent: false,
        unserved: false,
    };
    let Some(secret) = secret else {
        return (
            false,
            Some(unkept(
                "this machine holds no fleet key to read its issuer's book under",
            )),
        );
    };
    match copy_again(places, secret, me, deadline) {
        Ok(true) => (true, None),
        Ok(false) => (
            true,
            Some(unkept(
                "its issuer's book was read but not kept as this machine's copy (its issuer \
                 proved no key this machine binds it to, or the fleet held here moved meanwhile)",
            )),
        ),
        Err(why) => (true, Some(why)),
    }
}

/// A removed node may keep the fleet's secret, which the book does not take back.
const STILL_READS: &str = "  reads      it may keep the fleet's secret (it does unless it left), and while it \
                           does it reads this fleet's surveys: only `aterm fleet init --rotate` \
                           on the issuer takes that back (every member joins again)\n";

/// `peers rm` of a node only this machine's copy of its issuer's book names.
/// # Errors
/// The book cannot be read, `what` names more than one peer ([`named_here`]), or aterm.toml
/// cannot be edited.
fn drop_copied_grant(
    places: &Places,
    held: &HeldBook<'_>,
    what: &str,
    unread: Option<&Missed>,
) -> Result<Option<String>, Failure> {
    let book = member::read_peers(&places.state).map_err(failed)?;
    let Some(peer) = named_here(places, &book, what, unread).map_err(failed)? else {
        return Ok(None);
    };
    if peer.via != Via::IssuerBook || book.iter().any(|entry| entry.node == peer.node) {
        return Ok(None);
    }
    let Some(node) = peer.node.as_deref() else {
        return Ok(None);
    };
    let Some(gone) = drop_grants(places, held, |word| DriveGrant::node_of(word) == node)? else {
        return Ok(None);
    };
    let still = drives_here(places, node);
    Ok(Some(format!(
        "aterm fleet peers: {} ({}) stays in the fleet issuer's book, which this machine copies \
         (`aterm fleet peers rm` on the issuer takes it out); {}\n{}{}",
        peer.target(),
        short(node),
        if still {
            "it may still drive this Mac, as [fleet] drive_from's \"*\" lets every member its \
             issuer binds (`aterm fleet allow content` takes that back)"
        } else {
            "it drives this Mac no more"
        },
        gone.row(),
        if still {
            String::new()
        } else {
            undriven(&[node])
        }
    )))
}

/// The line for the nodes a write let drive this Mac no more: the running aterm refuses their next
/// drive at once, but cuts their drives and lifts their holds only at its next read of `[fleet]`.
fn undriven(nodes: &[&str]) -> String {
    let named: Vec<&str> = nodes.iter().map(|node| short(node)).collect();
    let (noun, its, it) = match nodes {
        [_] => ("node", "its", "it"),
        _ => ("nodes", "their", "they"),
    };
    format!(
        "  holds      {noun} {} may drive this Mac no more: an aterm running here refuses {its} \
         next drive from now on, and cuts {its} open drives and lifts any hold {it} took here at \
         its next read of [fleet] (each {} s), so a write here before then may still meet `ERR \
         halted`\n",
        named.join(", "),
        aterm_net::fleet::KEY_READ_EVERY.as_secs()
    )
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
        match member::parse_host_port(entry, Some(member::default_port())) {
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
        signed: None,
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
            let dropped = keep_book(places, &secret, (&reads, &book), own.as_deref());
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
    let place = own_place(table)?;
    let me = me?;
    let ip = place.ip();
    let target = member::Target {
        host: ip.to_string(),
        addr: Some(ip),
        port: place.port(),
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

/// The place [`own_listener`] dials: `[fleet] listen`, a wildcard's at the loopback.
fn own_place(table: &FleetTable) -> Option<SocketAddr> {
    let listen: SocketAddr = table.listen.as_deref()?.parse().ok()?;
    let ip = match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::from(std::net::Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::from(std::net::Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    Some(SocketAddr::new(ip, listen.port()))
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
/// [`SERVES_NEW_KEY`], until its `[fleet] listen` place proves this machine's node under it, so a
/// reader or a join that comes next meets the new key; the line saying why it still does not,
/// which the report's close then follows ([`follows`]). Only that proof is served: the wait ends
/// at once without a line where nothing listens there and no aterm here answers, and with one
/// where the place takes a connection and finishes no handshake beside an aterm here that answers
/// nothing ([`answers_no_handshake`]) or beside none ([`held_by_another`]), or where nothing
/// listens beside one that answers nothing ([`nothing_listens`]).
fn serves_new_key(places: &Places, secret: &FleetSecret) -> Option<String> {
    let table = ConfigFile::open(&places.config)
        .map_err(failed)
        .and_then(|config| fleet_table(text_of(&config)?).map_err(failed));
    let (Some(me), Ok(table)) = (own_identity(places), table) else {
        return None;
    };
    let at = own_place(&table)?;
    let after = format!(
        " {} s after the new key was written",
        SERVES_NEW_KEY.as_secs()
    );
    let line = |line: String| Some(format!("  listener   {line}\n"));
    let end = Instant::now() + SERVES_NEW_KEY;
    let mut here = None;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        let seen = match own_listener(
            &table,
            (secret, Some(&me)),
            left.clamp(PROBE_MIN, PROBE_MAX),
        ) {
            None | Some(Ok(_)) => return None,
            Some(Err(why)) => why,
        };
        match &seen {
            Unreached::Connect(at, e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                match here.get_or_insert_with(|| aterm_here(left.max(PROBE_MIN))) {
                    Here::Absent => return None,
                    asked @ (Here::Stopped(_) | Here::Silent(_)) => {
                        return line(nothing_listens(places, *at, "", asked));
                    }
                    Here::Answers(_) => {}
                }
            }
            // A stopped aterm's bound listener still takes a connection, in the kernel.
            Unreached::Deadline(member::Stage::Handshake, _) => {
                let asked = here.get_or_insert_with(|| aterm_here(left.max(PROBE_MIN)));
                if !matches!(asked, Here::Answers(_)) {
                    return line(
                        answers_no_handshake(at, "", asked)
                            .unwrap_or_else(|| held_by_another(places, at, "", asked)),
                    );
                }
            }
            // Another secret, a connection cut (a listener between off and on after a leave), or
            // anything else: not served yet.
            _ => {}
        }
        if Instant::now() >= end {
            // Its word as the wait ends, not as it began.
            return line(match seen {
                Unreached::Handshake(at, FleetError::ListenerWrongSecret) => format!(
                    "{at} still answers under another secret{after}: an aterm here that has not \
                     read it, or another fleet's listener holding that port ({})",
                    status_says(&aterm_here(PROBE_MAX))
                ),
                Unreached::Connect(at, e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                    nothing_listens(places, at, &after, &aterm_here(PROBE_MAX))
                }
                Unreached::Deadline(member::Stage::Handshake, _) => {
                    let asked = aterm_here(PROBE_MAX);
                    answers_no_handshake(at, &after, &asked)
                        .unwrap_or_else(|| held_by_another(places, at, &after, &asked))
                }
                why => format!(
                    "this machine's node did not prove itself at {at} under the new key{after} \
                     ({}): no aterm here serves it, or another program holds that port ({}, and \
                     {})",
                    aterm_codec::tty::one_line(&why.to_string(), 160),
                    status_says(&aterm_here(PROBE_MAX)),
                    member::moves_listener(&places.state)
                ),
            });
        }
        std::thread::sleep(PROBE_MIN.min(end.saturating_duration_since(Instant::now())));
    }
}

/// What `aterm ctl status` here says of the listener, for a place another program may hold: the
/// `fleet=` word the aterm here answered, or, where none answered one, what it would read.
fn status_says(here: &Here) -> String {
    match here {
        Here::Answers(Some(word)) => format!("`aterm ctl status` here reads fleet={word}"),
        Here::Absent => "no aterm runs here, and one started here would read fleet=retrying while \
                         another program holds that port"
            .to_owned(),
        _ => "the aterm here did not say how its listener stands: it would read fleet=retrying \
              while another program holds that port"
            .to_owned(),
    }
}

/// An aterm here, as [`aterm_here`] found it asked.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Here {
    /// No control socket here takes a connection.
    Absent,
    /// One takes a connection and answered no request, from this stopped process.
    Stopped(u32),
    /// One takes a connection and answered no request; the process behind it, where known.
    Silent(Option<u32>),
    /// One answers; its `status`'s `fleet=` word, where the answer names one.
    Answers(Option<String>),
}

/// A test's answer to how an aterm here stands ([`aterm_here`]); with none, no aterm runs here.
#[cfg(test)]
type AtermHere = Box<dyn FnMut() -> Here>;

#[cfg(test)]
thread_local! {
    static ATERM_HERE: std::cell::RefCell<Option<AtermHere>> =
        const { std::cell::RefCell::new(None) };
}

/// Whether this machine's own node proves itself at its `[fleet] listen` place under the key held
/// within `within` ([`own_listener`]), as `peers`' header says it listens there.
fn listens_here(places: &Places, within: Duration) -> bool {
    let table = ConfigFile::open(&places.config)
        .map_err(failed)
        .and_then(|config| fleet_table(text_of(&config)?).map_err(failed));
    let (Ok(secret), Some(me), Ok(table)) = (
        aterm_net::fleet::read_key_file(&places.state),
        own_identity(places),
        table,
    ) else {
        return false;
    };
    matches!(
        own_listener(&table, (&secret, Some(&me)), within),
        Some(Ok(_))
    )
}

/// How an aterm here stands, asked `status` within `within` ([`aterm_ctl::front_door_ask`]): it
/// counts as running only where its control socket answers, never where it merely takes a
/// connection, as a stopped or frozen one does.
fn aterm_here(within: Duration) -> Here {
    #[cfg(test)]
    {
        let _ = within;
        ATERM_HERE.with(|hook| {
            hook.borrow_mut()
                .as_mut()
                .map_or(Here::Absent, |hook| hook())
        })
    }
    #[cfg(not(test))]
    match aterm_ctl::front_door_ask("status\n", within) {
        aterm_ctl::FrontDoorAnswer::Absent => Here::Absent,
        aterm_ctl::FrontDoorAnswer::Silent(Some(pid)) if stopped(pid) => Here::Stopped(pid),
        aterm_ctl::FrontDoorAnswer::Silent(pid) => Here::Silent(pid),
        aterm_ctl::FrontDoorAnswer::Answered(line) => Here::Answers(
            line.split_whitespace()
                .find_map(|word| word.strip_prefix("fleet="))
                .map(str::to_owned),
        ),
    }
}

/// Whether process `pid` is stopped (a `SIGSTOP`, a tracer).
#[cfg(not(test))]
fn stopped(pid: u32) -> bool {
    #[cfg(target_vendor = "apple")]
    {
        i32::try_from(pid)
            .ok()
            .and_then(aterm_uds::process::read_short_info)
            .is_some_and(|info| info.pbsi_status == aterm_uds::process::BsdShortInfo::SSTOP)
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // `T` is a job-control stop, `t` a tracer's: either way it answers nothing until resumed.
        i32::try_from(pid).ok().is_some_and(|id| {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| aterm_uds::process::procfs::stat_start(&stat, id))
                .is_some_and(|(state, _)| matches!(state, 'T' | 't'))
        })
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    {
        let _ = pid;
        false
    }
}

/// THE ONE SENTENCE for this machine's own `[fleet] listen` place `at` where nothing listens,
/// `after` a verb's wait: its cause as the aterm here stands (`here`, [`aterm_here`]) and the
/// remedy, in an init, a join, a rotate and `peers` alike.
fn nothing_listens(places: &Places, at: SocketAddr, after: &str, here: &Here) -> String {
    let another = |whose: &str| {
        format!(
            "reads another aterm.toml than {}{whose} (one relaunched to read this one listens; \
             `aterm explain-config` in one of its tabs names the one it reads)",
            places.config.display()
        )
    };
    let cause = match here {
        Here::Absent => "no aterm runs here (no control socket here takes a connection): one \
                         started here reads [fleet] at launch and listens there"
            .to_owned(),
        Here::Stopped(pid) => format!(
            "the aterm running here (pid {pid}) is stopped, so it answers nothing and listens \
             nowhere: `kill -CONT {pid}` resumes it, and it listens there once it reads [fleet]"
        ),
        Here::Silent(pid) => {
            let pid = pid.map_or_else(|| "<its pid>".to_owned(), |pid| pid.to_string());
            format!(
                "the aterm running here (pid {pid}) takes a connection but answered no request, \
                 so it is frozen or too busy to answer: it listens there once it answers again, \
                 and one that stays frozen once it is quit and opened again (`kill -CONT {pid}` \
                 resumes one that is stopped)"
            )
        }
        Here::Answers(None) => format!(
            "the aterm running here did not say how its listener stands, so it has not read \
             [fleet] yet, it {}, or it refused the listen (`aterm ctl status` in one of its tabs \
             reads fleet=off or refused:<why> then)",
            another("")
        ),
        Here::Answers(Some(word)) => match word.as_str() {
            "off" => format!(
                "the aterm running here reads fleet=off, so it has not read [fleet] yet, or it {}",
                another(", whose [fleet] is off")
            ),
            "listening" => format!(
                "the aterm running here listens elsewhere (fleet=listening), so it has not read \
                 this [fleet] listen yet, or it {}",
                another("")
            ),
            "retrying" => format!(
                "the aterm running here reads fleet=retrying: it has not bound {at} yet (an \
                 address this machine does not hold, or a port another program held when it \
                 tried), and binds it once it can"
            ),
            word => match word.strip_prefix("refused:") {
                Some(why) => format!(
                    "the aterm running here refused the listen (fleet=refused:{why}): {}",
                    refused_listen(places, why, &another(""))
                ),
                None => format!("the aterm running here reads fleet={word}"),
            },
        },
    };
    format!("nothing listens on {at}{after}: {cause}")
}

/// The sentence for this machine's own `[fleet] listen` place `at` that takes a connection and
/// finishes no handshake `after` a verb's wait, beside an aterm here that answers nothing
/// (`here`): its cause and remedy; None beside one that answers, or none.
fn answers_no_handshake(at: SocketAddr, after: &str, here: &Here) -> Option<String> {
    let cause = match here {
        Here::Stopped(pid) => format!(
            "the aterm running here (pid {pid}) is stopped, so a listener it bound takes a \
             connection in the kernel and answers nothing: `kill -CONT {pid}` resumes it"
        ),
        Here::Silent(pid) => {
            let pid = pid.map_or_else(|| "<its pid>".to_owned(), |pid| pid.to_string());
            format!(
                "the aterm running here (pid {pid}) answered no request either, so it is frozen \
                 or too busy to answer: it answers there once it answers again, and one that \
                 stays frozen once it is quit and opened again (`kill -CONT {pid}` resumes one \
                 that is stopped)"
            )
        }
        Here::Absent | Here::Answers(_) => return None,
    };
    Some(format!(
        "{at} takes a connection but finishes no fleet handshake{after}: {cause}"
    ))
}

/// The sentence for this machine's own `[fleet] listen` place `at` that takes a connection and
/// finishes no handshake `after` a verb's wait beside an aterm here that answers, or none
/// (`here`): another program holds that port, as the aterm's own `fleet=` word allows, and the
/// act that moves this machine's listener.
fn held_by_another(places: &Places, at: SocketAddr, after: &str, here: &Here) -> String {
    let stalled = format!("{at} takes a connection but finishes no fleet handshake{after}");
    let moves = member::moves_listener(&places.state);
    let aterm = match here {
        // macOS binds a wildcard beside another program's loopback listen ([`wildcard_refused`]).
        Here::Answers(Some(word)) if word == "listening" => {
            return format!(
                "{stalled}, though the aterm running here reads fleet=listening: another program \
                 holds that port beside its wildcard listen (this machine's own dials of the \
                 loopback reach that program, and a peer's of this Mac's LAN address the aterm) \
                 or while it listens elsewhere, or the aterm is too busy to finish one in time; \
                 {moves}"
            );
        }
        Here::Absent => {
            "no aterm runs here, and one started here reads fleet=retrying until that program \
             lets it go"
                .to_owned()
        }
        Here::Answers(Some(word)) if word == "retrying" => {
            "the aterm running here reads fleet=retrying, and binds it once that program lets it go"
                .to_owned()
        }
        Here::Answers(Some(word)) => format!(
            "the aterm running here reads fleet={word} (`aterm ctl status` here), and once it \
             tries this [fleet] listen, fleet=retrying until that program lets it go"
        ),
        _ => "the aterm running here did not say how its listener stands, and once it tries this \
             [fleet] listen it reads fleet=retrying until that program lets it go"
            .to_owned(),
    };
    format!("{stalled}: another program holds that port, so {aterm}; {moves}")
}

/// What a listener's `refused:<why>` (`why`) says, and its remedy, where `another` is the words
/// for an aterm that reads another aterm.toml than this verb's.
fn refused_listen(places: &Places, why: &str, another: &str) -> String {
    match why {
        "listen" => format!(
            "the [fleet] listen it read is no numeric IP and port, so it has not read this one \
             yet, or it {another}"
        ),
        "allow" => {
            let names_one = ConfigFile::open(&places.config)
                .ok()
                .and_then(|config| fleet_table(text_of(&config).ok()?).ok())
                .is_some_and(|table| table.level().is_some());
            let config = places.config.display();
            if names_one {
                format!(
                    "the [fleet] allow it read is none of \"facts\", \"content\" and \"drive\", \
                     though {config} names one, so it has not read it yet, or it {another}"
                )
            } else {
                format!(
                    "the [fleet] allow it read is none of \"facts\", \"content\" and \"drive\": \
                     `aterm fleet allow facts` (or `content`) writes one in {config}"
                )
            }
        }
        "loopback" => "it listens on the loopback alone (ATERM_DEBUG_LOOPBACK_ONLY in its \
                       environment), and this place is not there: one started without that \
                       guard listens there"
            .to_owned(),
        "nested" => "it was started inside another aterm (from one of its tabs), and leaves the \
                     listen to that one, which does not listen there either: `aterm ctl status` \
                     in one of the outer aterm's tabs says why (its fleet= word), and one \
                     started outside any aterm listens itself"
            .to_owned(),
        "bind" => format!(
            "it could not bind that place (its log's `fleet listener not started: bind` line \
             says why): {}",
            member::moves_listener(&places.state)
        ),
        "thread" => "its listener could not start a thread: one quit and opened again listens \
                     there"
            .to_owned(),
        "stopped" => "its listener stopped: one quit and opened again listens there".to_owned(),
        "unsupported" => "this platform serves no fleet connection".to_owned(),
        key if key.starts_with("key") || key == "no-key" => {
            match aterm_net::fleet::read_key_file(&places.state) {
                Err(e) => no_key(places, &e),
                Ok(_) => format!(
                    "it reads no key it can use, though {} holds one: it has not read it yet (it \
                     reads it every {} s), or it reads another state folder",
                    places.state.join(KEY_FILE).display(),
                    aterm_net::fleet::KEY_READ_EVERY.as_secs()
                ),
            }
        }
        _ => "which names what keeps it from listening".to_owned(),
    }
}

/// Whether a read's answer is this machine's: its own node, proved under its own key.
fn is_this_machine(node: Option<&str>, proven: bool, own: Option<&str>) -> bool {
    proven && own.is_some() && node == own
}

fn own_node(places: &Places) -> Option<String> {
    member::read_node_id(&places.state).ok().flatten()
}

/// Vault paths associated with `places`: a scratch test root holding `.secrets/`, or the live
/// home when `places.state` is the live account's state directory.
fn vault_paths_for(places: &Places) -> Option<aterm_secrets::client::Paths> {
    if let Some(root) = places.state.parent()
        && root.join(".secrets").is_dir()
    {
        return Some(aterm_secrets::client::Paths::for_home(root));
    }
    if aterm_types::dirs::state_dir().as_deref() == Some(places.state.as_path()) {
        return aterm_secrets::client::Paths::live().ok();
    }
    None
}

/// Swarm-roster [`member::PeerIdentity`] rows from the locally applied vault copy, if any.
fn roster_identities(places: &Places) -> Vec<member::PeerIdentity> {
    let Some(paths) = vault_paths_for(places) else {
        return Vec::new();
    };
    let Ok(Some(roster)) = aterm_secrets::client::swarm_roster(&paths) else {
        return Vec::new();
    };
    roster
        .members
        .into_iter()
        .map(|member| member::PeerIdentity {
            node: member.node_id,
            host: None,
            label: Some(member.label),
            aliases: member.aliases,
            vault_name: (!member.vault_name.is_empty()).then_some(member.vault_name),
        })
        .collect()
}

/// This machine's own node, where `word` names it ([`member::names_node`]). Also matches `[fleet]
/// machine` in aterm.toml.
fn names_this_machine(places: &Places, word: &str) -> Option<String> {
    let own = own_node(places)?;
    if member::names_node(&own, word) {
        return Some(own);
    }
    let word = word.trim();
    if !word.is_empty()
        && let Ok(config) = ConfigFile::open(&places.config)
        && let Ok(text) = text_of(&config)
        && let Ok(table) = fleet_table(text)
        && table
            .machine
            .as_deref()
            .is_some_and(|machine| machine.trim().eq_ignore_ascii_case(word))
    {
        return Some(own);
    }
    None
}

/// This machine's own node where `word` names it ([`names_this_machine`]) and no other peer here
/// does ([`member::all_named`] over the book, the copy and the `[[fleet.peers]]` entries), for
/// every verb that takes a peer's word.
/// # Errors
/// `word` names this machine and another peer too: one sentence naming each by its whole node id,
/// this machine as itself.
fn this_machine_alone(places: &Places, word: &str) -> Result<Option<String>, String> {
    let Some(own) = names_this_machine(places, word) else {
        return Ok(None);
    };
    let book = member::read_peers(&places.state).unwrap_or_default();
    let others = others_listed(places, &book);
    let identities = roster_identities(places);
    let beside: Vec<&Peer> = member::all_named_with(&book, &others, &identities, word)
        .into_iter()
        .filter(|peer| peer.node.as_deref() != Some(own.as_str()))
        .collect();
    if beside.is_empty() {
        Ok(Some(own))
    } else {
        Err(member::several(word, Some(&own), &beside))
    }
}

/// This machine's own node, where `word` names its own listener without a dial: its `[fleet]
/// listen` port at its loopback, an interface address or its host name, as `peers add` refuses
/// that place ([`unread_place`]). A verb asks it once no list it reads names the word.
fn at_own_place(places: &Places, word: &str) -> Option<String> {
    let (host, port) = member::parse_host_port(word, Some(member::default_port())).ok()?;
    let config = ConfigFile::open(&places.config).ok()?;
    let table = fleet_table(text_of(&config).ok()?).ok()?;
    (table.listen_port() == Some(port) && this_machine_is(&host))
        .then(|| own_node(places))
        .flatten()
}

fn own_identity(places: &Places) -> Option<NodeIdentity> {
    member::read_node_identity(&places.state).ok().flatten()
}

/// What `read`, a read under `secret`, leaves as the copy of the issuer's book, the members that
/// answered it (`reads`, this machine's own node aside) noted as answering now
/// ([`member::ANSWERED_FILE`]), and the grants a proven book backs no more taken out, in one take
/// of the book's lock while that fleet is held still ([`member::write_held`]): a read that ends
/// after a leave, or after a join of another fleet, keeps nothing of the fleet it read. A read
/// that got no book keeps the copy as it is, and notes its answers still.
fn keep_book(
    places: &Places,
    secret: &FleetSecret,
    (reads, read): (&[member::PeerRead], &BookRead),
    own: Option<&str>,
) -> Option<String> {
    let vault_paths = vault_paths_for(places);
    let log = vault_paths
        .as_ref()
        .and_then(|paths| crate::fleet_log::LocalLog::of(paths).keys());
    let denied = vault_paths
        .as_ref()
        .and_then(|paths| {
            aterm_secrets::client::swarm_denied_nodes(paths)
                .ok()
                .flatten()
        })
        .unwrap_or_default();
    let checked_peers: Vec<Peer> = reads.iter().map(|r| r.peer.clone()).collect();
    member::prune_pushed_facts(&places.state, &checked_peers, &denied, log.as_deref());
    let copy = match read {
        BookRead::Fetched(book) => {
            member::prune_pushed_facts(&places.state, &book.peers, &denied, log.as_deref());
            let mut copies = member::copies_of(book, own);
            copies.retain(|copy| {
                !copy
                    .node
                    .as_deref()
                    .is_some_and(|node| aterm_net::fleet::is_denied_node(node, &denied))
                    && !copy.key.is_some_and(|key| {
                        aterm_net::fleet::is_denied_key(&key, &denied)
                            || log.as_ref().is_some_and(|log| log.holds_out(&key))
                    })
            });
            Some(Some((book.node.as_str(), copies)))
        }
        BookRead::Refused(_) | BookRead::NoIssuer => Some(None),
        BookRead::Unshared(_) | BookRead::Unread(_) => None,
    };
    let answers = answered_now(reads, own);
    if copy.is_none() && answers.is_empty() {
        return None;
    }
    let proven = matches!(read, BookRead::Fetched(book) if book.proven);
    at_step("keep");
    member::write_held(
        &places.state,
        Began::Under(&Held::Key(secret.clone())),
        |book| {
            // A record of when each member last answered: one this machine cannot write costs
            // `peers` its "since" line, never the read.
            let _ = book.note_answered(&answers);
            book.keep_issuer_book(
                copy.as_ref()?
                    .as_ref()
                    .map(|(issuer, copies)| (*issuer, copies.as_slice())),
            )
            .ok()?;
            proven
                .then(|| drop_unbacked_grants(places, book, log.as_deref(), &denied))
                .flatten()
        },
    )
    .ok()
    .flatten()
}

/// The members that answered `reads` now, each by its [`member::answer_key`] with this machine's
/// clock: every answer but this machine's own node's.
fn answered_now(reads: &[member::PeerRead], own: Option<&str>) -> Vec<(String, u64)> {
    let Some(now_ms) = aterm_time::unix_millis() else {
        return Vec::new();
    };
    reads
        .iter()
        .filter(|read| {
            matches!(&read.read, Ok(surveyed)
                if !is_this_machine(surveyed.node.as_deref(), surveyed.proven, own))
        })
        .map(|read| (member::answer_key(&read.peer), now_ms))
        .collect()
}

fn drop_unbacked_grants(
    places: &Places,
    held: &HeldBook<'_>,
    log: Option<&member::MemberKeys>,
    denied: &[String],
) -> Option<String> {
    let keys = Keyring::read(&places.state);
    let gone = drop_grants(places, held, |word| {
        DriveGrant::from_word(word).is_some_and(|grant| {
            !grant.all()
                && (aterm_net::fleet::is_denied_node(&grant.node, denied)
                    || aterm_net::fleet::is_denied_key(&grant.key, denied)
                    || log.is_some_and(|log| log.holds_out(&grant.key))
                    || (!log.is_some_and(|log| log.admits(&grant.key))
                        && keys.bound(&grant.node) != Some(grant.key)))
        })
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
    if peer.port == member::default_port() {
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
            (None, Some(listen)) => {
                listens_words(places, &listing, (listen, within), own.as_deref())
            }
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
    let mut reached = 0;
    let mut last_heard_ms = None;
    // When each member last answered this machine, and the newest such answer among those that
    // did not answer now: what the away line's "since" says. Each member that did not answer
    // gets a line of its own after the table (`last_answers`), so no row's words change.
    let answered = member::read_answered(&places.state);
    let mut answered_ms = None;
    let mut last_answers = Vec::new();
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
            Some(Ok(checked)) => {
                reached += 1;
                match (&peer.node, &checked.node) {
                    (None, Some(node)) => format!(
                        "ok, serves {} (node {}{})",
                        checked.scope.word(),
                        short(node),
                        if checked.proven { ", proven" } else { "" }
                    ),
                    _ => format!("ok, serves {}", checked.scope.word()),
                }
            }
            Some(Err(why)) => {
                // A peer never dialled (a name that did not resolve) says
                // nothing of this machine's route either way.
                if why.dialled() {
                    unreached += 1;
                    last_heard_ms = last_heard_ms.max(peer.seen_ms);
                }
                if why.no_route() {
                    no_route += 1;
                }
                let last = answered.get(&member::answer_key(peer)).copied();
                answered_ms = answered_ms.max(last);
                last_answers.push((
                    peer.node
                        .as_deref()
                        .map_or_else(|| peer.target(), |node| short(node).to_owned()),
                    last,
                ));
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
            } else if peer.is_dormant_at(now_ms) {
                "dormant"
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
    let offset_s = i64::try_from(now_ms / 1000)
        .ok()
        .and_then(aterm_uds::zone::local_offset_at);
    if let Some(away) = away_words(
        (reached, unreached),
        (last_heard_ms, answered_ms),
        now_ms,
        offset_s,
    ) {
        out.push_str(&away);
    }
    out.push_str(&last_answer_words(&last_answers, now_ms, offset_s));
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
/// that it did not prove itself there and what may hold the place, where nothing listens or
/// nothing finishes a handshake as the aterm here, asked within `within`, stands
/// ([`nothing_listens`], [`answers_no_handshake`]); a `listen` that is no IP:port or an `allow`
/// that is no level, which no listener takes, is named as that, and an `allow` beside its node's
/// proof as one the aterm listening there has not read.
fn listens_words(
    places: &Places,
    listing: &Listing,
    (listen, within): (&str, Duration),
    own: Option<&str>,
) -> String {
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
    if let Some(allow) = table.allow.as_deref().filter(|_| table.level().is_none()) {
        let allow = aterm_codec::tty::one_line(allow, 40);
        let writes = format!(
            "`aterm fleet allow facts` (or `content`) writes one in {}",
            places.config.display()
        );
        // Its node's proof there outweighs aterm.toml's word.
        return if matches!(listing.listener, Some(Ok(_))) {
            format!(
                " (it listens on {shown}, though [fleet] allow is \"{allow}\", none of \"facts\", \
                 \"content\" and \"drive\": the aterm listening there has not read it yet, or \
                 reads another aterm.toml, and one that reads it refuses the listen \
                 (fleet=refused:allow); {writes})"
            )
        } else {
            format!(
                " ([fleet] allow is \"{allow}\", none of \"facts\", \"content\" and \"drive\", so \
                 no aterm here listens for the fleet (`aterm ctl status` here reads \
                 fleet=refused:allow): {writes})"
            )
        };
    }
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
    // Asked once, where the place did not prove this machine's node: its `fleet=` word is quoted.
    let asked = std::cell::OnceCell::new();
    let here = || asked.get_or_init(|| aterm_here(within));
    let stalled = match &listing.listener {
        Some(Err(Unreached::Deadline(member::Stage::Handshake, _))) => own_place(table).map(|at| {
            answers_no_handshake(at, " just now", here())
                .unwrap_or_else(|| held_by_another(places, at, " just now", here()))
        }),
        _ => None,
    };
    if let Some(stalled) = stalled {
        return format!(" ([fleet] listen is {shown} and allow is {level}, but {stalled})");
    }
    match &listing.listener {
        Some(Ok(_)) => format!(" (it listens on {shown}, serving {level})"),
        Some(Err(Unreached::Connect(at, e))) if e.kind() == io::ErrorKind::ConnectionRefused => {
            format!(
                " ([fleet] listen is {shown} and allow is {level}, but {})",
                nothing_listens(places, *at, " just now", here())
            )
        }
        Some(Err(why)) => {
            let moves = member::moves_listener(&places.state);
            let held = format!("{}, and {moves}", status_says(here()));
            let (seen, then) = match why {
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
                " ([fleet] listen is {shown} and allow is {level}, but this machine's node did not \
                 prove itself there just now ({}): {then})",
                aterm_codec::tty::one_line(&seen, 160)
            )
        }
        None => format!(
            " ([fleet] listen is {shown} and allow is {level}; this machine holds no node key to \
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
/// a copy of the issuer's book is taken out on the issuer, a member's issuer here by its whole
/// node id, and another secret is told as the issuer that `minted` its code at that time sees it
/// ([`code_minted`]), or as a member.
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
    let here = own == Some(said.as_str())
        && (at.ip().is_loopback() || this_machine_is(&at.ip().to_string()));
    // A member's issuer is taken out only by its whole node id while its book cannot be read
    // ([`named_here`]), and it joins no port of its own.
    if peer.via == Via::Issuer {
        return format!(
            "{} is here: `aterm fleet peers rm {want}` if it left, or rejoin it",
            if here {
                "this machine".to_owned()
            } else {
                format!("node {}", short(said))
            }
        );
    }
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

/// The lines `peers` ends its table with when it dialled peers and none answered (`reached`,
/// `unreached`): this machine is away from every member, and the last this book heard of one
/// (`last_heard_ms`, the newest `seen_ms` of those dialled, a register or heartbeat it took);
/// then, where [`member::ANSWERED_FILE`] holds one, since when (`answered_ms`, the newest time a
/// member not answering now last answered this machine). Each time is a clock at `offset_s`
/// east of UTC (UTC when the zone is unknown) and an age at `now_ms`.
fn away_words(
    (reached, unreached): (usize, usize),
    (last_heard_ms, answered_ms): (Option<u64>, Option<u64>),
    now_ms: u64,
    offset_s: Option<i64>,
) -> Option<String> {
    if reached > 0 || unreached == 0 {
        return None;
    }
    let none = if unreached == 1 {
        "the one peer dialled did not answer".to_owned()
    } else {
        format!("none of the {unreached} peers dialled answered")
    };
    let heard = last_heard_ms.map_or_else(
        || "this book holds no time it heard of one".to_owned(),
        |ms| {
            format!(
                "the last this book heard of one was at {}",
                clock_ago(ms, now_ms, offset_s)
            )
        },
    );
    let mut words = format!("Away from every member: {none}; {heard}.\n");
    if let Some(ms) = answered_ms {
        words.push_str(&format!(
            "Away from every member since {}: the last answer one gave this Mac.\n",
            clock_ago(ms, now_ms, offset_s)
        ));
    }
    Some(words)
}

/// The lines `peers` gives, after its table, to each member that did not answer now (`last`:
/// its node's first eight digits, or its place for a peer with no node id, and when it last
/// answered this machine, [`member::ANSWERED_FILE`]), in the table's order: one line each, which
/// starts with words no row starts with, so a script that reads the rows reads them as it did.
/// Each time is a clock at `offset_s` east of UTC (UTC when the zone is unknown) and an age at
/// `now_ms`.
fn last_answer_words(last: &[(String, Option<u64>)], now_ms: u64, offset_s: Option<i64>) -> String {
    last.iter()
        .map(|(who, ms)| {
            format!(
                "Last answer to this Mac from {}: {}.\n",
                aterm_codec::tty::one_line(who, 64),
                ms.map_or_else(
                    || "none recorded".to_owned(),
                    |ms| clock_ago(ms, now_ms, offset_s)
                )
            )
        })
        .collect()
}

/// `ms` (Unix milliseconds) as a clock at `offset_s` east of UTC (`UTC` said when the zone is
/// unknown) and its age at `now_ms`: `09:00 (2h ago)`.
fn clock_ago(ms: u64, now_ms: u64, offset_s: Option<i64>) -> String {
    let at = i64::try_from(ms).unwrap_or(i64::MAX);
    format!(
        "{}{} ({} ago)",
        crate::supervise::occupancy::said::clock_words(at, offset_s.unwrap_or(0)),
        if offset_s.is_some() { "" } else { " UTC" },
        age(now_ms.saturating_sub(ms))
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

fn publish(places: &Places, args: &[String]) -> Result<String, Failure> {
    if !args.is_empty() {
        return Err(Failure::Usage("publish takes no flags".into()));
    }
    let code = member::read_join_code(&places.state)
        .map_err(|why| failed(format!("this machine minted no fleet's code here: {why}")))?;
    // The code is the secret alone (F17); the vault's entry ends with a place, a member to dial
    // first, which an aterm before 0.147 needs to read the code at all: the one the code's file
    // names (the init's, its `--host` among them), else this Mac's.
    let text = match &code.hint {
        Some(_) => code.written(),
        None => {
            let config = ConfigFile::open(&places.config).map_err(failed)?;
            let port = fleet_table(text_of(&config)?)
                .map_err(failed)?
                .listen_port()
                .unwrap_or_else(member::default_port);
            let host = aterm_uds::process::lan_host_name()
                .filter(|host| member::is_host(host))
                .ok_or_else(|| failed("this machine's host name is no host a member can dial"))?;
            code.text_at((&host, port))
        }
    };
    let vault = aterm_secrets::client::Paths::live().map_err(failed)?;
    aterm_secrets::client::admin_publish_alab_fleet(
        &vault,
        &aterm_secrets::SecretBytes::from(text),
    )
    .map_err(failed)?;
    Ok("published the existing fleet's join code to every ALab vault member; members converge on their next vault apply".into())
}

/// The record of the default ALab fleet a sync last found or put this Mac in, as that fleet's
/// mark ([`FleetSecret::mark`], which tells nothing of its secret): a sync moves this Mac to the
/// vault's newer code only away from that fleet (one a person joined by hand to the code the
/// vault published then included), never from another fleet a person joined by hand. Written
/// under the book's lock, by a sync's join in its aterm.toml take or by [`record_synced`]; a
/// leave removes it.
const SYNCED_FILE: &str = "fleet-synced";

/// The default grant a sync's join owes this Mac, written in that join's aterm.toml take before
/// its level and removed there with the grant: a join stopped between the two, or a grant that
/// failed, leaves it for the next sync, which writes the grant while `[fleet]` still reads at the
/// content that join wrote. A person's `aterm fleet allow`, `join`, `init` or `leave` removes it
/// under the book's lock, so no level a person set is widened by it.
const GRANT_OWED_FILE: &str = "fleet-sync-grant";

/// An older build's steward's mark of the fleet it formed; a leave removes it.
const STEWARDED_FILE: &str = "fleet-stewarded";

/// An older build's steward's record that the vault published its fleet; a leave removes it.
const STEWARD_PUBLISHED_FILE: &str = "fleet-steward-published";

/// An older build's steward's record that an admin removed its fleet's code; a leave removes it.
const STEWARD_REMOVED_FILE: &str = "fleet-steward-removed";

/// What a sync of the default ALab fleet did (`aterm fleet sync`, [`sync_from_cache`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetSync {
    /// `[swarm] enabled = false` in `aterm.toml`.
    SwarmOff,
    /// `aterm fleet leave` turned sync off (its `fleet-off` record).
    Off,
    /// No vault is configured.
    NoVault,
    /// The configured vault is not ALab's.
    NotAlab,
    /// The vault's applied copy gives this Mac no default fleet code: none is published, or this
    /// Mac is not (yet) a member it reaches. An apply that has not finished is an error instead
    /// ([`aterm_secrets::client::alab_fleet_code`]).
    NoCode,
    /// This Mac is in the vault's fleet: nothing of the fleet was written.
    Current,
    /// This Mac is in the vault's fleet, and the default grant a sync's earlier join owed it
    /// ([`GRANT_OWED_FILE`]) was written: the grant's report.
    Granted(String),
    /// This Mac joined the vault's fleet: the join's report, and the default grant's where one
    /// was written.
    Joined(String),
    /// The fleet this Mac holds was left as it is: why, and what a person does about it.
    Blocked(String),
}

impl FleetSync {
    /// What `aterm fleet sync` says of it.
    #[must_use]
    pub fn report(&self) -> String {
        match self {
            FleetSync::SwarmOff => aterm_secrets::swarm::SWARM_OFF.into(),
            FleetSync::Off => "fleet sync is off after aterm fleet leave; `aterm fleet sync --on` \
                               turns it on and joins the default fleet"
                .into(),
            FleetSync::NoVault => "no vault is configured; fleet membership is unchanged".into(),
            FleetSync::NotAlab => {
                "the configured vault is not ALab; fleet membership is unchanged".into()
            }
            FleetSync::NoCode => "the vault's copy applied here gives this Mac no default ALab \
                                  fleet code; fleet membership is unchanged"
                .into(),
            FleetSync::Current => "default ALab fleet membership is current".into(),
            FleetSync::Granted(report) | FleetSync::Joined(report) => report.clone(),
            FleetSync::Blocked(why) => format!("fleet membership is unchanged: {why}"),
        }
    }
}

/// Where a sync reads the vault's fleet code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fetch {
    /// After a full vault sync of its own (`aterm fleet sync`).
    Sync,
    /// From the signed vault copy this Mac applied last.
    Cache,
}

fn sync(places: &Places, args: &[String]) -> Result<String, Failure> {
    // `--on` is a person's: the converge pass the vault subscriber starts syncs without it
    // (`sync_from_cache`), so leave's fleet-off record keeps it off until a person turns it on.
    let on = match args {
        [] => false,
        [flag] if flag == "--on" => true,
        _ => return Err(Failure::Usage("sync takes --on alone".into())),
    };
    match sync_with(places, on, || vault_code(Fetch::Sync), &Opts::default())? {
        // Not converged: the run did not do what it was asked.
        FleetSync::Blocked(why) => Err(failed(format!("fleet membership is unchanged: {why}"))),
        synced => Ok(synced.report()),
    }
}

/// Why [`try_sync_from_cache`] did not finish, and whether the issuer listener was at capacity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetSyncError {
    /// Human-readable diagnostic.
    pub why: String,
    /// `true` when the issuer dial failed at connection or per-address capacity.
    pub at_capacity: bool,
}

/// Converge this Mac on the default ALab fleet from the signed vault copy it already applied:
/// `aterm fleet sync` without the vault sync it makes first, which stays the vault subscriber's
/// (or the app's) to make, so two of them never materialize at once. It reads aterm's own state
/// directory and aterm.toml, and the vault this Mac joined.
///
/// # Errors
/// There is no state directory or config path; the vault's local state, its signed copy or the
/// code cannot be read; or the join or the default grant did not finish (the issuer not
/// answering, say), for the next pass to try again.
pub fn sync_from_cache() -> Result<FleetSync, String> {
    try_sync_from_cache().map_err(|err| err.why)
}

/// [`sync_from_cache`], preserving whether an issuer refusal came from listener capacity so
/// `sync.fleet` retries within minutes instead of waiting for the next 30-minute pass.
///
/// # Errors
/// As [`sync_from_cache`], paired with `at_capacity` on [`FleetSyncError`].
pub fn try_sync_from_cache() -> Result<FleetSync, FleetSyncError> {
    let places = Places::here().map_err(|err| FleetSyncError {
        why: err.into_text(),
        at_capacity: false,
    })?;
    let opts = Opts::default();
    sync_with(&places, false, || vault_code(Fetch::Cache), &opts).map_err(|err| FleetSyncError {
        why: err.into_text(),
        at_capacity: opts.saw_capacity.load(std::sync::atomic::Ordering::Relaxed),
    })
}

/// What a sync of the default ALab fleet from the signed vault copy would find, read without
/// writing: the converge pass's probe before it runs [`sync_from_cache`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetCheck {
    /// A sync does nothing here: off after a leave, no vault, not ALab's, or no code
    /// ([`FleetSync::report`] says which).
    Idle(FleetSync),
    /// This Mac is in the vault's fleet, recorded as the one a sync follows, and owes no grant:
    /// a sync writes nothing.
    Current,
    /// A sync would write, or say why it leaves the fleet as it is: what it finds, in one line
    /// (never the secret).
    Due(String),
}

/// Read, writing nothing, what [`sync_from_cache`] would find: the same vault copy and the
/// same rules ([`converge_on`]). Another fleet's key, one this Mac issues, and a key that cannot
/// be read all read as due; the sync then says whether it joins or leaves the fleet as it is.
///
/// # Errors
/// As [`sync_from_cache`]'s reads: there is no state directory or config path, or the vault's
/// local state, its signed copy, the code or the fleet's state cannot be read.
pub fn check_from_cache() -> Result<FleetCheck, String> {
    let places = Places::here().map_err(Failure::into_text)?;
    check_with(&places, || vault_code(Fetch::Cache)).map_err(Failure::into_text)
}

/// [`check_from_cache`] over `places`, `vault` naming the default fleet's code.
fn check_with(
    places: &Places,
    vault: impl FnOnce() -> Result<VaultCode, Failure>,
) -> Result<FleetCheck, Failure> {
    if !aterm_secrets::swarm::enabled_at(&places.config).map_err(failed)? {
        return Ok(FleetCheck::Idle(FleetSync::SwarmOff));
    }
    if in_state(places, "fleet-off")? {
        return Ok(FleetCheck::Idle(FleetSync::Off));
    }
    let code = match vault()? {
        VaultCode::NoVault => return Ok(FleetCheck::Idle(FleetSync::NoVault)),
        VaultCode::NotAlab => return Ok(FleetCheck::Idle(FleetSync::NotAlab)),
        VaultCode::NoCode => return Ok(FleetCheck::Idle(FleetSync::NoCode)),
        VaultCode::Code { code, .. } => code,
    };
    let code = code
        .as_str()
        .ok_or_else(|| failed("the signed fleet code is not UTF-8"))?;
    check_on(places, code)
}

/// What [`converge_on`] would find for `code`, read without writing: current only where it would
/// write nothing (no join, no `fleet-synced` record, no owed grant written or dropped).
fn check_on(places: &Places, code: &str) -> Result<FleetCheck, Failure> {
    let parsed = JoinCode::parse(code).map_err(failed)?;
    let current = FleetSecret::new(&parsed.secret).map_err(failed)?;
    let issues = in_state(places, CODE_FILE)?;
    let due = |why: &str| Ok(FleetCheck::Due(why.to_owned()));
    match aterm_net::fleet::read_key_file(&places.state) {
        Ok(held) if held == current => {}
        Ok(_) if issues => return due("this Mac issues another fleet"),
        Ok(_) => return due("this Mac holds another fleet's key"),
        Err(aterm_net::fleet::KeyFileError::Missing) => return due("this Mac is in no fleet"),
        Err(_) => return due("this Mac's fleet key cannot be read"),
    }
    if !issues && !names_issuer(places) {
        return due("this Mac's join of the default fleet did not finish");
    }
    if synced(places) != Some(current.mark()) {
        return due("this Mac is in the default fleet, not yet recorded as the one a sync follows");
    }
    if in_state(places, GRANT_OWED_FILE)? {
        return due("a sync's join left this Mac's default grant owed");
    }
    Ok(FleetCheck::Current)
}

/// What the vault this Mac joined says of the default ALab fleet ([`vault_code`]).
enum VaultCode {
    NoVault,
    NotAlab,
    NoCode,
    /// The signed default fleet code (never printed).
    Code {
        code: aterm_secrets::SecretBytes,
    },
}

/// Read the vault this Mac joined (the live one: `$HOME`'s) for the default ALab fleet code,
/// after a vault sync of its own where `fetch` asks for one.
fn vault_code(fetch: Fetch) -> Result<VaultCode, Failure> {
    let vault = aterm_secrets::client::Paths::live().map_err(failed)?;
    let Some(config) = aterm_secrets::client::config(&vault).map_err(failed)? else {
        return Ok(VaultCode::NoVault);
    };
    if !aterm_secrets::client::alab_vault(&config) {
        return Ok(VaultCode::NotAlab);
    }
    if fetch == Fetch::Sync {
        aterm_secrets::client::sync(&vault, None).map_err(failed)?;
    }
    Ok(
        match aterm_secrets::client::alab_fleet(&vault).map_err(failed)? {
            Some(fleet) => VaultCode::Code { code: fleet.code },
            None => VaultCode::NoCode,
        },
    )
}

/// A sync: `on` (`aterm fleet sync --on`, a person's) first removes leave's fleet-off record,
/// which keeps every other sync off; then `vault` names the default fleet's code, and this Mac
/// converges on it ([`converge_on`]; `at` names where it listens, for a loopback fleet).
fn sync_with(
    places: &Places,
    on: bool,
    vault: impl FnOnce() -> Result<VaultCode, Failure>,
    at: &Opts,
) -> Result<FleetSync, Failure> {
    at_step("on");
    // A person's end of fleet-off, under the lock a leave writes it under.
    if on && in_state(places, member::OFF_FILE)? {
        member::write_held(&places.state, Began::Here, |book| {
            book.remove(member::OFF_FILE)
        })
        .map_err(failed)?
        .map_err(|error| failed(format!("could not clear fleet-off: {error}")))?;
    }
    if !on && !aterm_secrets::swarm::enabled_at(&places.config).map_err(failed)? {
        return Ok(FleetSync::SwarmOff);
    }
    if in_state(places, member::OFF_FILE)? {
        return Ok(FleetSync::Off);
    }
    let code = match vault()? {
        VaultCode::NoVault => return Ok(FleetSync::NoVault),
        VaultCode::NotAlab => return Ok(FleetSync::NotAlab),
        VaultCode::NoCode => return Ok(FleetSync::NoCode),
        VaultCode::Code { code } => code,
    };
    let code = code
        .as_str()
        .ok_or_else(|| failed("the signed fleet code is not UTF-8"))?;
    converge_on(places, code, at)
}

/// What a sync names as the way out of a fleet it leaves as it is: a leave, whose fleet-off
/// record a person's `--on` then removes.
const LEAVE_THEN_ON: &str = "`aterm fleet leave`, then `aterm fleet sync --on`";

/// Bring this Mac to the fleet of `code`, the vault's default ALab fleet code. It joins when it
/// holds no fleet, when its join of that fleet is unfinished, or when the fleet it holds is the
/// one a sync last found it in ([`SYNCED_FILE`]: the vault's code moved on); another fleet a
/// person joined by hand, one this Mac issues, and a key it cannot read are left as they are.
/// A sync's join keeps the level written before it (a new secret's join lowers drive to
/// content and takes named grants out, as every join does); a first join reads at content until
/// its default grant. The default grant (every member may drive) goes with a join made here on
/// every member, a vault admin included (docs/DESIGN-fluid-swarm-2026-10-28.md F2), and only
/// where no narrower level stands ([`default_grant_stands`]), so a level a person set after a
/// sync's join stays; one that join owed and did not write ([`GRANT_OWED_FILE`]) is written by a
/// later pass.
fn converge_on(places: &Places, code: &str, at: &Opts) -> Result<FleetSync, Failure> {
    let parsed = JoinCode::parse(code).map_err(failed)?;
    let current = FleetSecret::new(&parsed.secret).map_err(failed)?;
    let issues = in_state(places, CODE_FILE)?;
    // The fleet a join moves this Mac from, where one is due: its join is made only while this
    // Mac holds it still.
    let join_from = match aterm_net::fleet::read_key_file(&places.state) {
        Ok(held) if held == current => {
            (!issues && !names_issuer(places)).then_some(Held::Key(held))
        }
        Ok(_) if issues => {
            return Ok(FleetSync::Blocked(
                "this Mac issues another fleet: `aterm fleet leave` here (its members then hold \
                 a secret no issuer keeps), then `aterm fleet sync --on`"
                    .into(),
            ));
        }
        Ok(held) if synced(places) != Some(held.mark()) => {
            return Ok(FleetSync::Blocked(format!(
                "this Mac joined another fleet by hand: {LEAVE_THEN_ON} moves it to the default \
                 one"
            )));
        }
        // The fleet a sync last found or put this Mac in: the vault's code moved on.
        Ok(held) => Some(Held::Key(held)),
        // No fleet held.
        Err(aterm_net::fleet::KeyFileError::Missing) => Some(Held::Nothing),
        // A key that cannot be read may hold a fleet a person joined by hand, and a join writes
        // the code's key in place of one it cannot use: it is the person's to put right.
        Err(error) => return Ok(FleetSync::Blocked(unread_for_sync(places, &error))),
    };
    Ok(match join_from {
        Some(from) => {
            // Its aterm.toml take decides, from aterm.toml read there, the level (a first join
            // reads at content until its grant) and the default grant, written in that take.
            let opts = Opts {
                port: at.port,
                host: at.host.clone(),
                synced: Some(from),
                ..Opts::default()
            };
            let joined = join(places, &opts, code).inspect_err(|_| {
                if opts.saw_capacity.load(std::sync::atomic::Ordering::Relaxed) {
                    at.saw_capacity
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
            })?;
            FleetSync::Joined(joined)
        }
        None => {
            record_synced(places, &current)?;
            grant_owed(places, &current)?.map_or(FleetSync::Current, FleetSync::Granted)
        }
    })
}

/// Why a sync leaves a key it cannot read as it is, and what a person does about it.
fn unread_for_sync(places: &Places, error: &aterm_net::fleet::KeyFileError) -> String {
    let (cause, act) = match error {
        // One that holds no secret names no fleet a sync could tell is its own.
        aterm_net::fleet::KeyFileError::Malformed(_) => (error.to_string(), LEAVE_THEN_ON),
        _ => key_fault(places, error),
    };
    format!(
        "{cause} ({}), so sync cannot tell which fleet this Mac holds: {act}",
        places.state.join(KEY_FILE).display()
    )
}

/// The default grant (every member may drive this Mac) a sync's join owes it
/// ([`GRANT_OWED_FILE`]), beside `book`: written while the record stands, where `[fleet]` reads
/// at a level the default grant may replace or at the content that join left, the record
/// removed either way. A vault admin is granted as every member is: no member is a steward
/// (docs/DESIGN-fluid-swarm-2026-10-28.md F2, which retired C19a's exemption with the open
/// approver). `None` where nothing is owed now: a person's `allow`, `join`, `init` or `leave`
/// took the record.
fn grant_default(places: &Places, book: &HeldBook<'_>) -> Result<Option<String>, Failure> {
    if !in_state(places, GRANT_OWED_FILE)? {
        return Ok(None);
    }
    let config = ConfigFile::open(&places.config).map_err(failed)?;
    let table = fleet_table(text_of(&config)?).map_err(failed)?;
    let owed_content =
        table.level() == Some(FleetScope::Content.word()) && table.drive_from.is_empty();
    let report = if table.listen.is_some() && (default_grant_stands(&table) || owed_content) {
        let all = aterm_net::fleet::ALL_MEMBERS;
        let mut nodes = if table.level() == Some(aterm_net::fleet::DRIVE_LEVEL) {
            table.drive_from
        } else {
            Vec::new()
        };
        nodes.retain(|word| word != all);
        nodes.push(all.to_owned());
        Some(write_level(
            places,
            book,
            &config,
            (aterm_net::fleet::DRIVE_LEVEL, nodes),
        )?)
    } else {
        None
    };
    forget_owed_grant(places)?;
    Ok(report)
}

/// [`grant_default`] for a sync that found this Mac in `fleet`, in a take of its own while this
/// Mac holds it: a record a sync's join leaves stands only beside that join unfinished, which
/// the next pass finishes, so this writes what an older aterm's pass owed.
fn grant_owed(places: &Places, fleet: &FleetSecret) -> Result<Option<String>, Failure> {
    if !in_state(places, GRANT_OWED_FILE)? {
        return Ok(None);
    }
    let mut said = String::new();
    private_state(places, &mut said)?;
    let granted = match member::write_held(
        &places.state,
        Began::Under(&Held::Key(fleet.clone())),
        |book| grant_default(places, book),
    ) {
        Ok(granted) => granted.inspect_err(|_| say(&said))?,
        // A leave, a join of another code or an init came since, and took the record with it.
        Err(Unwritten::Moved) => None,
        Err(Unwritten::Lock(why)) => {
            say(&said);
            return Err(failed(format!(
                "{why}, so the default grant was not written: the next `aterm fleet sync` writes it"
            )));
        }
    };
    Ok(granted.map(|mut report| {
        report.insert_str(report.find('\n').map_or(report.len(), |at| at + 1), &said);
        report
    }))
}

/// Record that a sync's join owes this Mac the default grant ([`GRANT_OWED_FILE`]), in its
/// aterm.toml take.
fn owe_grant(places: &Places) -> Result<(), Failure> {
    aterm_uds::private_file::write_private(&places.state, GRANT_OWED_FILE, b"").map_err(|error| {
        failed(format!(
            "{}: {error}",
            places.state.join(GRANT_OWED_FILE).display()
        ))
    })
}

/// Remove [`GRANT_OWED_FILE`] under the book's lock: the grant is written, or no longer a sync's
/// to write.
fn forget_owed_grant(places: &Places) -> Result<(), Failure> {
    aterm_uds::private_file::remove_private(&places.state, GRANT_OWED_FILE)
        .map(|_| ())
        .map_err(|error| {
            failed(format!(
                "{}: {error}",
                places.state.join(GRANT_OWED_FILE).display()
            ))
        })
}

/// Whether this Mac's `[fleet]` level is one a sync's default grant may replace: none written
/// (no fleet yet, or a join cut short before its aterm.toml write), or the default grant itself.
fn default_grant_stands(table: &FleetTable) -> bool {
    table.allow.is_none()
        || (table.level() == Some(aterm_net::fleet::DRIVE_LEVEL)
            && table
                .drive_from
                .iter()
                .any(|node| node == aterm_net::fleet::ALL_MEMBERS))
}

/// The fleet [`SYNCED_FILE`] records, if it records one.
fn synced(places: &Places) -> Option<aterm_net::fleet::FleetMark> {
    let bytes = aterm_uds::private_file::read_private(&places.state, SYNCED_FILE, 64).ok()?;
    aterm_net::fleet::FleetMark::from_hex(std::str::from_utf8(&bytes).ok()?.trim())
}

/// Record `fleet` as the one a sync last found or put this Mac in ([`SYNCED_FILE`]), under the
/// book's lock: a sync's join's in its aterm.toml take, [`record_synced`]'s in one of its own.
fn write_synced(places: &Places, fleet: &FleetSecret) -> Result<(), String> {
    let mark = fleet.mark();
    if synced(places) == Some(mark) {
        return Ok(());
    }
    aterm_uds::private_file::write_private(
        &places.state,
        SYNCED_FILE,
        format!("{}\n", mark.hex()).as_bytes(),
    )
    .map_err(|error| format!("{}: {error}", places.state.join(SYNCED_FILE).display()))
}

/// [`write_synced`] for a sync that found this Mac in `fleet`, in a take of its own, where no
/// leave came since the sync read no fleet-off: a leave takes the record, and anything else
/// keeps it, as after a sync before it.
fn record_synced(places: &Places, fleet: &FleetSecret) -> Result<(), Failure> {
    if synced(places) == Some(fleet.mark()) {
        return Ok(());
    }
    member::write_held(&places.state, Began::Here, |_| {
        if in_state(places, member::OFF_FILE)? {
            return Ok(());
        }
        write_synced(places, fleet).map_err(failed)
    })
    .map_err(failed)?
}

fn allow(places: &Places, args: &[String]) -> Result<String, Failure> {
    let usage = |why: &str| {
        Failure::Usage(format!(
            "{why} (aterm fleet allow facts|content, aterm fleet allow drive --all, or aterm \
             fleet allow drive --from <host|node>)"
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
            None if arg == "--all" => from.push(aterm_net::fleet::ALL_MEMBERS.to_owned()),
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

/// [`allow`]'s write of `level`, with the drive grants `from` names at drive. A grant is written by [`member::write_held`], its nodes named, and each of its refusals made, from what it reads under the book's lock, beside the fleet whose issuer's book it read before it; facts or content names no fleet, so its write is held as a leave's is ([`member::level_held`]), under the book's lock that every other edit of `[fleet]` holds: it lands before or after a leave's, a join's or an init's whole write, as some serial order would, and no odd state directory blocks it.
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
    if !drive {
        let mut narrowed_line = String::new();
        let mut report = member::level_held(&places.state, |held| {
            if let Some(was) = held.narrowed() {
                narrowed_line = narrowed(places, was);
            }
            let config = ConfigFile::open(&places.config).map_err(failed)?;
            in_a_fleet(text_of(&config)?)?;
            at_step("level");
            // A person's level supersedes the default grant a sync's join still owes.
            forget_owed_grant(places)?;
            write_level(places, held, &config, (word, Vec::new()))
        })
        .map_err(failed)?
        .inspect_err(|_| say(&narrowed_line))?;
        report.insert_str(
            report.find('\n').map_or(report.len(), |at| at + 1),
            &narrowed_line,
        );
        return Ok(report);
    }
    // The issuer's book is read before the lock, which no dial holds; each refusal is made under
    // it, and a read made under what the lock finds moved is made again. `--all` names no node.
    let names = from
        .iter()
        .any(|word| word != aterm_net::fleet::ALL_MEMBERS);
    let mut moved = 0;
    loop {
        let listed = ConfigFile::open(&places.config)
            .ok()
            .is_some_and(|config| text_of(&config).is_ok_and(|text| in_a_fleet(text).is_ok()));
        let began = ReadUnder::now(places, Held::read(&places.state), listed);
        let missed = match &began.held {
            Held::Key(_) if listed && names => check_peers(places, issuer_read_within())
                .ok()
                .and_then(|listing| listing.missed),
            _ => None,
        };
        at_step("grant");
        let granted = member::write_held(&places.state, Began::Here, |book| {
            let config = ConfigFile::open(&places.config).map_err(failed)?;
            let table = in_a_fleet(text_of(&config)?)?;
            held_for_grant(places, book.held())?;
            if ReadUnder::now(places, book.held().clone(), true) != began {
                return Ok(None);
            }
            let (grants, unconfirmed) = drive_grants(places, from, missed)?;
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
            forget_owed_grant(places)?;
            write_level(places, book, &config, (word, nodes))
                .map(|report| Some(report + &unconfirmed))
        })
        .map_err(|unwritten| {
            failed(format!(
                "{unwritten}, so nothing was granted: `aterm fleet allow drive` names the nodes \
                 of the fleet this machine holds"
            ))
        })??;
        moved += 1;
        match granted {
            Some(report) => return Ok(report),
            None if moved < GRANT_READS => {}
            None => {
                return Err(failed(format!(
                    "the fleet this machine holds moved {GRANT_READS} times while this grant read \
                     its issuer's book (an `aterm fleet join`, `leave` or `init` came between), \
                     so nothing was granted: run this again"
                )));
            }
        }
    }
}

/// How many times a grant reads its issuer's book before it refuses as moved: one more than the
/// writes of a first `join` (its key, aterm.toml, its book's issuer), each of which moves it.
const GRANT_READS: usize = 4;

/// What a grant's or a `peers rm`'s read of its issuer's book is made under: the fleet's key,
/// whether `[fleet]` listens (a grant reads only where it does, `peers rm` either way), and the
/// issuer the book names, where it is dialled as which node.
#[derive(PartialEq)]
struct ReadUnder {
    held: Held,
    listed: bool,
    issuer: Option<member::Target>,
}

impl ReadUnder {
    fn now(places: &Places, held: Held, listed: bool) -> Self {
        let issuer = member::read_peers(&places.state).ok().and_then(|book| {
            book.iter()
                .find(|peer| peer.via == Via::Issuer)
                .map(target_of)
        });
        Self {
            held,
            listed,
            issuer,
        }
    }
}

/// The key a grant is written beside, read under the book's lock: none is a leave stopped short.
/// # Errors
/// No key, or one that cannot be read.
fn held_for_grant(places: &Places, held: &Held) -> Result<(), Failure> {
    let key = places.state.join(KEY_FILE);
    match held {
        Held::Key(_) => Ok(()),
        Held::Nothing => Err(failed(format!(
            "this machine holds no fleet's key ({}), so nothing was granted: a grant names nodes \
             of the fleet held here; `aterm fleet leave` finishes a leave that stopped short of \
             aterm.toml",
            key.display()
        ))),
        Held::Unreadable => {
            let (cause, act) = aterm_net::fleet::read_key_file(&places.state)
                .err()
                .map_or_else(
                    || (format!("{KEY_FILE} cannot be read"), "run this again"),
                    |e| key_fault(places, &e),
                );
            Err(failed(format!(
                "{cause} ({}), so nothing was granted: {act}",
                key.display()
            )))
        }
    }
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
/// beside `held`: a grant's book, or a level's hold.
fn write_level(
    places: &Places,
    held: &impl Beside,
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
            let bak = replace_held(held, config, &text)?;
            format!(
                "aterm fleet allow: {said} in {}{}\n",
                places.config.display(),
                bak.said()
            )
        }
    };
    if nodes.is_empty() && !had.is_empty() {
        // Members read at content still only where they did: a list a hand left at facts rises.
        let still = drove || was == Some(FleetScope::Content.word());
        out.push_str(&grants_removed(
            &had,
            &format!("allow is {word} now, below drive"),
            if still && word == FleetScope::Content.word() {
                CONTENT_LEFT
            } else {
                ""
            },
        ));
    }
    // A level below drive cuts drives only where it lowered drive or took a grant out.
    if nodes
        .iter()
        .any(|word| word == aterm_net::fleet::ALL_MEMBERS)
    {
        out.push_str(
            "  every member the fleet's issuer binds, proving the node key its book binds it to, \
             may drive this Mac's sessions (`aterm ctl --host <this Mac> …` there); a Mac that \
             book binds no more, or binds under another key, drives nothing. The person here \
             wins: Take Over refuses every remote write.\n",
        );
        out.push_str(granted_reads(was));
    } else if !nodes.is_empty() {
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

/// The grant each of `words` names for `allow drive --from`, under the book's lock (`missed`: why
/// the read of the issuer's book before it brought none), and a line for each key only a copy of
/// that book the issuer could not confirm binds.
/// # Errors
/// A word that names no peer, or a peer with no node or one bound to no key.
fn drive_grants(
    places: &Places,
    words: &[String],
    missed: Option<Missed>,
) -> Result<(Vec<DriveGrant>, String), Failure> {
    let mut unconfirmed = String::new();
    for word in words {
        if let Some(own) = this_machine_alone(places, word).map_err(failed)? {
            return Err(failed(format!(
                "`{}` is this machine itself (node {}): `allow drive --from` names another Mac of \
                 the fleet, and this one's own sessions are driven here with `aterm ctl`",
                aterm_codec::tty::one_line(word, 40),
                short(&own)
            )));
        }
    }
    let mut nodes = Vec::new();
    for word in words {
        if word == aterm_net::fleet::ALL_MEMBERS {
            nodes.push(DriveGrant::from_word(word).expect("the all-member grant parses"));
            continue;
        }
        let shown = aterm_codec::tty::one_line(word, 40);
        let bound = bound_peer(places, word, missed.as_ref())?;
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
            Bound::ByHand(_) => {
                return Err(failed(format!(
                    "`{shown}` is a peer named by hand, with no node: a Mac that may drive is \
                     named by the node its join recorded"
                )));
            }
            Bound::Nobody => return Err(failed(names_no_peer(places, word, missed.as_ref()))),
        };
        if let (Some(node), Some(key)) = (peer.node, peer.key) {
            if let Some(missed) = missed.as_ref().filter(|_| peer.via == Via::IssuerBook) {
                unconfirmed.push_str(&format!(
                    "  key        node {} is bound here only by this machine's copy of its \
                     issuer's book, which the issuer could not confirm just now ({}): the grant \
                     holds the key that copy binds, so a node bound afresh since drives nothing \
                     until it is granted again once the issuer answers\n",
                    short(&node),
                    missed.why
                ));
            }
            nodes.push(DriveGrant { node, key });
        }
    }
    Ok((nodes, unconfirmed))
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
    ByHand(Peer),
    /// No peer.
    Nobody,
}

/// The node `what` names, for a drive, its grant or a survey ([`named_here`] over the book, the
/// copy and the `[[fleet.peers]]` entries; `unread`: why the read of the issuer's book before it
/// brought none): for an entry with no node, the one bound at its place.
/// # Errors
/// The book cannot be read, or `what` names more than one peer, or an entry with no node at the
/// place of more than one node.
fn bound_peer(places: &Places, what: &str, unread: Option<&Missed>) -> Result<Bound, Failure> {
    let bound = member::bound_peers(&places.state);
    let book = member::read_peers(&places.state).map_err(failed)?;
    let Some(peer) = named_here(places, &book, what, unread).map_err(failed)? else {
        return Ok(Bound::Nobody);
    };
    Ok(match &peer.node {
        None => {
            let stead: Vec<&Peer> = bound
                .iter()
                .filter(|known| known.same_place(&peer))
                .collect();
            match stead.as_slice() {
                [] => Bound::ByHand(peer),
                [peer] => Bound::Node((*peer).clone()),
                many => {
                    return Err(failed(format!(
                        "`{}` is a peer named by hand at the place of {} nodes ({}): name \
                             one by its node id",
                        aterm_codec::tty::one_line(what, 80),
                        many.len(),
                        many.iter()
                            .map(|peer| peer.node.as_deref().unwrap_or("-"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
            }
        }
        Some(node) => bound
            .iter()
            .find(|known| known.node.as_ref() == Some(node))
            .map_or_else(
                || Bound::Unbound(peer.clone()),
                |known| Bound::Node(known.clone()),
            ),
    })
}

/// The word a `--host` drive or read binds for `what` ([`drive_fleet`], [`survey_fleet`]): `what`
/// itself where it names this machine ([`names_this_machine`], its `[fleet] machine` among them)
/// or a peer answers to it as every verb reads a peer's word ([`member::all_named_with`] over the
/// book, the copy, the `[[fleet.peers]]` entries and the swarm roster's labels and aliases), or
/// where no short name of a peer's host is it; else the whole node id (or the address and port,
/// for an entry with no node) of the one peer a short name names ([`member::all_short_named`]:
/// `MacbookAir` or `m27` for `MacbookAir-m27-M5-32GB-4TB.local`; `m102` of
/// `m102-macbook-M4-Pro-24GB-500GB.local` is read by [`member::all_named_with`] already, as every
/// verb reads a host's leading `m<N>`). A word that names no peer either way is read as today: a
/// drive refuses it, and a survey reads it as a host by DNS and multicast DNS. A short name this
/// machine's issuer answers to is left as it is while no copy of its book is held or its read
/// before brought none to keep (`unread`), so [`named_here`] refuses it as it does every word that
/// names the issuer loosely: a member that joined since may answer to it.
/// # Errors
/// The book cannot be read, or `what` is a short name of more than one peer
/// ([`member::several_short`]).
fn host_word(places: &Places, what: &str, unread: Option<&Missed>) -> Result<String, String> {
    host_word_among(places, what, unread, &roster_identities(places))
}

/// [`host_word`] with the swarm roster's `identities` given.
/// # Errors
/// As [`host_word`].
fn host_word_among(
    places: &Places,
    what: &str,
    unread: Option<&Missed>,
    identities: &[member::PeerIdentity],
) -> Result<String, String> {
    let book = member::read_peers(&places.state).map_err(|e| e.to_string())?;
    let others = others_listed(places, &book);
    if names_this_machine(places, what).is_some()
        || !member::all_named_with(&book, &others, identities, what).is_empty()
        || (loosely_named_issuer(places, &book, what).is_some()
            && (unread.is_some() || uncopied_issuer(places, &book).is_some()))
    {
        return Ok(what.to_owned());
    }
    match member::all_short_named(&book, &others, what).as_slice() {
        [] => Ok(what.to_owned()),
        [peer] => Ok(peer.node.clone().unwrap_or_else(|| peer.target())),
        many => Err(member::several_short(what, many)),
    }
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
        from: format!("{}{}", places.config.display(), bak.said()),
        left: match (kept.is_empty(), swept) {
            (false, _) => String::new(),
            (true, 0) => format!(
                ", and allow is {level}: no Mac drives this one until {} grants again{reads}",
                grants_again(&table.drive_from)
            ),
            (true, swept) => format!(
                ", with the {swept} more it listed, as allow is {level}, below drive: no Mac \
                 drives this one until {} grants again{reads}",
                grants_again(&table.drive_from)
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
    #[cfg(not(test))]
    {
        let home = aterm_types::dirs::home_dir();
        leave_within_node(
            places,
            home.as_deref(),
            Some(&crate::node_service::LiveSupervisor),
            PEER_CHECK_WITHIN,
        )
    }
    #[cfg(test)]
    {
        leave_within(places, PEER_CHECK_WITHIN)
    }
}

/// [`leave`], this machine's own listener given `within` to prove itself ([`listens_here`]).
#[cfg_attr(unix, cfg(test))]
fn leave_within(places: &Places, within: Duration) -> Result<String, Failure> {
    leave_within_node(places, None, None, within)
}

/// [`leave_within`], also removing `systems.alab.aterm-node` under `home` via `supervisor` when
/// provided (C20).
fn leave_within_node(
    places: &Places,
    node_home: Option<&Path>,
    node_supervisor: Option<&dyn crate::node_service::ServiceSupervisor>,
    within: Duration,
) -> Result<String, Failure> {
    // Read before anything goes: once it has left, this machine reaches no Mac to hand a tab back,
    // and an issuer's fleet, which goes with it, takes no join again.
    let issues = in_state(places, CODE_FILE).unwrap_or(false);
    let holds = held_lines(
        places,
        "once it has left",
        (!issues).then_some("joins again"),
    );
    let listening = listens_here(places, within);
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
        let files = [
            (KEY_FILE, "the fleet's secret"),
            (CODE_FILE, "the join code"),
            (PEERS_FILE, "the book of peers"),
            // Written by dev builds since 1b60ca3c4b; nothing reads it.
            (
                "fleet-peers-left",
                "what a join set aside of the fleet held before",
            ),
            (ISSUER_BOOK_FILE, "the copy of the issuer's book"),
        ];
        // The leave's first write, so one stopped short keeps sync off too; none in no fleet. What
        // cannot be read counts as held, and its removal below says why it failed.
        let has_node_job = node_home
            .zip(node_supervisor)
            .is_some_and(|(home, sup)| sup.unit_file(home).is_file());
        let holds = has_node_job
            || files
                .iter()
                .any(|(name, _)| in_state(places, name).unwrap_or(true))
            || ConfigFile::open(&places.config)
                .ok()
                .and_then(|config| without_fleet(text_of(&config).ok()?).ok())
                .is_none_or(|text| text.is_some());
        if holds {
            let off = places.state.join(member::OFF_FILE);
            let stands = || in_state(places, member::OFF_FILE).unwrap_or(false);
            // Nothing goes without it where a join could write a fleet: sync would join the
            // default fleet here again after a leave that wrote none. One its folder's flush
            // failed after is written.
            let unwritten = if stands() {
                None
            } else {
                leaving.write_off().err().filter(|_| !stands())
            };
            out.push_str(&match unwritten {
                None => format!("  off        {} (fleet sync stays off)\n", off.display()),
                Some(e) if leaving.locked() => {
                    return Err(failed(format!(
                        "{} could not be written ({e}), so nothing was removed: a leave writes it \
                         first, or `aterm fleet sync`, which the vault subscriber runs, would join \
                         the default fleet here again; once it can be written (room on that \
                         volume, say), run `aterm fleet leave` again",
                        off.display()
                    )));
                }
                Some(e) => format!(
                    "  off        {} could not be written ({e}), nor can any join write a fleet \
                     there, so fleet sync joins none\n",
                    off.display()
                ),
            });
        }
        for (name, what) in files {
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
        // What a sync or an older build's steward recorded goes with the fleet, unreported: a
        // later join by hand is the person's, and owes no default grant. So does the record of
        // when each member last answered, which alone holds no fleet.
        for name in [
            member::ANSWERED_FILE,
            SYNCED_FILE,
            GRANT_OWED_FILE,
            STEWARDED_FILE,
            STEWARD_PUBLISHED_FILE,
            STEWARD_REMOVED_FILE,
        ] {
            leaving
                .remove(name)
                .map_err(|e| failed(format!("{}: {e}", places.state.join(name).display())))?;
        }
        at_step("removed");
        let config = ConfigFile::open(&places.config).map_err(failed)?;
        at_step("config");
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
                bak.said()
            ));
            left = true;
        }
        at_step("unlisten");
        if let Some((home, sup)) = node_home.zip(node_supervisor)
            && let Some(unit) =
                crate::node_service::remove_with(home, &places.state, false, sup).map_err(failed)?
        {
            out.push_str(&format!("  removed    {} (the node job)\n", unit.display()));
            left = true;
        }
        Ok(())
    })
    .map_err(failed)?
    .inspect_err(|_| say(&out))?;
    if !left {
        return Ok(format!(
            "aterm fleet leave: this machine is in no fleet\n{out}"
        ));
    }
    // A listener needs the key and `[fleet] listen` both, and one stops only where it listened. The
    // issuer line goes only where this book still names an issuer: one a `peers rm` took out here
    // still lists this machine.
    let mut said: Vec<&str> = Vec::new();
    if unkeyed && unlistened && listening {
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
    let lifts = |rm| aterm_ctl::lifts_there(own.as_deref().unwrap_or("-"), rm);
    let found = held_elsewhere(places);
    let held = found.held.iter().map(|(at, sid, rm)| {
        format!(
            "  hold       {sid} on {at} stays held by this machine, which reaches that Mac no more \
             {once}: {}{}\n",
            lifts(*rm),
            again.map_or_else(String::new, |again| format!(
                ", and so does `aterm ctl --host {at} hold {sid} off` here once this machine \
                 {again}"
            ))
        )
    });
    // A Mac it could not read may hold a tab of its own: that is said too, never left out.
    let unread = found.unread.iter().map(|(at, why, rm)| {
        format!(
            "  unread     {at}: this machine could not read it just now ({why}), so it cannot say \
             which tabs it holds there, and any it holds stays held {once}: {}\n",
            lifts(*rm)
        )
    });
    let book = found.book.iter().map(|why| {
        format!(
            "  unread     its issuer's book was not read just now ({why}), so this machine cannot \
             say which tabs it holds on a member only that book names, and any it holds stays \
             held {once}: {}\n",
            lifts(aterm_ctl::RmLifts::Issuer)
        )
    });
    held.chain(unread).chain(book).collect()
}

/// What [`held_elsewhere`] read of the fleet.
#[derive(Default)]
struct HeldElsewhere {
    /// Each tab this machine holds on another Mac, beside the name a drive dials that Mac by and
    /// whose `peers rm` of this machine lifts it there.
    held: Vec<(String, String, aterm_ctl::RmLifts)>,
    /// Each Mac it could not read, beside why and whose `peers rm` lifts a hold there.
    unread: Vec<(String, String, aterm_ctl::RmLifts)>,
    /// Why the issuer's book, which names the members, was not read.
    book: Option<String>,
}

/// The sessions other Macs' proven surveys say this machine holds (`held_from=` its node), and
/// the Macs, and the issuer's book, its read could not check.
fn held_elsewhere(places: &Places) -> HeldElsewhere {
    let (Ok(secret), Some(own)) = (
        aterm_net::fleet::read_key_file(&places.state),
        own_node(places),
    ) else {
        return HeldElsewhere::default();
    };
    let Ok(known) = known_peers(places) else {
        return HeldElsewhere::default();
    };
    let book: Vec<Peer> = known.peers.into_iter().map(|(peer, _)| peer).collect();
    raise_open_files();
    let (reads, book_read) = member::read_whole(
        &book,
        &known.keys,
        Some(&own),
        (&secret, own_identity(places).as_ref()),
        peer_read_within(),
        "survey facts",
    );
    let mut found = HeldElsewhere {
        book: match book_read {
            BookRead::NoIssuer | BookRead::Fetched(_) => None,
            BookRead::Refused(why) => Some(why.to_string()),
            BookRead::Unshared(why) | BookRead::Unread(why) => Some(why),
        },
        ..HeldElsewhere::default()
    };
    // A Mac's own book binds this machine where one of them is the other's issuer; any other
    // member binds it through its copy of the issuer's book.
    let issues = in_state(places, CODE_FILE).unwrap_or(false);
    let held = &mut found.held;
    for read in reads {
        if read.peer.node.as_deref() == Some(own.as_str()) {
            continue;
        }
        let name = || aterm_codec::tty::one_line(&shown_name(&read.peer), 80);
        let rm = if issues || read.peer.via == Via::Issuer {
            aterm_ctl::RmLifts::There
        } else {
            aterm_ctl::RmLifts::Issuer
        };
        let surveyed = match read.read {
            Ok(surveyed) if surveyed.proven => surveyed,
            Ok(_) => {
                found
                    .unread
                    .push((name(), "it proved no node of the fleet".to_owned(), rm));
                continue;
            }
            Err(why) => {
                found.unread.push((
                    name(),
                    aterm_codec::tty::one_line(&why.to_string(), 160),
                    rm,
                ));
                continue;
            }
        };
        if surveyed.node.as_deref() == Some(own.as_str()) {
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
                && !held.iter().any(|(_, seen, _)| seen == sid)
            {
                held.push((name(), sid.to_owned(), rm));
            }
        }
    }
    found
}

/// The fleet client of `aterm ctl survey --fleet | --host`
/// (`aterm_ctl::LocalVerbs::fleet_survey`), each peer read within the survey's bound, or the one
/// [`SURVEY_WITHIN_SEAM`] names in a test of the binary.
/// # Errors
/// This machine holds no fleet key it can read, aterm.toml or the book cannot be read, or
/// `--host` names more than one peer, or neither a peer nor a host.
pub fn survey_fleet(
    ask: &aterm_ctl::FleetAsk<'_>,
) -> Result<aterm_ctl::FleetRead, aterm_ctl::FleetRefused> {
    let places = Places::here().map_err(failure_words)?;
    let ask = aterm_ctl::FleetAsk {
        within: seamed_within(SURVEY_WITHIN_SEAM, ask.within),
        ..*ask
    };
    survey_fleet_in(&places, &ask)
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
    let itself = |own: &str| -> aterm_ctl::DriveRefused {
        format!(
            "`{}` is this machine itself (node {}): its own sessions are driven here with `aterm \
             ctl`, never over the fleet",
            aterm_codec::tty::one_line(host, 80),
            short(own)
        )
        .into()
    };
    // Its whole id names no other node: refused before any read.
    if me.node().eq_ignore_ascii_case(host.trim()) {
        return Err(itself(me.node()));
    }
    let deadline = (Instant::now() + within, within);
    let check = issuer_read_within().min(within);
    let (read, mut unread) = copy_first(
        places,
        (Some(&secret), Some(&me)),
        host,
        (Instant::now() + check, check),
    );
    // A Mac's short name (`m102`) binds the one peer it names, by its whole node id.
    let mut word = host_word(places, host, unread.as_ref())?;
    if let Some(own) = this_machine_alone(places, &word)? {
        return Err(itself(&own));
    }
    let named = |word: &str, unread: Option<&Missed>| -> Result<_, aterm_ctl::DriveRefused> {
        let known = known_peers(places).map_err(failure_words)?;
        let book: Vec<Peer> = known.peers.into_iter().map(|(peer, _)| peer).collect();
        let bound = bound_peer(places, word, unread).map_err(failure_words)?;
        Ok((known.keys, book, bound))
    };
    let (mut keys, mut book, mut bound) = named(&word, unread.as_ref()).map_err(|refused| {
        match silent_issuer(places, &word, unread.as_ref()) {
            Some(missed) => aterm_ctl::DriveRefused {
                again: true,
                unserved: missed.unserved,
                ..refused
            },
            None => refused,
        }
    })?;
    if matches!(bound, Bound::Nobody) {
        if let Some(own) = at_own_place(places, &word) {
            return Err(format!(
                "`{}` is this machine itself (node {}): its own sessions are driven here with \
                 `aterm ctl`, never over the fleet",
                aterm_codec::tty::one_line(host, 80),
                short(&own)
            )
            .into());
        }
        if !read {
            match copy_again(places, &secret, Some(&me), (Instant::now() + check, check)) {
                Ok(true) => {
                    word = host_word(places, host, None)?;
                    if let Some(own) = this_machine_alone(places, &word)? {
                        return Err(itself(&own));
                    }
                    (keys, book, bound) = named(&word, None)?;
                }
                Ok(false) => {}
                Err(why) => unread = Some(why),
            }
        }
    }
    let shown = aterm_codec::tty::one_line(host, 80);
    let peer = match bound {
        Bound::Node(peer) | Bound::Unbound(peer) => peer,
        Bound::ByHand(_) => {
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
                        silent: false,
                        ungranted: false,
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
                     drive goes only to a node the book holds, named by its host, its short \
                     name (`m102`), node id or the address and port it lists, never to \
                     whichever member answers at an address"
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
                silent: drive_silent(&why),
                ungranted: matches!(
                    &why,
                    Unreached::Handshake(_, FleetError::UnknownOp(op))
                        if op == FleetOp::Drive.word()
                ),
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
            // Its remedy names the issuer's node short, which no word here names it by unread.
            Unreached::OtherNode { at, said, .. } => {
                Self::of(&BookUnread::OtherNode(*at, said.clone()))
            }
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

/// Whether nothing answered a drive's dial within its deadline, or no route reached the peer (a
/// Mac asleep or off its network, an aterm frozen), which may answer once it is back
/// ([`aterm_ctl::DriveRefused::silent`]).
fn drive_silent(why: &Unreached) -> bool {
    match why {
        Unreached::Deadline(..) | Unreached::Lookup(..) | Unreached::Resolve(_) => true,
        Unreached::Connect(_, e) => matches!(
            e.kind(),
            io::ErrorKind::TimedOut
                | io::ErrorKind::HostUnreachable
                | io::ErrorKind::NetworkUnreachable
                | io::ErrorKind::NetworkDown
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
) -> Result<aterm_ctl::FleetRead, aterm_ctl::FleetRefused> {
    let secret = aterm_net::fleet::read_key_file(&places.state).map_err(|e| no_key(places, &e))?;
    let known = known_peers(places).map_err(failure_words)?;
    let mut book: Vec<Peer> = known.peers.into_iter().map(|(peer, _)| peer).collect();
    let me = own_identity(places);
    if let Some(what) = ask.host {
        let itself = |own: &str| {
            format!(
                "`{}` is this machine itself (node {}): `aterm ctl survey` reads its own sessions",
                aterm_codec::tty::one_line(what, 80),
                short(own)
            )
        };
        // Its whole id names no other node: refused before any read.
        if let Some(own) = own_node(places).filter(|own| own.eq_ignore_ascii_case(what.trim())) {
            return Err(itself(&own).into());
        }
        let deadline = (Instant::now() + ask.within, ask.within);
        // A member's copy holds no member that joined since its last read: the issuer's book is
        // read in half the deadline at most, so a silent issuer leaves the rest for the host.
        let check = issuer_read_within().min(ask.within / 2);
        let (read, missed) = copy_first(
            places,
            (Some(&secret), me.as_ref()),
            what,
            (Instant::now() + check, check),
        );
        // A Mac's short name (`m102`) reads the one peer it names, by its whole node id.
        let mut word = host_word(places, what, missed.as_ref())?;
        if let Some(own) = this_machine_alone(places, &word)? {
            return Err(itself(&own).into());
        }
        let mut keys = known.keys;
        if read {
            let known = known_peers(places).map_err(failure_words)?;
            keys = known.keys;
            book = known.peers.into_iter().map(|(peer, _)| peer).collect();
        }
        let mut bound = bound_peer(places, &word, missed.as_ref()).map_err(|refused| {
            aterm_ctl::FleetRefused {
                unserved: silent_issuer(places, &word, missed.as_ref())
                    .is_some_and(|missed| missed.unserved),
                why: failure_words(refused),
            }
        })?;
        let mut unread = None;
        if matches!(bound, Bound::Nobody) {
            if let Some(own) = at_own_place(places, &word) {
                return Err(itself(&own).into());
            }
            if read {
                unread = missed;
            } else {
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
                        word = host_word(places, what, None)?;
                        if let Some(own) = this_machine_alone(places, &word)? {
                            return Err(itself(&own).into());
                        }
                        bound = bound_peer(places, &word, None).map_err(failure_words)?;
                    }
                    Ok(false) => {}
                    Err(why) => unread = Some(why),
                }
            }
        }
        let keys = &keys;
        let peer = match bound {
            Bound::Node(peer) | Bound::Unbound(peer) | Bound::ByHand(peer) => peer,
            // A node's word no book here binds is never asked of DNS or multicast DNS as a host.
            Bound::Nobody if member::is_node_word(what) => {
                return Err(aterm_ctl::FleetRefused {
                    why: names_no_peer(places, what, unread.as_ref()),
                    unserved: unread
                        .as_ref()
                        .is_some_and(|missed| missed.silent && missed.unserved),
                });
            }
            Bound::Nobody => {
                let (host, port) = member::parse_host_port(what, Some(member::default_port()))
                    .map_err(|why| {
                        format!(
                            "`{}` is no peer this machine knows (`aterm fleet peers`), and no \
                             host: {why}",
                            aterm_codec::tty::one_line(what, 80)
                        )
                    })?;
                manual_peer(host, port)
            }
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
        // A row that answered gets the note too: no book here says which member it is.
        let note = unread.as_ref().map(|missed| {
            if let Err(found) = &mut row.answer {
                let lead = if missed.silent {
                    "issuer silent, as a host: "
                } else {
                    "no issuer's book, as a host: "
                };
                let n = |s: &str| s.chars().count();
                if n(lead) + n(found) + n(&row.label) <= aterm_ctl::UNANSWERED_ROOM {
                    found.insert_str(0, lead);
                }
            }
            format!(
                "{}, so no book here names `{}`, and it was read as a host{}",
                missed.why,
                aterm_codec::tty::one_line(what, 80),
                if missed.silent {
                    ": try again once the issuer answers (`aterm fleet peers` says why it does \
                     not)"
                } else {
                    " (`aterm fleet peers` says why the book is not read)"
                }
            )
        });
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
    let dropped = keep_book(places, &secret, (&reads, &book_read), own.as_deref());
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
                addr: None,
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
        addr: read
            .as_ref()
            .ok()
            .map(|surveyed| surveyed.addr.ip().to_string()),
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
    machine: Option<String>,
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
        machine: string("machine"),
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
    without_fleet_keys(text, &["listen", "allow", "drive_from"])
}

/// [`without_fleet`] for `keys` of `[fleet]` alone (each `listen`, `allow` or `drive_from`).
/// # Errors
/// As [`without_fleet`]'s.
fn without_fleet_keys(text: &str, keys: &[&str]) -> Result<Option<String>, String> {
    use aterm_toml::edit::DocumentMut;
    let now = fleet_table(text)?;
    let set = |key: &str| match key {
        "listen" => now.listen.is_some(),
        "allow" => now.allow.is_some(),
        _ => !now.drive_from.is_empty(),
    };
    if !keys.iter().any(|key| set(key)) {
        return Ok(None);
    }
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("aterm.toml is not valid TOML, so it is left as it was: {e}"))?;
    for key in keys {
        doc.remove_path(&["fleet", key]);
    }
    if doc
        .get("fleet")
        .and_then(aterm_toml::edit::Item::as_table_like)
        .is_some_and(|fleet| fleet.is_empty())
    {
        doc.remove("fleet");
    }
    let edited = doc.to_string();
    reads_back(text, &edited, |fleet| {
        for key in keys {
            fleet.remove(key);
        }
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

/// What an edit of aterm.toml's `[fleet]` is made beside ([`replace_held`]): the book's lock, or a leave's or a level's hold.
trait Beside {}

impl Beside for HeldBook<'_> {}

impl Beside for Leaving<'_> {}

impl Beside for Leveling {}

/// Replace aterm.toml as `config` read it with `text`, beside `_held` ([`member::write_held`], [`member::leave_held`], [`member::level_held`]), and aterm.toml's write lock after the book's: every edit of `[fleet]`.
///
/// An edit whose folder's flush failed after its rename ([`EditLanded`]) is in effect, so it is
/// no failure: [`Edited::said`] says a power loss may take it back, beside the edit, and the
/// verb goes on to its end and its exit status. A failure would have the person believe
/// aterm.toml unchanged while `[fleet]` was what the next start reads.
fn replace_held(_held: &impl Beside, config: &ConfigFile, text: &str) -> Result<Edited, Failure> {
    match config.lock().and_then(|edit| edit.replace(text)) {
        Ok(bak) => Ok(Edited {
            bak,
            unflushed: None,
        }),
        Err(e) => match EditLanded::of(&e) {
            Some(landed) => Ok(Edited {
                bak: landed.bak.clone(),
                unflushed: Some(landed.flush.to_string()),
            }),
            None => Err(failed(e)),
        },
    }
}

/// What [`replace_held`] left in effect.
#[derive(Debug)]
struct Edited {
    /// Where the previous text was saved; `None` when there was no file.
    bak: Option<PathBuf>,
    /// The folder's flush error, where it failed after the edit's rename.
    unflushed: Option<String>,
}

impl Edited {
    /// What a report says after the edited file's name: where the previous text was saved,
    /// and, where the folder's flush failed, that a power loss may take the edit back.
    fn said(&self) -> String {
        let mut said = self.bak.as_ref().map_or_else(String::new, |b| {
            format!(" (previous saved as {})", b.display())
        });
        if let Some(flush) = &self.unflushed {
            said.push_str(&format!(
                "; the edit is in effect, but flushing its folder failed, so a power loss may \
                 take it back: {flush}"
            ));
        }
        said
    }
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

/// The line saying the drive grants `had` were removed, as `why` says, and what that `left`.
fn grants_removed(had: &[String], why: &str, left: &str) -> String {
    format!(
        "  drive      {} of [fleet] drive_from removed, as {why}: no Mac drives this one \
         until {} grants again{left}\n",
        if had.len() == 1 {
            "its grant".to_owned()
        } else {
            format!("its {} grants", had.len())
        },
        grants_again(had)
    )
}

/// The verb that grants a drive again once the grants `had` are gone: `--all` too where the `"*"`
/// grant was among them.
fn grants_again(had: &[String]) -> &'static str {
    if had
        .iter()
        .any(|word| word.trim() == aterm_net::fleet::ALL_MEMBERS)
    {
        "`aterm fleet allow drive --all` (every member, as before) or `--from <host|node>`"
    } else {
        "`aterm fleet allow drive --from <host|node>`"
    }
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
            format!("  config     {said} in {}{}\n", path.display(), bak.said())
        }
    };
    let gone =
        (had > 0 && (ungrant.is_some() || allow != aterm_net::fleet::DRIVE_LEVEL)).then(|| {
            grants_removed(
                &before.drive_from,
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

fn census(places: &Places, args: &[String]) -> Result<String, Failure> {
    let mut json = false;
    let mut force_refresh = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--refresh" => force_refresh = true,
            other => {
                return Err(Failure::Usage(format!(
                    "unexpected argument `{other}` (expected `[--json] [--refresh]`)"
                )));
            }
        }
    }
    let home = aterm_types::dirs::home_dir();
    let now_s = aterm_time::unix_secs().unwrap_or(0);
    let snapshot = crate::fleet_census::read_or_compute_census(
        &places.state,
        home.as_deref(),
        force_refresh,
        now_s,
    )
    .map_err(failed)?;
    if json {
        Ok(snapshot.to_json())
    } else {
        Ok(snapshot.render_human())
    }
}

#[cfg(test)]
#[path = "fleet_member_tests.rs"]
mod tests;
