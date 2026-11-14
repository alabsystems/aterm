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
use aterm_secrets::membership_log::Evidence;
use aterm_secrets::phrase::Phrase;

const USAGE: &str = "aterm members: Macs joining ALab and the members that let them in
  requests              the Macs asking that this Mac hears
  allow [--again] <name>
                        show a phrase for that Mac on this Mac's screen (--again: a Mac
                        the vault removed, which the run restores)
  invite                show a phrase for whichever Mac types it next
  ignore <name>         hide that Mac's request on this Mac
  list                  the members and the Macs the vault removed, as this Mac's log says:
                        who let each in, the day (UTC), how (`how=phrase` a person's Allow,
                        `manual` the member by itself, `listed` the genesis), whether its
                        seat holds a fleet node key, and each pair of seats the log ties
                        (`split`)
  remove <name> [--stolen]
                        take that Mac out of ALab as this Mac, rotating the fleet code
                        (--stolen: also deny-list its labels and node in the roster, and
                        file each credential it could read for rotation in the need ledger)
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

/// `list`'s lines: one per member (who let it in, the UTC day its entry was signed, how the
/// member knew to, and whether the log holds a fleet node key for its seat), then one per Mac the
/// log removed, then one per pair of standing seats the log ties ([`client::Split::line`]: one
/// Mac seated twice, a writer successor beside a seat of its line, or a Mac that asked under
/// another standing seat's label).
fn list_said(paths: &Paths) -> Result<Vec<String>, String> {
    let listed = client::member_list(paths)
        .map_err(|error| error.to_string())?
        .ok_or("this Mac's vault has no membership log yet")?;
    let splits = client::seat_splits(paths).map_err(|error| error.to_string())?;
    Ok(listed
        .iter()
        .map(listed_line)
        .chain(splits.iter().map(client::Split::line))
        .collect())
}

/// One Mac's `list` line: a member's says who let it in, the UTC day its entry was signed, how
/// the member knew to (`how=phrase`: a person's Allow; `how=manual`: the member by itself, on a
/// proof, a person's `aterm secrets approve` or a writer succession; `how=listed`: the genesis
/// listed it) and whether the log holds a fleet node key for its seat; a removed Mac's says who
/// removed it.
fn listed_line(mac: &client::ListedMac) -> String {
    match &mac.removed {
        Some(note) => format!(
            "{} {} code={} removed by={}{}",
            mac.label,
            mac.name,
            mac.code,
            note.by.join(","),
            if note.quorum { " quorum" } else { "" }
        ),
        None => format!(
            "{} {} code={}{}{}{} node={}",
            mac.label,
            mac.name,
            mac.code,
            mac.admitted_by
                .as_ref()
                .map_or(" founder".to_owned(), |by| format!(" admitted by={by}")),
            signed_day(mac.admitted_ms).map_or(String::new(), |day| format!(" on={day}")),
            mac.how.map_or("", |how| match how {
                Evidence::Phrase => " how=phrase",
                Evidence::Manual => " how=manual",
                Evidence::Listed => " how=listed",
            }),
            if mac.node { "yes" } else { "no" }
        ),
    }
}

/// The UTC day `ms` falls on, as `YYYY-MM-DD`; `None` for an entry whose record this Mac's log
/// does not hold (0).
fn signed_day(ms: u64) -> Option<String> {
    (ms > 0).then(|| {
        let days = i64::try_from(ms / 86_400_000).unwrap_or(i64::MAX);
        let (year, month, day) = aterm_types::rfc3339::civil_from_days(days);
        format!("{year:04}-{month:02}-{day:02}")
    })
}

/// What a person's removal resolves its word to ([`crate::fleet_member::removal_seat`], live):
/// the seat B2's resolver names, `None` for a word the removal resolves by itself, or the
/// resolver's refusal.
pub(crate) type SeatOf<'a> =
    &'a dyn Fn(&str) -> Result<Option<crate::fleet_member::RemovalSeat>, String>;

/// `remove`'s lines: what was removed and rotated, the log name and node id it acted on (§4.16),
/// what the Mac could read, and on `--stolen` the one line naming what was filed for rotation in
/// this Mac's need ledger. Where `seat_of` names a
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
    lines.extend(removal.rotation);
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
    // A lab identity (one a test, a debug build, a lab lane, a VM or a process in a foreign home
    // made) never asks the live ALab, whose members a browse of `_alab._tcp` finds: with no
    // `--at` it refuses before it looks for a member or dials one, and before the vault half
    // joins ALab's vault. A lab's own members service (`ATERM_LAB_MEMBER_SERVICE`) or the members
    // a person names with `--at` are asked as before, and a member on the live genesis refuses
    // the marked ask itself.
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
mod tests;
