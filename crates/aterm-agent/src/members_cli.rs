// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm members`: Macs joining ALab and the members that let them in
//! (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.3, F10). The member's verbs (`requests`,
//! `allow`, `invite`, `ignore`) go to this Mac's running window, which holds the desk and shows
//! the phrase on its own screen; they never print the phrase. The default `join` finds
//! ALab's members, proves this Mac to them first ([`proved_back`]: a member on the home ALab LAN
//! lets it in at once, with nobody's Allow), asks them plainly only where none did, queues at
//! the default vault for an admin's approval, and starts the subscriber, which proves and asks
//! again while it waits. An explicitly supplied phrase retains the manual join flow.
//!
//! F16's verbs run in this process, on this Mac's own vault files: `list` prints the membership
//! log's members and the Macs it removed, `remove` takes a member out as this Mac (any member,
//! no admin), and `undo` lists the "Undo <label>'s removals" rows this Mac shows and where each
//! quorum stands. It signs nothing: a share of the quorum `Remove` is a person's act (§3.2), a
//! press of the row on this Mac's Machines view, which a terminal, an agent, `aterm ctl` or a
//! script over SSH cannot give. On a vault with no membership log yet, `remove` says an admin's
//! `aterm secrets revoke` is the way.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use aterm_secrets::SecretBytes;
use aterm_secrets::client::{self, Paths};
use aterm_secrets::commons::bootstrap::Device;
use aterm_secrets::members::join::{self, Dial, Heard, Stream};
use aterm_secrets::members::joiner::Joining;
use aterm_secrets::phrase::Phrase;

const USAGE: &str = "aterm members: Macs joining ALab and the members that let them in
  requests              the Macs asking that this Mac hears
  allow [--again] <name>
                        show a phrase for that Mac on this Mac's screen (--again: a Mac
                        the vault removed, which the run restores)
  invite                show a phrase for whichever Mac types it next
  ignore <name>         hide that Mac's request on this Mac
  list                  the members and the Macs the vault removed, as this Mac's log says
  remove <name> [--stolen]
                        take that Mac out of ALab as this Mac, rotating the fleet code
                        (--stolen: also deny-list its labels and node in the roster, and
                        name what to rotate at the providers)
  undo [<name>]         the Undo rows this Mac shows for <name>'s removals; a person's
                        press of the row on the Machines view signs this Mac's share
  join [<phrase>] [--member <name>] [--at <host:port>]... [--no-wait]
                        join ALab automatically; an explicit phrase selects a manual join
  status                whether this Mac is a member, waiting, or neither";

/// How long the joiner browses for members.
const BROWSE_WITHIN: Duration = Duration::from_secs(3);

/// Each member's dial and every exchange on it.
const DIAL_WITHIN: Duration = Duration::from_secs(5);

/// How long a phraseless `join` keeps dialing members to prove this Mac to ([`proved_back`]),
/// which tries them one after another (the fleet path in the first half of what is left): once
/// it passed, no further member is dialed and, where none let this Mac in and none may have
/// signed, the members are asked plainly, all at once ([`join::ask_all`]), while this Mac's vault
/// subscriber goes on proving with no such bound. It bounds the dials only: an exchange already
/// begun keeps its own bounds (a member's port within its 3 s handshake watchdog, a fleet
/// member's `VaultJoin` within the member's 30 s `SERVED_WITHIN`), and once a member let this Mac
/// in, its first sync from the vault's servers keeps the transport's. So install.sh's 60 s hang
/// detector (`VERIFY_WAIT_SECS`) can cut a slow run short; a Mac let in before that is in, and
/// its subscriber, which the join set up, follows the vault from there.
const PROVE_WITHIN: Duration = Duration::from_secs(20);

/// The port a member's fleet listener serves when `--at` names none.
const FLEET_PORT: u16 = 7100;

/// This Mac's network place in the words every reporter uses (`network: …`); none off Unix or
/// with no state directory.
fn network_header() -> Option<String> {
    #[cfg(unix)]
    {
        let state = aterm_types::dirs::state_dir()?;
        Some(aterm_netplace::words::header(
            &aterm_netplace::read(&state),
            &aterm_netplace::words::Words::now(),
        ))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Where this Mac is for its join's words ([`join::JoinPlace::of`] its network place under the
/// state root `state`). A unit test reads no Mac: there it is not known, and the tests that word a
/// place name it.
fn join_place(state: &Path) -> join::JoinPlace {
    #[cfg(not(test))]
    {
        join::JoinPlace::of(&aterm_netplace::read(state))
    }
    #[cfg(test)]
    {
        let _ = state;
        join::JoinPlace::Unknown
    }
}

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
            // The requests a member hears are the ones on its network: where this Mac is says
            // why none come from a phone hotspot. On stderr, so the reply stays the wire's.
            if let Some(header) = network_header() {
                eprintln!("{header}");
            }
            aterm_ctl::main_entry(vec!["members".into(), "requests".into()])
        }
        Some("allow" | "ignore") if args.len() == 2 && valid_window_target(&args[1]) => {
            to_window(&args)
        }
        Some("allow")
            if args.len() == 3 && args[1] == "--again" && valid_window_target(&args[2]) =>
        {
            to_window(&args)
        }
        Some("list") if args.len() == 1 => in_vault(list_said),
        Some("remove") => match RemoveArgs::parse(&args[1..]) {
            Ok(remove) => in_vault(|paths| remove_said(paths, &remove)),
            Err(why) => {
                eprintln!("aterm members remove: {why}");
                ExitCode::from(2)
            }
        },
        Some("undo") if args.len() <= 2 => {
            in_vault(|paths| undo_said(paths, args.get(1).map(String::as_str)))
        }
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

/// `remove`'s arguments.
#[derive(Debug, Default, PartialEq, Eq)]
struct RemoveArgs {
    who: String,
    stolen: bool,
}

impl RemoveArgs {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut out = Self::default();
        for arg in args {
            match arg.as_str() {
                "--stolen" => out.stolen = true,
                flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
                word if out.who.is_empty() => word.clone_into(&mut out.who),
                _ => return Err("name one Mac".to_owned()),
            }
        }
        if out.who.is_empty() {
            return Err("name the Mac to remove: its label, name or join code".to_owned());
        }
        Ok(out)
    }
}

/// Run one of F16's in-process verbs on this Mac's own vault files: its lines on stdout, or its
/// refusal on stderr and exit 1.
fn in_vault(verb: impl FnOnce(&Paths) -> Result<Vec<String>, String>) -> ExitCode {
    let said = Paths::live()
        .map_err(|error| error.to_string())
        .and_then(|paths| verb(&paths));
    match said {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("aterm members: {why}");
            ExitCode::from(1)
        }
    }
}

/// `list`'s lines: one per member, then one per Mac the log removed, then one per pair of
/// standing seats the log ties ([`client::Split::line`]: one Mac seated twice, or a Mac that asked
/// under another standing seat's label).
fn list_said(paths: &Paths) -> Result<Vec<String>, String> {
    let listed = client::member_list(paths)
        .map_err(|error| error.to_string())?
        .ok_or("this Mac's vault has no membership log yet")?;
    let splits = client::seat_splits(paths).map_err(|error| error.to_string())?;
    Ok(listed
        .iter()
        .map(|mac| match &mac.removed {
            Some(note) => format!(
                "{} {} code={} removed by={}{}",
                mac.label,
                mac.name,
                mac.code,
                note.by.join(","),
                if note.quorum { " quorum" } else { "" }
            ),
            None => format!(
                "{} {} code={}{}",
                mac.label,
                mac.name,
                mac.code,
                mac.admitted_by
                    .as_ref()
                    .map_or(" founder".to_owned(), |by| format!(" admitted by={by}"))
            ),
        })
        .chain(splits.iter().map(client::Split::line))
        .collect())
}

/// `remove`'s lines: what was removed and rotated, what the Mac could read, and on `--stolen`
/// the one `NEEDS YOU:` line.
fn remove_said(paths: &Paths, args: &RemoveArgs) -> Result<Vec<String>, String> {
    let removal =
        client::remove_member(paths, &args.who, args.stolen).map_err(|error| error.to_string())?;
    let mut lines = vec![format!(
        "removed {}: {} records, new keys for {}",
        removal.label,
        removal.signed,
        removal.rotated.join(", ")
    )];
    if !removal.could_read.is_empty() {
        lines.push(format!(
            "{} could read: {}",
            removal.label,
            removal.could_read.join(", ")
        ));
    }
    if removal.fleet_code_rotated {
        lines.push("rotated aterm-fleet-code".to_owned());
    }
    lines.extend(removal.envelope_notes);
    lines.extend(removal.needs_you);
    Ok(lines)
}

/// `undo [<name>]`'s lines: the Undo rows this Mac shows ([`client::undo_offers`]), `who`'s
/// alone where it names one. It signs nothing ([`undo_lines`]).
fn undo_said(paths: &Paths, who: Option<&str>) -> Result<Vec<String>, String> {
    let offers = client::undo_offers(paths).map_err(|error| error.to_string())?;
    Ok(undo_lines(&offers, who))
}

/// One line per Undo row (by its remover's label or join code where `who` names one), each
/// saying where its quorum stands and that a person's press of the row signs this Mac's share
/// (§3.2); a line saying so where no row is shown (F16's review: the verb signed a share from any
/// terminal, agent or SSH session, which the window refuses for a press it did not get).
fn undo_lines(offers: &[client::UndoOffer], who: Option<&str>) -> Vec<String> {
    let named = |offer: &&client::UndoOffer| {
        who.is_none_or(|word| {
            offer.label.eq_ignore_ascii_case(word) || offer.code.eq_ignore_ascii_case(word)
        })
    };
    let lines: Vec<String> = offers
        .iter()
        .filter(named)
        .map(|offer| {
            format!(
                "Undo {}'s removals ({} other Macs out with this one): {}; {} more shares undo \
                 them",
                offer.label,
                offer.others,
                if offer.signed {
                    "this Mac signed its share"
                } else {
                    "press the row on this Mac's Machines view to sign this Mac's share \
                     (a person's act, never a terminal's)"
                },
                offer.needed
            )
        })
        .collect();
    if lines.is_empty() {
        return vec![match who {
            Some(word) => format!("no removal by {word} keeps this Mac out with an Undo to sign"),
            None => "no removal keeps this Mac out with an Undo to sign".to_owned(),
        }];
    }
    lines
}

fn valid_window_target(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with('-')
        && !word.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn window_request(args: &[String]) -> Result<String, &'static str> {
    match args {
        [verb] if verb == "invite" => Ok("members invite\n".to_owned()),
        [verb, who] if matches!(verb.as_str(), "allow" | "ignore") && valid_window_target(who) => {
            Ok(format!("members {verb} {who}\n"))
        }
        [verb, flag, who] if verb == "allow" && flag == "--again" && valid_window_target(who) => {
            Ok(format!("members allow --again {who}\n"))
        }
        _ => Err(USAGE),
    }
}

/// A member's verb, sent to this Mac's running window, which answers one line. `allow` and
/// `invite` with no window (SSH alone, no login) refuse as §3.3.5 says.
fn to_window(args: &[String]) -> ExitCode {
    let Ok(request) = window_request(args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
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
    match aterm_ctl::front_door_send(&sock, &request) {
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
    /// The phrase given on the command line or by System State's field, wiped when dropped.
    phrase: Option<SecretBytes>,
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
                    let (host, port) =
                        aterm_net::fleet_member::parse_host_port(at, Some(FLEET_PORT))
                            .map_err(|why| format!("--at {at}: {why}"))?;
                    out.at.push(aterm_net::fleet_member::host_port(&host, port));
                }
                flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
                word => words.push(word),
            }
        }
        if !words.is_empty() {
            out.phrase = Some(SecretBytes::from(words.join(" ")));
        }
        if out.no_wait && out.phrase.is_some() {
            return Err("--no-wait asks and returns; give the phrase without it".to_owned());
        }
        Ok(out)
    }
}

/// The joining Mac: its paths, Commons device, node key, name and model, and where it is for its
/// join's words ([`join_place`], read once when the run starts).
struct Me {
    paths: Paths,
    device: Device,
    node: [u8; 32],
    name: String,
    model: String,
    place: join::JoinPlace,
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
            place: join_place(state),
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
    dial_within(at, DIAL_WITHIN)
}

/// [`dial`], its connect and handshake within `within`, every exchange after them within
/// [`DIAL_WITHIN`].
fn dial_within(at: &str, within: Duration) -> aterm_secrets::Result<Box<dyn Stream>> {
    let (host, port) = aterm_net::fleet_member::parse_host_port(at, None)
        .map_err(|_| aterm_secrets::Error::new(format!("{at} is not host:port")))?;
    let (transport, _) = aterm_net::fleet_member::dial_ask(&host, None, port, within)
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
    /// The first fetch has not configured a vault; the durable intent is retained.
    Bootstrapping(Box<Self>, String),
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
            Self::Bootstrapping(follows, _) => follows.fix(),
        }
    }
}

/// What a Mac a member let in says while it waits for the vault to record it: that finishes by
/// itself only while its vault subscriber runs (`follows`).
fn allowed_words(follows: &Follows) -> String {
    allowed_line(follows.fix().as_deref())
}

/// [`allowed_words`] where `fix` is what starts the vault subscriber, if one is needed.
fn allowed_line(fix: Option<&str>) -> String {
    match fix {
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
    /// Prove this Mac to the members at the addresses given, dialing none after the instant
    /// given ([`proved_back`]).
    prove: &'a (dyn Fn(&Me, &[String], std::time::Instant) -> Proved + Sync),
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
        prove: &|me, ats, until| proved_back(me, ats, Some(until)),
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
    match join_tried(me, args, reach, out, err) {
        Tried::Ended(code) => code,
        Tried::NoMember | Tried::Unsent => 1,
    }
}

/// How a `join` ended ([`join_tried`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tried {
    /// With this exit code.
    Ended(u8),
    /// Having reached no member on this network (none could be looked for, or none answered):
    /// nothing was asked of a phrase, so one given is not spent. Exit code 1.
    NoMember,
    /// Members answered, but the phrase never went to one ([`join::PhraseFailed::Unsent`]: none
    /// shows a phrase for this Mac, several do, or the one that does could not be reached for
    /// the run): one given is not spent. Exit code 1.
    Unsent,
}

/// [`join_as`], saying whether it reached no member ([`Tried::NoMember`]).
fn join_tried(
    me: &Me,
    args: &JoinArgs,
    reach: &Reach<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Tried {
    let membership = client::status(&me.paths).map(|status| status.membership);
    if matches!(membership, Ok(client::Membership::In { .. })) {
        let _ = writeln!(out, "This Mac is already in ALab as {}.", me.name);
        return Tried::Ended(0);
    }
    // A Mac a member let in waits for the vault to record it and asks no member again: the
    // member that let it in, and each it told, closed its row, so "press Allow" would send the
    // person to a Mac that shows nothing to Allow. It makes sure its subscriber follows the vault
    // and says where it stands, as `status` does.
    if matches!(membership, Ok(client::Membership::Pending)) && client::holds_allowed(&me.paths) {
        let _ = writeln!(out, "{}", allowed_words(&(reach.follow)(&me.paths.home)));
        return Tried::Ended(0);
    }
    let joining = me.joining();
    let dial = reach.dial;
    // A phraseless join proves this Mac only where sharing is on: a proof a member takes writes
    // this Mac's vault and ~/.secrets, which `[swarm] enabled = false` (or a settings file aterm
    // cannot read) keeps from happening by itself, as it keeps the vault half below
    // (`client::prepare_alab_join_retry`) and this Mac's subscriber. The plain ask still goes:
    // it writes nothing here, and only a phrase a person types lets this Mac in from it.
    let proves = args.phrase.is_none()
        && matches!(
            aterm_secrets::swarm::enabled_in_home(&me.paths.home),
            Ok(true)
        );
    // `--no-wait` sets this Mac up at the ALab vault while it proves this Mac (dialing no member
    // after [`PROVE_WITHIN`]) and then asks the members, each within its own bound, so
    // install.sh's lane ends within its own.
    let (asked, waits) = std::thread::scope(|scope| {
        let waits = (args.no_wait || args.phrase.is_none())
            .then(|| scope.spawn(|| wait_at_vault(&me.paths, reach)));
        let found = if args.at.is_empty() {
            (reach.browse)()
        } else {
            Ok(args.at.clone())
        };
        // With no phrase, prove this Mac first, as its subscriber does ([`ask_members`]): a
        // member on the home ALab LAN lets it in at once, with nobody's Allow, and it asks the
        // members plainly only where none did and none may have signed it in, or where the
        // proof's time ran out before any member signed.
        let asked = found.map(|ats| {
            let proved = if proves {
                (reach.prove)(me, &ats, std::time::Instant::now() + PROVE_WITHIN)
            } else {
                Proved::No
            };
            let (heard, missed) = match proved {
                Proved::No => join::ask_all(dial, &joining, &ats),
                Proved::In | Proved::SignedMaybe => (Vec::new(), Vec::new()),
            };
            (proved, heard, missed)
        });
        let waits = waits.map(|waiting| {
            waiting
                .join()
                .unwrap_or_else(|_| Err("the vault join stopped".to_owned()))
        });
        (asked, waits)
    });
    let (heard, missed) = match (asked, waits) {
        (Ok((Proved::In, ..)), waits) => return Tried::Ended(said_proved(PROVED_IN, waits, out)),
        (Ok((Proved::SignedMaybe, ..)), waits) => {
            return Tried::Ended(said_proved(join::let_in(me.place), waits, out));
        }
        (Ok((_, heard, missed)), None) => (heard, missed),
        (Ok((_, heard, missed)), Some(waits)) => {
            return Tried::Ended(said_no_wait(me.place, Ok(&heard), &missed, waits, out, err));
        }
        // A Mac that cannot browse (off macOS, or one macOS refuses the local network) still
        // waits at the vault, and says why it asked no member where install.sh shows it.
        (Err(error), Some(waits)) => {
            return Tried::Ended(said_no_wait(
                me.place,
                Err(error.to_string()),
                &[],
                waits,
                out,
                err,
            ));
        }
        (Err(error), None) => {
            let _ = writeln!(err, "aterm members join: {error}");
            return Tried::NoMember;
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
        return Tried::NoMember;
    }
    let phrase = match Phrase::parse(
        args.phrase
            .as_ref()
            .and_then(SecretBytes::as_str)
            .unwrap_or_default(),
    ) {
        Ok(phrase) => phrase,
        Err(error) => {
            let _ = writeln!(err, "aterm members join: {error}");
            return Tried::Ended(2);
        }
    };
    let ran = join::run_phrase(
        &me.paths,
        dial,
        &joining,
        &heard,
        &phrase,
        args.member.as_deref(),
        &mut |line| {
            let _ = writeln!(err, "aterm members join: {line}");
        },
    );
    match ran {
        Ok(joined) => {
            // The bundle set this Mac up for the vault; only its subscriber follows it from here.
            let follows = (reach.follow)(&me.paths.home);
            for line in joined_lines(&joined, &follows) {
                let _ = writeln!(out, "{line}");
            }
            Tried::Ended(0)
        }
        Err(error) => {
            let _ = writeln!(err, "aterm members join: {error}");
            if error.sent() {
                Tried::Ended(1)
            } else {
                Tried::Unsent
            }
        }
    }
}

/// Join the ALab vault without waiting, as System State's Secrets stage does: its configuration,
/// a Pending state and a queued request. Nothing is asked of a person: no password, and no
/// signer but the ALab vault's own, which this build pins.
fn join_alab_vault(paths: &Paths) -> aterm_secrets::Result<()> {
    client::join_if_unchanged(
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
        None,
    )
    .map(drop)
}

/// `join --no-wait`'s vault half, as System State's Secrets stage joins: with no vault set up
/// here, join the ALab vault without waiting, then make sure this Mac's vault subscriber runs.
/// While this Mac waits on the ALab vault, that subscriber proves this Mac to ALab's members
/// and asks them again ([`ask_members`]), about half a minute later, then after 1, 2, 4 and 8
/// minutes and then every 15 (`aterm_secrets::client::ask_members_if_due`), so a member that
/// was asleep, away or restarting its window when this ran lets it in, or shows the row, too.
/// Before the first fetch, a separate durable intent lets the subscriber retry setup even if
/// no source was authenticated yet. That intent is never membership. `Err` says why setup
/// could not be made retryable.
fn wait_at_vault(paths: &Paths, reach: &Reach<'_>) -> Result<Follows, String> {
    let unset = || !matches!(client::config(paths), Ok(Some(_)));
    if unset() {
        client::prepare_alab_join_retry(paths).map_err(|error| error.to_string())?;
        if unset()
            && let Err(error) = (reach.join_vault)(paths)
            && unset()
        {
            return Ok(Follows::Bootstrapping(
                Box::new((reach.follow)(&paths.home)),
                error.to_string(),
            ));
        }
    }
    Ok((reach.follow)(&paths.home))
}

/// What a phraseless `join` says where a member let this Mac in by its proof ([`Proved::In`]).
const PROVED_IN: &str = "Joined ALab: a member let this Mac in by its proof.";

/// What a phraseless `join` whose proof let this Mac in ([`PROVED_IN`]), or may have
/// ([`Proved::SignedMaybe`]: [`join::let_in`] where this Mac is, as every waiting line), says,
/// `first` first, and its exit code: 0. Its first line is the one install.sh shows; `waits` says
/// whether this Mac's subscriber, which follows the vault from here, runs.
fn said_proved(first: &str, waits: Option<Result<Follows, String>>, out: &mut dyn Write) -> u8 {
    let _ = writeln!(out, "{first}");
    if let Some(fix) = waits.as_ref().and_then(|waits| waits.as_ref().ok()?.fix()) {
        let _ = writeln!(out, "{fix}");
    }
    0
}

/// What `join --no-wait` says, and its exit code; its first line is the one install.sh shows.
/// `heard` answered (`Err`: why no member could be looked for), `missed` did not, and `waits`
/// says whether this Mac waits at the ALab vault and whether its subscriber, which asks the
/// members again through the same browse, runs. What lets it in follows where it is (`at`,
/// [`join::let_in`]): automatic admission only on the ALab LAN.
fn said_no_wait(
    at: join::JoinPlace,
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
        (_, Ok(Follows::Bootstrapping(follows, why))) => {
            let next = if *follows == Follows::Runs {
                "this Mac's vault subscriber retries setup automatically"
            } else {
                "setup is saved here for the vault subscriber"
            };
            let _ = writeln!(out, "ALab vault setup has not finished ({why}); {next}");
        }
        (None, _) => {
            let _ = writeln!(out, "{}", join::let_in(at));
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
                 them again (about half a minute later, then after 1, 2, 4 and 8 minutes, then \
                 every 15): {}",
                join::let_in(at)
            );
        }
        (Some(None), Ok(_)) => {
            let _ = writeln!(
                out,
                "no ALab member answers on this network yet: {}",
                join::let_in(at)
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

/// Ask a rotating batch of ALab members discovered on the LAN, as `join --no-wait` does, for the
/// Mac whose home is `home`: how many heard it. Repeated calls in this process cover larger
/// stable discovered sets. The Secrets stage of System State and `aterm init` asks
/// through it once its join waits on the ALab vault
/// (`aterm_machines::init_secrets::on_ask_members`), so a member shows a row with nobody
/// running `aterm members join` first, and that Mac's vault subscriber asks through it while it
/// waits, about half a minute later, then after 1, 2, 4 and 8 minutes and then every 15
/// (`aterm_secrets::client::on_ask_members`), so a member that wakes, joins the LAN or restarts
/// its window since shows the row too.
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
    ask_after_proof(proved_back(&me, &ats, None), || {
        let dial: &Dial<'_> = &dial;
        let (heard, _) = join::ask_all(dial, &me.joining(), &ats);
        if heard.is_empty() {
            return Err("no ALab member answers on this network".to_owned());
        }
        Ok(heard.len())
    })
}

/// The fleet path's share of a proof that must dial no member past `until` ([`proved_back`]):
/// half the time left, so the home-LAN path that follows keeps the rest.
fn fleet_share(until: std::time::Instant) -> std::time::Instant {
    let now = std::time::Instant::now();
    now + until.saturating_duration_since(now) / 2
}

/// What proving this Mac to the members came to ([`proved_back`]).
#[derive(Debug)]
enum Proved {
    /// A member let it in.
    In,
    /// The last member tried may have signed this Mac's records (its possession answer went), or
    /// another proof of this Mac still runs ([`join::ProofLock`]): nothing more is sent this
    /// round, a plain ask included, until those records reached the others.
    SignedMaybe,
    /// No member let it in, and none may have signed.
    No,
}

/// What one try of a path ([`join::prove_first`]) came to, as [`Proved`].
fn proof_outcome(tried: Result<join::Joined, join::Missed>) -> Proved {
    match tried {
        Ok(_) => Proved::In,
        Err(missed) if missed.signed_maybe => Proved::SignedMaybe,
        Err(_) => Proved::No,
    }
}

/// After a proof, `ask` the members plainly only where none let this Mac in and none may have
/// signed it in: a Mac a member may have signed in already shows nobody a "New Mac" entry.
fn ask_after_proof(
    proved: Proved,
    ask: impl FnOnce() -> Result<usize, String>,
) -> Result<usize, String> {
    match proved {
        Proved::In | Proved::SignedMaybe => Ok(1),
        Proved::No => ask(),
    }
}

/// Before any person is asked, prove this Mac to the members with nobody's Allow
/// (`aterm_secrets::client::readmit`): a Mac that is ALab's already but whose vault key changed
/// gets back in by itself. Over the fleet first, where this Mac holds the fleet code and a book
/// ([`crate::vault_join::prove_over_fleet`]: the channel proves the fleet's key too, so a node key
/// no seat binds gets a seat), then on each member's own port at `ats` (its node key alone, which
/// moves the seat the log binds to it). One path after the other, and none after a try that may
/// have signed ([`Proved::SignedMaybe`]). No member is dialed on either path once `until` has
/// passed ([`crate::vault_join::dial_until`]; `None`: none is too late), and the fleet path dials
/// none past half the time left, so dead fleet peers never take the home-LAN proof's turn. One
/// proof of this Mac runs at a time across processes ([`join::ProofLock`]): while another holds
/// it (`aterm members join` beside the subscriber's round) this one waits until `until`, then
/// sends nothing, as after a member that may have signed. [`Proved::No`] where no member let it
/// in: this Mac then asks a person as before.
fn proved_back(me: &Me, ats: &[String], until: Option<std::time::Instant>) -> Proved {
    use aterm_net::fleet_member::{read_join_code, read_node_identity, read_peers};
    let Ok(lock) = join::ProofLock::take(&me.paths, until) else {
        return Proved::No;
    };
    let Some(_proving) = lock else {
        return Proved::SignedMaybe;
    };
    let Some(state) = aterm_types::dirs::state_dir() else {
        return Proved::No;
    };
    let Some(node) = read_node_identity(&state).ok().flatten() else {
        return Proved::No;
    };
    if *node.key().bytes() != me.node {
        return Proved::No;
    }
    let joining = me.joining();
    if let Ok(code) = read_join_code(&state)
        && let Ok(secret) = aterm_net::fleet::FleetSecret::new(&code.secret)
    {
        let peers: Vec<_> = read_peers(&state)
            .unwrap_or_default()
            .into_iter()
            .filter(|peer| peer.node.is_some())
            .map(|peer| (peer.host, peer.addr, peer.port))
            .collect();
        if !peers.is_empty() {
            // The next try comes after the subscriber's pause, by when a member that may have
            // signed has posted its records to the servers.
            match proof_outcome(crate::vault_join::prove_over_fleet(
                &me.paths,
                &joining,
                (&secret, &node),
                &peers,
                until.map(fleet_share),
            )) {
                Proved::No => {}
                done => return done,
            }
        }
    }
    let sign_node = |message: &[u8]| -> [u8; 64] {
        node.sign_record(aterm_secrets::members::desk::PROVE_RECORD, message)
    };
    let dial = crate::vault_join::dial_until(until, DIAL_WITHIN, &dial_within);
    let dial: &Dial<'_> = &dial;
    proof_outcome(join::prove_first(
        &me.paths, dial, &joining, &sign_node, ats,
    ))
}

/// What a try with the phrase came to ([`join_with_phrase`]), each with what the join said, for
/// the log (never the page).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhraseTry {
    /// It let this Mac in, or this Mac was in or let in already.
    Joined(Vec<String>),
    /// It reached a member and let nobody in, it is not three of the list's words, or this Mac
    /// cannot join: a phrase is good for one try, so the member's person presses Allow again.
    Refused(String),
    /// The phrase never went to a member: no ALab Mac answered on this network (one asleep, on
    /// another network, or macOS's Local Network question still unanswered), none could be
    /// looked for, none that answered shows a phrase for this Mac (the one that showed it is
    /// asleep or elsewhere) or several do, the one that shows it could not be reached for the
    /// run, or this Mac could not start one. Nothing was asked of the phrase, so the same words
    /// let this Mac in on a later try.
    NotReached(String),
}

/// Join ALab with the phrase a member shows once its person pressed Allow, as
/// `aterm members join <phrase>` does, with no terminal: System State's Secrets vault row takes
/// the phrase in its own field, and the window runs this on a worker
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.6 step 5). The phrase stays in [`SecretBytes`],
/// wiped when dropped. What the try came to ([`PhraseTry`]).
#[must_use]
pub fn join_with_phrase(phrase: &SecretBytes) -> PhraseTry {
    // A Mac that cannot name itself sent the phrase nowhere: the words stay good.
    let me = match Me::live() {
        Ok(me) => me,
        Err(why) => return PhraseTry::NotReached(why),
    };
    let reach = Reach {
        browse: &|| aterm_secrets::discover::browse_members(BROWSE_WITHIN),
        dial: &dial,
        prove: &|me, ats, until| proved_back(me, ats, Some(until)),
        join_vault: &join_alab_vault,
        follow: &Follows::live,
    };
    phrase_join_as(&me, phrase, &reach)
}

/// [`join_with_phrase`] as `me`, through `reach`: `join` with the phrase given, its lines on
/// stdout when it went (a Mac already in, or let in already, says so), else its lines on stderr.
/// A phrase that is not three of the list's words is refused before any member is looked for.
fn phrase_join_as(me: &Me, phrase: &SecretBytes, reach: &Reach<'_>) -> PhraseTry {
    let Some(text) = phrase.as_str() else {
        return PhraseTry::Refused("the phrase is not text".to_owned());
    };
    if let Err(error) = Phrase::parse(text) {
        return PhraseTry::Refused(format!("aterm members join: {error}"));
    }
    let args = JoinArgs {
        phrase: Some(SecretBytes::from(text.trim().to_owned())),
        ..JoinArgs::default()
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let tried = join_tried(me, &args, reach, &mut out, &mut err);
    let lines = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    match tried {
        Tried::Ended(0) => PhraseTry::Joined(lines(&out)),
        Tried::Ended(_) => PhraseTry::Refused(lines(&err).join("; ")),
        Tried::NoMember | Tried::Unsent => PhraseTry::NotReached(lines(&err).join("; ")),
    }
}

/// `status`: a member; allowed, with `~/.secrets` written, until the vault records it; asking;
/// or not in ALab.
fn status() -> ExitCode {
    let Ok(paths) = Paths::live() else {
        eprintln!("aterm members: no home directory");
        return ExitCode::from(1);
    };
    println!("{}", Standing::live(&paths).line());
    ExitCode::SUCCESS
}

/// `status`'s line for the Mac at `paths`, `follows` reading whether its vault subscriber runs
/// and `place` where it is, which says what lets an asking Mac in ([`join::let_in`]).
#[cfg(test)]
fn status_said(
    paths: &Paths,
    follows: &dyn Fn(&Path) -> Follows,
    place: &dyn Fn() -> join::JoinPlace,
) -> String {
    Standing::read(paths, follows, place).line()
}

/// Where this Mac stands in ALab, as `aterm members status` reads it from local files alone
/// (never a dial), typed so `aterm status` can say who acts next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// A member, under this machine name.
    Member(String),
    /// A member let it in and `~/.secrets` is written; the vault records it once this Mac's
    /// vault subscriber follows the vault.
    Allowed {
        /// What starts the subscriber, where none runs ([`Follows::fix`]).
        fix: Option<String>,
        /// A person turned Sync off on this Mac, which stands until a person turns it on.
        held: bool,
    },
    /// Asking to be let in from this place ([`join::let_in`] says what lets it in there).
    Asking(join::JoinPlace),
    /// Neither a member nor asking.
    NotIn,
    /// The vault's local state could not be read: why.
    Unknown(String),
}

impl Standing {
    /// This Mac's standing over `paths`, `follows` reading whether its vault subscriber runs and
    /// `place` where it is.
    fn read(
        paths: &Paths,
        follows: &dyn Fn(&Path) -> Follows,
        place: &dyn Fn() -> join::JoinPlace,
    ) -> Self {
        match client::status(paths).map(|status| status.membership) {
            Ok(client::Membership::In { .. }) => Self::Member(
                client::config(paths)
                    .ok()
                    .flatten()
                    .map_or_else(client::default_host_name, |config| config.host),
            ),
            Ok(client::Membership::Pending) if client::holds_allowed(paths) => {
                let follows = follows(&paths.home);
                Self::Allowed {
                    fix: follows.fix(),
                    held: matches!(follows, Follows::Off),
                }
            }
            Ok(client::Membership::Pending) => Self::Asking(place()),
            Ok(_) => Self::NotIn,
            Err(error) => Self::Unknown(error.to_string()),
        }
    }

    /// This Mac's standing, read only: its vault subscriber's state as installed (never
    /// repaired) and its network place as last recorded.
    #[must_use]
    pub fn live(paths: &Paths) -> Self {
        Self::read(paths, &Follows::now, &|| {
            aterm_types::dirs::state_dir()
                .map_or(join::JoinPlace::Unknown, |state| join_place(&state))
        })
    }

    /// `aterm members status`'s line.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Member(name) => format!("member as {name}"),
            Self::Allowed { fix, .. } => allowed_line(fix.as_deref()),
            Self::Asking(place) => format!("asking: {}", join::let_in(*place)),
            Self::NotIn => "not in ALab: run `aterm members join` here".to_owned(),
            Self::Unknown(error) => format!("unknown: {error}"),
        }
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
            place: join::JoinPlace::Home,
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
                prove: &|_, _, _| panic!("a phrase join proves nothing"),
                join_vault: &no_vault,
                follow,
            };
            let args = JoinArgs {
                phrase: Some(SecretBytes::from(words)),
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
        let absent = status_said(&j.paths, &|_| Follows::Absent, &|| join::JoinPlace::Home);
        assert!(absent.contains("aterm secrets agent on"), "{absent}");
        assert!(!absent.contains("finishes by itself"), "{absent}");
        let runs = status_said(&j.paths, &|_| Follows::Runs, &|| join::JoinPlace::Home);
        assert!(runs.contains("finishes by itself"), "{runs}");
    }

    /// System State's phrase field joins as `aterm members join <phrase>` does, with no
    /// terminal: the phrase the member shows lets this Mac in, and one it does not show is
    /// refused with why, which the window logs.
    ///
    /// NEGATIVE CONTROL: have `phrase_join_as` send `join_as` its phrase's first word alone:
    /// the phrase the member shows lets nobody in.
    #[test]
    fn the_windows_phrase_join_lets_the_mac_in_or_says_why_not() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let m1 = member(root.path(), "m1");
        let j = me(root.path(), "j");
        let (wrong, _) = with_member(&m1, |dial| {
            let ats = [M1.to_owned()];
            let (heard, _) = join::ask_all(dial, &j.joining(), &ats);
            assert_eq!(heard.len(), 1);
            let shown = allow(&m1);
            // A phrase of the list's words that the member does not show.
            let other =
                std::iter::repeat_with(|| Phrase::generate().expect("a phrase").words().join(" "))
                    .find(|words| *words != shown)
                    .expect("a phrase the member does not show");
            let reach = Reach {
                browse: &|| Ok(ats.to_vec()),
                dial,
                prove: &|_, _, _| panic!("a phrase join proves nothing"),
                join_vault: &no_vault,
                follow: &|_| Follows::Runs,
            };
            phrase_join_as(&j, &SecretBytes::from(other), &reach)
        });
        let PhraseTry::Refused(why) = wrong else {
            panic!("a phrase the member does not show lets nobody in: {wrong:?}");
        };
        assert!(!why.is_empty());
        assert!(client::config(&j.paths).expect("config").is_none());
        let (right, _) = with_member(&m1, |dial| {
            let ats = [M1.to_owned()];
            let _ = join::ask_all(dial, &j.joining(), &ats);
            let shown = allow(&m1);
            let reach = Reach {
                browse: &|| Ok(ats.to_vec()),
                dial,
                prove: &|_, _, _| panic!("a phrase join proves nothing"),
                join_vault: &no_vault,
                follow: &|_| Follows::Runs,
            };
            phrase_join_as(&j, &SecretBytes::from(format!("  {shown}\n")), &reach)
        });
        let PhraseTry::Joined(said) = right else {
            panic!("the phrase the member shows lets this Mac in: {right:?}");
        };
        assert!(
            client::config(&j.paths).expect("config").is_some(),
            "{said:?}"
        );
    }

    /// A try with the phrase that reached no member (none answered on this network: the
    /// member's Mac asleep or waking, macOS's Local Network question still unanswered) asked
    /// nothing of the phrase, so it is not a refused one: the window keeps the words, and the
    /// same phrase lets this Mac in on the next try. A phrase that is not three of the list's
    /// words is refused before any member is looked for.
    ///
    /// NEGATIVE CONTROL: map `Tried::NoMember` to `PhraseTry::Refused` in `phrase_join_as`: the
    /// first try reads as a spent phrase and the window clears the words it still holds good.
    #[test]
    fn a_phrase_try_that_reached_no_member_is_not_a_refused_phrase() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let m1 = member(root.path(), "m1");
        let j = me(root.path(), "j");
        let ((first, second), _) = with_member(&m1, |dial| {
            let ats = [M1.to_owned()];
            let _ = join::ask_all(dial, &j.joining(), &ats);
            let shown = SecretBytes::from(allow(&m1));
            let nobody = Reach {
                browse: &|| Ok(Vec::new()),
                dial,
                prove: &|_, _, _| panic!("a phrase join proves nothing"),
                join_vault: &no_vault,
                follow: &|_| Follows::Runs,
            };
            let first = phrase_join_as(&j, &shown, &nobody);
            let reach = Reach {
                browse: &|| Ok(ats.to_vec()),
                dial,
                prove: &|_, _, _| panic!("a phrase join proves nothing"),
                join_vault: &no_vault,
                follow: &|_| Follows::Runs,
            };
            (first, phrase_join_as(&j, &shown, &reach))
        });
        assert!(
            matches!(first, PhraseTry::NotReached(ref why) if why.contains("no ALab member")),
            "a try that reached no member spent no phrase: {first:?}"
        );
        assert!(
            matches!(second, PhraseTry::Joined(_)),
            "the same phrase lets the Mac in on the next try: {second:?}"
        );
        let unbrowsable = Reach {
            browse: &|| Err(aterm_secrets::Error::new("macOS refused the local network")),
            dial: &|_| panic!("no member is dialed"),
            prove: &|_, _, _| panic!("a phrase join proves nothing"),
            join_vault: &no_vault,
            follow: &|_| Follows::Runs,
        };
        let other = me(root.path(), "k");
        let phrase = SecretBytes::from(Phrase::generate().expect("a phrase").words().join(" "));
        assert!(
            matches!(
                phrase_join_as(&other, &phrase, &unbrowsable),
                PhraseTry::NotReached(_)
            ),
            "no member could be looked for: not reached"
        );
        assert!(
            matches!(
                phrase_join_as(
                    &other,
                    &SecretBytes::from("not a phrase".to_owned()),
                    &unbrowsable
                ),
                PhraseTry::Refused(_)
            ),
            "not three of the list's words: refused before any member is looked for"
        );
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
                    prove: &|_, _, _| panic!("a Mac let in proves nothing again"),
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

    /// What `join --no-wait` says lets this Mac in follows where it is: its first line ends with
    /// the words for that place (`join::let_in`: automatic admission on the ALab LAN only), and
    /// the exit code is 0 everywhere: being away is words, not a failure.
    /// NEGATIVE CONTROL: `said_no_wait` printing `join::LET_IN` whatever `at` says gives a Mac off
    /// the LAN the ALab LAN's words, and this test fails at the first of them, "Proves".
    #[test]
    fn join_no_wait_says_what_lets_this_mac_in_where_it_is() {
        for at in [
            join::JoinPlace::Home,
            join::JoinPlace::Proves,
            join::JoinPlace::Away,
            join::JoinPlace::Unknown,
        ] {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = said_no_wait(at, Ok(&[]), &[], Ok(Follows::Runs), &mut out, &mut err);
            let said = String::from_utf8(out).unwrap();
            assert_eq!(code, 0, "{at:?}: {said}");
            let first = said.lines().next().unwrap_or_default();
            assert!(first.ends_with(join::let_in(at)), "{at:?}: {said}");
            assert_eq!(
                first.ends_with(join::LET_IN),
                at == join::JoinPlace::Home,
                "{at:?}: {said}"
            );
        }
    }

    /// A first fetch failure must leave a subscriber able to retry, without claiming membership.
    #[test]
    fn a_first_vault_fetch_failure_keeps_headless_bootstrap_running() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths::for_home(root.path());
        let followed = std::sync::atomic::AtomicBool::new(false);
        let follow = |_: &Path| {
            followed.store(true, std::sync::atomic::Ordering::Relaxed);
            Follows::Runs
        };
        let reach = Reach {
            browse: &|| Ok(Vec::new()),
            dial: &|_| panic!("no member dial"),
            prove: &|_, _, _| panic!("the vault half proves nothing"),
            join_vault: &|_| Err(aterm_secrets::Error::new("first fetch unavailable")),
            follow: &follow,
        };
        let waiting = wait_at_vault(&paths, &reach);
        assert!(waiting.is_ok());
        assert!(followed.load(std::sync::atomic::Ordering::Relaxed));
        assert!(client::config(&paths).unwrap().is_none());
        assert_eq!(
            client::status(&paths).unwrap().membership,
            client::Membership::Unconfigured
        );
        assert!(!paths.home.join(".secrets").exists());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            said_no_wait(
                join::JoinPlace::Home,
                Ok(&[]),
                &[],
                waiting,
                &mut out,
                &mut err
            ),
            0
        );
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("retries setup automatically"), "{out}");
        assert!(
            !out.contains(join::LET_IN),
            "no membership or queued vault request yet: {out}"
        );
    }

    #[test]
    fn a_headless_bootstrap_keeps_sharing_off_before_fetch_or_follow() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        std::fs::create_dir_all(root.path().join(".config/aterm")).unwrap();
        std::fs::write(
            root.path().join(".config/aterm/aterm.toml"),
            "[swarm]\nenabled = false\n",
        )
        .unwrap();
        let reach = Reach {
            browse: &|| panic!("no browse"),
            dial: &|_| panic!("no dial"),
            prove: &|_, _, _| panic!("the vault half proves nothing"),
            join_vault: &|_| panic!("sharing off: no fetch"),
            follow: &|_| panic!("sharing off: no follower"),
        };
        assert!(wait_at_vault(&paths, &reach).is_err());
        assert!(client::config(&paths).unwrap().is_none());
    }

    /// install.sh's windowless `members join --no-wait` sets the Mac up at the ALab vault (its
    /// configuration, Pending, a queued request) and starts its vault subscriber, as System
    /// State's Secrets stage does, whether or not a member answers. That subscriber asks ALab's
    /// members again (about half a minute later, then after 1, 2, 4 and 8 minutes, then every
    /// 15), so a
    /// member asleep, away or
    /// restarting its window when the lane asked shows the row too; and with no member
    /// answering, the lane says so rather than nothing.
    ///
    /// NEGATIVE CONTROL: never start `wait_at_vault` (`waits` always `None`): neither Mac joins
    /// the vault or gets a subscriber, and the one no member answers fails with nothing on
    /// stdout, the line install.sh shows.
    #[test]
    fn default_and_install_joins_wait_at_the_vault_without_a_phrase_prompt() {
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
                    prove: &|_, _, _| Proved::No,
                    join_vault: &join_vault,
                    follow: &follow,
                };
                let args = JoinArgs {
                    no_wait: name == "k",
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
                assert!(
                    first.contains(
                        "asks them again (about half a minute later, then after 1, 2, 4 and 8 \
                         minutes, then every 15)"
                    ),
                    "{said}"
                );
            }
        }
        let rows = m1.lock().unwrap().rows(aterm_time::Instant::now());
        assert_eq!(rows.len(), 1, "j's request waits on m1");
    }

    /// A phraseless `join` (install.sh's `members join --no-wait` included) proves this Mac to
    /// the members it found before it asks any plainly, as its subscriber does: where a member
    /// let it in by its proof, or may have signed it in, no member is asked and no row waits for
    /// a person's Allow; only where none did is each asked. The vault half runs either way, and
    /// a line that waits says what lets this Mac in where it is (`join::let_in`), on the ALab
    /// LAN and off it alike.
    ///
    /// The proof dials no member after [`PROVE_WITHIN`] from its start, so install.sh's lane
    /// ends within its own bound.
    ///
    /// NEGATIVE CONTROLS: call `join::ask_all` whatever the proof came to: m1 shows a row for
    /// the Mac a member let in ("j"). Prove before a phrase join too: the phrase tests' `prove`
    /// panics ("a phrase join proves nothing"). Give the proof no deadline, or a later one: the
    /// `until` assertion fails. Print `join::LET_IN` where a member may have signed this Mac in,
    /// whatever its place: the Mac off the LAN that holds the fleet's book ("k-proves") is told
    /// it waits for automatic admission, and its first line fails.
    #[test]
    fn a_phraseless_join_proves_first_and_asks_only_where_no_member_let_it_in() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        for ((place, at), (name, outcome, first, asks)) in [
            (join::JoinPlace::Home, "home"),
            (join::JoinPlace::Proves, "proves"),
        ]
        .into_iter()
        .flat_map(|(place, at)| {
            [
                ("j", Proved::In, PROVED_IN, false),
                ("k", Proved::SignedMaybe, join::let_in(place), false),
                ("l", Proved::No, join::let_in(place), true),
            ]
            .map(|case| ((place, at), case))
        }) {
            // A machine name (lower case, `aterm_secrets::text::valid_machine_name`): the place's
            // word after the case's.
            let name = format!("{name}-{at}");
            let m1 = member(root.path(), &format!("m1-{name}"));
            let mac = Me {
                place,
                ..me(root.path(), &name)
            };
            let proved_at: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
            let outcome = Mutex::new(Some(outcome));
            let before = std::time::Instant::now();
            let prove = |_: &Me, ats: &[String], until: std::time::Instant| {
                assert!(
                    until >= before + PROVE_WITHIN
                        && until <= std::time::Instant::now() + PROVE_WITHIN,
                    "the proof dials no member after PROVE_WITHIN from its start"
                );
                proved_at.lock().unwrap().push(ats.to_vec());
                outcome.lock().unwrap().take().expect("one proof per join")
            };
            let ((code, said), dials) = with_member(&m1, |dial| {
                let reach = Reach {
                    browse: &|| Ok(vec![M1.to_owned()]),
                    dial,
                    prove: &prove,
                    join_vault: &|_| Ok(()),
                    follow: &|_| Follows::Runs,
                };
                let args = JoinArgs {
                    no_wait: true,
                    ..JoinArgs::default()
                };
                joined(&mac, &args, &reach)
            });
            assert_eq!(code, 0, "{name}: {said}");
            assert_eq!(
                *proved_at.lock().unwrap(),
                [vec![M1.to_owned()]],
                "{name} proves itself to the member it found first: {said}"
            );
            assert_eq!(said.lines().next(), Some(first), "{name}: {said}");
            let rows = m1.lock().unwrap().rows(aterm_time::Instant::now());
            assert_eq!(rows.len(), usize::from(asks), "{name}: {said}");
            assert_eq!(dials, usize::from(asks), "{name}: {said}");
        }
    }

    /// A phraseless `join` on a Mac whose person turned sharing off (`[swarm] enabled = false`)
    /// proves this Mac to no member, so no member's proof writes its vault or ~/.secrets, as the
    /// vault half and the subscriber keep that switch too; it still asks the members plainly,
    /// which writes nothing here.
    ///
    /// NEGATIVE CONTROL: prove whatever `[swarm]` says (`proves` without its `enabled_in_home`
    /// read): `prove` panics.
    #[test]
    fn a_phraseless_join_with_sharing_off_proves_nothing_and_writes_nothing() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let m1 = member(root.path(), "m1");
        let mac = me(root.path(), "j");
        let config = mac.paths.home.join(".config/aterm");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("aterm.toml"), "[swarm]\nenabled = false\n").unwrap();
        let ((code, said), dials) = with_member(&m1, |dial| {
            let reach = Reach {
                browse: &|| Ok(vec![M1.to_owned()]),
                dial,
                prove: &|_, _, _| panic!("sharing off: no proof"),
                join_vault: &|_| panic!("sharing off: no fetch"),
                follow: &|_| panic!("sharing off: no follower"),
            };
            let args = JoinArgs {
                no_wait: true,
                ..JoinArgs::default()
            };
            joined(&mac, &args, &reach)
        });
        assert_eq!(code, 0, "{said}");
        assert_eq!(dials, 1, "the plain ask still goes: {said}");
        let rows = m1.lock().unwrap().rows(aterm_time::Instant::now());
        assert_eq!(rows.len(), 1, "{said}");
        assert!(client::config(&mac.paths).unwrap().is_none(), "{said}");
        assert!(!mac.paths.home.join(".secrets").exists(), "{said}");
    }

    /// A proof whose time ran out ([`crate::vault_join::dial_until`], as [`proved_back`] dials
    /// by) dials no further member and reads as one no member let in ([`Proved::No`], never
    /// [`Proved::SignedMaybe`]: nothing went to that member), so the plain ask follows; before
    /// then each member is dialed within [`DIAL_WITHIN`] and what is left, and with no deadline
    /// within [`DIAL_WITHIN`].
    ///
    /// NEGATIVE CONTROL: let `dial_until` dial past `until`: the members are dialed after the
    /// deadline, and the first assertion fails.
    #[test]
    fn a_proof_whose_time_ran_out_dials_no_further_member_and_leaves_the_plain_ask() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let mac = me(root.path(), "j");
        let ats = ["m2:7100".to_owned(), M1.to_owned()];
        let sign_node = |_: &[u8]| [0u8; 64];
        let dialed: Mutex<Vec<(String, Duration)>> = Mutex::new(Vec::new());
        let reach = |at: &str, within: Duration| -> aterm_secrets::Result<Box<dyn Stream>> {
            dialed.lock().unwrap().push((at.to_owned(), within));
            Err(aterm_secrets::Error::new("connection refused"))
        };
        let proved = |until: Option<std::time::Instant>| {
            dialed.lock().unwrap().clear();
            let dial = crate::vault_join::dial_until(until, DIAL_WITHIN, &reach);
            let tried = join::prove_first(&mac.paths, &dial, &mac.joining(), &sign_node, &ats);
            (proof_outcome(tried), dialed.lock().unwrap().clone())
        };

        let (outcome, dialed_then) = proved(Some(std::time::Instant::now()));
        assert!(dialed_then.is_empty(), "{dialed_then:?}");
        assert!(matches!(outcome, Proved::No), "{outcome:?}");

        let (outcome, dialed_then) =
            proved(Some(std::time::Instant::now() + Duration::from_secs(60)));
        assert!(matches!(outcome, Proved::No), "{outcome:?}");
        assert_eq!(
            dialed_then
                .iter()
                .map(|(at, _)| at.as_str())
                .collect::<Vec<_>>(),
            [M1, "m2:7100"],
            "one after another, in address order"
        );
        assert!(dialed_then.iter().all(|(_, within)| *within <= DIAL_WITHIN));

        let (_, dialed_then) = proved(None);
        assert!(
            dialed_then.iter().all(|(_, within)| *within == DIAL_WITHIN),
            "{dialed_then:?}"
        );
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
            prove: &|_, _, _| panic!("with no browse there is no member to prove to"),
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
                .as_ref()
                .and_then(SecretBytes::as_str),
            Some("maple river ocean")
        );
        assert_eq!(
            parse(&["maple river ocean", "--member", "m7"]).unwrap(),
            JoinArgs {
                phrase: Some(SecretBytes::from("maple river ocean".to_owned())),
                member: Some("m7".into()),
                ..JoinArgs::default()
            }
        );
        assert_eq!(
            parse(&[
                "--no-wait",
                "--at",
                "m22.local",
                "--at",
                "10.0.0.2:7200",
                "--at",
                "::1",
                "--at",
                "[::1]:7200",
            ])
            .unwrap(),
            JoinArgs {
                no_wait: true,
                at: vec![
                    "m22.local:7100".into(),
                    "10.0.0.2:7200".into(),
                    "[::1]:7100".into(),
                    "[::1]:7200".into(),
                ],
                ..JoinArgs::default()
            }
        );
        assert!(parse(&["--at", "m22.local:0"]).is_err());
        assert!(parse(&["--at", "m22.local:abc"]).is_err());
        assert!(dial("127.0.0.1:+7100").is_err());
        assert!(dial("127.0.0.1:0").is_err());
        assert!(dial("[not-v6]:7100").is_err());
        assert!(parse(&["--no-wait", "maple", "river", "ocean"]).is_err());
        assert!(parse(&["--member"]).is_err());
        assert!(parse(&["--wait"]).is_err());
    }

    /// `members undo` signs nothing: it lists the Undo rows and says a person's press of one
    /// signs this Mac's share; on a Mac no removal keeps out it says so rather than refusing as
    /// a signing verb would (F16's review).
    ///
    /// NEGATIVE CONTROL: have `undo_said` call `client::undo_removals(paths, who)` as before:
    /// the Mac with no vault refuses ("…no membership log…") and the first assert fails.
    #[test]
    fn members_undo_lists_the_rows_and_signs_nothing() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let (paths, _device) = mac(root.path(), "m9");
        assert_eq!(
            undo_said(&paths, Some("m7")),
            Ok(vec![
                "no removal by m7 keeps this Mac out with an Undo to sign".to_owned()
            ])
        );
        let offer = |label: &str, signed: bool| client::UndoOffer {
            remover: [7; 48],
            label: label.to_owned(),
            code: format!("{label}-code"),
            others: 4,
            signed,
            needed: 3,
        };
        let offers = [offer("m7", false), offer("m12", true)];
        let lines = undo_lines(&offers, None);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].starts_with("Undo m7's removals (4 other Macs"),
            "{lines:?}"
        );
        assert!(
            lines[0].contains("press the row on this Mac's Machines view"),
            "{lines:?}"
        );
        assert!(lines[1].contains("this Mac signed its share"), "{lines:?}");
        assert_eq!(undo_lines(&offers, Some("M12-CODE")).len(), 1);
    }

    /// `members remove` names one Mac, with `--stolen` anywhere; no Mac, two, or another flag
    /// is refused before anything is read.
    #[test]
    fn remove_names_one_mac() {
        let parse = |args: &[&str]| {
            RemoveArgs::parse(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
        };
        assert_eq!(
            parse(&["m12"]),
            Ok(RemoveArgs {
                who: "m12".into(),
                stolen: false
            })
        );
        assert_eq!(
            parse(&["--stolen", "m12"]),
            Ok(RemoveArgs {
                who: "m12".into(),
                stolen: true
            })
        );
        assert!(parse(&[]).is_err());
        assert!(parse(&["m12", "m13"]).is_err());
        assert!(parse(&["m12", "--force"]).is_err());

        let req = |args: &[&str]| {
            window_request(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
        };
        assert_eq!(req(&["invite"]), Ok("members invite\n".to_owned()));
        assert_eq!(req(&["allow", "m12"]), Ok("members allow m12\n".to_owned()));
        assert_eq!(
            req(&["allow", "--again", "m12"]),
            Ok("members allow --again m12\n".to_owned())
        );
        assert_eq!(
            req(&["ignore", "m12"]),
            Ok("members ignore m12\n".to_owned())
        );
        for injected in [
            "",
            "-h",
            "--again",
            "--again victim",
            "m12\n@s-1 send bad",
            "m12\r@s-1 send bad",
            "m12 extra",
        ] {
            assert!(req(&["allow", injected]).is_err(), "{injected:?}");
            assert!(req(&["ignore", injected]).is_err(), "{injected:?}");
            assert!(
                req(&["allow", "--again", injected]).is_err(),
                "{injected:?}"
            );
            assert_eq!(
                main_entry(vec!["allow".into(), injected.into()]),
                ExitCode::from(2),
                "{injected:?}"
            );
        }
    }

    /// A typed phrase that no answering member shows (the member that showed it is asleep or on
    /// another network, while another member of ALab answers here, its person having pressed no
    /// Allow for this Mac) went to no member, so it is not spent: the page keeps the words and
    /// says the phrase is still good, never that it let nobody in.
    ///
    /// NEGATIVE CONTROL: map `Tried::Unsent` to `PhraseTry::Refused` in `phrase_join_as`.
    #[test]
    fn a_phrase_no_answering_member_shows_is_not_refused() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        // m2 answers here, but its person pressed no Allow for this Mac.
        let m2 = member(root.path(), "m2");
        let j = me(root.path(), "j");
        // The words another member (asleep now) showed, which the person typed.
        let typed = SecretBytes::from(Phrase::generate().expect("a phrase").words().join(" "));
        let (tried, _) = with_member(&m2, |dial| {
            let reach = Reach {
                browse: &|| Ok(vec![M1.to_owned()]),
                dial,
                prove: &|_, _, _| panic!("a phrase join proves nothing"),
                join_vault: &no_vault,
                follow: &|_| Follows::Runs,
            };
            phrase_join_as(&j, &typed, &reach)
        });
        assert!(
            matches!(tried, PhraseTry::NotReached(_)),
            "no member heard shows a phrase for this Mac, so nothing was asked of the typed one: \
             {tried:?}"
        );
    }

    /// A member that answered the ask and then could not be dialed for the run (its Mac went to
    /// sleep, the network moved) was never sent the phrase: not a refused one.
    ///
    /// NEGATIVE CONTROL: have `join::run_phrase` call a dial failure `PhraseFailed::Sent`.
    #[test]
    fn a_phrase_never_sent_to_the_member_is_not_refused() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let m1 = member(root.path(), "m1");
        let j = me(root.path(), "j");
        let (tried, _) = with_member(&m1, |dial| {
            let ats = [M1.to_owned()];
            let _ = join::ask_all(dial, &j.joining(), &ats);
            let shown = SecretBytes::from(allow(&m1));
            let calls = std::sync::atomic::AtomicUsize::new(0);
            // The ask reaches the member; every dial after it finds the member gone.
            let flaky = |at: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
                if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    dial(at)
                } else {
                    Err(aterm_secrets::Error::new("host is down"))
                }
            };
            let reach = Reach {
                browse: &|| Ok(ats.to_vec()),
                dial: &flaky,
                prove: &|_, _, _| panic!("a phrase join proves nothing"),
                join_vault: &no_vault,
                follow: &|_| Follows::Runs,
            };
            phrase_join_as(&j, &shown, &reach)
        });
        assert!(
            matches!(tried, PhraseTry::NotReached(_)),
            "the run never reached the member, so the phrase was never tried: {tried:?}"
        );
    }

    /// A proof that may have signed (its possession answer went to a member) ends the round: no
    /// plain ask follows, so no member shows a "New Mac" entry for a Mac it may have just let in.
    /// A proof no member took and none may have signed is followed by the plain ask.
    ///
    /// NEGATIVE CONTROL: `proof_outcome` reading a missed proof that may have signed as
    /// `Proved::No` (its `signed_maybe` arm cut): the plain ask runs and the first assert fails
    /// (left `Ok(3)`, right `Ok(1)`).
    #[test]
    fn a_proof_that_may_have_signed_sends_no_plain_ask() {
        let missed = |signed_maybe| join::Missed {
            refused: vec![("m22:7".to_owned(), "the stream ended".to_owned())],
            signed_maybe,
        };
        let asked = std::cell::Cell::new(0);
        let ask = || {
            asked.set(asked.get() + 1);
            Ok(3)
        };
        assert_eq!(
            ask_after_proof(proof_outcome(Err(missed(true))), ask),
            Ok(1)
        );
        assert_eq!(asked.get(), 0, "no ask after a proof that may have signed");
        assert_eq!(
            ask_after_proof(proof_outcome(Err(missed(false))), ask),
            Ok(3)
        );
        assert_eq!(asked.get(), 1, "a plain ask after a proof nobody took");
    }
}
