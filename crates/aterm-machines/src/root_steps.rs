// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The root steps of setup's one early elevation besides Apple's Command Line Tools
//! ([`crate::init_clt`]), which aterm asks for by itself, once, as a Mac's first launch begins
//! (owner ruling 2026-11-02: setup starts and finishes by itself; aterm raises only the prompts
//! macOS alone can answer, early and batched, and never waits on them):
//!
//! - developer mode (`DevToolsSecurity -enable`), with the console account authorized as an
//!   administrator or in `_developer`, so a debugger attaches with no SamplingTools dialog
//!   ([`DeveloperMode`]);
//! - the one narrow rule ([`ADMITTED_NAME_RULE`], [`name_rule`]) that lets that person's account
//!   set this Mac's ComputerName, LocalHostName and HostName to a swarm label (`m` and a number)
//!   with Apple's own `scutil`, and nothing else ([`NameRule`]), so the name the vault's roster
//!   gives the Mac at an admission that comes after the elevation applies with no second
//!   password ([`name_by_rule`]);
//! - the root LAN broker ([`crate::items::lan::LanBroker`]), a resident root LaunchDaemon through
//!   which all of aterm's own outbound LAN traffic goes, so macOS never asks its Local Network
//!   question about aterm (`docs/DESIGN-lan-broker-2026-11-05.md`).
//!
//! Why a sudoers rule for the name, and not a privileged helper or a second password: a second
//! password is a second prompt, which the ruling rules out, and a helper would put root code of
//! aterm's behind an input the person's account controls where a rule needs none. The rule runs no
//! code of aterm's as root: sudo matches the whole argument string against
//! `^--set <Pref> m<digits>$` and runs SIP-protected `/usr/sbin/scutil`, so the most the account
//! can do with it is rename this Mac to another swarm label. `visudo -c` checks the rule before it
//! goes live, so a sudo that cannot read it leaves none, and the name then waits for the next
//! elevation.
//!
//! The broker reverses this module's earlier refusal of any resident root helper, for LAN access
//! alone, because nothing else works: macOS exempts from Local Network privacy only a process
//! running as root (`docs/measured/local-network-exemption-2026-11-05.md`), and its question is an
//! alert the rulings exclude. The foothold that refusal named (root code whose inputs the person's
//! account controls, whose binary must stay root-owned beside an app the person updates) is kept
//! narrow: launchd runs a `root:wheel` clone of the app with no shared write bit, never the
//! person-owned bundle; the broker adopts an update only when its static signature meets the
//! running image's own team requirement (an ad-hoc image adopts nothing, and the next elevation
//! renews it); its surface is one Unix socket that opens sockets to local-network destinations and
//! makes `DNSService*` calls, running no code, opening no file and binding nothing a client names;
//! and it admits a client only by that client's code signature. A compromised aterm process gains
//! LAN sockets from it, which the app had whenever a person pressed Allow, and nothing that runs as
//! root.
//!
//! What the person's account can read of each step without root ([`owed`]) decides whether the
//! independent root work is owed, including when the compiler refuses or cannot be read:
//! an unreadable root step is not owed, so no password is asked for want of a reading.

use crate::items::{Ctx, Item};
use crate::model::{Probe, Read};
use crate::port::{Cmd, Ran, System};
use aterm_containment::consent::{ADMIN_GROUP, DebuggerMember, classify_membership};
use std::path::Path;

pub use aterm_types::macos_setup::ADMITTED_NAME_RULE;

/// Apple's developer-mode switch.
pub const DEVTOOLS_SECURITY: &str = "/usr/sbin/DevToolsSecurity";

/// Directory Services' group editor.
pub const DSEDITGROUP: &str = "/usr/sbin/dseditgroup";

/// The group whose members a debugger authorizes once developer mode is on.
pub const DEVELOPER_GROUP: &str = "_developer";

/// sudo's own syntax check.
pub const VISUDO: &str = "/usr/sbin/visudo";

/// `sudo`.
pub const SUDO: &str = "/usr/bin/sudo";

/// Where a rule this Mac's `visudo` refused is kept
/// ([`aterm_types::macos_setup::ADMITTED_NAME_RULE_REFUSED`]): the next elevation that runs for
/// another step tries the rule once more and removes it once the rule goes live.
pub const REFUSED_RULE: &str = aterm_types::macos_setup::ADMITTED_NAME_RULE_REFUSED;

/// The names the rule lets the account set, as `scutil` spells them.
pub const NAME_PREFS: [&str; 3] = ["ComputerName", "LocalHostName", "HostName"];

/// A swarm label as the rule matches it: `m` and one to six digits, no leading zero
/// ([`crate::init_clt::plain_name`]). No comma, colon, equals sign or backslash, which sudoers
/// would read as its own syntax.
const LABEL_PATTERN: &str = "m[1-9][0-9]?[0-9]?[0-9]?[0-9]?[0-9]?";

/// A root step the elevation still owes this Mac, beside the tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owed {
    /// Developer mode is off, or the console account is neither an admin nor a developer.
    Debugger,
    /// The rule that names the Mac after admission is not in place.
    NameRule,
    /// The root LAN broker's image or plist is missing or not this build's, save an image of the
    /// source's own signing team, which the running broker adopts by itself
    /// ([`crate::items::lan::reading`], [`aterm_uds::lan::install::Part::Adopting`]).
    LanBroker,
}

impl Owed {
    /// What the step does, in plain words.
    #[must_use]
    pub const fn plain(self) -> &'static str {
        match self {
            Self::Debugger => "authorizes this account to debug without a password",
            Self::NameRule => "lets aterm give this Mac the name its swarm admits it by",
            Self::LanBroker => {
                "installs the helper that lets aterm reach this network's Macs without macOS \
                 asking"
            }
        }
    }
}

/// `DevToolsSecurity -status`, which needs no authorization.
fn status_cmd() -> Cmd {
    Cmd::new(DEVTOOLS_SECURITY).arg("-status")
}

/// Whether the complete `-status` run definitely said developer mode is on or off, using the
/// same classifier as the prompt-free GUI probe; unreadable or ambiguous output admits no step.
#[must_use]
pub fn developer_mode(ran: &Ran) -> Option<bool> {
    use aterm_containment::consent::{DebuggerMode, ToolRun, classify_devtools_status};
    let run = ToolRun::Exited {
        code: ran.status,
        stdout: std::str::from_utf8(&ran.stdout).ok()?.to_owned(),
        stderr: std::str::from_utf8(&ran.stderr).ok()?.to_owned(),
    };
    match classify_devtools_status(&run).0 {
        DebuggerMode::Enabled => Some(true),
        DebuggerMode::Disabled => Some(false),
        DebuggerMode::Unrecognized | DebuggerMode::Unknown => None,
    }
}

fn developer_mode_now(sys: &dyn System) -> Option<bool> {
    sys.run_complete_output(&status_cmd())
        .ok()
        .as_ref()
        .and_then(developer_mode)
}

/// A definite `dseditgroup -o checkmember` answer: one UTF-8 stdout line, no diagnostic,
/// and a matching exit. A negative reading may exit 67 (`EX_NOUSER`); an arbitrary failure
/// or signal is not an answer, even when stdout starts with `yes` or `no`.
#[must_use]
pub fn member(ran: &Ran) -> Option<bool> {
    if !ran.stderr.is_empty() {
        return None;
    }
    let out = std::str::from_utf8(&ran.stdout)
        .ok()?
        .trim_end_matches(['\r', '\n']);
    if out.contains(['\r', '\n']) {
        return None;
    }
    match ran.status {
        Some(0) if out.starts_with("yes ") => Some(true),
        Some(0 | 67) if out.starts_with("no ") => Some(false),
        _ => None,
    }
}

fn member_now(sys: &dyn System, user: &str, group: &str) -> Option<bool> {
    let cmd = Cmd::new(DSEDITGROUP).args(["-o", "checkmember", "-m", user, group]);
    sys.run_complete_output(&cmd).ok().as_ref().and_then(member)
}

/// The root item's definite missing state: an unreadable mode admits no elevation, and an
/// unreadable membership is no proof that the account needs adding.
fn debugger_owed(mode: Option<bool>, member: DebuggerMember) -> bool {
    matches!(
        (mode, member),
        (Some(false), _) | (Some(true), DebuggerMember::NotMember)
    )
}

/// The same authorization rules the prompt-free Permissions probe uses: admins need no
/// developer-group entry; a non-admin's definite absence is the membership root must add.
fn debugger_member_now(sys: &dyn System, user: &crate::model::User) -> DebuggerMember {
    let admin = if user.admin {
        Some(true)
    } else {
        // The direct account roster can miss nested admin membership. Ask Directory Services
        // before treating its false flag as proof that this named account needs adding.
        member_now(sys, &user.name, ADMIN_GROUP)
    };
    let developer = (admin == Some(false))
        .then(|| member_now(sys, &user.name, DEVELOPER_GROUP))
        .flatten();
    classify_membership(admin, developer)
}

/// Membership of this process's account only while it is the account the root batch would
/// change at the console. No other account's membership can admit this login's elevation.
fn console_member_now(sys: &dyn System) -> DebuggerMember {
    if sys.euid() == 0 {
        return DebuggerMember::Unknown;
    }
    let Ok(identity) = sys.identity() else {
        return DebuggerMember::Unknown;
    };
    let Ok(console) = sys.stat(Path::new("/dev/console")) else {
        return DebuggerMember::Unknown;
    };
    if identity.user.uid != sys.euid() || console.uid != identity.user.uid {
        return DebuggerMember::Unknown;
    }
    debugger_member_now(sys, &identity.user)
}

/// Whether the rule is in place as a stat reads it, which needs no root (the include directory
/// is world-searchable, the rule itself readable by root alone): a root-owned regular file of
/// mode 0440 with nonzero length. `None` when the stat answered with anything but not-found.
#[must_use]
pub fn rule_in_place(sys: &dyn System) -> Option<bool> {
    match sys.stat(Path::new(ADMITTED_NAME_RULE)) {
        Ok(stat) => {
            Some(!stat.dir && !stat.symlink && stat.uid == 0 && stat.mode == 0o440 && stat.len > 0)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// What the elevation still owes this Mac beside the tools, as the person's account reads it.
#[must_use]
pub fn owed(sys: &dyn System) -> Vec<Owed> {
    let mut owed = Vec::new();
    let mode = developer_mode_now(sys);
    let member = if mode == Some(true) {
        console_member_now(sys)
    } else {
        DebuggerMember::Unknown
    };
    if debugger_owed(mode, member) {
        owed.push(Owed::Debugger);
    }
    if rule_in_place(sys) == Some(false)
        && matches!(sys.stat(Path::new(REFUSED_RULE)), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        owed.push(Owed::NameRule);
    }
    if crate::items::lan::reading(sys).is_some_and(|reading| reading.is_ok_and(|r| r.owed())) {
        owed.push(Owed::LanBroker);
    }
    owed
}

/// [`owed`] on this Mac, for `aterm init`'s item ([`crate::init_clt::CommandLineTools::with_owed`]).
#[must_use]
pub fn owed_here(sys: &dyn System) -> Vec<Owed> {
    if cfg!(target_os = "macos") {
        owed(sys)
    } else {
        Vec::new()
    }
}

/// An account name the rule may carry: letters, digits, `_`, `.` and `-`, not leading with `-`,
/// and never root, whose rule would add nothing. Anything else gets no rule.
fn rule_user(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 64
        && user != "root"
        && !user.starts_with('-')
        && user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// The rule's text for `user`: one line per name, each matching `--set <Pref> <label>` and
/// nothing more. `None` for an account name the rule may not carry ([`rule_user`]).
#[must_use]
pub fn name_rule(user: &str) -> Option<String> {
    if !rule_user(user) {
        return None;
    }
    let scutil = crate::items::names::SCUTIL;
    let mut text = String::from(
        "# aterm's setup elevation: lets one account set this Mac's names to a swarm label\n\
         # (m and a number) with Apple's scutil, and nothing else.\n",
    );
    for pref in NAME_PREFS {
        text.push_str(&format!(
            "{user} ALL = (root) NOPASSWD: {scutil} ^--set {pref} {LABEL_PATTERN}$\n"
        ));
    }
    Some(text)
}

/// Run `cmd`; its result when it exited 0, else one line naming it and how it ended.
fn run_ok(sys: &dyn System, cmd: &Cmd) -> Result<Ran, String> {
    let ran = sys
        .run(cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    if ran.success() {
        Ok(ran)
    } else {
        Err(format!("{}: {}", cmd.display(), ran.describe()))
    }
}

/// Give this Mac `label` (a [`crate::init_clt::plain_name`]) as its three names through the rule,
/// as the person's own account, with `sudo -n`, which never asks for a password: the step an
/// admission after the elevation takes. The names already `label` are left; the ones set are
/// returned. `current` re-reads the caller's naming intent after each name read, before any
/// write, so a newer choice retires this work without undoing an earlier name already set.
///
/// # Errors
/// A label that is no swarm label, no rule in place (the next elevation names the Mac), or a
/// name `sudo` or `scutil` refused, or `current` no longer admits this intent, in one line.
pub fn name_by_rule(
    sys: &dyn System,
    label: &str,
    current: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<&'static str>, String> {
    let Some(label) = crate::init_clt::plain_name(label) else {
        return Err(format!("{label:?} is no swarm label"));
    };
    if rule_in_place(sys) != Some(true) {
        return Err(
            "the rule that names this Mac is not in place, so setup's next Mac password names it"
                .to_owned(),
        );
    }
    let scutil = crate::items::names::SCUTIL;
    let mut set = Vec::new();
    for pref in NAME_PREFS {
        let get = Cmd::new(scutil).args(["--get", pref]);
        let now = sys.run(&get).map_or_else(
            |e| Read::Unusable(e.to_string()),
            |ran| crate::items::names::read_scutil(&ran),
        );
        current()?;
        if let Read::Value(now) = &now
            && aterm_types::machine_name::OsSlot::from_macos_preference(pref)
                .is_some_and(|slot| slot.accepts_projection(&label, now))
        {
            continue;
        }
        run_ok(
            sys,
            &Cmd::new(SUDO).args(["-n", scutil, "--set", pref, label.as_str()]),
        )?;
        set.push(pref);
    }
    Ok(set)
}

/// `[developer] debugger = true`: developer mode on, and the console account authorized as an
/// admin or in [`DEVELOPER_GROUP`]. Applied as root, by the elevation's root half.
pub struct DeveloperMode;

impl DeveloperMode {
    fn read(sys: &dyn System, ctx: &Ctx) -> (Option<bool>, Option<(String, DebuggerMember)>) {
        let user = ctx
            .console
            .as_ref()
            .map(|user| (user.name.clone(), debugger_member_now(sys, user)));
        (developer_mode_now(sys), user)
    }
}

impl Item for DeveloperMode {
    fn id(&self) -> String {
        "developer.debugger".to_owned()
    }

    fn family(&self) -> &'static str {
        "developer"
    }

    fn title(&self) -> String {
        "developer mode, so debuggers attach without a password".to_owned()
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let (mode, user) = Self::read(sys, ctx);
        let Some(mode) = mode else {
            return Probe::Unusable {
                why: format!("{} did not say", status_cmd().display()),
            };
        };
        if mode && let Some((name, DebuggerMember::Unknown)) = &user {
            return Probe::Unusable {
                why: format!("debugger authorization for {name} could not be read"),
            };
        }
        let missing = user
            .as_ref()
            .filter(|(_, member)| *member == DebuggerMember::NotMember)
            .map(|(name, _)| name.as_str());
        if !debugger_owed(
            Some(mode),
            user.as_ref()
                .map_or(DebuggerMember::Unknown, |(_, member)| *member),
        ) {
            Probe::Ok {
                now: "on".to_owned(),
            }
        } else {
            Probe::Change {
                from: format!(
                    "{}{}",
                    if mode { "on" } else { "off" },
                    missing.map_or_else(String::new, |name| format!(
                        ", {name} not in {DEVELOPER_GROUP}"
                    ))
                ),
                to: format!(
                    "on{}",
                    missing
                        .map_or_else(String::new, |name| format!(", {name} in {DEVELOPER_GROUP}"))
                ),
            }
        }
    }

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        let (mode, user) = Self::read(sys, ctx);
        let mode = mode.ok_or_else(|| format!("{} did not say", status_cmd().display()))?;
        if !mode {
            run_ok(sys, &Cmd::new(DEVTOOLS_SECURITY).arg("-enable"))?;
        }
        if let Some((name, member)) = user
            && member == DebuggerMember::NotMember
        {
            run_ok(
                sys,
                &Cmd::new(DSEDITGROUP).args([
                    "-o",
                    "edit",
                    "-a",
                    name.as_str(),
                    "-t",
                    "user",
                    DEVELOPER_GROUP,
                ]),
            )?;
        }
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        Some(Cmd::new(DEVTOOLS_SECURITY).arg("-disable").display())
    }
}

/// `[names] admitted_name_rule = true`: the rule ([`name_rule`]) for the console account, in
/// place as root wrote it. Applied as root, by the elevation's root half.
pub struct NameRule;

impl NameRule {
    fn wanted(ctx: &Ctx) -> Result<String, String> {
        let user = ctx
            .console
            .as_ref()
            .ok_or("no one is logged in at this Mac's console to give the rule to")?;
        name_rule(&user.name)
            .ok_or_else(|| format!("{:?} is no account name a rule carries", user.name))
    }
}

impl Item for NameRule {
    fn id(&self) -> String {
        "names.admitted-name-rule".to_owned()
    }

    fn family(&self) -> &'static str {
        "names"
    }

    fn title(&self) -> String {
        "the rule that lets aterm name this Mac by its swarm label".to_owned()
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let wanted = match Self::wanted(ctx) {
            Ok(text) => text,
            Err(why) => return Probe::Blocked { why },
        };
        let now = match sys.read_file(Path::new(ADMITTED_NAME_RULE)) {
            Ok(bytes) if bytes == wanted.as_bytes() && rule_in_place(sys) == Some(true) => {
                return Probe::Ok {
                    now: "in place".to_owned(),
                };
            }
            Ok(_) => "another rule".to_owned(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => "absent".to_owned(),
            Err(error) => {
                return Probe::Unusable {
                    why: format!("{ADMITTED_NAME_RULE}: {error}"),
                };
            }
        };
        Probe::Change {
            from: now,
            to: format!("{ADMITTED_NAME_RULE}, checked by visudo"),
        }
    }

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        let wanted = Self::wanted(ctx)?;
        let dir = Path::new(ADMITTED_NAME_RULE)
            .parent()
            .ok_or("the naming rule has no parent directory")?;
        // macOS's sudoers includes the directory; a Mac that has none gets it, as root's.
        sys.create_dir_all(dir, 0o755)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let allocation = crate::port::Scratch::new_in(sys, dir, "admitted-name")
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let staged = allocation.path().join("rule");
        sys.write_file(&staged, wanted.as_bytes(), 0o440)
            .map_err(|e| format!("{}: {e}", staged.display()))?;
        let checked = run_ok(sys, &Cmd::new(VISUDO).args(["-c", "-q", "-f"]).arg(&staged));
        if let Err(why) = checked {
            // Kept where sudo never reads it, so the rule stops being owed ([`REFUSED_RULE`]).
            let _ = sys.rename(&staged, Path::new(REFUSED_RULE));
            return Err(why);
        }
        sys.rename(&staged, Path::new(ADMITTED_NAME_RULE))
            .map_err(|e| format!("{ADMITTED_NAME_RULE}: {e}"))?;
        let _ = sys.remove(Path::new(REFUSED_RULE));
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        Some(format!("rm {ADMITTED_NAME_RULE}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::defaults::tests::{ctx, user};
    use crate::port::{Fake, Stat};

    const STATUS: &str = "/usr/sbin/DevToolsSecurity -status";

    fn console() -> Ctx {
        let mut ctx = ctx(&[]);
        let mut user = user("alice", 501);
        user.admin = false;
        ctx.console = Some(user);
        ctx
    }

    fn rule_stat(uid: u32, mode: u32) -> Stat {
        Stat {
            uid,
            gid: 0,
            mode,
            dir: false,
            symlink: false,
            len: 1,
        }
    }

    /// The account owes developer mode while its status says off, and naming while no
    /// nonempty root-owned 0440 file stands. Unknown status or metadata admits no elevation.
    /// NEGATIVE CONTROLS: empty, directory, symlink and account-writable rules are not in
    /// place; an unread refusal record is not evidence that no earlier refusal exists.
    #[test]
    fn owed_reads_each_step_and_an_unreadable_one_owes_nothing() {
        let sys = Fake::root();
        sys.on(STATUS, Ran::ok("Developer mode is currently disabled.\n"));
        assert_eq!(owed(&sys), vec![Owed::Debugger, Owed::NameRule]);

        let on = Fake::root();
        on.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
            .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440));
        assert_eq!(owed(&on), Vec::<Owed>::new());

        let unread = Fake::root();
        unread
            .on(
                STATUS,
                Ran::ok("Developer mode rights are in an unknown state.\n"),
            )
            .stat_fails(ADMITTED_NAME_RULE, std::io::ErrorKind::PermissionDenied);
        assert_eq!(owed(&unread), Vec::<Owed>::new());

        for stat in [
            rule_stat(501, 0o440),
            rule_stat(0, 0o666),
            Stat {
                len: 0,
                ..rule_stat(0, 0o440)
            },
            Stat {
                dir: true,
                ..rule_stat(0, 0o440)
            },
            Stat {
                symlink: true,
                ..rule_stat(0, 0o440)
            },
        ] {
            let planted = Fake::root();
            planted
                .on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
                .stat_of(ADMITTED_NAME_RULE, stat);
            assert_eq!(rule_in_place(&planted), Some(false));
            assert_eq!(owed(&planted), vec![Owed::NameRule]);
            assert!(name_by_rule(&planted, "m42", &|| Ok(())).is_err());
            assert!(!planted.calls().iter().any(|call| call.contains("scutil")));
        }
        // An unread refusal record is not evidence that no earlier refusal exists.
        for (kind, missing) in [
            (std::io::ErrorKind::NotFound, true),
            (std::io::ErrorKind::PermissionDenied, false),
            (std::io::ErrorKind::Other, false),
        ] {
            let sys = Fake::root();
            sys.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
                .stat_fails(REFUSED_RULE, kind);
            assert_eq!(owed(&sys).contains(&Owed::NameRule), missing, "{kind:?}");
        }
    }

    /// The raw process boundary requires complete UTF-8 streams and a successful definite
    /// answer. Neither a valid prefix before corrupt bytes nor a failed tool can authorize
    /// the root item, even if the other stream looks like a disabled-mode sentence.
    #[test]
    fn debugger_setup_refuses_incomplete_or_unsuccessful_tool_results() {
        let disabled = b"Developer mode is currently disabled.\n";
        let mut corrupt = disabled.to_vec();
        corrupt.push(0xff);
        for ran in [
            Ran {
                status: Some(0),
                stdout: corrupt,
                stderr: Vec::new(),
            },
            Ran {
                status: Some(0),
                stdout: disabled.to_vec(),
                stderr: vec![0xff],
            },
            Ran::exit(1, "Developer mode is currently disabled.\n"),
            Ran {
                status: None,
                stdout: disabled.to_vec(),
                stderr: Vec::new(),
            },
        ] {
            assert_eq!(developer_mode(&ran), None, "{ran:?}");
            let sys = Fake::root();
            sys.on(STATUS, ran.clone());
            assert!(matches!(
                DeveloperMode.probe(&sys, &ctx(&[])),
                Probe::Unusable { .. }
            ));
            assert!(!owed(&sys).contains(&Owed::Debugger));
            // A known-off probe can become unread before apply; the old plan grants no
            // authority to enable the switch from that new unknown reading.
            let stale = Fake::root();
            stale
                .on(STATUS, Ran::ok("Developer mode is currently disabled.\n"))
                .on(STATUS, ran)
                .on("/usr/sbin/DevToolsSecurity -enable", Ran::ok(""));
            assert!(matches!(
                DeveloperMode.probe(&stale, &ctx(&[])),
                Probe::Change { .. }
            ));
            assert!(DeveloperMode.apply(&stale, &ctx(&[])).is_err());
            assert!(
                !stale
                    .calls()
                    .iter()
                    .any(|call| call.contains("-enable") || call.contains("-o edit"))
            );
        }
    }

    #[test]
    fn membership_admission_requires_a_complete_answer_and_expected_exit() {
        let yes = b"yes alice is a member of _developer\n";
        let no = b"no alice is NOT a member of _developer\n";
        let reading = |status, stdout: &[u8], stderr: &[u8]| Ran {
            status,
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        };
        for (ran, expected) in [
            (reading(Some(0), yes, b""), Some(true)),
            (reading(Some(0), no, b""), Some(false)),
            (reading(Some(67), no, b""), Some(false)),
        ] {
            assert_eq!(member(&ran), expected);
        }
        let admin = "/usr/sbin/dseditgroup -o checkmember -m alice admin";
        let developer = "/usr/sbin/dseditgroup -o checkmember -m alice _developer";
        for ran in [
            reading(Some(1), yes, b""),
            reading(Some(1), no, b""),
            reading(Some(67), yes, b""),
            reading(None, yes, b""),
            reading(None, no, b""),
            reading(Some(0), b"yes alice is a member\xff", b""),
            reading(Some(0), b"no alice is NOT a member\xff", b""),
            reading(Some(0), yes, b"\xff"),
            reading(Some(0), yes, b"directory service warning\n"),
            reading(
                Some(0),
                b"yes alice is a member\nno alice is NOT a member\n",
                b"",
            ),
        ] {
            assert_eq!(member(&ran), None, "{ran:?}");
            for boundary in [admin, developer] {
                let ctx = console();
                let user = ctx.console.as_ref().unwrap();
                let mut sys = Fake::root();
                sys.euid = user.uid;
                sys.logged_in(user)
                    .stat_of("/dev/console", rule_stat(user.uid, 0o600))
                    .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440))
                    .on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
                    .on(
                        admin,
                        if boundary == admin {
                            ran.clone()
                        } else {
                            reading(Some(67), no, b"")
                        },
                    )
                    .on(
                        developer,
                        if boundary == developer {
                            ran.clone()
                        } else {
                            reading(Some(67), no, b"")
                        },
                    );
                assert!(!owed(&sys).contains(&Owed::Debugger), "{boundary}: {ran:?}");
                assert!(matches!(
                    DeveloperMode.probe(&sys, &ctx),
                    Probe::Unusable { .. }
                ));
                DeveloperMode.apply(&sys, &ctx).unwrap();
                assert!(
                    !sys.calls()
                        .iter()
                        .any(|call| call.contains("-o edit") || call.contains("-enable"))
                );
            }
        }
    }

    #[test]
    fn incomplete_root_query_capture_admits_no_debugger_elevation() {
        let admin = "/usr/sbin/dseditgroup -o checkmember -m alice admin";
        let developer = "/usr/sbin/dseditgroup -o checkmember -m alice _developer";
        for query in [STATUS, admin, developer] {
            let ctx = console();
            let user = ctx.console.as_ref().unwrap();
            let mut sys = Fake::root();
            sys.euid = user.uid;
            sys.logged_in(user)
                .stat_of("/dev/console", rule_stat(user.uid, 0o600))
                .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440))
                .on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
                .on(admin, Ran::ok("no alice is NOT a member of admin\n"))
                .on(
                    developer,
                    Ran::ok("no alice is NOT a member of _developer\n"),
                )
                .refuse_complete_stdout(query, "stdout did not reach EOF");
            assert!(
                matches!(DeveloperMode.probe(&sys, &ctx), Probe::Unusable { .. }),
                "{query}"
            );
            assert!(!owed(&sys).contains(&Owed::Debugger), "{query}");
        }
        // A mode sentence on stdout cannot override unproven diagnostic capture.
        let sys = Fake::root();
        sys.on(
            STATUS,
            Ran {
                status: Some(0),
                stdout: b"Developer mode is currently disabled.\n".to_vec(),
                stderr: vec![b' '; aterm_uds::deadline::OUTPUT_KEEP + 1],
            },
        );
        assert!(matches!(
            DeveloperMode.probe(&sys, &ctx(&[])),
            Probe::Unusable { .. }
        ));
        assert!(!owed(&sys).contains(&Owed::Debugger));
    }

    /// A Mac whose mode is already on still needs the root batch when its console account
    /// cannot use it. The admission and the root item's own probe agree; once membership is
    /// present neither owes a change. Both readings are prompt-free and mutate nothing.
    #[test]
    fn enabled_mode_still_owes_the_console_accounts_missing_membership() {
        let check = "/usr/sbin/dseditgroup -o checkmember -m alice _developer";
        let admin_check = "/usr/sbin/dseditgroup -o checkmember -m alice admin";
        for (admin, admin_answer, answer, missing) in [
            (
                false,
                "no alice is NOT a member of admin\n",
                "no alice is NOT a member of _developer\n",
                true,
            ),
            (
                false,
                "no alice is NOT a member of admin\n",
                "yes alice is a member of _developer\n",
                false,
            ),
            (
                false,
                "yes alice is a member of admin\n",
                "no alice is NOT a member of _developer\n",
                false,
            ),
            (
                true,
                "membership could not be read\n",
                "no alice is NOT a member of _developer\n",
                false,
            ),
        ] {
            let mut ctx = console();
            let user = ctx.console.as_mut().unwrap();
            user.admin = admin;
            let mut sys = Fake::root();
            sys.euid = user.uid;
            sys.logged_in(user)
                .stat_of("/dev/console", rule_stat(user.uid, 0o600))
                .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440))
                .on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
                .on(admin_check, Ran::ok(admin_answer))
                .on(check, Ran::ok(answer));
            assert_eq!(
                owed(&sys),
                if missing {
                    vec![Owed::Debugger]
                } else {
                    Vec::new()
                }
            );
            assert_eq!(
                matches!(DeveloperMode.probe(&sys, &ctx), Probe::Change { .. }),
                missing
            );
            if admin || admin_answer.starts_with("yes ") {
                DeveloperMode.apply(&sys, &ctx).unwrap();
                assert!(
                    !sys.calls().iter().any(|call| call == check),
                    "admins already have authorization: {:?}",
                    sys.calls()
                );
            }
            if admin {
                assert!(
                    !sys.calls().iter().any(|call| call == admin_check),
                    "a known admin needs no further membership check"
                );
            }
            assert!(
                sys.calls().iter().all(|call| {
                    !call.contains("-enable")
                        && !call.contains("-o edit")
                        && !call.starts_with("write ")
                }),
                "{:?}",
                sys.calls()
            );
        }
    }

    /// Membership that cannot be confirmed, or belongs to another console account, is no
    /// reason to ask this login for a password. An unreadable mode keeps the same policy even
    /// if a membership query could prove this account absent.
    #[test]
    fn uncertain_or_other_accounts_membership_admits_no_elevation() {
        let mut user = user("alice", 501);
        user.admin = false;
        let check = "/usr/sbin/dseditgroup -o checkmember -m alice _developer";
        let admin_check = "/usr/sbin/dseditgroup -o checkmember -m alice admin";
        for (console_uid, mode, admin_answer, answer, queries_member) in [
            (
                Some(501),
                "Developer mode is currently enabled.\n",
                "no alice is NOT a member of admin\n",
                "membership could not be read\n",
                true,
            ),
            (
                Some(501),
                "Developer mode is currently enabled.\n",
                "admin membership could not be read\n",
                "no alice is NOT a member of _developer\n",
                false,
            ),
            (
                Some(502),
                "Developer mode is currently enabled.\n",
                "no alice is NOT a member of admin\n",
                "no alice is NOT a member of _developer\n",
                false,
            ),
            (
                None,
                "Developer mode is currently enabled.\n",
                "no alice is NOT a member of admin\n",
                "no alice is NOT a member of _developer\n",
                false,
            ),
            (
                Some(501),
                "Developer mode rights are in an unknown state.\n",
                "no alice is NOT a member of admin\n",
                "no alice is NOT a member of _developer\n",
                false,
            ),
        ] {
            let mut sys = Fake::root();
            sys.euid = user.uid;
            sys.logged_in(&user)
                .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440))
                .on(STATUS, Ran::ok(mode))
                .on(admin_check, Ran::ok(admin_answer))
                .on(check, Ran::ok(answer));
            if let Some(uid) = console_uid {
                sys.stat_of("/dev/console", rule_stat(uid, 0o600));
            }
            assert_eq!(owed(&sys), Vec::<Owed>::new(), "{console_uid:?} {mode}");
            assert_eq!(
                sys.calls().iter().any(|call| call == check),
                queries_member,
                "{console_uid:?} {mode} {:?}",
                sys.calls()
            );
        }
    }

    /// A root batch already admitted for another step can enable mode without assigning a
    /// role Directory Services could not read. Readback retains the unread authorization;
    /// neither unknown admin membership nor unknown developer membership grants a new role.
    #[test]
    fn an_authorized_root_apply_never_grants_uncertain_membership() {
        let admin = "/usr/sbin/dseditgroup -o checkmember -m alice admin";
        let developer = "/usr/sbin/dseditgroup -o checkmember -m alice _developer";
        let enable = "/usr/sbin/DevToolsSecurity -enable";
        for (admin_answer, developer_answer) in [
            (
                "admin membership could not be read\n",
                "no alice is NOT a member of _developer\n",
            ),
            (
                "no alice is NOT a member of admin\n",
                "developer membership could not be read\n",
            ),
        ] {
            let sys = Fake::root();
            sys.on(STATUS, Ran::ok("Developer mode is currently disabled.\n"))
                .on(STATUS, Ran::ok("Developer mode is currently disabled.\n"))
                .on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
                .on(admin, Ran::ok(admin_answer))
                .on(developer, Ran::ok(developer_answer))
                .on(enable, Ran::ok(""));
            let ctx = console();
            assert!(
                matches!(DeveloperMode.probe(&sys, &ctx), Probe::Change { .. }),
                "a definite mode-enable step does not wait for membership evidence"
            );
            DeveloperMode.apply(&sys, &ctx).unwrap();
            assert!(sys.calls().iter().any(|call| call == enable));
            assert!(
                !sys.calls().iter().any(|call| call.contains("-o edit")),
                "{:?}",
                sys.calls()
            );
            assert!(
                matches!(DeveloperMode.probe(&sys, &ctx), Probe::Unusable { .. }),
                "an enabled mode does not prove the account has authorization"
            );
        }
    }

    /// The broker is owed, as the account reads it, while its clone is missing or another
    /// build's, or its plist is missing or another source's; nothing is owed once both are this
    /// build's. NEGATIVE CONTROLS: a clone and a plist the account cannot read owe nothing, and
    /// neither does a development build outside a bundle, nor any of it off macOS.
    #[test]
    fn the_broker_is_owed_by_its_image_and_plist_and_an_unreadable_one_owes_nothing() {
        use aterm_uds::lan::{IMAGE_EXECUTABLE, PLIST_PATH, install};
        const SOURCE: &str = "/Applications/aterm.app";
        let ours = install::test_image(0x0100_000c, &[b"this build"]);
        let plist = install::plist_text(Path::new(SOURCE));
        let reading = |image: Option<&[u8]>, daemon: Option<&str>, denied: bool, linux: bool| {
            let mut sys = Fake::root();
            sys.running_app = Some(SOURCE.into());
            sys.linux = linux;
            sys.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
                .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440))
                .file("/Applications/aterm.app/Contents/MacOS/aterm", &ours);
            if let Some(image) = image {
                sys.file(IMAGE_EXECUTABLE, image);
            }
            if let Some(daemon) = daemon {
                sys.file(PLIST_PATH, daemon.as_bytes());
            }
            if denied {
                sys.stat_fails(IMAGE_EXECUTABLE, std::io::ErrorKind::PermissionDenied)
                    .stat_fails(PLIST_PATH, std::io::ErrorKind::PermissionDenied);
            }
            owed(&sys)
        };
        let theirs = install::test_image(0x0100_000c, &[b"another build"]);
        let elsewhere = install::plist_text(Path::new("/Users//a/Applications/aterm.app"));
        let broker = vec![Owed::LanBroker];
        assert_eq!(
            reading(None, Some(&plist), false, false),
            broker,
            "no image"
        );
        assert_eq!(
            reading(Some(&theirs), Some(&plist), false, false),
            broker,
            "cdhash differs"
        );
        assert_eq!(reading(Some(&ours), None, false, false), broker, "no plist");
        assert_eq!(
            reading(Some(&ours), Some(&elsewhere), false, false),
            broker,
            "plist differs"
        );
        assert_eq!(
            reading(Some(&ours), Some(&plist), false, false),
            Vec::<Owed>::new()
        );
        assert_eq!(
            reading(Some(&ours), Some(&plist), true, false),
            Vec::<Owed>::new(),
            "unread"
        );
        assert_eq!(
            reading(None, None, false, true),
            Vec::<Owed>::new(),
            "linux"
        );

        let dev = Fake::root();
        dev.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
            .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440));
        assert_eq!(owed(&dev), Vec::<Owed>::new(), "a development build");
    }

    /// The rule names Apple's `scutil` alone, anchored on both ends, one line per name, with no
    /// character sudoers reads as its own syntax. NEGATIVE CONTROL: an account name that could
    /// carry sudoers syntax or an option, or root's, gets no rule.
    #[test]
    fn the_rule_lets_one_account_set_the_three_names_to_a_label_and_nothing_else() {
        let text = name_rule("alice").unwrap();
        let rules: Vec<&str> = text.lines().filter(|line| !line.starts_with('#')).collect();
        assert_eq!(rules.len(), 3, "{text}");
        for (rule, pref) in rules.iter().zip(NAME_PREFS) {
            assert_eq!(
                *rule,
                format!(
                    "alice ALL = (root) NOPASSWD: /usr/sbin/scutil ^--set {pref} \
                     m[1-9][0-9]?[0-9]?[0-9]?[0-9]?[0-9]?$"
                )
            );
            let args = rule.split_once("scutil ").unwrap().1;
            assert!(!args.contains([',', ':', '=', '\\', '*']), "{rule}");
        }
        for bad in [
            "",
            "root",
            "-alice",
            "al ice",
            "alice,ALL",
            "a:b",
            "x=y",
            "a\\b",
        ] {
            assert_eq!(name_rule(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn overlapping_rule_checks_publish_their_own_checked_files() {
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        };
        let entered = Arc::new(AtomicBool::new(false));
        let checks = Arc::new(Mutex::new(Vec::new()));
        let observed = checks.clone();
        let sys = Fake::root();
        sys.simulate(move |sys, command| {
            if command.program != Path::new(VISUDO) {
                return None;
            }
            let staged = std::path::PathBuf::from(command.args.last().unwrap());
            let bytes = sys.read_file(&staged).unwrap();
            observed.lock().unwrap().push((staged, bytes));
            if !entered.swap(true, Ordering::SeqCst) {
                let mut other = console();
                other.console.as_mut().unwrap().name = "bob".into();
                NameRule.apply(sys, &other).unwrap();
            }
            Some(Ran::ok(""))
        });
        NameRule.apply(&sys, &console()).unwrap();
        let checked = checks.lock().unwrap();
        assert_eq!(checked.len(), 2);
        assert_eq!(checked[0].1, name_rule("alice").unwrap().as_bytes());
        assert_eq!(checked[1].1, name_rule("bob").unwrap().as_bytes());
        assert_eq!(
            sys.read_file(Path::new(ADMITTED_NAME_RULE)).unwrap(),
            checked[0].1
        );
        assert_ne!(checked[0].0, checked[1].0);
        for (staged, _) in checked.iter() {
            assert!(sys.stat(staged).is_err());
            assert!(sys.stat(staged.parent().unwrap()).is_err());
        }
    }

    /// As root, the rule is staged where sudo reads nothing, checked by `visudo -c`, and only
    /// then renamed into place. NEGATIVE CONTROL: a rule `visudo` refuses never goes live: it is
    /// kept where sudo reads nothing, and the rule is no longer owed, so no later launch asks for
    /// the password again for it.
    #[test]
    fn the_rule_goes_live_only_once_visudo_passes_it() {
        let watch = |sys: &Fake, reply: Ran| {
            let path = std::sync::Arc::new(std::sync::Mutex::new(None));
            let observed = path.clone();
            sys.simulate(move |sys, command| {
                if command.program != Path::new(VISUDO) {
                    return None;
                }
                let staged = std::path::PathBuf::from(command.args.last().unwrap());
                let parent = sys.stat(staged.parent().unwrap()).unwrap();
                assert!(parent.dir && !parent.symlink);
                assert_eq!(parent.uid, 0);
                assert_eq!(parent.mode & 0o777, 0o700);
                *observed.lock().unwrap() = Some(staged);
                Some(reply.clone())
            });
            path
        };
        let sys = Fake::root();
        let checked_path = watch(&sys, Ran::ok(""));
        let ctx = console();
        assert!(matches!(NameRule.probe(&sys, &ctx), Probe::Change { .. }));
        NameRule.apply(&sys, &ctx).unwrap();
        let calls = sys.calls();
        let staged = calls.iter().position(|c| c.starts_with("write ")).unwrap();
        let checked = calls.iter().position(|c| c.starts_with(VISUDO)).unwrap();
        let live = calls.iter().position(|c| c.starts_with("rename ")).unwrap();
        assert!(staged < checked && checked < live, "{calls:?}");
        assert_eq!(
            sys.written(ADMITTED_NAME_RULE).as_deref(),
            name_rule("alice").as_deref().map(str::as_bytes)
        );
        assert_eq!(sys.mode_of(ADMITTED_NAME_RULE), Some(0o440));
        let path = checked_path.lock().unwrap().clone().unwrap();
        assert!(sys.stat(&path).is_err());
        assert!(sys.stat(path.parent().unwrap()).is_err());
        assert!(matches!(NameRule.probe(&sys, &ctx), Probe::Ok { .. }));

        let refused = Fake::root();
        let refused_path = watch(&refused, Ran::exit(1, "syntax error"));
        refused.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"));
        assert_eq!(owed(&refused), vec![Owed::NameRule]);
        assert!(NameRule.apply(&refused, &ctx).is_err());
        assert_eq!(refused.written(ADMITTED_NAME_RULE), None);
        let path = refused_path.lock().unwrap().clone().unwrap();
        assert!(refused.stat(&path).is_err());
        assert!(refused.stat(path.parent().unwrap()).is_err());
        assert!(refused.written(REFUSED_RULE).is_some());
        assert_eq!(owed(&refused), Vec::<Owed>::new());

        // Neither a failed publication nor a failed refusal-marker move may replace the
        // live rule or discard an older refusal record. Only this attempt's stage is removed.
        for (destination, reply) in [
            (ADMITTED_NAME_RULE, Ran::ok("")),
            (REFUSED_RULE, Ran::exit(1, "syntax error")),
        ] {
            let failed = Fake::root();
            let previous = name_rule("bob").unwrap();
            failed
                .file(ADMITTED_NAME_RULE, previous.as_bytes())
                .file(REFUSED_RULE, b"previous refusal")
                .rename_fails(destination, std::io::ErrorKind::PermissionDenied);
            let checked_path = watch(&failed, reply);
            assert!(NameRule.apply(&failed, &ctx).is_err());
            assert_eq!(
                failed.read_file(Path::new(ADMITTED_NAME_RULE)).unwrap(),
                previous.as_bytes()
            );
            assert_eq!(
                failed.read_file(Path::new(REFUSED_RULE)).unwrap(),
                b"previous refusal"
            );
            let path = checked_path.lock().unwrap().clone().unwrap();
            assert!(failed.stat(&path).is_err());
            assert!(failed.stat(path.parent().unwrap()).is_err());
        }
    }

    /// Developer mode turns on with a non-admin console account added to `_developer`, each only where
    /// it is not so already. NEGATIVE CONTROL: with both in place the probe reads in place and
    /// the apply runs neither change.
    #[test]
    fn developer_mode_turns_on_with_the_account_in_the_developer_group() {
        let admin = "/usr/sbin/dseditgroup -o checkmember -m alice admin";
        let checkmember = "/usr/sbin/dseditgroup -o checkmember -m alice _developer";
        let enable = "/usr/sbin/DevToolsSecurity -enable";
        let add = "/usr/sbin/dseditgroup -o edit -a alice -t user _developer";
        let sys = Fake::root();
        sys.on(STATUS, Ran::ok("Developer mode is currently disabled.\n"))
            .on(admin, Ran::ok("no alice is NOT a member of admin\n"))
            .on(
                checkmember,
                Ran {
                    status: Some(67),
                    stdout: b"no alice is NOT a member of _developer\n".to_vec(),
                    stderr: Vec::new(),
                },
            )
            .on(enable, Ran::ok(""))
            .on(add, Ran::ok(""));
        let ctx = console();
        assert_eq!(
            DeveloperMode.probe(&sys, &ctx),
            Probe::Change {
                from: "off, alice not in _developer".to_owned(),
                to: "on, alice in _developer".to_owned()
            }
        );
        DeveloperMode.apply(&sys, &ctx).unwrap();
        let calls = sys.calls();
        assert!(calls.iter().any(|c| c == enable) && calls.iter().any(|c| c == add));

        let done = Fake::root();
        done.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
            .on(admin, Ran::ok("no alice is NOT a member of admin\n"))
            .on(
                checkmember,
                Ran::ok("yes alice is a member of _developer\n"),
            );
        assert_eq!(
            DeveloperMode.probe(&done, &ctx),
            Probe::Ok {
                now: "on".to_owned()
            }
        );
        DeveloperMode.apply(&done, &ctx).unwrap();
        let calls = done.calls();
        assert!(!calls.iter().any(|c| c == enable || c == add), "{calls:?}");
    }

    /// After an admission that came past the elevation, the person's account sets each name not
    /// yet the label with `sudo -n` (never a prompt). NEGATIVE CONTROLS: with no rule in place
    /// nothing runs, and free text is no label.
    #[test]
    fn an_admission_after_the_elevation_names_the_mac_through_the_rule_alone() {
        let sys = Fake::root();
        sys.stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440))
            .on(
                "/usr/sbin/scutil --get ComputerName",
                Ran::ok("Alice's Mac\n"),
            )
            .on("/usr/sbin/scutil --get LocalHostName", Ran::ok("m42\n"))
            .on(
                "/usr/sbin/scutil --get HostName",
                Ran::exit(1, "HostName: not set\n"),
            )
            .on(
                "/usr/bin/sudo -n /usr/sbin/scutil --set ComputerName m42",
                Ran::ok(""),
            )
            .on(
                "/usr/bin/sudo -n /usr/sbin/scutil --set HostName m42",
                Ran::ok(""),
            );
        assert_eq!(
            name_by_rule(&sys, "m42", &|| Ok(())),
            Ok(vec!["ComputerName", "HostName"])
        );

        let none = Fake::root();
        assert!(name_by_rule(&none, "m42", &|| Ok(())).is_err());
        assert!(
            none.calls().iter().all(|c| !c.contains("sudo")),
            "{:?}",
            none.calls()
        );
        assert!(name_by_rule(&sys, "Mac-mini", &|| Ok(())).is_err());

        for (number, expected) in [
            ("m183", vec!["ComputerName"]),
            ("m184", vec!["ComputerName", "LocalHostName"]),
        ] {
            let sys = Fake::root();
            sys.stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440));
            for (field, value) in [
                ("ComputerName", "drift"),
                ("LocalHostName", "m183-2"),
                ("HostName", number),
            ] {
                sys.on(&format!("/usr/sbin/scutil --get {field}"), Ran::ok(value));
                sys.on(
                    &format!("/usr/bin/sudo -n /usr/sbin/scutil --set {field} {number}"),
                    Ran::ok(""),
                );
            }
            assert_eq!(name_by_rule(&sys, number, &|| Ok(())).unwrap(), expected);
            let writes: Vec<_> = sys
                .calls()
                .into_iter()
                .filter(|call| call.contains(" --set "))
                .collect();
            assert_eq!(
                writes,
                expected
                    .iter()
                    .map(|field| format!(
                        "/usr/bin/sudo -n /usr/sbin/scutil --set {field} {number}"
                    ))
                    .collect::<Vec<_>>(),
                "a repair of another slot must preserve an accepted Bonjour alias"
            );
        }
    }
}
