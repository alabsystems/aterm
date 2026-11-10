// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm status`: one read of this machine — its setup (macOS), its ALab membership, its
//! secrets vault, its fleet, its one name, its tools, its repositories and its AI CLIs — each
//! area with what it is now, its next step and who takes that step: aterm by itself, an agent
//! (anyone at a terminal) by running a command, or a person, named only where only a person can
//! act.
//!
//! Every area is read by the reader its own verb uses (`aterm members status`, `aterm secrets
//! status`, `aterm fleet sync`'s read-only check, `aterm pkg doctor`'s toolset row, the setup
//! snapshot System State reads, the Command Line Tools presence check the shims share, and
//! `aterm ai status`'s look), from local files alone: no dial, no prompt, no write. Each reader
//! runs on a thread of its own and an area not read within [`READ_WITHIN`] says so. The output
//! passes through [`scrub`], so no email address reaches it, and no secret value is ever read.

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_agent::FleetCheck;
use aterm_agent::members_cli::Standing;
use aterm_converge::{ItemReport, Outcome, Remedy};
use aterm_machines::init::OwedClone;
use aterm_secrets::alab_join::JoinPlace;

/// The longest `aterm status` waits for any one area's reader.
const READ_WITHIN: Duration = Duration::from_secs(3);

const USAGE: &str = "aterm status [--json]: this machine at a glance — setup (macOS), membership, \
vault, fleet, name, tools, repos and agents — each with its state (ok | working | due | off | \
unknown), its next step and who takes it (aterm | agent | person). It reads local state only \
and changes nothing; `aterm help status` explains each area.";

/// Who takes an area's next step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Who {
    /// aterm, by itself: nobody needs to do anything.
    Aterm,
    /// Anyone at a terminal, an agent included, by running the command the step names.
    Agent,
    /// A person: only a person can take this step.
    Person,
}

impl Who {
    const fn word(self) -> &'static str {
        match self {
            Self::Aterm => "aterm",
            Self::Agent => "agent",
            Self::Person => "person",
        }
    }
}

/// Where an area stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    /// As wanted; no step.
    Ok,
    /// aterm is on it: the step is aterm's.
    Working,
    /// A step is due, an agent's, a person's or aterm's at its next pass.
    Due,
    /// Not wanted here, or held off by a person's choice; no step.
    Off,
    /// Could not be read; the step says what tells why.
    Unknown,
}

impl Level {
    const fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Working => "working",
            Self::Due => "due",
            Self::Off => "off",
            Self::Unknown => "unknown",
        }
    }
}

/// An area's next step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Step {
    pub(crate) who: Who,
    pub(crate) act: String,
}

/// One area of the read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Area {
    pub(crate) name: &'static str,
    pub(crate) level: Level,
    pub(crate) says: String,
    /// `Some` exactly when the level is working, due or unknown.
    pub(crate) next: Option<Step>,
}

impl Area {
    fn ok(name: &'static str, says: impl Into<String>) -> Self {
        Self::of(name, Level::Ok, says, None)
    }

    fn off(name: &'static str, says: impl Into<String>) -> Self {
        Self::of(name, Level::Off, says, None)
    }

    fn working(name: &'static str, says: impl Into<String>, act: impl Into<String>) -> Self {
        Self::of(name, Level::Working, says, Some((Who::Aterm, act.into())))
    }

    fn due(name: &'static str, says: impl Into<String>, who: Who, act: impl Into<String>) -> Self {
        Self::of(name, Level::Due, says, Some((who, act.into())))
    }

    /// An area whose reader failed (`why`), its step the verb that says more (`reader`).
    fn unread(name: &'static str, why: impl Into<String>, reader: &str) -> Self {
        Self::of(
            name,
            Level::Unknown,
            format!("not read: {}", why.into()),
            Some((Who::Agent, format!("`{reader}` says why"))),
        )
    }

    fn of(
        name: &'static str,
        level: Level,
        says: impl Into<String>,
        next: Option<(Who, String)>,
    ) -> Self {
        Self {
            name,
            level,
            says: says.into(),
            next: next.map(|(who, act)| Step { who, act }),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The facts each area is judged by, as its reader gives them.

/// One System State stage row: its name, `pending | running | waiting | applied | stopped`, and
/// its words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StageRow {
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) detail: String,
}

/// The first-launch setup, as System State reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "read_setup builds `NotHere` off macOS alone and the others on macOS alone; the \
                  tests build every one"
    )
)]
pub(crate) enum Setup {
    /// Not macOS: no first-launch setup.
    NotHere,
    /// Its records are changing under an update.
    Changing,
    /// A newer aterm saved it.
    Newer,
    /// Its records could not be read.
    Unread(String),
    Read {
        /// This Mac holds a record of the setup: aterm.app's first launch began it. Before
        /// that (aterm installed from a terminal, the app never opened) nothing runs the stages
        /// the rows show pending.
        begun: bool,
        finished: bool,
        /// A Cancel an older aterm's review recorded stands.
        cancelled: bool,
        /// The stages its plan selects.
        stages: Vec<StageRow>,
    },
}

impl Setup {
    fn stage(&self, name: &str) -> Option<&StageRow> {
        match self {
            Self::Read { stages, .. } => stages.iter().find(|stage| stage.name == name),
            _ => None,
        }
    }

    /// Whether the setup still runs the stage `name`: begun, selected and not yet applied or
    /// stopped.
    fn runs(&self, name: &str) -> bool {
        self.begun()
            && self.stage(name).is_some_and(|stage| {
                matches!(stage.status.as_str(), "pending" | "running" | "waiting")
            })
    }

    /// Whether this Mac's setup has begun: read, with a record of it.
    const fn begun(&self) -> bool {
        matches!(self, Self::Read { begun: true, .. })
    }
}

/// The vault subscriber that keeps this machine following the vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Follows {
    Runs,
    /// A person turned Sync off on this machine.
    Off,
    Absent,
    Unread(String),
}

/// The secrets vault, as `aterm secrets status` reads it: names only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Vault {
    Unread(String),
    Unconfigured,
    Pending,
    In {
        name: String,
        entries: usize,
        follows: Follows,
        /// Entries saved with different values while apart, by name.
        conflicts: Vec<aterm_secrets::client::Conflict>,
    },
    /// The latest envelope has no locker for this machine and it is not [`Vault::Dropped`]: an
    /// act it can read took it out, or its log does not say a rotation left it out on purpose
    /// (none trusted, one that still seats or wraps it while it waits on a heal or a sync, or one
    /// whose newest epoch never knew it). The command an admin runs.
    Revoked(Option<String>),
    /// A rotation left this machine out with no act against it that it can read
    /// (`client::dropped_words`, one-mesh E2): every desk that read the rotation refuses its key,
    /// so it comes back only as a new Mac, its vault folder set aside. `aterm secrets status`'s
    /// words.
    Dropped(String),
    Plain,
}

/// Apple's Command Line Tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Clt {
    NotHere,
    Present,
    /// aterm's own Mac Setup tab is installing them (`clt-shim.installing`).
    Installing,
    /// Missing, and nothing installs them now: no tab of aterm's is on its way (never opened, a
    /// person stopped it, or this account may not install them). `may_install`: the account is
    /// one of the Mac's administrators (or its groups did not read), the shim's own test.
    Missing {
        may_install: bool,
    },
}

/// The AI CLIs, as `aterm ai status`'s look and health read them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Agents {
    /// Each CLI someone asked for and wants here, with its state word.
    pub(crate) asked: Vec<(&'static str, &'static str)>,
    /// Those whose install or sign-in `aterm ai doctor` makes itself.
    pub(crate) by_aterm: Vec<&'static str>,
    /// Those only a person signs in, and the command that starts each.
    pub(crate) by_person: Vec<&'static str>,
    pub(crate) person_steps: Vec<String>,
    /// What a person does first where a sign-in starts nothing until then.
    pub(crate) mends: Vec<String>,
    /// How many CLIs nobody asked for are left on demand.
    pub(crate) unasked: usize,
}

// ---------------------------------------------------------------------------------------------
// Judging each area.

/// The first-launch setup's area; `None` off macOS.
pub(crate) fn setup_area(setup: &Setup) -> Option<Area> {
    const NAME: &str = "setup";
    Some(match setup {
        Setup::NotHere => return None,
        Setup::Changing => Area::working(
            NAME,
            "its records are changing under an update",
            "the updated aterm carries on with it",
        ),
        Setup::Newer => Area::off(
            NAME,
            "a newer aterm saved this Mac's setup; this one leaves it",
        ),
        Setup::Unread(why) => Area::unread(NAME, why, "aterm doctor"),
        Setup::Read {
            cancelled: true, ..
        } => Area::off(
            NAME,
            "an older aterm's Cancel stands: nothing runs until a group is turned on",
        ),
        Setup::Read { begun: false, .. } => Area::due(
            NAME,
            "not begun: aterm.app has not been opened on this Mac, and its first launch runs it",
            Who::Agent,
            "`aterm init` sets this Mac up now (opening aterm.app runs the same setup by itself)",
        ),
        Setup::Read {
            finished: true,
            stages,
            ..
        } => {
            let stopped: Vec<&str> = stages
                .iter()
                .filter(|stage| stage.status == "stopped")
                .map(|stage| stage.name.as_str())
                .collect();
            if !stopped.is_empty() {
                Area::off(
                    NAME,
                    format!(
                        "a person's Stop or Restore ended it before: {} (Set up now in System \
                         State goes on)",
                        stopped.join(", ")
                    ),
                )
            } else {
                let unfinished: Vec<&StageRow> = stages
                    .iter()
                    .filter(|stage| {
                        !matches!(
                            stage.status.as_str(),
                            "ok" | "done" | "applied" | "not-applicable"
                        )
                    })
                    .collect();
                if unfinished.is_empty() {
                    Area::ok(
                        NAME,
                        format!("selected setup completed ({} steps)", stages.len()),
                    )
                } else {
                    use std::fmt::Write as _;
                    let automatic = unfinished
                        .iter()
                        .all(|stage| stage.name == "AI account provisioning");
                    let mut says = String::from("unfinished: ");
                    for (idx, stage) in unfinished.iter().enumerate() {
                        if idx > 0 {
                            says.push_str("; ");
                        }
                        if stage.detail.trim().is_empty() {
                            says.push_str(&stage.name);
                        } else {
                            let _ = write!(says, "{}: {}", stage.name, stage.detail);
                        }
                    }
                    Area::due(
                        NAME,
                        says,
                        if automatic { Who::Aterm } else { Who::Agent },
                        if automatic {
                            "automatic provisioning resumes when the synced account roster changes"
                        } else {
                            "aterm init"
                        },
                    )
                }
            }
        }
        Setup::Read { stages, .. } => {
            let held: Vec<&str> = stages
                .iter()
                .filter(|stage| {
                    stage
                        .detail
                        .ends_with(aterm_types::macos_setup::StageRow::HELD_TAIL)
                })
                .map(|stage| stage.name.as_str())
                .collect();
            if !held.is_empty() {
                Area::due(
                    NAME,
                    format!("held after aterm stopped twice in: {}", held.join(", ")),
                    Who::Person,
                    "press Check again in System State",
                )
            } else {
                let open: Vec<&str> = stages
                    .iter()
                    .filter(|stage| {
                        matches!(stage.status.as_str(), "pending" | "running" | "waiting")
                    })
                    .map(|stage| stage.name.as_str())
                    .collect();
                if open.is_empty() {
                    Area::ok(NAME, "nothing left to run")
                } else {
                    Area::working(
                        NAME,
                        format!("still to finish: {}", open.join(", ")),
                        "aterm runs each step by itself",
                    )
                }
            }
        }
    })
}

/// The step of a Mac a rotation dropped ([`Vault::Dropped`], [`Standing::Dropped`]); both areas
/// carry it, since each is read on its own and either may be late. No admin's approve and no
/// person's Allow brings that key back, so it comes back as a new Mac.
const DROPPED_STEP: &str = "set the vault folder named here aside (rename it; nothing is \
                            deleted), then join its vault again as a new Mac (`aterm members \
                            join` for ALab)";

/// The ALab membership area: `aterm members status`'s standing.
pub(crate) fn membership_area(standing: &Standing, setup: &Setup) -> Area {
    const NAME: &str = "membership";
    let says = standing.line();
    match standing {
        Standing::Member(_) => Area::ok(NAME, says),
        Standing::Seatless => Area::working(
            NAME,
            says,
            "its vault subscriber asks a member for a seat with the keys it holds",
        ),
        Standing::Allowed { fix: None, .. } => {
            Area::working(NAME, says, "the vault records this Mac by itself")
        }
        Standing::Allowed { held: true, .. } => Area::off(NAME, says),
        Standing::Allowed { fix: Some(fix), .. } => Area::due(
            NAME,
            "allowed: ~/.secrets is written",
            Who::Agent,
            fix.clone(),
        ),
        Standing::Asking(JoinPlace::Away) => Area::due(
            NAME,
            "asking, off the ALab LAN, where nothing lets this Mac in by itself",
            Who::Person,
            "press Allow on any member, then type the phrase it shows into `aterm members join \
             <phrase>` here (or bring this Mac back to the ALab LAN)",
        ),
        Standing::Asking(_) => {
            Area::working(NAME, says, "aterm keeps asking until a member lets it in")
        }
        // Not the setup's, even while it runs: its Secrets step reads a dropped Mac blocked and
        // plans no join, since a join keeps this Mac's key, which every member that read the
        // rotation refuses.
        Standing::Dropped(_) => Area::due(NAME, says, Who::Agent, DROPPED_STEP),
        Standing::NotIn if setup.runs(aterm_types::macos_setup::SECRETS_VAULT_ROW) => {
            Area::working(
                NAME,
                "not in ALab yet",
                "the setup's Secrets vault step joins ALab",
            )
        }
        Standing::NotIn => Area::due(
            NAME,
            "not in ALab",
            Who::Agent,
            "`aterm members join` asks every member",
        ),
        Standing::Unknown(why) => Area::unread(NAME, why, "aterm members status"),
    }
}

/// The secrets vault area.
pub(crate) fn vault_area(vault: &Vault, setup: &Setup) -> Area {
    const NAME: &str = "vault";
    match vault {
        Vault::Unread(why) => Area::unread(NAME, why, "aterm secrets status"),
        Vault::Unconfigured if setup.runs(aterm_types::macos_setup::SECRETS_VAULT_ROW) => {
            Area::working(
                NAME,
                "no vault configured yet",
                "the setup's Secrets vault step joins it",
            )
        }
        Vault::Unconfigured => Area::due(
            NAME,
            "no vault configured",
            Who::Agent,
            "`aterm secrets join` joins the ALab vault",
        ),
        Vault::Pending => Area::working(
            NAME,
            "asked to join; no locker has opened yet",
            "the vault subscriber finishes the join once this Mac is let in (see membership)",
        ),
        Vault::In {
            name,
            entries,
            follows,
            conflicts,
        } => {
            let says = format!("{name}: in, {entries} entries");
            if !conflicts.is_empty() {
                let (plain, tokens) = aterm_secrets::client::Conflict::split(conflicts);
                let set = "pick each value with `aterm secrets set <name>`";
                let pick = match (plain.is_empty(), tokens.is_empty()) {
                    (_, true) => set.to_owned(),
                    (true, false) => aterm_secrets::client::TOKEN_CONFLICT_PICK.to_owned(),
                    (false, false) => {
                        format!("{set}; {}", aterm_secrets::client::TOKEN_CONFLICT_PICK)
                    }
                };
                return Area::due(
                    NAME,
                    format!(
                        "{says}; saved with different values while apart: {}",
                        plain
                            .iter()
                            .chain(&tokens)
                            .copied()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    Who::Person,
                    pick,
                );
            }
            match follows {
                Follows::Runs => Area::ok(NAME, says),
                Follows::Off => Area::off(
                    NAME,
                    format!("{says}; Sync is off on this Mac (a person's choice)"),
                ),
                Follows::Absent => Area::due(
                    NAME,
                    format!("{says}; no vault subscriber runs, so it follows no change"),
                    Who::Agent,
                    "`aterm secrets agent on` starts it",
                ),
                Follows::Unread(why) => Area::unread(NAME, why, "aterm secrets status"),
            }
        }
        Vault::Revoked(approve) => Area::due(
            NAME,
            "revoked: the latest envelope has no locker for this Mac",
            Who::Agent,
            match approve {
                Some(command) => format!("on an admin Mac, `{command}` lets it in again"),
                None => "on an admin Mac, `aterm secrets approve` lets it in again".to_owned(),
            },
        ),
        Vault::Dropped(says) => Area::due(NAME, says, Who::Agent, DROPPED_STEP),
        Vault::Plain => Area::ok(NAME, "reading a plain text file"),
    }
}

/// The fleet area: the read-only check of what a sync would do, and this Mac's own presence row
/// (`own`, [`aterm_agent::fleet_view::own_line`]; one-mesh §4.16).
pub(crate) fn fleet_area(check: &Result<FleetCheck, String>, own: &str) -> Area {
    const NAME: &str = "fleet";
    match check {
        Err(why) => Area::unread(NAME, why, "aterm fleet"),
        Ok(FleetCheck::Current) => Area::ok(NAME, format!("in the vault's fleet; {own}")),
        Ok(FleetCheck::Due(why)) => Area::working(
            NAME,
            format!("a sync is due: {why}"),
            "the vault subscriber's next pass syncs it (`aterm fleet sync` does it now)",
        ),
        Ok(FleetCheck::Blocked(why)) => Area::due(
            NAME,
            format!("left as it is: {why}"),
            Who::Person,
            "decide which fleet this Mac keeps (`aterm help fleet`)",
        ),
        Ok(FleetCheck::Idle(idle)) => match idle {
            aterm_agent::FleetSync::NoVault | aterm_agent::FleetSync::NoCode => Area::working(
                NAME,
                format!("not in a fleet yet ({})", idle.report()),
                "it joins the default fleet once this Mac is in the ALab vault",
            ),
            _ => Area::off(NAME, format!("{}; {own}", idle.report())),
        },
    }
}

/// The name area: the one name this Mac goes by (`sync.identity`'s: the person's `[identity]
/// name`, else the swarm `m<N>` label the vault's roster gave it), its fleet name against it, and
/// whether the Mac's own names (Computer Name, Local hostname) say it yet.
pub(crate) fn name_area(name: &Result<crate::converge::MacName, String>) -> Area {
    use crate::converge::{MacName, MacNamePending};
    use aterm_agent::FleetName;
    const NAME: &str = "name";
    let (target, fleet, pending) = match name {
        Err(why) => return Area::unread(NAME, why, "aterm doctor"),
        Ok(MacName::Off(why)) => return Area::off(NAME, format!("not named by aterm: {why}")),
        Ok(MacName::Unlabelled) => {
            return Area::working(
                NAME,
                "no swarm label yet: the vault's roster gives this Mac its m<N> at admission",
                "aterm names the Mac once it is let in (see membership)",
            );
        }
        Ok(MacName::Named {
            target,
            fleet,
            pending,
        }) => (target, fleet, pending.as_ref()),
    };
    match fleet {
        FleetName::Due { from } => {
            return Area::working(
                NAME,
                format!("{target}; fleet name still {from}"),
                "the next sync.identity pass gives the fleet the same name",
            );
        }
        FleetName::Taken(other) => {
            return Area::due(
                NAME,
                format!("{target}; fleet name left as it is: {other} answers to {target}"),
                Who::Agent,
                "name this Mac another way in the fleet (`aterm fleet name set <name>`)",
            );
        }
        FleetName::Current | FleetName::Kept(_) | FleetName::NotApplicable(_) => {}
    }
    let fleet = match fleet {
        FleetName::Kept(own) => format!("fleet name {own} (yours)"),
        FleetName::NotApplicable(why) => format!("fleet name left as it is: {why}"),
        _ => format!("fleet name {target}"),
    };
    match pending {
        Some(MacNamePending::ThroughRule(pending)) => Area::working(
            NAME,
            format!("{target}; {fleet}; {pending}"),
            "the next sync.identity pass repairs the Mac's names through its existing naming rule",
        ),
        Some(MacNamePending::Elevation(pending)) => Area::due(
            NAME,
            format!("{target}; {fleet}; {pending}"),
            Who::Person,
            "complete setup's Mac password step (or run `aterm init` in your own Mac login)",
        ),
        Some(MacNamePending::Unread(pending)) => Area::unread(
            NAME,
            format!(
                "{target}; {fleet}; {pending}; the existing naming permission could not be checked"
            ),
            "check the Mac's existing naming permission before asking for another password",
        ),
        None => Area::ok(NAME, format!("{target}; {fleet}")),
    }
}

/// The tools area: Apple's Command Line Tools first, then the toolset `aterm pkg doctor` reads.
pub(crate) fn tools_area(clt: Clt, toolset: &Result<Option<ItemReport>, String>) -> Area {
    const NAME: &str = "tools";
    match clt {
        Clt::Installing => {
            return Area::working(
                NAME,
                "Apple's Command Line Tools are installing",
                "aterm finishes the install; git and the compilers work once it does",
            );
        }
        // Nothing installs them now, so the step is a person's: the install asks for an
        // administrator's password, which no agent types.
        Clt::Missing { may_install: true } => {
            return Area::due(
                NAME,
                "Apple's Command Line Tools are missing, and nothing is installing them",
                Who::Person,
                "Install\u{2026} on System State, or `aterm init --only base.clt` in a terminal \
                 (your Mac password once)",
            );
        }
        Clt::Missing { may_install: false } => {
            return Area::due(
                NAME,
                "Apple's Command Line Tools are missing",
                Who::Person,
                "only an administrator of this Mac can install them",
            );
        }
        Clt::NotHere | Clt::Present => {}
    }
    let clt_words = if clt == Clt::Present {
        "Command Line Tools present; "
    } else {
        ""
    };
    let row = match toolset {
        Err(why) => return Area::unread(NAME, why, "aterm pkg doctor"),
        Ok(None) => {
            return Area::working(
                NAME,
                format!("{clt_words}no toolset store yet"),
                "aterm's install pass lays the toolset",
            );
        }
        Ok(Some(row)) => row,
    };
    let says = row.explain.as_ref().map_or_else(
        || row.title.clone(),
        |explain| format!("{}: {}", row.title, explain.why),
    );
    match row.outcome {
        Outcome::Ok | Outcome::Changed => Area::ok(NAME, format!("{clt_words}{}", row.title)),
        Outcome::NotApplicable => Area::off(NAME, format!("{clt_words}{says}")),
        _ => {
            let remedy = row.explain.as_ref().map(|explain| &explain.remedy);
            match remedy {
                Some(Remedy::Person { pane, how }) => Area::due(
                    NAME,
                    format!("{clt_words}{says}"),
                    Who::Person,
                    format!("{pane}: {how}"),
                ),
                Some(Remedy::Network) => Area::working(
                    NAME,
                    format!("{clt_words}{says}"),
                    "aterm's next pass lays it once online",
                ),
                Some(Remedy::None) => Area::off(NAME, format!("{clt_words}{says}")),
                Some(remedy) => Area::due(
                    NAME,
                    format!("{clt_words}{says}"),
                    Who::Agent,
                    format!(
                        "`{}`",
                        remedy
                            .command()
                            .unwrap_or_else(|| "aterm doctor".to_owned())
                    ),
                ),
                None => Area::due(
                    NAME,
                    format!("{clt_words}{says}"),
                    Who::Agent,
                    "`aterm doctor` repairs what aterm lays",
                ),
            }
        }
    }
}

/// The repositories area: the setup's GitHub repositories step and what it waits on, or, where
/// no setup clones them (off macOS, or before aterm.app's first launch), the clone `aterm init`
/// owes (`owed`, as `sync.clone` reads it).
pub(crate) fn repos_area(
    setup: &Setup,
    git: bool,
    token: bool,
    owed: &Result<OwedClone, String>,
) -> Area {
    const NAME: &str = "repos";
    let stage_name = aterm_types::macos_setup::REPOSITORIES_STAGE.name;
    let waits = || {
        if !git {
            Some("waits for Apple's Command Line Tools (see tools)")
        } else if !token {
            Some("waits for the GitHub token the vault writes (see vault)")
        } else {
            None
        }
    };
    match setup {
        Setup::NotHere | Setup::Read { begun: false, .. } => owed_area(owed, token),
        Setup::Unread(why) => Area::unread(NAME, why, "aterm doctor"),
        Setup::Changing => Area::working(
            NAME,
            "the setup's records are changing under an update",
            "the updated aterm carries on with them",
        ),
        Setup::Newer => Area::off(
            NAME,
            "a newer aterm saved this Mac's setup; this one leaves it",
        ),
        Setup::Read { .. } => match setup.stage(stage_name) {
            None => Area::off(NAME, "the setup clones no repositories on this Mac"),
            Some(stage) => match stage.status.as_str() {
                "applied" => owed_area(owed, token),
                "stopped" => Area::off(NAME, "a Stop ended the setup before it cloned them"),
                "running" => Area::working(NAME, "cloning", "the setup clones them"),
                _ if stage
                    .detail
                    .ends_with(aterm_types::macos_setup::StageRow::HELD_TAIL) =>
                {
                    Area::due(
                        NAME,
                        stage.detail.clone(),
                        Who::Person,
                        "press Check again in System State",
                    )
                }
                _ => Area::working(
                    NAME,
                    waits().unwrap_or("to be cloned"),
                    "the setup clones them by itself",
                ),
            },
        },
    }
}

/// The repositories where no setup clones them: what `aterm init`'s owed clone says. With none
/// owed, `aterm init` either cloned what it found or has not run. Neither the absence of
/// debt nor a token proves repositories are present: ask init to reconcile the inventory.
fn owed_area(owed: &Result<OwedClone, String>, token: bool) -> Area {
    const NAME: &str = "repos";
    match owed {
        Err(why) => Area::unread(NAME, why, "aterm doctor"),
        Ok(OwedClone::Verified { present, total }) => Area::ok(
            NAME,
            format!("{present}/{total} vault repositories verified on this machine"),
        ),
        Ok(OwedClone::Nothing(_)) if token => Area::due(
            NAME,
            "no clone is owed; repository contents have not been verified",
            Who::Agent,
            "`aterm init --only github` verifies and clones the vault's repository inventory",
        ),
        Ok(OwedClone::Nothing(_)) => Area::due(
            NAME,
            "no clone is owed and no GitHub token is here",
            Who::Agent,
            "`aterm init` clones them, or owes the clone to the vault's sync until the token lands",
        ),
        Ok(OwedClone::Off(why) | OwedClone::Held(why)) => Area::off(NAME, why.clone()),
        Ok(OwedClone::Waiting(why)) => Area::working(
            NAME,
            why.clone(),
            "the vault's sync clones them by itself once it can",
        ),
        Ok(OwedClone::Due(why)) => Area::working(
            NAME,
            why.clone(),
            "the vault subscriber's next pass clones them (`aterm fleet sync` does it now)",
        ),
        Ok(OwedClone::Person(how)) => Area::due(
            NAME,
            "the clone aterm init owes stopped at a step only a person takes",
            Who::Person,
            how.clone(),
        ),
    }
}

/// The agents area: the AI CLIs someone asked for. One nobody asked for, signed out, is a note
/// on demand and never a step.
pub(crate) fn agents_area(agents: &Agents) -> Area {
    use std::fmt::Write as _;
    const NAME: &str = "agents";
    let mut says = String::new();
    if agents.asked.is_empty() {
        says.push_str("none asked for here");
    } else {
        for (idx, (id, state)) in agents.asked.iter().enumerate() {
            if idx > 0 {
                says.push_str(", ");
            }
            let _ = write!(says, "{id} {state}");
        }
    }
    if agents.unasked != 0 {
        let _ = write!(says, "; {} more on demand", agents.unasked);
    }
    if !agents.by_aterm.is_empty() {
        Area::due(
            NAME,
            says,
            Who::Agent,
            format!(
                "`aterm ai doctor` installs or signs in {} itself",
                agents.by_aterm.join(", ")
            ),
        )
    } else if !agents.by_person.is_empty() {
        Area::due(
            NAME,
            says,
            Who::Person,
            format!(
                "sign {} in once at this Mac ({})",
                agents.by_person.join(", "),
                agents.person_steps.join("; ")
            ),
        )
    } else if !agents.mends.is_empty() {
        Area::due(NAME, says, Who::Person, agents.mends.join("; "))
    } else {
        Area::ok(NAME, says)
    }
}

// ---------------------------------------------------------------------------------------------
// Output.

/// `text` with every email address in it replaced by `<email>`: a word with a local part, an
/// `@` and a dotted domain. `@self`, `user@host` and a lone `@` stay.
pub(crate) fn scrub(text: &str) -> String {
    scrub_cow(text).into_owned()
}

fn scrub_cow(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('@') {
        return std::borrow::Cow::Borrowed(text);
    }
    let local = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-');
    let domain = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-');
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let Some(rel) = bytes[i..].iter().position(|&b| b == b'@') else {
            out.push_str(&text[i..]);
            break;
        };
        let at = i + rel;
        out.push_str(&text[i..at]);
        let start = out.len()
            - out
                .as_bytes()
                .iter()
                .rev()
                .take_while(|&&b| local(b))
                .count();
        let end = bytes[at + 1..]
            .iter()
            .position(|&b| !domain(b))
            .map_or(bytes.len(), |n| at + 1 + n);
        let host = text[at + 1..end].trim_end_matches('.');
        let dotted = host
            .split_once('.')
            .is_some_and(|(name, tld)| !name.is_empty() && !tld.is_empty());
        if start < out.len() && dotted {
            out.truncate(start);
            out.push_str("<email>");
            i = at + 1 + host.len();
        } else {
            out.push('@');
            i = at + 1;
        }
    }
    std::borrow::Cow::Owned(out)
}

/// The text report: one line an area, and its step under it.
pub(crate) fn render_text(areas: &[Area]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("aterm status: this machine, read now (nothing was changed)\n");
    for area in areas {
        let _ = writeln!(
            out,
            "{:<11} {:<8} {}",
            area.name,
            area.level.word(),
            area.says
        );
        if let Some(step) = &area.next {
            let _ = writeln!(out, "{:<20} next, {}: {}", "", step.who.word(), step.act);
        }
    }
    if out.contains('@') { scrub(&out) } else { out }
}

/// The `--json` report: `{"schema":1,"areas":[{"area","state","says","next":{"who","act"}|null}]}`.
pub(crate) fn render_json(areas: &[Area]) -> String {
    let areas: Vec<aterm_json::Value> = areas
        .iter()
        .map(|area| {
            let next = area.next.as_ref().map_or(aterm_json::Value::Null, |step| {
                aterm_json::Value::Object(aterm_json::Map::from([
                    ("who".to_string(), step.who.word().into()),
                    ("act".to_string(), scrub_cow(&step.act).into_owned().into()),
                ]))
            });
            aterm_json::Value::Object(aterm_json::Map::from([
                ("area".to_string(), area.name.into()),
                ("state".to_string(), area.level.word().into()),
                (
                    "says".to_string(),
                    scrub_cow(&area.says).into_owned().into(),
                ),
                ("next".to_string(), next),
            ]))
        })
        .collect();
    aterm_json::Value::Object(aterm_json::Map::from([
        ("schema".to_string(), 1.into()),
        ("areas".to_string(), aterm_json::Value::Array(areas)),
    ]))
    .to_string()
}

// ---------------------------------------------------------------------------------------------
// The live readers.

/// Run `read` on a thread of its own and wait for it until `deadline`; `None` past it.
fn bounded<T: Send + 'static>(
    read: impl FnOnce() -> T + Send + 'static,
) -> impl FnOnce(Instant) -> Option<T> {
    let (tx, rx) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("aterm-status".into())
        .spawn(move || {
            let _ = tx.send(read());
        })
        .is_ok();
    move |deadline| {
        spawned
            .then(|| {
                rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .ok()
            })
            .flatten()
    }
}

#[cfg(target_os = "macos")]
fn read_setup() -> Setup {
    use aterm_cli::macos_doctor::{InitialSetupSnapshotError, initial_setup_snapshot_read_only};
    match initial_setup_snapshot_read_only() {
        Ok(snapshot) => {
            let mut stages: Vec<StageRow> = snapshot
                .rows
                .into_iter()
                .map(|row| StageRow {
                    name: row.name,
                    status: row.status,
                    detail: row.detail,
                })
                .collect();
            if snapshot
                .plan
                .selection
                .contains(aterm_types::macos_setup::Setting::Secrets)
            {
                let account = crate::doctor::ai_accounts_setup_observation();
                stages.push(StageRow {
                    name: "AI account provisioning".to_owned(),
                    status: if matches!(
                        account.standing,
                        aterm_types::macos_setup::Standing::InPlace
                            | aterm_types::macos_setup::Standing::NotNeeded
                    ) {
                        "applied"
                    } else {
                        "waiting"
                    }
                    .to_owned(),
                    detail: account.note.unwrap_or_default(),
                });
            }
            Setup::Read {
                begun: snapshot.begun,
                finished: snapshot.finished,
                cancelled: snapshot.cancelled,
                stages,
            }
        }
        Err(InitialSetupSnapshotError::Updating) => Setup::Changing,
        Err(InitialSetupSnapshotError::Newer) => Setup::Newer,
        Err(InitialSetupSnapshotError::Read(why)) => Setup::Unread(why),
    }
}

#[cfg(not(target_os = "macos"))]
fn read_setup() -> Setup {
    Setup::NotHere
}

/// `aterm secrets status`'s read as a [`Vault`]: a Mac its vault let go is [`Vault::Dropped`]
/// where no act it can read took it out (`client::dropped_words`), [`Vault::Revoked`] otherwise.
fn read_vault(paths: &aterm_secrets::client::Paths) -> Vault {
    use aterm_secrets::client::{self, Membership};
    let status = match client::status(paths) {
        Ok(status) => status,
        Err(error) => return Vault::Unread(error.to_string()),
    };
    match status.membership {
        Membership::Unconfigured => Vault::Unconfigured,
        Membership::Pending => Vault::Pending,
        Membership::Plain => Vault::Plain,
        Membership::Revoked { seq } => client::dropped_words(paths, seq)
            .map_or_else(|| Vault::Revoked(status.approve_command()), Vault::Dropped),
        Membership::In { .. } => Vault::In {
            name: status.config.map_or_else(String::new, |config| config.name),
            entries: status.names.len(),
            follows: read_follows(&paths.home),
            conflicts: client::conflicts(paths).unwrap_or_default(),
        },
    }
}

fn read_follows(home: &Path) -> Follows {
    use aterm_secrets::service::{self, Job, State};
    match service::state(home, Job::Subscriber) {
        Ok(State::Installed { .. }) => Follows::Runs,
        Ok(State::Absent) if service::subscriber_turned_off(home) => Follows::Off,
        Ok(State::Absent) => Follows::Absent,
        Err(error) => Follows::Unread(error.to_string()),
    }
}

fn read_clt() -> Clt {
    if !cfg!(target_os = "macos") {
        Clt::NotHere
    } else if aterm_types::clt_shim::developer_git() {
        Clt::Present
    } else if aterm_types::clt_shim::shim_dir().is_some_and(|shim| {
        std::fs::symlink_metadata(aterm_types::clt_shim::installing_mark(&shim))
            .is_ok_and(|meta| meta.is_file())
    }) {
        Clt::Installing
    } else {
        Clt::Missing {
            may_install: aterm_uds::account::administrator() != Some(false),
        }
    }
}

fn read_toolset() -> Result<Option<ItemReport>, String> {
    Ok(atpkg::store::resolve_configured().and_then(|layout| atpkg::doctor::toolset_row(&layout)))
}

fn read_agents() -> Agents {
    let env = aterm_agent::ai::cli::Env::from_process();
    let look = aterm_agent::ai::cli::look(&env);
    let health = aterm_agent::ai::state::health(&look.rows);
    Agents {
        asked: look
            .rows
            .iter()
            .filter(|row| row.asked && row.wanted)
            .map(|row| (row.connector.id, row.state.word()))
            .collect(),
        by_aterm: health.by_aterm,
        by_person: health.by_person,
        person_steps: health.person_steps,
        mends: health.mends,
        unasked: look.rows.iter().filter(|row| !row.asked).count(),
    }
}

/// Every area, each read on its own thread under one [`READ_WITHIN`] bound.
fn read_areas() -> Vec<Area> {
    let deadline = Instant::now() + READ_WITHIN;
    let late = |name: &'static str, reader: &str| {
        Area::unread(
            name,
            format!(
                "its reader did not answer within {} s",
                READ_WITHIN.as_secs()
            ),
            reader,
        )
    };
    let paths = aterm_secrets::client::Paths::live().map_err(|error| error.to_string());
    let setup = bounded(read_setup);
    let standing = {
        let paths = paths.clone();
        bounded(move || paths.map(|paths| Standing::live(&paths)))
    };
    let vault = {
        let paths = paths.clone();
        bounded(move || paths.map(|paths| read_vault(&paths)))
    };
    let fleet_check = bounded(aterm_agent::check_from_cache);
    // This Mac's row of the view, read apart from the sync check: a slow node service costs the
    // line its row, never the check.
    let fleet_own = {
        let paths = paths.clone().ok();
        bounded(move || {
            aterm_types::dirs::state_dir().map_or_else(
                || "presence not read: $HOME is not set".to_owned(),
                |state| {
                    let (fleet, at_ms) = aterm_agent::fleet_view::read(&state, paths.as_ref());
                    aterm_agent::fleet_view::own_line(&fleet, at_ms)
                },
            )
        })
    };
    let name = bounded(crate::converge::mac_names);
    let clt = bounded(read_clt);
    let toolset = bounded(read_toolset);
    let token = {
        let home = aterm_types::dirs::home_dir();
        bounded(move || {
            home.is_some_and(|home| {
                std::fs::symlink_metadata(home.join(agh::identity::vault::DEV_TOKEN_FILE))
                    .is_ok_and(|meta| meta.is_file())
            })
        })
    };
    let agents = bounded(read_agents);
    let owed = bounded(|| {
        aterm_machines::sync::Ports::owed_clone(
            &crate::converge::Live::here(),
            None,
            aterm_secrets::converge::RepositoryWork::Repair,
        )
    });

    let setup = setup(deadline);
    let mut areas = Vec::with_capacity(9);
    let setup = match setup {
        Some(setup) => {
            areas.extend(setup_area(&setup));
            setup
        }
        None => {
            areas.push(late("setup", "aterm doctor"));
            Setup::Unread("not read in time".into())
        }
    };
    areas.push(match standing(deadline) {
        Some(Ok(standing)) => membership_area(&standing, &setup),
        Some(Err(why)) => Area::unread("membership", why, "aterm members status"),
        None => late("membership", "aterm members status"),
    });
    areas.push(match vault(deadline) {
        Some(Ok(vault)) => vault_area(&vault, &setup),
        Some(Err(why)) => Area::unread("vault", why, "aterm secrets status"),
        None => late("vault", "aterm secrets status"),
    });
    areas.push(match fleet_check(deadline) {
        Some(check) => fleet_area(
            &check,
            &fleet_own(deadline).unwrap_or_else(|| {
                format!(
                    "this Mac's presence row not read within {} s",
                    READ_WITHIN.as_secs()
                )
            }),
        ),
        None => late("fleet", "aterm fleet"),
    });
    areas
        .push(name(deadline).map_or_else(|| late("name", "aterm doctor"), |name| name_area(&name)));
    let clt = clt(deadline);
    areas.push(match (clt, toolset(deadline)) {
        (Some(clt), Some(toolset)) => tools_area(clt, &toolset),
        _ => late("tools", "aterm pkg doctor"),
    });
    let git = matches!(clt, Some(Clt::Present | Clt::NotHere));
    areas.push(match (token(deadline), owed(deadline)) {
        (Some(token), Some(owed)) => repos_area(&setup, git, token, &owed),
        _ => late("repos", "aterm doctor"),
    });
    areas.push(match agents(deadline) {
        Some(agents) => agents_area(&agents),
        None => late("agents", "aterm ai status"),
    });
    areas
}

/// What `aterm status`'s words ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    Text,
    Json,
    Help,
}

/// `aterm status`'s words: nothing, `--json`, or `-h`/`--help`; anything else is a usage error.
fn asked(words: &[impl AsRef<std::ffi::OsStr>]) -> Option<Asked> {
    match words {
        [] => Some(Asked::Text),
        [flag] if flag.as_ref() == "--json" => Some(Asked::Json),
        [flag] if flag.as_ref() == "-h" || flag.as_ref() == "--help" => Some(Asked::Help),
        _ => None,
    }
}

/// `aterm status [--json]`. A read exits 0 whatever it finds: a step due is no failure.
pub(crate) fn main_entry(args: &[OsString]) -> ExitCode {
    let text = match asked(args) {
        None => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
        Some(Asked::Help) => format!("{USAGE}\n"),
        Some(Asked::Json) => {
            let mut json = render_json(&read_areas());
            json.push('\n');
            json
        }
        Some(Asked::Text) => render_text(&read_areas()),
    };
    aterm_uds::stdout::answer_exit("aterm status", &text, 0)
}

#[cfg(test)]
#[path = "machine_status_tests.rs"]
mod tests;
