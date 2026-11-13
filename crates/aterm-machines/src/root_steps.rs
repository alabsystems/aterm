// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The root steps of setup's one early elevation besides Apple's Command Line Tools
//! ([`crate::init_clt`]), which aterm asks for by itself, once, as a Mac's first launch begins
//! (setup starts and finishes by itself; aterm raises only the prompts
//! macOS alone can answer, early and batched, and never waits on them):
//!
//! - developer mode (`DevToolsSecurity -enable`), with the console account authorized as an
//!   administrator or in `_developer`, so a debugger attaches with no SamplingTools dialog
//!   ([`DeveloperMode`]);
//! - the one narrow rule ([`ADMITTED_NAME_RULE`], [`name_rule`]) that lets that person's account
//!   set this Mac's ComputerName, LocalHostName and HostName to generated names (number and nickname)
//!   with Apple's own `scutil`, and nothing else ([`NameRule`]), so the name the vault's roster
//!   gives the Mac at an admission that comes after the elevation applies with no second
//!   password ([`name_by_rule`]);
//! - the root LAN broker ([`crate::items::lan::LanBroker`]), a resident root LaunchDaemon through
//!   which all of aterm's own outbound LAN traffic goes, so macOS never asks its Local Network
//!   question about aterm (`docs/DESIGN-lan-broker-2026-11-05.md`);
//! - disabled Spotlight indexing on the data volume, so generated files outside Rust's
//!   build caches cannot start an indexing storm ([`crate::init_spotlight`]).
//!
//! Why a sudoers rule for the name, and not a privileged helper or a second password: a second
//! password is a second prompt, which the ruling rules out, and a helper would put root code of
//! aterm's behind an input the person's account controls where a rule needs none. The rule runs no
//! code of aterm's as root: sudo matches the whole argument string against
//! anchored, bounded generated-name patterns and runs SIP-protected `/usr/sbin/scutil`,
//! so the account can change only these three names, never run another command. `visudo -c` checks the rule before it
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
use aterm_types::machine_name::{GeneratedNames, OsSlot};
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

fn name_pattern(pref: &str) -> String {
    // Sudo matches in sudoers_locale (C by default), not the caller's locale.
    // ComputerName is deliberately a coarse naming permission: in C, opaque
    // UTF-8 bytes are allowed, but ASCII punctuation/controls and an option-like
    // first byte are not. GeneratedNames and the caller's current-intent fence
    // retain Unicode grammar and exact target selection. No locale is changed.
    // Each number-width alternative leaves the full target at most 63 bytes in
    // C. An administrator's multibyte sudoers locale counts characters instead;
    // this remains finite, while GeneratedNames still caps every target at 63 bytes.
    let alternatives = (1..=6)
        .map(|digits| {
            let number = if digits == 1 {
                "m[0-9]".to_owned()
            } else {
                format!("m[1-9][0-9]{{{}}}", digits - 1)
            };
            if pref == "ComputerName" {
                return format!(
                    "{number}( [^[:cntrl:][:punct:] ][^[:cntrl:][:punct:]]{{0,{}}})?",
                    60 - digits,
                );
            }
            format!(
                "{number}(-[a-z0-9]([a-z0-9-]{{0,{}}}[a-z0-9])?)?",
                59 - digits,
            )
        })
        .collect::<Vec<_>>()
        .join("|");
    format!("({alternatives})")
}

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
    /// Spotlight still indexes the writable data volume.
    Spotlight,
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
            Self::Spotlight => "disables Spotlight indexing of the data volume",
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

/// Whether a rule file stands as a stat reads it, which needs no root (the include directory
/// is world-searchable): a root-owned regular file of mode 0440 with nonzero length. `None`
/// when the stat answered with anything but not-found. Metadata alone: it tells a missing rule
/// ([`owed`]) and gates an explicit attempt ([`name_by_rule`]), but a rule an older build wrote
/// passes it while sudo refuses the generated names, so nothing outside this module reads it;
/// whether the rule can set them is [`name_rule_current`].
fn rule_in_place(sys: &dyn System) -> Option<bool> {
    match sys.stat(Path::new(ADMITTED_NAME_RULE)) {
        Ok(stat) => {
            Some(!stat.dir && !stat.symlink && stat.uid == 0 && stat.mode == 0o440 && stat.len > 0)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// The naming permission as this process's ordinary account reads it ([`name_rule_reading`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameRuleReading {
    /// No rule stands as setup leaves one: no file, or not a root-owned 0440 regular file of
    /// nonzero length. Setup's password step owes it ([`owed`]) unless this Mac's `visudo`
    /// refused it before ([`REFUSED_RULE`]).
    Missing,
    /// A rule stands as setup leaves one, but its bytes are not this build's [`name_rule`] for
    /// this account (an older build's text, or another account's rule). Setup's password step
    /// never owes it ([`owed`] counts a missing rule only), so only setup the person asks for
    /// renews it.
    Other,
    /// This build's [`name_rule`] for this account, byte for byte.
    Current,
}

/// Whether this process's ordinary account (not root) can read the exact safe, compiled naming
/// permission: this build's [`name_rule`] for that account, byte for byte. The rule's text has
/// changed only with what it admits (the nickname, the account quoting, `m0`, non-ASCII
/// nicknames), so a rule of other bytes may refuse this build's generated names, and an
/// unreadable one predates the read access this build grants. Only
/// `Ok(NameRuleReading::Current)` admits an automatic naming attempt or a status that promises
/// one; a missing rule waits for setup's password step, and any other reading for setup the
/// person asks for. This is evidence for presentation and that gate; every effect still goes
/// through sudo's current policy.
/// # Errors
/// The account or existing root-owned rule cannot be read safely.
pub fn name_rule_reading(sys: &dyn System) -> Result<NameRuleReading, String> {
    let identity = sys.identity()?;
    if identity.user.uid != sys.euid() || sys.euid() == 0 {
        return Err("naming permission belongs to the current ordinary account".into());
    }
    let wanted =
        name_rule(&identity.user.name).ok_or("the current account cannot own a naming rule")?;
    match rule_in_place(sys) {
        Some(false) => return Ok(NameRuleReading::Missing),
        None => return Err("the existing naming permission could not be inspected".into()),
        Some(true) => {}
    }
    let bytes = sys
        .read_owned_file(Path::new(ADMITTED_NAME_RULE), 0, 16 * 1024, false)
        .map_err(|why| format!("the existing naming permission could not be read: {why}"))?;
    Ok(if bytes == wanted.as_bytes() {
        NameRuleReading::Current
    } else {
        NameRuleReading::Other
    })
}

/// [`name_rule_reading`] as a yes or no: `Ok(true)` exactly where it reads
/// [`NameRuleReading::Current`].
/// # Errors
/// As [`name_rule_reading`].
pub fn name_rule_current(sys: &dyn System) -> Result<bool, String> {
    name_rule_reading(sys).map(|reading| reading == NameRuleReading::Current)
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
    if crate::items::lan::reading(sys).is_some_and(|reading| {
        reading.is_ok_and(|r| {
            r.owed_with_running(|| {
                crate::items::aterm::system_job_pid(sys, aterm_uds::lan::LABEL)
                    .ok()
                    .map(|job| matches!(job, Some(Some(_))))
            })
        })
    }) {
        owed.push(Owed::LanBroker);
    }
    if sys.macos() && crate::init_spotlight::indexing(sys) == Ok(true) {
        owed.push(Owed::Spotlight);
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
         # (number and bounded nickname) with Apple's scutil, and nothing else.\n",
    );
    for pref in NAME_PREFS {
        // Even an ASCII account can spell ALL or an existing User_Alias. Quote the
        // validated word so sudoers treats it as one literal account, never an alias.
        text.push_str(&format!(
            "\"{user}\" ALL = (root) NOPASSWD: {scutil} ^--set {pref} {}$\n",
            name_pattern(pref),
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

/// Give this Mac its typed generated names through the rule,
/// as the person's own account, with `sudo -n`, which never asks for a password: the step an
/// admission after the elevation takes. Exact names and diagnosed collision aliases are left; changed slots are
/// returned. `current` re-reads the caller's naming intent after each name read, before any
/// write, so a newer choice retires this work without undoing an earlier name already set.
///
/// The guard is the rule file's metadata alone, so an explicit attempt also tries a rule an
/// older build wrote that may still admit these names; `sudo -n` asks nothing either way. An
/// automatic caller first holds [`name_rule_current`] to `Ok(true)`.
///
/// # Errors
/// No rule is in place (the next elevation names the Mac), or a
/// name `sudo` or `scutil` refused, or `current` no longer admits this intent, in one line.
pub fn name_by_rule(
    sys: &dyn System,
    names: &GeneratedNames,
    current: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<&'static str>, String> {
    if rule_in_place(sys) != Some(true) {
        return Err(
            "the rule that names this Mac is not in place, so setup's next Mac password names it"
                .to_owned(),
        );
    }
    let scutil = crate::items::names::SCUTIL;
    let mut set = Vec::new();
    for pref in NAME_PREFS {
        let slot = OsSlot::from_macos_preference(pref).expect("a fixed macOS name slot");
        let label = names.for_slot(slot).expect("generated macOS name");
        let get = Cmd::new(scutil).args(["--get", pref]);
        let now = sys.run(&get).map_or_else(
            |e| Read::Unusable(e.to_string()),
            |ran| crate::items::names::read_scutil(&ran),
        );
        current()?;
        if let Read::Value(now) = &now
            && names.accepts(slot, now)
        {
            continue;
        }
        run_ok(
            sys,
            &Cmd::new(SUDO).args(["-n", scutil, "--set", pref, label]),
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
pub enum NameRule {
    /// Ordinary root setup leaves an already matching rule alone.
    Current,
    /// An explicit permission repair reinstalls even matching bytes, renewing the read ACL.
    Refresh(std::sync::atomic::AtomicBool),
}

impl NameRule {
    /// Reinstall the rule once in this explicit run, even when only its read ACL is missing.
    pub fn refresh() -> Self {
        Self::Refresh(std::sync::atomic::AtomicBool::new(false))
    }

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
                if matches!(self, Self::Current)
                    || matches!(self, Self::Refresh(done)
                        if done.load(std::sync::atomic::Ordering::Acquire))
                {
                    return Probe::Ok {
                        now: "in place".to_owned(),
                    };
                }
                "matching rule; refresh its account read access".to_owned()
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
        let user = ctx
            .console
            .as_ref()
            .ok_or("no console account owns the naming permission")?;
        // wanted() already validated this username. Only that account gains read access,
        // on the staged nonsecret policy; POSIX mode stays 0440 and no write ACL is granted.
        run_ok(
            sys,
            &Cmd::new("/bin/chmod")
                .args(["+a", &format!("user:{} allow read", user.name)])
                .arg(&staged),
        )?;
        let checked = run_ok(sys, &Cmd::new(VISUDO).args(["-c", "-q", "-f"]).arg(&staged));
        if let Err(why) = checked {
            // Kept where sudo never reads it, so the rule stops being owed ([`REFUSED_RULE`]).
            let _ = sys.rename(&staged, Path::new(REFUSED_RULE));
            return Err(why);
        }
        sys.rename(&staged, Path::new(ADMITTED_NAME_RULE))
            .map_err(|e| format!("{ADMITTED_NAME_RULE}: {e}"))?;
        let _ = sys.remove(Path::new(REFUSED_RULE));
        if let Self::Refresh(done) = self {
            done.store(true, std::sync::atomic::Ordering::Release);
        }
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        Some(format!("rm {ADMITTED_NAME_RULE}"))
    }
}

#[cfg(test)]
#[path = "root_steps_tests.rs"]
mod tests;
