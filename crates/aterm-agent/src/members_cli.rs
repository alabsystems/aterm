// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm members`: Macs joining ALab and the members that let them in
//! (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.3, F10). The member's verbs (`requests`,
//! `allow`, `invite`, `ignore`) go to this Mac's running window, which holds the desk and shows
//! the phrase on its own screen; they never print the phrase. The joiner's `join` finds ALab's
//! members on the LAN by itself, asks each, and runs the phrase a person typed with the member
//! that shows it; `--no-wait` asks and returns, as install.sh's windowless lanes run it, having
//! joined the ALab vault without waiting and started this Mac's vault subscriber, which asks the
//! members again while this Mac waits.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use aterm_secrets::client::{self, Paths};
use aterm_secrets::commons::bootstrap::Device;
use aterm_secrets::members::desk::Answer;
use aterm_secrets::members::join::{self, Dial, Heard, Stream};
use aterm_secrets::members::joiner::Joining;
use aterm_secrets::phrase::Phrase;

const USAGE: &str = "aterm members: Macs joining ALab and the members that let them in
  requests              the Macs asking that this Mac hears
  allow <name>          show a phrase for that Mac on this Mac's screen
  invite                show a phrase for whichever Mac types it next
  ignore <name>         hide that Mac's request on this Mac
  join [<phrase>] [--member <name>] [--at <host:port>]... [--no-wait]
                        find ALab, ask every member, and type the phrase one shows
  status                whether this Mac is a member, waiting, or neither";

/// How long the joiner browses for members.
const BROWSE_WITHIN: Duration = Duration::from_secs(3);

/// Each member's dial and every exchange on it.
const DIAL_WITHIN: Duration = Duration::from_secs(5);

/// How often a waiting join asks again.
const ASK_EVERY: Duration = Duration::from_secs(3);

/// The port a member's fleet listener serves when `--at` names none.
const FLEET_PORT: u16 = 7100;

/// `aterm members …`: `argv[1..]` in, exit code out.
pub fn main_entry(argv: Vec<OsString>) -> ExitCode {
    let args: Vec<String> = argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("requests") if args.len() == 1 => {
            aterm_ctl::main_entry(vec!["members".into(), "requests".into()])
        }
        Some("allow" | "ignore") if args.len() == 2 => to_window(&args),
        Some("invite") if args.len() == 1 => to_window(&args),
        Some("join") => match JoinArgs::parse(&args[1..]) {
            Ok(join) => run_join(&join),
            Err(why) => {
                eprintln!("aterm members join: {why}");
                ExitCode::from(2)
            }
        },
        Some("status") if args.len() == 1 => status(),
        Some(_) => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// A member's verb, sent to this Mac's running window, which answers one line. `allow` and
/// `invite` with no window (SSH alone, no login) refuse as §3.3.5 says.
fn to_window(args: &[String]) -> ExitCode {
    let shows = matches!(args[0].as_str(), "allow" | "invite");
    let Some(sock) = aterm_ctl::front_door_instance() else {
        eprintln!(
            "aterm members: {}",
            if shows {
                "press Allow on a Mac with a screen"
            } else {
                "no aterm window runs on this Mac"
            }
        );
        return ExitCode::from(1);
    };
    match aterm_ctl::front_door_send(&sock, &format!("members {}\n", args.join(" "))) {
        Ok(reply) => match reply.strip_prefix("OK ") {
            Some(said) => {
                println!("{said}");
                ExitCode::SUCCESS
            }
            None => {
                let why = reply.strip_prefix("ERR ").unwrap_or(&reply);
                eprintln!(
                    "aterm {}",
                    why.strip_prefix("members: ")
                        .map_or(why.to_owned(), |why| format!("members: {why}"))
                );
                ExitCode::from(1)
            }
        },
        Err(error) => {
            eprintln!("aterm members: this Mac's window did not answer: {error}");
            ExitCode::from(1)
        }
    }
}

/// `join`'s arguments.
#[derive(Debug, Default, PartialEq, Eq)]
struct JoinArgs {
    phrase: Option<String>,
    member: Option<String>,
    at: Vec<String>,
    no_wait: bool,
}

impl JoinArgs {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut out = Self::default();
        let mut words: Vec<&str> = Vec::new();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--no-wait" => out.no_wait = true,
                "--member" => {
                    out.member = Some(args.next().ok_or("--member needs a name")?.clone());
                }
                "--at" => {
                    let at = args.next().ok_or("--at needs a host or host:port")?;
                    out.at.push(
                        if at
                            .rsplit_once(':')
                            .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
                        {
                            at.clone()
                        } else {
                            format!("{at}:{FLEET_PORT}")
                        },
                    );
                }
                flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
                word => words.push(word),
            }
        }
        if !words.is_empty() {
            out.phrase = Some(words.join(" "));
        }
        if out.no_wait && out.phrase.is_some() {
            return Err("--no-wait asks and returns; give the phrase without it".to_owned());
        }
        Ok(out)
    }
}

/// The joining Mac: its paths, Commons device, node key, name and model.
struct Me {
    paths: Paths,
    device: Device,
    node: [u8; 32],
    name: String,
    model: String,
}

impl Me {
    fn live() -> Result<Self, String> {
        Self::at(Paths::live().map_err(|error| error.to_string())?)
    }

    /// The Mac whose vault files `paths` names, with this process's node key.
    fn at(paths: Paths) -> Result<Self, String> {
        let state = aterm_types::dirs::state_dir().ok_or("no state directory")?;
        Self::at_state(paths, &state)
    }

    /// [`Self::at`] with its node key in the state directory `state`. The directory is made
    /// private first, as `fleet init` and `fleet join` make it: install.sh leaves it 0755 (the
    /// package store under it) and a new home has none, and the book's lock refuses either.
    fn at_state(paths: Paths, state: &Path) -> Result<Self, String> {
        client::machine_identity(&paths).map_err(|error| error.to_string())?;
        let device = Device::open(&paths)
            .map_err(|error| error.to_string())?
            .ok_or("this Mac has no machine key")?;
        aterm_net::fleet_member::ensure_private_dir(state)
            .map_err(|error| format!("this Mac's state directory: {error}"))?;
        let node = aterm_net::fleet_member::node_identity(state)
            .map_err(|error| format!("this Mac's node key: {error}"))?;
        let name = client::config(&paths)
            .ok()
            .flatten()
            .map(|config| config.host)
            .filter(|host| !host.is_empty())
            .unwrap_or_else(client::default_host_name);
        Ok(Self {
            paths,
            device,
            node: *node.key().bytes(),
            name,
            model: model(),
        })
    }

    fn joining(&self) -> Joining<'_> {
        Joining {
            device: &self.device,
            node: self.node,
            name: self.name.clone(),
            model: self.model.clone(),
        }
    }
}

/// This Mac's model identifier (`Mac15,3`), for the member's row.
fn model() -> String {
    #[cfg(target_os = "macos")]
    {
        let mut sysctl = aterm_uds::spawnfd::command(aterm_uds::os_path!("sysctl"));
        sysctl.args(["-n", "hw.model"]);
        let out = aterm_uds::deadline::output_within(&mut sysctl, DIAL_WITHIN);
        if let Ok(out) = out
            && let Ok(model) = String::from_utf8(out.stdout)
        {
            let model = model.trim();
            if !model.is_empty() && model.bytes().all(|b| b == b' ' || b.is_ascii_graphic()) {
                return model.to_owned();
            }
        }
    }
    std::env::consts::OS.to_owned()
}

/// Reach the member at `at` (`host:port`): its `ask` op, every step within [`DIAL_WITHIN`].
fn dial(at: &str) -> aterm_secrets::Result<Box<dyn Stream>> {
    let (host, port) = at
        .rsplit_once(':')
        .and_then(|(host, port)| Some((host, port.parse::<u16>().ok()?)))
        .ok_or_else(|| aterm_secrets::Error::new(format!("{at} is not host:port")))?;
    let (transport, _) = aterm_net::fleet_member::dial_ask(host, None, port, DIAL_WITHIN)
        .map_err(|error| aterm_secrets::Error::new(error.to_string()))?;
    let stream = transport.into_stream();
    stream.sock.set_read_timeout(Some(DIAL_WITHIN))?;
    stream.sock.set_write_timeout(Some(DIAL_WITHIN))?;
    Ok(Box::new(stream))
}

/// Whether this Mac's vault subscriber (`aterm secrets watch`) runs. Only it follows the vault
/// once the vault records this Mac, and asks again while this Mac waits; nothing else on a lane
/// with no window (SSH, `--no-app`, Linux) starts one, since the window's mend and System State's
/// Secrets stage run only once a window opens, and install.sh's next run passes over a Mac that
/// holds a vault configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Follows {
    /// It runs.
    Runs,
    /// A person turned Sync off on this Mac, which stands.
    Off,
    /// None is installed.
    Absent,
    /// It does not run: why.
    Failed(String),
}

impl Follows {
    /// Install this Mac's subscriber unless it runs or a person turned Sync off, as the window's
    /// mend repairs it: with the program its unit names when that one lasts, else this aterm.
    fn live(home: &Path) -> Self {
        use aterm_secrets::service::{self, Job};
        let program = match service::kept_program(home, Job::Subscriber) {
            Some(program) => Ok(program),
            None => service::program_for_service(),
        };
        let repaired = program.and_then(|program| {
            service::repair_subscriber(home, &program, &["secrets".to_owned(), "watch".to_owned()])
        });
        match repaired {
            Ok(Some(_)) => Self::Runs,
            Ok(None) => Self::Off,
            Err(error) => Self::Failed(error.to_string()),
        }
    }

    /// Whether this Mac's subscriber is installed now, read only (`aterm members status`).
    fn now(home: &Path) -> Self {
        use aterm_secrets::service::{self, Job, State};
        match service::state(home, Job::Subscriber) {
            Ok(State::Installed { .. }) => Self::Runs,
            Ok(State::Absent) if service::subscriber_turned_off(home) => Self::Off,
            Ok(State::Absent) => Self::Absent,
            Err(error) => Self::Failed(error.to_string()),
        }
    }

    /// What a person runs to follow the vault when nothing on this Mac does.
    fn fix(&self) -> Option<String> {
        const ON: &str = "`aterm secrets agent on` starts the one that does";
        match self {
            Self::Runs => None,
            Self::Off => Some(
                "Sync is off on this Mac, so it follows no vault change: `aterm secrets agent on` \
                 turns it on"
                    .to_owned(),
            ),
            Self::Absent => Some(format!(
                "no vault subscriber runs on this Mac, so it follows no vault change: {ON}"
            )),
            Self::Failed(why) => Some(format!(
                "this Mac's vault subscriber does not run ({why}), so it follows no vault change: \
                 {ON}"
            )),
        }
    }
}

/// What a Mac a member let in says while it waits for the vault to record it: that finishes by
/// itself only while its vault subscriber runs (`follows`).
fn allowed_words(follows: &Follows) -> String {
    match follows.fix() {
        None => {
            "allowed: ~/.secrets is written, and this Mac follows vault changes once the vault \
                 records it (that finishes by itself)"
                .to_owned()
        }
        Some(fix) => format!("allowed: ~/.secrets is written, but {fix}"),
    }
}

/// What `join` reaches beyond this process: ALab's members on this network, the ALab vault and
/// this Mac's vault subscriber. [`run_join`]'s are live; a test's stand in for them.
struct Reach<'a> {
    /// The members on this network, each `host:port`.
    browse: &'a (dyn Fn() -> aterm_secrets::Result<Vec<String>> + Sync),
    /// One member's `ask` op.
    dial: &'a Dial<'a>,
    /// Join the ALab vault without waiting ([`join_alab_vault`]).
    join_vault: &'a (dyn Fn(&Paths) -> aterm_secrets::Result<()> + Sync),
    /// Make sure this Mac's vault subscriber runs ([`Follows::live`]).
    follow: &'a (dyn Fn(&Path) -> Follows + Sync),
}

fn run_join(args: &JoinArgs) -> ExitCode {
    let me = match Me::live() {
        Ok(me) => me,
        Err(why) => {
            eprintln!("aterm members join: {why}");
            return ExitCode::from(1);
        }
    };
    let reach = Reach {
        browse: &|| aterm_secrets::discover::browse_members(BROWSE_WITHIN),
        dial: &dial,
        join_vault: &join_alab_vault,
        follow: &Follows::live,
    };
    ExitCode::from(join_as(
        &me,
        args,
        &reach,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    ))
}

/// `join` as `me`, through `reach`, saying what it did on `out` and `err`: the exit code.
fn join_as(
    me: &Me,
    args: &JoinArgs,
    reach: &Reach<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let membership = client::status(&me.paths).map(|status| status.membership);
    if matches!(membership, Ok(client::Membership::In { .. })) {
        let _ = writeln!(out, "This Mac is already in ALab as {}.", me.name);
        return 0;
    }
    // A Mac a member let in waits for the vault to record it and asks no member again: the
    // member that let it in, and each it told, closed its row, so "press Allow" would send the
    // person to a Mac that shows nothing to Allow. It makes sure its subscriber follows the vault
    // and says where it stands, as `status` does.
    if matches!(membership, Ok(client::Membership::Pending)) && client::holds_allowed(&me.paths) {
        let _ = writeln!(out, "{}", allowed_words(&(reach.follow)(&me.paths.home)));
        return 0;
    }
    let joining = me.joining();
    let dial = reach.dial;
    // `--no-wait` sets this Mac up at the ALab vault while it asks the members, each within its
    // own bound, so install.sh's lane ends within its own.
    let (asked, waits) = std::thread::scope(|scope| {
        let waits = args
            .no_wait
            .then(|| scope.spawn(|| wait_at_vault(&me.paths, reach)));
        let found = if args.at.is_empty() {
            (reach.browse)()
        } else {
            Ok(args.at.clone())
        };
        let asked = found.map(|ats| {
            let (heard, missed) = join::ask_all(dial, &joining, &ats);
            (ats, heard, missed)
        });
        let waits = waits.map(|waiting| {
            waiting
                .join()
                .unwrap_or_else(|_| Err("the vault join stopped".to_owned()))
        });
        (asked, waits)
    });
    let (ats, mut heard, missed) = match (asked, waits) {
        (Ok(asked), None) => asked,
        (Ok((_, heard, missed)), Some(waits)) => {
            return said_no_wait(Ok(&heard), &missed, waits, out, err);
        }
        // A Mac that cannot browse (off macOS, or one macOS refuses the local network) still
        // waits at the vault, and says why it asked no member where install.sh shows it.
        (Err(error), Some(waits)) => {
            return said_no_wait(Err(error.to_string()), &[], waits, out, err);
        }
        (Err(error), None) => {
            let _ = writeln!(err, "aterm members join: {error}");
            return 1;
        }
    };
    if heard.is_empty() {
        let _ = writeln!(
            err,
            "aterm members join: no ALab member answers on this network"
        );
        for (at, why) in &missed {
            let _ = writeln!(err, "  {at}: {why}");
        }
        return 1;
    }
    let phrase = match &args.phrase {
        Some(phrase) => phrase.clone(),
        None => {
            let _ = writeln!(
                out,
                "This Mac is {}. To join ALab, press Allow on any Mac in ALab, then type the \
                 phrase that Mac shows.",
                me.name
            );
            heard = wait_for_allow(dial, &joining, &ats, heard);
            let Some(phrase) = read_phrase(&heard, args.member.as_deref()) else {
                return 1;
            };
            // Ask again now that the person typed it: the run goes to the member that shows a
            // phrase for this Mac now, not to whichever answered first while this Mac waited
            // (a host that says it shows an invite ends that wait at once).
            let (again, _) = join::ask_all(dial, &joining, &ats);
            if !again.is_empty() {
                heard = again;
            }
            phrase
        }
    };
    let phrase = match Phrase::parse(&phrase) {
        Ok(phrase) => phrase,
        Err(error) => {
            let _ = writeln!(err, "aterm members join: {error}");
            return 2;
        }
    };
    match join::run_phrase(
        &me.paths,
        dial,
        &joining,
        &heard,
        &phrase,
        args.member.as_deref(),
    ) {
        Ok(joined) => {
            // The bundle set this Mac up for the vault; only its subscriber follows it from here.
            let follows = (reach.follow)(&me.paths.home);
            for line in joined_lines(&joined, &follows) {
                let _ = writeln!(out, "{line}");
            }
            0
        }
        Err(error) => {
            let _ = writeln!(err, "aterm members join: {error}");
            1
        }
    }
}

/// Join the ALab vault without waiting, as System State's Secrets stage does: its configuration,
/// a Pending state and a queued request. Nothing is asked of a person: no password, and no
/// signer but the ALab vault's own, which this build pins.
fn join_alab_vault(paths: &Paths) -> aterm_secrets::Result<()> {
    client::join(
        paths,
        &client::JoinOptions {
            source: String::new(),
            host: None,
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: client::JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .map(drop)
}

/// `join --no-wait`'s vault half, as System State's Secrets stage joins: with no vault set up
/// here, join the ALab vault without waiting, then make sure this Mac's vault subscriber runs.
/// While this Mac waits on the ALab vault, that subscriber asks ALab's members again every
/// minute (`aterm_secrets::client::ask_members_if_due`), so a member that was asleep, away or
/// restarting its window when this asked shows the row too. `Err` says why no vault is set up.
fn wait_at_vault(paths: &Paths, reach: &Reach<'_>) -> Result<Follows, String> {
    let unset = || !matches!(client::config(paths), Ok(Some(_)));
    if unset()
        && let Err(error) = (reach.join_vault)(paths)
        && unset()
    {
        return Err(error.to_string());
    }
    Ok((reach.follow)(&paths.home))
}

/// What `join --no-wait` says, and its exit code; its first line is the one install.sh shows.
/// `heard` answered (`Err`: why no member could be looked for), `missed` did not, and `waits`
/// says whether this Mac waits at the ALab vault and whether its subscriber, which asks the
/// members again through the same browse, runs.
fn said_no_wait(
    heard: Result<&[Heard], String>,
    missed: &[(String, String)],
    waits: Result<Follows, String>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let fix = waits.as_ref().ok().and_then(Follows::fix);
    // `None`: a member heard; `Some(None)`: none answered; `Some(Some(why))`: none was looked for.
    let heard = match heard {
        Ok(heard) => heard.is_empty().then_some(None),
        Err(why) => Some(Some(why)),
    };
    match (heard, waits) {
        (None, _) => {
            let _ = writeln!(out, "{}", join::LET_IN);
        }
        (Some(Some(why)), Ok(_)) => {
            let _ = writeln!(out, "{why}; this Mac's request waits at the ALab vault");
        }
        (Some(Some(why)), Err(vault)) => {
            let _ = writeln!(
                err,
                "aterm members join: {why}, and the ALab vault was not reached ({vault})"
            );
            return 1;
        }
        (Some(None), Ok(Follows::Runs)) => {
            let _ = writeln!(
                out,
                "no ALab member answers on this network yet, and this Mac's vault subscriber asks \
                 them again every minute: {}",
                join::LET_IN
            );
        }
        (Some(None), Ok(_)) => {
            let _ = writeln!(
                out,
                "no ALab member answers on this network yet: {}",
                join::LET_IN
            );
        }
        (Some(None), Err(why)) => {
            let _ = writeln!(
                err,
                "aterm members join: no ALab member answers on this network, and the ALab vault \
                 was not reached ({why})"
            );
            for (at, why) in missed {
                let _ = writeln!(err, "  {at}: {why}");
            }
            return 1;
        }
    }
    if let Some(fix) = fix {
        let _ = writeln!(out, "{fix}");
    }
    0
}

/// What a run with the phrase says, `follows` being whether this Mac's vault subscriber runs:
/// only a running one follows the vault with no one here and asks the vault's server again.
fn joined_lines(joined: &join::Joined, follows: &Follows) -> Vec<String> {
    let runs = *follows == Follows::Runs;
    let files = joined.report.written.len() + joined.report.unchanged.len();
    // Only the log's `Admit` makes this Mac a member at once; F10's interim leaves it pending in
    // the vault until an admin Mac's approver records the member's `Allowed`.
    let mut lines = vec![match &joined.admission {
        join::Admission::Logged => format!(
            "Joined ALab as {} (let in by {}): {files} file(s) written to ~/.secrets.",
            joined.name, joined.member
        ),
        join::Admission::Asked(_) => format!(
            "Allowed into ALab as {} by {}: {files} file(s) written to ~/.secrets; the vault \
             lists this Mac as pending until an admin Mac's approver records it.",
            joined.name, joined.member
        ),
    }];
    lines.push(match (&joined.admission, runs) {
        (join::Admission::Logged, true) => format!(
            "{} signed this Mac into ALab's membership log; this Mac follows vault changes from \
             the log, which needs no one here.",
            joined.member
        ),
        (join::Admission::Logged, false) => format!(
            "{} signed this Mac into ALab's membership log.",
            joined.member
        ),
        (join::Admission::Asked(Ok(code)), true) => format!(
            "The vault's server holds the request (code {code}); this Mac follows vault changes \
             once the vault records it, which needs no one here."
        ),
        (join::Admission::Asked(Err(why)), true) => format!(
            "No vault server took the request yet ({why}); this Mac's vault subscriber asks \
             again every hour while it waits."
        ),
        (join::Admission::Asked(Ok(code)), false) => {
            format!("The vault's server holds the request (code {code}).")
        }
        (join::Admission::Asked(Err(why)), false) => {
            format!("No vault server took the request yet ({why}).")
        }
    });
    lines.extend(follows.fix());
    lines
}

/// Ask every ALab member this Mac hears on the LAN, as `join --no-wait` does, for the Mac whose
/// home is `home`: how many heard it. The Secrets stage of System State and `aterm init` asks
/// through it once its join waits on the ALab vault
/// (`aterm_machines::init_secrets::on_ask_members`), so a member shows a row with nobody
/// running `aterm members join` first, and that Mac's vault subscriber asks through it every
/// minute while it waits (`aterm_secrets::client::on_ask_members`), so a member that wakes,
/// joins the LAN or restarts its window since shows the row too.
/// # Errors
/// This Mac is already a member, has no machine or node key, or no member answered.
pub fn ask_members(home: &Path) -> Result<usize, String> {
    let me = Me::at(Paths::for_home(home))?;
    if client::status(&me.paths)
        .is_ok_and(|status| matches!(status.membership, client::Membership::In { .. }))
    {
        return Err(format!("this Mac is already in ALab as {}", me.name));
    }
    let ats = aterm_secrets::discover::browse_members(BROWSE_WITHIN)
        .map_err(|error| error.to_string())?;
    let dial: &Dial<'_> = &dial;
    let (heard, _) = join::ask_all(dial, &me.joining(), &ats);
    if heard.is_empty() {
        return Err("no ALab member answers on this network".to_owned());
    }
    Ok(heard.len())
}

/// Ask every member again every [`ASK_EVERY`] until one says a person allowed this Mac.
fn wait_for_allow(
    dial: &Dial<'_>,
    joining: &Joining<'_>,
    ats: &[String],
    mut heard: Vec<Heard>,
) -> Vec<Heard> {
    while !heard
        .iter()
        .any(|heard| heard.answer == Answer::Allowed || heard.greeting.invite)
    {
        std::thread::sleep(ASK_EVERY);
        let (again, _) = join::ask_all(dial, joining, ats);
        if !again.is_empty() {
            heard = again;
        }
    }
    heard
}

/// Read the phrase a person types, naming the member that shows it.
fn read_phrase(heard: &[Heard], member: Option<&str>) -> Option<String> {
    let shows = join::pick(heard, member)
        .map(|heard| heard.greeting.name.clone())
        .unwrap_or_else(|_| "the member".to_owned());
    print!("The phrase {shows} shows: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(n) if n > 0 => Some(line.trim().to_owned()),
        _ => {
            eprintln!("aterm members join: no phrase was typed");
            None
        }
    }
}

/// `status`: a member; allowed, with `~/.secrets` written, until the vault records it; asking;
/// or not in ALab.
fn status() -> ExitCode {
    let Ok(paths) = Paths::live() else {
        eprintln!("aterm members: no home directory");
        return ExitCode::from(1);
    };
    println!("{}", status_said(&paths, &Follows::now));
    ExitCode::SUCCESS
}

/// `status`'s line for the Mac at `paths`, `follows` reading whether its vault subscriber runs.
fn status_said(paths: &Paths, follows: &dyn Fn(&Path) -> Follows) -> String {
    match client::status(paths).map(|status| status.membership) {
        Ok(client::Membership::In { .. }) => {
            let name = client::config(paths)
                .ok()
                .flatten()
                .map_or_else(client::default_host_name, |config| config.host);
            format!("member as {name}")
        }
        Ok(client::Membership::Pending) if client::holds_allowed(paths) => {
            allowed_words(&follows(&paths.home))
        }
        Ok(client::Membership::Pending) => {
            format!("asking: {}", aterm_secrets::members::join::LET_IN)
        }
        Ok(_) => "not in ALab: run `aterm members join` here".to_owned(),
        Err(error) => format!("unknown: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_secrets::SecretBytes;
    use aterm_secrets::commons::journal::Trust;
    use aterm_secrets::members::Bundle;
    use aterm_secrets::members::bundle::Handed;
    use aterm_secrets::members::desk::{self, Desk, Handing, Handout, Local};
    use aterm_secrets::text::{self, Audience, Entry};
    use std::net::IpAddr;
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::sync::Mutex;

    const TRUST: Trust = Trust {
        network: [1; 16],
        authority: [2; 32],
        genesis: [3; 32],
    };

    /// A Mac with a synthetic machine key and Commons device under `root/name`.
    fn mac(root: &Path, name: &str) -> (Paths, Device) {
        let paths = Paths::for_home(&root.join(name));
        client::machine_identity(&paths).expect("identity");
        let device = Device::open(&paths).expect("device").expect("an identity");
        (paths, device)
    }

    /// The joining Mac `name` under `root`, with a synthetic node key.
    fn me(root: &Path, name: &str) -> Me {
        let (paths, device) = mac(root, name);
        Me {
            paths,
            device,
            node: [7; 32],
            name: name.to_owned(),
            model: "Mac15,3".to_owned(),
        }
    }

    /// The desk of the member `name` under `root`, which can allow.
    fn member(root: &Path, name: &str) -> Mutex<Desk> {
        let (_, device) = mac(root, name);
        Mutex::new(Desk::new(
            Local::with(device, TRUST, true, name).expect("member"),
        ))
    }

    /// The bundle a member hands: F10's files, one synthetic `[everyone]` file, and a vault
    /// server that refuses at once (nothing listens on port 1).
    fn bundle(handing: &Handing<'_>) -> aterm_secrets::Result<Handout> {
        let bundle = Bundle {
            vault: "alab".into(),
            signer: [9; 32],
            followed: Vec::new(),
            servers: vec!["http://127.0.0.1:1/v1/alab".into()],
            allowed: handing.allowed.clone(),
            handed: Handed::Files(text::render_everyone_locker(&[Entry {
                name: "gh_access_token".into(),
                value: SecretBytes::new(b"synthetic-gh-token".to_vec()),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            }])),
        }
        .encode()?;
        Ok(Handout {
            bundle,
            logged: false,
            note: None,
        })
    }

    /// Allow the one row on `desk`: the words its sheet shows.
    fn allow(desk: &Mutex<Desk>) -> String {
        let mut desk = desk.lock().unwrap();
        let now = aterm_time::Instant::now();
        let row = desk.rows(now)[0].id;
        let phrase = desk.allow(row, now).expect("allowed");
        desk.words(phrase, now).expect("live").join(" ")
    }

    /// The member at [`M1`].
    const M1: &str = "m1:7100";

    /// Run `body` with a dial that reaches `desk` at [`M1`] (every other address refuses), the
    /// desk serving each connection on its own thread with a one-minute hang detector on both
    /// ends; and how many times it was dialed.
    fn with_member<T>(desk: &Mutex<Desk>, body: impl FnOnce(&Dial<'_>) -> T) -> (T, usize) {
        let dials = std::sync::atomic::AtomicUsize::new(0);
        let said = std::thread::scope(|scope| {
            let dial = |at: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
                if at != M1 {
                    return Err(aterm_secrets::Error::new("connection refused"));
                }
                dials.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let (ours, theirs) = UnixStream::pair()?;
                for end in [&ours, &theirs] {
                    end.set_read_timeout(Some(Duration::from_secs(60)))?;
                }
                scope.spawn(move || {
                    let mut ours = ours;
                    let _ = desk::serve(desk, &mut ours, IpAddr::from([192, 0, 2, 9]), &bundle);
                });
                Ok(Box::new(theirs))
            };
            body(&dial)
        });
        (said, dials.into_inner())
    }

    /// A vault no test reaches.
    fn no_vault(_: &Paths) -> aterm_secrets::Result<()> {
        Err(aterm_secrets::Error::new("no vault in this test"))
    }

    /// `join_as` with `args` through `reach`: its exit code, and what it said on stdout and on
    /// stderr.
    fn joined_apart(me: &Me, args: &JoinArgs, reach: &Reach<'_>) -> (u8, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = join_as(me, args, reach, &mut out, &mut err);
        let (out, err) = (
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        );
        (code, out, err)
    }

    /// `join_as` with `args` through `reach`: its exit code, and what it said on both streams.
    fn joined(me: &Me, args: &JoinArgs, reach: &Reach<'_>) -> (u8, String) {
        let (code, out, err) = joined_apart(me, args, reach);
        (code, out + &err)
    }

    /// Let `j` in through `m1`: it asks, a person allows the row, and `j` runs the phrase with
    /// `follow` standing in for its subscriber's install. What the run said.
    fn let_in(m1: &Mutex<Desk>, j: &Me, follow: &(dyn Fn(&Path) -> Follows + Sync)) -> String {
        let ((code, said), _) = with_member(m1, |dial| {
            let ats = [M1.to_owned()];
            let (heard, missed) = join::ask_all(dial, &j.joining(), &ats);
            assert_eq!(heard.len(), 1, "{missed:?}");
            let words = allow(m1);
            let reach = Reach {
                browse: &|| Ok(Vec::new()),
                dial,
                join_vault: &no_vault,
                follow,
            };
            let args = JoinArgs {
                phrase: Some(words),
                at: ats.to_vec(),
                ..JoinArgs::default()
            };
            joined(j, &args, &reach)
        });
        assert_eq!(code, 0, "{said}");
        said
    }

    /// A Mac the phrase lets in on a lane with no window (SSH, `--no-app`, Linux) gets its vault
    /// subscriber from the run, since nothing else there starts one, and hears that it follows
    /// the vault with no one here only while that subscriber runs; `status` says the same.
    ///
    /// NEGATIVE CONTROL: drop the `(reach.follow)(…)` call in `join_as`'s `Ok` arm and print the
    /// two lines as before (`asks again every hour` whatever runs): no install is asked for, and
    /// the run promises a subscriber that does not run.
    #[test]
    fn a_mac_the_phrase_lets_in_starts_its_vault_subscriber_and_says_it_follows_only_when_one_runs()
    {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let m1 = member(root.path(), "m1");
        let j = me(root.path(), "j");
        let homes: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
        let follow = |home: &Path| {
            homes.lock().unwrap().push(home.to_path_buf());
            Follows::Failed("launchd refused it".to_owned())
        };
        let said = let_in(&m1, &j, &follow);
        assert!(
            client::config(&j.paths).expect("config").is_some(),
            "the bundle set up the vault: {said}"
        );
        assert_eq!(
            homes.lock().unwrap().as_slice(),
            std::slice::from_ref(&j.paths.home),
            "the run installs this Mac's subscriber: {said}"
        );
        assert!(
            !said.contains("needs no one here") && !said.contains("asks again every hour"),
            "{said}"
        );
        assert!(said.contains("aterm secrets agent on"), "{said}");
        // `status` there: it finishes by itself only while the subscriber runs.
        let absent = status_said(&j.paths, &|_| Follows::Absent);
        assert!(absent.contains("aterm secrets agent on"), "{absent}");
        assert!(!absent.contains("finishes by itself"), "{absent}");
        let runs = status_said(&j.paths, &|_| Follows::Runs);
        assert!(runs.contains("finishes by itself"), "{runs}");
    }

    /// A Mac a member let in that runs `aterm members join` again (one still waiting on a lane
    /// with no window, or install.sh's `--no-wait` on its next lane) says it was allowed and
    /// makes sure its subscriber runs. It asks no member and does not say to press Allow: the
    /// member that let it in, and each it told, closed its row and shows nothing to Allow.
    ///
    /// NEGATIVE CONTROL: drop the `holds_allowed` return in `join_as`: `--no-wait` asks m1 again,
    /// which answers Closed, and the Mac says "press Allow on any Mac in ALab" again.
    #[test]
    fn a_mac_let_in_already_says_so_and_asks_no_member_again() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let m1 = member(root.path(), "m1");
        let j = me(root.path(), "j");
        let_in(&m1, &j, &|_| Follows::Absent);
        let homes: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
        let follow = |home: &Path| {
            homes.lock().unwrap().push(home.to_path_buf());
            Follows::Runs
        };
        for args in [
            JoinArgs {
                no_wait: true,
                ..JoinArgs::default()
            },
            JoinArgs::default(),
        ] {
            let ((code, said), dials) = with_member(&m1, |dial| {
                let reach = Reach {
                    browse: &|| Ok(vec![M1.to_owned()]),
                    dial,
                    join_vault: &no_vault,
                    follow: &follow,
                };
                joined(&j, &args, &reach)
            });
            assert_eq!(code, 0, "{said}");
            assert_eq!(dials, 0, "no member is asked again: {said}");
            assert!(!said.contains(join::LET_IN), "{said}");
            assert!(said.contains("allowed:"), "{said}");
            assert!(said.contains("finishes by itself"), "{said}");
        }
        assert_eq!(
            homes.lock().unwrap().len(),
            2,
            "each run makes sure it follows"
        );
    }

    /// install.sh's windowless `members join --no-wait` sets the Mac up at the ALab vault (its
    /// configuration, Pending, a queued request) and starts its vault subscriber, as System
    /// State's Secrets stage does, whether or not a member answers. That subscriber asks ALab's
    /// members again every minute, so a member asleep, away or restarting its window when the
    /// lane asked shows the row too; and with no member answering, the lane says so rather than
    /// nothing.
    ///
    /// NEGATIVE CONTROL: never start `wait_at_vault` (`waits` always `None`): neither Mac joins
    /// the vault or gets a subscriber, and the one no member answers fails with nothing on
    /// stdout, the line install.sh shows.
    #[test]
    fn the_install_lane_waits_at_the_vault_with_a_subscriber_that_asks_the_members_again() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let m1 = member(root.path(), "m1");
        for (name, members) in [("j", vec![M1.to_owned()]), ("k", Vec::new())] {
            let mac = me(root.path(), name);
            let joins: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
            let homes: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
            let join_vault = |paths: &Paths| -> aterm_secrets::Result<()> {
                joins.lock().unwrap().push(paths.home.clone());
                Ok(())
            };
            let follow = |home: &Path| {
                homes.lock().unwrap().push(home.to_path_buf());
                Follows::Runs
            };
            let ((code, said), _) = with_member(&m1, |dial| {
                let reach = Reach {
                    browse: &|| Ok(members.clone()),
                    dial,
                    join_vault: &join_vault,
                    follow: &follow,
                };
                let args = JoinArgs {
                    no_wait: true,
                    ..JoinArgs::default()
                };
                joined(&mac, &args, &reach)
            });
            assert_eq!(code, 0, "{name}: {said}");
            let home = [mac.paths.home.clone()];
            assert_eq!(
                *joins.lock().unwrap(),
                home,
                "{name} joins the vault: {said}"
            );
            assert_eq!(*homes.lock().unwrap(), home, "{name} follows it: {said}");
            let first = said.lines().next().unwrap_or_default();
            assert!(first.ends_with(join::LET_IN), "{name}: {said}");
            if members.is_empty() {
                assert!(first.contains("asks them again every minute"), "{said}");
            }
        }
        let rows = m1.lock().unwrap().rows(aterm_time::Instant::now());
        assert_eq!(rows.len(), 1, "j's request waits on m1");
    }

    /// A member whose vault state a pass holds (an admission publishing on the admin Mac, an
    /// apply) waits for it inside the `ask` exchange, which the fleet listener's handshake
    /// watchdog cuts at `HANDSHAKE_WITHIN`. The wait leaves most of that bound to the TLS
    /// handshake, CPace and the possession proof, so the member's refusal ("Allow the new Mac
    /// again once it ends") reaches the new Mac rather than a cut socket, and a relayed bundle is
    /// never written into a socket the new Mac has left (the row closed and the `Allowed` posted
    /// for a Mac that wrote nothing).
    ///
    /// NEGATIVE CONTROL: put `HANDOVER_LOCK_WAIT` back at 60 s: the wait outlasts the bound.
    #[test]
    fn a_busy_members_handover_wait_ends_well_inside_the_ask_exchanges_bound() {
        let (wait, bound) = (
            client::HANDOVER_LOCK_WAIT,
            aterm_net::fleet::HANDSHAKE_WITHIN,
        );
        assert!(wait * 3 <= bound, "{wait:?} of the exchange's {bound:?}");
    }

    /// On a Mac that cannot browse for members (off macOS, where the browse says
    /// `discover::NO_BROWSE`, or one macOS refuses the local network), install.sh's
    /// `members join --no-wait` still sets the Mac up at the ALab vault with its subscriber, as
    /// `secrets join --no-wait` did on that lane before F10, and says why it asked no member on
    /// stdout, the line install.sh shows, rather than nothing; it does not promise that its
    /// subscriber asks the members, which browses the same way.
    ///
    /// NEGATIVE CONTROL: on a browse error return 1 with the error on stderr before
    /// `said_no_wait`, as before: stdout is empty, so install.sh shows nothing.
    #[test]
    fn a_mac_that_cannot_browse_still_waits_at_the_vault_and_says_why_it_asked_no_member() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let k = me(root.path(), "k");
        let joins: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
        let homes: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
        let join_vault = |paths: &Paths| -> aterm_secrets::Result<()> {
            joins.lock().unwrap().push(paths.home.clone());
            Ok(())
        };
        let follow = |home: &Path| {
            homes.lock().unwrap().push(home.to_path_buf());
            Follows::Runs
        };
        let unreached = |_: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
            Err(aterm_secrets::Error::new("no member is dialed"))
        };
        let reach = Reach {
            browse: &|| {
                Err(aterm_secrets::Error::new(
                    aterm_secrets::discover::NO_BROWSE,
                ))
            },
            dial: &unreached,
            join_vault: &join_vault,
            follow: &follow,
        };
        let args = JoinArgs {
            no_wait: true,
            ..JoinArgs::default()
        };
        let (code, out, err) = joined_apart(&k, &args, &reach);
        assert_eq!(code, 0, "{out}{err}");
        let home = [k.paths.home.clone()];
        assert_eq!(
            *joins.lock().unwrap(),
            home,
            "it joins the vault: {out}{err}"
        );
        assert_eq!(*homes.lock().unwrap(), home, "it follows it: {out}{err}");
        let first = out.lines().next().unwrap_or_default();
        assert!(
            first.contains(aterm_secrets::discover::NO_BROWSE),
            "{out}{err}"
        );
        assert!(!out.contains("asks them again"), "{out}");
    }

    /// A new Mac's `members join` mints its node key whether install.sh left the state
    /// directory open to others (0755, made with the package store under it) or there is none
    /// yet, and leaves it 0700, as `fleet init` and `fleet join` do.
    /// NEGATIVE CONTROL: drop `ensure_private_dir` from [`Me::at_state`] and both fail with
    /// "this Mac's node key: fleet-peers's lock: the directory …".
    #[cfg(unix)]
    #[test]
    fn a_new_macs_join_mints_its_node_key_where_install_sh_left_the_state_open_or_none() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let open = tmp.path().join("open");
        std::fs::create_dir(&open).expect("open state directory");
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).expect("0755");
        let none = tmp.path().join("none");
        for (home, state) in [("a", &open), ("b", &none)] {
            let home = tmp.path().join(home);
            std::fs::create_dir(&home).expect("home");
            let me = Me::at_state(Paths::for_home(&home), state).expect("the joining Mac");
            let kept = aterm_net::fleet_member::read_node_identity(state)
                .expect("read the node")
                .map(|node| *node.key().bytes());
            assert_eq!(kept, Some(me.node), "{}", state.display());
            let mode = std::fs::metadata(state)
                .expect("state directory")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "{}", state.display());
        }
    }

    #[test]
    fn join_takes_a_phrase_as_one_word_or_three_and_no_wait_alone() {
        let parse = |args: &[&str]| {
            JoinArgs::parse(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
        };
        assert_eq!(
            parse(&["maple", "river", "ocean"])
                .unwrap()
                .phrase
                .as_deref(),
            Some("maple river ocean")
        );
        assert_eq!(
            parse(&["maple river ocean", "--member", "m7"]).unwrap(),
            JoinArgs {
                phrase: Some("maple river ocean".into()),
                member: Some("m7".into()),
                ..JoinArgs::default()
            }
        );
        assert_eq!(
            parse(&["--no-wait", "--at", "m22.local", "--at", "10.0.0.2:7200"]).unwrap(),
            JoinArgs {
                no_wait: true,
                at: vec!["m22.local:7100".into(), "10.0.0.2:7200".into()],
                ..JoinArgs::default()
            }
        );
        assert!(parse(&["--no-wait", "maple", "river", "ocean"]).is_err());
        assert!(parse(&["--member"]).is_err());
        assert!(parse(&["--wait"]).is_err());
    }
}
