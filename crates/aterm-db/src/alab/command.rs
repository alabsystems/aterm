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
    import::{self, Coord, Finding, Input, KnownMac, Mode, Plan, Sheet, SheetKind, Step, Vendors},
    registry::{self, ACCOUNT, MACHINE, PHONE, PREFERENCE, REGISTRY},
};
use crate::{
    Error, Result,
    att::{self, Handle},
    machines::Label,
    typed::{
        Ty,
        cell::{self, CellKey, CellValue, Projection},
        grid, guard, keys,
        secret::{Sealer, Secret},
    },
};
use astream_wire::sync::Digest;
use aterm_json::{Map, Value, json};
use aterm_secrets::client::Paths;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
};

/// The verbs' help.
pub const USAGE: &str = "aterm db alab — the ALab lab model on Commons (Unix)\n\
  import [--vault-sheet <csv>] [--logins <csv>] [--fleet-accounts <toml>]... [--ai-pool <toml>]\n\
         [--emails|--computers|--phones <csv>|<xlsx>[#<tab>]] [--mailbox-toml <toml>]\n\
         [--fleet-db --authoritative|--fill-only]\n\
         [--dry-run|--apply] [--review] [--findings] [--show-addresses]\n\
                    plan the account sheets, this Mac's fleet_db replica (~/.aterm/lab-db, read\n\
                    in place; no dump is written), the harness's fleet and pool files and its\n\
                    mailbox file (~/.secrets/ai-mailbox.toml, only when named) as registry\n\
                    writes: a dry run unless --apply. A sheet is a CSV, or an xlsx workbook's\n\
                    tab, read in memory (`#<tab>` names it; else the tab the flag names:\n\
                    Emails, Computers, Phones). A sheet only fills: it\n\
                    writes empty cells and its own earlier import's, and a cell another import\n\
                    wrote otherwise is a `differs` finding, by key and column. The replica\n\
                    that holds the corrections is --authoritative, any other --fill-only; its\n\
                    passwords and phone numbers become Secret cells, sealed here, and a sheet's\n\
                    password column is only a flag, never one. After --apply the import judges\n\
                    each replica cell and records beside the replica whether the registry\n\
                    covers them all (`covered`, else each uncovered cell by key and column),\n\
                    with the replica's keyed digest; once covered, fleet_db's put, delete,\n\
                    load, bootstrap and rekey refuse there. The mailbox file's every field goes\n\
                    to its MAILBOX or login address cell, its passwords and TOTP seeds as Secret\n\
                    cells; a field with no column is a finding by name, and after --apply the\n\
                    report says whether the file is `covered`. The owner's edits stay and show as\n\
                    findings. A note\n\
                    crosses into its row's note unless it is shaped like a credential, says or\n\
                    reads like a secret or holds an address: then only its coordinates do. --review opens a\n\
                    private review in amacs that shows them (never a credential-shaped cell)\n\
                    and waits; it is removed once closed.\n\
                    --apply into a registry this Mac holds nothing of waits while a peer's\n\
                    recent repair round shows the lab holds it. It exits 1 where it wrote the\n\
                    rows but not the providers they name: run it again\n\
  import --fleet-db --digest\n\
                    the replica's keyed digest under the newest [everyone] epoch held here (its\n\
                    first 8 hex) and its row counts, so two holders compare replicas; never a\n\
                    value, and nothing is imported\n\
  import --compare-sheet <csv>\n\
                    what importing the sheet would do, by count, key and column, and how its\n\
                    password flags meet the registry's Secret cells: never a value\n\
  check             preferences, labels, logins, aliases and mailboxes, then every conflict,\n\
                    dangling\n\
                    reference and unique clash, what this Mac's self-registration decides\n\
                    now and why (registration; its findings join lab), writing nothing, and\n\
                    whether this Mac's journal is on\n\
  adopt [--dry-run|--apply]\n\
                    bind the Macs the vault's swarm roster names to their machine rows, by fleet\n\
                    node, host, then label or alias; never births a row\n\
  merge [--dry-run|--apply]\n\
                    fold each machine row that shares a name with a node-bound row into that\n\
                    row (merged_into): the Mac's current name is its label and the others its\n\
                    aliases, a cell only the duplicate holds moves, its preferences move. Prints\n\
                    keys, labels and columns, never a value; rows it cannot decide are held\n\
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
  machines | accounts | preferences | mail | phones\n\
                    the registry's rows by key and label; mail shows each address by key, the\n\
                    mailbox its codes arrive in (key and label) and how, never an address;\n\
                    phones shows each phone row by key, last4, carrier, line type and device\n\
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

/// Opens a review file for the owner and returns once they have closed it: the review folder
/// lives only that long.
pub type Review<'a> = &'a mut dyn FnMut(&Path) -> Result<()>;

/// The production review opener: `aterm edit --wait <file>` from this same executable
/// ([`att::amacs`]), which inside aterm opens amacs in an Editor tab beside this one and
/// returns when the tab closes. A review file names no concealed column: where it is private,
/// the window conceals every cell of it.
///
/// # Errors
/// This executable cannot be found or started, or amacs did not open the file.
pub fn amacs_wait(file: &Path) -> Result<()> {
    att::amacs(file, 1, None)
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
            Ok(text) => aterm_uds::stdout::answer_exit(
                "aterm db alab",
                &format!("{text}\n"),
                exit_of(&value),
            ),
            Err(_) => ExitCode::from(1),
        },
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "aterm db alab: {error}");
            ExitCode::from(1)
        }
    }
}

/// How long each of an import's passes waits and runs. A person runs an import once and waits on
/// it, so a product open's 2 s ([`crate::admission`]) is no measure of it: on a loaded Mac that
/// budget refused a seed pass in 25 of 123 sealed imports. A pass that still overruns this says
/// so, and running the import again resumes it, since each record's request id is its content's.
/// The empty-view guard reads this Mac's retained repair history, so the deadline bounds only
/// the passes that write.
const IMPORT_PASS: std::time::Duration = std::time::Duration::from_secs(60);

/// The key of the report's reason an import's seed pass wrote nothing.
const SEEDS_FAILED: &str = "seeds_failed";

/// Put what `seeded` did in an import's `report`. The import's rows are written by now whatever
/// this says, so a failure names what is missing and the step that writes it.
fn report_seeds(report: &mut Value, seeded: Result<crate::alab::Seeded>) {
    match seeded {
        Ok(seeded) => insert(report, "seeds", json!(seeded.records)),
        Err(error) => {
            insert(report, "seeds", json!(0));
            insert(
                report,
                SEEDS_FAILED,
                json!(format!(
                    "{error}: the rows are written, but the providers, CLIs and plans they name \
                     are not; a later pass fills them, so run this import again, or on another \
                     Mac where this one's quota is spent"
                )),
            );
        }
    }
}

/// The exit status of a report: 1 where an import wrote its rows but not what they name, so no
/// script or person takes that for a finished import.
fn exit_of(report: &Value) -> u8 {
    u8::from(!report[SEEDS_FAILED].is_null())
}

#[derive(Default)]
struct Args {
    apply: bool,
    dry_run: bool,
    review: bool,
    findings: bool,
    reveal: bool,
    vault: Option<PathBuf>,
    logins: Option<PathBuf>,
    computers: Option<PathBuf>,
    emails: Option<PathBuf>,
    phones: Option<PathBuf>,
    fleets: Vec<PathBuf>,
    pool: Option<PathBuf>,
    /// `import --mailbox-toml <file>`: the harness's mailbox file.
    mailbox_toml: Option<PathBuf>,
    /// `import --fleet-db`: this Mac's fleet_db replica, read in place, and the mode it is read
    /// in.
    fleet_db: bool,
    dump_mode: Option<Mode>,
    /// `import --fleet-db --digest`: the replica's keyed digest, importing nothing.
    digest: bool,
    /// `import --compare-sheet <csv>`.
    compare: Option<PathBuf>,
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
const VERBS: [&str; 21] = [
    "import",
    "check",
    "adopt",
    "merge",
    "prefer",
    "unprefer",
    "roll",
    "export",
    "machines",
    "accounts",
    "preferences",
    "mail",
    "phones",
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
            ("import" | "adopt" | "merge" | "roll" | "export", "--apply") => args.apply = true,
            ("import", flag @ ("--authoritative" | "--fill-only")) => {
                let mode = if flag == "--authoritative" {
                    Mode::Authoritative
                } else {
                    Mode::FillOnly
                };
                if args.dump_mode.replace(mode).is_some() {
                    return Err(Error::new(
                        "import: name the dump's mode once (--authoritative or --fill-only)",
                    ));
                }
            }
            ("import", "--fleet-db") => {
                if std::mem::replace(&mut args.fleet_db, true) {
                    return Err(Error::new("duplicate --fleet-db"));
                }
            }
            ("import", "--digest") => args.digest = true,
            ("import", "--compare-sheet") => {
                if args.compare.is_some() {
                    return Err(Error::new("duplicate --compare-sheet"));
                }
                args.compare = Some(value("--compare-sheet")?);
            }
            ("import" | "adopt" | "merge" | "roll" | "export", "--dry-run") => {
                args.dry_run = true;
            }
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
            ("import", "--computers") => {
                if args.computers.is_some() {
                    return Err(Error::new("duplicate --computers"));
                }
                args.computers = Some(value("--computers")?);
            }
            ("import", "--emails") => {
                if args.emails.is_some() {
                    return Err(Error::new("duplicate --emails"));
                }
                args.emails = Some(value("--emails")?);
            }
            ("import", "--phones") => {
                if args.phones.is_some() {
                    return Err(Error::new("duplicate --phones"));
                }
                args.phones = Some(value("--phones")?);
            }
            ("import", "--fleet-accounts") => args.fleets.push(value("--fleet-accounts")?),
            ("import", "--mailbox-toml") => {
                if args.mailbox_toml.is_some() {
                    return Err(Error::new("duplicate --mailbox-toml"));
                }
                args.mailbox_toml = Some(value("--mailbox-toml")?);
            }
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
    if args.digest {
        if !args.fleet_db
            || args.apply
            || args.review
            || args.dump_mode.is_some()
            || args.mailbox_toml.is_some()
        {
            return Err(Error::new(
                "import: --digest goes with --fleet-db alone; it prints the replica's keyed \
                 digest and imports nothing",
            ));
        }
    } else if args.fleet_db != args.dump_mode.is_some() {
        return Err(Error::new(
            "import: --fleet-db needs its mode named, --authoritative or --fill-only, and the \
             mode goes with --fleet-db",
        ));
    }
    if args.compare.is_some()
        && (args.apply || args.review || args.fleet_db || args.mailbox_toml.is_some())
    {
        return Err(Error::new(
            "import: --compare-sheet reports alone; it writes and reviews nothing",
        ));
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
    let mut open =
        |file: &Path, line: usize, secret: &[String]| att::amacs(file, line, Some(secret));
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
        "phones" => verbs::phones(paths),
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
            let (view, usage) = match handle.usage() {
                Ok(read) => read,
                Err(error) => {
                    return unread(paths, error, "aterm db alab import --dry-run, then --apply");
                }
            };
            // No import has run where no source row is: what `check` reads is the Macs' own rows.
            let imported = !view.project(&registry::SOURCE).rows.is_empty();
            // Two phone rows whose sealed numbers open to one number, by key only.
            let phones = view.project(&PHONE);
            let live_numbers = phones
                .rows
                .values()
                .filter(|row| row.live() && row.get("number").and_then(Value::as_str).is_some())
                .take(2)
                .count();
            let duplicates = if live_numbers >= 2 {
                handle
                    .with_sealer(|sealer| Ok(duplicate_numbers(&phones, sealer)))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
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
            // A later generation whose Secret tokens do not open under its key (an older build's
            // roll copied them): `roll::endorse` refuses it; named by id, never a value.
            findings.extend(
                Registry::at(paths.clone())
                    .unopened()
                    .unwrap_or_default()
                    .iter()
                    .map(unopened_json),
            );
            Ok(json!({
                "complete": view.complete,
                "refused": view.refused.iter().map(|id| aterm_codec::hex::encode_lower(id)).collect::<Vec<_>>(),
                "registry": if imported { "imported" } else { "not imported" },
                "next": (!imported).then_some("aterm db alab import --dry-run, then --apply"),
                "lab": lab.iter().map(|f| json!({
                    "finding": f.kind,
                    "collection": f.collection,
                    "pks": f.pks.iter().map(|pk| att::command::safe_key(pk)).collect::<Vec<_>>(),
                })).chain(duplicates.iter().map(|pks| json!({
                    "finding": "duplicate-number",
                    "collection": PHONE.name,
                    "pks": pks,
                }))).collect::<Vec<_>>(),
                "findings": findings,
                "registration": registration,
                "journal": journal,
                "exports": exports,
            }))
        }
        "adopt" => adopt(paths, &args, now_ms),
        "merge" => merge(paths, &args),
        "roll" => roll(paths, &args, now_ms),
        "export" => super::export::run(paths, vendors, args.apply, now_ms / 1000),
        "prefer" | "unprefer" => prefer(paths, &args, verb == "prefer", now_ms),
        _ => Err(Error::new("unknown alab verb (see `aterm db alab --help`)")),
    }
}

/// `check`'s finding for a later registry generation whose `Secret` tokens do not open under its
/// key (`roll::Registry::unopened`): its generation and the first 8 hex of its table digest,
/// how many cells, and whether this Mac endorsed it; never a value.
fn unopened_json(found: &super::roll::Unopened) -> Value {
    let id = aterm_codec::hex::encode_lower(&found.table);
    let short = id.get(..8).unwrap_or(&id);
    json!({
        "finding": "generation-unopened",
        "generation": found.generation,
        "table": id,
        "cells": found.cells,
        "endorsed": found.declared,
        "line": format!(
            "registry generation {} ({short}…) holds {} Secret cells whose token does not open \
             under its key: {}",
            found.generation,
            found.cells,
            if found.declared { "endorsed before this build refused it" } else { "not endorsed" },
        ),
    })
}

/// The live phone rows whose `number` cells open to one number, each group by key: two Macs
/// that imported one new phone at once. A token that does not open is no duplicate.
pub(super) fn duplicate_numbers(phones: &Projection, sealer: &dyn Sealer) -> Vec<Vec<String>> {
    let opened: Vec<(&String, Secret)> = phones
        .rows
        .iter()
        .filter(|(_, row)| row.live())
        .filter_map(|(pk, row)| {
            let token = row.get("number")?.as_str()?;
            Some((pk, sealer.open(PHONE.name, pk, "number", token).ok()?))
        })
        .collect();
    let mut grouped: Vec<Vec<String>> = Vec::new();
    let mut placed = vec![false; opened.len()];
    for (i, (pk, number)) in opened.iter().enumerate() {
        if placed[i] {
            continue;
        }
        let mut group = vec![(*pk).clone()];
        for (j, (other, same)) in opened.iter().enumerate().skip(i + 1) {
            if !placed[j] && number.same(same) {
                placed[j] = true;
                group.push((*other).clone());
            }
        }
        if group.len() > 1 {
            grouped.push(group);
        }
    }
    grouped
}

/// The sheet at `explicit`, or the default file under `home` when it exists. An explicit
/// `<file>.xlsx`, or `<file>.xlsx#<tab>`, is that workbook's tab ([`workbook`]).
fn sheet(kind: SheetKind, explicit: Option<&Path>, default: PathBuf) -> Result<Option<Sheet>> {
    let (path, tab) = match explicit {
        Some(path) => match workbook(path, kind) {
            Some((file, tab)) => (file, Some(tab)),
            None => (path.to_path_buf(), None),
        },
        None => {
            let Some(bytes) = crate::import::read_bounded_if_file(&default)? else {
                return Ok(None);
            };
            let name = default
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::new("a sheet's file name is not UTF-8"))?;
            return Sheet::parse(kind, name, &bytes).map(Some);
        }
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::new("a sheet's file name is not UTF-8"))?;
    match tab {
        Some(tab) => {
            let bytes = zeroize::Zeroizing::new(crate::import::read_within(
                &path,
                crate::import::MAX_WORKBOOK,
            )?);
            Sheet::xlsx(kind, name, &tab, &bytes).map(Some)
        }
        None => {
            let bytes = crate::import::read_bounded(&path)?;
            Sheet::parse(kind, name, &bytes).map(Some)
        }
    }
}

/// The workbook and tab an explicit sheet path names: `<file>.xlsx#<tab>`, or `<file>.xlsx`
/// alone for the tab named like the flag (`Emails` for `--emails`). A path that is itself a
/// file is read as it is named, so a `#` in a CSV's name stays part of it. `None` for a CSV.
fn workbook(path: &Path, kind: SheetKind) -> Option<(PathBuf, String)> {
    let xlsx = |file: &Path| {
        file.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("xlsx"))
    };
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
        || !path.to_str().is_some_and(|p| p.contains('#'))
    {
        return xlsx(path).then(|| (path.to_path_buf(), default_tab(kind).to_owned()));
    }
    let (file, tab) = path.to_str()?.rsplit_once('#')?;
    let file = PathBuf::from(file);
    (xlsx(&file) && !tab.trim().is_empty()).then(|| (file, tab.trim().to_owned()))
}

/// The tab a workbook named without one is read from, by the flag that named it.
const fn default_tab(kind: SheetKind) -> &'static str {
    match kind {
        SheetKind::Emails => "Emails",
        SheetKind::Computers => "Computers",
        SheetKind::Phones => "Phones",
        SheetKind::Logins => "Logins",
        _ => "Accounts",
    }
}

/// The harness's mailbox file at `path` as a source ([`import::mailbox_sheet`]), its bytes
/// zeroed once read.
fn mailbox_toml(path: &Path) -> Result<Sheet> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::new("the mailbox file's name is not UTF-8"))?;
    let bytes =
        zeroize::Zeroizing::new(crate::import::read_within(path, import::MAILBOX_TOML_MAX)?);
    import::mailbox_sheet(name, &bytes)
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
                .node_key
                .as_deref()
                .and_then(aterm_net::fleet::NodeKey::from_hex)
                .map(|key| aterm_net::fleet_member::node_principal(key)[2..].to_owned()),
            fleet_node: member
                .node_id
                .as_deref()
                .map(str::to_ascii_lowercase)
                .filter(|id| aterm_codec::hex::is_lower(id, 32)),
            hosts: Vec::new(),
        })
        .collect()
}

/// The Macs the vault's swarm roster names: label, aliases and fleet node. `None` when this Mac
/// holds no copy of the roster.
fn roster_macs(paths: &Paths) -> Result<Option<Vec<KnownMac>>> {
    let roster = aterm_secrets::client::swarm_roster_until(paths, crate::admission::deadline())
        .map_err(crate::table::failure)?;
    Ok(roster.as_ref().map(known_macs_from_roster))
}

/// The Macs the vault's swarm roster names, none where this Mac holds no copy of it.
fn known_macs(paths: &Paths) -> Result<Vec<KnownMac>> {
    Ok(roster_macs(paths)?.unwrap_or_default())
}

/// The harness files this import reads: those named, else the ones beside the vault sheet that
/// exist. A file named and unreadable refuses the run; one found and unreadable is reported in
/// `unread`, and since its kind then counts as unread this run it retracts nothing and no
/// preference ends. Each is a secrets file, so its source is keyed under the `[everyone]` key
/// (`Sheet::stated`): where this Mac holds none, a file is unreadable here, never digested plain.
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
        std::iter::once(aterm_secrets::entry_class::FLEET_ACCOUNTS.to_owned())
            .chain(["codex", "grok", "kimi"].map(aterm_secrets::entry_class::cli_accounts))
            .map(|name| (SheetKind::FleetAccounts, secrets.join(name)))
            .chain([(
                SheetKind::AiPool,
                secrets.join(aterm_secrets::entry_class::AI_POOL),
            )])
            .collect()
    };
    let everyone = everyone(paths);
    let mut sheets = Vec::new();
    let mut unread = Vec::new();
    for (kind, path) in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::new("a source's file name is not UTF-8"))?
            .to_owned();
        // A file found is read through a final link like a named one; one absent (a dangling
        // link included) or not a regular file is not there, and one there and unreadable is
        // reported in `unread`.
        let bytes = if named {
            crate::import::read_bounded(&path).ok()
        } else {
            match crate::import::read_bounded_if_file(&path) {
                Ok(None) => continue,
                Ok(Some(bytes)) => Some(bytes),
                Err(_) => None,
            }
        };
        // Never the parser's own words, which may quote a row.
        let read = everyone.as_ref().zip(bytes).and_then(|((_, key), bytes)| {
            let text = std::str::from_utf8(&bytes).ok()?;
            let rows = vendors.stated(kind, &name, text).ok()?;
            Sheet::stated(kind, &name, &bytes, &rows, key).ok()
        });
        match read {
            Some(sheet) => sheets.push(sheet),
            None if named && everyone.is_none() => {
                return Err(Error::new(&format!(
                    "this Mac holds no [everyone] key, so {name} cannot be recorded as a source"
                )));
            }
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

/// This Mac's fleet_db replica, read in place: its folder and its whole dump, passwords
/// included, zeroed when dropped.
struct Replica {
    dir: PathBuf,
    dump: zeroize::Zeroizing<Vec<u8>>,
    /// The records it holds and that dump leaves out ([`crate::fleet_db::withheld_operations`]).
    withheld: Vec<Digest>,
}

impl Replica {
    /// Its keyed digest under `key` ([`import::covered::replica_digest`]).
    fn digest(&self, key: &[u8; 32]) -> String {
        import::covered::replica_digest(&self.dump, &self.withheld, key)
    }
}

/// The name the replica's sources carry (`lab-db#emails`, …).
const REPLICA: &str = "lab-db";

/// The folder of this Mac's fleet_db replica: `ATERM_LAB_DB_DIR` where it is set, as fleet_db
/// reads it, else `~/.aterm/lab-db` under this Commons' home. A test binary reads only its
/// scratch home's, so a runner's `ATERM_LAB_DB_DIR` never leads a test to the person's replica.
pub(crate) fn replica_dir(paths: &Paths) -> PathBuf {
    let home = paths.home.join(".aterm").join("lab-db");
    if std::env::current_exe().is_ok_and(|exe| aterm_uds::test_binary::is_test_binary(&exe)) {
        return home;
    }
    std::env::var_os("ATERM_LAB_DB_DIR")
        .filter(|dir| !dir.is_empty())
        .map_or(home, PathBuf::from)
}

/// Read the replica at `dir` in place, through fleet_db's own read path (`dump_all`, in this
/// process): no dump file is written, and a folder that holds no replica is refused, never made
/// one.
///
/// # Errors
/// No replica is there, a rekey of it was cut short, its fleet key is not held here, or it cannot
/// be read.
fn read_replica(dir: &Path) -> Result<Replica> {
    // Ahead of the folder check: right after a rekey's rename the replica is in the kept folder.
    crate::fleet_db::refuse_unfinished(dir)?;
    // fleet_db's `local::CONFIG`: a folder without it holds no replica, and opening one would make
    // it.
    if !std::fs::symlink_metadata(dir.join("database.json")).is_ok_and(|meta| meta.is_file()) {
        return Err(Error::new(
            "this Mac holds no fleet_db replica to import (~/.aterm/lab-db)",
        ));
    }
    let db = crate::fleet_db::open_at(dir)?;
    let document = crate::fleet_db::dump_all(&db, true)?;
    let withheld = crate::fleet_db::withheld_operations(&db);
    drop(db);
    let dump = zeroize::Zeroizing::new(
        aterm_json::to_vec(&document)
            .map_err(|_| Error::new("the replica could not be read as a dump"))?,
    );
    Ok(Replica {
        dir: dir.to_owned(),
        dump,
        withheld,
    })
}

/// The newest `[everyone]` epoch this Mac holds, its id and key: what a keyed digest is keyed
/// by. `None` where this Mac holds none.
pub(crate) fn everyone(paths: &Paths) -> Option<(Digest, zeroize::Zeroizing<[u8; 32]>)> {
    #[cfg(test)]
    if let Some(held) = tests::EVERYONE.with(std::cell::Cell::get) {
        return Some((held.0, zeroize::Zeroizing::new(held.1)));
    }
    aterm_secrets::commons::everyone_key::newest(paths)
        .ok()
        .flatten()
        .map(|held| (held.epoch, held.key))
}

/// `import --fleet-db --digest`: the replica's keyed digest under the newest `[everyone]` epoch
/// this Mac holds, with its row counts, so two holders compare replicas by digest under the same
/// epoch. It imports nothing and prints no value.
///
/// # Errors
/// No replica is here, a rekey of it was cut short, or this Mac holds no `[everyone]` key.
fn replica_digest_report(paths: &Paths) -> Result<Value> {
    let replica = read_replica(&replica_dir(paths))?;
    let (epoch, key) = everyone(paths).ok_or_else(|| {
        Error::new("this Mac holds no [everyone] key, so the replica cannot be digested")
    })?;
    let document: Value = aterm_json::from_slice(&replica.dump)
        .map_err(|_| Error::new("the replica could not be read as a dump"))?;
    let rows: Map = crate::fleet_db::TABLES
        .iter()
        .map(|table| {
            let n = document
                .get(table)
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            ((*table).to_owned(), json!(n))
        })
        .collect();
    drop(document);
    let epoch = aterm_codec::hex::encode_lower(&epoch);
    Ok(json!({
        "replica": REPLICA,
        "epoch": &epoch[..8],
        "rows": rows,
        "withheld": replica.withheld.len(),
        "digest": replica.digest(&key),
    }))
}

/// Judge, after an `--apply`, whether the registry covers every cell of the replica it imported
/// ([`import::covered::judge`]), and record it beside the replica, with its keyed digest under the
/// newest `[everyone]` epoch this Mac holds. The report carries the same record.
///
/// # Errors
/// The registry cannot be read back, or the record cannot be written.
fn record_coverage(
    paths: &Paths,
    handle: &Handle,
    replica: &Replica,
    sheets: &[Sheet],
    vendors: &dyn Vendors,
    now_ms: u64,
) -> Result<Value> {
    let mut listed = uncovered(handle, sheets, vendors, now_ms, true)?;
    let count = listed.len() + replica.withheld.len();
    // No cell of the dump, and first, so the listing's cap cannot drop it.
    if !replica.withheld.is_empty() {
        listed.insert(
            0,
            import::covered::Uncovered {
                kind: "withheld-operations",
                collection: None,
                pk: None,
                column: None,
            },
        );
    }
    let held = everyone(paths);
    let record = import::covered::Record {
        covered: count == 0,
        uncovered: count,
        digest: held.as_ref().map(|(_, key)| replica.digest(key)),
        epoch: held
            .as_ref()
            .map(|(epoch, _)| aterm_codec::hex::encode_lower(epoch)),
        at_ms: now_ms,
    };
    import::covered::write(&replica.dir, &record, &listed)?;
    Ok(record.json(&listed))
}

/// The cells of `sheets` the registry does not cover, `sheets` planned alone against the
/// registry as an `--apply` left it ([`import::covered::judge`]). With `flags`, a flag-shaped
/// `Secret` value (a replica's has-password `1`) is covered where it agrees with whether the
/// registry's cell is set; without, it never is (a mailbox file's short value is a real, too
/// short credential).
///
/// # Errors
/// The registry cannot be read back.
fn uncovered(
    handle: &Handle,
    sheets: &[Sheet],
    vendors: &dyn Vendors,
    now_ms: u64,
    flags: bool,
) -> Result<Vec<import::covered::Uncovered>> {
    handle.view_sealed(|view, sealer| {
        let input = Input {
            sheets,
            macs: &[],
            vendors,
            sealer,
            now_ms,
            births: true,
            unread: &[],
        };
        let plan = import::plan(&input, &view.cells)?;
        let projections: BTreeMap<&str, Projection> = [
            &registry::ADDRESS,
            &registry::MAILBOX,
            &PHONE,
            &MACHINE,
            &ACCOUNT,
            &PREFERENCE,
        ]
        .into_iter()
        .map(|collection| (collection.name, view.project(collection)))
        .collect();
        let set = |collection: &str, pk: &str, column: &str| {
            projections
                .get(collection)
                .and_then(|p| p.rows.get(pk))
                .and_then(|row| row.get(column))
                .is_some_and(|value| !value.is_null())
        };
        let flag_agrees = |finding: &Finding| {
            let (Some(collection), Some(pk), Some(column)) =
                (finding.collection, &finding.pk, &finding.column)
            else {
                return false;
            };
            let held = set(collection, pk, column);
            flags
                && !finding.at.is_empty()
                && finding
                    .at
                    .iter()
                    .all(|at| flag_at(sheets, at) == Some(held))
        };
        Ok(import::covered::judge(&plan, &flag_agrees))
    })
}

/// What the replica's `Secret` value at `at` says as a flag word
/// ([`crate::typed::secret::flag_says`]); `None` where it is no flag word or is not there.
fn flag_at(sheets: &[Sheet], at: &Coord) -> Option<bool> {
    let col = at.col.checked_sub(1)?;
    let value = sheets.get(at.sheet)?.secrets.get(&(at.row, col))?;
    crate::typed::secret::flag_says(value)
}

/// How recent a peer's repair round must be to speak for the lab now.
pub(super) const FRESH_MS: u64 = 15 * 60 * 1000;

/// Why an import into a registry this Mac holds nothing of must wait, read from the node
/// service's latest repair round with each peer it knows (`peers`, `None` for a peer with no
/// round retained) (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.7, §8 step 2). Only a round within
/// [`FRESH_MS`] of `now_ms` speaks, over the registry's `table` (any table where this Mac cannot
/// name it) or over this Mac's `catalog`. The guard waits only on what a peer reported: a round
/// over the registry that received records (a failed round's count is a lower bound) or deferred
/// some says the lab holds what this Mac has not heard yet; one that exchanged and brought nothing
/// says that peer holds nothing more. The caller passes `catalog` only where this Mac's catalog
/// holds no declaration of the registry by another Mac: only an import declares it, it does so
/// before it writes a row, and a declaration rides the catalog, which every exchange relays, so a
/// Mac that holds rows is heard of through any peer it exchanged with. A round over the catalog
/// that exchanged and brought nothing says that peer holds no declaration this Mac lacks; one that
/// deferred catalog records may have held one back; one that brought records said nothing this
/// read has weighed. A failed round is no word: in a lab of dozens of Macs some peer's round
/// fails on every pass (unreachable, an old build, a busy disk), so waiting on any failure is a
/// livelock, and what that peer holds reaches this Mac through the peers that exchange. Repair
/// visits only tables some Mac holds, so before the lab's first import no round over the registry
/// ever comes: without the catalog's word that import would wait for good wherever a peer is
/// known. Where no peer's round speaks at all, whether the lab holds the registry is unknown: an
/// import now could write concurrent candidates over every row and mint a second key for every
/// phone, so it waits too. `None` where no peer is known or a round that speaks found nothing to
/// bring.
pub(super) fn empty_view_wait<'r>(
    table: Option<[u8; 32]>,
    catalog: Option<[u8; 32]>,
    peers: impl IntoIterator<Item = Option<&'r aterm_types::network_state::RepairObservation>>,
    now_ms: u64,
) -> Option<&'static str> {
    use aterm_types::network_state::RepairOutcome;
    let mut known = false;
    let mut drained = false;
    for round in peers {
        known = true;
        let Some(round) =
            round.filter(|round| now_ms.saturating_sub(round.completed_at_ms) <= FRESH_MS)
        else {
            continue;
        };
        let deferred = matches!(round.outcome, RepairOutcome::Deferred(_))
            || round.deferred.is_some_and(|n| n > 0);
        let exchanged = matches!(
            round.outcome,
            RepairOutcome::Exchanged | RepairOutcome::NoCommonTable
        );
        let received = round.received.is_some_and(|n| n > 0);
        let over_registry = match (table, round.table) {
            (Some(wanted), Some(seen)) => wanted == seen,
            _ => true,
        };
        if over_registry {
            if received {
                return Some(
                    "a peer's repair round just brought registry records that this Mac has not applied yet",
                );
            }
            if deferred {
                return Some(
                    "a peer's repair round deferred registry records: the lab holds what this Mac has not heard",
                );
            }
            drained |= exchanged;
        } else if catalog.is_some() && round.table == catalog {
            if deferred {
                return Some(
                    "a peer's repair round deferred catalog records: one may declare the registry",
                );
            }
            drained |= exchanged && !received;
        }
    }
    (known && !drained).then_some(
        "no peer's repair round over the registry or the catalog in the last 15 minutes says whether the lab holds it",
    )
}

/// What a read of the registry that failed answers: "not imported", with `next`, only where this
/// Mac's catalog declares no registry (or there is no Commons here). A declared registry's failed
/// read (a sync racing it past its deadline, an expired pass on a loaded Mac, damaged storage) is
/// that error, never a call to import, and so is one whose catalog cannot be read either (E1).
///
/// # Errors
/// `error`, unless this Mac's catalog says it declares no registry.
pub(super) fn unread(paths: &Paths, error: Error, next: &str) -> Result<Value> {
    match Registry::at(paths.clone()).declared() {
        Ok(false) => Ok(json!({
            "registry": "not imported",
            "why": error.to_string(),
            "next": next,
        })),
        Ok(true) | Err(_) => Err(error),
    }
}

/// What this Mac's catalog says for an import into a registry it holds nothing of, read once
/// before the writer ([`Registry::catalog_word`]): the catalog's table id where no other Mac's
/// declaration of the registry is in it, so its rounds speak ([`empty_view_wait`]).
///
/// # Errors
/// Another Mac declared the registry here (the lab holds it), or the catalog cannot be read.
fn empty_view_catalog(paths: &Paths, ops: usize) -> Result<Option<[u8; 32]>> {
    if ops > 0 {
        return Ok(None);
    }
    match Registry::at(paths.clone()).catalog_word(std::time::Instant::now() + IMPORT_PASS) {
        Ok(Some((_, true))) => Err(Error::new(
            "this Mac holds none of the registry yet and another Mac declared it here, so the lab \
             holds it; import once its repair round brings it",
        )),
        Ok(Some((catalog, false))) => Ok(Some(catalog)),
        Ok(None) => Ok(None),
        Err(_) => Err(Error::new(
            "this Mac holds none of the registry and cannot read whether its catalog declares it",
        )),
    }
}

/// [`empty_view_wait`] over this Mac's retained network history, for a registry it holds
/// `ops` operations of, with the catalog [`empty_view_catalog`] read.
///
/// # Errors
/// The history is there and cannot be read: whether the lab holds the registry is unknown.
fn empty_view_guard(
    paths: &Paths,
    ops: usize,
    catalog: Option<[u8; 32]>,
    now_ms: u64,
) -> Result<()> {
    if ops > 0 {
        return Ok(());
    }
    let Some(config) = aterm_types::dirs::aterm_config_path() else {
        return Ok(());
    };
    let network = crate::network_state::read_history(&config).map_err(|_| {
        Error::new("this Mac holds none of the registry and cannot read whether its peers do")
    })?;
    let table = Registry::at(paths.clone()).table().ok().flatten();
    let peers = network.peers.iter().map(|peer| peer.last_repair.as_ref());
    match empty_view_wait(table, catalog, peers, now_ms) {
        Some(why) => Err(Error::new(&format!(
            "this Mac holds none of the registry yet and {why}; import once its repair round brings it"
        ))),
        None => Ok(()),
    }
}

/// The sealer a dry run plans with where this Mac holds no registry key: it seals nothing (the
/// plan is never written, each sealed cell a well-formed token no key opens) and opens nothing.
struct Unheld;

/// The token [`Unheld`] seals every value to: an authenticated cell's shape, holding nothing.
fn unheld_token() -> String {
    "sec1:0000000000000000000000000000000000000000000000000000000000000000:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==".to_owned()
}

impl Sealer for Unheld {
    fn table(&self) -> &str {
        ""
    }

    fn seal(&self, _: &str, _: &str, _: &str, _: &Secret) -> Result<String> {
        Ok(unheld_token())
    }

    fn open(&self, _: &str, _: &str, _: &str, _: &str) -> Result<Secret> {
        Err(Error::new("this Mac holds no registry key"))
    }
}

/// What one import reads but the registry's sealer.
struct Sources<'a> {
    sheets: &'a [Sheet],
    macs: &'a [KnownMac],
    vendors: &'a dyn Vendors,
    now_ms: u64,
    unread: &'a [SheetKind],
}

impl Sources<'_> {
    fn input<'b>(&'b self, sealer: &'b dyn Sealer) -> Input<'b> {
        Input {
            sheets: self.sheets,
            macs: self.macs,
            vendors: self.vendors,
            sealer,
            now_ms: self.now_ms,
            births: true,
            unread: self.unread,
        }
    }
}

/// The plan of `sources` against this Mac's registry, writing nothing, with how much of it is
/// held here and the seed records a write would add.
///
/// # Errors
/// The plan fails against the registry this Mac holds; one it cannot open (no key here) is
/// planned against an empty registry instead.
fn dry_plan(handle: &Handle, sources: &Sources<'_>) -> Result<(Plan, &'static str, usize)> {
    let (plan, registry, seeds, _) = dry_plan_with(handle, sources, |_| ())?;
    Ok((plan, registry, seeds))
}

fn dry_plan_with<T>(
    handle: &Handle,
    sources: &Sources<'_>,
    extra: impl FnOnce(&crate::att::View) -> T,
) -> Result<(Plan, &'static str, usize, Option<T>)> {
    let planned = |view: &crate::att::View,
                   sealer: &dyn Sealer,
                   extra: &mut Option<T>|
     -> Result<(Plan, &'static str, usize)> {
        let plan = import::plan(&sources.input(sealer), &view.cells)?;
        let seeds = crate::att::pack(super::seeds::rows_for(&view.cells)?)?.len();
        let _ = extra;
        Ok((
            plan,
            if view.complete {
                "held"
            } else {
                "incomplete here"
            },
            seeds,
        ))
    };
    let mut out_extra = None;
    match handle.view_sealed(|view, sealer| {
        let res = planned(view, sealer, &mut out_extra);
        out_extra = Some(extra(view));
        Ok(res)
    }) {
        Ok(planned) => {
            let (plan, registry, seeds) = planned?;
            Ok((plan, registry, seeds, out_extra))
        }
        Err(_) => {
            let cells = Default::default();
            let plan = import::plan(&sources.input(&Unheld), &cells)?;
            let seeds = crate::att::pack(super::seeds::rows_for(&cells)?)?.len();
            Ok((plan, "not held here", seeds, None))
        }
    }
}

/// Whether `plan` writes nothing and finds nothing the registry holds otherwise or keeps from
/// the sources: the sources say what the registry says.
fn matches_registry(plan: &Plan) -> bool {
    use import::Kind;
    plan.rows.is_empty()
        && !plan.findings.iter().any(|f| {
            matches!(
                f.kind,
                Kind::Differs
                    | Kind::OwnerKept
                    | Kind::Conflicted
                    | Kind::RetiredKept
                    | Kind::DumpConflicted
                    | Kind::ProtectedSecret
                    | Kind::Unmapped
            )
        })
}

/// The `differs` findings by key and column: never a value.
fn differs_json(plan: &Plan) -> Vec<Value> {
    plan.findings
        .iter()
        .filter(|f| f.kind == import::Kind::Differs)
        .map(|f| {
            json!({
                "collection": f.collection,
                "pk": f.pk.as_deref().map(crate::att::command::safe_key),
                "column": f.column,
            })
        })
        .collect()
}

fn import(
    paths: &Paths,
    args: &Args,
    vendors: &dyn Vendors,
    review: Review<'_>,
    now_ms: u64,
) -> Result<Value> {
    if let Some(csv) = &args.compare {
        return compare_sheet(paths, csv, vendors, now_ms);
    }
    if args.digest {
        return replica_digest_report(paths);
    }
    let mut sheets: Vec<Sheet> = [
        sheet(
            SheetKind::Vault,
            args.vault.as_deref(),
            paths
                .home
                .join(".secrets")
                .join(aterm_secrets::entry_class::AI_ACCOUNTS_CSV),
        )?,
        sheet(
            SheetKind::Phones,
            args.phones.as_deref(),
            paths
                .home
                .join(".secrets")
                .join(aterm_secrets::entry_class::ALAB_PHONES),
        )?,
        sheet(
            SheetKind::Computers,
            args.computers.as_deref(),
            paths
                .home
                .join(".secrets")
                .join(aterm_secrets::entry_class::ALAB_COMPUTERS),
        )?,
        sheet(
            SheetKind::Emails,
            args.emails.as_deref(),
            paths
                .home
                .join(".secrets")
                .join(aterm_secrets::entry_class::ALAB_EMAILS),
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
    let replica = match args.dump_mode.filter(|_| args.fleet_db) {
        Some(mode) => {
            let replica = read_replica(&replica_dir(paths))?;
            // The dump holds passwords: its source is keyed under the `[everyone]` key, never a
            // plain digest in the shared `source` row (`import::dump_sheets`).
            let (_, key) = everyone(paths).ok_or_else(|| {
                Error::new(
                    "this Mac holds no [everyone] key, so the replica's dump cannot be recorded \
                     as a source",
                )
            })?;
            let read = import::dump_sheets(REPLICA, &replica.dump, mode, &key)?;
            let at = sheets.len()..sheets.len() + read.len();
            sheets.extend(read);
            Some((replica, at))
        }
        None => None,
    };
    let (harness, unread) = harness_sheets(paths, args, vendors)?;
    sheets.extend(harness);
    let mailboxes = match &args.mailbox_toml {
        Some(path) => {
            sheets.push(mailbox_toml(path)?);
            Some(sheets.len() - 1)
        }
        None => None,
    };
    if sheets.is_empty() {
        return Err(Error::new(
            "no sheet to import: give --vault-sheet, --logins, --mailbox-toml or --fleet-db, or \
             put the sheets in their places",
        ));
    }
    let macs = known_macs(paths)?;
    let unread_kinds: Vec<SheetKind> = unread.iter().map(|(kind, _)| *kind).collect();
    let unread: Vec<String> = unread.into_iter().map(|(_, name)| name).collect();
    let sources = Sources {
        sheets: &sheets,
        macs: &macs,
        vendors,
        now_ms,
        unread: &unread_kinds,
    };
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    if args.apply {
        // A Mac that holds none of the registry while the lab does would plan every row as new
        // and write concurrent candidates over every owner edit. Checked before the registry is
        // declared here, and again under the writer.
        let held = handle.view().map_or(0, |view| view.ops.len());
        let catalog = empty_view_catalog(paths, held)?;
        empty_view_guard(paths, held, catalog, now_ms)?;
        // The compiled providers, CLIs and plans first, in records of their own under this
        // Mac's quota of automatic records, so every account's `provider` names a row. A seed
        // pass that cannot write (this Mac's automatic records are spent) leaves the import to
        // go ahead, since any Mac's later pass fills the same absent cells; the report and the
        // exit status say so ([`report_seeds`]).
        let seeded =
            Registry::at(paths.clone()).seed_until(std::time::Instant::now() + IMPORT_PASS);
        let pass = std::time::Instant::now() + IMPORT_PASS;
        let (plan, applied) = handle.commit_sealed_until(true, pass, |view, sealer| {
            empty_view_guard(paths, view.ops.len(), catalog, now_ms)?;
            let mut plan = import::plan(&sources.input(sealer), &view.cells)?;
            let rows = std::mem::take(&mut plan.rows);
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
        if let Some((replica, at)) = &replica {
            let record = record_coverage(
                paths,
                &handle,
                replica,
                &sheets[at.clone()],
                vendors,
                now_ms,
            )?;
            insert(&mut report, "replica", record);
        }
        if let Some(at) = mailboxes {
            let uncovered = uncovered(&handle, &sheets[at..=at], vendors, now_ms, false)?;
            insert(
                &mut report,
                "mailbox_toml",
                import::covered::report(&uncovered),
            );
        }
        report_seeds(&mut report, seeded);
        return Ok(report);
    }
    let (mut plan, registry, seeds) = dry_plan(&handle, &sources)?;
    let mut report = summary(&plan, &sheets, &macs, registry, args.findings);
    let matches = matches_registry(&plan);
    insert(&mut report, "dry_run", json!(true));
    insert(&mut report, "unread", json!(unread));
    if let Some((replica, _)) = &replica {
        insert(
            &mut report,
            "replica_withheld",
            json!(replica.withheld.len()),
        );
    }
    let records = crate::att::pack(std::mem::take(&mut plan.rows))?.len();
    insert(&mut report, "records", json!(records));
    insert(&mut report, "seeds", json!(seeds));
    insert(&mut report, "matches_registry", json!(matches));
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

/// `import --compare-sheet <csv>`: what the sheet's fill-only import would do, counted and keyed
/// by row key and column, and how its password flags meet the registry's `address.password`
/// cells (set, held, missing). It writes nothing and prints no value: no cell, no address.
fn compare_sheet(paths: &Paths, csv: &Path, vendors: &dyn Vendors, now_ms: u64) -> Result<Value> {
    let name = csv
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::new("a sheet's file name is not UTF-8"))?;
    let bytes = crate::import::read_bounded(csv)?;
    let sheet = Sheet::parse_detected(name, &bytes)?;
    let kind = sheet.kind;
    let (flagged, set, unkeyed) = sheet.password_flags();
    let sheets = [sheet];
    let macs = known_macs(paths)?;
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    let sources = Sources {
        sheets: &sheets,
        macs: &macs,
        vendors,
        now_ms,
        unread: &[],
    };
    let (plan, registry, _, addresses) =
        dry_plan_with(&handle, &sources, |view| view.project(&registry::ADDRESS))?;
    let addresses = match addresses {
        Some(addresses) => addresses,
        None => handle
            .view()
            .map(|view| view.project(&registry::ADDRESS))
            .unwrap_or_default(),
    };
    // Each flagged address's row, as the import matches it: by key, else by what a row says now.
    // One several rows say names none of them, and counts as missing.
    let index = registry::AddressIndex::of(&addresses);
    let mut unmatched = 0;
    let flagged: std::collections::BTreeSet<String> = flagged
        .iter()
        .filter_map(|address| match index.matched(address) {
            Ok(registry::AddressHolder::Row(pk)) => Some(pk.to_owned()),
            Ok(registry::AddressHolder::Free { derived, .. }) => Some(derived),
            Ok(registry::AddressHolder::Several(_)) | Err(_) => {
                unmatched += 1;
                None
            }
        })
        .collect();
    let sealed = |pk: &String| {
        addresses
            .rows
            .get(pk)
            .is_some_and(|row| row.live() && row.get("password").is_some_and(|x| !x.is_null()))
    };
    let held = flagged.iter().filter(|pk| sealed(pk)).count();
    let unflagged = addresses
        .rows
        .iter()
        .filter(|(pk, _)| sealed(pk) && !flagged.contains(*pk))
        .count();
    let mut report = summary(&plan, &sheets, &[], registry, false);
    insert(&mut report, "kind", json!(kind.word()));
    insert(
        &mut report,
        "matches_registry",
        json!(matches_registry(&plan)),
    );
    insert(&mut report, "differs", json!(differs_json(&plan)));
    insert(
        &mut report,
        "password_flags",
        json!({
            "set": set,
            "sealed_in_registry": held,
            "missing_in_registry": flagged.len() - held + unmatched,
            "no_address": unkeyed,
            "sealed_unflagged": unflagged,
        }),
    );
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
    let macs = known_macs(paths)?;
    let input = Input {
        sheets: &[],
        macs: &macs,
        vendors: &NoSheets,
        sealer: &Unheld,
        now_ms,
        births: false,
        unread: &[],
    };
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    if args.apply {
        let (plan, applied) = handle.commit_derived(false, |view| {
            let mut plan = import::plan(&input, &view.cells)?;
            let rows = std::mem::take(&mut plan.rows);
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
        json!(crate::att::pack(plan.rows)?.len()),
    );
    Ok(report)
}

/// `merge`: the registry's duplicate machine rows folded into their node-bound rows, planned over
/// what this Mac holds; `--apply` plans again under the writer.
/// What `merge` would do over `view`: the machines any Mac's own facts row names move only by it.
pub(super) fn merge_plan(
    view: &att::View,
    roster: &super::merge::Roster,
) -> Result<super::merge::Plan> {
    let facts = view.owned.get(registry::MACHINE_FACTS.name);
    let bound: BTreeSet<String> = facts
        .into_iter()
        .flat_map(|facts| facts.rows.values())
        .flat_map(|row| ["mac", "merged_into"].map(|column| row.value.get(column)))
        .filter_map(|cell| cell.and_then(Value::as_str).map(str::to_owned))
        .collect();
    super::merge::plan(
        &view.project(&MACHINE),
        &view.project(&PREFERENCE),
        &bound,
        roster,
    )
}

fn merge(paths: &Paths, args: &Args) -> Result<Value> {
    // Which name is a Mac's current one, and whether two names are two Macs, is the roster's word:
    // without it a merge would fold what it would hold and keep names it would change.
    let roster: super::merge::Roster = roster_macs(paths)?
        .ok_or_else(|| Error::new("merge reads the roster, and this Mac holds no copy of it"))?
        .iter()
        .filter_map(|mac| {
            let label = mac.label.as_ref()?.canonical();
            let stated = [("node", &mac.node), ("fleet_node", &mac.fleet_node)]
                .into_iter()
                .filter_map(|(column, node)| Some((column, node.clone()?)))
                .collect();
            Some((label, stated))
        })
        .collect();
    let planned = |view: &att::View| merge_plan(view, &roster);
    let cells = |rows: &[Vec<crate::database::Change>]| rows.iter().map(Vec::len).sum::<usize>();
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    if args.apply {
        let (mut report, applied) = handle.commit_owner(|view| {
            let mut plan = planned(view)?;
            let rows = std::mem::take(&mut plan.rows);
            Ok((plan.report(), rows))
        })?;
        insert(&mut report, "dry_run", json!(false));
        insert(&mut report, "records", json!(applied.records));
        insert(&mut report, "written", json!(applied.cells));
        return Ok(report);
    }
    let view = handle.view()?;
    let plan = planned(&view)?;
    let mut report = plan.report();
    insert(&mut report, "dry_run", json!(true));
    insert(&mut report, "complete", json!(view.complete));
    insert(&mut report, "cells", json!(cells(&plan.rows)));
    insert(
        &mut report,
        "records",
        json!(crate::att::pack(plan.rows)?.len()),
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
                        .is_some_and(|field| field.has_member(label))
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
/// shaped like a credential, naming a secret or holding a phone number (a `Secret` cell's
/// value) is never shown, not even here.
fn review_text(text: &str, reveal: bool) -> String {
    if guard::withheld(text) || guard::phone_shaped(text) {
        return WITHHELD.to_owned();
    }
    let flat: String = text
        .chars()
        .map(|c| if c == '\t' || c.is_control() { ' ' } else { c })
        .collect();
    if reveal || !flat.contains('@') {
        return flat;
    }
    let mut out = String::with_capacity(flat.len());
    for (i, word) in flat.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if word.contains('@') {
            out.push_str(&grid::mask(word));
        } else {
            out.push_str(word);
        }
    }
    out
}

fn tsv(rows: &[Vec<String>]) -> String {
    let cap: usize = rows
        .iter()
        .map(|row| row.iter().map(|cell| cell.len() + 1).sum::<usize>() + 1)
        .sum();
    let mut out = String::with_capacity(cap);
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.push('\t');
            }
            for c in cell.chars() {
                out.push(if c == '\t' || c.is_control() { ' ' } else { c });
            }
        }
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
    let mut findings = Vec::with_capacity(1 + plan.findings.len());
    findings.push(
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
    );
    for finding in &plan.findings {
        let base = [
            finding.kind.word(),
            finding.collection.unwrap_or_default(),
            finding
                .pk
                .as_deref()
                .map_or("", crate::att::command::safe_key),
            finding.column.as_deref().unwrap_or_default(),
        ];
        if finding.at.is_empty() {
            findings.push(vec![
                base[0].to_owned(),
                base[1].to_owned(),
                base[2].to_owned(),
                base[3].to_owned(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ]);
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
            findings.push(vec![
                base[0].to_owned(),
                base[1].to_owned(),
                base[2].to_owned(),
                base[3].to_owned(),
                file,
                at.row.to_string(),
                at.col.to_string(),
                text,
            ]);
        }
    }
    let mut planned = Vec::with_capacity(1 + plan.planned.len());
    planned.push(
        ["collection", "pk", "column", "value", "step"]
            .map(str::to_owned)
            .to_vec(),
    );
    for item in &plan.planned {
        let shown = match item.column.split_once('.') {
            Some((_, elem)) => elem.to_owned(),
            None => {
                let ty = REGISTRY
                    .collection(item.collection)
                    .and_then(|c| c.column(&item.column))
                    .map_or(Ty::Json(0), |c| c.ty);
                grid::show(&ty, &item.x, reveal)
            }
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

#[cfg(unix)]
pub(crate) mod session;
mod verbs;
pub(crate) mod views;

#[cfg(test)]
mod tests;
