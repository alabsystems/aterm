// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What `ship provision` audits past its own Apple, notary and credential checks
//! (docs/DESIGN-init-system-2026-10-20.md §4.6): a release host is a develop machine
//! plus the `release` layer. Each is an aterm-converge item, probed through one port
//! ([`ReleaseHost`]) and run in the engine's plan mode, so nothing here changes the
//! machine; a full `ship provision` runs `verify provision` first, and this reads what
//! that left.
//!
//! - `base`: the installed aterm's `aterm init --check --json --only base`, complete.
//! - `develop`: every row a bare `targo --unverified verify provision` provides, in
//!   place (read in process with that verb's own check: `aterm init --only develop`
//!   is S8's).
//! - `release.openssl`: `/usr/bin/openssl` is macOS's LibreSSL (KEEP-OS, probe only).
//! - `release.publication`: the publication engine checkout and `bin/pub`, with
//!   gitleaks where `pub stage` finds it.
//! - `release.site`: the site checkout with its executable `deploy.sh`.
//! - `release.firebase`: node and firebase-tools, which `deploy.sh` runs, until S17.
//!
//! `release.rustup-x86_64`, `.identity`, `.notary`, `.credentials` and `.gh` are the
//! `x86 slice`, Apple certificate, `notary`, `profile` and `github` checks beside these.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_converge::{Class, Explain, Item, ItemReport, Layer, Mode, Outcome, Probe, Remedy};
use aterm_verify::provision::State;

use super::Check;

/// What the layer's items read: this machine ([`System`]), or a test's.
pub(crate) trait ReleaseHost: Send + Sync {
    /// The installed aterm's `aterm init --check --json --only base` report; `Ok(None)`
    /// where no installed aterm is found.
    ///
    /// # Errors
    /// It ran and printed no report: why.
    fn base(&self) -> Result<Option<aterm_converge::Report>, String>;

    /// Each row a bare `verify provision` provides, as its read-only check finds it.
    ///
    /// # Errors
    /// No `$HOME` names the caches.
    fn develop(&self) -> Result<Vec<(&'static str, State)>, String>;

    /// `/usr/bin/openssl version`'s line; `None` off macOS, where no LibreSSL is wanted.
    fn openssl(&self) -> Option<Result<String, String>>;

    /// The publication engine's checkout (`$PUBLICATION_ENGINE`, else `~/publication`).
    fn publication(&self) -> PathBuf;

    /// The site checkout `ship site` syncs (`$SITE_DIR`, else `publish/site.toml`'s
    /// `dir`), where one is named.
    ///
    /// # Errors
    /// `publish/site.toml` exists and does not parse.
    fn site(&self) -> Result<Option<PathBuf>, String>;

    /// The program `name` as aterm's resolver finds it (`aterm_uds::host::find_tool`).
    fn find(&self, name: &str) -> Option<PathBuf>;
}

/// What one item probes, and its one remedy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Base,
    Develop,
    Openssl,
    Publication,
    Site,
    Firebase,
}

/// One item of the layers this audit reads.
struct LayerItem {
    id: &'static str,
    kind: Kind,
    host: Arc<dyn ReleaseHost>,
}

/// The items, in the order the audit prints them.
const ITEMS: [(&str, Kind); 6] = [
    ("base", Kind::Base),
    ("develop", Kind::Develop),
    ("release.openssl", Kind::Openssl),
    ("release.publication", Kind::Publication),
    ("release.site", Kind::Site),
    ("release.firebase", Kind::Firebase),
];

/// Whether `path` is a regular file with an execute bit.
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

impl LayerItem {
    fn probe_base(&self) -> Probe {
        match self.host.base() {
            Ok(None) => Probe::Blocked {
                why: "no installed aterm (~/.local/bin/aterm, /Applications/aterm.app) to run \
                      `aterm init --check --only base`"
                    .into(),
            },
            Ok(Some(report)) if report.complete => Probe::Ok {
                now: format!(
                    "`aterm init --check --only base` complete ({} items)",
                    report.items.len()
                ),
            },
            Ok(Some(report)) => {
                let open: Vec<&str> = report
                    .items
                    .iter()
                    .filter(|r| {
                        !matches!(
                            r.outcome,
                            Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
                        )
                    })
                    .map(|r| r.id.as_str())
                    .collect();
                Probe::Change {
                    from: if open.is_empty() {
                        "`aterm init --check --only base` incomplete".into()
                    } else {
                        format!("base items open: {}", open.join(", "))
                    },
                    to: "complete".into(),
                }
            }
            Err(why) => Probe::Unusable { why },
        }
    }

    fn probe_develop(&self) -> Probe {
        let rows = match self.host.develop() {
            Ok(rows) => rows,
            Err(why) => return Probe::Unusable { why },
        };
        let open: Vec<&str> = rows
            .iter()
            .filter(|(_, s)| matches!(s, State::Missing(_) | State::Failed(_)))
            .map(|(name, _)| *name)
            .collect();
        if open.is_empty() {
            let placed: Vec<&str> = rows
                .iter()
                .filter(|(_, s)| s.in_place())
                .map(|(n, _)| *n)
                .collect();
            Probe::Ok {
                now: format!("`verify provision` rows in place: {}", placed.join(", ")),
            }
        } else {
            Probe::Change {
                from: format!("`verify provision` rows missing: {}", open.join(", ")),
                to: "every row in place".into(),
            }
        }
    }

    fn probe_openssl(&self) -> Probe {
        match self.host.openssl() {
            None => Probe::NotApplicable {
                why: "release signing runs on macOS only".into(),
            },
            Some(Ok(line)) if line.starts_with("LibreSSL") => Probe::Ok { now: line },
            Some(Ok(line)) => Probe::Person {
                step: self.id.into(),
                how: format!(
                    "/usr/bin/openssl answers `{line}`, not macOS's LibreSSL: an OpenSSL 3 \
                     writes a PKCS#12 `security import` cannot read"
                ),
            },
            Some(Err(why)) => Probe::Unusable { why },
        }
    }

    fn probe_publication(&self) -> Probe {
        let engine = self.host.publication();
        let shown = engine.display();
        if !engine.join(".git").exists() || !is_executable(&engine.join("bin/pub")) {
            return Probe::Person {
                step: self.id.into(),
                how: format!("no publication engine at {shown} (a checkout with bin/pub)"),
            };
        }
        match self.host.find("gitleaks") {
            Some(gitleaks) => Probe::Ok {
                now: format!("{shown}/bin/pub, gitleaks {}", gitleaks.display()),
            },
            None => Probe::Person {
                step: self.id.into(),
                how: format!(
                    "the publication engine at {shown} has no gitleaks where `pub stage` finds \
                     it (its own dependency)"
                ),
            },
        }
    }

    fn probe_site(&self) -> Probe {
        match self.host.site() {
            Err(why) => Probe::Unusable { why },
            Ok(None) => Probe::Person {
                step: self.id.into(),
                how: "no site checkout is named (SITE_DIR, or publish/site.toml's dir)".into(),
            },
            Ok(Some(dir)) if !is_executable(&dir.join("deploy.sh")) => Probe::Person {
                step: self.id.into(),
                how: format!(
                    "the site checkout {} has no executable deploy.sh",
                    dir.display()
                ),
            },
            Ok(Some(dir)) => Probe::Ok {
                now: format!("{}/deploy.sh", dir.display()),
            },
        }
    }

    fn probe_firebase(&self) -> Probe {
        let missing: Vec<&str> = ["node", "firebase"]
            .into_iter()
            .filter(|t| self.host.find(t).is_none())
            .collect();
        if missing.is_empty() {
            Probe::Ok {
                now: "node and firebase-tools, which deploy.sh runs".into(),
            }
        } else {
            Probe::Person {
                step: self.id.into(),
                how: format!(
                    "no {} for the site's deploy.sh (firebase-tools under node, until S17 \
                     deploys over Firebase Hosting's REST API)",
                    missing.join(" or ")
                ),
            }
        }
    }

    /// The one act that makes this item right.
    fn remedy(&self) -> Remedy {
        match self.kind {
            Kind::Base => Remedy::Init {
                only: "base".into(),
            },
            Kind::Develop => Remedy::Provision {
                args: String::new(),
            },
            Kind::Openssl => Remedy::None,
            Kind::Publication => Remedy::Command(
                "clone the publication engine at ~/publication (or name one: \
                 PUBLICATION_ENGINE), with gitleaks on PATH"
                    .into(),
            ),
            Kind::Site => Remedy::Command(
                "clone the site repo and name it: SITE_DIR, or publish/site.toml's dir".into(),
            ),
            Kind::Firebase => Remedy::Command(
                "install node and the site's pinned firebase-tools on this release host".into(),
            ),
        }
    }
}

impl Item for LayerItem {
    fn id(&self) -> String {
        self.id.into()
    }

    fn family(&self) -> &'static str {
        match self.kind {
            Kind::Base => "base",
            Kind::Develop => "develop",
            _ => "release",
        }
    }

    fn layer(&self) -> Layer {
        match self.kind {
            Kind::Base => Layer::Base,
            Kind::Develop => Layer::Develop,
            _ => Layer::Release,
        }
    }

    fn class(&self) -> Class {
        match self.kind {
            Kind::Base => Class::Relay,
            Kind::Develop => Class::Install,
            _ => Class::Person,
        }
    }

    fn title(&self) -> String {
        match self.kind {
            Kind::Base => "every machine's base layer".into(),
            Kind::Develop => "the tools a checkout's tests run".into(),
            Kind::Openssl => "macOS's LibreSSL, which release signing uses".into(),
            Kind::Publication => "the publication engine".into(),
            Kind::Site => "the site checkout and its deploy.sh".into(),
            Kind::Firebase => "node and firebase-tools, for the site deploy".into(),
        }
    }

    fn probe(&self) -> Probe {
        match self.kind {
            Kind::Base => self.probe_base(),
            Kind::Develop => self.probe_develop(),
            Kind::Openssl => self.probe_openssl(),
            Kind::Publication => self.probe_publication(),
            Kind::Site => self.probe_site(),
            Kind::Firebase => self.probe_firebase(),
        }
    }

    fn apply(&self) -> Result<(), String> {
        // The audit runs in plan mode only: what acquires these is `verify provision`
        // (develop), `aterm init` (base) or a person (the release layer).
        Err(format!("{} is audited here, never applied", self.id))
    }

    fn explain(&self, probe: &Probe) -> Explain {
        let why = match probe {
            Probe::Ok { now } => now.clone(),
            Probe::Change { from, to } => format!("{from}; wanted {to}"),
            Probe::Person { how, .. } => how.clone(),
            Probe::Blocked { why }
            | Probe::Waits { why, .. }
            | Probe::NotApplicable { why }
            | Probe::Deferred { why }
            | Probe::Unusable { why } => why.clone(),
        };
        let remedy = match (self.kind, probe) {
            (Kind::Base, Probe::Blocked { .. }) => Remedy::Command(
                "install aterm (tools/install.sh), then `aterm init --only base`".into(),
            ),
            _ => self.remedy(),
        };
        Explain { why, remedy }
    }

    fn essential(&self) -> bool {
        true
    }
}

/// The items over `host`.
fn items(host: &Arc<dyn ReleaseHost>) -> Vec<Box<dyn Item>> {
    ITEMS
        .iter()
        .map(|&(id, kind)| {
            Box::new(LayerItem {
                id,
                kind,
                host: Arc::clone(host),
            }) as Box<dyn Item>
        })
        .collect()
}

/// `remedy` as the `fix:` line an audit prints: a command that exists, or the act.
fn fix_words(remedy: &Remedy) -> String {
    match remedy {
        Remedy::Repair => "aterm pkg repair".into(),
        Remedy::Init { only } => format!("aterm init --only {only}"),
        Remedy::Develop => "aterm init --develop".into(),
        Remedy::Provision { args } => format!("targo --unverified verify provision {args}")
            .trim_end()
            .to_owned(),
        Remedy::Person { how, .. } => how.clone(),
        Remedy::Network => "run it again once online".into(),
        Remedy::Command(command) => command.clone(),
        Remedy::None => "nothing on this machine changes it".into(),
    }
}

/// `row` as one audit line.
fn check_of(row: &ItemReport) -> Check {
    let explain = row.explain.clone().unwrap_or(Explain {
        why: row.why.clone().unwrap_or_else(|| row.title.clone()),
        remedy: Remedy::None,
    });
    match row.outcome {
        Outcome::Ok | Outcome::Changed => {
            Check::Pass(row.to.clone().unwrap_or_else(|| row.title.clone()))
        }
        Outcome::NotApplicable => Check::Pass(format!("not needed here — {}", explain.why)),
        Outcome::Unknown => Check::CouldNotRun(explain.why),
        Outcome::Deferred => Check::Todo {
            what: explain.why,
            next: fix_words(&explain.remedy),
        },
        Outcome::Planned | Outcome::Person | Outcome::Blocked | Outcome::Failed => Check::Fail {
            what: explain.why,
            fix: fix_words(&explain.remedy),
        },
    }
}

/// Every item over `host`, probed and planned (nothing applied), as `(id, line)` in
/// [`ITEMS`] order.
pub(crate) fn audit(host: &Arc<dyn ReleaseHost>) -> Vec<(&'static str, Check)> {
    let items = items(host);
    match aterm_converge::run(&items, Mode::Plan, &mut aterm_converge::ReadOnly) {
        Ok(rows) => ITEMS
            .iter()
            .map(|&(id, _)| {
                let check = rows.iter().find(|r| r.id == id).map_or_else(
                    || Check::CouldNotRun(format!("the engine reported no row for {id}")),
                    check_of,
                );
                (id, check)
            })
            .collect(),
        Err(refusal) => ITEMS
            .iter()
            .map(|&(id, _)| (id, Check::CouldNotRun(refusal.to_string())))
            .collect(),
    }
}

/// This machine.
#[cfg(unix)]
pub(crate) struct System {
    repo: PathBuf,
    home: PathBuf,
}

#[cfg(unix)]
impl System {
    /// This machine, for the checkout at `repo` and the account at `home`.
    pub(crate) fn new(repo: &Path, home: &Path) -> Self {
        Self {
            repo: repo.to_path_buf(),
            home: home.to_path_buf(),
        }
    }

    /// The installed aterm: the account's `~/.local/bin/aterm` link, else the app's.
    fn installed_aterm(&self) -> Option<PathBuf> {
        [
            self.home.join(".local/bin/aterm"),
            PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
        ]
        .into_iter()
        .find(|p| is_executable(p))
    }
}

/// The JSON report among `stdout`'s lines: the last that parses as one.
fn report_in(stdout: &str) -> Option<aterm_converge::Report> {
    stdout
        .lines()
        .rev()
        .filter(|l| l.trim_start().starts_with('{'))
        .find_map(|l| aterm_json::from_str(l.trim()).ok())
}

#[cfg(unix)]
impl ReleaseHost for System {
    fn base(&self) -> Result<Option<aterm_converge::Report>, String> {
        use aterm_uds::output::CommandOutput as _;
        let Some(aterm) = self.installed_aterm() else {
            return Ok(None);
        };
        // Read only (`--check`), and base items alone: nothing of this login is read.
        let out = aterm_uds::spawnfd::command(&aterm)
            .args(["init", "--check", "--json", "--only", "base"])
            .output_owned()
            .map_err(|e| format!("could not run {}: {e}", aterm.display()))?;
        report_in(&String::from_utf8_lossy(&out.stdout))
            .map(Some)
            .ok_or_else(|| {
                let err = String::from_utf8_lossy(&out.stderr);
                format!(
                    "{} init --check --json --only base printed no report (exit {:?}): {}",
                    aterm.display(),
                    out.status.code(),
                    err.trim().lines().last().unwrap_or_default()
                )
            })
    }

    fn develop(&self) -> Result<Vec<(&'static str, State)>, String> {
        let machine = aterm_verify::provision::this_machine(&self.repo)
            .ok_or("no $HOME names this user's cache")?;
        let host = aterm_verify::provision::System::new(&self.repo);
        Ok(
            aterm_verify::provision::select(&aterm_verify::provision::Options::default())
                .into_iter()
                .map(|row| {
                    (
                        row.name,
                        aterm_verify::provision::check(row, &machine, &host),
                    )
                })
                .collect(),
        )
    }

    fn openssl(&self) -> Option<Result<String, String>> {
        use aterm_uds::output::CommandOutput as _;
        if !cfg!(target_os = "macos") {
            return None;
        }
        Some(
            aterm_uds::spawnfd::command("/usr/bin/openssl")
                .arg("version")
                .output_owned()
                .map_err(|e| format!("could not run /usr/bin/openssl: {e}"))
                .and_then(|o| {
                    let line = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if o.status.success() && !line.is_empty() {
                        Ok(line)
                    } else {
                        Err(format!(
                            "/usr/bin/openssl version exited {:?}",
                            o.status.code()
                        ))
                    }
                }),
        )
    }

    fn publication(&self) -> PathBuf {
        std::env::var_os("PUBLICATION_ENGINE")
            .filter(|v| !v.is_empty())
            .map_or_else(|| self.home.join("publication"), PathBuf::from)
    }

    fn site(&self) -> Result<Option<PathBuf>, String> {
        crate::site::Env::from_process(&self.repo, None).map(|env| env.site_dir)
    }

    fn find(&self, name: &str) -> Option<PathBuf> {
        aterm_uds::host::find_tool(name)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A release host made of a scratch home, with what each item reads set by hand.
    struct Fake {
        home: PathBuf,
        base: Option<bool>,
        develop: Vec<(&'static str, State)>,
        openssl: Option<String>,
        site: Option<PathBuf>,
        tools: Vec<&'static str>,
    }

    impl ReleaseHost for Fake {
        fn base(&self) -> Result<Option<aterm_converge::Report>, String> {
            Ok(self.base.map(|complete| {
                aterm_converge::Report::new(aterm_converge::ReportMode::Check, complete, Vec::new())
            }))
        }
        fn develop(&self) -> Result<Vec<(&'static str, State)>, String> {
            Ok(self.develop.clone())
        }
        fn openssl(&self) -> Option<Result<String, String>> {
            self.openssl.clone().map(Ok)
        }
        fn publication(&self) -> PathBuf {
            self.home.join("publication")
        }
        fn site(&self) -> Result<Option<PathBuf>, String> {
            Ok(self.site.clone())
        }
        fn find(&self, name: &str) -> Option<PathBuf> {
            self.tools
                .contains(&name)
                .then(|| PathBuf::from(format!("/fake/bin/{name}")))
        }
    }

    fn executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A host every item of the layer passes on.
    fn whole(home: &Path) -> Fake {
        std::fs::create_dir_all(home.join("publication/.git")).unwrap();
        executable(&home.join("publication/bin/pub"));
        executable(&home.join("site/deploy.sh"));
        Fake {
            home: home.to_path_buf(),
            base: Some(true),
            develop: vec![("fish", State::Present(home.join("fish")))],
            openssl: Some("LibreSSL 3.3.6".into()),
            site: Some(home.join("site")),
            tools: vec!["gitleaks", "node", "firebase"],
        }
    }

    /// `ship provision`'s closing over `checks` on a minted machine: its verdict.
    fn closing(home: &Path, checks: &[(&'static str, Check)]) -> crate::ledger::Result<()> {
        closing_as(home, checks, false, true)
    }

    /// `ship provision`'s closing over `checks`, a `--check` or not, minted or not.
    fn closing_as(
        home: &Path,
        checks: &[(&'static str, Check)],
        check_only: bool,
        key_exists: bool,
    ) -> crate::ledger::Result<()> {
        let (fails, waiting) = super::super::tally(checks);
        super::super::close(super::super::Closing {
            id: "fake-host",
            home: home.to_str().unwrap(),
            check_only,
            report: None,
            profile: None,
            open: super::super::open_labels(checks),
            authority: None,
            roster_kept: None,
            key_exists,
            fails,
            waiting,
            host_cannot_cut: false,
        })
    }

    /// The release item `id`'s line over `fake`.
    fn line_of(fake: Fake, id: &str) -> Check {
        let checks = audit(&(Arc::new(fake) as Arc<dyn ReleaseHost>));
        checks.into_iter().find(|(i, _)| *i == id).unwrap().1
    }

    /// Each release item's own gap is a FAIL naming what is missing: gitleaks beside the
    /// publication engine, an executable deploy.sh in the site checkout, node and
    /// firebase-tools. NEGATIVE CONTROL: the whole host passes each (the test above).
    #[test]
    fn each_release_items_gap_fails_naming_what_is_missing() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let home = dir.path();

        let mut fake = whole(home);
        fake.tools = vec!["node", "firebase"];
        match line_of(fake, "release.publication") {
            Check::Fail { what, .. } => assert!(what.contains("no gitleaks"), "{what}"),
            other => panic!("{other:?}"),
        }

        let mut fake = whole(home);
        fake.site = None;
        match line_of(fake, "release.site") {
            Check::Fail { what, .. } => assert!(what.contains("no site checkout"), "{what}"),
            other => panic!("{other:?}"),
        }
        {
            use std::os::unix::fs::PermissionsExt as _;
            let deploy = home.join("site/deploy.sh");
            std::fs::set_permissions(&deploy, std::fs::Permissions::from_mode(0o644)).unwrap();
            match line_of(whole_keeping(home), "release.site") {
                Check::Fail { what, .. } => {
                    assert!(what.contains("has no executable deploy.sh"), "{what}");
                }
                other => panic!("{other:?}"),
            }
        }

        for (gone, named) in [("node", "no node"), ("firebase", "no firebase")] {
            let mut fake = whole(home);
            fake.tools.retain(|t| *t != gone);
            match line_of(fake, "release.firebase") {
                Check::Fail { what, .. } => assert!(what.contains(named), "{gone}: {what}"),
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(
            line_of(whole(home), "release.firebase"),
            Check::Pass(_)
        ));
    }

    /// [`whole`]'s host over files already laid, left as they are.
    fn whole_keeping(home: &Path) -> Fake {
        Fake {
            home: home.to_path_buf(),
            base: Some(true),
            develop: vec![("fish", State::Present(home.join("fish")))],
            openssl: Some("LibreSSL 3.3.6".into()),
            site: Some(home.join("site")),
            tools: vec!["gitleaks", "node", "firebase"],
        }
    }

    /// A `--check` of a host not yet minted exits non-zero naming each open item, as one of
    /// a minted host does. NEGATIVE CONTROL: the same audit with nothing open exits 0.
    #[test]
    fn an_unminted_check_with_a_gap_fails_naming_it() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let home = dir.path();
        let fake: Arc<dyn ReleaseHost> = Arc::new(whole(home));
        assert!(closing_as(home, &audit(&fake), true, false).is_ok());
        std::fs::remove_file(home.join("publication/bin/pub")).unwrap();
        let said = closing_as(home, &audit(&fake), true, false)
            .expect_err("an open item fails the audit")
            .to_string();
        assert!(
            said.contains("unminted") && said.contains("release.publication (gap)"),
            "{said}"
        );
    }

    /// The design's S7 test: a missing release item makes `ship provision` exit non-zero
    /// naming it. NEGATIVE CONTROL: the same host with the engine in place closes READY
    /// TO CUT.
    #[test]
    fn a_missing_release_item_fails_the_run_naming_it() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let home = dir.path();
        let fake: Arc<dyn ReleaseHost> = Arc::new(whole(home));
        let checks = audit(&fake);
        assert!(
            checks.iter().all(|(_, c)| matches!(c, Check::Pass(_))),
            "{checks:#?}"
        );
        assert!(
            closing(home, &checks).is_ok(),
            "the whole layer closes READY TO CUT"
        );

        std::fs::remove_file(home.join("publication/bin/pub")).unwrap();
        let checks = audit(&fake);
        let err = closing(home, &checks).expect_err("a missing release item fails the run");
        let said = err.to_string();
        assert!(said.contains("release.publication (gap)"), "{said}");
        assert!(
            !said.contains("release.site"),
            "only the missing item is named: {said}"
        );
        let (_, check) = checks
            .iter()
            .find(|(id, _)| *id == "release.publication")
            .unwrap();
        match check {
            Check::Fail { what, fix } => {
                assert!(what.contains("no publication engine at"), "{what}");
                assert!(fix.contains("PUBLICATION_ENGINE"), "{fix}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// Base and develop gate the run as the release items do, each naming its one
    /// command; a LibreSSL off macOS is no gap.
    #[test]
    fn base_and_develop_name_their_commands() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let home = dir.path();
        let mut fake = whole(home);
        fake.base = Some(false);
        fake.develop = vec![
            ("fish", State::Missing("fish 4.9.3: not in …".into())),
            ("pwsh", State::Present(home.join("pwsh"))),
        ];
        fake.openssl = None;
        let checks = audit(&(Arc::new(fake) as Arc<dyn ReleaseHost>));
        let find = |id: &str| &checks.iter().find(|(i, _)| *i == id).unwrap().1;
        match find("base") {
            Check::Fail { fix, .. } => assert_eq!(fix, "aterm init --only base"),
            other => panic!("{other:?}"),
        }
        match find("develop") {
            Check::Fail { what, fix } => {
                assert!(what.contains("missing: fish"), "{what}");
                assert_eq!(fix, "targo --unverified verify provision");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(find("release.openssl"), Check::Pass(p) if p.starts_with("not needed")));
        let said = closing(home, &checks).unwrap_err().to_string();
        assert!(said.contains("base (gap), develop (gap)"), "{said}");
    }

    #[test]
    fn the_report_is_the_last_json_line_init_prints() {
        let report =
            aterm_converge::Report::new(aterm_converge::ReportMode::Check, true, Vec::new());
        let json = aterm_json::to_string(&report).unwrap();
        let earlier =
            aterm_converge::Report::new(aterm_converge::ReportMode::Check, false, Vec::new());
        assert_ne!(earlier, report);
        let earlier = aterm_json::to_string(&earlier).unwrap();
        assert_eq!(
            report_in(&format!("{earlier}\nnoise\n{{not json\n{json}\n")),
            Some(report)
        );
        assert_eq!(report_in("aterm init: unknown argument\n"), None);
    }
}
