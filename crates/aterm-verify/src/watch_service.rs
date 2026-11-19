// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The follower survives its deploying worktree and the terminal that installed it.

use super::Options;
use crate::launch_agent::{Removed, xml};
use std::fs;
use std::path::Path;
use std::time::Duration;

const LABEL: &str = "systems.alab.aterm-verify-watch";
/// Seconds launchd gives the follower to end after its SIGTERM (`ExitTimeOut`): it stops its
/// pass and files the attempt cancelled before it goes.
const EXIT_TIMEOUT: u64 = 60;
/// How long an uninstall, or the install that replaces a loaded follower, waits for it to end:
/// its [`EXIT_TIMEOUT`] and a margin, since a wait no longer than that gives up on a follower
/// that is still ending as designed.
const UNLOAD_WAIT: Duration = Duration::from_secs(EXIT_TIMEOUT + 15);
/// How a person removes the installed follower, as `--install` prints it.
const UNINSTALL: &str = "targo --unverified verify watch --uninstall";

fn manifest(
    exe: &Path,
    root: &Path,
    repair: bool,
    repair_cwd: &Path,
    home: &Path,
    path: &str,
    interval: u64,
) -> String {
    // The switch is always written: what the loaded follower does with a red is read from its
    // own arguments, and a list without it is no follower's (`--repair-cwd` alone is the
    // follower's before the switch).
    let repair = repair_word(repair);
    // The rows must run at default QoS, as a cut does: a Standard or Background launchd job,
    // or a utility clamp, starves the release tree's paint measurement.
    let exe = xml(&exe.to_string_lossy());
    let root = xml(&root.to_string_lossy());
    let repair_cwd = xml(&repair_cwd.to_string_lossy());
    let home = xml(&home.to_string_lossy());
    let path = xml(path);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>{exe}</string><string>watch</string><string>--workspace</string><string>{root}</string><string>--interval</string><string>{interval}</string><string>--repair</string><string>{repair}</string><string>--repair-cwd</string><string>{repair_cwd}</string></array>
<key>WorkingDirectory</key><string>{root}</string>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>{home}</string><key>PATH</key><string>{path}</string></dict>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>ThrottleInterval</key><integer>60</integer>
<key>ExitTimeOut</key><integer>{EXIT_TIMEOUT}</integer>
<key>ProcessType</key><string>Interactive</string>
<key>StandardOutPath</key><string>{root}/.git/watch-service.log</string>
<key>StandardErrorPath</key><string>{root}/.git/watch-service.err</string>
</dict></plist>
"#
    )
}

/// The repair switch as the arguments spell it.
const fn repair_word(repair: bool) -> &'static str {
    if repair { "worker" } else { "off" }
}

pub(super) fn install(opts: &Options, cwd: &Path) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("persistent watch installation requires macOS launchd".into());
    }
    if opts.once || opts.branch != "main" {
        return Err("the persistent follower requires main and cannot use --once".into());
    }
    let root = super::prepare_workspace(opts, cwd)?;
    let repair_cwd = fs::canonicalize(opts.repair_cwd(cwd)?).map_err(|e| e.to_string())?;
    let home = std::env::var_os("HOME").ok_or("no HOME")?;
    let home = Path::new(&home);
    // Immutable deployments: never overwrite a possibly executing Mach-O. The service
    // cannot depend on the caller's worktree, which is removed after landing.
    let token = aterm_uds::rand::hex_token::<16>().map_err(|e| e.to_string())?;
    let deployment = home
        .join(".local/state/aterm-verify/.ubd/watch/controllers")
        .join(token);
    fs::create_dir_all(&deployment).map_err(|e| e.to_string())?;
    let exe = deployment.join("aterm-verify");
    fs::copy(std::env::current_exe().map_err(|e| e.to_string())?, &exe)
        .map_err(|e| e.to_string())?;
    let text = manifest(
        &exe,
        &root,
        opts.repair,
        &repair_cwd,
        home,
        &std::env::var("PATH").unwrap_or_default(),
        opts.interval.as_secs(),
    );
    let target = crate::launch_agent::replace(home, LABEL, &text, UNLOAD_WAIT)?;
    println!("{}", installed(&target, &root, opts.repair, &repair_cwd));
    Ok(())
}

/// What `--install` says: where the follower runs and files its verdicts, and how to remove it.
fn installed(target: &str, root: &Path, repair: bool, repair_cwd: &Path) -> String {
    let repairs = if repair {
        "repair=worker (a red starts one repair worker, an agent session in repair_cwd)"
    } else {
        "repair=off (a red is filed in the journal; no agent session or tab is started; --install --repair worker turns it on)"
    };
    format!(
        "verify watch installed: {target}\ncheckout={}\n{repairs}\nrepair_cwd={}\nlog={}/.git/watch-service.log\nverdicts={}, verdicts/<tree>.toml\nuninstall={UNINSTALL}",
        root.display(),
        repair_cwd.display(),
        root.display(),
        crate::judged::REF
    )
}

/// `verify watch --uninstall`: remove this user's follower through
/// [`crate::launch_agent::remove`], and say what it found and left.
pub(super) fn uninstall() -> Result<String, String> {
    if !cfg!(target_os = "macos") {
        return Err("the persistent follower is a macOS LaunchAgent".into());
    }
    let home = std::env::var_os("HOME").ok_or("no HOME")?;
    uninstall_in(Path::new(&home), crate::launch_agent::remove)
}

/// [`uninstall`] under `home`, the agent removed by `remove` ([`crate::launch_agent::remove`]).
/// Only the follower's own label is named, and nothing else under `home` is touched.
fn uninstall_in(
    home: &Path,
    remove: impl FnOnce(&Path, &str, Duration) -> Result<Removed, String>,
) -> Result<String, String> {
    let plist = crate::launch_agent::plist(home, LABEL);
    Ok(match remove(home, LABEL, UNLOAD_WAIT)? {
        Removed::Absent => format!(
            "verify watch: no follower is installed here ({} is absent and no {LABEL} is loaded)",
            plist.display()
        ),
        Removed::Unloaded => format!(
            "verify watch uninstalled: {} removed and {LABEL} unloaded; its checkout, its logs and the verdicts it filed stay",
            plist.display()
        ),
        Removed::Exiting(why) => format!(
            "verify watch uninstalled: {} removed, so nothing starts the follower again; the loaded one had not ended when the wait gave up ({why}), and launchd ends it at its ExitTimeOut ({EXIT_TIMEOUT} s)",
            plist.display()
        ),
    })
}

/// Whether `list`, the text of this label's property list, is that of a controller installed
/// before the follower: it runs `watch`, and its arguments carry neither `--repair-cwd` nor
/// `--repair`, one of which every follower's list has carried since there was a follower.
fn pre_follower(list: &str) -> bool {
    list.contains(&format!("<string>{LABEL}</string>"))
        && list.contains("<string>watch</string>")
        && !list.contains("<string>--repair-cwd</string>")
        && !list.contains("<string>--repair</string>")
}

/// The tip's verifier was handed `--sample-ready`, which only a controller installed before
/// the follower passes: where this user's LaunchAgent [`LABEL`] is such a controller's
/// ([`pre_follower`]), retire it through [`crate::launch_agent::remove`], and say what was
/// done. That controller tries each tip again every minute and files two or three journal
/// records a try, since the option it passes no longer exists. What it is about to remove goes
/// to stderr first, which that controller points at its checkout's `.git/watch-sample.log`.
pub(super) fn retire_pre_follower() -> Result<String, String> {
    if !cfg!(target_os = "macos") {
        return Ok("verify watch: --sample-ready is no option of this verifier".into());
    }
    let home = std::env::var_os("HOME").ok_or("no HOME")?;
    retire_pre_follower_in(
        Path::new(&home),
        &mut |line| eprintln!("{line}"),
        crate::launch_agent::remove,
    )
}

/// [`retire_pre_follower`] under `home`, the agent removed by `remove`. It reads one property
/// list and removes at most that one, with its loaded job: a follower's list, another label's
/// and a controller somebody runs by hand are left as they are. `say` is told what is about to
/// be removed before `remove` is asked, and the outcome is returned after it.
fn retire_pre_follower_in(
    home: &Path,
    say: &mut dyn FnMut(&str),
    remove: impl FnOnce(&Path, &str, Duration) -> Result<Removed, String>,
) -> Result<String, String> {
    let plist = crate::launch_agent::plist(home, LABEL);
    let list = match fs::read_to_string(&plist) {
        Ok(list) => list,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(format!(
                "verify watch: --sample-ready is the option of a controller from before the follower; no {LABEL} is installed here, so nothing was retired"
            ));
        }
        Err(error) => return Err(format!("{}: {error}", plist.display())),
    };
    if !pre_follower(&list) {
        return Ok(format!(
            "verify watch: --sample-ready is the option of a controller from before the follower; {} is a follower's and stays",
            plist.display()
        ));
    }
    // Said before the removal, never only after it. Unloading the job makes launchd end that
    // controller, whose stop ends its pass, and its pass is this verifier: it does not live to
    // say anything once `remove` has unloaded the job, and an agent that vanished with no line
    // anywhere on its Mac is not something the next reader can explain.
    say(&format!(
        "verify watch: retiring the controller from before the follower ({LABEL}): removing {} and unloading its job, which ends this verifier with it. It handed this verifier --sample-ready, which no longer exists, at every turn. Nothing judges main on this Mac afterwards: install the follower with `targo --unverified verify watch --install`",
        plist.display()
    ));
    let removed = remove(home, LABEL, UNLOAD_WAIT)?;
    Ok(format!(
        "verify watch: retired the controller from before the follower ({LABEL}): {} removed, {}",
        plist.display(),
        match removed {
            Removed::Exiting(why) => format!("its job still ending ({why})"),
            Removed::Absent | Removed::Unloaded => "its job unloaded".to_owned(),
        }
    ))
}

#[cfg(test)]
#[path = "watch_service_tests.rs"]
mod tests;
