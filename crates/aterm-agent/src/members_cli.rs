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
//! The lab verbs (`requests`, `allow`, `invite`, `ignore`, `remove`) are `aterm fleet`'s too
//! (`requests`, `admit`, `invite`, `ignore`, `remove`; one-mesh §4.16, B9): both front ends parse
//! into one [`LabVerb`] ([`parse_lab`]) and run it with [`run_lab`], so each signs and asks
//! exactly what the other does, in its own name.
//!
//! F16's verbs run in this process, on this Mac's own vault files: `list` prints the membership
//! log's members and the Macs it removed, `remove` takes a member out as this Mac (any member,
//! no admin), and `undo` lists the "Undo <label>'s removals" rows this Mac shows and where each
//! quorum stands. It signs nothing: a share of the quorum `Remove` is a person's act (§3.2), a
//! press of the row on this Mac's Machines view, which a terminal, an agent, `aterm ctl` or a
//! script over SSH cannot give. On a vault with no membership log yet, `remove` says an admin's
//! `aterm secrets revoke` is the way.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use aterm_secrets::SecretBytes;
use aterm_secrets::client::{self, Paths};
use aterm_secrets::commons::bootstrap::Device;
use aterm_secrets::discover;
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
  status                whether this Mac is a member, waiting, dropped, or neither";

/// How long the joiner browses for members.
const BROWSE_WITHIN: Duration = Duration::from_secs(3);

/// Each member's dial and every exchange on it.
const DIAL_WITHIN: Duration = Duration::from_secs(5);

/// One automatic proof round's dial budget. Fresh member addresses take the first half;
/// retained fleet recovery keeps the rest. Each connect, TLS handshake and complete proof
/// exchange share one absolute deadline. Verified admission returns after guarded local
/// publication; the subscriber follows the saved vault and repairs materialization separately.
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

/// The front end a lab verb was typed at: its name in every line the verb says, and its usage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Front {
    /// `aterm members`.
    Members,
    /// `aterm fleet` (one-mesh §4.16).
    Fleet,
}

impl Front {
    /// Its name, as each of its lines opens.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Members => "aterm members",
            Self::Fleet => "aterm fleet",
        }
    }

    /// Its usage.
    fn usage(self) -> &'static str {
        match self {
            Self::Members => USAGE,
            Self::Fleet => crate::fleet_member::USAGE,
        }
    }
}

/// One lab verb as either front end parsed it ([`parse_lab`]): what runs, the same for both.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LabVerb {
    /// The Macs asking that this Mac hears (`requests`).
    Requests,
    /// The line this Mac's running window answers (`members allow|ignore|invite …`), and whether
    /// it shows a phrase on this Mac's screen.
    Window {
        /// The request, one line.
        request: String,
        /// `allow` (`admit`) and `invite` show a phrase.
        shows: bool,
    },
    /// Take a Mac out of the lab as this Mac (`remove`).
    Remove(RemoveArgs),
}

/// Why a front end's words are no lab verb.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LabRefusal {
    /// Not one of its lab verbs, or one given the wrong words: its usage, exit 2.
    Usage,
    /// The verb's own refusal of its words, said after the front end's name: exit 2.
    Said(String),
}

/// `args` (the verb and its words) of `front` as a lab verb: `aterm members requests | allow
/// [--again] <name> | invite | ignore <name> | remove <name> [--stolen]`, and `aterm fleet`'s
/// same verbs, with `admit` for `allow` (`aterm fleet allow` is this Mac's own grant).
pub(crate) fn parse_lab(front: Front, args: &[String]) -> Result<LabVerb, LabRefusal> {
    let mut words = args.to_vec();
    match (front, words.first().map(String::as_str)) {
        (Front::Fleet, Some("admit")) => words[0] = "allow".to_owned(),
        (Front::Fleet, Some("allow")) => return Err(LabRefusal::Usage),
        _ => {}
    }
    match words.first().map(String::as_str) {
        Some("requests") if words.len() == 1 => Ok(LabVerb::Requests),
        Some("remove") => RemoveArgs::parse(&words[1..])
            .map(LabVerb::Remove)
            .map_err(|why| LabRefusal::Said(format!("remove: {why}"))),
        Some(verb @ ("allow" | "ignore" | "invite")) => {
            let shows = verb != "ignore";
            window_request(&words)
                .map(|request| LabVerb::Window { request, shows })
                .map_err(|_| LabRefusal::Usage)
        }
        _ => Err(LabRefusal::Usage),
    }
}

/// Run `verb` as `front`'s.
pub(crate) fn run_lab(front: Front, verb: LabVerb) -> ExitCode {
    match verb {
        LabVerb::Requests => {
            // The requests a member hears are the ones on its network: where this Mac is says
            // why none come from a phone hotspot. On stderr, so the reply stays the wire's.
            if let Some(header) = network_header() {
                eprintln!("{header}");
            }
            aterm_ctl::main_entry(vec!["members".into(), "requests".into()])
        }
        LabVerb::Window { request, shows } => to_window(front, &request, shows),
        LabVerb::Remove(remove) => in_vault(front, |paths| {
            remove_said(paths, &remove, &|word| {
                crate::fleet_member::removal_seat(paths, word)
            })
        }),
    }
}

/// `args` of `front` parsed and run as a lab verb ([`parse_lab`], [`run_lab`]).
pub(crate) fn lab_entry(front: Front, args: &[String]) -> ExitCode {
    match parse_lab(front, args) {
        Ok(verb) => run_lab(front, verb),
        Err(LabRefusal::Usage) => {
            eprintln!("{}", front.usage());
            ExitCode::from(2)
        }
        Err(LabRefusal::Said(why)) => {
            eprintln!("{} {why}", front.name());
            ExitCode::from(2)
        }
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
        Some("requests" | "allow" | "ignore" | "invite" | "remove") => {
            lab_entry(Front::Members, &args)
        }
        Some("list") if args.len() == 1 => in_vault(Front::Members, list_said),
        Some("undo") if args.len() <= 2 => in_vault(Front::Members, |paths| {
            undo_said(paths, args.get(1).map(String::as_str))
        }),
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
pub(crate) struct RemoveArgs {
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
            return Err("name the Mac to remove: its node id, label, name or join code".to_owned());
        }
        Ok(out)
    }
}

/// Run one of F16's in-process verbs on this Mac's own vault files: its lines on stdout (a
/// closed pipe or a full disk said as every answer says them), or its refusal on stderr and
/// exit 1, each in `front`'s name.
fn in_vault(front: Front, verb: impl FnOnce(&Paths) -> Result<Vec<String>, String>) -> ExitCode {
    let said = Paths::live()
        .map_err(|error| error.to_string())
        .and_then(|paths| verb(&paths));
    match said {
        Ok(lines) => {
            let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
            aterm_uds::stdout::answer_exit(front.name(), &text, 0)
        }
        Err(why) => {
            eprintln!("{}: {why}", front.name());
            ExitCode::from(1)
        }
    }
}

/// Each current seat's host name and node id, by its log label: what a removal names besides
/// the label it reports.
fn seats_by_label(paths: &Paths) -> BTreeMap<String, (String, Option<String>)> {
    let Ok(read) = client::load_acknowledged_and_ahead(paths) else {
        return BTreeMap::new();
    };
    let Some(log) = read.acknowledged.log() else {
        return BTreeMap::new();
    };
    crate::fleet_view::log_seats(log)
        .into_iter()
        .map(|seat| {
            let node = seat.node_key.as_ref().map(crate::fleet_view::principal_of);
            (seat.label, (seat.name, node))
        })
        .collect()
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

/// What a person's removal resolves its word to ([`crate::fleet_member::removal_seat`], live):
/// the seat B2's resolver names, `None` for a word the removal resolves by itself, or the
/// resolver's refusal.
pub(crate) type SeatOf<'a> =
    &'a dyn Fn(&str) -> Result<Option<crate::fleet_member::RemovalSeat>, String>;

/// `remove`'s lines: what was removed and rotated, the log name and node id it acted on (§4.16),
/// what the Mac could read, and on `--stolen` the one `NEEDS YOU:` line. Where `seat_of` names a
/// seat (live: for a node id or a registry label, B12) that seat is removed
/// ([`client::remove_seat`]), and where it refuses nothing is signed; any other word is resolved
/// by [`client::remove_member`].
pub(crate) fn remove_said(
    paths: &Paths,
    args: &RemoveArgs,
    seat_of: SeatOf<'_>,
) -> Result<Vec<String>, String> {
    let (removal, acted_on) = match seat_of(&args.who)? {
        Some(seat) => (
            client::remove_seat(paths, seat.actor, args.stolen),
            Some((seat.name, Some(seat.node))),
        ),
        None => {
            // Read before the removal: the seat is gone once its `Remove` stands.
            let mut seats = seats_by_label(paths);
            let removal = client::remove_member(paths, &args.who, args.stolen);
            let acted_on = removal
                .as_ref()
                .ok()
                .and_then(|removal| seats.remove(&removal.label));
            (removal, acted_on)
        }
    };
    let removal = removal.map_err(|error| error.to_string())?;
    let mut lines = vec![format!(
        "removed {}: {} records, new keys for {}",
        removal.label,
        removal.signed,
        removal.rotated.join(", ")
    )];
    if let Some((name, node)) = acted_on {
        lines.push(format!(
            "{} was {name}, node {}",
            removal.label,
            node.as_deref().unwrap_or("- (it bound no node key)")
        ));
    }
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

/// A member's verb, the line `request`, sent to this Mac's running window, which answers one
/// line. A verb that `shows` a phrase (`allow`, `invite`) with no window (SSH alone, no login)
/// refuses as §3.3.5 says.
fn to_window(front: Front, request: &str, shows: bool) -> ExitCode {
    let front = front.name();
    let Some(sock) = aterm_ctl::front_door_instance() else {
        eprintln!(
            "{front}: {}",
            if shows {
                "press Allow on a Mac with a screen"
            } else {
                "no aterm window runs on this Mac"
            }
        );
        return ExitCode::from(1);
    };
    match aterm_ctl::front_door_send(&sock, request) {
        Ok(reply) => match reply.strip_prefix("OK ") {
            Some(said) => {
                let mut said = said.to_owned();
                if !said.ends_with('\n') {
                    said.push('\n');
                }
                aterm_uds::stdout::answer_exit(front, &said, 0)
            }
            None => {
                let why = reply.strip_prefix("ERR ").unwrap_or(&reply);
                eprintln!("{front}: {}", why.strip_prefix("members: ").unwrap_or(why));
                ExitCode::from(1)
            }
        },
        Err(error) => {
            eprintln!("{front}: this Mac's window did not answer: {error}");
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

/// The joining Mac: its paths, Commons device, node key, name and model, where it is for its
/// join's words ([`join_place`], read once when the run starts), and the members service its
/// browse looks for.
struct Me {
    paths: Paths,
    device: Device,
    node: [u8; 32],
    name: String,
    model: String,
    place: join::JoinPlace,
    /// The DNS-SD service a browse for members looks for ([`discover::member_service`]): ALab's
    /// ([`discover::MEMBER_SERVICE`]), or a lab's where `ATERM_LAB_MEMBER_SERVICE` names one.
    members: String,
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
            members: discover::member_service(
                std::env::var_os(discover::LAB_MEMBER_SERVICE_ENV).as_deref(),
            )
            .unwrap_or_else(|_| discover::MEMBER_SERVICE.to_owned()),
        })
    }

    fn joining(&self) -> Result<Joining<'_>, String> {
        let store = client::load_records(&self.paths).map_err(|error| error.to_string())?;
        let records_bound =
            client::records_bound(&self.paths, &store).map_err(|error| error.to_string())?;
        Ok(Joining {
            device: &self.device,
            records_bound,
            node: self.node,
            name: self.name.clone(),
            model: self.model.clone(),
        })
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

/// [`dial`], one absolute budget shared by connect, TLS and the whole proof exchange.
fn dial_within(at: &str, within: Duration) -> aterm_secrets::Result<Box<dyn Stream>> {
    let until = std::time::Instant::now() + within;
    let (host, port) = aterm_net::fleet_member::parse_host_port(at, None)
        .map_err(|_| aterm_secrets::Error::new(format!("{at} is not host:port")))?;
    let (transport, _) = aterm_net::fleet_member::dial_ask(&host, None, port, within)
        .map_err(|error| aterm_secrets::Error::new(error.to_string()))?;
    let stream = transport.into_bounded_stream(until)?;
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

/// The two sources an automatic join can try without waiting for every member to be named.
trait MemberDiscovery: Sync {
    fn begin(&self, _within: Duration) -> aterm_secrets::Result<NumericMembers> {
        Ok(Vec::new().into())
    }
    fn more(&self, within: Duration) -> aterm_secrets::Result<Vec<String>>;
}

impl<F: Fn() -> aterm_secrets::Result<Vec<String>> + Sync> MemberDiscovery for F {
    fn more(&self, _within: Duration) -> aterm_secrets::Result<Vec<String>> {
        self()
    }
}

struct NativeMembers;

impl MemberDiscovery for NativeMembers {
    fn begin(&self, within: Duration) -> aterm_secrets::Result<NumericMembers> {
        Ok(NumericMembers::Probe(crate::lan_members::Probe::start(
            within.min(Duration::from_millis(500)),
        )?))
    }

    fn more(&self, within: Duration) -> aterm_secrets::Result<Vec<String>> {
        if within.is_zero() {
            Ok(Vec::new())
        } else {
            discover::browse_members(within)
        }
    }
}

/// Own passive native member availability hints for the subscriber's existing readiness
/// worker. They wake an ordinary authenticated ask and never supply membership evidence.
///
/// # Errors
/// The live member browse is refused by lab isolation, the identity cannot be read, or the
/// native arrival source cannot be opened.
pub fn member_arrivals(paths: &Paths) -> std::io::Result<Box<dyn client::MemberArrivals>> {
    Ok(Box::new(crate::lan_members::Arrivals::start(paths)?))
}

/// One owned numeric round. Explicit addresses and supplied complete sets need no
/// solicitation; a live probe keeps its nonce and sockets across authentication.
enum NumericMembers {
    Fixed(Option<Vec<String>>),
    Probe(crate::lan_members::Probe),
}

impl From<Vec<String>> for NumericMembers {
    fn from(found: Vec<String>) -> Self {
        Self::Fixed(Some(found))
    }
}

impl NumericMembers {
    fn next(&mut self, until: std::time::Instant) -> aterm_secrets::Result<Vec<String>> {
        match self {
            Self::Fixed(found) => Ok(found.take().unwrap_or_default()),
            Self::Probe(probe) => Ok(probe
                .next(until)?
                .into_iter()
                .map(|hint| hint.address.to_string())
                .collect()),
        }
    }

    fn complete(&mut self, until: std::time::Instant) -> aterm_secrets::Result<Vec<String>> {
        let mut all = Vec::new();
        loop {
            let found = self.next(until)?;
            if found.is_empty() {
                return Ok(all);
            }
            for at in found {
                if !all.contains(&at) {
                    all.push(at);
                }
            }
        }
    }
}

type Current<'a> = dyn Fn() -> aterm_secrets::Result<()> + Sync + 'a;
type ProveMembers<'a> =
    dyn Fn(&Me, &[String], std::time::Instant, &Current<'_>) -> Proved + Sync + 'a;
type AskedMembers = (Proved, Vec<Heard>, Vec<(String, String)>);

/// What `join` reaches beyond this process: ALab's members on this network, the ALab vault and
/// this Mac's vault subscriber. [`run_join`]'s are live; a test's stand in for them.
struct Reach<'a> {
    /// Direct numeric hints, followed where needed by the remaining members.
    browse: &'a dyn MemberDiscovery,
    /// One member's `ask` op.
    dial: &'a Dial<'a>,
    /// Prove this Mac to the members at the addresses given, dialing none after the instant
    /// given ([`proved_back`]).
    prove: &'a ProveMembers<'a>,
    /// Join the ALab vault without waiting ([`join_alab_vault`]).
    join_vault: &'a (dyn Fn(&Paths) -> aterm_secrets::Result<()> + Sync),
    /// Make sure this Mac's vault subscriber runs ([`Follows::live`]).
    follow: &'a (dyn Fn(&Path) -> Follows + Sync),
}

fn run_join(args: &JoinArgs) -> ExitCode {
    if args.phrase.is_none() && args.at.is_empty() {
        let tried = (|| {
            let paths = Paths::live()?;
            if client::config(&paths)?.is_some() {
                return Ok((paths, client::NativeAdmission::None));
            }
            client::refuse_lab_identity(&paths)?;
            client::prepare_alab_join_retry(&paths)?;
            client::native_bootstrap(&paths, &|| {
                if client::config(&paths)?.is_some() {
                    return Err(aterm_secrets::Error::new(
                        "the vault source changed during bootstrap",
                    ));
                }
                Ok(())
            })
            .map(|admitted| (paths, admitted))
        })();
        match tried {
            Ok((paths, admitted)) if admitted != client::NativeAdmission::None => {
                let follows = Follows::live(&paths.home);
                let first = if admitted == client::NativeAdmission::Admitted {
                    PROVED_IN
                } else {
                    join::LET_IN
                };
                return ExitCode::from(said_proved(
                    first,
                    Some(Ok(follows)),
                    &mut std::io::stdout(),
                ));
            }
            Err(error) => {
                eprintln!("aterm members join: {error}");
                return ExitCode::from(1);
            }
            _ => {}
        }
    }
    let me = match Me::live() {
        Ok(me) => me,
        Err(why) => {
            eprintln!("aterm members join: {why}");
            return ExitCode::from(1);
        }
    };
    let reach = Reach {
        browse: &NativeMembers,
        dial: &dial,
        prove: &|me, ats, until, current| {
            proved_back(me, ats, Some(until), args.at.is_empty(), false, current)
        },
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
    match join_tried(me, args, reach, out, err, &|| Ok(())) {
        Tried::Ended(code) => code,
        Tried::NoMember | Tried::Unsent | Tried::Dropped => 1,
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
    /// Before any member was looked for: a rotation dropped this Mac from the vault whose members
    /// it would ask ([`dropped_here`]), which refuse its key, so it asked none and stays dropped,
    /// with `aterm secrets status`'s words on stderr. Exit code 1.
    Dropped,
}

/// `aterm secrets status`'s words for `me` where `membership` reads it out of its vault, a
/// rotation dropped it ([`client::dropped_words`]), and the members this join asks serve that
/// vault ([`browse_serves`]): every member that read the rotation refuses its key, so no join
/// asks one with it and no phrase is spent on it (one-mesh E2, f299). Members a person names
/// with `--at` (`at`) serve a vault nothing here knows, and the members of another vault than
/// the one that dropped it never refused its key (a phrase moves it there afresh): both are
/// asked as before.
fn dropped_here(
    me: &Me,
    membership: &aterm_secrets::Result<client::Membership>,
    at: &[String],
) -> Option<String> {
    let Ok(client::Membership::Revoked { seq }) = membership else {
        return None;
    };
    let config = client::config(&me.paths).ok().flatten()?;
    if !at.is_empty() || !browse_serves(&me.members, &config) {
        return None;
    }
    client::dropped_words(&me.paths, *seq)
}

/// Whether the members a browse of `service` finds serve `config`'s vault: ALab's service finds
/// ALab's members, which advertise it only from ALab's vault, and a lab's service the lab's,
/// whose vault a lab Mac follows by its URL.
fn browse_serves(service: &str, config: &client::Config) -> bool {
    (service == discover::MEMBER_SERVICE) == client::alab_vault(config)
}

/// [`join_as`], saying whether it reached no member ([`Tried::NoMember`]).
fn join_tried(
    me: &Me,
    args: &JoinArgs,
    reach: &Reach<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
    authorized: &Current<'_>,
) -> Tried {
    let membership = client::status(&me.paths).map(|status| status.membership);
    // A seatless recipient (its locker opened, no seat binds it) is no member: it asks for a
    // seat as a new Mac asks, with the keys it holds (one-mesh S1).
    if matches!(membership, Ok(client::Membership::In { .. })) && !client::seat_asks(&me.paths) {
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
    if let Some(words) = dropped_here(me, &membership, &args.at) {
        let _ = writeln!(err, "aterm members join: {words}");
        return Tried::Dropped;
    }
    // A lab identity (one a test, a debug build, a lab lane or a VM made) never asks the live
    // ALab, whose members a browse of `_alab._tcp` finds: with no `--at` it refuses before it
    // looks for a member or dials one, and before the vault half joins ALab's vault. A lab's own
    // members service (`ATERM_LAB_MEMBER_SERVICE`) or the members a person names with `--at`
    // are asked as before, and a member on the live genesis refuses the marked ask itself.
    if args.at.is_empty()
        && let Err(error) = client::refuse_lab_member_browse(&me.paths)
    {
        let _ = writeln!(err, "aterm members join: {error}");
        return Tried::Ended(1);
    }
    let joining = match me.joining() {
        Ok(joining) => joining,
        Err(error) => {
            let _ = writeln!(err, "aterm members: {error}");
            return Tried::Ended(1);
        }
    };
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
    let native_first = args.phrase.is_none() && args.at.is_empty();
    // A later discovery stage must not adopt a different source as fresh authority.
    let expected = if native_first {
        match client::config(&me.paths) {
            Ok(expected) => expected,
            Err(error) => {
                let _ = writeln!(err, "aterm members join: {error}");
                return Tried::Ended(1);
            }
        }
    } else {
        None
    };
    let current = || {
        authorized()?;
        if native_first && client::config(&me.paths)? != expected {
            return Err(aterm_secrets::Error::new(
                "the vault source changed during member discovery",
            ));
        }
        Ok(())
    };
    let (asked, waits) = std::thread::scope(|scope| {
        let waits = (!native_first && (args.no_wait || args.phrase.is_none()))
            .then(|| scope.spawn(|| wait_at_vault(&me.paths, reach)));
        // With no phrase, prove this Mac first, as its subscriber does ([`ask_members`]): a
        // member on the home ALab LAN lets it in at once, with nobody's Allow, and it asks the
        // members plainly only where none did and none may have signed it in, or where the
        // proof's time ran out before any member signed.
        let asked = discover_and_ask(me, args, reach, &joining, proves, &current);
        let waits = if native_first {
            Some(
                authorized()
                    .map_err(|error| error.to_string())
                    .and_then(|()| match &asked {
                        Ok((Proved::In | Proved::SignedMaybe, ..)) => {
                            // No HTTP enrollment follows a successful or ambiguous possession.
                            // The proof's durable pin also survives a crash before this intent write.
                            if matches!(&asked, Ok((Proved::SignedMaybe, ..))) {
                                client::prepare_alab_join_retry(&me.paths)
                                    .map_err(|error| error.to_string())
                                    .map(|()| (reach.follow)(&me.paths.home))
                            } else {
                                Ok((reach.follow)(&me.paths.home))
                            }
                        }
                        _ => current()
                            .map_err(|error| error.to_string())
                            .and_then(|()| wait_at_vault(&me.paths, reach)),
                    }),
            )
        } else {
            waits.map(|waiting| {
                waiting
                    .join()
                    .unwrap_or_else(|_| Err("the vault join stopped".to_owned()))
            })
        };
        (asked, waits)
    });
    let (heard, missed) = match (asked, waits) {
        (Ok((Proved::In, ..)), waits) => return Tried::Ended(said_proved(PROVED_IN, waits, out)),
        (Ok((Proved::SignedMaybe, ..)), waits) => {
            return Tried::Ended(said_proved(join::let_in(me.place), waits, out));
        }
        (Ok((_, heard, missed)), None) => (heard, missed),
        (Ok((_, heard, missed)), Some(waits)) => {
            let browsed = args.at.is_empty();
            return Tried::Ended(said_no_wait(
                me.place,
                Asked {
                    heard: Ok(&heard),
                    missed: &missed,
                    browsed,
                },
                waits,
                out,
                err,
            ));
        }
        // A Mac that cannot browse (off macOS, or one with no LAN broker to browse through) still
        // waits at the vault, and says why it asked no member where install.sh shows it.
        (Err(error), Some(waits)) => {
            return Tried::Ended(said_no_wait(
                me.place,
                Asked {
                    heard: Err(error.to_string()),
                    missed: &[],
                    browsed: args.at.is_empty(),
                },
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
        say_missed(err, &missed, args.at.is_empty());
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

/// Automatic joins try numeric and retained routes before DNS. Only a definite refusal
/// continues; every possession keeps the caller's source check and one final proof deadline.
fn discover_and_ask(
    me: &Me,
    args: &JoinArgs,
    reach: &Reach<'_>,
    joining: &Joining<'_>,
    proves: bool,
    current: &Current<'_>,
) -> aterm_secrets::Result<AskedMembers> {
    let staged = args.phrase.is_none() && args.at.is_empty();
    let began = std::time::Instant::now();
    let mut numeric = if args.at.is_empty() {
        reach
            .browse
            .begin(BROWSE_WITHIN)
            .unwrap_or_else(|_| Vec::new().into())
    } else {
        args.at.clone().into()
    };
    if staged && proves {
        let until = began + PROVE_WITHIN;
        let (proved, all) = staged_member_proof(
            numeric,
            until,
            |ats, deadline| (reach.prove)(me, ats, deadline, current),
            || {
                reach.browse.more(
                    BROWSE_WITHIN
                        .saturating_sub(began.elapsed())
                        .min(until.saturating_duration_since(std::time::Instant::now())),
                )
            },
            current,
        )?;
        let (heard, missed) = match proved {
            Proved::No => join::ask_all_admitted(reach.dial, joining, &all, current),
            Proved::In | Proved::SignedMaybe => (Vec::new(), Vec::new()),
        };
        return Ok((proved, heard, missed));
    }
    let until = std::time::Instant::now() + PROVE_WITHIN;
    current()?;
    let mut all = numeric.complete(until).unwrap_or_default();
    if args.at.is_empty() {
        let found = match reach
            .browse
            .more(BROWSE_WITHIN.saturating_sub(began.elapsed()))
        {
            Ok(found) => found,
            Err(_) if !all.is_empty() => Vec::new(),
            Err(error) => return Err(error),
        };
        let mut more = Vec::new();
        for at in found {
            if !all.contains(&at) && !more.contains(&at) {
                more.push(at);
            }
        }
        all.extend(more.iter().cloned());
    }
    current()?;
    let proved = if proves {
        (reach.prove)(me, &all, until, current)
    } else {
        Proved::No
    };
    let (heard, missed) = match proved {
        Proved::No => join::ask_all_admitted(reach.dial, joining, &all, current),
        Proved::In | Proved::SignedMaybe => (Vec::new(), Vec::new()),
    };
    Ok((proved, heard, missed))
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
        // A lab identity (one `--at` sends to a lab's members) leaves no intent to join the live
        // ALab's vault: its subscriber would retry a join refused every time (A12).
        client::refuse_lab_identity(paths).map_err(|error| error.to_string())?;
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

/// What `join` adds under the members its browse named when aterm's own lookup of every one of
/// their names got no answer ([`aterm_net::fleet_member::LOOKUP_UNANSWERED`]). macOS's Bonjour
/// answered the browse with those names (it runs as `dns-sd`, which macOS does not hold), and
/// macOS holds an app's own lookups on the local network while its question whether that app may
/// find devices there stands unanswered. E3 (2026-11-05, `docs/measured/e2e-new-mac-2026-11-05.md`)
/// measured it in a guest: `members join <phrase>` waited out its 5 s on the member's `.local`
/// name while macOS asked, and `dscacheutil` resolved the same name at once.
const LOOKUP_HELD: &str = "macOS's Bonjour found these members, but aterm's own lookup of their \
     names got no answer: macOS holds those lookups while it asks whether aterm may find devices \
     on the local network. Answer Allow there (or turn aterm on in System Settings › Privacy & \
     Security › Local Network), then run this again.";

/// Each member `join` did not reach, with why, on `err`; then [`LOOKUP_HELD`] where the browse
/// named them all (`browsed`: no `--at`) and every lookup of their names got no answer. A name a
/// person gave with `--at` that does not resolve is no sign of macOS's question: that Mac may be
/// asleep or elsewhere.
fn say_missed(err: &mut dyn Write, missed: &[(String, String)], browsed: bool) {
    for (at, why) in missed {
        let _ = writeln!(err, "  {at}: {why}");
    }
    let unanswered = |why: &String| why.contains(aterm_net::fleet_member::LOOKUP_UNANSWERED);
    if browsed && !missed.is_empty() && missed.iter().map(|(_, why)| why).all(unanswered) {
        let _ = writeln!(err, "  {LOOKUP_HELD}");
    }
}

/// Who the members' half of `join --no-wait` asked ([`said_no_wait`]).
struct Asked<'a> {
    /// The members that answered (`Err`: why no member could be looked for).
    heard: Result<&'a [Heard], String>,
    /// Those that did not, each with why.
    missed: &'a [(String, String)],
    /// Whether the browse named them (no `--at`).
    browsed: bool,
}

/// What `join --no-wait` says, and its exit code; its first line is the one install.sh shows.
/// `asked` is who its members' half asked, and `waits` says whether this Mac waits at the ALab
/// vault and whether its subscriber, which asks the members again through the same browse,
/// runs. What lets it in follows where it is (`at`, [`join::let_in`]): automatic admission only
/// on the ALab LAN.
fn said_no_wait(
    at: join::JoinPlace,
    Asked {
        heard,
        missed,
        browsed,
    }: Asked<'_>,
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
        (heard, Ok(Follows::Bootstrapping(follows, why))) => {
            let next = if *follows == Follows::Runs {
                "this Mac's vault subscriber retries setup automatically"
            } else {
                "setup is saved here for the vault subscriber"
            };
            let _ = writeln!(out, "ALab vault setup has not finished ({why}); {next}");
            // The members' half still says how it went: a browse that could not run (no LAN
            // broker) or one that heard nobody is the person's to read here too.
            let _ = match heard {
                None => writeln!(out, "an ALab member on this network heard this Mac's ask"),
                Some(None) => writeln!(out, "no ALab member answers on this network yet"),
                Some(Some(why)) => writeln!(out, "{why}; no ALab member was asked"),
            };
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
            say_missed(err, missed, browsed);
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
    ask_members_admitted(home, &|| Ok(()))
}

/// Ask members while retaining the setup admission that requested the ask. The same callback
/// reaches native proof publication and ordinary requests after greetings, without taking a
/// new setup authority for either path.
/// # Errors
/// As [`ask_members`], or the caller's current authority is withdrawn.
pub fn ask_members_admitted(
    home: &Path,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<usize, String> {
    let state = aterm_types::dirs::state_dir().ok_or("no state directory")?;
    ask_members_at_state(Paths::for_home(home), &state, &NativeMembers, current)
}

/// The ordinary subscriber producer, with its actual node storage and discovery adapter.
fn ask_members_at_state(
    paths: Paths,
    state: &Path,
    discovery: &dyn MemberDiscovery,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<usize, String> {
    current()?;
    let expected = client::config(&paths).map_err(|error| error.to_string())?;
    let me = Me::at_state(paths, state)?;
    let authorized = || {
        current().map_err(aterm_secrets::Error::new)?;
        if client::config(&me.paths)? != expected {
            return Err(aterm_secrets::Error::new(
                "the vault source changed during member discovery",
            ));
        }
        Ok(())
    };
    if client::status(&me.paths)
        .is_ok_and(|status| matches!(status.membership, client::Membership::In { .. }))
        && !client::seat_asks(&me.paths)
    {
        return Err(format!("this Mac is already in ALab as {}", me.name));
    }
    // A lab identity's subscriber asks no live ALab member ([`join_tried`]).
    client::refuse_lab_member_browse(&me.paths).map_err(|error| error.to_string())?;
    authorized().map_err(|error| error.to_string())?;
    let began = std::time::Instant::now();
    let until = began + PROVE_WITHIN;
    let numeric = discovery
        .begin(BROWSE_WITHIN)
        .unwrap_or_else(|_| Vec::new().into());
    ask_discovered(
        numeric,
        until,
        |ats, deadline| {
            prove_routes_at_state(&me, state, ats, Some(deadline), true, &|| {
                authorized()?;
                client::check_bootstrap_allowed(&me.paths)
            })
        },
        || {
            discovery
                .more(
                    BROWSE_WITHIN
                        .saturating_sub(began.elapsed())
                        .min(until.saturating_duration_since(std::time::Instant::now())),
                )
                .map_err(|error| error.to_string())
        },
        &authorized,
        |ats| {
            authorized().map_err(|error| error.to_string())?;
            let dial: &Dial<'_> = &dial;
            let (heard, _) = join::ask_all_admitted(dial, &me.joining()?, ats, &authorized);
            authorized().map_err(|error| error.to_string())?;
            if heard.is_empty() {
                return Err("no ALab member answers on this network".to_owned());
            }
            Ok(heard.len())
        },
    )
}

/// Prove an unconfigured default join through numeric native hints, then the retained fleet.
/// Both paths require compiled membership authority before possession; no vault lookup or
/// HTTP enrollment runs here. The caller owns setup authorization and subscriber intent.
/// # Errors
/// Setup authority, sharing, source identity or lab isolation refuses this operation.
pub fn bootstrap_from_members(
    paths: &Paths,
    current: &(dyn Fn() -> aterm_secrets::Result<()> + Sync),
) -> aterm_secrets::Result<client::NativeAdmission> {
    current()?;
    client::refuse_lab_identity(paths)?;
    if client::config(paths)?.is_some() {
        return Err(aterm_secrets::Error::new(
            "native bootstrap requires an unconfigured source",
        ));
    }
    let me = Me::at(paths.clone()).map_err(aterm_secrets::Error::new)?;
    let until = std::time::Instant::now() + PROVE_WITHIN;
    let mut probe = crate::lan_members::Probe::start(Duration::from_millis(500)).ok();
    let authorized = || {
        current()?;
        if client::config(paths)?.is_some() {
            return Err(aterm_secrets::Error::new(
                "the vault source changed during bootstrap",
            ));
        }
        Ok(())
    };
    Ok(
        match bootstrap_native_proof(
            until,
            || {
                Ok(probe
                    .as_mut()
                    .map(|probe| probe.next(until))
                    .transpose()?
                    .unwrap_or_default()
                    .into_iter()
                    .map(|hint| hint.address.to_string())
                    .collect())
            },
            |ats, deadline| prove_routes(&me, ats, Some(deadline), true, &authorized),
            &authorized,
        )? {
            Proved::In => client::NativeAdmission::Admitted,
            Proved::SignedMaybe => client::NativeAdmission::Uncertain,
            Proved::No => {
                authorized()?;
                client::NativeAdmission::None
            }
        },
    )
}

/// The native-only bootstrap's proof turn, under its caller's original deadline.
fn bootstrap_native_proof(
    until: std::time::Instant,
    mut next: impl FnMut() -> aterm_secrets::Result<Vec<String>>,
    prove: impl FnMut(&[String], std::time::Instant) -> Proved,
    current: &Current<'_>,
) -> aterm_secrets::Result<Proved> {
    current()?;
    let ats = next().unwrap_or_default();
    current()?;
    native_proof_batches(ats, first_proof_share(until), until, next, prove, current)
        .map(|(outcome, _)| outcome)
}

/// Only definite refusals advance the held native round. Its caller fixes both
/// phase deadlines once; no batch gets another collection or proof lease.
fn native_proof_batches(
    mut batch: Vec<String>,
    first_until: std::time::Instant,
    until: std::time::Instant,
    mut next: impl FnMut() -> aterm_secrets::Result<Vec<String>>,
    mut prove: impl FnMut(&[String], std::time::Instant) -> Proved,
    current: &Current<'_>,
) -> aterm_secrets::Result<(Proved, Vec<String>)> {
    let mut all = Vec::new();
    let mut first = true;
    loop {
        current()?;
        let mut fresh = Vec::new();
        for at in batch {
            if !all.contains(&at) && !fresh.contains(&at) {
                fresh.push(at);
            }
        }
        if !first && fresh.is_empty() {
            return Ok((Proved::No, all));
        }
        let deadline = if first { first_until } else { until };
        first = false;
        let outcome = prove(&fresh, deadline);
        all.extend(fresh);
        match outcome {
            Proved::In | Proved::SignedMaybe => return Ok((outcome, all)),
            Proved::No => {}
        }
        current()?;
        if std::time::Instant::now() >= until {
            return Ok((Proved::No, all));
        }
        batch = next().unwrap_or_default();
    }
}

/// Only authenticated proof ends discovery. An untrusted first batch that
/// proves nothing continues to the other members; a possibly signed admission
/// sends nothing more until its ordinary retry can observe the records.
fn ask_discovered(
    direct: impl Into<NumericMembers>,
    until: std::time::Instant,
    prove: impl FnMut(&[String], std::time::Instant) -> Proved,
    more: impl FnOnce() -> Result<Vec<String>, String>,
    current: &Current<'_>,
    ask: impl FnOnce(&[String]) -> Result<usize, String>,
) -> Result<usize, String> {
    let (outcome, all) = staged_member_proof(
        direct,
        until,
        prove,
        || more().map_err(aterm_secrets::Error::new),
        current,
    )
    .map_err(|error| error.to_string())?;
    ask_after_proof(outcome, || ask(&all))
}

/// Automatic CLI and subscriber rounds share one bounded proof policy. An empty numeric
/// batch still tries retained peers; only genuinely new DNS hints get the remaining turn.
fn staged_member_proof(
    direct: impl Into<NumericMembers>,
    until: std::time::Instant,
    mut prove: impl FnMut(&[String], std::time::Instant) -> Proved,
    more: impl FnOnce() -> aterm_secrets::Result<Vec<String>>,
    current: &Current<'_>,
) -> aterm_secrets::Result<(Proved, Vec<String>)> {
    let mut numeric = direct.into();
    current()?;
    let direct = numeric.next(until).unwrap_or_default();
    current()?;
    let native_until = first_proof_share(until);
    let (outcome, mut direct) = native_proof_batches(
        direct,
        native_until,
        native_until,
        || numeric.next(native_until),
        &mut prove,
        current,
    )?;
    match outcome {
        Proved::In | Proved::SignedMaybe => return Ok((outcome, direct)),
        Proved::No => {}
    }
    current()?;
    let found = more();
    current()?;
    let found = match found {
        Ok(found) => found,
        Err(_) if !direct.is_empty() => Vec::new(),
        Err(error) => return Err(error),
    };
    let mut additional = Vec::new();
    for at in found {
        if !direct.contains(&at) && !additional.contains(&at) {
            additional.push(at);
        }
    }
    let outcome = if additional.is_empty() {
        Proved::No
    } else {
        prove(&additional, until)
    };
    direct.extend(additional);
    Ok((outcome, direct))
}

/// The fresh-address path's share of a bounded proof: half the time left, so
/// recovery over a previously held fleet keeps the rest.
fn first_proof_share(until: std::time::Instant) -> std::time::Instant {
    let now = std::time::Instant::now();
    (now + until.saturating_duration_since(now) / 2).min(until)
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
/// gets back in by itself. Freshly heard member addresses go first (its node key
/// alone, which moves the seat the log binds to it), before cached fleet peers
/// ([`crate::vault_join::prove_over_fleet`]: the channel proves the fleet's key too,
/// so a node key no seat binds gets a seat). One path after the other, and none after a try that may
/// have signed ([`Proved::SignedMaybe`]). No member is dialed on either path once `until` has
/// passed ([`crate::vault_join::dial_until`]; `None`: none is too late). The first path
/// dials none past half the time left, preserving the recovery path's turn. One
/// proof of this Mac runs at a time across processes ([`join::ProofLock`]): while another holds
/// it (`aterm members join` beside the subscriber's round) this one waits until `until`, then
/// sends nothing, as after a member that may have signed. [`Proved::No`] where no member let it
/// in: this Mac then asks a person as before.
fn proved_back(
    me: &Me,
    ats: &[String],
    until: Option<std::time::Instant>,
    default_alab: bool,
    background: bool,
    current: &Current<'_>,
) -> Proved {
    prove_routes(me, ats, until, default_alab, &|| {
        current()?;
        if background {
            client::check_bootstrap_allowed(&me.paths)?;
        }
        Ok(())
    })
}

struct ProofScope<'a> {
    admission: join::ProofAdmission<'a>,
}

impl<'a> ProofScope<'a> {
    fn read(
        paths: &Paths,
        expected: Option<&'a client::Config>,
        default_alab: bool,
        current: &'a (dyn Fn() -> aterm_secrets::Result<()> + Sync),
        proving: &'a join::ProofLock,
    ) -> aterm_secrets::Result<Self> {
        let retained = join::pending_proof(paths)?;
        let default_context = expected.is_none_or(client::alab_vault);
        Ok(Self {
            admission: join::ProofAdmission {
                expected,
                default_alab: default_context && (default_alab || retained),
                current,
                proving,
            },
        })
    }

    fn direct(
        &self,
        paths: &Paths,
        dial: &Dial<'_>,
        joining: &Joining<'_>,
        sign_node: aterm_secrets::members::joiner::SignNode<'_>,
        ats: &[String],
    ) -> Result<join::Joined, join::Missed> {
        join::prove_first_admitted(paths, dial, joining, sign_node, ats, &self.admission)
    }

    fn direct_prepared(
        &self,
        paths: &Paths,
        dial: &Dial<'_>,
        joining: &Joining<'_>,
        sign_node: aterm_secrets::members::joiner::SignNode<'_>,
        ats: &[String],
        until: std::time::Instant,
    ) -> Result<join::Joined, join::Missed> {
        if !join::pending_proof(paths).is_ok_and(|pending| !pending) {
            return self.direct(paths, dial, joining, sign_node, ats);
        }
        let (numeric, named): (Vec<_>, Vec<_>) = ats.iter().cloned().partition(|at| {
            aterm_net::fleet_member::parse_host_port(at, None)
                .is_ok_and(|(host, _)| host.parse::<std::net::IpAddr>().is_ok())
        });
        if numeric.is_empty() {
            return self.direct(paths, dial, joining, sign_node, ats);
        }
        let prepare = |at: &str| crate::vault_join::prepare_ask(at, until, DIAL_WITHIN);
        let mut missed = match join::prove_first_prepared_admitted(
            paths,
            &prepare,
            until,
            joining,
            sign_node,
            &numeric,
            Some(&self.admission),
        ) {
            Ok(joined) => return Ok(joined),
            Err(missed) if missed.signed_maybe => return Err(missed),
            Err(missed) => missed,
        };
        if named.is_empty() {
            return Err(missed);
        }
        self.direct(paths, dial, joining, sign_node, &named)
            .map_err(|mut later| {
                missed.refused.append(&mut later.refused);
                missed.signed_maybe = later.signed_maybe;
                missed
            })
    }
}

fn prove_routes(
    me: &Me,
    ats: &[String],
    until: Option<std::time::Instant>,
    default_alab: bool,
    authorized: &(dyn Fn() -> aterm_secrets::Result<()> + Sync),
) -> Proved {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return Proved::No;
    };
    prove_routes_at_state(me, &state, ats, until, default_alab, authorized)
}

fn prove_routes_at_state(
    me: &Me,
    state: &Path,
    ats: &[String],
    until: Option<std::time::Instant>,
    default_alab: bool,
    authorized: &(dyn Fn() -> aterm_secrets::Result<()> + Sync),
) -> Proved {
    use aterm_net::fleet_member::read_node_identity;
    let Ok(lock) = join::ProofLock::take(&me.paths, until) else {
        return Proved::No;
    };
    let Some(proving) = lock else {
        return Proved::SignedMaybe;
    };
    if authorized().is_err() {
        return Proved::No;
    }
    let Some(node) = read_node_identity(state).ok().flatten() else {
        return Proved::No;
    };
    if *node.key().bytes() != me.node {
        return Proved::No;
    }
    let Ok(joining) = me.joining() else {
        return Proved::No;
    };
    let sign_node = |message: &[u8]| -> [u8; 64] {
        node.sign_record(aterm_secrets::members::desk::PROVE_RECORD, message)
    };
    let direct_until = until.map(first_proof_share);
    let direct = crate::vault_join::dial_until(direct_until, DIAL_WITHIN, &dial_within);
    let Ok(expected) = client::config(&me.paths) else {
        return Proved::No;
    };
    let current = || {
        authorized()?;
        if client::config(&me.paths)? != expected
            || !aterm_secrets::swarm::enabled_in_home(&me.paths.home)?
        {
            return Err(aterm_secrets::Error::new(
                "this Mac's bootstrap authority changed",
            ));
        }
        Ok(())
    };
    let scope = match ProofScope::read(
        &me.paths,
        expected.as_ref(),
        default_alab,
        &current,
        &proving,
    ) {
        Ok(scope) => scope,
        Err(_) => return Proved::SignedMaybe,
    };
    let tried = match direct_until {
        Some(until) => scope.direct_prepared(&me.paths, &direct, &joining, &sign_node, ats, until),
        None => scope.direct(&me.paths, &direct, &joining, &sign_node, ats),
    };
    match proof_outcome(tried) {
        Proved::No => {}
        done => return done,
    }
    prove_retained_routes(&me.paths, state, &joining, &node, until, &scope.admission)
}

/// The same retained route for CLI and subscriber proofs, after the caller captured its
/// exact source/current policy and took the cross-process proof lock.
fn prove_retained_routes(
    paths: &Paths,
    state: &Path,
    joining: &Joining<'_>,
    node: &aterm_net::fleet::NodeIdentity,
    until: Option<std::time::Instant>,
    admission: &join::ProofAdmission<'_>,
) -> Proved {
    use aterm_net::fleet_member::read_peers;
    // Every joined member holds the canonical private key. Only the issuer has an
    // invitation file, whose absence or stale contents must not hide retained routes.
    if let Ok(secret) = aterm_net::fleet::read_key_file(state) {
        let peers: Vec<_> = read_peers(state)
            .unwrap_or_default()
            .into_iter()
            .filter(|peer| peer.node.is_some())
            .map(|peer| (peer.host, peer.addr, peer.port))
            .collect();
        if !peers.is_empty() {
            // The next try comes after the subscriber's pause, by when a member that may have
            // signed has posted its records to the servers.
            match proof_outcome(crate::vault_join::prove_over_fleet_admitted(
                paths,
                joining,
                (&secret, node),
                &peers,
                until,
                Some(admission),
            )) {
                Proved::No => {}
                done => return done,
            }
        }
    }
    Proved::No
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
    /// A rotation dropped this Mac from the vault whose members it would ask: none was asked with
    /// its key, which they refuse, so no phrase lets it in; it comes back as a new Mac. `aterm
    /// secrets status`'s words ([`client::dropped_words`]), which name its vault folder.
    Dropped(String),
    /// The phrase never went to a member: no ALab Mac answered on this network (one asleep, or
    /// on another network), none could be looked for (no LAN broker to browse through), none that answered shows a phrase for this Mac (the one that showed it is
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
        browse: &NativeMembers,
        dial: &dial,
        prove: &|me, ats, until, current| proved_back(me, ats, Some(until), false, false, current),
        join_vault: &join_alab_vault,
        follow: &Follows::live,
    };
    phrase_join_as(&me, phrase, &reach)
}

/// [`join_with_phrase`] as `me`, through `reach`: `join` with the phrase given, its lines on
/// stdout when it went (a Mac already in, or let in already, says so), else its lines on stderr.
/// A phrase that is not three of the list's words is refused before any member is looked for,
/// and a Mac a rotation dropped reads [`PhraseTry::Dropped`] before its phrase is read.
fn phrase_join_as(me: &Me, phrase: &SecretBytes, reach: &Reach<'_>) -> PhraseTry {
    // Before the phrase is read: no words let a dropped Mac in, so none read as a spent phrase.
    let membership = client::status(&me.paths).map(|status| status.membership);
    if let Some(words) = dropped_here(me, &membership, &[]) {
        return PhraseTry::Dropped(words);
    }
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
    let tried = join_tried(me, &args, reach, &mut out, &mut err, &|| Ok(()));
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
        Tried::Dropped => PhraseTry::Dropped(lines(&err).join("; ")),
    }
}

/// `status`: a member; allowed, with `~/.secrets` written, until the vault records it; asking;
/// dropped (it says how it comes back); or not in ALab.
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
    /// The vault's locker opened here, but no seat of the membership log binds this Mac: it is
    /// no member, and its vault subscriber asks ALab's members for a seat with the same keys
    /// (one-mesh S1, [`client::Seat::Seatless`]).
    Seatless,
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
    /// Its vault let it go with no act against it that it can read: a rotation left it out, and
    /// every member that read that rotation refuses its key, its proof and a person's Allow
    /// alike, so it comes back only as a new Mac, its vault folder set aside (one-mesh E2). The
    /// words are [`client::dropped_words`]'s, as `aterm secrets status` says them.
    Dropped(String),
    /// Neither a member nor asking, and not dropped: never in, taken out by an act it can read,
    /// or let go where its log does not say a rotation left it out on purpose (no trusted log, a
    /// heal or sync it waits on, or a key no newest epoch knew).
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
        Self::of_status(paths, client::status(paths), follows, place)
    }

    /// The standing `status` (the vault's at `paths`, read once) says.
    fn of_status(
        paths: &Paths,
        status: aterm_secrets::Result<client::Status>,
        follows: &dyn Fn(&Path) -> Follows,
        place: &dyn Fn() -> join::JoinPlace,
    ) -> Self {
        let status = match status {
            Ok(status) => status,
            Err(error) => return Self::Unknown(error.to_string()),
        };
        match status.membership {
            client::Membership::In { .. } if client::seat_asks(paths) => Self::Seatless,
            client::Membership::In { .. } => Self::Member(
                status
                    .config
                    .map_or_else(client::default_host_name, |config| config.host),
            ),
            client::Membership::Pending if client::holds_allowed(paths) => {
                let follows = follows(&paths.home);
                Self::Allowed {
                    fix: follows.fix(),
                    held: matches!(follows, Follows::Off),
                }
            }
            client::Membership::Pending => Self::Asking(place()),
            client::Membership::Revoked { seq } => {
                client::dropped_words(paths, seq).map_or(Self::NotIn, Self::Dropped)
            }
            client::Membership::Unconfigured | client::Membership::Plain => Self::NotIn,
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

    /// The standing `status` (the vault's at `paths`, read once) says to a reader of in, joining
    /// or out alone ([`crate::machine_name::InFleet`]: the machine title's row, `aterm name`'s
    /// `fleet=`): it asks no service manager whether the subscriber runs and reads no network
    /// place, so an `Allowed` says no turn-off and `Asking` an unknown place, and it spawns
    /// nothing. Not for `aterm members status`, whose lines need both.
    #[must_use]
    pub(crate) fn membership_of(
        paths: &Paths,
        status: aterm_secrets::Result<client::Status>,
    ) -> Self {
        Self::of_status(paths, status, &|_| Follows::Absent, &|| {
            join::JoinPlace::Unknown
        })
    }

    /// `aterm members status`'s line.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Member(name) => format!("member as {name}"),
            Self::Seatless => client::Seat::SEATLESS.to_owned(),
            Self::Allowed { fix, .. } => allowed_line(fix.as_deref()),
            Self::Asking(place) => format!("asking: {}", join::let_in(*place)),
            Self::Dropped(words) => words.clone(),
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

    mod staged_discovery_tests;

    const TRUST: Trust = Trust {
        network: [1; 16],
        authority: [2; 32],
        genesis: [3; 32],
    };

    /// A Mac with a synthetic machine key and Commons device under `root/name`. Its identity is
    /// unmarked, as a release build makes one on a Mac: these joins ask members a browse finds,
    /// which a lab identity never does ([`a_lab_identity_asks_no_member_a_browse_finds`]).
    fn mac(root: &Path, name: &str) -> (Paths, Device) {
        let paths = Paths::for_home(&root.join(name));
        client::machine_identity_as(&paths, aterm_secrets::Making::default()).expect("identity");
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
            members: discover::MEMBER_SERVICE.to_owned(),
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
            let (heard, missed) = join::ask_all(dial, &j.joining().unwrap(), &ats);
            assert_eq!(heard.len(), 1, "{missed:?}");
            let words = allow(m1);
            let reach = Reach {
                browse: &|| Ok(Vec::new()),
                dial,
                prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
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

    /// Write `membership` into the vault state of the Mac at `paths`, as its sync stores it.
    fn store_membership(paths: &Paths, membership: &str) {
        let at = paths.dir.join("state.toml");
        let mut state: aterm_toml::Table =
            aterm_toml::from_str(&std::fs::read_to_string(&at).expect("state")).expect("toml");
        state.insert(
            "membership".into(),
            aterm_toml::Value::String(membership.into()),
        );
        aterm_secrets::fsx::write_atomic(
            &at,
            aterm_toml::to_string(&state).expect("toml").as_bytes(),
            0o600,
        )
        .expect("state written");
    }

    /// A Mac a rotation dropped with no `Remove` (one-mesh E2) is told so, in `aterm secrets
    /// status`'s words, with the way back (a new Mac), and not to run `aterm members join` here,
    /// which every member that read the rotation refuses for its key.
    /// `client::test_read_dropped` stands in for the judgment, which aterm-secrets'
    /// `a_recipient_a_rotation_drops_with_no_remove_reads_out_and_says_dropped` holds to a real
    /// rotation.
    ///
    /// CONTROL: the same Mac, revoked and not dropped, reads `NotIn` and its old line.
    #[test]
    fn a_dropped_mac_says_how_it_comes_back_where_a_revoked_one_reads_not_in() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let m1 = member(root.path(), "m1");
        let j = me(root.path(), "j");
        let _ = let_in(&m1, &j, &|_| Follows::Runs);
        store_membership(&j.paths, "revoked");
        let Ok(client::Membership::Revoked { seq }) =
            client::status(&j.paths).map(|status| status.membership)
        else {
            panic!("j reads itself out");
        };
        let read = || Standing::read(&j.paths, &|_| Follows::Runs, &|| join::JoinPlace::Home);
        assert!(!client::dropped(&j.paths), "no rotation dropped j");
        assert_eq!(read(), Standing::NotIn, "the control");
        assert!(read().line().contains("aterm members join"));

        let (standing, words) =
            client::test_read_dropped(|| (read(), client::out_words(&j.paths, seq)));
        assert_eq!(standing, Standing::Dropped(words.clone()));
        let line = standing.line();
        assert_eq!(line, words, "the words `aterm secrets status` says");
        assert!(line.starts_with("dropped"), "{line}");
        assert!(!line.contains("aterm members join"), "{line}");
    }

    /// IN A FLEET (`docs/DESIGN-machine-names-2026-11-04.md` §11.85): a vault that took this
    /// Mac out outranks the fleet key it still holds, which is a secret it was let go from, not
    /// a membership.
    ///
    /// CONTROLS: the same Mac and key while it is in reads in, and the key alone, where no vault
    /// is read ([`crate::machine_name::standing_vault`]), still reads in.
    #[test]
    fn a_vault_that_took_this_mac_out_outranks_its_fleet_key() {
        use crate::machine_name::InFleet;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let m1 = member(root.path(), "m1");
        let j = me(root.path(), "j");
        let _ = let_in(&m1, &j, &|_| Follows::Runs);
        let state = root.path().join("state");
        aterm_uds::private_file::ensure_private_dir(&state).expect("state");
        let key = format!("{}\n", "ab".repeat(32));
        aterm_uds::private_file::write_private(&state, aterm_net::fleet::KEY_FILE, key.as_bytes())
            .expect("a fleet key");
        assert_eq!(
            InFleet::read(&state, Some(&j.paths)),
            InFleet::In,
            "the control: in, with a key"
        );
        store_membership(&j.paths, "revoked");
        assert_eq!(InFleet::read(&state, Some(&j.paths)), InFleet::Out);
        assert_eq!(InFleet::read(&state, None), InFleet::In, "the key alone");
    }

    /// A Mac a rotation dropped (one-mesh E2, f299) asks no member of that vault with its key:
    /// where its browse finds the members of the vault that dropped it (here a lab's members
    /// service, the lab's vault being this world's), `aterm members join` and the window's phrase
    /// join say `aterm secrets status`'s words before a member is looked for, dialed or proved to,
    /// or ALab's vault joined, a phrase that is not one included, and the Mac stays revoked.
    /// `client::test_read_dropped` stands in for the judgment, which aterm-secrets'
    /// `a_dropped_macs_join_sends_no_key_and_keeps_its_way_back` holds to a real rotation.
    ///
    /// CONTROLS, still dropped: a browse of ALab's service finds ALab's members, which never
    /// refused this key (a phrase moves the Mac to ALab afresh), and members a person names with
    /// `--at` serve a vault nothing here knows, so both joins look for a member as before. Not
    /// dropped, a phrase that is not one reads `Refused`.
    /// NEGATIVE CONTROL: without `join_tried`'s dropped check the lab join looks for a member
    /// (the count), and without `phrase_join_as`'s a phrase that is not one reads `Refused`
    /// ("Press Allow again" on the page).
    #[test]
    fn a_dropped_mac_asks_no_member_of_its_vault_and_no_phrase_reads_spent() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let m1 = member(root.path(), "m1");
        let mut j = me(root.path(), "j");
        let _ = let_in(&m1, &j, &|_| Follows::Runs);
        store_membership(&j.paths, "revoked");
        let Ok(client::Membership::Revoked { seq }) =
            client::status(&j.paths).map(|status| status.membership)
        else {
            panic!("j reads itself out");
        };
        let config = client::config(&j.paths).expect("config").expect("a vault");
        assert!(
            !client::alab_vault(&config),
            "this world's vault is a lab's"
        );
        let lab = "_alab-lab-e2._tcp";
        assert!(aterm_secrets::discover::lab_service(lab));
        assert!(browse_serves(lab, &config));
        assert!(!browse_serves(discover::MEMBER_SERVICE, &config));
        j.members = lab.to_owned();

        let reached = AtomicUsize::new(0);
        let dial = |_: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
            reached.fetch_add(1, Ordering::SeqCst);
            Err(aterm_secrets::Error::new("no member here"))
        };
        let reach = Reach {
            browse: &|| {
                reached.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            },
            dial: &dial,
            prove: &|_, _, _, _| {
                reached.fetch_add(1, Ordering::SeqCst);
                Proved::No
            },
            join_vault: &|paths| {
                reached.fetch_add(1, Ordering::SeqCst);
                no_vault(paths)
            },
            follow: &|_| Follows::Runs,
        };
        let phrase = SecretBytes::from(Phrase::generate().expect("a phrase").words().join(" "));
        let not_one = SecretBytes::from("not a phrase".to_owned());
        let joined = |me: &Me, args: &JoinArgs| {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let tried = join_tried(me, args, &reach, &mut out, &mut err, &|| Ok(()));
            (tried, String::from_utf8_lossy(&err).into_owned())
        };
        let named = JoinArgs {
            at: vec!["127.0.0.1:1".to_owned()],
            ..JoinArgs::default()
        };
        let alab = Me {
            members: discover::MEMBER_SERVICE.to_owned(),
            ..me(root.path(), "j")
        };

        let (words, tried, tried_as, phrased, unread, at_alab, at_named) =
            client::test_read_dropped(|| {
                let words = client::out_words(&j.paths, seq);
                let tried = joined(&j, &JoinArgs::default());
                let tried_as = join_as(
                    &j,
                    &JoinArgs::default(),
                    &reach,
                    &mut Vec::new(),
                    &mut Vec::new(),
                );
                let phrased = phrase_join_as(&j, &phrase, &reach);
                let unread = phrase_join_as(&j, &not_one, &reach);
                let before = reached.load(Ordering::SeqCst);
                let at_alab = (
                    joined(&alab, &JoinArgs::default()).0,
                    reached.load(Ordering::SeqCst) > before,
                );
                let before = reached.load(Ordering::SeqCst);
                let at_named = (
                    joined(&j, &named).0,
                    reached.load(Ordering::SeqCst) > before,
                );
                (words, tried, tried_as, phrased, unread, at_alab, at_named)
            });
        assert!(words.starts_with("dropped"), "{words}");
        assert_eq!(
            tried,
            (Tried::Dropped, format!("aterm members join: {words}\n"))
        );
        assert_eq!(tried_as, 1);
        assert_eq!(phrased, PhraseTry::Dropped(words.clone()));
        assert_eq!(unread, PhraseTry::Dropped(words));
        assert!(
            at_alab.0 != Tried::Dropped && at_alab.1,
            "ALab's members never refused this key: {at_alab:?}"
        );
        assert!(
            at_named.0 != Tried::Dropped && at_named.1,
            "members named with --at are asked: {at_named:?}"
        );
        assert!(matches!(
            client::status(&j.paths).map(|status| status.membership),
            Ok(client::Membership::Revoked { .. })
        ));

        // CONTROL: not dropped, the same Mac reads a phrase that is not one as refused.
        let refused = phrase_join_as(&j, &not_one, &reach);
        assert!(matches!(refused, PhraseTry::Refused(_)), "{refused:?}");
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
            let (heard, _) = join::ask_all(dial, &j.joining().unwrap(), &ats);
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
                prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
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
            let _ = join::ask_all(dial, &j.joining().unwrap(), &ats);
            let shown = allow(&m1);
            let reach = Reach {
                browse: &|| Ok(ats.to_vec()),
                dial,
                prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
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
    /// member's Mac asleep or waking, or no LAN broker to browse through) asked
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
            let _ = join::ask_all(dial, &j.joining().unwrap(), &ats);
            let shown = SecretBytes::from(allow(&m1));
            let nobody = Reach {
                browse: &|| Ok(Vec::new()),
                dial,
                prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
                join_vault: &no_vault,
                follow: &|_| Follows::Runs,
            };
            let first = phrase_join_as(&j, &shown, &nobody);
            let reach = Reach {
                browse: &|| Ok(ats.to_vec()),
                dial,
                prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
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
            browse: &|| {
                Err(aterm_secrets::Error::new(
                    aterm_uds::lan::Unavailable::NotInstalled.to_string(),
                ))
            },
            dial: &|_| panic!("no member is dialed"),
            prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
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

    /// A join whose browse named a member, and whose own lookup of that member's name got no
    /// answer, says macOS's Local Network question holds it (E3, 2026-11-05: a guest's
    /// `members join <phrase>` waited out its 5 s on the member's `.local` name while macOS asked,
    /// and was told only that no member answers). The same lookup of a name a person gave with
    /// `--at`, or a member the browse named that refused the connection, says no such thing: that
    /// Mac may be asleep or elsewhere.
    ///
    /// NEGATIVE CONTROL: drop the [`LOOKUP_HELD`] line from `say_missed`: the browsed join's
    /// words name no Local Network question, as the guest's did.
    #[test]
    fn a_member_the_browse_named_that_aterm_could_not_look_up_is_macos_local_network_question() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        // An identity a release build made: a lab identity browses no members (A12).
        let j = me(root.path(), "j");
        assert_eq!(client::lab_mark(&j.paths).expect("its mark"), None);
        let named = "lab-mini.local:7486".to_owned();
        let unanswered = aterm_net::fleet_member::Unreached::Lookup(
            "lab-mini.local".to_owned(),
            Duration::from_secs(5),
        )
        .to_string();
        let said = |args: &JoinArgs, why: &str| {
            let dial = |_: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
                Err(aterm_secrets::Error::new(why))
            };
            let reach = Reach {
                browse: &|| Ok(vec![named.clone()]),
                dial: &dial,
                prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
                join_vault: &no_vault,
                follow: &|_| Follows::Runs,
            };
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let tried = join_tried(&j, args, &reach, &mut out, &mut err, &|| Ok(()));
            assert_eq!(tried, Tried::NoMember);
            String::from_utf8(err).unwrap()
        };
        let phrase = || {
            Some(SecretBytes::from(
                Phrase::generate().expect("a phrase").words().join(" "),
            ))
        };
        let browsed = JoinArgs {
            phrase: phrase(),
            ..JoinArgs::default()
        };
        let err = said(&browsed, &unanswered);
        assert!(err.contains(&unanswered), "{err}");
        assert!(err.contains(LOOKUP_HELD), "the browse named it: {err}");
        let given = JoinArgs {
            phrase: phrase(),
            at: vec![named.clone()],
            ..JoinArgs::default()
        };
        let err = said(&given, &unanswered);
        assert!(
            err.contains(&unanswered) && !err.contains(LOOKUP_HELD),
            "{err}"
        );
        let err = said(&browsed, "nothing listens on 192.168.64.17:7486");
        assert!(!err.contains(LOOKUP_HELD), "a refused connect: {err}");
    }

    /// A12: a lab identity (one this test binary makes, as a debug build, a lab lane or a VM
    /// makes one) that runs `aterm members join` with no `--at`, or the window's phrase join,
    /// looks for no member, dials none and proves itself to none: a loopback member records no
    /// connection. The same join from an identity a release build made browses and dials it: the
    /// pair shows the refusal bites.
    ///
    /// NEGATIVE CONTROL: drop the `refuse_lab_member_browse` return in `join_tried`: the lab
    /// identity browses, and the loopback member hears it.
    #[test]
    fn a_lab_identity_asks_no_member_a_browse_finds() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback member");
        let at = listener.local_addr().expect("its address").to_string();
        let (rang, rings) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let stream = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .map(|(stream, _)| stream);
                drop(stream);
                if rang.send(()).is_err() {
                    break;
                }
            }
        });
        let (browsed, proved, vaulted) = (
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        );
        let dial = |at: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
            let stream = std::net::TcpStream::connect(at)?;
            stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
            Ok(Box::new(stream))
        };
        let reach = Reach {
            browse: &|| {
                browsed.fetch_add(1, Ordering::SeqCst);
                Ok(vec![at.clone()])
            },
            dial: &dial,
            prove: &|_, _, _, _| {
                proved.fetch_add(1, Ordering::SeqCst);
                Proved::No
            },
            join_vault: &|paths| {
                vaulted.fetch_add(1, Ordering::SeqCst);
                no_vault(paths)
            },
            follow: &|_| Follows::Runs,
        };
        // The identity this Mac's process makes, which `me` keeps.
        client::machine_identity(&Paths::for_home(&root.path().join("lab"))).expect("identity");
        let lab = me(root.path(), "lab");
        assert_eq!(
            client::lab_mark(&lab.paths).expect("its mark"),
            Some(aterm_secrets::LabMark::TestBinary)
        );
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let tried = join_tried(
            &lab,
            &JoinArgs::default(),
            &reach,
            &mut out,
            &mut err,
            &|| Ok(()),
        );
        let said = String::from_utf8_lossy(&err).into_owned();
        assert_eq!(tried, Tried::Ended(1), "{said}");
        assert!(said.contains(client::LAB_IDENTITY_REFUSAL), "{said}");
        let phrase = SecretBytes::from(Phrase::generate().expect("a phrase").words().join(" "));
        assert!(
            matches!(
                phrase_join_as(&lab, &phrase, &reach),
                PhraseTry::Refused(ref why) if why.contains(client::LAB_IDENTITY_REFUSAL)
            ),
            "the window's phrase join refuses too"
        );
        assert_eq!(
            browsed.load(Ordering::SeqCst),
            0,
            "no member was looked for"
        );
        assert_eq!(proved.load(Ordering::SeqCst), 0, "no member was proved to");
        assert_eq!(
            vaulted.load(Ordering::SeqCst),
            0,
            "ALab's vault was not joined"
        );
        assert!(rings.try_recv().is_err(), "no member was dialed");

        // The members a person names with `--at` are asked; the live ALab's vault is neither
        // joined nor left for the subscriber to retry.
        let named = JoinArgs {
            at: vec![at.clone()],
            ..JoinArgs::default()
        };
        join_tried(&lab, &named, &reach, &mut out, &mut err, &|| Ok(()));
        rings
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the member named is dialed");
        assert_eq!(browsed.load(Ordering::SeqCst), 0, "none was looked for");
        assert_eq!(
            vaulted.load(Ordering::SeqCst),
            0,
            "ALab's vault was not joined"
        );
        assert!(
            !lab.paths.dir.join("alab-join-pending").exists(),
            "no intent to join ALab's vault"
        );

        let release = me(root.path(), "release");
        assert_eq!(client::lab_mark(&release.paths).expect("its mark"), None);
        let mut err = Vec::new();
        // The member hangs up on the ask, and the join waits at the vault, as any does.
        join_tried(
            &release,
            &JoinArgs::default(),
            &reach,
            &mut out,
            &mut err,
            &|| Ok(()),
        );
        let said = String::from_utf8_lossy(&err).into_owned();
        assert!(!said.contains(client::LAB_IDENTITY_REFUSAL), "{said}");
        assert_eq!(
            browsed.load(Ordering::SeqCst),
            1,
            "a release identity browses"
        );
        assert_eq!(vaulted.load(Ordering::SeqCst), 1, "and joins ALab's vault");
        rings
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("and dials the member it found");
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
                    prove: &|_, _, _, _| panic!("a Mac let in proves nothing again"),
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
            let code = said_no_wait(
                at,
                Asked {
                    heard: Ok(&[]),
                    missed: &[],
                    browsed: true,
                },
                Ok(Follows::Runs),
                &mut out,
                &mut err,
            );
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
        // A Mac a release build set up: a lab identity sets up no join of ALab's vault.
        client::machine_identity_as(&paths, aterm_secrets::Making::default()).expect("identity");
        let followed = std::sync::atomic::AtomicBool::new(false);
        let follow = |_: &Path| {
            followed.store(true, std::sync::atomic::Ordering::Relaxed);
            Follows::Runs
        };
        let reach = Reach {
            browse: &|| Ok(Vec::new()),
            dial: &|_| panic!("no member dial"),
            prove: &|_, _, _, _| panic!("the vault half proves nothing"),
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
                Asked {
                    heard: Ok(&[]),
                    missed: &[],
                    browsed: true,
                },
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
        assert!(
            out.contains("no ALab member answers on this network yet"),
            "the members' half is said too: {out}"
        );
    }

    /// A vault setup that has not finished still says how the members' half went: a browse that
    /// could not run names why (no LAN broker to browse through), and a member that heard is
    /// said as such.
    /// NEGATIVE CONTROL: the arm that printed only the vault's line (round 8's vault-phrase row
    /// read it and could not tell a browse that could not run from an empty one) fails each
    /// assertion.
    #[test]
    fn an_unfinished_vault_setup_still_says_how_the_members_ask_went() {
        let bootstrapping = || {
            Ok(Follows::Bootstrapping(
                Box::new(Follows::Runs),
                "first fetch unavailable".to_owned(),
            ))
        };
        let refused = format!(
            "browsing _alab._tcp: {}",
            aterm_uds::lan::Unavailable::NotInstalled
        );
        let said = |heard: Result<&[Heard], String>| {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = said_no_wait(
                join::JoinPlace::Home,
                Asked {
                    heard,
                    missed: &[],
                    browsed: true,
                },
                bootstrapping(),
                &mut out,
                &mut err,
            );
            assert_eq!(code, 0);
            String::from_utf8(out).unwrap()
        };
        let out = said(Err(refused.clone()));
        assert!(
            out.starts_with("ALab vault setup has not finished"),
            "{out}"
        );
        assert!(
            out.contains(&format!("{refused}; no ALab member was asked")),
            "{out}"
        );
        let out = said(Ok(&[]));
        assert!(
            out.contains("no ALab member answers on this network yet"),
            "{out}"
        );
        assert!(!out.contains("LAN broker"), "{out}");
    }

    #[test]
    fn a_headless_bootstrap_keeps_sharing_off_before_fetch_or_follow() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        std::fs::create_dir_all(root.path().join(".config/aterm")).unwrap();
        aterm_uds::atomic_file::write(
            &root.path().join(".config/aterm/aterm.toml"),
            b"[swarm]\nenabled = false\n",
            0o600,
        )
        .unwrap();
        let reach = Reach {
            browse: &|| panic!("no browse"),
            dial: &|_| panic!("no dial"),
            prove: &|_, _, _, _| panic!("the vault half proves nothing"),
            join_vault: &|_| panic!("sharing off: no fetch"),
            follow: &|_| panic!("sharing off: no follower"),
        };
        // A Mac a release build set up, so sharing alone stops it (a lab identity would be
        // refused first).
        client::machine_identity_as(&paths, aterm_secrets::Making::default()).expect("identity");
        let refused = wait_at_vault(&paths, &reach).expect_err("sharing off");
        assert!(refused.contains("sharing is off"), "{refused}");
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
                    prove: &|_, _, _, _| Proved::No,
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
            let prove = |_: &Me, ats: &[String], until: std::time::Instant, _: &Current<'_>| {
                if ats.is_empty() {
                    return Proved::No; // This fixture has no cached fleet route.
                }
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
        crate::test_config::write(config.join("aterm.toml"), "[swarm]\nenabled = false\n").unwrap();
        let ((code, said), dials) = with_member(&m1, |dial| {
            let reach = Reach {
                browse: &|| Ok(vec![M1.to_owned()]),
                dial,
                prove: &|_, _, _, _| panic!("sharing off: no proof"),
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
            let tried =
                join::prove_first(&mac.paths, &dial, &mac.joining().unwrap(), &sign_node, &ats);
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

    #[test]
    fn a_pending_default_vault_requires_authority_after_restart_without_a_native_pin() {
        use std::io::{BufRead as _, Write as _};
        let root = aterm_tempfile::tempdir().unwrap();
        let mac = me(root.path(), "pending");
        let source = mac.paths.dir.join("source.toml");
        let saved = format!(
            "url = {:?}\nname = 'alab'\nsigner = '{}'\nhost = 'pending'\n",
            aterm_secrets::ALAB_DEFAULT_URL,
            aterm_secrets::ALAB_SIGNER,
        );
        aterm_secrets::fsx::write_atomic(&source, saved.as_bytes(), 0o600).unwrap();
        assert_eq!(
            client::status(&mac.paths).unwrap().membership,
            client::Membership::Pending
        );
        let paths = Paths::for_home(&mac.paths.home);
        assert!(!join::pending_proof(&paths).unwrap());
        let expected = client::config(&paths).unwrap();
        assert!(expected.as_ref().is_some_and(client::alab_vault));
        let device = Device::open(&paths).unwrap().unwrap();
        let joining = Joining {
            device: &device,
            records_bound: aterm_secrets::crossing::RecordsBound::uncrossed(),
            node: mac.node,
            name: mac.name.clone(),
            model: mac.model.clone(),
        };
        let proving = join::ProofLock::take(&paths, None).unwrap().unwrap();
        let current = || Ok(());
        let scope = ProofScope::read(&paths, expected.as_ref(), true, &current, &proving).unwrap();
        let captured = Mutex::new(Vec::new());
        let tried = std::thread::scope(|threads| {
            let dial = |_: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
                let (mut member, joiner) = UnixStream::pair()?;
                for stream in [&member, &joiner] {
                    stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
                    stream.set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))?;
                }
                let captured = &captured;
                threads.spawn(move || {
                        // A legacy member advertises a syntactically valid greeting, then
                        // refuses this extension before issuing any possession challenge.
                        writeln!(member,
                            "ALAB member=1 key={} network={} authority={} genesis={} name=legacy allow=1 invite=0 challenge={}",
                            "09".repeat(32), "01".repeat(16), "02".repeat(32),
                            "03".repeat(32), "04".repeat(16),
                        ).unwrap();
                        let mut line = String::new();
                        std::io::BufReader::new(&mut member).read_line(&mut line).unwrap();
                        captured.lock().unwrap().push(line);
                        member.write_all(b"NO why=unsupported-authority\n").unwrap();
                    });
                Ok(Box::new(joiner))
            };
            scope.direct(&paths, &dial, &joining, &|_| [0; 64], &[M1.to_owned()])
        });
        assert!(matches!(proof_outcome(tried), Proved::No));
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(request.starts_with("prove "));
        let nonce = request
            .split_ascii_whitespace()
            .find_map(|field| field.strip_prefix("authority="))
            .expect("the production selector must use the admitted witness path");
        assert_eq!(nonce.len(), 64);
        assert!(nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(std::fs::read(&source).unwrap(), saved.as_bytes());
        assert!(!join::pending_proof(&paths).unwrap());
        assert_eq!(
            client::status(&paths).unwrap().membership,
            client::Membership::Pending
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
    /// `discover::NO_BROWSE`, or one with no LAN broker to browse through), install.sh's
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
            prove: &|_, ats, _, current| {
                current().unwrap();
                assert!(ats.is_empty(), "this fixture has no cached fleet route");
                Proved::No
            },
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

    /// The kinds of the records `paths`' log holds that `before` does not, in order of kind.
    fn signed_since(
        paths: &Paths,
        before: &std::collections::BTreeSet<aterm_secrets::membership_log::RecordId>,
    ) -> Vec<aterm_secrets::membership_log::Kind> {
        let store = client::load_records(paths).expect("records");
        let mut kinds: Vec<_> = store
            .log()
            .expect("a log")
            .records()
            .filter(|record| !before.contains(record.id()))
            .map(aterm_secrets::membership_log::Record::kind)
            .collect();
        kinds.sort_unstable();
        kinds
    }

    /// `aterm fleet remove` and `aterm members remove` are one verb (one-mesh §4.16): each front
    /// parses to the same removal, which signs the same records into a like vault and says the
    /// same lines, the log name and node id it acted on among them.
    ///
    /// NEGATIVE CONTROL: in `parse_lab`, set `stolen` on the fleet front's `remove`: its removal
    /// says the `NEEDS YOU:` line and the two no longer compare equal.
    #[test]
    fn fleet_remove_and_members_remove_sign_the_same_records() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let words = ["remove".to_owned(), "member".to_owned()];
        let mut removals = Vec::new();
        for (front, dir) in [(Front::Members, "members"), (Front::Fleet, "fleet")] {
            let Ok(LabVerb::Remove(remove)) = parse_lab(front, &words) else {
                panic!("{} remove member is a removal", front.name());
            };
            let fixture =
                client::test_commons_excluded(&root.path().join(dir)).expect("a vault and member");
            let before = client::load_records(&fixture.admin)
                .expect("records")
                .inventory();
            // `member` is no node id or registry label: the removal resolves it by word.
            let said =
                remove_said(&fixture.admin, &remove, &|_| Ok(None)).expect("the admin removes it");
            let signed = signed_since(&fixture.admin, &before);
            fixture.server.stop();
            removals.push((remove, said, signed));
        }
        assert_eq!(removals[0], removals[1]);
        let (_, said, signed) = &removals[0];
        assert!(
            signed.contains(&aterm_secrets::membership_log::Kind::Remove),
            "{signed:?}"
        );
        assert!(
            said.iter()
                .any(|line| line.ends_with("was member, node - (it bound no node key)")),
            "{said:?}"
        );
    }

    /// Both front ends read the same lab verbs; `aterm fleet` says `admit` where `aterm members`
    /// says `allow`, since `aterm fleet allow` is this Mac's own drive grant.
    #[test]
    fn both_fronts_read_the_same_lab_verbs() {
        let words = |text: &str| -> Vec<String> { text.split(' ').map(str::to_owned).collect() };
        for (members, fleet) in [
            ("requests", "requests"),
            ("invite", "invite"),
            ("ignore m12", "ignore m12"),
            ("allow m12", "admit m12"),
            ("allow --again m12", "admit --again m12"),
            ("remove m12", "remove m12"),
            ("remove --stolen m12", "remove --stolen m12"),
        ] {
            let (m, f) = (
                parse_lab(Front::Members, &words(members)),
                parse_lab(Front::Fleet, &words(fleet)),
            );
            assert!(m.is_ok(), "{members}: {m:?}");
            assert_eq!(m, f, "{members} / {fleet}");
        }
        assert_eq!(
            parse_lab(Front::Fleet, &words("allow m12")),
            Err(LabRefusal::Usage)
        );
        assert_eq!(
            parse_lab(Front::Members, &words("admit m12")),
            Err(LabRefusal::Usage)
        );
        assert_eq!(
            parse_lab(Front::Fleet, &words("requests now")),
            Err(LabRefusal::Usage)
        );
        assert!(matches!(
            parse_lab(Front::Fleet, &words("remove")),
            Err(LabRefusal::Said(_))
        ));
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
                prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
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
            let _ = join::ask_all(dial, &j.joining().unwrap(), &ats);
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
                prove: &|_, _, _, _| panic!("a phrase join proves nothing"),
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

    #[test]
    fn direct_admission_skips_bonjour_only_after_proof_and_continues_unproved_hints() {
        for outcome in [Proved::In, Proved::SignedMaybe] {
            let mut outcome = Some(outcome);
            assert_eq!(
                ask_discovered(
                    vec!["192.0.2.1:7100".into()],
                    std::time::Instant::now() + PROVE_WITHIN,
                    |_, _| outcome.take().unwrap(),
                    || panic!("a direct proof needs no browse"),
                    &|| Ok(()),
                    |_| panic!("an admission that may have signed sends no ask")
                ),
                Ok(1)
            );
        }
        let attempts = std::cell::RefCell::new(Vec::new());
        let deadlines = std::cell::RefCell::new(Vec::new());
        let until = std::time::Instant::now() + PROVE_WITHIN;
        assert_eq!(
            ask_discovered(
                vec!["192.0.2.1:7100".into()],
                until,
                |ats, deadline| {
                    attempts.borrow_mut().push(ats.to_vec());
                    deadlines.borrow_mut().push(deadline);
                    if ats == ["member.local:7100"] {
                        Proved::In
                    } else {
                        Proved::No
                    }
                },
                || Ok(vec![
                    "192.0.2.1:7100".into(),
                    "member.local:7100".into(),
                    "member.local:7100".into()
                ]),
                &|| Ok(()),
                |_| panic!("the continued discovery proved a member")
            ),
            Ok(1)
        );
        let deadlines = deadlines.borrow();
        assert!(deadlines[0] < deadlines[1]);
        assert_eq!(
            deadlines[1], until,
            "later hints retain only the original final deadline"
        );
        assert_eq!(
            *attempts.borrow(),
            [
                vec!["192.0.2.1:7100".to_owned()],
                vec!["member.local:7100".to_owned()]
            ]
        );
    }

    #[test]
    fn an_elapsed_round_never_renews_the_retained_proof_deadline() {
        let until = std::time::Instant::now() - Duration::from_secs(1);
        let attempts = std::cell::Cell::new(0);
        let (outcome, found) = staged_member_proof(
            Vec::new(),
            until,
            |ats, deadline| {
                assert!(ats.is_empty());
                assert!(
                    deadline <= until,
                    "discovery cannot renew an elapsed proof round"
                );
                attempts.set(attempts.get() + 1);
                Proved::No
            },
            || Ok(Vec::new()),
            &|| Ok(()),
        )
        .unwrap();
        assert!(matches!(outcome, Proved::No));
        assert!(found.is_empty());
        assert_eq!(
            attempts.get(),
            1,
            "an empty DNS set never repeats retained proof"
        );
    }

    #[test]
    fn failed_direct_proofs_keep_all_reachable_members_for_the_plain_request() {
        assert_eq!(
            ask_discovered(
                vec!["192.0.2.1:7100".into()],
                std::time::Instant::now() + PROVE_WITHIN,
                |_, _| Proved::No,
                || Ok(vec!["member.local:7100".into()]),
                &|| Ok(()),
                |ats| {
                    assert_eq!(ats, ["192.0.2.1:7100", "member.local:7100"]);
                    Ok(2)
                }
            ),
            Ok(2)
        );
    }
}
