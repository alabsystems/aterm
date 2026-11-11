// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm name`: this machine's name, its NAME (`m8`, or `unnumbered` until it is numbered) and nickname
//! (`docs/DESIGN-machine-names-2026-11-04.md` §5.7), made by the first read that finds none
//! ([`crate::machine_name::ensure`]). `aterm help name` is its page.

use std::ffi::OsString;
use std::process::ExitCode;

/// `aterm name --help`.
pub const USAGE: &str = "\
aterm name — this machine's name: its NAME (`m8`, or `unnumbered` until it is numbered) and nickname

  aterm name [--json]
  aterm name set <mN|N> [--nick <word>] [--dry-run] [--json]
  aterm name nick <word> | --random  [--dry-run] [--json]

  `aterm name` prints `m8 Heron  class=seeded fleet=in node=<node id>` (`fleet=` is in,
  joining, out, or unread where the vault's state cannot be read), or with `--json` one
  object of its fields; the first run to find no `machine-name.toml` makes it. `set` chooses
  this machine's number, optionally its nickname in the same change; a number another signed
  node currently holds is refused. A successful set also records the explicit naming policy
  and publishes the checked fleet name. On macOS the existing setup authorization changes all three system names,
  generated from the saved number and nickname. Missing authorization stays pending; this
  command does not open a password prompt.
  `nick` sets the
  nickname to <word> (one argument: quote `Blue Heron`) or to a noun drawn again, as a chosen
  nickname, and prints the name as it then stands; `--dry-run` changes no nickname (a first
  run still makes the name, as any read does). `aterm help name` has the page. Unix only for
  now.
";

/// What `aterm name …` asks for, decided without running it so a test can read it.
#[derive(Debug, PartialEq, Eq)]
enum Mode {
    /// `-h` or `--help`: the usage, exit 0.
    Help,
    /// The name, as a line or (`--json`) one object.
    Show { json: bool },
    /// `set`: choose the number and optionally a nickname in one change.
    Set { to: Name, dry_run: bool, json: bool },
    /// `nick`: set the nickname, then show the name as `Show` does.
    Nick { to: Nick, dry_run: bool, json: bool },
}

/// What `aterm name nick` sets the nickname to.
#[derive(Debug, PartialEq, Eq)]
enum Nick {
    /// This word.
    Word(aterm_types::machine_name::Nickname),
    /// `--random`: a noun drawn again.
    Random,
}

/// The portable arguments of `name set`; the Unix adapter supplies its storage API.
#[derive(Debug, PartialEq, Eq)]
struct Name {
    num: aterm_types::machine_name::Number,
    nick: Option<aterm_types::machine_name::Nickname>,
}

fn parse(argv: &[OsString]) -> Result<Mode, String> {
    let mut words = Vec::with_capacity(argv.len());
    for arg in argv {
        words.push(arg.to_str().ok_or("an argument is not UTF-8")?);
    }
    if words.first() == Some(&"nick") {
        return parse_nick(&words[1..]);
    }
    if words.first() == Some(&"set") {
        return parse_set(&words[1..]);
    }
    let mut json = false;
    for word in words {
        match word {
            "-h" | "--help" => return Ok(Mode::Help),
            "--json" if json => return Err("`--json` is given twice".to_owned()),
            "--json" => json = true,
            "nick" => return Err("`nick` comes first: `aterm name nick …`".to_owned()),
            arg => return Err(format!("`{arg}` is no `aterm name` argument")),
        }
    }
    Ok(Mode::Show { json })
}

fn parse_set(words: &[&str]) -> Result<Mode, String> {
    use aterm_types::machine_name::{Nickname, Number};
    if words.iter().any(|word| matches!(*word, "-h" | "--help")) {
        return Ok(Mode::Help);
    }
    let (mut num, mut nick, mut dry_run, mut json) = (None, None, false, false);
    let mut words = words.iter().copied();
    while let Some(word) = words.next() {
        let flag = match word {
            "--dry-run" => &mut dry_run,
            "--json" => &mut json,
            "--nick" => {
                if nick.is_some() {
                    return Err("`--nick` is given twice".to_owned());
                }
                let word = words.next().ok_or("`--nick` needs a nickname")?;
                nick = Some(
                    Nickname::parse(word)
                        .map_err(|why| format!("`{word}` is no nickname: {why}"))?,
                );
                continue;
            }
            arg if arg.starts_with('-') => {
                return Err(format!("`{arg}` is no `aterm name set` argument"));
            }
            _ => {
                if num.is_some() {
                    return Err("`aterm name set` takes one number".to_owned());
                }
                num = Some(
                    Number::parse(word)
                        .map_err(|why| format!("`{word}` is no machine number: {why}"))?,
                );
                continue;
            }
        };
        if std::mem::replace(flag, true) {
            return Err(format!("`{word}` is given twice"));
        }
    }
    Ok(Mode::Set {
        to: Name {
            num: num.ok_or("`aterm name set` takes a machine number")?,
            nick,
        },
        dry_run,
        json,
    })
}

fn parse_nick(words: &[&str]) -> Result<Mode, String> {
    use aterm_types::machine_name::Nickname;
    if words.iter().any(|word| matches!(*word, "-h" | "--help")) {
        return Ok(Mode::Help);
    }
    let (mut to, mut dry_run, mut json) = (None, false, false);
    for &word in words {
        let flag = match word {
            "--dry-run" => &mut dry_run,
            "--json" => &mut json,
            _ => {
                let next = if word == "--random" {
                    Nick::Random
                } else if word.starts_with('-') {
                    return Err(format!("`{word}` is no `aterm name nick` argument"));
                } else {
                    Nick::Word(
                        Nickname::parse(word)
                            .map_err(|why| format!("`{word}` is no nickname: {why}"))?,
                    )
                };
                if to.replace(next).is_some() {
                    return Err("`aterm name nick` takes one word or `--random`".to_owned());
                }
                continue;
            }
        };
        if std::mem::replace(flag, true) {
            return Err(format!("`{word}` is given twice"));
        }
    }
    let to = to.ok_or("`aterm name nick` takes a word or `--random`")?;
    Ok(Mode::Nick { to, dry_run, json })
}

/// `aterm name …`: `argv[1..]` in, exit code out (2 for a usage error or a platform that keeps no
/// machine name, 1 for a name that cannot be read, made or changed).
pub fn main_entry(argv: Vec<OsString>) -> ExitCode {
    match parse(&argv) {
        Ok(Mode::Help) => aterm_uds::stdout::answer_exit("aterm name", USAGE, 0),
        Ok(Mode::Show { json }) => show(json),
        Ok(Mode::Set { to, dry_run, json }) => set(&to, dry_run, json),
        Ok(Mode::Nick { to, dry_run, json }) => nick(&to, dry_run, json),
        Err(why) => {
            eprintln!("aterm name: {why}\n(`aterm name --help` says what it takes)");
            ExitCode::from(2)
        }
    }
}

/// `aterm name set`: aterm.toml's naming policy is read first (a dry run's too), so a naming
/// hold or an aterm.toml that cannot be edited changes no name; then the set; then, for a real
/// change, its publication ([`crate::machine_name::publish::publish`]), whose first unfinished
/// step is reported as publication pending.
#[cfg(unix)]
fn set(to: &Name, dry_run: bool, json: bool) -> ExitCode {
    use crate::machine_name::publish;
    let Some(state) = aterm_types::dirs::state_dir() else {
        eprintln!("aterm name: no state directory: $HOME is not set");
        return ExitCode::from(1);
    };
    let policy = aterm_types::dirs::aterm_config_path()
        .ok_or_else(|| "no aterm configuration path".to_owned())
        .and_then(|config| publish::Policy::read(&config, to.num).map(|policy| (policy, config)));
    let (policy, config) = match policy {
        Ok(read) => read,
        Err(why) => {
            eprintln!("aterm name: {why}; no name changed");
            return ExitCode::from(1);
        }
    };
    let to = crate::machine_name::NameTo {
        num: to.num,
        nick: to.nick.clone(),
    };
    let vault = crate::machine_name::standing_vault();
    let ask = unix::NameAsk {
        to: &to,
        kind: caller(session_status().as_deref()),
        dry_run,
    };
    match unix::rename(
        &state,
        vault.as_ref(),
        &ask,
        json,
        crate::machine_name::vault_roster_label,
        aterm_netplace::unix_ms_now(),
    ) {
        Ok((text, notes)) => {
            #[cfg(target_os = "macos")]
            let system: Option<&dyn publish::SystemNames> = Some(&SystemNaming);
            #[cfg(not(target_os = "macos"))]
            let system: Option<&dyn publish::SystemNames> = None;
            if !dry_run && let Err(pending) = publish::publish(&state, &config, &policy, system) {
                eprintln!(
                    "aterm name: {} was requested; publication pending: {}; inspect the current name before retrying",
                    to.num,
                    pending.why()
                );
                return ExitCode::from(1);
            }
            for note in notes {
                eprintln!("aterm name: {}", note.text(&state));
            }
            aterm_uds::stdout::answer_exit("aterm name", &text, 0)
        }
        Err(why) => {
            eprintln!("aterm name: {why}");
            ExitCode::from(1)
        }
    }
}

/// CLI and menu publication share the application's existing noninteractive rule.
#[cfg(target_os = "macos")]
struct SystemNaming;

#[cfg(target_os = "macos")]
impl crate::machine_name::publish::SystemNames for SystemNaming {
    fn read(&self) -> [Option<String>; 3] {
        crate::machine_name::publish::read_system_names()
    }

    fn apply(
        &self,
        state: &std::path::Path,
        config: &std::path::Path,
        names: &aterm_types::machine_name::GeneratedNames,
    ) -> Result<(), String> {
        crate::machine_name::publish::apply_registered(state, config, names)
    }
}

#[cfg(not(unix))]
fn set(_to: &Name, _dry_run: bool, _json: bool) -> ExitCode {
    show(false)
}

#[cfg(unix)]
/// Who asks for a change from this command (§5.7): an agent when it runs in an aterm session whose
/// `status` names an agent program, or reads an agent's state there (`agent=`, which aterm sets
/// for one it told by its screen, such as Claude Code run by `node`); a person otherwise, and
/// where that status cannot be read. A command a person types through an agent (Claude Code's
/// `!`) is the agent's.
fn caller(status: Option<&str>) -> aterm_net::fleet_member::name_record::ByKind {
    use crate::supervise::screen::status_field;
    use aterm_net::fleet_member::name_record::ByKind;
    let Some(reply) = status else {
        return ByKind::Person;
    };
    let program = status_field(reply, "program").map(aterm_codec::pct::decode);
    if program.is_some_and(|program| aterm_phase::program_of(&program).is_some())
        || status_field(reply, "agent").is_some()
    {
        ByKind::Agent
    } else {
        ByKind::Person
    }
}

#[cfg(unix)]
/// How long this command waits for its session's `status` (§5.7's `by`), seconds: a session
/// whose window does not answer costs the caller's kind, never the change.
const STATUS_WAIT_S: &str = "2";

#[cfg(unix)]
/// This session's `status` reply, where this command runs in an aterm session, within
/// [`STATUS_WAIT_S`].
fn session_status() -> Option<String> {
    std::env::var_os("ATERM_SESSION_ID")?;
    let exe = crate::own_exe().ok()?;
    crate::CtlClient::own_at(exe, None)
        .run(&["--timeout", STATUS_WAIT_S, "@self", "status"])
        .ok()
}

#[cfg(unix)]
fn show(json: bool) -> ExitCode {
    let Some(state) = aterm_types::dirs::state_dir() else {
        eprintln!("aterm name: no state directory: $HOME is not set");
        return ExitCode::from(1);
    };
    let vault = crate::machine_name::standing_vault();
    let roster = crate::machine_name::vault_roster_label;
    match unix::report(
        &state,
        vault.as_ref(),
        json,
        roster,
        aterm_netplace::unix_ms_now(),
    ) {
        Ok((text, narrowed)) => {
            if let Some(was) = narrowed {
                eprintln!(
                    "aterm name: {} was open to other users ({was:o}); it is 0700 now",
                    state.display()
                );
            }
            aterm_uds::stdout::answer_exit("aterm name", &text, 0)
        }
        Err(why) => {
            eprintln!("aterm name: {why}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(unix))]
fn show(_json: bool) -> ExitCode {
    eprintln!("aterm name: this platform keeps no machine name yet");
    ExitCode::from(2)
}

#[cfg(unix)]
fn nick(to: &Nick, dry_run: bool, json: bool) -> ExitCode {
    use crate::machine_name::NickTo;
    let to = match to {
        Nick::Word(word) => NickTo::Word(word.clone()),
        Nick::Random => NickTo::Random,
    };
    let Some(state) = aterm_types::dirs::state_dir() else {
        eprintln!("aterm name: no state directory: $HOME is not set");
        return ExitCode::from(1);
    };
    let policy = (|| {
        let current = crate::machine_name::file::read(&state).map_err(|why| why.to_string())?;
        let Some(number) = current
            .filter(|name| {
                name.class == aterm_types::machine_name::Class::Chosen
                    && name.migration != Some(crate::machine_name::file::Migration::Waiting)
            })
            .and_then(|name| name.num)
        else {
            return Ok(None);
        };
        let config = aterm_types::dirs::aterm_config_path().ok_or("no aterm configuration path")?;
        let policy = crate::machine_name::publish::Policy::read(&config, number)?;
        Ok::<_, String>(Some((config, policy)))
    })();
    let policy = match policy {
        Ok(policy) => policy,
        Err(why) => {
            eprintln!("aterm name: {why}; no nickname changed");
            return ExitCode::from(1);
        }
    };
    let vault = crate::machine_name::standing_vault();
    let kind = caller(session_status().as_deref());
    let roster = crate::machine_name::vault_roster_label;
    let ask = unix::NickAsk {
        to: &to,
        kind,
        dry_run,
    };
    match unix::renick(
        &state,
        vault.as_ref(),
        &ask,
        json,
        roster,
        aterm_netplace::unix_ms_now(),
    ) {
        Ok((text, notes)) => {
            if !dry_run && let Some((config, policy)) = &policy {
                #[cfg(target_os = "macos")]
                let system: Option<&dyn crate::machine_name::publish::SystemNames> =
                    Some(&SystemNaming);
                #[cfg(not(target_os = "macos"))]
                let system: Option<&dyn crate::machine_name::publish::SystemNames> = None;
                if let Err(pending) =
                    crate::machine_name::publish::publish(&state, config, policy, system)
                {
                    eprintln!(
                        "aterm name: the nickname was saved; name synchronization pending: {}",
                        pending.why()
                    );
                    return ExitCode::from(1);
                }
            }
            for note in notes {
                eprintln!("aterm name: {}", note.text(&state));
            }
            aterm_uds::stdout::answer_exit("aterm name", &text, 0)
        }
        Err(why) => {
            eprintln!("aterm name: {why}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(unix))]
fn nick(_to: &Nick, _dry_run: bool, _json: bool) -> ExitCode {
    show(false)
}

#[cfg(unix)]
mod unix {
    use std::io;
    use std::path::Path;

    use aterm_net::fleet_member::read_node_identity;

    use aterm_secrets::client::Paths;

    use crate::machine_name::file::{MachineName, class_word, migration_word, nick_class_word};
    use crate::machine_name::{InFleet, Origin, ensure_in_view};

    /// The report of this machine's name in `state`, made first where no file is there
    /// ([`ensure_in_view`], whose first-name sources are the registry, roster and legacy name;
    /// a read of a name already made opens no naming source), with the
    /// mode `state` was narrowed from when it was open to others. Whether it is in a fleet is
    /// [`InFleet::read`] over `state` and the vault at `vault`, where one is read.
    /// An unreadable identity is journaled and leaves the node unknown beside a valid name.
    ///
    /// # Errors
    /// `state` cannot be made private, or the name cannot be read or made ([`ensure_in_view`]).
    pub(super) fn report(
        state: &Path,
        vault: Option<&Paths>,
        json: bool,
        roster: impl FnOnce() -> io::Result<Option<String>>,
        now_ms: u64,
    ) -> Result<(String, Option<u32>), String> {
        let narrowed =
            aterm_uds::private_file::ensure_private_dir(state).map_err(|e| e.to_string())?;
        let (name, origin) = ensure_in_view(state, roster, now_ms).map_err(|e| e.to_string())?;
        let me = report_node(state);
        let shown = Shown {
            name: &name,
            origin,
            fleet: InFleet::read(state, vault),
            node: me.as_ref().map(|me| me.node()),
        };
        Ok((if json { shown.json() } else { shown.line() }, narrowed))
    }

    /// Optional report metadata, never minted or repaired here. Setters validate the identity
    /// before changing the name; a later metadata read cannot turn a committed edit into a
    /// reported failure.
    fn report_node(state: &Path) -> Option<aterm_net::fleet::NodeIdentity> {
        match read_node_identity(state) {
            Ok(me) => me,
            Err(why) => {
                aterm_log::info!(
                    "machine name: node identity unavailable ({why}); reporting the local name"
                );
                None
            }
        }
    }

    /// What `aterm name nick` says beside the name, on stderr.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Note {
        /// The state directory was open to others, in this mode, and is 0700 now.
        Narrowed(u32),
        /// The nickname, in display form, was held already as chosen.
        Already(String),
        /// This exact number and optional nickname were held already.
        NameAlready,
        /// Another machine in view goes by this nickname too.
        Shared(String),
        /// A dry run: no nickname changed.
        DryRun,
        /// A rename preview changed nothing.
        NameDryRun,
    }

    impl Note {
        pub(super) fn text(&self, state: &Path) -> String {
            match self {
                Self::Narrowed(was) => format!(
                    "{} was open to other users ({was:o}); it is 0700 now",
                    state.display()
                ),
                Self::Already(nick) => format!("the nickname is {nick} already"),
                Self::NameAlready => "the name is already as requested".to_owned(),
                Self::Shared(nick) => format!("{nick} is another machine's nickname too"),
                Self::DryRun => "a dry run: no nickname changed".to_owned(),
                Self::NameDryRun => "a dry run: no name changed".to_owned(),
            }
        }
    }

    /// What `aterm name nick` asks for: the nickname, the kind of caller that asks, and whether
    /// it only says what would change.
    pub(super) struct NickAsk<'a> {
        pub(super) to: &'a crate::machine_name::NickTo,
        pub(super) kind: aterm_net::fleet_member::name_record::ByKind,
        pub(super) dry_run: bool,
    }

    /// What `aterm name set` asks for: the number and nickname, the kind of caller that asks,
    /// and whether it only says what would change.
    pub(super) struct NameAsk<'a> {
        pub(super) to: &'a crate::machine_name::NameTo,
        pub(super) kind: aterm_net::fleet_member::name_record::ByKind,
        pub(super) dry_run: bool,
    }

    /// Choose a number and optional nickname, then report the resulting or previewed name.
    /// Whether it is in a fleet is [`InFleet::read`] over `state` and the vault at `vault`,
    /// where one is read.
    ///
    /// # Errors
    /// As [`report`]; the requested change cannot be validated or written ([`crate::machine_name::set_name`]).
    pub(super) fn rename(
        state: &Path,
        vault: Option<&Paths>,
        ask: &NameAsk<'_>,
        json: bool,
        roster: impl FnOnce() -> io::Result<Option<String>>,
        now_ms: u64,
    ) -> Result<(String, Vec<Note>), String> {
        let NameAsk { to, kind, dry_run } = *ask;
        let narrowed =
            aterm_uds::private_file::ensure_private_dir(state).map_err(|e| e.to_string())?;
        let lab = || crate::machine_name::lab_labels(vault);
        let set = crate::machine_name::set_name(state, to, lab, kind, roster, now_ms, dry_run)
            .map_err(|e| e.to_string())?;
        let me = report_node(state);
        let mut notes: Vec<Note> = narrowed.into_iter().map(Note::Narrowed).collect();
        if !set.changed {
            notes.push(Note::NameAlready);
        }
        if set.shared {
            notes.push(Note::Shared(set.name.display_nick()));
        }
        if dry_run {
            notes.push(Note::NameDryRun);
        }
        let shown = Shown {
            name: &set.name,
            origin: set.origin,
            fleet: InFleet::read(state, vault),
            node: me.as_ref().map(|me| me.node()),
        };
        Ok((if json { shown.json() } else { shown.line() }, notes))
    }

    /// [`crate::machine_name::set_nick`] as `aterm name nick` runs it, then the report of the name
    /// as it stands (or would, after a dry run), with the [`Note`]s said beside it.
    ///
    /// # Errors
    /// As [`report`]; the requested change cannot be validated or written ([`crate::machine_name::set_nick`]).
    pub(super) fn renick(
        state: &Path,
        vault: Option<&Paths>,
        ask: &NickAsk<'_>,
        json: bool,
        roster: impl FnOnce() -> io::Result<Option<String>>,
        now_ms: u64,
    ) -> Result<(String, Vec<Note>), String> {
        let NickAsk { to, kind, dry_run } = *ask;
        let narrowed =
            aterm_uds::private_file::ensure_private_dir(state).map_err(|e| e.to_string())?;
        let set = crate::machine_name::set_nick(state, to, kind, roster, now_ms, dry_run)
            .map_err(|e| e.to_string())?;
        let me = report_node(state);
        let mut notes: Vec<Note> = narrowed.into_iter().map(Note::Narrowed).collect();
        let nick = set.name.display_nick();
        if !set.changed {
            notes.push(Note::Already(nick.clone()));
        }
        if set.shared {
            notes.push(Note::Shared(nick));
        }
        if dry_run {
            notes.push(Note::DryRun);
        }
        let shown = Shown {
            name: &set.name,
            origin: set.origin,
            fleet: InFleet::read(state, vault),
            node: me.as_ref().map(|me| me.node()),
        };
        Ok((if json { shown.json() } else { shown.line() }, notes))
    }

    /// What `aterm name` says of `name`.
    pub(super) struct Shown<'a> {
        pub(super) name: &'a MachineName,
        pub(super) origin: Origin,
        /// Whether this machine is in a fleet ([`InFleet`]).
        pub(super) fleet: InFleet,
        /// Its fleet node, when the identity is present and readable.
        pub(super) node: Option<&'a str>,
    }

    impl Shown<'_> {
        /// `m8 Heron  class=seeded fleet=in node=<node>`, `migration=` before `node=` while one
        /// is not finished.
        pub(super) fn line(&self) -> String {
            let mut line = format!(
                "{}  class={} fleet={}",
                self.name.title(),
                class_word(self.name.class),
                self.fleet.word()
            );
            if let Some(migration) = self.name.migration {
                line.push_str(" migration=");
                line.push_str(migration_word(migration));
            }
            if let Some(node) = self.node {
                line.push_str(" node=");
                line.push_str(node);
            }
            line.push('\n');
            line
        }

        /// One object of every field, keys in order.
        pub(super) fn json(&self) -> String {
            use aterm_json::Value;
            let name = self.name;
            let text = |word: &str| Value::String(word.to_owned());
            let mut object = aterm_json::Map::new();
            object.insert("class".to_owned(), text(class_word(name.class)));
            object.insert("fleet".to_owned(), text(self.fleet.word()));
            object.insert(
                "made".to_owned(),
                text(match self.origin {
                    Origin::Kept => "kept",
                    Origin::Registry => "registry",
                    Origin::Evidence => "evidence",
                    Origin::Drawn => "drawn",
                }),
            );
            object.insert(
                "migration".to_owned(),
                name.migration
                    .map_or(Value::Null, |migration| text(migration_word(migration))),
            );
            object.insert("name".to_owned(), text(&name.name()));
            object.insert("nick".to_owned(), text(name.nick.as_str()));
            object.insert(
                "nick_class".to_owned(),
                text(nick_class_word(name.nick_class)),
            );
            object.insert("node".to_owned(), self.node.map_or(Value::Null, text));
            object.insert(
                "number".to_owned(),
                name.num
                    .map_or(Value::Null, |num| Value::from(u64::from(num.get()))),
            );
            object.insert("rev".to_owned(), Value::from(name.rev));
            object.insert("since".to_owned(), Value::from(name.since));
            object.insert("title".to_owned(), text(&name.title()));
            let mut json = aterm_json::to_string(&Value::Object(object)).unwrap_or_default();
            json.push('\n');
            json
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
