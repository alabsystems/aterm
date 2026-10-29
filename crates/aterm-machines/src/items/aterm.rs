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
            .map(|p| p.parse::<u64>().ok())
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
                Ok(ran) if ran.success() && ran.out().trim() == EXE => {
                    return Probe::Ok {
                        now: format!("{} -> {EXE}", link.display()),
                    };
                }
                Ok(ran) if ran.success() => format!("a link to {}", ran.out().trim()),
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
    value.parse().map_err(|_| Probe::Unusable {
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
    let cmd =
        Cmd::new(super::sharing::LAUNCHCTL).args(aterm_secrets::service::system_launchctl_argv(
            aterm_secrets::service::SystemLaunchctl::Print(aterm_secrets::service::DAEMON_LABEL),
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
        if daemon_loaded_pid(sys)?.is_some() {
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
mod tests {
    use super::*;
    use crate::items::apps::tests::{
        CODESIGN_WRONG_TEAM, SPCTL_KYA_ON, admin, codesign_key, ctx_with, dir, err, ok, out_status,
        sha_of, stage_path,
    };
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran, Stat};

    /// [`first_run_record`] on this Mac.
    const FIRST_RUN: &str = "/Library/Management/aterm/state/aterm.app-first-run";

    const ZIP_SHA: &str = "457cee7e20d0d406fb6ce43275593d0f553eb0fccd46fcc812dfb8d02c0b427f";
    const INSTALL_SH_SHA: &str = "dd92e2224c1c5a8e2c69992c4bafdf79142cf27f36a5bacfbfaa8efe12949b98";

    const INSTALL_SH_OUT: &str = "install.sh: linked aterm 0.105.0 -> /Users//admin/.local/bin/aterm (follows the app's updates)\ninstall.sh: --no-toolchain: /Users//admin/.config/aterm/aterm.toml already says `auto_install = false` — left as it is\n";

    const DOCTOR_EMPTY: &str = r#"{"schema":1,"mode":"check","complete":false,"items":[{"id":"base.pkg.path","family":"pkg","title":"the managed bin/ on PATH","outcome":"planned","from":"managed bin/ is not on PATH here","to":"on PATH","effect":"now","layer":"base","class":"person","explain":{"why":"managed bin/ is not on PATH here; a new aterm shell adds it, or add: export PATH=\"$PATH:/Users//admin/Library/Application Support/aterm/pkg/bin\"","remedy":{"command":"export PATH=\"$PATH:/Users//admin/Library/Application Support/aterm/pkg/bin\""}}},{"id":"base.shell.hooks","family":"shell","title":"the shell.d hooks","outcome":"planned","from":"shell.d hooks not generated yet (an install writes them)","to":"present","effect":"now","layer":"base","class":"relay","explain":{"why":"shell.d hooks not generated yet (an install writes them)","remedy":"repair"}},{"id":"base.pkg.toolset","family":"pkg","title":"the ALab toolset","outcome":"planned","from":"no ALab programs are installed","to":"the ALab default set, installed","effect":"now","layer":"base","class":"install","explain":{"why":"no ALab programs are installed","remedy":{"command":"aterm pkg install --default-set"}},"essential":true}]}"#;

    const DOCTOR_FULL: &str = r#"{"schema":1,"mode":"check","complete":true,"items":[{"id":"base.pkg.path","family":"pkg","title":"the managed bin/ on PATH","outcome":"planned","from":"managed bin/ is not on PATH here","to":"on PATH","effect":"now","layer":"base","class":"person","explain":{"why":"managed bin/ is not on PATH here; a new aterm shell adds it, or add: export PATH=\"$PATH:/Users//admin/Library/Application Support/aterm/pkg/bin\"","remedy":{"command":"export PATH=\"$PATH:/Users//admin/Library/Application Support/aterm/pkg/bin\""}}},{"id":"base.pkg.toolset","family":"pkg","title":"the ALab toolset","outcome":"ok","to":"12 program(s) active","effect":"now","detail":"12","layer":"base","class":"install","essential":true}]}"#;

    const DOCTOR_BROKEN_SHIM: &str = r#"{"schema":1,"mode":"check","complete":false,"items":[{"id":"base.pkg.shims@cargo","family":"pkg","title":"the bin/ shim cargo","outcome":"planned","from":"broken bin shim /Users//admin/Library/Application Support/aterm/pkg/bin/cargo","to":"forwarding to a build in the store","effect":"now","layer":"base","class":"relay","explain":{"why":"broken bin shim /Users//admin/Library/Application Support/aterm/pkg/bin/cargo","remedy":"repair"},"essential":true},{"id":"base.pkg.toolset","family":"pkg","title":"the ALab toolset","outcome":"ok","to":"12 program(s) active","effect":"now","detail":"12","layer":"base","class":"install","essential":true}]}"#;

    const DOCTOR_DECLINED: &str = r#"{"schema":1,"mode":"check","complete":true,"items":[{"id":"base.pkg.toolset","family":"pkg","title":"the ALab toolset","outcome":"not-applicable","why":"the ALab toolset was removed on this machine (`aterm pkg install --default-set` reinstalls it)","effect":"now","layer":"base","class":"install","explain":{"why":"the ALab toolset was removed on this machine (`aterm pkg install --default-set` reinstalls it)","remedy":{"command":"aterm pkg install --default-set"}},"essential":true}]}"#;

    fn plistbuddy(bundle: &str, key: &str) -> String {
        format!("/usr/libexec/PlistBuddy -c 'Print :{key}' {bundle}/Contents/Info.plist")
    }

    fn table() -> Table {
        Table {
            version: Some("0.104.0".into()),
            archive_sha256: Some(ZIP_SHA.into()),
            install_sh_sha256: Some(INSTALL_SH_SHA.into()),
            owner: None,
            cli_users: vec![],
            toolset: None,
            vault_server: None,
            vault_advertise: None,
        }
    }

    fn installed(sys: &Fake, version: &str, uid: u32) {
        sys.file(APP, b"").stat_of(APP, Stat { uid, ..dir() });
        sys.file(&format!("{APP}/Contents/Info.plist"), b"");
        sys.on(
            &plistbuddy(APP, "CFBundleIdentifier"),
            ok("com.aterm.aterm\n"),
        );
        sys.on(
            &plistbuddy(APP, "CFBundleShortVersionString"),
            ok(&format!("{version}\n")),
        );
        sys.on(&codesign_key(APP, TEAM_ID), ok(""));
    }

    #[test]
    fn the_app_comes_first_then_each_user_s_command_then_each_toolset() {
        let ctx = ctx_with(&Fake::root(), &[], true);
        let mut t = table();
        t.toolset = Some(true);
        let items = expand(&t, &ctx).unwrap();
        let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
        assert_eq!(
            ids,
            [
                "aterm.app",
                "aterm.cli@admin",
                "aterm.toolset@admin",
                "aterm.vault-server"
            ]
        );
        assert_eq!(
            items[1].after(),
            ["aterm.app", "developer.command-line-tools"]
        );
        assert_eq!(expand(&table(), &ctx).unwrap()[1].after(), ["aterm.app"]);
        assert_eq!(
            items[2].after(),
            ["aterm.cli@admin", "developer.command-line-tools"]
        );
        t.version = Some("v0.104.0".into());
        assert!(
            expand(&t, &ctx)
                .err()
                .unwrap()
                .contains("MAJOR.MINOR.PATCH")
        );
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(at_least("0.105.0", "0.104.0"));
        assert!(at_least("0.104.0", "0.104.0"));
        assert!(at_least("0.104.10", "0.104.9"));
        assert!(!at_least("0.103.9", "0.104.0"));
        assert!(!at_least("0.104.0-dev", "0.104.0"));
        assert!(is_version("0.104.0") && !is_version("0.104") && !is_version("01.2.3"));
    }

    #[test]
    fn a_release_aterm_at_or_past_the_pin_owned_by_its_owner_is_ok() {
        let sys = Fake::root();
        installed(&sys, "0.105.0", 501);
        let ctx = ctx_with(&sys, &[], false);
        let items = expand(&table(), &ctx).unwrap();
        assert_eq!(
            items[0].probe(&sys, &ctx),
            Probe::Ok {
                now: "aterm 0.105.0 (team A66A9P66Z7), owned by admin".into()
            }
        );
    }

    #[test]
    fn a_wrong_owner_is_a_chown_that_needs_no_artifact() {
        let sys = Fake::root();
        installed(&sys, "0.104.0", 0);
        sys.on(&format!("/usr/sbin/chown -R 501:20 {APP}"), ok(""));
        sys.on(&format!("/bin/chmod -R go-w,ug-s {APP}"), ok(""));
        let ctx = ctx_with(&sys, &[], false);
        let items = expand(&table(), &ctx).unwrap();
        assert_eq!(
            items[0].probe(&sys, &ctx),
            Probe::Change {
                from: "aterm 0.104.0, owned by uid 0".into(),
                to: "aterm 0.104.0, owned by admin".into()
            }
        );
        items[0].apply(&sys, &ctx).unwrap();
        assert!(!sys.calls().iter().any(|c| c.contains("ditto")));
    }

    #[test]
    fn without_a_resolved_or_cached_release_the_app_is_blocked() {
        let sys = Fake::root();
        let ctx = ctx_with(&sys, &[], true);
        let mut t = table();
        t.archive_sha256 = None;
        let items = expand(&t, &ctx).unwrap();
        assert!(
            matches!(items[0].probe(&sys, &ctx), Probe::Blocked { why } if why.contains("controller resolves it"))
        );
        let items = expand(&table(), &ctx).unwrap();
        assert!(
            matches!(items[0].probe(&sys, &ctx), Probe::Blocked { why } if why.contains("not in the cache"))
        );
    }

    #[test]
    fn an_older_aterm_that_is_running_waits_for_an_idle_mac() {
        let sys = Fake::root();
        installed(&sys, "0.103.0", 501);
        sys.on(
            "/usr/bin/pgrep -f '/Applications/aterm\\.app/Contents/MacOS/'",
            ok("5295\n"),
        );
        let ctx = ctx_with(&sys, &[(ZIP_SHA, b"zip")], false);
        let items = expand(&table(), &ctx).unwrap();
        assert!(matches!(items[0].probe(&sys, &ctx), Probe::Deferred { .. }));
    }

    #[test]
    fn the_release_zip_is_vetted_given_to_the_owner_and_swapped_in() {
        let sys = Fake::root();
        let bytes = b"aterm zip";
        let sha = sha_of(bytes);
        let stage = stage_path(0);
        let mut t = table();
        t.archive_sha256 = Some(sha.clone());
        let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
        sys.on(
            &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
            ok(""),
        );
        let staged = format!("{stage}/new/aterm.app");
        sys.on_write(
            &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
            Path::new(&staged),
            b"",
        )
        .on_stat(
            &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
            Path::new(&staged),
            dir(),
        );
        sys.on_write(
            &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
            Path::new(&format!("{staged}/Contents/Info.plist")),
            b"",
        );
        sys.on(&format!("/usr/sbin/chown -R 501:20 {staged}"), ok(""));
        sys.on(&format!("/bin/chmod -R go-w,ug-s {staged}"), ok(""));
        sys.on(&codesign_key(&staged, TEAM_ID), ok(""));
        sys.on(
            &format!("/usr/sbin/spctl -a -vv -t exec {staged}"),
            err(0, SPCTL_KYA_ON),
        );
        sys.on(
            &plistbuddy(&staged, "CFBundleShortVersionString"),
            ok("0.104.0\n"),
        );
        sys.on(
            &plistbuddy(&staged, "CFBundleIdentifier"),
            ok("com.aterm.aterm\n"),
        );
        let first_run = format!("admin$ {EXE} --version");
        let budget = std::sync::Arc::new(std::sync::Mutex::new(None));
        let seen = std::sync::Arc::clone(&budget);
        let key = first_run.clone();
        sys.simulate(move |_, cmd| {
            (cmd.display() == key).then(|| {
                *seen.lock().unwrap() = Some(cmd.budget);
                ok("aterm 0.104.0\n")
            })
        });
        sys.awake_at(5_000_000);
        let items = expand(&t, &ctx).unwrap();
        assert!(
            matches!(items[0].probe(&sys, &ctx), Probe::Change { from, .. } if from == "absent")
        );
        items[0].apply(&sys, &ctx).unwrap();
        let calls = sys.calls();
        let at = |c: &str| {
            calls
                .iter()
                .position(|x| x.starts_with(c))
                .unwrap_or_else(|| panic!("{c}: {calls:#?}"))
        };
        assert!(at("/usr/sbin/chown -R 501:20") < at("/usr/bin/codesign"));
        assert!(at("/usr/bin/codesign") < at(&format!("rename {staged} {APP}")));
        let stamp = at(&format!("write {FIRST_RUN}/exec 644"));
        assert!(
            at(&format!("mkdir {FIRST_RUN} 755")) < stamp,
            "a plan without root reads it"
        );
        assert!(at(&format!("rename {staged} {APP}")) < stamp && stamp < at(&first_run));
        assert!(at(&first_run) < at(&format!("write {FIRST_RUN}/ran 644")));
        assert_eq!(
            *budget.lock().unwrap(),
            Some(aterm_uds::deadline::FIRST_EXEC_HANG)
        );
        assert_eq!(items[0].detail(&sys, &ctx), None, "Gatekeeper accepted it");
    }

    #[test]
    fn a_first_run_that_did_not_show_it_ran_notes_how_long_its_bundle_is_held() {
        let note = |took_ms: u64, reply: Option<Ran>| {
            let sys = Fake::root();
            sys.awake_at(5_000_000);
            let key = format!("admin$ {EXE} --version");
            sys.simulate(move |sys, cmd| {
                if cmd.display() != key {
                    return None;
                }
                sys.awake_at(5_000_000 + took_ms);
                reply.clone()
            });
            let app = App {
                version: None,
                archive: None,
                owner: admin(),
                notes: Notes::default(),
            };
            app.first_run(&sys).unwrap();
            assert!(sys.written(&format!("{FIRST_RUN}/ran")).is_none());
            app.notes.joined().unwrap()
        };
        let killed = Ran {
            status: None,
            stdout: Vec::new(),
            stderr: b"Killed: 9".to_vec(),
        };
        let held = note(0, Some(killed.clone()));
        assert!(
            held.contains(
                "nothing replaces it for 10 more minute(s) of awake time, or until a restart"
            ),
            "{held}"
        );
        let spent = note(600_000, Some(killed));
        assert!(
            spent.contains("its hold is spent, so the next apply may replace it"),
            "{spent}"
        );
        let unstarted = note(60_000, None);
        assert!(
            unstarted.contains("could not start") && unstarted.contains("for 9 more minute(s)"),
            "{unstarted}"
        );
    }

    #[test]
    fn an_aterm_app_that_has_not_shown_it_ran_is_not_replaced_even_now() {
        let probe = |record: &[(&str, &[u8])], awake: u64| {
            let sys = Fake::root();
            installed(&sys, "0.103.0", 501);
            for (name, bytes) in record {
                sys.file(&format!("{FIRST_RUN}/{name}"), bytes);
            }
            sys.awake_at(awake);
            let ctx = ctx_with(&sys, &[(ZIP_SHA, b"zip")], true);
            expand(&table(), &ctx).unwrap()[0].probe(&sys, &ctx)
        };
        let started: &[(&str, &[u8])] = &[("exec", b"5000000")];
        assert!(
            matches!(
                probe(started, 5_100_000),
                Probe::Deferred { why } if why.contains("in 9 minute(s)")
            ),
            "{:?}",
            probe(started, 5_100_000)
        );
        let changes = |p: Probe| matches!(p, Probe::Change { from, .. } if from == "aterm 0.103.0");
        assert!(changes(probe(&[], 5_100_000)), "no record");
        assert!(
            changes(probe(&[("exec", b"5000000"), ("ran", b"")], 5_100_000)),
            "it showed it ran"
        );
        assert!(changes(probe(started, 5_600_000)), "its hang has passed");
        assert!(
            changes(probe(started, 4_000_000)),
            "the Mac restarted since"
        );
    }

    #[test]
    fn a_release_zip_signed_by_another_team_is_refused() {
        let sys = Fake::root();
        let bytes = b"forged zip";
        let sha = sha_of(bytes);
        let stage = stage_path(0);
        let mut t = table();
        t.archive_sha256 = Some(sha.clone());
        let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
        sys.on(
            &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
            ok(""),
        );
        let staged = format!("{stage}/new/aterm.app");
        sys.on_write(
            &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
            Path::new(&staged),
            b"",
        )
        .on_stat(
            &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
            Path::new(&staged),
            dir(),
        );
        sys.on(&format!("/usr/sbin/chown -R 501:20 {staged}"), ok(""));
        sys.on(&format!("/bin/chmod -R go-w,ug-s {staged}"), ok(""));
        sys.on(&codesign_key(&staged, TEAM_ID), err(3, CODESIGN_WRONG_TEAM));
        let items = expand(&t, &ctx).unwrap();
        let why = items[0].apply(&sys, &ctx).unwrap_err();
        assert!(why.contains("not by Developer ID team A66A9P66Z7"), "{why}");
        assert!(!sys.calls().iter().any(|c| c.starts_with("rename")));
    }

    #[test]
    fn the_command_is_a_link_to_the_app_s_executable() {
        let sys = Fake::root();
        let link = "/Users//admin/.local/bin/aterm";
        sys.stat_of(
            link,
            Stat {
                symlink: true,
                dir: false,
                ..dir()
            },
        );
        sys.on(
            &format!("/usr/bin/readlink {link}"),
            ok("/Applications/aterm.app/Contents/MacOS/aterm\n"),
        );
        let ctx = ctx_with(&sys, &[], true);
        let items = expand(&table(), &ctx).unwrap();
        assert_eq!(
            items[1].probe(&sys, &ctx),
            Probe::Ok {
                now:
                    "/Users//admin/.local/bin/aterm -> /Applications/aterm.app/Contents/MacOS/aterm"
                        .into()
            }
        );
        let sys = Fake::root();
        sys.stat_of(
            link,
            Stat {
                symlink: true,
                dir: false,
                ..dir()
            },
        );
        sys.on(
            &format!("/usr/bin/readlink {link}"),
            ok("/Users//admin/Applications/aterm.app/Contents/MacOS/aterm\n"),
        );
        let ctx = ctx_with(&sys, &[(INSTALL_SH_SHA, b"#!/bin/bash")], true);
        assert!(
            matches!(items[1].probe(&sys, &ctx), Probe::Change { from, .. } if from.starts_with("a link to /Users//admin/Applications"))
        );
    }

    #[test]
    fn install_sh_runs_as_the_user_from_the_root_only_cache_on_standard_input() {
        let ctx = ctx_with(&Fake::root(), &[], true);
        let cli = Cli {
            user: admin(),
            script: Some(INSTALL_SH_SHA.into()),
            toolset: false,
        };
        let cmd = cli.command(Path::new("/c/x"));
        assert_eq!(
            cmd.display(),
            "admin$ /bin/bash -s -- --no-app --no-toolchain"
        );
        assert_eq!(cmd.stdin.as_deref(), Some(Path::new("/c/x")));
        assert_eq!(cmd.budget, CLI_BUDGET);
        let cli = Cli {
            toolset: true,
            ..cli
        };
        assert_eq!(
            cli.command(Path::new("/c/x")).display(),
            "admin$ /bin/bash -s -- --no-app"
        );
        let sys = Fake::root();
        let script = b"main \"$@\"\n";
        let sha = sha_of(script);
        let mut t = table();
        t.install_sh_sha256 = Some(sha.clone());
        let ctx2 = ctx_with(&sys, &[(&sha, script)], ctx.disrupt);
        let link = "/Users//admin/.local/bin/aterm";
        sys.on(
            &format!("/usr/bin/readlink {link}"),
            ok(&format!("{EXE}\n")),
        );
        sys.on(
            "admin$ /bin/bash -s -- --no-app --no-toolchain",
            ok(INSTALL_SH_OUT),
        );
        let items = expand(&t, &ctx2).unwrap();
        assert!(
            matches!(items[1].probe(&sys, &ctx2), Probe::Change { from, .. } if from == "absent")
        );
        items[1].apply(&sys, &ctx2).unwrap();
        sys.stat_of(
            link,
            Stat {
                symlink: true,
                dir: false,
                ..dir()
            },
        );
        assert!(matches!(items[1].probe(&sys, &ctx2), Probe::Ok { .. }));
    }

    #[test]
    fn the_toolset_is_done_when_atpkg_s_doctor_is_and_a_held_lock_blocks_rather_than_fails() {
        let sys = Fake::root();
        sys.file(EXE, b"");
        let doctor = "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg doctor --json";
        sys.on(doctor, out_status(1, DOCTOR_EMPTY));
        let install = "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg install --default-set --wait-lock 1800";
        sys.on(
            install,
            Ran {
                status: Some(75),
                stdout: Vec::new(),
                stderr: b"atpkg: another atpkg run holds the store lock".to_vec(),
            },
        );
        let ctx = ctx_with(&sys, &[], true);
        let mut t = table();
        t.toolset = Some(true);
        let items = expand(&t, &ctx).unwrap();
        let toolset = &items[2..3];
        let rows = crate::engine::run(
            toolset,
            Mode::Plan,
            &sys,
            &ctx,
            &mut crate::engine::NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Planned);
        assert_eq!(
            rows[0].from.as_deref(),
            Some("base.pkg.toolset: no ALab programs are installed"),
            "the row that fails doctor, by its id; the advisories are not why"
        );
        let rows = crate::engine::run(
            toolset,
            Mode::Apply,
            &sys,
            &ctx,
            &mut crate::engine::NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{:?}", rows[0]);
        assert!(rows[0].why.as_deref().unwrap().contains("exit 75"));
        assert!(!rows[0].outcome.is_failure());
        let sys = Fake::root();
        sys.file(EXE, b"");
        sys.on(doctor, out_status(1, DOCTOR_EMPTY));
        sys.on(doctor, ok(DOCTOR_FULL));
        sys.on(install, ok(""));
        let rows = crate::engine::run(
            toolset,
            Mode::Apply,
            &sys,
            &ctx,
            &mut crate::engine::NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0]);
        assert_eq!(
            items[2].probe(&sys, &ctx),
            Probe::Ok {
                now: "12 program(s) active; 1 thing(s) to look at".into()
            }
        );
    }

    #[test]
    fn a_broken_shim_is_a_change_of_the_toolset_row_not_no_answer() {
        let sys = Fake::root();
        sys.file(EXE, b"");
        sys.on(
            "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg doctor --json",
            out_status(1, DOCTOR_BROKEN_SHIM),
        );
        let ctx = ctx_with(&sys, &[], true);
        let mut t = table();
        t.toolset = Some(true);
        let items = expand(&t, &ctx).unwrap();
        assert_eq!(
            items[2].probe(&sys, &ctx),
            Probe::Change {
                from: "base.pkg.shims@cargo: broken bin shim /Users//admin/Library/Application Support/aterm/pkg/bin/cargo".into(),
                to: "the ALab default set, complete".into(),
            }
        );
    }

    #[test]
    fn a_doctor_exit_1_with_no_failing_row_is_no_answer() {
        let doctor = "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg doctor --json";
        let mut t = table();
        t.toolset = Some(true);
        let complete = DOCTOR_FULL;
        let unnamed = DOCTOR_FULL.replace(r#""complete":true"#, r#""complete":false"#);
        for (ran, says) in [
            (err(1, ""), "exit 1"),
            (out_status(1, complete), "exit 1"),
            (out_status(1, &unnamed), "names no row"),
            (out_status(0, DOCTOR_EMPTY), "exit 0"),
            (out_status(0, "doctor: healthy\n"), "not its report"),
        ] {
            let sys = Fake::root();
            sys.file(EXE, b"");
            sys.on(doctor, ran);
            let ctx = ctx_with(&sys, &[], true);
            let items = expand(&t, &ctx).unwrap();
            let probe = items[2].probe(&sys, &ctx);
            assert!(
                matches!(&probe, Probe::Unusable { why } if why.contains(says)),
                "{says}: {probe:?}"
            );
        }
    }

    #[test]
    fn a_declined_toolset_is_a_change_the_owner_s_baseline_lifts() {
        let sys = Fake::root();
        sys.file(EXE, b"");
        sys.on(
            "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg doctor --json",
            ok(DOCTOR_DECLINED),
        );
        let ctx = ctx_with(&sys, &[], true);
        let mut t = table();
        t.toolset = Some(true);
        let items = expand(&t, &ctx).unwrap();
        assert_eq!(
            items[2].probe(&sys, &ctx),
            Probe::Change {
                from: "base.pkg.toolset: the ALab toolset was removed on this machine (`aterm pkg install --default-set` reinstalls it)".into(),
                to: "the ALab default set, complete".into(),
            }
        );
    }

    #[test]
    fn a_report_of_another_schema_or_without_the_toolset_row_is_no_answer() {
        let doctor = "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg doctor --json";
        let mut t = table();
        t.toolset = Some(true);
        let schema_2 = DOCTOR_FULL.replace(r#""schema":1"#, r#""schema":2"#);
        let planned = DOCTOR_FULL.replace(r#""mode":"check""#, r#""mode":"plan""#);
        let no_toolset =
            DOCTOR_FULL.replace(r#""id":"base.pkg.toolset""#, r#""id":"base.pkg.other""#);
        for (ran, says) in [
            (out_status(0, &schema_2), "schema 2"),
            (
                out_status(
                    1,
                    &schema_2.replace(r#""complete":true"#, r#""complete":false"#),
                ),
                "schema 2",
            ),
            (out_status(0, &planned), "not a check"),
            (out_status(0, &no_toolset), "no toolset row"),
        ] {
            let sys = Fake::root();
            sys.file(EXE, b"");
            sys.on(doctor, ran);
            let ctx = ctx_with(&sys, &[], true);
            let items = expand(&t, &ctx).unwrap();
            let probe = items[2].probe(&sys, &ctx);
            assert!(
                matches!(&probe, Probe::Unusable { why } if why.contains(says)),
                "{says}: {probe:?}"
            );
        }
    }

    #[test]
    fn an_aterm_without_doctor_json_is_no_answer_that_says_so() {
        let sys = Fake::root();
        sys.file(EXE, b"");
        sys.on(
            "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg doctor --json",
            err(2, "atpkg: doctor takes no arguments (got --json)"),
        );
        let ctx = ctx_with(&sys, &[], true);
        let mut t = table();
        t.toolset = Some(true);
        let items = expand(&t, &ctx).unwrap();
        let probe = items[2].probe(&sys, &ctx);
        assert!(
            matches!(&probe, Probe::Unusable { why } if why.contains("exit 2")
                && why.contains("an aterm older than `pkg doctor --json`")),
            "{probe:?}"
        );
    }

    #[test]
    fn any_other_atpkg_failure_is_a_failure() {
        let sys = Fake::root();
        sys.on(
            "admin$ /Applications/aterm.app/Contents/MacOS/aterm pkg install --default-set --wait-lock 1800",
            err(1, "atpkg: default-set bootstrap failed"),
        );
        let ctx = ctx_with(&sys, &[], true);
        let mut t = table();
        t.toolset = Some(true);
        let items = expand(&t, &ctx).unwrap();
        assert!(
            items[2]
                .apply(&sys, &ctx)
                .unwrap_err()
                .contains("bootstrap failed")
        );
        assert_eq!(items[2].stood_aside(), None);
    }

    #[test]
    fn always_on_desktop_installs_vault_server_launchdaemon_before_login_and_retires_user_agent() {
        let pmset_always_on = "AC Power:\n sleep\t\t0\n displaysleep\t15\n";
        let print_cmd = format!(
            "{} print system/{}",
            super::super::sharing::LAUNCHCTL,
            aterm_secrets::service::DAEMON_LABEL
        );
        let enable_cmd = format!(
            "{} enable system/{}",
            super::super::sharing::LAUNCHCTL,
            aterm_secrets::service::DAEMON_LABEL
        );
        let bootstrap_cmd = format!(
            "{} bootstrap system {}",
            super::super::sharing::LAUNCHCTL,
            aterm_secrets::service::DAEMON_PLIST
        );
        let kickstart_cmd = format!(
            "{} kickstart system/{}",
            super::super::sharing::LAUNCHCTL,
            aterm_secrets::service::DAEMON_LABEL
        );
        let user_agent_plist =
            "/Users//admin/Library/LaunchAgents/systems.alab.aterm-secrets-server.plist";

        let sys = Fake::root();
        sys.file(EXE, b"");
        // Negative control: before login (`console_user == None` after a reboot), a per-user
        // LaunchAgent plist on disk is not loaded in `system` and leaves the vault server absent.
        sys.file(user_agent_plist, b"<plist/>");
        sys.on("/usr/bin/pmset -g custom", ok(pmset_always_on));
        sys.on(&print_cmd, err(super::super::sharing::NOT_LOADED, ""));
        sys.on(&print_cmd, err(super::super::sharing::NOT_LOADED, ""));
        sys.on(
            &print_cmd,
            ok("system/systems.alab.aterm-vault-server = {\n\tpid = 7484\n}\n"),
        );

        let mut ctx = ctx_with(&sys, &[], true);
        ctx.facts.form = crate::model::Form::Desktop;
        ctx.facts.console_user = None;
        ctx.console = None;
        ctx.host_name = "m19-Mac-Studio-M4-Max".into();

        let t = table();
        assert_eq!(t.vault_server, None, "C7a LaunchDaemon defaults on (F6)");
        let items = expand(&t, &ctx).unwrap();
        let daemon = items
            .iter()
            .find(|item| item.id() == "aterm.vault-server")
            .unwrap();
        assert_eq!(daemon.after(), ["aterm.app", "power.ac.sleep"]);
        assert_eq!(
            daemon.probe(&sys, &ctx),
            Probe::Change {
                from: "not installed".into(),
                to: "systems.alab.aterm-vault-server running in system (advertising m19)".into(),
            }
        );

        sys.on("/usr/bin/pmset -g custom", ok(pmset_always_on));
        sys.on(
            &format!(
                "{} bootout gui/501/systems.alab.aterm-secrets-server",
                super::super::sharing::LAUNCHCTL
            ),
            err(super::super::sharing::NO_SUCH_PROCESS, ""),
        );
        sys.on(
            &format!(
                "{} bootout user/501/systems.alab.aterm-secrets-server",
                super::super::sharing::LAUNCHCTL
            ),
            err(super::super::sharing::NO_SUCH_PROCESS, ""),
        );
        sys.on(&enable_cmd, ok(""));
        sys.on(&bootstrap_cmd, ok(""));
        sys.on(&kickstart_cmd, ok(""));
        daemon.apply(&sys, &ctx).unwrap();

        assert!(
            sys.read_file(Path::new(user_agent_plist)).is_err(),
            "per-login LaunchAgent plist retired so port 7484 is not contested at login"
        );
        let written = String::from_utf8(
            sys.read_file(Path::new(aterm_secrets::service::DAEMON_PLIST))
                .unwrap(),
        )
        .unwrap();
        assert!(written.contains("<string>systems.alab.aterm-vault-server</string>"));
        assert!(written.contains("<string>--advertise</string>"));
        assert!(written.contains("<string>m19</string>"));
        assert!(!written.contains("LimitLoadToSessionType"));
        assert!(!written.contains("<key>HOME</key>"));

        // After reboot before any login (`console_user == None`), `system/systems.alab.aterm-vault-server`
        // is loaded and running.
        sys.on("/usr/bin/pmset -g custom", ok(pmset_always_on));
        assert_eq!(
            daemon.probe(&sys, &ctx),
            Probe::Ok {
                now: "systems.alab.aterm-vault-server running in system (pid 7484)".into(),
            }
        );
    }

    #[test]
    fn laptops_and_sleeping_desktops_keep_launchagent_and_vault_server_off_removes_daemon() {
        let sys = Fake::root();
        sys.file(EXE, b"");
        let mut ctx = ctx_with(&sys, &[], true);
        ctx.host_name = "m22-MacBook-Pro".into();
        ctx.facts.form = crate::model::Form::Laptop;

        let mut t = table();
        assert_eq!(t.vault_server, None, "C7a LaunchDaemon defaults on (F6)");
        let items = expand(&t, &ctx).unwrap();
        let daemon = items.last().unwrap();
        assert!(daemon.after().is_empty());
        assert!(matches!(
            daemon.probe(&sys, &ctx),
            Probe::NotApplicable { ref why } if why.contains("LaunchAgent")
        ));
        let laptop_run = crate::engine::run(
            &items,
            Mode::Apply,
            &sys,
            &ctx,
            &mut crate::engine::NoJournal,
        );
        assert_eq!(laptop_run[0].outcome, Outcome::Blocked);
        assert_eq!(laptop_run.last().unwrap().outcome, Outcome::NotApplicable);

        ctx.facts.form = crate::model::Form::Desktop;
        sys.on("/usr/bin/pmset -g custom", ok("AC Power:\n sleep\t\t10\n"));
        assert!(matches!(
            daemon.probe(&sys, &ctx),
            Probe::NotApplicable { ref why } if why.contains("sleep = 10")
        ));

        t.vault_advertise = Some("-bad".into());
        assert!(expand(&t, &ctx).is_err());

        // Turning `vault_server = false` boots out and unlinks an installed daemon even when
        // `aterm.app` is blocked.
        let mut off_table = table();
        off_table.vault_server = Some(false);
        let off_items = expand(&off_table, &ctx).unwrap();
        let off = off_items.last().unwrap();
        assert!(off.after().is_empty());
        sys.file(aterm_secrets::service::DAEMON_PLIST, b"<plist/>");
        let print_cmd = format!(
            "{} print system/{}",
            super::super::sharing::LAUNCHCTL,
            aterm_secrets::service::DAEMON_LABEL
        );
        let bootout_cmd = format!(
            "{} bootout system/{}",
            super::super::sharing::LAUNCHCTL,
            aterm_secrets::service::DAEMON_LABEL
        );
        sys.on(
            &print_cmd,
            ok("system/systems.alab.aterm-vault-server = {\n\tpid = 7484\n}\n"),
        );
        sys.on(
            &print_cmd,
            ok("system/systems.alab.aterm-vault-server = {\n\tpid = 7484\n}\n"),
        );
        sys.on(&print_cmd, err(super::super::sharing::NOT_LOADED, ""));
        assert_eq!(
            off.probe(&sys, &ctx),
            Probe::Change {
                from: "running in system (pid 7484)".into(),
                to: "off".into(),
            }
        );
        sys.on(&bootout_cmd, ok(""));
        let off_run = crate::engine::run(
            &off_items,
            Mode::Apply,
            &sys,
            &ctx,
            &mut crate::engine::NoJournal,
        );
        assert_eq!(off_run[0].outcome, Outcome::Blocked);
        assert_eq!(off_run.last().unwrap().outcome, Outcome::Changed);
        assert!(
            sys.read_file(Path::new(aterm_secrets::service::DAEMON_PLIST))
                .is_err()
        );
    }
}
