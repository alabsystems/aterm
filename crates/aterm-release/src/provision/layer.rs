// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What `ship provision` audits past its own Apple, notary and credential checks: a release
//! host is a develop machine plus the `release` layer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_converge::{Class, Explain, Item, ItemReport, Layer, Mode, Outcome, Probe, Remedy};
use aterm_verify::provision::State;

use super::Check;

/// What the layer's items read: this machine ([`System`]), or a test's.
pub(crate) trait ReleaseHost: Send + Sync {
    /// The installed aterm's `aterm init --check --json --only base` report.
    /// # Errors
    /// It ran and printed no report: why.
    fn base(&self) -> Result<Option<aterm_converge::Report>, String>;

    /// Each row a bare `verify provision` provides, as its read-only check finds it.
    /// # Errors
    /// No `$HOME` names the caches.
    fn develop(&self) -> Result<Vec<(&'static str, State)>, String>;

    /// `/usr/bin/openssl version`'s line; `None` off macOS, where no LibreSSL is wanted.
    fn openssl(&self) -> Option<Result<String, String>>;

    /// The publication engine's checkout (`$PUBLICATION_ENGINE`, else `~/publication`).
    fn publication(&self) -> PathBuf;

    /// The site checkout `ship site` syncs, where one is named.
    /// # Errors
    /// `publish/site.toml` exists and does not parse.
    fn site(&self) -> Result<Option<PathBuf>, String>;

    /// The service-account key `ship site` deploys with, where one is named
    /// (`crate::site::Env::sa_key`).
    /// # Errors
    /// `publish/site.toml` exists and does not parse.
    fn site_key(&self) -> Result<Option<PathBuf>, String>;

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
    SiteKey,
}

struct LayerItem {
    id: &'static str,
    kind: Kind,
    host: Arc<dyn ReleaseHost>,
}

const ITEMS: [(&str, Kind); 6] = [
    ("base", Kind::Base),
    ("develop", Kind::Develop),
    ("release.openssl", Kind::Openssl),
    ("release.publication", Kind::Publication),
    ("release.site", Kind::Site),
    ("release.site-key", Kind::SiteKey),
];

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
            Ok(Some(dir)) if !dir.join("firebase.json").is_file() => Probe::Person {
                step: self.id.into(),
                how: format!("the site checkout {} has no firebase.json", dir.display()),
            },
            Ok(Some(dir)) => Probe::Ok {
                now: format!("{}/firebase.json", dir.display()),
            },
        }
    }

    /// The deployer's key is a file this account can open: opened, never read, so the
    /// audit holds none of its bytes.
    fn probe_site_key(&self) -> Probe {
        match self.host.site_key() {
            Err(why) => Probe::Unusable { why },
            Ok(None) => Probe::Person {
                step: self.id.into(),
                how: "no service-account key is named for the site deploy (FIREBASE_SA_KEY, or \
                      publish/site.toml's firebase_sa_key)"
                    .into(),
            },
            Ok(Some(key)) => {
                let opened = std::fs::File::open(&key)
                    .and_then(|f| f.metadata())
                    .map(|m| m.is_file());
                match opened {
                    Ok(true) => Probe::Ok {
                        now: format!("{} is readable", key.display()),
                    },
                    Ok(false) => Probe::Person {
                        step: self.id.into(),
                        how: format!(
                            "the site's service-account key {} is not a file",
                            key.display()
                        ),
                    },
                    Err(e) => Probe::Person {
                        step: self.id.into(),
                        how: format!(
                            "the site's service-account key {} cannot be read: {e}",
                            key.display()
                        ),
                    },
                }
            }
        }
    }

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
            Kind::SiteKey => Remedy::Command(
                "put the site deployer's service-account key where publish/site.toml's \
                 firebase_sa_key names it (or name it: FIREBASE_SA_KEY)"
                    .into(),
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
            Kind::Site => "the site checkout and its firebase.json".into(),
            Kind::SiteKey => "the site deployer's service-account key".into(),
        }
    }

    fn probe(&self) -> Probe {
        match self.kind {
            Kind::Base => self.probe_base(),
            Kind::Develop => self.probe_develop(),
            Kind::Openssl => self.probe_openssl(),
            Kind::Publication => self.probe_publication(),
            Kind::Site => self.probe_site(),
            Kind::SiteKey => self.probe_site_key(),
        }
    }

    fn apply(&self) -> Result<(), String> {
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

/// Every item over `host`, probed and planned, as `(id, check)` in [`ITEMS`] order.
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

    fn installed_aterm(&self) -> Option<PathBuf> {
        [
            self.home.join(".local/bin/aterm"),
            PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
        ]
        .into_iter()
        .find(|p| is_executable(p))
    }
}

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

    fn site_key(&self) -> Result<Option<PathBuf>, String> {
        crate::site::Env::from_process(&self.repo, None).map(|env| match &env.site_dir {
            Some(dir) => env.sa_key(dir),
            None => env.firebase_sa_key,
        })
    }

    fn find(&self, name: &str) -> Option<PathBuf> {
        aterm_uds::host::find_tool(name)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    struct Fake {
        home: PathBuf,
        base: Option<bool>,
        develop: Vec<(&'static str, State)>,
        openssl: Option<String>,
        site: Option<PathBuf>,
        site_key: Option<PathBuf>,
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
        fn site_key(&self) -> Result<Option<PathBuf>, String> {
            Ok(self.site_key.clone())
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

    fn whole(home: &Path) -> Fake {
        std::fs::create_dir_all(home.join("publication/.git")).unwrap();
        executable(&home.join("publication/bin/pub"));
        std::fs::create_dir_all(home.join("site")).unwrap();
        std::fs::write(home.join("site/firebase.json"), "{}").unwrap();
        std::fs::write(home.join("deployer.json"), "{}").unwrap();
        whole_keeping(home)
    }

    fn closing(home: &Path, checks: &[(&'static str, Check)]) -> crate::ledger::Result<()> {
        closing_as(home, checks, false, true)
    }

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

    fn line_of(fake: Fake, id: &str) -> Check {
        let checks = audit(&(Arc::new(fake) as Arc<dyn ReleaseHost>));
        checks.into_iter().find(|(i, _)| *i == id).unwrap().1
    }

    #[test]
    fn each_release_items_gap_fails_naming_what_is_missing() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let home = dir.path();

        let mut fake = whole(home);
        fake.tools = Vec::new();
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
        std::fs::remove_file(home.join("site/firebase.json")).unwrap();
        match line_of(whole_keeping(home), "release.site") {
            Check::Fail { what, .. } => assert!(what.contains("has no firebase.json"), "{what}"),
            other => panic!("{other:?}"),
        }

        let mut fake = whole(home);
        fake.site_key = None;
        match line_of(fake, "release.site-key") {
            Check::Fail { what, fix } => {
                assert!(what.contains("no service-account key"), "{what}");
                assert!(fix.contains("firebase_sa_key"), "{fix}");
            }
            other => panic!("{other:?}"),
        }
        std::fs::remove_file(home.join("deployer.json")).unwrap();
        match line_of(whole_keeping(home), "release.site-key") {
            Check::Fail { what, .. } => assert!(what.contains("cannot be read"), "{what}"),
            other => panic!("{other:?}"),
        }
        let mut fake = whole(home);
        fake.site_key = Some(home.join("site"));
        match line_of(fake, "release.site-key") {
            Check::Fail { what, .. } => assert!(what.contains("not a file"), "{what}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            line_of(whole(home), "release.site-key"),
            Check::Pass(_)
        ));
    }

    fn whole_keeping(home: &Path) -> Fake {
        Fake {
            home: home.to_path_buf(),
            base: Some(true),
            develop: vec![("fish", State::Present(home.join("fish")))],
            openssl: Some("LibreSSL 3.3.6".into()),
            site: Some(home.join("site")),
            site_key: Some(home.join("deployer.json")),
            tools: vec!["gitleaks"],
        }
    }

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
