// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm itself.

use super::apps::{
    Notes, Signed, Stage, account, bundle_info, cached_verified, clear_stage, ditto_unzip,
    fresh_stage, give_to, is_sha256, last_words, not_cached, one_app_in, running, swap_into,
    verify_team, vet_bundle,
};
use super::{Ctx, Item};
use crate::model::{Effect, Outcome, Probe, Read, User};
use crate::port::{Cmd, Ran, RunFailed, System};
use aterm_converge::StoodAside;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// The Developer ID team that signs every official aterm release (install.sh's `OFFICIAL_TEAM_ID`).
pub const TEAM_ID: &str = "A66A9P66Z7";

/// The release bundle's identifier.
pub const BUNDLE_ID: &str = "com.aterm.aterm";

/// Where aterm.app is installed.
pub const APP: &str = "/Applications/aterm.app";

/// The one executable, which `~/.local/bin/aterm` links to.
pub const EXE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

/// Where [`App`] records whether the aterm.app it installed showed it ran
/// ([`crate::agent::Layout::app_first_run`]): nothing replaces that bundle while its hold
/// lasts.
fn first_run_record() -> PathBuf {
    crate::agent::Layout::installed().app_first_run()
}

/// `staff`, the group install.sh's own installs leave aterm.app in.
pub const STAFF_GID: u32 = 20;

/// atpkg's exit when another run holds the store lock past `--wait-lock`.
pub const ATPKG_CONTENDED: i32 = 75;

/// atpkg's exit when the index cannot be reached.
pub const ATPKG_OFFLINE: i32 = 69;

const CLI_BUDGET: Duration = Duration::from_secs(600);

const TOOLSET_BUDGET: Duration = Duration::from_secs(3 * 3600);

/// The table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// The aterm version the fleet runs (the release the controller cached).
    #[serde(default)]
    pub version: Option<String>,
    /// SHA-256 of that release's `aterm-<version>-mac.zip` (filled by the controller).
    #[serde(default)]
    pub archive_sha256: Option<String>,
    /// SHA-256 of that release's `install.sh` (filled by the controller).
    #[serde(default)]
    pub install_sh_sha256: Option<String>,
    /// Who owns `/Applications/aterm.app`, so its updater works (default: the first of `users`).
    #[serde(default)]
    pub owner: Option<String>,
    /// Users who get the `aterm` command and PATH wiring (default: `users`).
    #[serde(default)]
    pub cli_users: Vec<String>,
    /// `aterm pkg install --default-set --wait-lock 1800` as each CLI user.
    #[serde(default)]
    pub toolset: Option<bool>,
    /// `systems.alab.aterm-vault-server` as a system LaunchDaemon on always-on members
    /// (`Form::Desktop` with AC `sleep = 0`; `NotApplicable` elsewhere).
    #[serde(default)]
    pub vault_server: Option<bool>,
    /// The mDNS host label `aterm-vault-server` advertises (`_alab-vault._tcp`); defaults to the
    /// host's leading `{tag}` when valid, or `--no-advertise` when `""`.
    #[serde(default)]
    pub vault_advertise: Option<String>,
}

/// The items `table` asks for: `aterm.app`, then `aterm.cli@<user>` for each CLI user, then (with
/// `toolset = true`) `aterm.toolset@<user>` for each.
/// # Errors
/// One line naming what cannot be expanded: no owner, an account this Mac does not have, a
/// malformed version or SHA-256.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    if let Some(v) = &table.version
        && !is_version(v)
    {
        return Err(format!("[aterm] version {v:?} is not MAJOR.MINOR.PATCH"));
    }
    for (key, sha) in [
        ("archive_sha256", &table.archive_sha256),
        ("install_sh_sha256", &table.install_sh_sha256),
    ] {
        if let Some(sha) = sha
            && !is_sha256(sha)
        {
            return Err(format!(
                "[aterm] {key} {sha:?} is not 64 hexadecimal digits"
            ));
        }
    }
    let owner_name = table
        .owner
        .clone()
        .or_else(|| ctx.users.first().map(|u| u.name.clone()))
        .ok_or("[aterm]: no owner, and the baseline lists no users")?;
    let owner = account(ctx, &owner_name)
        .ok_or_else(|| format!("[aterm] owner {owner_name}: no such account on this Mac"))?;
    let cli_users: Vec<User> = if table.cli_users.is_empty() {
        ctx.users.clone()
    } else {
        table
            .cli_users
            .iter()
            .map(|name| {
                account(ctx, name)
                    .ok_or_else(|| format!("[aterm] cli_users: no account {name} on this Mac"))
            })
            .collect::<Result<_, _>>()?
    };
    let advertise = match table.vault_advertise.as_deref() {
        None => {
            let candidate = super::names::tag(&ctx.host_name);
            is_dns_label(candidate).then(|| candidate.to_owned())
        }
        Some("") => None,
        Some(label) if is_dns_label(label) => Some(label.to_owned()),
        Some(label) => {
            return Err(format!(
                "[aterm] vault_advertise {label:?} is not a DNS host label"
            ));
        }
    };
    let toolset = table.toolset == Some(true);
    let mut items: Vec<Box<dyn Item>> = vec![Box::new(App {
        version: table.version.clone(),
        archive: table.archive_sha256.clone(),
        owner,
        notes: Notes::default(),
    })];
    for user in &cli_users {
        items.push(Box::new(Cli {
            user: user.clone(),
            script: table.install_sh_sha256.clone(),
            toolset,
        }));
    }
    if toolset {
        for user in &cli_users {
            items.push(Box::new(Toolset {
                user: user.clone(),
                aside: Mutex::new(None),
            }));
        }
    }
    let on = table.vault_server.unwrap_or(true);
    items.push(Box::new(VaultServer {
        on,
        desktop: ctx.facts.form == crate::model::Form::Desktop,
        advertise,
        users: cli_users,
    }));
    Ok(items)
}

/// Whether `v` is a canonical `MAJOR.MINOR.PATCH`.
#[must_use]
pub fn is_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        })
}

/// Whether version `have` is `want` or newer, compared number by number.
#[must_use]
pub fn at_least(have: &str, want: &str) -> bool {
    let nums = |s: &str| {
        s.split('.')
            .map(crate::parse_uint::<u64>)
            .collect::<Option<Vec<u64>>>()
    };
    match (nums(have), nums(want)) {
        (Some(mut h), Some(mut w)) => {
            let n = h.len().max(w.len());
            h.resize(n, 0);
            w.resize(n, 0);
            h >= w
        }
        _ => have == want,
    }
}

/// What is at `/Applications/aterm.app`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Installed {
    /// Nothing.
    Missing,
    /// A release aterm.app of the team, at the pinned version or newer, owned by `uid`.
    Current {
        /// `CFBundleShortVersionString`.
        version: String,
        /// The bundle's owner.
        uid: u32,
    },
    /// Something else, described.
    Stale(String),
}

struct App {
    version: Option<String>,
    archive: Option<String>,
    owner: User,
    notes: Notes,
}

impl App {
    fn wanted(&self) -> String {
        match &self.version {
            Some(v) => format!("aterm {v} (team {TEAM_ID}), owned by {}", self.owner.name),
            None => format!("aterm (team {TEAM_ID}), owned by {}", self.owner.name),
        }
    }

    fn installed(&self, sys: &dyn System) -> Result<Installed, String> {
        let app = Path::new(APP);
        let st = match sys.stat(app) {
            Ok(st) => st,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Installed::Missing),
            Err(e) => return Err(format!("{APP}: {e}")),
        };
        if !st.dir || st.symlink {
            return Ok(Installed::Stale(format!("{APP} is not a bundle")));
        }
        match bundle_info(sys, app, "CFBundleIdentifier") {
            Read::Value(id) if id == BUNDLE_ID => {}
            Read::Value(id) => return Ok(Installed::Stale(format!("a foreign aterm.app ({id})"))),
            Read::Absent => return Ok(Installed::Stale("a bundle with no identifier".into())),
            Read::Unusable(why) => return Err(why),
        }
        let version = match bundle_info(sys, app, "CFBundleShortVersionString") {
            Read::Value(v) => v,
            Read::Absent => return Ok(Installed::Stale("aterm with no version".into())),
            Read::Unusable(why) => return Err(why),
        };
        match verify_team(sys, app, TEAM_ID) {
            Signed::Team => {}
            Signed::Other(why) => return Ok(Installed::Stale(format!("aterm {version} ({why})"))),
            Signed::Unusable(why) => return Err(why),
        }
        if self
            .version
            .as_deref()
            .is_some_and(|want| !at_least(&version, want))
        {
            return Ok(Installed::Stale(format!("aterm {version}")));
        }
        Ok(Installed::Current {
            version,
            uid: st.uid,
        })
    }

    fn install(&self, sys: &dyn System, cached: &Path, stage: &Stage<'_>) -> Result<(), String> {
        let new_dir = stage.join("new");
        sys.create_dir_all(&new_dir, 0o700)
            .map_err(|e| format!("{}: {e}", new_dir.display()))?;
        ditto_unzip(sys, cached, &new_dir)?;
        let found = one_app_in(sys, &new_dir)?;
        if found != "aterm.app" {
            return Err(format!("the release zip holds {found}, not aterm.app"));
        }
        let staged = new_dir.join("aterm.app");
        give_to(sys, &staged, self.owner.uid, STAFF_GID)?;
        vet_bundle(sys, &staged, TEAM_ID, self.version.as_deref(), &self.notes)?;
        match bundle_info(sys, &staged, "CFBundleIdentifier") {
            Read::Value(id) if id == BUNDLE_ID => {}
            Read::Value(id) => {
                return Err(format!("the release zip's bundle is {id}, not {BUNDLE_ID}"));
            }
            Read::Absent => return Err("the release zip's bundle has no identifier".into()),
            Read::Unusable(why) => return Err(why),
        }
        swap_into(sys, &staged, Path::new(APP), stage)?;
        self.first_run(sys)
    }

    /// Run the aterm just installed once, as its owner: its first exec, which
    /// [`first_run_record`] holds the bundle in place for until it showed it ran. The record is
    /// readable by all, so a plan run without root reads the hold too.
    fn first_run(&self, sys: &dyn System) -> Result<(), String> {
        let record = &first_run_record();
        sys.create_dir_all(record, 0o755)
            .and_then(|()| crate::first_exec::stamp(sys, record))
            .map_err(|e| format!("{APP} is in place, but {}: {e}", record.display()))?;
        let ran = sys.run(
            &Cmd::new(EXE)
                .arg("--version")
                .as_user(&self.owner)
                .budget(aterm_uds::deadline::FIRST_EXEC_HANG),
        );
        let why = match ran {
            Ok(ran) if crate::first_exec::showed_it_ran(&ran) => {
                crate::first_exec::mark_ran(sys, record);
                return Ok(());
            }
            Ok(ran) => ran.describe(),
            Err(e @ RunFailed::TimedOut(_)) => format!(
                "{e}; a first exec this long suggests macOS's check of it is stuck, which a \
                 restart clears"
            ),
            Err(e) => e.to_string(),
        };
        let then = match crate::first_exec::hold_left(sys, record) {
            Some(left) => format!(
                "nothing replaces it for {} more minute(s) of awake time, or until a restart",
                crate::first_exec::minutes(left)
            ),
            None => "its hold is spent, so the next apply may replace it".to_owned(),
        };
        self.notes.push(format!(
            "aterm.app's first run did not show it ran ({why}); {then}"
        ));
        Ok(())
    }
}

impl Item for App {
    fn id(&self) -> String {
        "aterm.app".into()
    }

    fn family(&self) -> &'static str {
        "aterm"
    }

    fn title(&self) -> String {
        self.wanted()
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let from = match self.installed(sys) {
            Ok(Installed::Current { version, uid }) if uid == self.owner.uid => {
                return Probe::Ok {
                    now: format!(
                        "aterm {version} (team {TEAM_ID}), owned by {}",
                        self.owner.name
                    ),
                };
            }
            Ok(Installed::Current { version, uid }) => {
                return Probe::Change {
                    from: format!("aterm {version}, owned by uid {uid}"),
                    to: format!("aterm {version}, owned by {}", self.owner.name),
                };
            }
            Ok(Installed::Missing) => "absent".to_owned(),
            Ok(Installed::Stale(what)) => what,
            Err(why) => return Probe::Unusable { why },
        };
        let Some(sha) = &self.archive else {
            return Probe::Blocked {
                why: "[aterm] archive_sha256 is not filled; the controller resolves it from the release"
                    .into(),
            };
        };
        if ctx.cached(sha).is_none() {
            return Probe::Blocked {
                why: not_cached(sha),
            };
        }
        if from != "absent"
            && let Some(left) = crate::first_exec::hold_left(sys, &first_run_record())
        {
            return Probe::Deferred {
                why: format!(
                    "the aterm.app installed last has not shown it ran, and macOS may still be \
                     checking it; it is replaced in {} minute(s), or after a restart",
                    crate::first_exec::minutes(left)
                ),
            };
        }
        if from != "absent" && !ctx.disrupt {
            match running(sys, Path::new(APP)) {
                Read::Value(true) => {
                    return Probe::Deferred {
                        why: "aterm is running; aterm.app is replaced when the Mac is idle, or with --now"
                            .into(),
                    };
                }
                Read::Unusable(why) => return Probe::Unusable { why },
                Read::Value(false) | Read::Absent => {}
            }
        }
        Probe::Change {
            from,
            to: self.wanted(),
        }
    }

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        self.notes.clear();
        if let Installed::Current { .. } = self.installed(sys)? {
            return give_to(sys, Path::new(APP), self.owner.uid, STAFF_GID);
        }
        let sha = self
            .archive
            .as_deref()
            .ok_or("[aterm] archive_sha256 is not filled")?;
        let cached = cached_verified(sys, ctx, sha)?;
        let stage = fresh_stage(sys)?;
        let done = self.install(sys, &cached, &stage);
        clear_stage(stage, done, &self.notes)
    }

    fn revert(&self) -> Option<String> {
        Some(
            "none kept: the replaced aterm.app is removed after the swap; pin the earlier [aterm] \
             version and apply again"
                .into(),
        )
    }

    fn effect(&self) -> Effect {
        Effect::Now
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.notes.joined()
    }
}

struct Cli {
    user: User,
    script: Option<String>,
    toolset: bool,
}

impl Cli {
    fn link(&self) -> PathBuf {
        Path::new(&self.user.home).join(".local/bin/aterm")
    }

    fn command(&self, script: &Path) -> Cmd {
        let mut cmd = Cmd::new("/bin/bash").args(["-s", "--", "--no-app"]);
        cmd = if self.toolset {
            cmd.budget(TOOLSET_BUDGET)
        } else {
            cmd.arg("--no-toolchain").budget(CLI_BUDGET)
        };
        cmd.as_user(&self.user).stdin_file(script)
    }
}

impl Item for Cli {
    fn id(&self) -> String {
        format!("aterm.cli@{}", self.user.name)
    }

    fn family(&self) -> &'static str {
        "aterm"
    }

    fn title(&self) -> String {
        format!("the aterm command for {}", self.user.name)
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let link = self.link();
        let from = match sys.stat(&link) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "absent".to_owned(),
            Err(e) => {
                return Probe::Unusable {
                    why: format!("{}: {e}", link.display()),
                };
            }
            Ok(st) if !st.symlink => format!("{} is a file, not a link", link.display()),
            Ok(_) => match sys.run(&Cmd::new("/usr/bin/readlink").arg(&link)) {
                Ok(ran) if ran.success() && link_target(&ran.out()) == EXE => {
                    return Probe::Ok {
                        now: format!("{} -> {EXE}", link.display()),
                    };
                }
                Ok(ran) if ran.success() => format!("a link to {}", link_target(&ran.out())),
                Ok(ran) => {
                    return Probe::Unusable {
                        why: format!("readlink {}: {}", link.display(), ran.describe()),
                    };
                }
                Err(e) => {
                    return Probe::Unusable {
                        why: format!("readlink {}: {e}", link.display()),
                    };
                }
            },
        };
        let Some(sha) = &self.script else {
            return Probe::Blocked {
                why: "[aterm] install_sh_sha256 is not filled; the controller resolves it from the release tag"
                    .into(),
            };
        };
        if ctx.cached(sha).is_none() {
            return Probe::Blocked {
                why: not_cached(sha),
            };
        }
        Probe::Change {
            from,
            to: format!("{} -> {EXE}", link.display()),
        }
    }

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        let sha = self
            .script
            .as_deref()
            .ok_or("[aterm] install_sh_sha256 is not filled")?;
        let script = cached_verified(sys, ctx, sha)?;
        let ran = sys
            .run(&self.command(&script))
            .map_err(|e| format!("install.sh as {}: {e}", self.user.name))?;
        if ran.success() {
            Ok(())
        } else {
            Err(format!(
                "install.sh as {}: {}{}",
                self.user.name,
                ran.describe(),
                last_words(&ran)
            ))
        }
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "rm {}, and delete install.sh's marked PATH block from {}'s shell profile if it wrote one",
            self.link().display(),
            self.user.name
        ))
    }

    fn effect(&self) -> Effect {
        Effect::Now
    }

    fn after(&self) -> Vec<String> {
        if self.toolset {
            vec!["aterm.app".into(), "developer.command-line-tools".into()]
        } else {
            vec!["aterm.app".into()]
        }
    }
}

struct Toolset {
    user: User,
    /// Why the last apply stood aside, if it did.
    aside: Mutex<Option<StoodAside>>,
}

impl Toolset {
    fn pkg(&self, args: &[&str]) -> Cmd {
        Cmd::new(EXE)
            .arg("pkg")
            .args(args.iter().copied())
            .as_user(&self.user)
    }

    fn set_aside(&self, why: Option<StoodAside>) {
        *self
            .aside
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = why;
    }
}

impl Item for Toolset {
    fn id(&self) -> String {
        format!("aterm.toolset@{}", self.user.name)
    }

    fn family(&self) -> &'static str {
        "aterm"
    }

    fn title(&self) -> String {
        format!("the ALab toolset for {}", self.user.name)
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let to = "the ALab default set, complete".to_owned();
        match sys.stat(Path::new(EXE)) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Probe::Change {
                    from: "aterm is not installed yet".into(),
                    to,
                };
            }
            Err(e) => {
                return Probe::Unusable {
                    why: format!("{EXE}: {e}"),
                };
            }
        }
        // It may be the first exec of an aterm.app no [`first_run_record`] covers, so it is
        // killed no sooner than macOS's check of that exec can last.
        let ran = match sys.run(
            &self
                .pkg(&["doctor", "--json"])
                .budget(aterm_uds::deadline::FIRST_EXEC_HANG),
        ) {
            Ok(ran) => ran,
            Err(e) => {
                return Probe::Unusable {
                    why: format!("aterm pkg doctor --json as {}: {e}", self.user.name),
                };
            }
        };
        doctor_says(&ran, &self.user.name, to)
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.set_aside(None);
        let cmd = self
            .pkg(&["install", "--default-set", "--wait-lock", "1800"])
            .budget(TOOLSET_BUDGET);
        let ran = sys
            .run(&cmd)
            .map_err(|e| format!("aterm pkg install as {}: {e}", self.user.name))?;
        match ran.status {
            Some(0) => Ok(()),
            Some(ATPKG_CONTENDED) => {
                self.set_aside(Some(StoodAside::Lock(format!(
                    "another atpkg run held {}'s store lock past the 30-minute wait (atpkg exit 75); the next apply retries",
                    self.user.name
                ))));
                Ok(())
            }
            Some(ATPKG_OFFLINE) => {
                self.set_aside(Some(StoodAside::Offline(
                    "the ALab index could not be reached (atpkg exit 69, offline); the next apply retries"
                        .into(),
                )));
                Ok(())
            }
            _ => Err(format!(
                "aterm pkg install --default-set as {}: {}{}",
                self.user.name,
                ran.describe(),
                last_words(&ran)
            )),
        }
    }

    fn revert(&self) -> Option<String> {
        Some(format!("aterm pkg uninstall --all, as {}", self.user.name))
    }

    fn effect(&self) -> Effect {
        Effect::Now
    }

    fn after(&self) -> Vec<String> {
        vec![
            format!("aterm.cli@{}", self.user.name),
            "developer.command-line-tools".into(),
        ]
    }

    fn stood_aside(&self) -> Option<StoodAside> {
        self.aside
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

const DOCTOR_TOOLSET_ROW: &str = "base.pkg.toolset";

fn doctor_says(ran: &Ran, user: &str, to: String) -> Probe {
    let unusable = |what: String| Probe::Unusable {
        why: format!("aterm pkg doctor --json as {user}: {what}"),
    };
    if ran.status == Some(2) {
        return unusable(format!(
            "{} — an aterm older than `pkg doctor --json` refuses it; the fleet reads no prose",
            ran.describe()
        ));
    }
    let Ok(report) = aterm_json::from_str::<aterm_converge::Report>(ran.out().trim()) else {
        return unusable(format!(
            "{}; what it printed is not its report",
            ran.describe()
        ));
    };
    if report.schema != aterm_converge::SCHEMA {
        return unusable(format!(
            "{}; its report is schema {}, and this agent reads schema {}",
            ran.describe(),
            report.schema,
            aterm_converge::SCHEMA
        ));
    }
    if report.mode != aterm_converge::ReportMode::Check {
        return unusable(format!("{}; its report is not a check", ran.describe()));
    }
    let why = |row: &aterm_converge::ItemReport| {
        let why = row
            .explain
            .as_ref()
            .map(|e| e.why.as_str())
            .or(row.why.as_deref())
            .or(row.from.as_deref())
            .unwrap_or(row.title.as_str());
        format!("{}: {why}", row.id)
    };
    match (ran.status, report.complete) {
        (Some(0), true) => {
            let Some(toolset) = report.items.iter().find(|row| row.id == DOCTOR_TOOLSET_ROW) else {
                return unusable(format!(
                    "{}, but its report has no toolset row",
                    ran.describe()
                ));
            };
            if toolset.outcome != Outcome::Ok {
                return Probe::Change {
                    from: why(toolset),
                    to,
                };
            }
            let active = toolset.to.clone().unwrap_or_else(|| toolset.title.clone());
            let flagged = report
                .items
                .iter()
                .filter(|row| {
                    !matches!(
                        row.outcome,
                        Outcome::Ok | Outcome::Changed | Outcome::NotApplicable | Outcome::Deferred
                    )
                })
                .count();
            Probe::Ok {
                now: if flagged == 0 {
                    active
                } else {
                    format!("{active}; {flagged} thing(s) to look at")
                },
            }
        }
        (Some(1), false) => {
            let failing: Vec<String> = report
                .items
                .iter()
                .filter(|row| row.fails_surface())
                .map(why)
                .collect();
            if failing.is_empty() {
                unusable(format!(
                    "{}, and its report names no row that fails it",
                    ran.describe()
                ))
            } else {
                Probe::Change {
                    from: failing.join("; "),
                    to,
                }
            }
        }
        (_, complete) => unusable(format!(
            "{}, but its report says the toolset is {}",
            ran.describe(),
            if complete { "complete" } else { "not complete" }
        )),
    }
}

/// The target `readlink` printed: its answer without the one newline it ends with. Other
/// trailing whitespace belongs to the target, so a link to `<exe> ` never reads as `<exe>`.
fn link_target(printed: &str) -> &str {
    printed.strip_suffix('\n').unwrap_or(printed)
}

fn is_dns_label(label: &str) -> bool {
    (1..=63).contains(&label.len())
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

struct VaultServer {
    on: bool,
    desktop: bool,
    advertise: Option<String>,
    users: Vec<User>,
}

impl VaultServer {
    fn plist_text(&self) -> String {
        let args = aterm_secrets::service::daemon_serve_args(
            Path::new(aterm_secrets::service::DAEMON_STORE),
            self.advertise.as_deref(),
        );
        aterm_secrets::service::daemon_plist_text(
            Path::new(EXE),
            &args,
            Path::new(aterm_secrets::service::DAEMON_LOG),
            Some(BUNDLE_ID),
        )
    }

    fn wanted(&self) -> String {
        match &self.advertise {
            Some(label) => format!(
                "{} running in system (advertising {label})",
                aterm_secrets::service::DAEMON_LABEL
            ),
            None => format!(
                "{} running in system (--no-advertise)",
                aterm_secrets::service::DAEMON_LABEL
            ),
        }
    }
}

fn ac_sleep_minutes(sys: &dyn System) -> Result<u64, Probe> {
    let cmd = Cmd::new(super::power::PMSET).args(["-g", "custom"]);
    let ran = match sys.run(&cmd) {
        Ok(ran) if ran.success() => ran,
        Ok(ran) => {
            return Err(Probe::Unusable {
                why: format!("{}: {}", cmd.display(), ran.describe()),
            });
        }
        Err(e) => {
            return Err(Probe::Unusable {
                why: format!("{}: {e}", cmd.display()),
            });
        }
    };
    let sections = super::power::parse_custom(&ran.out());
    let Some((_, rows)) = sections.iter().find(|(name, _)| name == "AC Power") else {
        return Err(Probe::NotApplicable {
            why: "this Mac has no AC Power profile (`pmset -g custom` prints no AC Power section)"
                .to_owned(),
        });
    };
    let Some((_, value)) = rows.iter().find(|(key, _)| key == "sleep") else {
        return Err(Probe::NotApplicable {
            why: "this Mac's AC Power profile has no `sleep` setting".to_owned(),
        });
    };
    crate::parse_uint(value).ok_or_else(|| Probe::Unusable {
        why: format!("`pmset -g custom` gave sleep {value:?}, not a number"),
    })
}

fn always_on_desktop(sys: &dyn System, ctx: &Ctx) -> Result<(), Probe> {
    if ctx.facts.form != crate::model::Form::Desktop {
        return Err(Probe::NotApplicable {
            why: "only always-on desktop Macs run the vault server as a LaunchDaemon; other Macs \
                  use the per-login LaunchAgent"
                .to_owned(),
        });
    }
    let sleep = ac_sleep_minutes(sys)?;
    if sleep != 0 {
        return Err(Probe::NotApplicable {
            why: format!(
                "this desktop sleeps on AC power (`sleep = {sleep}`); only always-on desktops \
                 (`sleep = 0`) run the vault server LaunchDaemon"
            ),
        });
    }
    Ok(())
}

fn daemon_loaded_pid(sys: &dyn System) -> Result<Option<Option<u32>>, String> {
    system_job_pid(sys, aterm_secrets::service::DAEMON_LABEL)
}

/// Whether launchd's `system` domain holds the job `label`: `None` when it is not loaded, else
/// its pid when it runs.
///
/// # Errors
/// `launchctl print` could not run or answered with anything but loaded or not, in one line.
pub(crate) fn system_job_pid(sys: &dyn System, label: &str) -> Result<Option<Option<u32>>, String> {
    let cmd =
        Cmd::new(super::sharing::LAUNCHCTL).args(aterm_secrets::service::system_launchctl_argv(
            aterm_secrets::service::SystemLaunchctl::Print(label),
        ));
    match sys.run(&cmd) {
        Ok(ran) if ran.success() => Ok(Some(aterm_secrets::service::print_pid(&ran.out()))),
        Ok(ran) if ran.status == Some(super::sharing::NOT_LOADED) => Ok(None),
        Ok(ran) => Err(format!("{}: {}", cmd.display(), ran.describe())),
        Err(e) => Err(format!("{}: {e}", cmd.display())),
    }
}

fn run_launchctl(sys: &dyn System, args: Vec<String>) -> Result<Ran, String> {
    let cmd = Cmd::new(super::sharing::LAUNCHCTL).args(args);
    sys.run(&cmd).map_err(|e| format!("{}: {e}", cmd.display()))
}

/// (Re)start the system job `label` that `plist_path` defines: `bootout` where it is loaded (one
/// not loaded is no error), then `enable`, `bootstrap` and `kickstart`.
///
/// # Errors
/// The first `launchctl` call that failed, in one line.
pub(crate) fn start_system_job(
    sys: &dyn System,
    plist_path: &Path,
    label: &str,
) -> Result<(), String> {
    if system_job_pid(sys, label)?.is_some() {
        let bootout = aterm_secrets::service::system_launchctl_argv(
            aterm_secrets::service::SystemLaunchctl::Bootout(label),
        );
        let ran = run_launchctl(sys, bootout)?;
        if !ran.success()
            && ran.status != Some(super::sharing::NO_SUCH_PROCESS)
            && ran.status != Some(super::sharing::NOT_LOADED)
        {
            return Err(format!(
                "launchctl bootout system/{label}: {}",
                ran.describe()
            ));
        }
    }
    let enable = run_launchctl(sys, vec!["enable".into(), format!("system/{label}")])?;
    if !enable.success() {
        return Err(format!(
            "launchctl enable system/{label}: {}",
            enable.describe()
        ));
    }
    let bootstrap = run_launchctl(
        sys,
        aterm_secrets::service::system_launchctl_argv(
            aterm_secrets::service::SystemLaunchctl::Bootstrap(plist_path),
        ),
    )?;
    if !bootstrap.success() {
        return Err(format!(
            "launchctl bootstrap system {}: {}",
            plist_path.display(),
            bootstrap.describe()
        ));
    }
    let kickstart = run_launchctl(
        sys,
        aterm_secrets::service::system_launchctl_argv(
            aterm_secrets::service::SystemLaunchctl::Kickstart(label),
        ),
    )?;
    if !kickstart.success() {
        return Err(format!(
            "launchctl kickstart system/{label}: {}",
            kickstart.describe()
        ));
    }
    Ok(())
}

impl Item for VaultServer {
    fn id(&self) -> String {
        "aterm.vault-server".to_owned()
    }

    fn family(&self) -> &'static str {
        "aterm"
    }

    fn title(&self) -> String {
        format!(
            "the vault server LaunchDaemon {}",
            if self.on { "on" } else { "off" }
        )
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let plist_path = Path::new(aterm_secrets::service::DAEMON_PLIST);
        if !self.on {
            let on_disk = match sys.stat(plist_path) {
                Ok(_) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => {
                    return Probe::Unusable {
                        why: format!("{}: {e}", plist_path.display()),
                    };
                }
            };
            let loaded = match daemon_loaded_pid(sys) {
                Ok(loaded) => loaded,
                Err(why) => return Probe::Unusable { why },
            };
            return match (on_disk, loaded) {
                (false, None) => Probe::Ok { now: "off".into() },
                (true, Some(Some(pid))) => Probe::Change {
                    from: format!("running in system (pid {pid})"),
                    to: "off".into(),
                },
                (true, _) => Probe::Change {
                    from: "plist installed".into(),
                    to: "off".into(),
                },
                (false, Some(_)) => Probe::Change {
                    from: "loaded in system".into(),
                    to: "off".into(),
                },
            };
        }
        if let Err(not_here) = always_on_desktop(sys, ctx) {
            return not_here;
        }
        let to = self.wanted();
        match sys.stat(Path::new(EXE)) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Probe::Change {
                    from: "aterm is not installed yet".into(),
                    to,
                };
            }
            Err(e) => {
                return Probe::Unusable {
                    why: format!("{EXE}: {e}"),
                };
            }
        }
        let held = match sys.read_file(plist_path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Probe::Unusable {
                    why: format!("{}: {e}", plist_path.display()),
                };
            }
        };
        let loaded = match daemon_loaded_pid(sys) {
            Ok(loaded) => loaded,
            Err(why) => return Probe::Unusable { why },
        };
        let expected = self.plist_text();
        let plist_matches = held.as_deref() == Some(expected.as_bytes());
        match (plist_matches, loaded) {
            (true, Some(Some(pid))) => Probe::Ok {
                now: format!(
                    "{} running in system (pid {pid})",
                    aterm_secrets::service::DAEMON_LABEL
                ),
            },
            (true, Some(None)) => Probe::Change {
                from: "loaded in system, not running".into(),
                to,
            },
            (true, None) => Probe::Change {
                from: "plist installed, not loaded in system".into(),
                to,
            },
            (false, _) if held.is_some() => Probe::Change {
                from: "stale LaunchDaemon plist".into(),
                to,
            },
            (false, _) => Probe::Change {
                from: "not installed".into(),
                to,
            },
        }
    }

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        let plist_path = Path::new(aterm_secrets::service::DAEMON_PLIST);
        let label = aterm_secrets::service::DAEMON_LABEL;
        if !self.on {
            let bootout = aterm_secrets::service::system_launchctl_argv(
                aterm_secrets::service::SystemLaunchctl::Bootout(label),
            );
            let ran = run_launchctl(sys, bootout)?;
            if !ran.success()
                && ran.status != Some(super::sharing::NO_SUCH_PROCESS)
                && ran.status != Some(super::sharing::NOT_LOADED)
            {
                return Err(format!(
                    "launchctl bootout system/{label}: {}",
                    ran.describe()
                ));
            }
            sys.remove(plist_path)
                .map_err(|e| format!("{}: {e}", plist_path.display()))?;
            return Ok(());
        }
        match always_on_desktop(sys, ctx) {
            Ok(()) => {}
            Err(Probe::NotApplicable { .. }) => return Ok(()),
            Err(Probe::Unusable { why }) => return Err(why),
            Err(other) => return Err(format!("aterm.vault-server: {other:?}")),
        }
        // Retire any per-login user LaunchAgent on the same port before starting the daemon.
        let user_label = aterm_secrets::service::Job::Server.label();
        for user in &self.users {
            for domain in [
                aterm_secrets::service::Domain::Gui,
                aterm_secrets::service::Domain::User,
            ] {
                let _ = run_launchctl(
                    sys,
                    aterm_secrets::service::launchctl_argv(
                        user.uid,
                        aterm_secrets::service::Launchctl::Bootout(domain, user_label),
                    ),
                );
            }
            let agent_plist = aterm_secrets::service::plist_path(
                Path::new(&user.home),
                aterm_secrets::service::Job::Server,
            );
            let _ = sys.remove(&agent_plist);
        }
        let store_dir = Path::new(aterm_secrets::service::DAEMON_STORE);
        sys.create_dir_all(store_dir, 0o700)
            .map_err(|e| format!("{}: {e}", store_dir.display()))?;
        if let Some(log_dir) = Path::new(aterm_secrets::service::DAEMON_LOG).parent() {
            sys.create_dir_all(log_dir, 0o755)
                .map_err(|e| format!("{}: {e}", log_dir.display()))?;
        }
        if let Some(daemons) = plist_path.parent() {
            sys.create_dir_all(daemons, 0o755)
                .map_err(|e| format!("{}: {e}", daemons.display()))?;
        }
        let expected = self.plist_text();
        sys.write_file(plist_path, expected.as_bytes(), 0o644)
            .map_err(|e| format!("{}: {e}", plist_path.display()))?;
        sys.chown(plist_path, 0, 0)
            .map_err(|e| format!("chown {}: {e}", plist_path.display()))?;
        start_system_job(sys, plist_path, label)
    }

    fn revert(&self) -> Option<String> {
        let label = aterm_secrets::service::DAEMON_LABEL;
        let plist = aterm_secrets::service::DAEMON_PLIST;
        if self.on {
            Some(format!(
                "{} bootout system/{label}; /bin/rm -f {plist}",
                super::sharing::LAUNCHCTL
            ))
        } else {
            Some("aterm machines apply (with [aterm] vault_server = true)".to_owned())
        }
    }

    fn effect(&self) -> Effect {
        Effect::Now
    }

    fn after(&self) -> Vec<String> {
        if self.on && self.desktop {
            vec!["aterm.app".into(), "power.ac.sleep".into()]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
#[path = "aterm_tests.rs"]
mod tests;
