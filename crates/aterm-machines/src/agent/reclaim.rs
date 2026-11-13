// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one-time launchd job that removes the folders [`super::install::uninstall`] keeps
//! ([`crate::first_exec::hold_left`]) once macOS can no longer be checking a program started
//! from them, and then removes itself. It outlives the agent, and its plist in
//! `/Library/LaunchDaemons` starts it again at each boot until it has run to its end.

use super::Layout;
use crate::controller::enroll::boot_id_check;
use crate::model::PValue;
use crate::port::{Cmd, System, shell_word};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The job's label, less the `.<run>` of the uninstall that set it up.
const LABEL: &str = "systems.alab.aterm-machines.reclaim";

/// Where launchd finds system jobs, at each boot too.
const DAEMONS: &str = "/Library/LaunchDaemons";

/// The file uninstall writes in each folder it keeps, holding its run id. The job takes only
/// a folder that holds it, so a folder made again at that path since (a later enrollment's
/// stage), or one a program is started from again ([`crate::first_exec::stamp`]) before the
/// job claims its mark, stays.
pub const KEPT: &str = "kept";

/// The hard link by which a reclaim job claims [`KEPT`] before reading it. A new start
/// cancels `KEPT`, then refuses any claim; the job removes its claim only after emptying
/// the folder. Its run id lets the same launchd job resume after a restart, while another
/// generation's claim stays untouched.
pub const CLAIM: &str = "reclaiming";

/// The commands the job reads the clocks, waits and takes itself out with.
struct Commands<'a> {
    /// Prints this boot's id (`kern.bootsessionuuid`).
    session: &'a str,
    /// Prints the last wake, which changes at each wake.
    wake: &'a str,
    /// Waits for its argument's seconds.
    sleep: &'a str,
    /// `launchctl`.
    launchctl: &'a str,
    /// Atomically link the uninstall's mark to its claim without replacing a destination.
    link: &'a str,
}

/// This Mac's [`Commands`].
const MAC: Commands<'static> = Commands {
    session: "/usr/sbin/sysctl -n kern.bootsessionuuid",
    wake: "/usr/sbin/sysctl -n kern.waketime",
    sleep: "/bin/sleep",
    launchctl: "/bin/launchctl",
    link: "/bin/ln",
};

/// The job's `/bin/sh` script, run as `sh -c <script> <label> <run> <boot id> <seconds>
/// <folder>...`. Unless the Mac has restarted since uninstall read `<boot id>`, it first waits
/// `<seconds>` with no wake inside them, so they are awake time whether or not `sleep` counts
/// time asleep. Then, from inside each folder that is not a link, it atomically claims its
/// [`KEPT`] and empties it only where the claim is a regular file, not a link, naming `<run>`,
/// removing the claim last so a run a shutdown cuts short is taken up at the next boot. It
/// removes each folder, `stage/`, `state/` and the agent root that is empty then, and last its
/// plist and itself.
fn script(layout: &Layout, plist: &Path, label: &str, c: &Commands<'_>) -> String {
    let Commands {
        session,
        wake,
        sleep,
        launchctl,
        link,
    } = c;
    let q = |p: &Path| shell_word(&p.to_string_lossy());
    let (root, stage, state, plist) = (
        q(&layout.root),
        q(&layout.stage()),
        q(&layout.state()),
        q(plist),
    );
    format!(
        "run=$1 was=$2 secs=$3\n\
         shift 3\n\
         sess=$({session})\n\
         {}known=$ok\n\
         {}if [ -z \"$known\" ] || [ -z \"$ok\" ] || [ \"$sess\" = \"$was\" ]; then\n\
         while :; do\n\
         woke=$({wake})\n\
         {sleep} \"$secs\"\n\
         [ \"$({wake})\" != \"$woke\" ] || break\n\
         done\n\
         fi\n\
         for dir in \"$@\"; do\n\
         [ -L \"$dir\" ] || (cd -P \"$dir\" || exit\n\
         if [ ! -e {CLAIM} ] && [ ! -L {CLAIM} ]; then {link} {KEPT} {CLAIM} 2>/dev/null || :; fi\n\
         [ -f {CLAIM} ] && [ ! -L {CLAIM} ] \
         && [ \"$(/bin/cat {CLAIM} 2>/dev/null)\" = \"$run\" ] \
         && /usr/bin/find . -mindepth 1 ! -path ./{CLAIM} -delete && /bin/rm {CLAIM})\n\
         /bin/rmdir \"$dir\" 2>/dev/null\n\
         done\n\
         /bin/rmdir {stage} {state} {root} 2>/dev/null\n\
         /bin/rm -f {plist}\n\
         {launchctl} bootout system/{label}\n",
        boot_id_check("$was"),
        boot_id_check("$sess"),
    )
}

/// The job's launchd plist: [`script`] with its arguments, started when it is loaded.
fn document(
    label: &str,
    script: String,
    run: &str,
    session: &str,
    secs: u64,
    kept: &[PathBuf],
) -> String {
    let mut argv = vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script,
        label.to_owned(),
        run.to_owned(),
        session.to_owned(),
        secs.to_string(),
    ];
    argv.extend(kept.iter().map(|dir| dir.to_string_lossy().into_owned()));
    crate::plist::document(&PValue::Dict(vec![
        ("Label".to_owned(), PValue::Str(label.to_owned())),
        (
            "ProgramArguments".to_owned(),
            PValue::Array(argv.into_iter().map(PValue::Str).collect()),
        ),
        ("RunAtLoad".to_owned(), PValue::Bool(true)),
    ]))
}

/// Leave `kept` to a one-time launchd job, which removes each of them once `left` of awake
/// time has passed from now, or after a restart, and then removes itself. `run` names the
/// job and marks each folder ([`KEPT`]).
///
/// # Errors
/// One line saying what could not be set up. Once the plist is written, launchd starts the
/// job at the next boot even when it could not be started now.
pub fn schedule(
    sys: &dyn System,
    layout: &Layout,
    run: &str,
    kept: &[PathBuf],
    left: Duration,
) -> Result<(), String> {
    for dir in kept {
        let marker = dir.join(KEPT);
        sys.write_file(&marker, run.as_bytes(), 0o644)
            .map_err(|e| format!("{}: {e}", marker.display()))?;
    }
    let label = format!("{LABEL}.{run}");
    let plist = Path::new(DAEMONS).join(format!("{label}.plist"));
    let session = crate::first_exec::boot_clocks(sys)
        .map(|c| c.session)
        .unwrap_or_default();
    let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    let doc = document(
        &label,
        script(layout, &plist, &label, &MAC),
        run,
        &session,
        secs,
        kept,
    );
    sys.write_file(&plist, doc.as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", plist.display()))?;
    let bootstrap = Cmd::new("/bin/launchctl")
        .args(["bootstrap", "system"])
        .arg(plist.as_os_str());
    match sys.run(&bootstrap) {
        Ok(ran) if ran.success() => Ok(()),
        Ok(ran) => Err(format!(
            "{}: {}; it starts at the next restart",
            bootstrap.display(),
            ran.describe()
        )),
        Err(e) => Err(format!(
            "{}: {e}; it starts at the next restart",
            bootstrap.display()
        )),
    }
}

#[cfg(test)]
#[path = "reclaim_tests.rs"]
mod tests;
