// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Apple's Command Line Tools, installed headlessly through `softwareupdate` (design §1.13).

use super::{Ctx, Item};
use crate::model::Probe;
use crate::port::{Cmd, System};
use serde::Deserialize;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

/// Where the tools install.
pub const CLT_DIR: &str = "/Library/Developer/CommandLineTools";

/// The file whose presence means the tools are installed.
pub const CLT_GIT: &str = "/Library/Developer/CommandLineTools/usr/bin/git";

/// The tools' package receipt (its version is the report's).
pub const CLT_RECEIPT: &str = "com.apple.pkg.CLTools_Executables";

/// The placeholder that makes `softwareupdate -l` list the tools.
pub const PLACEHOLDER: &str = "/tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress";

/// `softwareupdate`.
pub const SOFTWAREUPDATE: &str = "/usr/sbin/softwareupdate";

/// `xcode-select`.
pub const XCODE_SELECT: &str = aterm_uds::os_path!("xcode-select");

/// The catalog host whose reachability the probe checks before it plans an install.
pub const CATALOG: &str = "https://swscan.apple.com/";

/// `softwareupdate -l` reads Apple's catalog; slow networks take minutes.
pub const LIST_BUDGET: Duration = Duration::from_secs(20 * 60);

/// `softwareupdate -i` downloads about half a gigabyte and installs it.
pub const INSTALL_BUDGET: Duration = Duration::from_secs(2 * 60 * 60);

/// The `[developer]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `true` installs the newest `Command Line Tools for Xcode` label when none is installed, and
    /// selects them when nothing is selected.
    pub command_line_tools: Option<bool>,
}

/// The items `table` asks for.
/// # Errors
/// Never today; the signature is the family contract.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    Ok(if table.command_line_tools == Some(true) {
        vec![Box::new(CommandLineTools {
            planned: Mutex::new(None),
        })]
    } else {
        Vec::new()
    })
}

/// One `Command Line Tools` entry of `softwareupdate -l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    /// What `softwareupdate -i` takes.
    pub label: String,
    /// Its version, from the `Title:` line's `Version:` or else from the label.
    pub version: Vec<u32>,
    /// A beta (picked only when nothing else is listed).
    pub beta: bool,
}

/// Every `Command Line Tools` label in `softwareupdate -l`'s answer, in its order.
#[must_use]
pub fn parse_labels(text: &str) -> Vec<Label> {
    let mut out: Vec<Label> = Vec::new();
    let mut last_was_clt = false;
    for l in text.lines() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("* ") {
            let label = rest.strip_prefix("Label: ").unwrap_or(rest).trim();
            last_was_clt = label.contains("Command Line Tools");
            if last_was_clt {
                out.push(Label {
                    label: label.to_owned(),
                    version: version_after_xcode(label),
                    beta: label.to_ascii_lowercase().contains("beta"),
                });
            }
            continue;
        }
        if last_was_clt
            && let Some(v) = t.split_once("Version: ").map(|(_, v)| leading_version(v))
            && !v.is_empty()
            && let Some(last) = out.last_mut()
        {
            last.version = v;
        }
        last_was_clt = false;
    }
    out
}

/// The label to install: the newest non-beta one, else the newest beta.
#[must_use]
pub fn pick(labels: &[Label]) -> Option<&Label> {
    let newest = |beta: bool| {
        labels
            .iter()
            .filter(|l| l.beta == beta)
            .max_by(|a, b| a.version.cmp(&b.version))
    };
    newest(false).or_else(|| newest(true))
}

fn version_after_xcode(label: &str) -> Vec<u32> {
    label
        .rsplit_once("Xcode")
        .map(|(_, rest)| leading_version(rest.trim_start_matches([' ', '-', '('])))
        .unwrap_or_default()
}

fn leading_version(text: &str) -> Vec<u32> {
    let digits: String = text
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.split('.').map_while(|p| p.parse().ok()).collect()
}

/// What the probe found, for apply and revert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Planned {
    /// Not installed: install, then select if nothing is selected.
    Install,
    /// Installed but no developer directory selected: select.
    Select,
}

struct CommandLineTools {
    planned: Mutex<Option<Planned>>,
}

fn selected(sys: &dyn System) -> Result<Option<String>, String> {
    let cmd = Cmd::new(XCODE_SELECT).arg("-p");
    let ran = sys
        .run(&cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    if ran.success() {
        let dir = ran.out().trim().to_owned();
        return Ok((!dir.is_empty()).then_some(dir));
    }
    if ran.status == Some(2)
        && ran
            .err()
            .contains("Unable to get active developer directory")
    {
        return Ok(None);
    }
    Err(format!("{}: {}", cmd.display(), ran.describe()))
}

fn installed(sys: &dyn System) -> Result<bool, String> {
    match sys.stat(Path::new(CLT_GIT)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{CLT_GIT}: {e}")),
    }
}

fn receipt_version(sys: &dyn System) -> Option<String> {
    let ran = sys
        .run(&Cmd::new("/usr/sbin/pkgutil").arg(format!("--pkg-info={CLT_RECEIPT}")))
        .ok()
        .filter(crate::port::Ran::success)?;
    ran.out()
        .lines()
        .find_map(|l| l.strip_prefix("version: ").map(|v| v.trim().to_owned()))
}

struct Placeholder<'a> {
    sys: &'a dyn System,
}

impl Drop for Placeholder<'_> {
    fn drop(&mut self) {
        let _ = self.sys.remove(Path::new(PLACEHOLDER));
    }
}

fn run_ok(sys: &dyn System, cmd: &Cmd) -> Result<String, String> {
    let ran = sys
        .run(cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    if ran.success() {
        Ok(ran.out())
    } else {
        Err(format!("{}: {}", cmd.display(), ran.describe()))
    }
}

impl CommandLineTools {
    fn install(sys: &dyn System) -> Result<(), String> {
        sys.create_new(Path::new(PLACEHOLDER), b"", 0o644)
            .map_err(|e| format!("{PLACEHOLDER}: {e}"))?;
        let _placeholder = Placeholder { sys };
        let list = Cmd::new(SOFTWAREUPDATE).arg("-l").budget(LIST_BUDGET);
        let listed = run_ok(sys, &list)?;
        let labels = parse_labels(&listed);
        let Some(label) = pick(&labels) else {
            return Err(format!(
                "{} lists no Command Line Tools for Xcode, with the on-demand placeholder in place",
                list.display()
            ));
        };
        let install = Cmd::new(SOFTWAREUPDATE)
            .args(["-i", label.label.as_str()])
            .budget(INSTALL_BUDGET);
        let ran = sys
            .run(&install)
            .map_err(|e| format!("{}: {e}", install.display()))?;
        if !ran.success() {
            return Err(format!("{}: {}", install.display(), ran.describe()));
        }
        if !installed(sys)? && receipt_version(sys).is_none() {
            let said = ran
                .err()
                .lines()
                .chain(ran.out().lines().rev())
                .map(str::trim)
                .find(|l| !l.is_empty() && !SOFTWAREUPDATE_BOILERPLATE.contains(l))
                .unwrap_or("it said nothing more")
                .to_owned();
            return Err(format!(
                "{} exited 0 but installed nothing ({CLT_RECEIPT} has no receipt): {said}",
                install.display()
            ));
        }
        Ok(())
    }
}

const SOFTWAREUPDATE_BOILERPLATE: [&str; 2] =
    ["Software Update Tool", "Finding available software"];

impl Item for CommandLineTools {
    fn id(&self) -> String {
        "developer.command-line-tools".to_owned()
    }

    fn family(&self) -> &'static str {
        "developer"
    }

    fn title(&self) -> String {
        "Command Line Tools for Xcode".to_owned()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let (installed, selected) = match installed(sys).and_then(|i| selected(sys).map(|s| (i, s)))
        {
            Ok(both) => both,
            Err(why) => return Probe::Unusable { why },
        };
        let planned = match (installed, &selected) {
            (true, Some(dir)) => {
                let version = receipt_version(sys).map_or_else(String::new, |v| format!(" {v}"));
                return Probe::Ok {
                    now: format!("installed{version}; selected {dir}"),
                };
            }
            (true, None) => Planned::Select,
            (false, _) => {
                let probe = Cmd::new("/usr/bin/curl")
                    .args([
                        "--disable",
                        "--proto",
                        "=https",
                        "-sSI",
                        "--max-time",
                        "20",
                        CATALOG,
                    ])
                    .budget(Duration::from_secs(30));
                match sys.run(&probe) {
                    Ok(r) if r.success() => {}
                    Ok(r) => {
                        return Probe::Blocked {
                            why: format!(
                                "Apple's software update catalog is unreachable ({}: {})",
                                probe.display(),
                                r.describe()
                            ),
                        };
                    }
                    Err(e) => {
                        return Probe::Blocked {
                            why: format!(
                                "Apple's software update catalog is unreachable ({}: {e})",
                                probe.display()
                            ),
                        };
                    }
                }
                Planned::Install
            }
        };
        if let Ok(mut slot) = self.planned.lock() {
            *slot = Some(planned);
        }
        Probe::Change {
            from: match planned {
                Planned::Install => "not installed".to_owned(),
                Planned::Select => "installed; no developer directory selected".to_owned(),
            },
            to: format!(
                "installed; selected {}",
                selected.as_deref().unwrap_or(CLT_DIR)
            ),
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if !installed(sys)? {
            Self::install(sys)?;
        }
        if selected(sys)?.is_none() {
            run_ok(sys, &Cmd::new(XCODE_SELECT).args(["--switch", CLT_DIR]))?;
        }
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        match (*self.planned.lock().ok()?)? {
            Planned::Install => Some(format!(
                "{}; {}",
                Cmd::new("/bin/rm").args(["-rf", CLT_DIR]).display(),
                Cmd::new("/usr/sbin/pkgutil")
                    .arg(format!("--forget={CLT_RECEIPT}"))
                    .display()
            )),
            Planned::Select => Some(Cmd::new(XCODE_SELECT).arg("--reset").display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::model::{HostFacts, Mode, Outcome};
    use crate::port::{Fake, Ran, Stat};

    fn ctx() -> Ctx {
        Ctx {
            facts: HostFacts::default(),
            users: Vec::new(),
            console: None,
            host_name: "lab-07".into(),
            cache_dir: "/c".into(),
            disrupt: true,
        }
    }

    const VM_LIST: &str = "Software Update Tool\n\
\n\
Finding available software\n\
Software Update found the following new or updated software:\n\
* Label: Command Line Tools for Xcode 27.0-27.0\n\
\tTitle: Command Line Tools for Xcode 27.0, Version: 27.0, Size: 519460KiB, Recommended: YES, \n\
* Label: macOS 27.0.1-26A434\n\
\tTitle: macOS 27.0.1, Version: 27.0.1, Size: 3235867KiB, Recommended: YES, Action: restart, \n\
";

    const OLD_LIST: &str = "Software Update Tool\n\
\n\
Finding available software\n\
Software Update found the following new or updated software:\n\
* Label: Command Line Tools for Xcode-15.3\n\
\tTitle: Command Line Tools for Xcode, Version: 15.3, Size: 707397KiB, Recommended: YES, \n\
* Label: Command Line Tools for Xcode-14.3\n\
\tTitle: Command Line Tools for Xcode, Version: 14.3, Size: 711657KiB, Recommended: YES, \n\
* Label: Command Line Tools beta 2 for Xcode-16.0\n\
\tTitle: Command Line Tools beta 2 for Xcode, Version: 16.0, Size: 721000KiB, Recommended: YES, \n\
";

    fn none_selected() -> Ran {
        Ran::exit(
            2,
            "xcode-select: error: Unable to get active developer directory. Use `sudo xcode-select --switch path/to/Xcode.app` to set one (or see `man xcode-select`)",
        )
    }

    fn git() -> Stat {
        Stat {
            uid: 0,
            gid: 0,
            mode: 0o755,
            dir: false,
            symlink: false,
            len: 3_600_000,
        }
    }

    fn reachable() -> Ran {
        Ran::ok("HTTP/1.1 404 Not Found\r\nServer: Apple\r\nContent-Length: 0\r\n\r\n")
    }

    const CURL: &str =
        "/usr/bin/curl --disable --proto =https -sSI --max-time 20 https://swscan.apple.com/";

    const RECEIPT: &str = "package-id: com.apple.pkg.CLTools_Executables\nversion: 27.0.0.0.1788430756\nvolume: /\nlocation: /\ninstall-time: 1792019691\n";

    fn no_receipt() -> Ran {
        Ran::exit(
            1,
            "No receipt for 'com.apple.pkg.CLTools_Executables' found at '/'.",
        )
    }

    const PKGUTIL: &str = "/usr/sbin/pkgutil --pkg-info=com.apple.pkg.CLTools_Executables";

    fn no_space() -> Ran {
        Ran {
            status: Some(0),
            stdout: b"Software Update Tool\n\nFinding available software\n".to_vec(),
            stderr: b"Not enough free disk space: a total of 9.52 GB is required.\n".to_vec(),
        }
    }

    fn installed_quietly() -> Ran {
        Ran::ok("Software Update Tool\n\nFinding available software\n")
    }

    #[test]
    fn the_newest_label_is_picked_in_the_new_space_form_and_the_old_hyphen_form() {
        let labels = parse_labels(VM_LIST);
        assert_eq!(
            labels,
            vec![Label {
                label: "Command Line Tools for Xcode 27.0-27.0".into(),
                version: vec![27, 0],
                beta: false,
            }]
        );
        let old = parse_labels(OLD_LIST);
        assert_eq!(old.len(), 3);
        assert_eq!(
            pick(&old).unwrap().label,
            "Command Line Tools for Xcode-15.3"
        );
        let betas: Vec<Label> = old.into_iter().filter(|l| l.beta).collect();
        assert_eq!(pick(&betas).unwrap().version, vec![16, 0]);
        assert_eq!(
            version_after_xcode("Command Line Tools for Xcode 26.4-26.4.1"),
            vec![26, 4]
        );
        let without = VM_LIST.replace(
            "* Label: Command Line Tools for Xcode 27.0-27.0\n\tTitle: Command Line Tools for Xcode 27.0, Version: 27.0, Size: 519460KiB, Recommended: YES, \n",
            "",
        );
        assert_ne!(without, VM_LIST);
        assert!(pick(&parse_labels(&without)).is_none());
    }

    #[test]
    fn a_title_version_outranks_the_labels_own_digits() {
        let text = "* Label: Command Line Tools for Xcode 26.4-26.4.1\n\tTitle: Command Line Tools for Xcode 26.4, Version: 26.4.1, Size: 1KiB, \n* Label: Command Line Tools for Xcode 26.4-26.4\n\tTitle: Command Line Tools for Xcode 26.4, Version: 26.4, Size: 1KiB, \n";
        assert_eq!(
            pick(&parse_labels(text)).unwrap().label,
            "Command Line Tools for Xcode 26.4-26.4.1"
        );
    }

    #[test]
    fn installed_and_selected_tools_are_ok_with_their_receipt_version() {
        let sys = Fake::root();
        sys.stat_of(CLT_GIT, git())
            .on(
                "/usr/bin/xcode-select -p",
                Ran::ok("/Library/Developer/CommandLineTools\n"),
            )
            .on(PKGUTIL, Ran::ok(RECEIPT));
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            items[0].probe(&sys, &ctx()),
            Probe::Ok {
                now: "installed 27.0.0.0.1788430756; selected /Library/Developer/CommandLineTools"
                    .into()
            }
        );
        assert!(sys.calls().iter().all(|c| !c.contains("softwareupdate")));
    }

    #[test]
    fn a_fresh_mac_installs_the_listed_label_then_selects_it_and_the_placeholder_goes() {
        let sys = Fake::root();
        sys.on("/usr/bin/xcode-select -p", none_selected())
            .on("/usr/bin/xcode-select -p", none_selected())
            .on(
                "/usr/bin/xcode-select -p",
                Ran::ok("/Library/Developer/CommandLineTools\n"),
            )
            .on(CURL, reachable())
            .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
            .on(
                "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                installed_quietly(),
            )
            .on(PKGUTIL, Ran::ok(RECEIPT))
            .on(
                "/usr/bin/xcode-select --switch /Library/Developer/CommandLineTools",
                Ran::ok(""),
            );
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        let rows = {
            let probe = items[0].probe(&sys, &ctx());
            assert_eq!(
                probe,
                Probe::Change {
                    from: "not installed".into(),
                    to: "installed; selected /Library/Developer/CommandLineTools".into()
                }
            );
            items[0].apply(&sys, &ctx()).unwrap();
            sys.stat_of(CLT_GIT, git());
            items[0].probe(&sys, &ctx())
        };
        assert!(matches!(rows, Probe::Ok { .. }), "{rows:?}");
        let calls = sys.calls();
        let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle)).unwrap();
        assert!(
            at("create /tmp/.com.apple.dt.CommandLineTools") < at("/usr/sbin/softwareupdate -l")
        );
        assert!(
            at("/usr/sbin/softwareupdate -i") < at("remove /tmp/.com.apple.dt.CommandLineTools")
        );
        assert!(at("remove /tmp/.com.apple.dt") < at("/usr/bin/xcode-select --switch"));
        assert_eq!(
            items[0].revert().as_deref(),
            Some(
                "/bin/rm -rf /Library/Developer/CommandLineTools; /usr/sbin/pkgutil --forget=com.apple.pkg.CLTools_Executables"
            )
        );
    }

    #[test]
    fn the_placeholder_is_removed_even_when_softwareupdate_lists_no_tools() {
        let sys = Fake::root();
        sys.on("/usr/bin/xcode-select -p", none_selected())
            .on(CURL, reachable())
            .on(
                "/usr/sbin/softwareupdate -l",
                Ran::ok(&VM_LIST.replace("Command Line Tools for Xcode 27.0", "Safari 27.0")),
            );
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert!(
            rows[0]
                .why
                .as_deref()
                .unwrap()
                .contains("lists no Command Line Tools")
        );
        assert!(
            sys.calls()
                .iter()
                .any(|c| c.starts_with("remove /tmp/.com.apple.dt.CommandLineTools"))
        );
        assert!(sys.written(PLACEHOLDER).is_none());
    }

    #[test]
    fn an_install_softwareupdate_refused_with_exit_0_fails_with_its_own_words() {
        let sys = Fake::root();
        sys.on("/usr/bin/xcode-select -p", none_selected())
            .on(CURL, reachable())
            .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
            .on(
                "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                no_space(),
            )
            .on(PKGUTIL, no_receipt());
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Failed);
        let why = rows[0].why.as_deref().unwrap();
        assert!(why.contains("installed nothing"), "{why}");
        assert!(why.contains("9.52 GB is required"), "{why}");
        let calls = sys.calls();
        assert!(calls.iter().all(|c| !c.contains("--switch")));
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("remove /tmp/.com.apple.dt"))
        );
    }

    #[test]
    fn no_route_to_apples_catalog_blocks_the_install_instead_of_failing_it() {
        let sys = Fake::root();
        sys.on("/usr/bin/xcode-select -p", none_selected()).on(
            CURL,
            Ran::exit(6, "curl: (6) Could not resolve host: swscan.apple.com"),
        );
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Blocked);
        assert!(
            rows[0]
                .why
                .as_deref()
                .unwrap()
                .contains("Could not resolve host")
        );
        assert!(
            sys.calls()
                .iter()
                .all(|c| !c.contains("softwareupdate") && !c.starts_with("write"))
        );
    }

    #[test]
    fn installed_tools_with_nothing_selected_are_only_selected() {
        let sys = Fake::root();
        sys.stat_of(CLT_GIT, git())
            .on("/usr/bin/xcode-select -p", none_selected())
            .on("/usr/bin/xcode-select -p", none_selected())
            .on(
                "/usr/bin/xcode-select -p",
                Ran::ok("/Library/Developer/CommandLineTools\n"),
            )
            .on(
                "/usr/bin/xcode-select --switch /Library/Developer/CommandLineTools",
                Ran::ok(""),
            );
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:#?}");
        assert_eq!(
            rows[0].revert.as_deref(),
            Some("/usr/bin/xcode-select --reset")
        );
        assert!(sys.calls().iter().all(|c| !c.contains("softwareupdate")));
    }

    #[test]
    fn a_selected_xcode_is_left_selected_when_the_tools_are_added() {
        let sys = Fake::root();
        sys.on(
            "/usr/bin/xcode-select -p",
            Ran::ok("/Applications/Xcode.app/Contents/Developer\n"),
        )
        .on(CURL, reachable())
        .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
        .on(
            "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
            installed_quietly(),
        )
        .on(PKGUTIL, Ran::ok(RECEIPT));
        let items = expand(
            &Table {
                command_line_tools: Some(true),
            },
            &ctx(),
        )
        .unwrap();
        items[0].apply(&sys, &ctx()).unwrap();
        assert!(sys.calls().iter().all(|c| !c.contains("--switch")));
    }

    #[test]
    fn false_or_absent_leaves_the_tools_alone() {
        assert!(
            expand(
                &Table {
                    command_line_tools: Some(false)
                },
                &ctx()
            )
            .unwrap()
            .is_empty()
        );
        assert!(expand(&Table::default(), &ctx()).unwrap().is_empty());
    }

    #[test]
    fn an_existing_clt_marker_is_never_replaced_or_removed_by_an_install() {
        let ready = || {
            let sys = Fake::root();
            sys.on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
                .on(
                    "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                    installed_quietly(),
                )
                .on(PKGUTIL, Ran::ok(RECEIPT));
            sys
        };

        let fresh = ready();
        CommandLineTools::install(&fresh).unwrap();
        assert!(fresh.written(PLACEHOLDER).is_none());
        assert!(
            fresh
                .calls()
                .iter()
                .any(|c| c == "/usr/sbin/softwareupdate -l")
        );
        assert!(
            fresh
                .calls()
                .iter()
                .any(|c| c.starts_with("/usr/sbin/softwareupdate -i "))
        );
        assert!(
            fresh
                .calls()
                .iter()
                .any(|c| c == &format!("remove {PLACEHOLDER}"))
        );

        let no_tools = Fake::root();
        no_tools.on("/usr/sbin/softwareupdate -l", Ran::ok("no tools listed\n"));
        assert!(CommandLineTools::install(&no_tools).is_err());
        assert!(no_tools.written(PLACEHOLDER).is_none());
        assert!(
            no_tools
                .calls()
                .iter()
                .any(|c| c == &format!("remove {PLACEHOLDER}"))
        );

        let saved = b"another install owns this marker\n";
        let target = "/unowned-clt-marker-target";
        let mut observed = Vec::new();
        for linked in [false, true] {
            let sys = ready();
            if linked {
                sys.file(target, saved).link(PLACEHOLDER, target);
                assert!(sys.stat(Path::new(PLACEHOLDER)).unwrap().symlink);
            } else {
                sys.file(PLACEHOLDER, saved);
                assert_eq!(sys.written(PLACEHOLDER).as_deref(), Some(saved.as_slice()));
            }
            let result = CommandLineTools::install(&sys);
            let calls = sys.calls();
            let preserved = if linked {
                sys.stat(Path::new(PLACEHOLDER)).is_ok_and(|s| s.symlink)
                    && sys.written(PLACEHOLDER).is_none()
                    && sys.written(target).as_deref() == Some(saved.as_slice())
            } else {
                sys.written(PLACEHOLDER).as_deref() == Some(saved.as_slice())
            };
            observed.push((
                result.is_err(),
                preserved,
                !calls.iter().any(|c| c.starts_with(SOFTWAREUPDATE)),
                !calls.iter().any(|c| c == &format!("remove {PLACEHOLDER}")),
            ));
        }
        assert_eq!(
            observed,
            vec![(true, true, true, true); 2],
            "occupied CLT markers retain their owners"
        );
    }
}
