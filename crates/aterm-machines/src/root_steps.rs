// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The root steps of setup's one early elevation besides Apple's Command Line Tools
//! ([`crate::init_clt`]), which aterm asks for by itself, once, as a Mac's first launch begins
//! (owner ruling 2026-11-02: setup starts and finishes by itself; aterm raises only the prompts
//! macOS alone can answer, early and batched, and never waits on them):
//!
//! - developer mode (`DevToolsSecurity -enable`), with the person who typed the password in the
//!   `_developer` group, so a debugger attaches with no SamplingTools dialog ([`DeveloperMode`]);
//! - the one narrow rule ([`ADMITTED_NAME_RULE`], [`name_rule`]) that lets that person's account
//!   set this Mac's ComputerName, LocalHostName and HostName to a swarm label (`m` and a number)
//!   with Apple's own `scutil`, and nothing else ([`NameRule`]), so the name the vault's roster
//!   gives the Mac at an admission that comes after the elevation applies with no second
//!   password ([`name_by_rule`]).
//!
//! Why a sudoers rule, and not a privileged helper or a second password: a second password is a
//! second prompt, which the ruling rules out. A helper (a launchd daemon, a setuid tool) is root
//! code of aterm's that stays resident or callable, whose every input the person's account (and
//! so anything running as it) controls, and whose binary must itself stay root-owned beside an
//! app the person updates: a broad foothold to keep safe. The rule runs no code of aterm's as
//! root: sudo matches the whole argument string against `^--set <Pref> m<digits>$` and runs
//! SIP-protected `/usr/sbin/scutil`, so the most the account can do with it is rename this Mac to
//! another swarm label. `visudo -c` checks the rule before it goes live, so a sudo that cannot
//! read it leaves none, and the name then waits for the next elevation.
//!
//! What the person's account can read of each step without root ([`owed`]) decides whether the
//! elevation is owed on a Mac that already has the tools: an unreadable step is not owed, so no
//! password is asked for want of a reading.

use crate::items::{Ctx, Item};
use crate::model::{Probe, Read};
use crate::port::{Cmd, Ran, System};
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

/// Where the rule is written before `visudo -c` passes it: sudo's include directory skips every
/// name with a dot, so a staged rule never authorizes anything.
const STAGED_RULE: &str = "/private/etc/sudoers.d/.aterm-admitted-name";

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
    /// Developer mode is off.
    Debugger,
    /// The rule that names the Mac after admission is not in place.
    NameRule,
}

impl Owed {
    /// What the step does, in plain words.
    #[must_use]
    pub const fn plain(self) -> &'static str {
        match self {
            Self::Debugger => "turns on developer mode, so debuggers attach without a password",
            Self::NameRule => "lets aterm give this Mac the name its swarm admits it by",
        }
    }
}

/// `DevToolsSecurity -status`, which needs no authorization.
fn status_cmd() -> Cmd {
    Cmd::new(DEVTOOLS_SECURITY).arg("-status")
}

/// Whether `-status` said developer mode is on; `None` for any other answer.
#[must_use]
pub fn developer_mode(ran: &Ran) -> Option<bool> {
    if !ran.success() {
        return None;
    }
    let out = ran.out();
    if out.contains("currently enabled") {
        Some(true)
    } else if out.contains("currently disabled") {
        Some(false)
    } else {
        None
    }
}

fn developer_mode_now(sys: &dyn System) -> Option<bool> {
    sys.run(&status_cmd())
        .ok()
        .as_ref()
        .and_then(developer_mode)
}

/// `dseditgroup -o checkmember`'s answer for `user` in [`DEVELOPER_GROUP`]: `yes …` or `no …`.
#[must_use]
pub fn member(ran: &Ran) -> Option<bool> {
    let out = ran.out();
    if out.starts_with("yes ") {
        Some(true)
    } else if out.starts_with("no ") {
        Some(false)
    } else {
        None
    }
}

fn member_now(sys: &dyn System, user: &str) -> Option<bool> {
    let cmd = Cmd::new(DSEDITGROUP).args(["-o", "checkmember", "-m", user, DEVELOPER_GROUP]);
    sys.run(&cmd).ok().as_ref().and_then(member)
}

/// Whether the rule is in place as a stat reads it, which needs no root (the include directory
/// is world-searchable, the rule itself readable by root alone): a root-owned regular file of
/// mode 0440. `None` when the stat answered with anything but not-found.
#[must_use]
pub fn rule_in_place(sys: &dyn System) -> Option<bool> {
    match sys.stat(Path::new(ADMITTED_NAME_RULE)) {
        Ok(stat) => Some(!stat.dir && !stat.symlink && stat.uid == 0 && stat.mode == 0o440),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// What the elevation still owes this Mac beside the tools, as the person's account reads it.
#[must_use]
pub fn owed(sys: &dyn System) -> Vec<Owed> {
    let mut owed = Vec::new();
    if developer_mode_now(sys) == Some(false) {
        owed.push(Owed::Debugger);
    }
    if rule_in_place(sys) == Some(false) && sys.stat(Path::new(REFUSED_RULE)).is_err() {
        owed.push(Owed::NameRule);
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
/// returned.
///
/// # Errors
/// A label that is no swarm label, no rule in place (the next elevation names the Mac), or a
/// name `sudo` or `scutil` refused, in one line.
pub fn name_by_rule(sys: &dyn System, label: &str) -> Result<Vec<&'static str>, String> {
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
        if now == Read::Value(label.clone()) {
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

/// `[developer] debugger = true`: developer mode on, and the console account in
/// [`DEVELOPER_GROUP`]. Applied as root, by the elevation's root half.
pub struct DeveloperMode;

impl DeveloperMode {
    fn read(sys: &dyn System, ctx: &Ctx) -> (Option<bool>, Option<(String, Option<bool>)>) {
        let user = ctx
            .console
            .as_ref()
            .map(|user| (user.name.clone(), member_now(sys, &user.name)));
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
        let missing = user
            .as_ref()
            .filter(|(_, member)| *member == Some(false))
            .map(|(name, _)| name.as_str());
        match (mode, missing) {
            (true, None) => Probe::Ok {
                now: "on".to_owned(),
            },
            (mode, missing) => Probe::Change {
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
            },
        }
    }

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        let (mode, user) = Self::read(sys, ctx);
        if mode != Some(true) {
            run_ok(sys, &Cmd::new(DEVTOOLS_SECURITY).arg("-enable"))?;
        }
        if let Some((name, member)) = user
            && member != Some(true)
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
        let staged = Path::new(STAGED_RULE);
        // macOS's sudoers includes the directory; a Mac that has none gets it, as root's.
        if let Some(dir) = staged.parent() {
            sys.create_dir_all(dir, 0o755)
                .map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        sys.write_file(staged, wanted.as_bytes(), 0o440)
            .map_err(|e| format!("{STAGED_RULE}: {e}"))?;
        let checked = run_ok(sys, &Cmd::new(VISUDO).args(["-c", "-q", "-f", STAGED_RULE]));
        if let Err(why) = checked {
            // Kept where sudo never reads it, so the rule stops being owed ([`REFUSED_RULE`]).
            if sys.rename(staged, Path::new(REFUSED_RULE)).is_err() {
                let _ = sys.remove(staged);
            }
            return Err(why);
        }
        sys.rename(staged, Path::new(ADMITTED_NAME_RULE))
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
        ctx.console = Some(user("alice", 501));
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

    /// The account's reading owes developer mode while `-status` says it is off and the rule
    /// while no root-owned 0440 file stands. NEGATIVE CONTROLS: an answer `-status` gives that is
    /// neither, and a stat that fails other than not-found, owe nothing, so no password is asked
    /// for want of a reading; a rule the account could have written (its own uid, or a wider
    /// mode) is no rule in place.
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

        for (uid, mode) in [(501, 0o440), (0, 0o666)] {
            let planted = Fake::root();
            planted
                .on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
                .stat_of(ADMITTED_NAME_RULE, rule_stat(uid, mode));
            assert_eq!(owed(&planted), vec![Owed::NameRule], "{uid} {mode:o}");
        }
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

    /// As root, the rule is staged where sudo reads nothing, checked by `visudo -c`, and only
    /// then renamed into place. NEGATIVE CONTROL: a rule `visudo` refuses never goes live: it is
    /// kept where sudo reads nothing, and the rule is no longer owed, so no later launch asks for
    /// the password again for it.
    #[test]
    fn the_rule_goes_live_only_once_visudo_passes_it() {
        let check = "/usr/sbin/visudo -c -q -f /private/etc/sudoers.d/.aterm-admitted-name";
        let sys = Fake::root();
        sys.on(check, Ran::ok(""));
        let ctx = console();
        assert!(matches!(NameRule.probe(&sys, &ctx), Probe::Change { .. }));
        NameRule.apply(&sys, &ctx).unwrap();
        let calls = sys.calls();
        let staged = calls.iter().position(|c| c.starts_with("write ")).unwrap();
        let checked = calls.iter().position(|c| c == check).unwrap();
        let live = calls.iter().position(|c| c.starts_with("rename ")).unwrap();
        assert!(staged < checked && checked < live, "{calls:?}");
        assert_eq!(
            sys.written(ADMITTED_NAME_RULE).as_deref(),
            name_rule("alice").as_deref().map(str::as_bytes)
        );
        assert_eq!(sys.mode_of(ADMITTED_NAME_RULE), Some(0o440));
        assert!(matches!(NameRule.probe(&sys, &ctx), Probe::Ok { .. }));

        let refused = Fake::root();
        refused.on(check, Ran::exit(1, "syntax error"));
        refused.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"));
        assert_eq!(owed(&refused), vec![Owed::NameRule]);
        assert!(NameRule.apply(&refused, &ctx).is_err());
        assert_eq!(refused.written(ADMITTED_NAME_RULE), None);
        assert_eq!(refused.written(STAGED_RULE), None);
        assert!(refused.written(REFUSED_RULE).is_some());
        assert_eq!(owed(&refused), Vec::<Owed>::new());
    }

    /// Developer mode turns on with the console account added to `_developer`, each only where
    /// it is not so already. NEGATIVE CONTROL: with both in place the probe reads in place and
    /// the apply runs neither change.
    #[test]
    fn developer_mode_turns_on_with_the_account_in_the_developer_group() {
        let checkmember = "/usr/sbin/dseditgroup -o checkmember -m alice _developer";
        let enable = "/usr/sbin/DevToolsSecurity -enable";
        let add = "/usr/sbin/dseditgroup -o edit -a alice -t user _developer";
        let sys = Fake::root();
        sys.on(STATUS, Ran::ok("Developer mode is currently disabled.\n"))
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
            name_by_rule(&sys, "m42"),
            Ok(vec!["ComputerName", "HostName"])
        );

        let none = Fake::root();
        assert!(name_by_rule(&none, "m42").is_err());
        assert!(
            none.calls().iter().all(|c| !c.contains("sudo")),
            "{:?}",
            none.calls()
        );
        assert!(name_by_rule(&sys, "Mac-mini").is_err());
    }
}
