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
use crate::port::{Cmd, Ran, System};
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
        swap_into(sys, &staged, Path::new(APP), stage)
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
        // Distinct branch.
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
        let ran = match sys.run(
            &self
                .pkg(&["doctor", "--json"])
                .budget(Duration::from_secs(300)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::apps::tests::{
        CODESIGN_WRONG_TEAM, SPCTL_KYA_ON, admin, codesign_key, ctx_with, dir, err, ok, out_status,
        sha_of, stage_path,
    };
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran, Stat};

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
        assert_eq!(ids, ["aterm.app", "aterm.cli@admin", "aterm.toolset@admin"]);
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
        assert_eq!(items[0].detail(&sys, &ctx), None, "Gatekeeper accepted it");
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
        let toolset = &items[2..];
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
}
