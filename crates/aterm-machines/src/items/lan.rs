// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The root LAN broker's LaunchDaemon, installed, verified and repaired by setup's one elevation
//! (`docs/DESIGN-lan-broker-2026-11-05.md` §3, §5).
//!
//! As root, [`LanBroker`] clones the app bundle the elevation runs from (the **source**: the
//! `.app` around the root half's canonical executable, which runs as
//! `<app>/Contents/MacOS/aterm machines here apply`) into a root-only staging folder, seals the
//! clone `root:wheel` with no group or other write bit, swaps it over
//! [`aterm_uds::lan::IMAGE_BUNDLE`] (removing the image it replaced), writes
//! [`aterm_uds::lan::PLIST_PATH`] (`0644 root:wheel`, [`aterm_uds::lan::install::plist_text`]),
//! and (re)starts the job with `launchctl bootout`/`enable`/`bootstrap`/`kickstart`, as
//! [`super::aterm`]'s vault server does. launchd runs only the clone, never the person-owned
//! bundle.
//!
//! The clone is APFS `clonefile(2)`: `/Applications` and `/Library` are one volume, so it takes
//! no space. An app kept on another volume cannot be cloned there (`EXDEV`), and Apple's
//! `ditto` copies it instead ([`crate::port::Cloned::Copied`], which the row's detail says).
//!
//! The apply runs no aterm process: the clone and its seal are system calls in the root half,
//! and its only children are `launchctl` (and `ditto` for a copy), so the elevation's count of
//! root aterm runs (`aterm-lab`'s `mac_nux::root_aterm_runs`) is unchanged by it. The broker
//! launchd starts is a root aterm too, which process accounting records each time it exits (an
//! adoption's restart, a re-elevation's bootout, a crash): the lane takes those out by launchd's
//! own count of the job's ended runs (`mac_nux::broker_runs_ended`, its `runs` less the one
//! running, read as accounting starts and as it stops), so its count stays one per root half.

use super::{Ctx, Item};
use crate::model::{Effect, Probe};
use crate::port::{Cloned, System};
use aterm_uds::lan::{self, install};
use serde::Deserialize;
use std::path::Path;
use std::sync::Mutex;

/// The `[lan]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `true` installs the root LAN broker from the app bundle this aterm runs from
    /// ([`LanBroker`]).
    pub broker: Option<bool>,
}

/// The items `table` asks for.
///
/// # Errors
/// Never today; the signature is the family contract.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    if table.broker == Some(true) {
        items.push(Box::new(LanBroker::default()));
    }
    Ok(items)
}

/// Where the broker's log goes, as a folder.
fn log_dir() -> &'static Path {
    Path::new(lan::LOG_PATH)
        .parent()
        .unwrap_or_else(|| Path::new("/Library/Logs"))
}

/// Why a development build installs no broker.
const NO_BUNDLE: &str = "this aterm runs outside an app bundle (a development build), and the \
                         broker runs only a root-owned clone of an app bundle: an installed \
                         aterm.app's `sudo aterm machines here apply --plain` installs it";

/// [`install::Files`] read through a [`System`], so a test's fake files are what is read: a
/// file is stat'ed first, so a stat that fails reads as that failure.
pub(crate) struct Through<'a>(pub &'a dyn System);

impl install::Files for Through<'_> {
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        self.0.stat(path)?;
        self.0.read_file(path)
    }

    fn code_directories(&self, path: &Path) -> std::io::Result<install::CodeDirectories> {
        self.0.code_directories(path)
    }
}

/// The broker's install as `sys` reads it for the bundle it runs from: `None` off macOS or
/// outside a bundle, else [`install::read`]'s answer.
#[must_use]
pub fn reading(sys: &dyn System) -> Option<Result<install::Reading, String>> {
    if !sys.macos() {
        return None;
    }
    let source = sys.running_app()?;
    Some(install::read(&Through(sys), &source))
}

/// `[lan] broker = true`: the root LAN broker installed from the app bundle the elevation runs
/// from (module docs). Applied as root, by the elevation's root half.
#[derive(Default)]
pub struct LanBroker {
    /// How the last apply made the image, for the row's detail.
    made: Mutex<Option<Cloned>>,
}

fn io(what: &Path, e: &std::io::Error) -> String {
    format!("{}: {e}", what.display())
}

impl Item for LanBroker {
    fn id(&self) -> String {
        "lan.broker".to_owned()
    }

    fn family(&self) -> &'static str {
        "lan"
    }

    fn title(&self) -> String {
        "the LAN broker, so aterm reaches this network's Macs without a macOS question".to_owned()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        if !sys.macos() {
            return Probe::NotApplicable {
                why: "only macOS asks about local network access, so only a Mac runs the broker"
                    .to_owned(),
            };
        }
        let Some(source) = sys.running_app() else {
            return Probe::NotApplicable {
                why: NO_BUNDLE.to_owned(),
            };
        };
        let reading = match install::read(&Through(sys), &source) {
            Ok(reading) => reading,
            Err(why) => return Probe::Unusable { why },
        };
        let loaded = match super::aterm::system_job_pid(sys, lan::LABEL) {
            Ok(loaded) => loaded,
            Err(why) => return Probe::Unusable { why },
        };
        let to = format!(
            "{} running in system from a root-owned clone of {}",
            lan::LABEL,
            source.display()
        );
        match (reading.current(), loaded) {
            (true, Some(Some(pid))) => Probe::Ok {
                now: format!("{} running in system (pid {pid})", lan::LABEL),
            },
            (true, Some(None)) => Probe::Change {
                from: "installed and loaded in system, not running".to_owned(),
                to,
            },
            (true, None) => Probe::Change {
                from: "installed, not loaded in system".to_owned(),
                to,
            },
            (false, _) => Probe::Change {
                from: reading.words(),
                to,
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let source = sys.running_app().ok_or(NO_BUNDLE)?;
        let image_dir = Path::new(lan::IMAGE_DIR);
        // Every folder on the image's way is root's with no shared write, so no other account
        // can move the image or put another in its place.
        for dir in image_dir
            .ancestors()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            sys.create_dir_all(dir, 0o755).map_err(|e| io(dir, &e))?;
            sys.root_own(dir, false).map_err(|e| io(dir, &e))?;
        }
        let staging = Path::new(install::STAGING_DIR);
        crate::agent::remove_tree(sys, staging).map_err(|e| io(staging, &e))?;
        sys.create_dir_all(staging, 0o700)
            .map_err(|e| io(staging, &e))?;
        sys.root_own(staging, false).map_err(|e| io(staging, &e))?;
        let staged = staging.join("aterm.app");
        let made = sys
            .clone_tree(&source, &staged)
            .map_err(|e| format!("clone {} to {}: {e}", source.display(), staged.display()))?;
        *self
            .made
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(made);
        sys.root_own(&staged, true).map_err(|e| io(&staged, &e))?;
        let image = Path::new(lan::IMAGE_BUNDLE);
        match sys.exchange(&staged, image) {
            // `staged` now names the image it replaced.
            Ok(()) => crate::agent::remove_tree(sys, &staged).map_err(|e| io(&staged, &e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                sys.rename(&staged, image).map_err(|e| io(image, &e))?;
            }
            Err(e) => return Err(io(image, &e)),
        }
        let _ = sys.remove(staging);
        let log = log_dir();
        sys.create_dir_all(log, 0o755).map_err(|e| io(log, &e))?;
        let plist = Path::new(lan::PLIST_PATH);
        if let Some(daemons) = plist.parent() {
            sys.create_dir_all(daemons, 0o755)
                .map_err(|e| io(daemons, &e))?;
        }
        sys.write_file(plist, install::plist_text(&source).as_bytes(), 0o644)
            .map_err(|e| io(plist, &e))?;
        sys.chown(plist, 0, 0)
            .map_err(|e| format!("chown {}: {e}", plist.display()))?;
        super::aterm::start_system_job(sys, plist, lan::LABEL)
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "{} bootout system/{}; /bin/rm -f {}; /bin/rm -rf {}",
            super::sharing::LAUNCHCTL,
            lan::LABEL,
            crate::port::shell_word(lan::PLIST_PATH),
            crate::port::shell_word(lan::IMAGE_BUNDLE),
        ))
    }

    fn effect(&self) -> Effect {
        Effect::Now
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        match *self
            .made
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            Some(Cloned::Copied) => Some(
                "the app is on another volume than /Library, so its image is a full copy, not a \
                 clone"
                    .to_owned(),
            ),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::defaults::tests::ctx;
    use crate::port::{Fake, Ran};
    use std::path::PathBuf;

    const SOURCE: &str = "/Applications/aterm.app";
    const SOURCE_EXE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
    const PRINT: &str = "/bin/launchctl print system/systems.alab.aterm-lan";

    fn this_build() -> Vec<u8> {
        install::test_image(0x0100_000c, &[b"this build"])
    }

    fn from_app() -> Fake {
        let mut sys = Fake::root();
        sys.running_app = Some(PathBuf::from(SOURCE));
        sys.file(SOURCE_EXE, &this_build());
        sys
    }

    /// As root the bundle is cloned into the root-only staging folder, sealed, swapped in, and
    /// only then is the plist written and the job (re)started, each launchctl step in launchd's
    /// order; the probe then reads it in place and running. No aterm process is started.
    /// NEGATIVE CONTROL: a development build outside a bundle is not applicable and its apply
    /// touches nothing.
    #[test]
    fn the_broker_is_cloned_sealed_swapped_then_started() {
        let sys = from_app();
        // Not loaded for the probe and the apply, then running for the probe after it.
        sys.on(PRINT, Ran::exit(113, "Could not find service"))
            .on(PRINT, Ran::exit(113, "Could not find service"))
            .on(PRINT, Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"))
            .on("/bin/launchctl enable system/systems.alab.aterm-lan", Ran::ok(""))
            .on(
                "/bin/launchctl bootstrap system /Library/LaunchDaemons/systems.alab.aterm-lan.plist",
                Ran::ok(""),
            )
            .on("/bin/launchctl kickstart system/systems.alab.aterm-lan", Ran::ok(""));
        let ctx = ctx(&[]);
        assert!(matches!(
            LanBroker::default().probe(&sys, &ctx),
            Probe::Change { .. }
        ));
        let item = LanBroker::default();
        item.apply(&sys, &ctx).unwrap();
        let calls = sys.calls();
        let at = |prefix: &str| {
            calls
                .iter()
                .position(|c| c.starts_with(prefix))
                .unwrap_or_else(|| panic!("{prefix}: {calls:?}"))
        };
        let staged = "/Library/Application Support/aterm/lan/.staging/aterm.app";
        let order = [
            at("root-own /Library/Application Support/aterm/lan"),
            at("mkdir /Library/Application Support/aterm/lan/.staging 700"),
            at(&format!("clone {SOURCE} {staged}")),
            at(&format!("root-own -R {staged}")),
            at(&format!("exchange {staged} {}", lan::IMAGE_BUNDLE)),
            at(&format!("rename {staged} {}", lan::IMAGE_BUNDLE)),
            at(&format!("write {} 644", lan::PLIST_PATH)),
            at(&format!("chown 0:0 {}", lan::PLIST_PATH)),
            at("/bin/launchctl enable"),
            at("/bin/launchctl bootstrap"),
            at("/bin/launchctl kickstart"),
        ];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?} {calls:?}");
        assert!(
            calls
                .iter()
                .filter(|c| c.starts_with('/'))
                .all(|c| c.starts_with("/bin/launchctl ")),
            "launchctl alone runs, no aterm: {calls:?}"
        );
        assert_eq!(sys.written(lan::IMAGE_EXECUTABLE), Some(this_build()));
        assert_eq!(sys.mode_of(lan::PLIST_PATH), Some(0o644));
        assert_eq!(item.detail(&sys, &ctx), None);

        assert_eq!(
            LanBroker::default().probe(&sys, &ctx),
            Probe::Ok {
                now: "systems.alab.aterm-lan running in system (pid 77)".to_owned()
            }
        );

        let dev = Fake::root();
        assert!(matches!(
            LanBroker::default().probe(&dev, &ctx),
            Probe::NotApplicable { .. }
        ));
        assert!(LanBroker::default().apply(&dev, &ctx).is_err());
        assert!(dev.calls().is_empty(), "{:?}", dev.calls());
    }

    /// The elevation's own baseline installs the broker before the Command Line Tools, whose
    /// download can take minutes while the vault look waits on the broker.
    #[test]
    fn the_elevation_installs_the_broker_before_the_tools() {
        let baseline =
            crate::baseline::parse(&crate::init_clt::CommandLineTools::baseline()).unwrap();
        let ids: Vec<String> = crate::items::expand(&baseline, &ctx(&[]))
            .unwrap()
            .iter()
            .map(|item| item.id())
            .collect();
        let at = |id: &str| {
            ids.iter()
                .position(|i| i == id)
                .unwrap_or_else(|| panic!("{ids:?}"))
        };
        assert!(
            at("lan.broker") < at(crate::items::github::CLT_ITEM),
            "{ids:?}"
        );
    }

    /// A broker already running from another build's image is booted out before the new job
    /// loads, and the image it ran from is exchanged for the clone, never renamed over.
    #[test]
    fn a_running_broker_is_replaced_by_exchange_and_restarted() {
        let sys = from_app();
        sys.file(
            lan::IMAGE_EXECUTABLE,
            &install::test_image(0x0100_000c, &[b"old"]),
        )
        .on(
            PRINT,
            Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 9\n}\n"),
        )
        .on(
            "/bin/launchctl bootout system/systems.alab.aterm-lan",
            Ran::ok(""),
        )
        .on(
            "/bin/launchctl enable system/systems.alab.aterm-lan",
            Ran::ok(""),
        )
        .on(
            "/bin/launchctl bootstrap system /Library/LaunchDaemons/systems.alab.aterm-lan.plist",
            Ran::ok(""),
        )
        .on(
            "/bin/launchctl kickstart system/systems.alab.aterm-lan",
            Ran::ok(""),
        );
        let ctx = ctx(&[]);
        match LanBroker::default().probe(&sys, &ctx) {
            Probe::Change { from, .. } => {
                assert_eq!(from, "image another build's, plist missing");
            }
            other => panic!("{other:?}"),
        }
        LanBroker::default().apply(&sys, &ctx).unwrap();
        let calls = sys.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("exchange ")),
            "{calls:?}"
        );
        assert!(!calls.iter().any(|c| c.starts_with("rename ")), "{calls:?}");
        let bootout = calls.iter().position(|c| c.contains("bootout")).unwrap();
        let bootstrap = calls.iter().position(|c| c.contains("bootstrap")).unwrap();
        assert!(bootout < bootstrap, "{calls:?}");
        assert_eq!(sys.written(lan::IMAGE_EXECUTABLE), Some(this_build()));
    }
}
