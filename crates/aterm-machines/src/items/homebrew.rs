// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Homebrew from its notarized `Homebrew.pkg`, then formulae and casks as the owning user.

use super::apps::{
    Artifact, Notes, account, cached_verified, check_artifact, clear_stage, fresh_stage,
    last_words, not_cached, run_installer, stage_pkg, vet_pkg,
};
use super::{Ctx, Item};
use crate::model::{Effect, Probe, User};
use crate::port::{Cmd, System};
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

/// The prefix the pkg installs.
pub const PREFIX: &str = "/opt/homebrew";

/// The `brew` every item runs.
pub const BREW: &str = "/opt/homebrew/bin/brew";

/// The file that names the pkg's owner to its scripts.
pub const PKG_USER_PLIST: &str = "/var/tmp/.homebrew_pkg_user.plist";

/// Homebrew.pkg's receipt id.
pub const PKG_ID: &str = "sh.brew.homebrew";

/// Where Homebrew keeps its portable Ruby.
pub const VENDOR: &str = "/opt/homebrew/Library/Homebrew/vendor";

const RUBY_LINK: &str = "portable-ruby/current";

const INSTALL_BUDGET: Duration = Duration::from_secs(3600);

const BOOTSTRAP_BUDGET: Duration = Duration::from_secs(900);

/// The `[homebrew]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// The pinned `Homebrew.pkg`.
    pub pkg: Option<crate::items::apps::Artifact>,
    /// The account that owns `/opt/homebrew` (default: the first of `users`).
    pub owner: Option<String>,
    /// Formulae to install.
    #[serde(default)]
    pub formulae: Vec<String>,
    /// Casks to install.
    #[serde(default)]
    pub casks: Vec<String>,
}

/// The items `table` asks for: `homebrew.pkg`, `homebrew.ruby` after it, then
/// `homebrew.formula.<name>` and `homebrew.cask.<name>`, each waiting for both.
/// # Errors
/// One line naming what cannot be expanded: no owner, an owner this Mac does not have or
/// root, a malformed pkg pin, a formula or cask name `brew` could read as an option or a
/// path.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let owner_name = table
        .owner
        .clone()
        .or_else(|| ctx.users.first().map(|u| u.name.clone()))
        .ok_or("[homebrew]: no owner, and the baseline lists no users")?;
    let owner = account(ctx, &owner_name)
        .ok_or_else(|| format!("[homebrew] owner {owner_name}: no such account on this Mac"))?;
    if owner.uid == 0 {
        return Err("[homebrew] owner: Homebrew refuses to be owned by root".into());
    }
    if let Some(pkg) = &table.pkg {
        check_artifact("[homebrew] pkg", pkg)?;
        if pkg.team_id.is_none() {
            return Err(
                "[homebrew] pkg: team_id is required (Homebrew.pkg's installer certificate is 927JGANW46)"
                    .into(),
            );
        }
        if pkg.unsigned {
            return Err("[homebrew] pkg: Homebrew.pkg is signed; unsigned does not apply".into());
        }
    }
    let not_applicable = matches!(arch_gate(ctx), Some(Probe::NotApplicable { .. }));
    let mut items: Vec<Box<dyn Item>> = vec![
        Box::new(Pkg {
            art: table.pkg.clone(),
            owner: owner.clone(),
            not_applicable,
            notes: Notes::default(),
        }),
        Box::new(Ruby {
            owner: owner.clone(),
            not_applicable,
        }),
    ];
    for (names, cask) in [(&table.formulae, false), (&table.casks, true)] {
        for name in names {
            check_name(name)?;
            items.push(Box::new(Package {
                name: name.clone(),
                cask,
                owner: owner.clone(),
                not_applicable,
            }));
        }
    }
    Ok(items)
}

fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.contains("..")
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'@' | b'+' | b'.' | b'_' | b'/' | b'-')
        });
    if ok {
        Ok(())
    } else {
        Err(format!(
            "[homebrew]: {name:?} is not a formula or cask name"
        ))
    }
}

fn arch_gate(ctx: &Ctx) -> Option<Probe> {
    match ctx.facts.arch.as_str() {
        "arm64" => None,
        "" => Some(Probe::Unusable {
            why: "this Mac's architecture was not read".into(),
        }),
        other => Some(Probe::NotApplicable {
            why: format!("Homebrew supports Apple silicon only; this Mac is {other}"),
        }),
    }
}

fn brew(owner: &User, args: &[&str]) -> Cmd {
    Cmd::new(BREW)
        .args(args.iter().copied())
        .as_user(owner)
        .env("HOMEBREW_NO_ANALYTICS", "1")
        .env("HOMEBREW_NO_ENV_HINTS", "1")
}

fn pkg_user_plist(owner: &str) -> String {
    let name = aterm_codec::xml::escape(owner);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>HOMEBREW_PKG_USER</key>\n\
         \t<string>{name}</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

fn name_owner(sys: &dyn System, owner: &User) -> Result<(), String> {
    let path = Path::new(PKG_USER_PLIST);
    sys.write_file(path, pkg_user_plist(&owner.name).as_bytes(), 0o600)
        .map_err(|e| format!("{PKG_USER_PLIST}: {e}"))?;
    sys.chown(path, 0, 0)
        .map_err(|e| format!("chown {PKG_USER_PLIST}: {e}"))?;
    let ran = sys
        .run(&Cmd::new("/bin/chmod").arg("-N").arg(path))
        .map_err(|e| format!("chmod -N {PKG_USER_PLIST}: {e}"))?;
    if !ran.success() {
        return Err(format!("chmod -N {PKG_USER_PLIST}: {}", ran.describe()));
    }
    match sys.stat(path) {
        Ok(st) if st.uid == 0 && st.mode == 0o600 && !st.dir && !st.symlink => Ok(()),
        Ok(st) => Err(format!(
            "{PKG_USER_PLIST} reads uid {} mode {:o}, not root 0600; Homebrew.pkg would ignore it",
            st.uid, st.mode
        )),
        Err(e) => Err(format!("{PKG_USER_PLIST}: {e}")),
    }
}

struct Pkg {
    art: Option<Artifact>,
    owner: User,
    not_applicable: bool,
    notes: Notes,
}

impl Item for Pkg {
    fn id(&self) -> String {
        "homebrew.pkg".into()
    }

    fn family(&self) -> &'static str {
        "homebrew"
    }

    fn title(&self) -> String {
        format!("Homebrew in {PREFIX}, owned by {}", self.owner.name)
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        if let Some(gate) = arch_gate(ctx) {
            return gate;
        }
        match sys.stat(Path::new(BREW)) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let Some(art) = &self.art else {
                    return Probe::Blocked {
                        why: "Homebrew is not installed, and [homebrew] pkg names no Homebrew.pkg to install it from".into(),
                    };
                };
                if ctx.cached(&art.sha256).is_none() {
                    return Probe::Blocked {
                        why: not_cached(&art.sha256),
                    };
                }
                return Probe::Change {
                    from: "absent".into(),
                    to: self.title(),
                };
            }
            Err(e) => {
                return Probe::Unusable {
                    why: format!("{BREW}: {e}"),
                };
            }
        }
        match sys.stat(Path::new(PREFIX)) {
            Ok(st) if st.uid == self.owner.uid => {}
            Ok(st) => {
                return Probe::Blocked {
                    why: format!(
                        "{PREFIX} belongs to uid {}, not {} (uid {}); Homebrew serves one owner: \
                         chown -R {}:admin {PREFIX}, or name its owner in [homebrew] owner",
                        st.uid, self.owner.name, self.owner.uid, self.owner.name
                    ),
                };
            }
            Err(e) => {
                return Probe::Unusable {
                    why: format!("{PREFIX}: {e}"),
                };
            }
        }
        let cmd = brew(&self.owner, &["--version"]).env("HOMEBREW_NO_AUTO_UPDATE", "1");
        match sys.run(&cmd) {
            Ok(ran) if ran.success() => Probe::Ok {
                now: format!(
                    "{} in {PREFIX}, owned by {}",
                    ran.out().lines().next().unwrap_or("Homebrew").trim(),
                    self.owner.name
                ),
            },
            Ok(ran) => Probe::Unusable {
                why: format!("brew --version as {}: {}", self.owner.name, ran.describe()),
            },
            Err(e) => Probe::Unusable {
                why: format!("brew --version as {}: {e}", self.owner.name),
            },
        }
    }

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        self.notes.clear();
        let art = self
            .art
            .as_ref()
            .ok_or("[homebrew] pkg names no Homebrew.pkg")?;
        let cached = cached_verified(sys, ctx, &art.sha256)?;
        let stage = fresh_stage(sys)?;
        let done = stage_pkg(sys, &cached, &stage, art).and_then(|pkg| {
            vet_pkg(sys, &pkg, art, &stage, &self.notes)?;
            name_owner(sys, &self.owner)?;
            run_installer(sys, &pkg)
        });
        clear_stage(stage, done, &self.notes)
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "Homebrew's uninstall.sh (github.com/Homebrew/install) removes {PREFIX}; then \
             `pkgutil --forget {PKG_ID}`"
        ))
    }

    fn effect(&self) -> Effect {
        Effect::Now
    }

    fn after(&self) -> Vec<String> {
        if self.not_applicable {
            Vec::new()
        } else {
            vec!["developer.command-line-tools".into()]
        }
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.notes.joined()
    }
}

/// What [`portable_ruby`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RubyState {
    /// In place, at this version.
    Ready(String),
    /// Not in place: what is there instead.
    Missing(String),
}

/// Whether Homebrew's portable Ruby is in place, by the test `brew` itself makes before a Ruby
/// command (`Library/Homebrew/utils/ruby.sh`, `setup-ruby-path`, read on lab-03).
/// # Errors
/// One line when the answer could not be read.
fn portable_ruby(sys: &dyn System) -> Result<RubyState, String> {
    let vendor = Path::new(VENDOR);
    let pin = vendor.join("portable-ruby-version");
    let want = sys
        .read_file(&pin)
        .map_err(|e| format!("{}: {e}", pin.display()))?;
    let want = String::from_utf8_lossy(&want).trim().to_owned();
    if want.is_empty() || want.contains('/') || want.starts_with('.') {
        return Err(format!("{} names {want:?}, not a version", pin.display()));
    }
    let link = vendor.join(RUBY_LINK);
    match sys.stat(&link) {
        Ok(st) if st.symlink => {}
        Ok(_) => {
            return Ok(RubyState::Missing(format!(
                "{} is not a link to {want}",
                link.display()
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RubyState::Missing("not fetched".into()));
        }
        Err(e) => return Err(format!("{}: {e}", link.display())),
    }
    let target = sys
        .read_link(&link)
        .map_err(|e| format!("{}: {e}", link.display()))?;
    let pinned = vendor.join("portable-ruby").join(&want);
    if target != Path::new(&want) && target != pinned {
        return Ok(RubyState::Missing(format!(
            "portable Ruby {}, not the {want} this Homebrew wants",
            target.display()
        )));
    }
    let ruby = pinned.join("bin/ruby");
    match sys.stat(&ruby) {
        Ok(st) if !st.dir && !st.symlink && st.mode & 0o111 != 0 => Ok(RubyState::Ready(want)),
        Ok(st) => Ok(RubyState::Missing(format!(
            "{} is not an executable file (mode {:o})",
            ruby.display(),
            st.mode
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(RubyState::Missing(format!("{} is missing", ruby.display())))
        }
        Err(e) => Err(format!("{}: {e}", ruby.display())),
    }
}

struct Ruby {
    owner: User,
    not_applicable: bool,
}

impl Item for Ruby {
    fn id(&self) -> String {
        "homebrew.ruby".into()
    }

    fn family(&self) -> &'static str {
        "homebrew"
    }

    fn title(&self) -> String {
        format!("Homebrew's portable Ruby in {VENDOR}")
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        if let Some(gate) = arch_gate(ctx) {
            return gate;
        }
        match sys.stat(Path::new(BREW)) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Probe::Change {
                    from: "absent (Homebrew is not installed yet)".into(),
                    to: self.title(),
                };
            }
            Err(e) => {
                return Probe::Unusable {
                    why: format!("{BREW}: {e}"),
                };
            }
        }
        match portable_ruby(sys) {
            Ok(RubyState::Ready(version)) => Probe::Ok {
                now: format!("portable Ruby {version}"),
            },
            Ok(RubyState::Missing(what)) => Probe::Change {
                from: what,
                to: self.title(),
            },
            Err(why) => Probe::Unusable { why },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let cmd = brew(&self.owner, &["config"])
            .env("HOMEBREW_NO_AUTO_UPDATE", "1")
            .budget(BOOTSTRAP_BUDGET);
        match sys.run(&cmd) {
            Ok(ran) if ran.success() => Ok(()),
            Ok(ran) => Err(format!(
                "brew config as {}: {}{}",
                self.owner.name,
                ran.describe(),
                last_words(&ran)
            )),
            Err(e) => Err(format!("brew config as {}: {e}", self.owner.name)),
        }
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "none apart from Homebrew: the Ruby is Homebrew's own, in {VENDOR}/portable-ruby, \
             and goes with {PREFIX}"
        ))
    }

    fn effect(&self) -> Effect {
        Effect::Now
    }

    fn after(&self) -> Vec<String> {
        if self.not_applicable {
            Vec::new()
        } else {
            vec!["homebrew.pkg".into()]
        }
    }
}

struct Package {
    name: String,
    cask: bool,
    owner: User,
    not_applicable: bool,
}

impl Package {
    const fn flag(&self) -> &'static str {
        if self.cask { "--cask" } else { "--formula" }
    }

    const fn what(&self) -> &'static str {
        if self.cask { "cask" } else { "formula" }
    }
}

impl Item for Package {
    fn id(&self) -> String {
        format!("homebrew.{}.{}", self.what(), self.name)
    }

    fn family(&self) -> &'static str {
        "homebrew"
    }

    fn title(&self) -> String {
        format!("brew {} {}", self.what(), self.name)
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        if let Some(gate) = arch_gate(ctx) {
            return gate;
        }
        match sys.stat(Path::new(BREW)) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Probe::Change {
                    from: "absent (Homebrew is not installed yet)".into(),
                    to: "installed".into(),
                };
            }
            Err(e) => {
                return Probe::Unusable {
                    why: format!("{BREW}: {e}"),
                };
            }
        }
        match portable_ruby(sys) {
            Ok(RubyState::Ready(_)) => {}
            Ok(RubyState::Missing(what)) => {
                return Probe::Waits {
                    on: "homebrew.ruby".into(),
                    why: format!("Homebrew's portable Ruby: {what}; waits for homebrew.ruby"),
                };
            }
            Err(why) => {
                return Probe::Unusable {
                    why: format!("Homebrew's portable Ruby: {why}"),
                };
            }
        }
        let cmd = brew(
            &self.owner,
            &["list", self.flag(), "--versions", &self.name],
        )
        .env("HOMEBREW_NO_AUTO_UPDATE", "1");
        match sys.run(&cmd) {
            Ok(ran) if ran.success() && !ran.out().trim().is_empty() => Probe::Ok {
                now: ran.out().trim().to_owned(),
            },
            Ok(ran)
                if ran.status == Some(1) && ran.out().trim().is_empty() && ran.err().is_empty() =>
            {
                Probe::Change {
                    from: "absent".into(),
                    to: "installed".into(),
                }
            }
            Ok(ran) => Probe::Unusable {
                why: format!(
                    "brew list {} as {}: {}",
                    self.name,
                    self.owner.name,
                    ran.describe()
                ),
            },
            Err(e) => Probe::Unusable {
                why: format!("brew list {} as {}: {e}", self.name, self.owner.name),
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let cmd = brew(&self.owner, &["install", self.flag(), &self.name]).budget(INSTALL_BUDGET);
        let ran = sys
            .run(&cmd)
            .map_err(|e| format!("brew install {}: {e}", self.name))?;
        if ran.success() {
            Ok(())
        } else {
            Err(format!(
                "brew install {}: {}{}",
                self.name,
                ran.describe(),
                last_words(&ran)
            ))
        }
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "sudo -u {} {BREW} uninstall {} {}",
            self.owner.name,
            self.flag(),
            self.name
        ))
    }

    fn effect(&self) -> Effect {
        Effect::Now
    }

    fn after(&self) -> Vec<String> {
        if self.not_applicable {
            Vec::new()
        } else {
            vec!["homebrew.pkg".into(), "homebrew.ruby".into()]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::apps::tests::{
        PKGUTIL_HOMEBREW, SPCTL_HOMEBREW_OFF, admin, ctx_with, dir, err, ok, out_status, sha_of,
        stage_path,
    };
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran, Stat};

    const HOMEBREW_SHA: &str = "e021295947a4a9b7ca1a8eff310c9ea1db14d923c5acf7d79e325b60a81f1a15";

    const INSTALLER_HOMEBREW: &str = "installer: Package name is Homebrew\ninstaller: Installing at base path /\ninstaller: The install was successful.\n";

    const BREW_VERSION: &str = "Homebrew >=4.3.0 (shallow or no git repository)\n";

    const ENV: &str = "HOMEBREW_NO_ANALYTICS=1 HOMEBREW_NO_ENV_HINTS=1";

    fn table(sha: &str) -> Table {
        Table {
            pkg: Some(Artifact {
                url: "https://github.com/Homebrew/brew/releases/download/7.0.9/Homebrew-7.0.9.pkg"
                    .into(),
                sha256: sha.into(),
                team_id: Some("927JGANW46".into()),
                unsigned: false,
            }),
            owner: None,
            formulae: vec!["jq".into()],
            casks: vec![],
        }
    }

    fn owned_by(uid: u32) -> Stat {
        Stat { uid, ..dir() }
    }

    fn brew_installed(sys: &Fake) {
        sys.stat_of(
            BREW,
            Stat {
                dir: false,
                ..owned_by(501)
            },
        );
        sys.stat_of(PREFIX, owned_by(501));
    }

    fn ruby_in_place(sys: &Fake) {
        sys.file(&format!("{VENDOR}/portable-ruby-version"), b"4.0.7\n");
        sys.link(&format!("{VENDOR}/portable-ruby/current"), "4.0.7");
        let ruby = format!("{VENDOR}/portable-ruby/4.0.7/bin/ruby");
        sys.file(&ruby, b"ruby").stat_of(
            &ruby,
            Stat {
                mode: 0o555,
                dir: false,
                ..owned_by(501)
            },
        );
    }

    #[test]
    fn the_pkg_waits_for_command_line_tools_and_each_formula_waits_for_its_ruby() {
        let ctx = ctx_with(&Fake::root(), &[], true);
        let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
        let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
        assert_eq!(
            ids,
            ["homebrew.pkg", "homebrew.ruby", "homebrew.formula.jq"]
        );
        assert_eq!(items[0].after(), ["developer.command-line-tools"]);
        assert_eq!(items[1].after(), ["homebrew.pkg"]);
        assert_eq!(items[2].after(), ["homebrew.pkg", "homebrew.ruby"]);
    }

    #[test]
    fn an_intel_mac_is_not_applicable_and_an_unread_architecture_is_unknown() {
        let sys = Fake::root();
        let mut ctx = ctx_with(&sys, &[], true);
        ctx.facts.arch = "x86_64".into();
        let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
        for item in &items {
            assert!(item.after().is_empty());
            assert!(matches!(
                item.probe(&sys, &ctx),
                Probe::NotApplicable { .. }
            ));
        }
        let report = crate::engine::run(
            &items,
            Mode::Apply,
            &sys,
            &ctx,
            &mut crate::engine::NoJournal,
        );
        assert!(
            report
                .iter()
                .all(|row| row.outcome == Outcome::NotApplicable),
            "{report:?}"
        );
        ctx.facts.arch = String::new();
        assert!(matches!(items[0].probe(&sys, &ctx), Probe::Unusable { .. }));
        let unread = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
        assert_eq!(unread[0].after(), ["developer.command-line-tools"]);
        assert_eq!(unread[1].after(), ["homebrew.pkg"]);
        assert_eq!(unread[2].after(), ["homebrew.pkg", "homebrew.ruby"]);
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn an_owner_this_mac_lacks_is_refused_at_expansion() {
        let ctx = ctx_with(&Fake::root(), &[], true);
        let mut t = table(HOMEBREW_SHA);
        t.owner = Some("nobody-here".into());
        assert!(expand(&t, &ctx).err().unwrap().contains("no such account"));
        let mut t = table(HOMEBREW_SHA);
        t.formulae = vec!["--force".into()];
        assert!(expand(&t, &ctx).err().unwrap().contains("not a formula"));
    }

    #[test]
    fn without_homebrew_the_pkg_plans_an_install_from_the_cache() {
        let sys = Fake::root();
        let ctx = ctx_with(&sys, &[], true);
        let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
        assert!(
            matches!(items[0].probe(&sys, &ctx), Probe::Blocked { why } if why.contains("not in the cache"))
        );
        let ctx = ctx_with(&sys, &[(HOMEBREW_SHA, b"pkg")], true);
        assert_eq!(
            items[0].probe(&sys, &ctx),
            Probe::Change {
                from: "absent".into(),
                to: "Homebrew in /opt/homebrew, owned by admin".into()
            }
        );
        assert!(matches!(items[1].probe(&sys, &ctx), Probe::Change { .. }));
        assert!(matches!(items[2].probe(&sys, &ctx), Probe::Change { .. }));
    }

    #[test]
    fn installing_the_pkg_names_its_owner_in_a_root_only_file_first() {
        let sys = Fake::root();
        let bytes = b"Homebrew.pkg";
        let sha = sha_of(bytes);
        let stage = stage_path(0);
        let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
        sys.on(
            &format!("/usr/sbin/pkgutil --check-signature {stage}/Homebrew-7.0.9.pkg"),
            ok(PKGUTIL_HOMEBREW),
        );
        sys.on(
            &format!("/usr/sbin/spctl -a -vv -t install {stage}/Homebrew-7.0.9.pkg"),
            err(0, SPCTL_HOMEBREW_OFF),
        );
        sys.on(&format!("/bin/chmod -N {PKG_USER_PLIST}"), ok(""));
        sys.on(
            &format!("/usr/sbin/installer -pkg {stage}/Homebrew-7.0.9.pkg -target /"),
            ok(INSTALLER_HOMEBREW),
        );
        let items = expand(&table(&sha), &ctx).unwrap();
        items[0].apply(&sys, &ctx).unwrap();
        let calls = sys.calls();
        let at = |c: &str| {
            calls
                .iter()
                .position(|x| x == c)
                .unwrap_or_else(|| panic!("{c}: {calls:#?}"))
        };
        assert!(!calls.iter().any(|c| c.starts_with("admin$")), "{calls:#?}");
        let write = at(&format!("write {PKG_USER_PLIST} 600"));
        assert!(write < at(&format!("chown 0:0 {PKG_USER_PLIST}")));
        assert!(
            at(&format!("/bin/chmod -N {PKG_USER_PLIST}"))
                < at(&format!(
                    "/usr/sbin/installer -pkg {stage}/Homebrew-7.0.9.pkg -target /"
                ))
        );
        assert!(
            at(&format!(
                "/usr/sbin/spctl -a -vv -t install {stage}/Homebrew-7.0.9.pkg"
            )) < write
        );
        assert!(
            at(&format!("copy /c/{sha} {stage}/Homebrew-7.0.9.pkg 600"))
                < at(&format!(
                    "/usr/sbin/pkgutil --check-signature {stage}/Homebrew-7.0.9.pkg"
                ))
        );
        let plist = String::from_utf8(sys.written(PKG_USER_PLIST).unwrap()).unwrap();
        assert!(
            plist.contains("<key>HOMEBREW_PKG_USER</key>\n\t<string>admin</string>"),
            "{plist}"
        );
        assert!(
            items[0]
                .detail(&sys, &ctx)
                .unwrap()
                .contains("override=security disabled")
        );
    }

    #[test]
    fn a_pkg_signed_by_another_installer_never_runs() {
        let sys = Fake::root();
        let bytes = b"Homebrew.pkg";
        let sha = sha_of(bytes);
        let stage = stage_path(0);
        let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
        sys.on(
            &format!("/usr/sbin/pkgutil --check-signature {stage}/Homebrew-7.0.9.pkg"),
            ok(&PKGUTIL_HOMEBREW.replace("927JGANW46", "AAAAAAAAAA")),
        );
        let items = expand(&table(&sha), &ctx).unwrap();
        assert!(
            items[0]
                .apply(&sys, &ctx)
                .unwrap_err()
                .contains("not a Developer ID Installer of team 927JGANW46")
        );
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.contains("installer -pkg") || c.starts_with("write"))
        );
    }

    #[test]
    fn homebrew_owned_by_its_owner_is_ok_and_owned_by_another_is_blocked() {
        let sys = Fake::root();
        sys.stat_of(
            BREW,
            Stat {
                dir: false,
                ..owned_by(501)
            },
        );
        sys.stat_of(PREFIX, owned_by(501));
        sys.on(
            &format!("admin$ {ENV} HOMEBREW_NO_AUTO_UPDATE=1 {BREW} --version"),
            ok(BREW_VERSION),
        );
        let ctx = ctx_with(&sys, &[], true);
        let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
        assert_eq!(
            items[0].probe(&sys, &ctx),
            Probe::Ok {
                now: "Homebrew >=4.3.0 (shallow or no git repository) in /opt/homebrew, owned by admin".into()
            }
        );
        sys.stat_of(PREFIX, owned_by(502));
        assert!(
            matches!(items[0].probe(&sys, &ctx), Probe::Blocked { why } if why.contains("belongs to uid 502"))
        );
    }

    #[test]
    fn a_formula_is_read_and_installed_as_the_owner_with_homebrew_s_quiet_switches() {
        let sys = Fake::root();
        brew_installed(&sys);
        ruby_in_place(&sys);
        let list =
            format!("admin$ {ENV} HOMEBREW_NO_AUTO_UPDATE=1 {BREW} list --formula --versions jq");
        sys.on(&list, out_status(1, ""));
        sys.on(&list, ok("jq 1.8.2\n"));
        sys.on(
            &format!("admin$ {ENV} {BREW} install --formula jq"),
            ok("==> Pouring jq--1.8.2.arm64_golden_gate.bottle.1.tar.gz\n"),
        );
        let ctx = ctx_with(&sys, &[], true);
        let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
        let rows = crate::engine::run(
            &items[1..],
            Mode::Apply,
            &sys,
            &ctx,
            &mut crate::engine::NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Ok, "{:?}", rows[0]);
        assert_eq!(rows[1].outcome, Outcome::Changed, "{:?}", rows[1]);
        assert_eq!(rows[1].from.as_deref(), Some("absent"));
        assert_eq!(
            rows[1].revert.as_deref(),
            Some("sudo -u admin /opt/homebrew/bin/brew uninstall --formula jq")
        );
        assert_eq!(
            items[2].probe(&sys, &ctx),
            Probe::Ok {
                now: "jq 1.8.2".into()
            }
        );
    }

    #[test]
    fn a_brew_list_that_fails_out_loud_is_no_answer_rather_than_absent() {
        let sys = Fake::root();
        brew_installed(&sys);
        ruby_in_place(&sys);
        sys.on(
            &format!("admin$ {ENV} HOMEBREW_NO_AUTO_UPDATE=1 {BREW} list --formula --versions jq"),
            err(
                1,
                "Error: Permission denied @ dir_s_mkdir - /opt/homebrew/var",
            ),
        );
        let ctx = ctx_with(&sys, &[], true);
        let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
        assert!(
            matches!(items[2].probe(&sys, &ctx), Probe::Unusable { why } if why.contains("Permission denied")),
        );
    }

    #[test]
    fn a_failed_brew_install_says_brew_s_last_words() {
        let sys = Fake::root();
        sys.stat_of(
            BREW,
            Stat {
                dir: false,
                ..owned_by(501)
            },
        );
        sys.on(
            &format!("admin$ {ENV} {BREW} install --cask nosuchcask"),
            Ran {
                status: Some(1),
                stdout: b"==> Searching\n".to_vec(),
                stderr:
                    b"Error: Cask 'nosuchcask' is unavailable: No Cask with this name exists.\n"
                        .to_vec(),
            },
        );
        let mut t = table(HOMEBREW_SHA);
        t.formulae.clear();
        t.casks = vec!["nosuchcask".into()];
        let ctx = ctx_with(&sys, &[], true);
        let items = expand(&t, &ctx).unwrap();
        assert_eq!(items[2].id(), "homebrew.cask.nosuchcask");
        let why = items[2].apply(&sys, &ctx).unwrap_err();
        assert!(why.contains("No Cask with this name exists"), "{why}");
    }

    #[test]
    fn the_owner_plist_is_the_document_homebrew_s_scripts_read() {
        assert_eq!(
            pkg_user_plist(&admin().name),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>HOMEBREW_PKG_USER</key>\n\t<string>admin</string>\n</dict>\n</plist>\n"
        );
    }

    const CONFIG: &str = "admin$ HOMEBREW_NO_ANALYTICS=1 HOMEBREW_NO_ENV_HINTS=1 HOMEBREW_NO_AUTO_UPDATE=1 /opt/homebrew/bin/brew config";

    #[test]
    fn homebrew_without_its_ruby_fetches_only_the_ruby_as_the_owner() {
        let sys = Fake::root();
        brew_installed(&sys);
        sys.file(&format!("{VENDOR}/portable-ruby-version"), b"4.0.7\n");
        let ctx = ctx_with(&sys, &[], true);
        let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
        let Probe::Change { from, .. } = items[1].probe(&sys, &ctx) else {
            panic!("{:?}", items[1].probe(&sys, &ctx));
        };
        assert_eq!(from, "not fetched");
        sys.on(CONFIG, ok(""));
        items[1].apply(&sys, &ctx).unwrap();
        assert_eq!(sys.calls(), [CONFIG]);
        ruby_in_place(&sys);
        assert_eq!(
            items[1].probe(&sys, &ctx),
            Probe::Ok {
                now: "portable Ruby 4.0.7".into()
            }
        );
    }

    #[test]
    fn a_ruby_other_than_the_one_homebrew_names_is_fetched_again_and_an_unread_one_is_unknown() {
        let sys = Fake::root();
        brew_installed(&sys);
        ruby_in_place(&sys);
        sys.file(&format!("{VENDOR}/portable-ruby-version"), b"4.0.8\n");
        let ctx = ctx_with(&sys, &[], true);
        let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
        assert!(
            matches!(items[1].probe(&sys, &ctx), Probe::Change { from, .. } if from.contains("4.0.7")),
        );
        sys.stat_fails(
            &format!("{VENDOR}/portable-ruby/current"),
            std::io::ErrorKind::PermissionDenied,
        );
        assert!(matches!(items[1].probe(&sys, &ctx), Probe::Unusable { .. }));
        assert!(matches!(items[2].probe(&sys, &ctx), Probe::Unusable { .. }));
        let sys = Fake::root();
        brew_installed(&sys);
        assert!(matches!(items[1].probe(&sys, &ctx), Probe::Unusable { .. }));
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    }

    #[test]
    fn a_formula_waits_for_the_ruby_and_never_starts_its_fetch() {
        let sys = Fake::root();
        brew_installed(&sys);
        sys.file(&format!("{VENDOR}/portable-ruby-version"), b"4.0.7\n");
        sys.on(
            &format!("admin$ {ENV} HOMEBREW_NO_AUTO_UPDATE=1 {BREW} --version"),
            ok(BREW_VERSION),
        );
        let ctx = ctx_with(&sys, &[], true);
        let items = expand(&table(HOMEBREW_SHA), &ctx).unwrap();
        let outcomes = |mode| {
            crate::engine::run(&items, mode, &sys, &ctx, &mut crate::engine::NoJournal)
                .into_iter()
                .map(|r| r.outcome)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            outcomes(Mode::Plan),
            [Outcome::Ok, Outcome::Planned, Outcome::Planned]
        );
        sys.on(CONFIG, err(1, "curl: (6) Could not resolve host: ghcr.io"));
        assert_eq!(
            outcomes(Mode::Apply),
            [Outcome::Ok, Outcome::Failed, Outcome::Blocked]
        );
        assert!(
            !sys.calls().iter().any(|c| c.contains(" list ")),
            "{:#?}",
            sys.calls()
        );
    }
}
