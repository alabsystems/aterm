// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db alab …` (docs/DESIGN-alab-lab-model-2026-11-01.md §0, §8, §9): `import`, `check`,
//! `adopt`, `roll`, `export`, `prefer` and `unprefer` over `alab/registry`, and the readers and
//! owner writers of `verbs` (`machines`, `accounts`, `preferences`, `mail`, `who`, `quota`, `log`,
//! `signins`, `add`, `retire`, `relabel`, `edit`). Every report names keys, columns, counts
//! and source coordinates, never a value; addresses are masked unless `--show-addresses` is
//! given. A sheet's note crosses into its row's `note` only past the note guard. The import's
//! review (`--review`: a private `$TMPDIR/.ubd/alab-import-*.noindex/` folder opened in amacs and
//! removed once the owner closes it) is the only place a withheld note's text or a losing value
//! is shown, and only on this Mac; a cell shaped like a credential, or one that names a secret, is
//! never shown even there.

use super::{
    Registry, checks,
    import::{
        self, Coord, Finding, Input, KnownMac, MailboxKeys, Plan, Sheet, SheetKind, Step, Vendors,
    },
    registry::{self, ACCOUNT, MACHINE, MAILBOX, PREFERENCE, REGISTRY},
};
use crate::{
    Error, Result,
    att::{self, Handle},
    machines::Label,
    typed::{
        Ty,
        cell::{self, CellKey, CellValue, Projection},
        grid, guard, keys,
    },
};
use aterm_json::{Map, Value, json};
use aterm_secrets::client::Paths;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
};

/// The verbs' help.
pub const USAGE: &str = "aterm db alab — the ALab lab model on Commons (Unix)\n\
  import [--vault-sheet <csv>] [--logins <csv>] [--fleet-accounts <toml>]... [--ai-pool <toml>]\n\
         [--dry-run|--apply [--first]] [--review] [--findings] [--show-addresses]\n\
                    plan the account sheets and the harness's fleet and pool files as registry\n\
                    writes: a dry run unless --apply. Only empty cells, or cells an import\n\
                    wrote, are written; the owner's edits stay and show as findings. A note\n\
                    crosses into its row's note unless it is shaped like a credential, says or\n\
                    reads like a secret or holds an address: then only its coordinates do. --review opens a\n\
                    private review in amacs that shows them (never a credential-shaped cell)\n\
                    and waits; it is removed once closed.\n\
                    --apply waits until this Mac holds the registry (a peer's repair round\n\
                    brings it); --first writes where no Mac in the lab holds it yet\n\
  check             preferences, labels, logins, aliases and mailboxes, then every conflict,\n\
                    dangling\n\
                    reference and unique clash, what this Mac's self-registration decides\n\
                    now and why (registration; its findings join lab), writing nothing, and\n\
                    whether this Mac's journal is on\n\
  adopt [--dry-run|--apply]\n\
                    bind the Macs the vault's swarm roster names to their machine rows, by fleet\n\
                    node, host, then label or alias; never births a row\n\
  prefer <machine> <account> [--rank <n>]\n\
                    this machine prefers this account (smaller ranks are tried first). A\n\
                    mapping, never a limit: any other live account can still run there\n\
  roll [--dry-run|--apply]\n\
                    restate the registry into its next generation (every register, conflicts\n\
                    open, every owned row as its signed record), seal the old one once every\n\
                    Writer runs a build that reads the next (`waiting` names the others), and\n\
                    release this Mac's storage of it once the covers carry what it holds; check\n\
                    asks for it at a spent quota or 75 % of the store. Run again on a Mac that\n\
                    edited the old generation before it heard the seal, it carries those edits\n\
  unprefer <machine> <account>\n\
                    end that preference; nobody is signed out. A machine is its label, an alias\n\
                    or its key; an account its label, legacy label or key, never its address\n\
  export [--dry-run|--apply]\n\
                    the files older Macs still read, rendered from the lab and its place rows,\n\
                    compared with the vault's and (--apply) published in one admin write\n\
  machines | accounts | preferences | mail\n\
                    the registry's rows by key and label; mail shows each address by key, the\n\
                    mailbox its codes arrive in (key and label) and how, never an address\n\
  machines <machine>\n\
                    what runs on it: its facts and model, its preferences by rank with each\n\
                    account's plan and quota, the accounts it runs unpreferred, its latest sum\n\
  who <account>     who last used it and on which Mac (its clock), from the journal, relays\n\
                    and the claims ring, and which Macs hold or claim it now\n\
  quota [<account>] each window's usage, reset, age, Mac and source; never a guessed 0\n\
  log <account|machine> [--days <n>]\n\
                    the journal's events about it (or that Mac logged), with this Mac's heard\n\
                    archive past the window; 14 days unless --days (at most 120)\n\
  signins           each account's latest sign-in word on each Mac\n\
  add machine <label> [--model <hw.model>] [--role <word>]\n\
                    a planned machine, until the Mac registers itself\n\
  add account <provider>\n\
                    an account for the address read from stdin (never the command line)\n\
  retire account|machine <name> [--why <text>]\n\
                    the row retired, the reason in its note; no login moves, nobody is signed out\n\
  relabel <machine> <new label>\n\
                    the new label, the old one kept as an alias\n\
  edit accounts|preferences|machines|mail\n\
                    the rows as a grid in amacs, saved cell by cell: a cell that changed\n\
                    meanwhile reopens with both values; a deleted row retires (a preference ends)\n\
  The sheets default to ~/.secrets/ai-accounts.csv and ~/ai-account-logins.csv, the harness's\n\
  files to ~/.secrets/fleet-accounts.toml, ai-accounts-<cli>.toml and ai-pool.toml, each read\n\
  when present. Addresses are masked unless --show-addresses is given.\n";

/// Where the importing Mac keeps its mailbox keys: private, beside its vault state.
pub const MAILBOX_KEYS: &str = "alab-mailbox-keys.json";

/// Opens a review file for the owner and returns once they have closed it: the review folder
/// lives only that long.
pub type Review<'a> = &'a mut dyn FnMut(&Path) -> Result<()>;

/// The production review opener: `aterm edit --wait <file>` from this same executable
/// ([`att::amacs`]), which inside aterm opens amacs in an Editor tab beside this one and
/// returns when the tab closes.
///
/// # Errors
/// This executable cannot be found or started, or amacs did not open the file.
pub fn amacs_wait(file: &Path) -> Result<()> {
    att::amacs(file, 1)
}

/// Run `aterm db alab <words>` on this Mac's Commons. `vendors` is the harness's reading of a
/// provider cell, which only the `aterm` executable links; without it `import` refuses.
pub fn main_entry(words: &[String], vendors: Option<&dyn Vendors>) -> ExitCode {
    if words.is_empty() || words.iter().any(|w| w == "--help" || w == "-h") {
        return aterm_uds::stdout::answer_exit("aterm db alab", USAGE, 0);
    }
    let paths = match Paths::live() {
        Ok(paths) => paths,
        Err(_) => {
            let _ = writeln!(std::io::stderr(), "aterm db alab: no vault paths here");
            return ExitCode::from(1);
        }
    };
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    match run(&paths, words, vendors, &mut amacs_wait, now_ms) {
        Ok(value) => match aterm_json::to_string(&value) {
            Ok(text) => aterm_uds::stdout::answer_exit("aterm db alab", &format!("{text}\n"), 0),
            Err(_) => ExitCode::from(1),
        },
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "aterm db alab: {error}");
            ExitCode::from(1)
        }
    }
}

#[derive(Default)]
struct Args {
    apply: bool,
    first: bool,
    dry_run: bool,
    review: bool,
    findings: bool,
    reveal: bool,
    vault: Option<PathBuf>,
    logins: Option<PathBuf>,
    fleets: Vec<PathBuf>,
    pool: Option<PathBuf>,
    rank: Option<u64>,
    /// `add machine --model <hw.model>`.
    model: Option<String>,
    /// `add machine --role <word>`.
    role: Option<String>,
    /// `retire … --why <text>`.
    why: Option<String>,
    /// `log … --days <n>`.
    days: Option<u64>,
    operands: Vec<String>,
}

/// What a verb reads from and opens beyond the Commons: the import's review opener, the grid
/// opener `edit` waits on, the stdin `add account` reads its address from, and this Mac's journal
/// spool, whose heard archive `log` reads (`None`: the window alone).
pub struct Io<'a> {
    pub review: Review<'a>,
    pub open: att::Opener<'a>,
    pub stdin: &'a mut dyn FnMut() -> Result<String>,
    pub heard: Option<super::journal::Spool>,
}

/// The first line of this process's stdin, at most 1 KiB: where `add account` reads its address.
///
/// # Errors
/// Stdin cannot be read or is not UTF-8.
fn stdin_line() -> Result<String> {
    first_line(&mut std::io::stdin().lock())
}

/// The first line `from` gives, at most 1 KiB, returned once its newline arrives (a terminal's
/// Enter), never waiting for end of input; end of input before a newline ends the line too.
fn first_line(from: &mut dyn std::io::BufRead) -> Result<String> {
    use std::io::{BufRead as _, Read as _};
    let mut bytes = Vec::new();
    from.take(1024)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| Error::new("stdin could not be read"))?;
    String::from_utf8(bytes).map_err(|_| Error::new("stdin is not UTF-8"))
}

/// A whole number an option takes.
fn whole(name: &str, raw: &std::ffi::OsStr) -> Result<u64> {
    let text = raw
        .to_str()
        .ok_or_else(|| Error::new(&format!("{name} is a whole number")))?;
    if text.is_empty()
        || (text.len() > 1 && text.starts_with('0'))
        || !text.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Error::new(&format!("{name} is a whole number")));
    }
    text.parse()
        .map_err(|_| Error::new(&format!("{name} is a whole number")))
}

/// The verbs `run` takes.
const VERBS: [&str; 19] = [
    "import",
    "check",
    "adopt",
    "prefer",
    "unprefer",
    "roll",
    "export",
    "machines",
    "accounts",
    "preferences",
    "mail",
    "who",
    "quota",
    "log",
    "signins",
    "add",
    "retire",
    "relabel",
    "edit",
];

fn parse(verb: &str, words: &[String]) -> Result<Args> {
    let mut args = Args::default();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        let mut value = |name: &str| {
            words
                .next()
                .map(PathBuf::from)
                .ok_or_else(|| Error::new(&format!("{name} needs a value")))
        };
        match (verb, word.as_str()) {
            ("import" | "adopt" | "roll" | "export", "--apply") => args.apply = true,
            ("import", "--first") => args.first = true,
            ("import" | "adopt" | "roll" | "export", "--dry-run") => args.dry_run = true,
            ("import", "--review") => args.review = true,
            ("import", "--no-review") => args.review = false,
            ("import" | "adopt", "--findings") => args.findings = true,
            (_, "--show-addresses") => args.reveal = true,
            ("import", "--vault-sheet") => {
                if args.vault.is_some() {
                    return Err(Error::new("duplicate --vault-sheet"));
                }
                args.vault = Some(value("--vault-sheet")?);
            }
            ("import", "--logins") => {
                if args.logins.is_some() {
                    return Err(Error::new("duplicate --logins"));
                }
                args.logins = Some(value("--logins")?);
            }
            ("import", "--fleet-accounts") => args.fleets.push(value("--fleet-accounts")?),
            ("import", "--ai-pool") => {
                if args.pool.is_some() {
                    return Err(Error::new("duplicate --ai-pool"));
                }
                args.pool = Some(value("--ai-pool")?);
            }
            ("prefer", "--rank") => {
                if args.rank.is_some() {
                    return Err(Error::new("duplicate --rank"));
                }
                args.rank = Some(whole("--rank", value("--rank")?.as_os_str())?);
            }
            ("log", "--days") => {
                if args.days.is_some() {
                    return Err(Error::new("duplicate --days"));
                }
                args.days = Some(whole("--days", value("--days")?.as_os_str())?);
            }
            ("add" | "retire", option @ ("--model" | "--role" | "--why")) => {
                let raw = value(option)?;
                let text = raw
                    .to_str()
                    .filter(|t| !t.is_empty())
                    .ok_or_else(|| Error::new(&format!("{option} needs a value")))?
                    .to_owned();
                let slot = match (verb, option) {
                    ("add", "--model") => &mut args.model,
                    ("add", "--role") => &mut args.role,
                    ("retire", "--why") => &mut args.why,
                    _ => {
                        return Err(Error::new(&format!(
                            "{verb}: unknown option (see `aterm db alab --help`)"
                        )));
                    }
                };
                if slot.replace(text).is_some() {
                    return Err(Error::new(&format!("duplicate {option}")));
                }
            }
            (
                "prefer" | "unprefer" | "machines" | "who" | "quota" | "log" | "add" | "retire"
                | "relabel" | "edit",
                operand,
            ) if !operand.starts_with("--") => {
                args.operands.push(operand.to_owned());
            }
            _ => {
                return Err(Error::new(&format!(
                    "{verb}: unknown {} (see `aterm db alab --help`)",
                    if word.starts_with("--") {
                        "option"
                    } else {
                        "operand"
                    }
                )));
            }
        }
    }
    if args.apply && args.dry_run {
        return Err(Error::new(&format!(
            "{verb}: --apply and --dry-run are opposites"
        )));
    }
    if args.apply && args.review {
        return Err(Error::new(&format!(
            "{verb}: --review goes with a dry run, before --apply"
        )));
    }
    if args.first && !args.apply {
        return Err(Error::new(&format!("{verb}: --first goes with --apply")));
    }
    Ok(args)
}

/// Run one verb against the Commons at `paths`, reading `now_ms` as this Mac's clock.
///
/// # Errors
/// The words are not a verb, a sheet cannot be read, or the registry cannot be read or written.
pub fn run(
    paths: &Paths,
    words: &[String],
    vendors: Option<&dyn Vendors>,
    review: Review<'_>,
    now_ms: u64,
) -> Result<Value> {
    let mut open = |file: &Path, line: usize| att::amacs(file, line);
    let mut stdin = stdin_line;
    let mut io = Io {
        review,
        open: &mut open,
        stdin: &mut stdin,
        heard: super::journal::Spool::local(),
    };
    run_io(paths, words, vendors, &mut io, now_ms)
}

/// [`run`] with what a verb reads and opens beyond the Commons given ([`Io`]).
///
/// # Errors
/// As [`run`].
pub fn run_io(
    paths: &Paths,
    words: &[String],
    vendors: Option<&dyn Vendors>,
    io: &mut Io<'_>,
    now_ms: u64,
) -> Result<Value> {
    let verb = words
        .first()
        .map(String::as_str)
        .ok_or_else(|| Error::new("missing alab verb"))?;
    if !VERBS.contains(&verb) {
        return Err(Error::new("unknown alab verb (see `aterm db alab --help`)"));
    }
    let args = parse(verb, &words[1..])?;

    match verb {
        "machines" => verbs::machines(paths, &args, io, now_ms),
        "accounts" => verbs::accounts(paths),
        "preferences" => verbs::preferences(paths),
        "mail" => verbs::mail(paths),
        "who" => verbs::who(paths, &args, io, now_ms),
        "quota" => verbs::quota(paths, &args, io, now_ms),
        "log" => verbs::log(paths, &args, io, now_ms),
        "signins" => verbs::signins(paths, io, now_ms),
        "add" => verbs::add(paths, &args, io),
        "retire" => verbs::retire(paths, &args),
        "relabel" => verbs::relabel(paths, &args),
        "edit" => verbs::edit(paths, &args, io),
        "import" => {
            let vendors = vendors.ok_or_else(|| {
                Error::new("the importer reads the sheets the harness's way; run it as `aterm db alab import`")
            })?;
            import(paths, &args, vendors, &mut *io.review, now_ms)
        }
        "check" => {
            let handle = Handle::at(paths.clone(), &att::REGISTRY);
            let (view, usage) = handle.usage()?;
            let (mut lab, generic) = checks::lab(&view.cells);
            lab.extend(checks::facts(
                view.owned
                    .get(registry::MACHINE_FACTS.name)
                    .unwrap_or(&Default::default()),
                &Registry::at(paths.clone()).live_uuids()?,
                &view.project(&MACHINE),
            ));
            lab.extend(checks::mailbox_obs(
                view.owned
                    .get(registry::MAILBOX_OBS.name)
                    .unwrap_or(&Default::default()),
                &view.project(&registry::ADDRESS),
                &view.project(&registry::MAILBOX),
            ));
            let registration = match vendors.and_then(|vendors| vendors.this_mac()) {
                None => Value::Null,
                Some(mac) => {
                    match Registry::at(paths.clone()).preview(&mac.config, &mac.probe, now_ms) {
                        Err(why) => json!({"error": why.to_string()}),
                        Ok(pass) => {
                            lab.extend(pass.findings);
                            let (outcome, machine) = pass.outcome.report();
                            json!({"outcome": outcome, "machine": machine})
                        }
                    }
                }
            };
            // This Mac's journal: off, on, or local with the reason its last flush was kept.
            let journal = super::journal::Spool::local().map(|spool| {
                super::journal::status(&spool, aterm_types::dirs::aterm_config_path().as_deref())
            });
            // The generated exports against what this Mac's `~/.secrets` holds (design §7).
            let exports = super::export::check(paths, vendors, now_ms / 1000);
            let table = handle.compiled().table;
            let mut findings: Vec<Value> = generic.iter().map(att::command::finding_json).collect();
            findings.extend(
                att::capacity(table, &view, &usage)
                    .iter()
                    .map(|found| att::command::capacity_json(table, found, &usage.actor)),
            );
            Ok(json!({
                "complete": view.complete,
                "refused": view.refused.iter().map(|id| aterm_codec::hex::encode_lower(id)).collect::<Vec<_>>(),
                "lab": lab.iter().map(|f| json!({
                    "finding": f.kind,
                    "collection": f.collection,
                    "pks": f.pks.iter().map(|pk| att::command::safe_key(pk)).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "findings": findings,
                "registration": registration,
                "journal": journal,
                "exports": exports,
            }))
        }
        "adopt" => adopt(paths, &args, now_ms),
        "roll" => roll(paths, &args, now_ms),
        "export" => super::export::run(paths, vendors, args.apply, now_ms / 1000),
        "prefer" | "unprefer" => prefer(paths, &args, verb == "prefer", now_ms),
        _ => Err(Error::new("unknown alab verb (see `aterm db alab --help`)")),
    }
}

/// The sheet at `explicit`, or the default file under `home` when it exists.
fn sheet(kind: SheetKind, explicit: Option<&Path>, default: PathBuf) -> Result<Option<Sheet>> {
    let path = match explicit {
        Some(path) => path.to_path_buf(),
        None if default.is_file() => default,
        None => return Ok(None),
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::new("a sheet's file name is not UTF-8"))?;
    let bytes = crate::import::read_bounded(&path)?;
    Sheet::parse(kind, name, &bytes).map(Some)
}

/// The non-denied Macs `roster` names: label, aliases and fleet node.
pub(super) fn known_macs_from_roster(
    roster: &aterm_secrets::swarm_roster::SwarmRoster,
) -> Vec<KnownMac> {
    roster
        .members
        .iter()
        .filter(|member| !roster.is_member_denied(member))
        .map(|member| KnownMac {
            label: Label::parse(&member.label).ok(),
            aliases: member
                .aliases
                .iter()
                .filter_map(|a| Label::parse(a).ok())
                .collect(),
            node: member
                .node_id
                .clone()
                .filter(|node| aterm_codec::hex::is_lower(node, 32)),
            hosts: Vec::new(),
        })
        .collect()
}

/// The Macs the vault's swarm roster names: label, aliases and fleet node.
fn known_macs(paths: &Paths) -> Vec<KnownMac> {
    let Ok(Some(roster)) = aterm_secrets::client::swarm_roster(paths) else {
        return Vec::new();
    };
    known_macs_from_roster(&roster)
}

/// The harness files this import reads: those named, else the ones beside the vault sheet that
/// exist. A file named and unreadable refuses the run; one found and unreadable is reported in
/// `unread`, and since its kind then counts as unread this run it retracts nothing and no
/// preference ends.
fn harness_sheets(
    paths: &Paths,
    args: &Args,
    vendors: &dyn Vendors,
) -> Result<(Vec<Sheet>, Unread)> {
    let secrets = paths.home.join(".secrets");
    let named = !args.fleets.is_empty() || args.pool.is_some();
    let files: Vec<(SheetKind, PathBuf)> = if named {
        args.fleets
            .iter()
            .map(|path| (SheetKind::FleetAccounts, path.clone()))
            .chain(
                args.pool
                    .iter()
                    .map(|path| (SheetKind::AiPool, path.clone())),
            )
            .collect()
    } else {
        [
            "fleet-accounts.toml",
            "ai-accounts-codex.toml",
            "ai-accounts-grok.toml",
            "ai-accounts-kimi.toml",
        ]
        .into_iter()
        .map(|name| (SheetKind::FleetAccounts, secrets.join(name)))
        .chain([(SheetKind::AiPool, secrets.join("ai-pool.toml"))])
        .filter(|(_, path)| path.is_file())
        .collect()
    };
    let mut sheets = Vec::new();
    let mut unread = Vec::new();
    for (kind, path) in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::new("a source's file name is not UTF-8"))?
            .to_owned();
        // Never the parser's own words, which may quote a row.
        let read = crate::import::read_bounded(&path).ok().and_then(|bytes| {
            let text = std::str::from_utf8(&bytes).ok()?;
            let rows = vendors.stated(kind, &name, text).ok()?;
            Sheet::stated(kind, &name, &bytes, &rows).ok()
        });
        match read {
            Some(sheet) => sheets.push(sheet),
            None if named => {
                return Err(Error::new(&format!(
                    "{name} is not a {} file this build reads",
                    kind.word()
                )));
            }
            None => unread.push((kind, name)),
        }
    }
    Ok((sheets, unread))
}

/// The harness files found and not read, each with its kind.
type Unread = Vec<(SheetKind, String)>;

/// The importer's mailbox keys ([`MAILBOX_KEYS`]) beside the vault state at `paths`, empty
/// where this Mac never imported.
///
/// # Errors
/// The file is there and is not one this build reads.
pub fn read_keys(paths: &Paths) -> Result<MailboxKeys> {
    match aterm_uds::private_file::read_private(&paths.dir, MAILBOX_KEYS, 1024 * 1024) {
        Ok(bytes) => {
            let value = aterm_json::from_slice(&bytes)
                .map_err(|_| Error::new("the mailbox key file is not JSON"))?;
            MailboxKeys::from_json(&value)
        }
        Err(aterm_uds::private_file::PrivateFileError::Missing) => Ok(MailboxKeys::default()),
        Err(_) => Err(Error::new("the mailbox key file cannot be read")),
    }
}

fn save_keys(paths: &Paths, keys: &MailboxKeys) -> Result<()> {
    let text = aterm_json::to_vec(&keys.to_json())
        .map_err(|_| Error::new("the mailbox keys could not be encoded"))?;
    aterm_types::fs_restricted::ensure_private_dir(&paths.dir)
        .map_err(|_| Error::new("the vault state folder could not be made private"))?;
    aterm_uds::private_file::write_private(&paths.dir, MAILBOX_KEYS, &text)
        .map_err(|_| Error::new("the mailbox keys could not be saved"))
}

fn import(
    paths: &Paths,
    args: &Args,
    vendors: &dyn Vendors,
    review: Review<'_>,
    now_ms: u64,
) -> Result<Value> {
    let mut sheets: Vec<Sheet> = [
        sheet(
            SheetKind::Vault,
            args.vault.as_deref(),
            paths.home.join(".secrets").join("ai-accounts.csv"),
        )?,
        sheet(
            SheetKind::Logins,
            args.logins.as_deref(),
            paths.home.join("ai-account-logins.csv"),
        )?,
    ]
    .into_iter()
    .flatten()
    .collect();
    let (harness, unread) = harness_sheets(paths, args, vendors)?;
    sheets.extend(harness);
    if sheets.is_empty() {
        return Err(Error::new(
            "no sheet to import: give --vault-sheet or --logins, or put the sheets in their places",
        ));
    }
    let macs = known_macs(paths);
    let keys = read_keys(paths)?;
    let unread_kinds: Vec<SheetKind> = unread.iter().map(|(kind, _)| *kind).collect();
    let unread: Vec<String> = unread.into_iter().map(|(_, name)| name).collect();
    let input = Input {
        sheets: &sheets,
        macs: &macs,
        vendors,
        keys: keys.clone(),
        now_ms,
        births: true,
        unread: &unread_kinds,
    };
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    if args.apply {
        // A Mac that holds none of the registry would plan every row as new, write concurrent
        // candidates over every owner edit and mint duplicate mailboxes. Checked before the
        // registry is declared here, and again under the writer.
        let unheld = || {
            Error::new(
                "this Mac holds none of the registry yet: import where it is held, let a peer's \
                 repair round bring it here, or pass --first where no Mac in the lab holds it",
            )
        };
        if !args.first && handle.view().map_or(true, |view| view.ops.is_empty()) {
            return Err(unheld());
        }
        // The compiled providers, CLIs and plans first, in records of their own under this
        // Mac's quota of automatic records, so every account's `provider` names a row. A seed
        // pass that cannot write (this Mac's automatic records are spent) leaves the import to
        // go ahead; any Mac's later pass fills the same absent cells.
        let seeded = Registry::at(paths.clone()).seed();
        let (plan, applied) = handle.commit_derived(true, |view| {
            if view.ops.is_empty() && !args.first {
                return Err(unheld());
            }
            let plan = import::plan(&input, &view.cells)?;
            // New mailbox keys beside mailbox rows this Mac has no keys for would be duplicate
            // mailboxes: those keys stay on the Mac that imported first (until step 11 keeps
            // them in the vault).
            let known = keys.pks();
            let births_mailbox = plan.keys.len() > keys.len()
                || plan
                    .counts
                    .get(MAILBOX.name)
                    .is_some_and(|c| c.new_rows > 0);
            if births_mailbox
                && view
                    .project(&MAILBOX)
                    .rows
                    .keys()
                    .any(|pk| !known.contains(pk.as_str()))
            {
                return Err(Error::new(
                    "the registry holds mailboxes whose keys are not on this Mac: import on the \
                     Mac that imported them",
                ));
            }
            if plan.keys != keys {
                // Before any record names a minted mailbox key, so a re-run finds it.
                save_keys(paths, &plan.keys)?;
            }
            let rows = plan.rows.clone();
            Ok((plan, rows))
        })?;
        let mut report = summary(&plan, &sheets, &macs, "held", args.findings);
        insert(&mut report, "dry_run", json!(false));
        insert(&mut report, "unread", json!(unread));
        insert(&mut report, "records", json!(applied.records));
        insert(&mut report, "written", json!(applied.cells));
        insert(
            &mut report,
            "retried",
            json!(applied.operations.len().saturating_sub(applied.records)),
        );
        match seeded {
            Ok(seeded) => insert(&mut report, "seeds", json!(seeded.records)),
            Err(error) => insert(&mut report, "seeds", json!(error.to_string())),
        }
        return Ok(report);
    }
    let (cells, registry) = match handle.view() {
        Ok(view) if view.complete => (view.cells, "held"),
        Ok(view) => (view.cells, "incomplete here"),
        Err(_) => (Default::default(), "not held here"),
    };
    let plan = import::plan(&input, &cells)?;
    let mut report = summary(&plan, &sheets, &macs, registry, args.findings);
    insert(&mut report, "dry_run", json!(true));
    insert(&mut report, "unread", json!(unread));
    let records = crate::att::pack(plan.rows.clone())?.len();
    insert(&mut report, "records", json!(records));
    let seeds = crate::att::pack(super::seeds::rows_for(&cells)?)?.len();
    insert(&mut report, "seeds", json!(seeds));
    if args.review {
        // The folder lives while the owner looks: dropping it removes it, however the review
        // ends, so no note's text is left behind in a temporary folder.
        let folder = write_review(&plan, &sheets, args.reveal)?;
        for file in ["findings.tsv", "planned.tsv"] {
            review(&folder.path().join(file))?;
        }
    }
    insert(&mut report, "review", json!(args.review));
    Ok(report)
}

/// `roll [--dry-run|--apply]` (design §3.8): restate the registry into its next generation, seal
/// the old one once no current Writer is `waiting` (has shown no build that reads the next), and
/// release this Mac's storage of it where the covers carry it. A dry run says what it would
/// write and writes nothing.
fn roll(paths: &Paths, args: &Args, now_ms: u64) -> Result<Value> {
    let rolled = Registry::at(paths.clone()).roll(args.apply, now_ms)?;
    let hex = |id: &[u8; 32]| aterm_codec::hex::encode_lower(id);
    let name = |generation: u32| {
        att::REGISTRY
            .chain()
            .into_iter()
            .find(|compiled| compiled.generation == generation)
            .map(att::Compiled::name)
    };
    Ok(json!({
        "applied": args.apply,
        "from": name(rolled.from),
        "to": name(rolled.to),
        "registers": rolled.registers,
        "archived": rolled.archived,
        "cover": rolled.cover,
        "records": rolled.records,
        "sealed": rolled.sealed,
        "waiting": rolled.waiting.iter().map(keys::uuid32).collect::<Vec<_>>(),
        "unread": rolled.unread,
        "released": rolled.released.released.iter().map(hex).collect::<Vec<_>>(),
        "kept": rolled.released.kept.iter().map(|(id, why)| json!({
            "table": hex(id), "why": why.word(),
        })).collect::<Vec<_>>(),
    }))
}

fn adopt(paths: &Paths, args: &Args, now_ms: u64) -> Result<Value> {
    struct NoSheets;
    impl Vendors for NoSheets {
        fn mark(&self, _: &str, _: &str, _: &str) -> import::Mark {
            import::Mark::Absent
        }
    }
    let macs = known_macs(paths);
    let input = Input {
        sheets: &[],
        macs: &macs,
        vendors: &NoSheets,
        keys: MailboxKeys::default(),
        now_ms,
        births: false,
        unread: &[],
    };
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    if args.apply {
        let (plan, applied) = handle.commit_derived(false, |view| {
            let plan = import::plan(&input, &view.cells)?;
            let rows = plan.rows.clone();
            Ok((plan, rows))
        })?;
        let mut report = summary(&plan, &[], &macs, "held", args.findings);
        insert(&mut report, "dry_run", json!(false));
        insert(&mut report, "records", json!(applied.records));
        insert(&mut report, "written", json!(applied.cells));
        return Ok(report);
    }
    let view = handle.view()?;
    let plan = import::plan(&input, &view.cells)?;
    let mut report = summary(&plan, &[], &macs, "held", args.findings);
    insert(&mut report, "dry_run", json!(true));
    insert(
        &mut report,
        "records",
        json!(crate::att::pack(plan.rows.clone())?.len()),
    );
    Ok(report)
}

/// The one row of `rows` whose text `matches`, by key first. `require_live` (`prefer`) refuses a
/// retired or conflicted row even when named by key, while `!require_live` (`unprefer`) tries live
/// rows first and falls back to retired rows so a lingering preference on a retired machine or
/// account can be ended by label or key.
fn named_row(
    what: &str,
    rows: &Projection,
    name: &str,
    require_live: bool,
    matches: impl Fn(&str, &cell::Row) -> bool,
) -> Result<String> {
    if let Some(row) = rows.rows.get(name) {
        if require_live && !row.live() {
            return Err(Error::new(&format!("no live {what} goes by that name")));
        }
        return Ok(name.to_owned());
    }
    let mut found: Vec<&String> = rows
        .rows
        .iter()
        .filter(|(pk, row)| (!require_live || row.live()) && matches(pk, row))
        .map(|(pk, _)| pk)
        .collect();
    if found.is_empty() && !require_live {
        found = rows
            .rows
            .iter()
            .filter(|(pk, row)| !row.live() && matches(pk, row))
            .map(|(pk, _)| pk)
            .collect();
    }
    let adjective = if require_live { "live " } else { "" };
    match found.as_slice() {
        [pk] => Ok((*pk).clone()),
        [] => Err(Error::new(&format!(
            "no {adjective}{what} goes by that name"
        ))),
        _ => Err(Error::new(&format!(
            "more than one {adjective}{what} goes by that name: name it by its key"
        ))),
    }
}

/// `prefer` and `unprefer`, the owner's word on one mapping: only the cells that change are
/// written, so a repeat writes no record. Ending a preference touches no login: nothing here
/// reaches a Mac's account directories.
fn prefer(paths: &Paths, args: &Args, on: bool, now_ms: u64) -> Result<Value> {
    let [machine, account] = args.operands.as_slice() else {
        return Err(Error::new("name a machine and an account"));
    };
    if account.contains('@') || machine.contains('@') {
        return Err(Error::new(
            "an account is named by its label, legacy label or key, never its address",
        ));
    }
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    let (report, applied) = handle.commit_derived(false, |view| {
        let wanted = Label::parse(machine).ok().map(|label| label.canonical());
        let machine = named_row("machine", &view.project(&MACHINE), machine, on, |_, row| {
            wanted.as_deref().is_some_and(|label| {
                row.get("label").and_then(Value::as_str) == Some(label)
                    || row
                        .fields
                        .get("aliases")
                        .is_some_and(|field| field.members().contains(label))
            })
        })?;
        let account = named_row(
            "account",
            &view.project(&ACCOUNT),
            account,
            on,
            |pk, row| {
                registry::account_label(pk, row).as_deref() == Some(account)
                    || row.get("legacy_label").and_then(Value::as_str) == Some(account)
            },
        )?;
        let pk = keys::preference_pk(&machine, &account)?;
        let projected = view.project(&PREFERENCE);
        let current = projected.rows.get(&pk);
        let live = current.is_some_and(cell::Row::live);
        let mut wanted: Vec<(&str, Value)> = Vec::new();
        if on {
            wanted.extend([("machine", json!(machine)), ("account", json!(account))]);
            if !live {
                wanted.extend([
                    ("since", json!(now_ms)),
                    (crate::typed::LIFECYCLE, json!("live")),
                ]);
            }
            if let Some(rank) = args.rank {
                wanted.push(("rank", json!(rank)));
            }
            // The owner's word makes the row theirs: a source dropping it no longer ends it.
            wanted.push(("why", json!("owner")));
        } else if current.is_none() {
            return Err(Error::new("this machine does not prefer that account"));
        } else if live {
            wanted.push((crate::typed::LIFECYCLE, json!("ended")));
        }
        let mut changes = Vec::new();
        for (column, x) in wanted {
            if current.and_then(|row| row.get(column)) == Some(&x) {
                continue;
            }
            changes.push(cell::change(
                &PREFERENCE,
                CellKey {
                    pk: &pk,
                    col: column,
                    elem: None,
                },
                &CellValue::owner(PREFERENCE.version, x),
            )?);
        }
        let report = json!({
            "preference": pk,
            "machine": machine,
            "account": account,
            "row": if on { "live" } else { "ended" },
        });
        let rows = if changes.is_empty() {
            Vec::new()
        } else {
            vec![changes]
        };
        Ok((report, rows))
    })?;
    let mut report = report;
    insert(&mut report, "written", json!(applied.cells));
    Ok(report)
}

fn insert(report: &mut Value, key: &str, value: Value) {
    if let Value::Object(object) = report {
        object.insert(key.to_owned(), value);
    }
}

fn at_json(at: &Coord, sheets: &[Sheet]) -> Value {
    match sheets.get(at.sheet) {
        Some(sheet) => json!({"file": sheet.name, "row": at.row, "col": at.col}),
        None => json!({"file": "swarm roster", "row": at.row}),
    }
}

/// Counts, finding kinds and sources: never a value.
fn summary(plan: &Plan, sheets: &[Sheet], macs: &[KnownMac], registry: &str, list: bool) -> Value {
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for finding in &plan.findings {
        *kinds.entry(finding.kind.word()).or_default() += 1;
    }
    let counts: Map = plan
        .counts
        .iter()
        .map(|(name, c)| {
            (
                (*name).to_owned(),
                json!({"rows": c.rows, "new": c.new_rows, "cells": c.cells,
                "unchanged": c.unchanged, "kept": c.kept}),
            )
        })
        .collect();
    let mut report = json!({
        "registry": registry,
        "sources": sheets.iter().map(|s| json!({"kind": s.kind.word(), "name": s.name, "rows": s.rows.len()})).collect::<Vec<_>>(),
        "known_macs": macs.len(),
        "counts": counts,
        "findings": kinds,
        "mailbox_keys": plan.keys.len(),
    });
    if list {
        let listed: Vec<Value> = plan
            .findings
            .iter()
            .map(|f: &Finding| {
                json!({"finding": f.kind.word(), "collection": f.collection, "pk": f.pk.as_deref().map(crate::att::command::safe_key),
                    "column": f.column, "at": f.at.iter().map(|a| at_json(a, sheets)).collect::<Vec<_>>()})
            })
            .collect();
        insert(&mut report, "finding_list", json!(listed));
    }
    report
}

/// What the review shows in place of a cell shaped like a credential or naming a secret.
pub const WITHHELD: &str = "(withheld: shaped like a secret)";

/// A sheet cell for the local review: every word holding `@` masked unless `reveal`; a cell
/// shaped like a credential or naming a secret is never shown, not even here.
fn review_text(text: &str, reveal: bool) -> String {
    if guard::withheld(text) {
        return WITHHELD.to_owned();
    }
    let flat: String = text
        .chars()
        .map(|c| if c == '\t' || c.is_control() { ' ' } else { c })
        .collect();
    if reveal {
        return flat;
    }
    flat.split(' ')
        .map(|word| {
            if word.contains('@') {
                grid::mask(word)
            } else {
                word.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn tsv(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .map(|c| {
                c.chars()
                    .map(|c| if c == '\t' || c.is_control() { ' ' } else { c })
                    .collect()
            })
            .collect();
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }
    out
}

/// The private review folder, removed when the returned handle drops: `findings.tsv` (each
/// finding with the text at its coordinates, the only place that text is shown, never for a
/// credential-shaped cell) and `planned.tsv` (each register the import would write, leave or
/// find unchanged).
fn write_review(plan: &Plan, sheets: &[Sheet], reveal: bool) -> Result<aterm_tempfile::TempDir> {
    let folder = aterm_tempfile::Builder::new()
        .prefix("alab-import-")
        .suffix(".noindex")
        .tempdir()
        .map_err(|_| Error::new("the review's private folder could not be made"))?;
    let mut findings = vec![
        [
            "finding",
            "collection",
            "pk",
            "column",
            "file",
            "row",
            "col",
            "text",
        ]
        .map(str::to_owned)
        .to_vec(),
    ];
    for finding in &plan.findings {
        let base = [
            finding.kind.word().to_owned(),
            finding.collection.unwrap_or_default().to_owned(),
            finding
                .pk
                .as_deref()
                .map_or("", crate::att::command::safe_key)
                .to_owned(),
            finding.column.clone().unwrap_or_default(),
        ];
        if finding.at.is_empty() {
            findings.push([base.to_vec(), vec![String::new(); 4]].concat());
        }
        for at in &finding.at {
            let (file, text) = match sheets.get(at.sheet) {
                // A credential-shaped cell is never read, so it is never shown either.
                Some(s) if finding.kind == import::Kind::CredentialShaped => {
                    (s.name.clone(), WITHHELD.to_owned())
                }
                Some(s) => (
                    s.name.clone(),
                    review_text(s.text(at).unwrap_or_default(), reveal),
                ),
                None if at.sheet == import::ROSTER => ("swarm roster".to_owned(), String::new()),
                None => (String::new(), String::new()),
            };
            findings.push(
                [
                    base.to_vec(),
                    vec![file, at.row.to_string(), at.col.to_string(), text],
                ]
                .concat(),
            );
        }
    }
    let mut planned = vec![
        ["collection", "pk", "column", "value", "step"]
            .map(str::to_owned)
            .to_vec(),
    ];
    for item in &plan.planned {
        let column = item.column.split('.').next().unwrap_or_default();
        let ty = REGISTRY
            .collection(item.collection)
            .and_then(|c| c.column(column))
            .map_or(Ty::Json(0), |c| c.ty);
        let shown = match (&ty, item.column.contains('.')) {
            (_, true) => item.column.split('.').nth(1).unwrap_or_default().to_owned(),
            (ty, false) => grid::show(ty, &item.x, reveal),
        };
        let step = match item.step {
            Step::Write => "write",
            Step::Unchanged => "unchanged",
            Step::Kept => "kept",
        };
        planned.push(vec![
            item.collection.to_owned(),
            crate::att::command::safe_key(&item.pk).to_owned(),
            item.column.clone(),
            shown,
            step.to_owned(),
        ]);
    }
    for (name, rows) in [("findings.tsv", &findings), ("planned.tsv", &planned)] {
        aterm_uds::private_file::write_private(folder.path(), name, tsv(rows).as_bytes())
            .map_err(|_| Error::new("the review could not be written"))?;
    }
    Ok(folder)
}

mod verbs;

#[cfg(test)]
mod tests;
