// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What `ship provision` audits past its own Apple, notary and credential checks: a release
//! host is a develop machine plus the `release` layer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_converge::{Class, Explain, Item, ItemReport, Layer, Mode, Outcome, Probe, Remedy};
use aterm_verify::provision::State;

use super::Check;
use crate::release_steps::Bootstrap;

/// What the layer's items read: this machine ([`System`]), or a test's.
pub(crate) trait ReleaseHost: Send + Sync {
    /// The installed aterm's `aterm init --check --json --only base` report.
    /// # Errors
    /// It ran and printed no report: why.
    fn base(&self) -> Result<Option<aterm_converge::Report>, String>;

    /// Each row `verify provision --release` provides, checked read-only.
    /// # Errors
    /// No `$HOME` names the caches.
    fn develop(&self) -> Result<Vec<(&'static str, State)>, String>;

    /// `/usr/bin/openssl version`'s line; `None` off macOS, where no LibreSSL is wanted.
    fn openssl(&self) -> Option<Result<String, String>>;

    /// The publication engine's checkout whose launcher the audit starts: `$PUBLICATION_ENGINE`
    /// (a release hands each child its own engine), else the engine a release of this clone made
    /// and runs ([`aterm_verify::engine::engine_copy`]), else `~/publication`, whose origin a
    /// first release clones. A checkout, not a commit, because the probe runs its `bin/pub`; what
    /// it judges is this Mac's side of the launch (the signed prerequisites the launcher leases),
    /// and the engine's files themselves are read from a commit ([`Self::engine_registry`]).
    fn publication(&self) -> PathBuf;

    /// The engine's registry ([`REGISTRY`]) on one commit of the engine
    /// ([`aterm_verify::engine`]: a named checkout's HEAD, else its origin's main fetched now),
    /// never a working tree: where it was read, and its text.
    /// # Errors
    /// No engine is named, its commit cannot be read, or it holds no registry: why, naming where.
    fn engine_registry(&self) -> Result<(String, String), String>;

    /// Whether the engine's launcher starts successfully on the publication PATH.
    /// # Errors
    /// The launcher or its runtime is unavailable.
    fn publication_bootstrap(&self) -> Result<(), Bootstrap>;

    /// Exercise source signing separately from the Apple certificate and notary tools.
    /// # Errors
    /// The canonical ring the release reads is incomplete or unauthorized
    /// ([`SigningGap::Ring`]), or the signing tools are unusable.
    fn source_signing(&self) -> Result<(), SigningGap>;

    /// The site checkout `ship site` syncs, where one is named.
    /// # Errors
    /// `publish/site.toml` exists and does not parse.
    fn site(&self) -> Result<Option<PathBuf>, String>;

    /// The service-account key `ship site` deploys with, where one is named
    /// (`crate::site::Env::sa_key`).
    /// # Errors
    /// `publish/site.toml` exists and does not parse.
    fn site_key(&self) -> Result<Option<PathBuf>, String>;

    /// Where the vault delivers the deployer's key: its catalog path under this account's
    /// home (`aterm_secrets::need::Want::hold_path`).
    fn catalog_site_key(&self) -> Option<PathBuf>;

    /// This Mac's need for the deployer's key (docs/DESIGN-credential-requests-2026-10-27.md
    /// §12.1), moved by `step`, as the need's line: `None` when there is none on file.
    /// # Errors
    /// The need ledger cannot be read or written.
    fn site_key_need(&self, step: NeedStep) -> Result<Option<String>, String>;

    /// This machine's git lists `url`'s `HEAD`, with no prompt, as the engine's fetches and
    /// pushes reach it.
    /// # Errors
    /// The listing failed or did not finish: git's last line.
    fn reach(&self, url: &str) -> Result<(), String>;

    /// Whether Google accepts the deployer's key at `key` and lets its account update the
    /// configured Hosting site, as `ship site --check-access` asks: a token exchange, a read
    /// of the site and an IAM permission test, none of which deploys or changes policy. `Ok`
    /// is the line to show.
    /// # Errors
    /// The key, the site or the permission could not be verified: why.
    fn site_access(&self, key: &Path) -> Result<String, String>;

    /// Whether the origin of the site checkout at `site` would take a push from this Mac's
    /// git: a dry run to a ref no sync writes ([`PUSH_PROBE`]), which sends nothing.
    /// # Errors
    /// git refused or did not finish: its last line.
    fn site_push(&self, site: &Path) -> Result<(), String>;

    /// Whether the identity this Mac's agh routes a write of the GitHub repository `slug` to
    /// may push to it: one read of the repository's permissions as that identity, what
    /// `agh can <slug> push` asks. Nothing is written.
    /// # Errors
    /// [`PushGap::Denied`] when no writer is routed or it holds no push;
    /// [`PushGap::Unasked`] when GitHub could not be asked.
    fn can_push(&self, slug: &str) -> Result<(), PushGap>;

    /// Post the deployer key's need to the lab's table now, as `aterm secrets need
    /// site-deployer-key --purpose release.site-key` does (the raise, which is idempotent,
    /// and this Mac's pass), so the request does not wait for the app's own pass. Its output
    /// is discarded.
    /// # Errors
    /// No installed aterm ran it: why. The need stays on file for the next pass.
    fn post_site_key_need(&self) -> Result<(), String>;
}

/// Why a push right is not shown ([`ReleaseHost::can_push`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PushGap {
    /// No writer is routed for the repository, GitHub refuses it, or it holds no push.
    Denied(String),
    /// GitHub could not be asked: no verdict.
    Unasked(String),
}

/// Why source signing is not ready.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SigningGap {
    /// The canonical ring under `~/.aterm/release-ring/` (key, record, roster pair), the one
    /// the release's step 1 and the engine read, does not authorize a cut, which
    /// `ship provision --id` restores from `dist/` or the release channel.
    Ring(String),
    /// The ring is whole, but the signing tools or a signature failed.
    Unusable(String),
}

/// What a provision does to the deployer key's need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NeedStep {
    /// Only read where it stands (`--check`, and every audit).
    Read,
    /// The key is missing at the catalog path: ask the lab for it.
    Raise,
    /// The deployer's check passed: the need, if any, is met.
    Satisfied,
}

/// What one item probes, and its one remedy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Base,
    Develop,
    Openssl,
    Publication,
    SourceSigning,
    EngineReach,
    Site,
    SiteKey,
    SiteAccess,
    SitePush,
    Writes,
    Tap,
}

struct LayerItem {
    id: &'static str,
    kind: Kind,
    host: Arc<dyn ReleaseHost>,
}

const ITEMS: [(&str, Kind); 12] = [
    ("base", Kind::Base),
    ("develop", Kind::Develop),
    ("release.openssl", Kind::Openssl),
    ("release.publication", Kind::Publication),
    ("release.source-signing", Kind::SourceSigning),
    ("release.engine-reach", Kind::EngineReach),
    ("release.site", Kind::Site),
    ("release.site-key", Kind::SiteKey),
    ("release.site-access", Kind::SiteAccess),
    ("release.site-push", Kind::SitePush),
    ("release.writes", Kind::Writes),
    ("release.tap", Kind::Tap),
];

/// The deployer key's item, whose row a release's step 1 completes with the vault's answer.
const SITE_KEY: &str = "release.site-key";

/// The Homebrew tap the after-cut step aligns: the publication engine's `pub tap --bump`
/// writes this repository and no other.
const TAP_REPO: &str = "alabsystems/homebrew-tap";

/// The ref [`ReleaseHost::site_push`] names in its dry run: one no site sync writes, so the
/// answer is the origin's verdict on this Mac's credential and never on how far the checkout
/// is behind its branch.
pub(crate) const PUSH_PROBE: &str = "refs/heads/aterm-release-push-probe";

/// The GitHub `OWNER/REPO` a registry address names, however its host is spelled (the
/// registry spells some with an SSH alias): its last two path components, without `.git`.
fn address_slug(url: &str) -> Option<String> {
    let path = url.trim().trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.rsplit(['/', ':']);
    let (repo, owner) = (parts.next()?, parts.next()?);
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    (valid(owner) && valid(repo)).then(|| format!("{owner}/{repo}"))
}

/// The engine's registry, in its checkout.
const REGISTRY: &str = "registry/repos.toml";

/// The repository `ship release` stages and publishes, as the engine's registry names it.
const ENGINE_REPO: &str = "aterm";

/// [`ENGINE_REPO`]'s addresses in the registry `text`, as `(role, url)`: where a stage reads
/// the dev tree, pushes the staging snapshot and reads the public tree.
fn engine_addresses(text: &str) -> Result<Vec<(&'static str, String)>, String> {
    let doc = aterm_toml::from_str::<aterm_toml::Table>(text).map_err(|e| e.to_string())?;
    let entry = doc
        .get("repos")
        .and_then(|repos| repos.get(ENGINE_REPO))
        .ok_or_else(|| format!("no [repos.{ENGINE_REPO}] entry"))?;
    ["dev", "staging", "release"]
        .into_iter()
        .map(|role| {
            entry
                .get(role)
                .and_then(aterm_toml::Value::as_str)
                .map(|url| (role, url.to_string()))
                .ok_or_else(|| format!("[repos.{ENGINE_REPO}] names no {role} address"))
        })
        .collect()
}

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
        match self.host.publication_bootstrap() {
            Ok(()) => Probe::Ok {
                now: format!("{shown}/bin/pub with signed, leased publication prerequisites"),
            },
            Err(Bootstrap::Prerequisites(why)) => Probe::Change {
                from: why,
                to: "signed cpython, gitleaks, openssl-signing and lean4 packages".into(),
            },
            Err(Bootstrap::Failed(why)) => Probe::Unusable { why },
        }
    }

    /// Each of [`ENGINE_REPO`]'s registry addresses, listed by this machine's git. The
    /// registry names them as the engine's home machine spells them (an SSH alias, an
    /// `insteadOf` route), so a machine without that spelling stages nothing.
    fn probe_engine_reach(&self) -> Probe {
        let (shown, text) = match self.host.engine_registry() {
            Ok(read) => read,
            Err(why) => {
                return Probe::Blocked {
                    why: format!("cannot read the engine's registry: {why}"),
                };
            }
        };
        let addresses = match engine_addresses(&text) {
            Ok(addresses) => addresses,
            Err(why) => {
                return Probe::Unusable {
                    why: format!("{shown}: {why}"),
                };
            }
        };
        let unreached: Vec<String> = addresses
            .iter()
            .filter_map(|(role, url)| {
                self.host
                    .reach(url)
                    .err()
                    .map(|why| format!("{role} {url} ({why})"))
            })
            .collect();
        if unreached.is_empty() {
            let reached: Vec<String> = addresses
                .iter()
                .map(|(role, url)| format!("{role} {url}"))
                .collect();
            Probe::Ok {
                now: format!("git reaches {ENGINE_REPO}'s {}", reached.join(", ")),
            }
        } else {
            Probe::Person {
                step: self.id.into(),
                how: format!(
                    "this machine's git cannot reach {ENGINE_REPO}'s {}",
                    unreached.join("; ")
                ),
            }
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

    /// Where the lab stands on the deployer key, for a row that lacks it: read only.
    fn site_key_lab(&self) -> String {
        match self.host.site_key_need(NeedStep::Read) {
            Ok(Some(line)) => format!("; the lab: {line}"),
            Ok(None) => String::new(),
            Err(why) => format!("; the need ledger: {why}"),
        }
    }

    /// Validate the key with the deployment parser, without sending a token request.
    fn probe_site_key(&self) -> Probe {
        match self.host.site_key() {
            Err(why) => Probe::Unusable { why },
            Ok(None) => Probe::Person {
                step: self.id.into(),
                how: format!(
                    "no service-account key is named for the site deploy (FIREBASE_SA_KEY, or \
                     publish/site.toml's firebase_sa_key){}",
                    self.site_key_lab()
                ),
            },
            Ok(Some(key)) => match validate_site_key(&key) {
                Ok(()) => Probe::Ok {
                    now: format!(
                        "{} parses as a signing key for Google's token endpoint; Google authentication is not checked",
                        key.display()
                    ),
                },
                Err(why) => Probe::Person {
                    step: self.id.into(),
                    how: format!(
                        "the site's service-account key is unusable: {why}{}",
                        self.site_key_lab()
                    ),
                },
            },
        }
    }

    /// Google's own answer for the deployer's key, asked only of a key that parses.
    fn probe_site_access(&self) -> Probe {
        let key = match self.host.site_key() {
            Ok(Some(key)) if validate_site_key(&key).is_ok() => key,
            Ok(_) => {
                return Probe::Blocked {
                    why: format!(
                        "not asked of Google: the deployer's key is not usable yet ({SITE_KEY})"
                    ),
                };
            }
            Err(why) => return Probe::Unusable { why },
        };
        match self.host.site_access(&key) {
            Ok(now) => Probe::Ok { now },
            Err(why) => Probe::Person {
                step: self.id.into(),
                how: format!(
                    "Google does not let the deployer's key at {} update the site: {why}",
                    key.display()
                ),
            },
        }
    }

    /// The push a site sync ends with, as a dry run from the site checkout.
    fn probe_site_push(&self) -> Probe {
        let dir = match self.host.site() {
            Ok(Some(dir)) if dir.join("firebase.json").is_file() => dir,
            Ok(_) => {
                return Probe::Blocked {
                    why: "not tried: no usable site checkout is named (release.site)".into(),
                };
            }
            Err(why) => return Probe::Unusable { why },
        };
        match self.host.site_push(&dir) {
            Ok(()) => Probe::Ok {
                now: format!(
                    "the origin of {} would take a push from this Mac's git (git push \
                     --dry-run; nothing was sent)",
                    dir.display()
                ),
            },
            Err(why) => Probe::Person {
                step: self.id.into(),
                how: format!(
                    "the origin of the site checkout {} would not take a push from this Mac's \
                     git: {why}",
                    dir.display()
                ),
            },
        }
    }

    /// One row over `repos`, each `(what it is, OWNER/REPO)`: a repository whose routed
    /// identity may not push is a gap, one GitHub could not be asked about is no verdict.
    fn probe_pushes(&self, repos: &[(String, String)]) -> Probe {
        let (mut denied, mut unasked) = (Vec::new(), Vec::new());
        for (what, slug) in repos {
            match self.host.can_push(slug) {
                Ok(()) => {}
                Err(PushGap::Denied(why)) => denied.push(format!("{what} {slug} ({why})")),
                Err(PushGap::Unasked(why)) => unasked.push(format!("{what} {slug} ({why})")),
            }
        }
        if !denied.is_empty() {
            Probe::Person {
                step: self.id.into(),
                how: format!(
                    "the identity agh routes a write to may not push to {}",
                    denied.join("; ")
                ),
            }
        } else if !unasked.is_empty() {
            Probe::Unusable {
                why: format!(
                    "GitHub could not be asked whether agh's routed identity may push to {}",
                    unasked.join("; ")
                ),
            }
        } else {
            let named: Vec<String> = repos
                .iter()
                .map(|(what, slug)| format!("{what} {slug}"))
                .collect();
            Probe::Ok {
                now: format!(
                    "agh's routed identity may push to {} (a read of each repository's \
                     permissions; nothing was written)",
                    named.join(", ")
                ),
            }
        }
    }

    /// The repositories a publish and a cut write through agh: [`ENGINE_REPO`]'s dev (the
    /// ledger's claim on main), staging (the engine's stage) and release (its promote).
    fn probe_writes(&self) -> Probe {
        let (shown, text) = match self.host.engine_registry() {
            Ok(read) => read,
            Err(why) => {
                return Probe::Unusable {
                    why: format!("cannot read the engine's registry: {why}"),
                };
            }
        };
        let addresses = match engine_addresses(&text) {
            Ok(addresses) => addresses,
            Err(why) => {
                return Probe::Unusable {
                    why: format!("{shown}: {why}"),
                };
            }
        };
        let mut repos = Vec::new();
        for (role, url) in addresses {
            let Some(slug) = address_slug(&url) else {
                return Probe::Unusable {
                    why: format!(
                        "{shown}: {ENGINE_REPO}'s {role} address {url} names no GitHub OWNER/REPO"
                    ),
                };
            };
            repos.push((format!("{ENGINE_REPO}'s {role}"), slug));
        }
        self.probe_pushes(&repos)
    }

    fn remedy(&self) -> Remedy {
        match self.kind {
            Kind::Base => Remedy::Init {
                only: Layer::Base.name().into(),
            },
            Kind::Develop => Remedy::Provision {
                args: "--release".into(),
            },
            Kind::Openssl => Remedy::None,
            Kind::Publication => Remedy::Command(
                "clone the publication engine at ~/publication (or name one: \
                 PUBLICATION_ENGINE), with its signed atpkg prerequisites installed"
                    .into(),
            ),
            Kind::SourceSigning => Remedy::Command(
                "build atpkg-keys with `targo --unverified build --profile gate -p atpkg-keys`, and \
                 install its signed verifier with `aterm pkg install openssl-signing`"
                    .into(),
            ),
            Kind::EngineReach => Remedy::Command(
                "give this machine what the registry's address needs (its SSH key and host \
                 alias), or route the address over HTTPS: git config --global --add \
                 url.https://github.com/.insteadOf <its prefix, e.g. git@github.com:>"
                    .into(),
            ),
            Kind::Site => Remedy::Command(
                "clone the site repo and name it: SITE_DIR, or publish/site.toml's dir".into(),
            ),
            Kind::SiteKey => Remedy::Command(
                "`aterm secrets need site-deployer-key` (the vault lands it where \
                 publish/site.toml's firebase_sa_key names it)"
                    .into(),
            ),
            Kind::SiteAccess => Remedy::Command(
                "grant the deployer's service account Firebase Hosting's update permission on \
                 the site, or put its current key where the deploy reads it; `targo \
                 --unverified ship site --check-access` asks Google again"
                    .into(),
            ),
            Kind::SitePush => Remedy::Command(format!(
                "give this Mac's git a credential that may push to the site checkout's origin; \
                 in the checkout, `git push --dry-run origin HEAD:{PUSH_PROBE}` asks again"
            )),
            Kind::Writes | Kind::Tap => Remedy::Command(
                "route a writer for the repository in this Mac's agh (`agh whoami --for \
                 OWNER/REPO` explains the route, `aterm secrets sync` brings the vault's \
                 tokens); `agh can OWNER/REPO push` asks again"
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
            Kind::Develop => "the pinned build, source-signing and publication-engine tools".into(),
            Kind::Openssl => "macOS's LibreSSL, which release signing uses".into(),
            Kind::Publication => "the publication engine".into(),
            Kind::SourceSigning => "source-attestation signing and independent verification".into(),
            Kind::EngineReach => "the engine's addresses for this repository".into(),
            Kind::Site => "the site checkout and its firebase.json".into(),
            Kind::SiteKey => "the site deployer's service-account key".into(),
            Kind::SiteAccess => "Google's grant to the site deployer's key".into(),
            Kind::SitePush => "the site checkout's push to its origin".into(),
            Kind::Writes => {
                "the routed identities' push to the repositories a publish writes".into()
            }
            Kind::Tap => "the routed identity's push to the Homebrew tap".into(),
        }
    }

    fn probe(&self) -> Probe {
        match self.kind {
            Kind::Base => self.probe_base(),
            Kind::Develop => self.probe_develop(),
            Kind::Openssl => self.probe_openssl(),
            Kind::Publication => self.probe_publication(),
            Kind::SourceSigning => match self.host.source_signing() {
                Ok(()) => Probe::Ok {
                    now: "registered ring, source signer and OpenSSL Ed25519 verification passed"
                        .into(),
                },
                // The step-1 refusal's own cause: a gap with its fix, as the release says it.
                Err(SigningGap::Ring(why)) => Probe::Change {
                    from: format!("source signing ring: {why}"),
                    to: "the complete canonical ring".into(),
                },
                Err(SigningGap::Unusable(why)) => Probe::Unusable { why },
            },
            Kind::EngineReach => self.probe_engine_reach(),
            Kind::Site => self.probe_site(),
            Kind::SiteKey => self.probe_site_key(),
            Kind::SiteAccess => self.probe_site_access(),
            Kind::SitePush => self.probe_site_push(),
            Kind::Writes => self.probe_writes(),
            Kind::Tap => self.probe_pushes(&[("the Homebrew tap".to_owned(), TAP_REPO.to_owned())]),
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
            (Kind::Publication, Probe::Change { .. }) => {
                Remedy::Command(crate::managed_prerequisites::REMEDY.into())
            }
            (Kind::SourceSigning, Probe::Change { .. }) => Remedy::Command(format!(
                "{} provision --id <this machine's roster id>",
                crate::publish::SHIP_COMMAND
            )),
            _ => self.remedy(),
        };
        Explain { why, remedy }
    }

    fn essential(&self) -> bool {
        true
    }
}

#[derive(Clone, Copy)]
enum Audit {
    All,
    BeforeIdentity,
    SourceSigning,
    /// What steps 7 to 10 of a release write with ([`distribution`]).
    Distribution,
}

impl Audit {
    fn includes(self, kind: Kind) -> bool {
        match self {
            Self::All => true,
            Self::BeforeIdentity => kind != Kind::SourceSigning,
            Self::SourceSigning => kind == Kind::SourceSigning,
            Self::Distribution => matches!(
                kind,
                Kind::Site
                    | Kind::SiteKey
                    | Kind::SiteAccess
                    | Kind::SitePush
                    | Kind::Writes
                    | Kind::Tap
            ),
        }
    }
}

fn items(host: &Arc<dyn ReleaseHost>, audit: Audit) -> Vec<Box<dyn Item>> {
    ITEMS
        .iter()
        .filter(|(_, kind)| audit.includes(*kind))
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
        Remedy::Person { how, .. } => how.clone(),
        Remedy::Network => "run it again once online".into(),
        Remedy::None => "nothing on this machine changes it".into(),
        Remedy::Repair
        | Remedy::Init { .. }
        | Remedy::Develop
        | Remedy::Provision { .. }
        | Remedy::Command(_) => remedy.command().unwrap_or_default(),
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
    audit_selected(host, Audit::All)
}

pub(crate) fn audit_before_identity(host: &Arc<dyn ReleaseHost>) -> Vec<(&'static str, Check)> {
    audit_selected(host, Audit::BeforeIdentity)
}

pub(crate) fn audit_source_signing(host: &Arc<dyn ReleaseHost>) -> Vec<(&'static str, Check)> {
    audit_selected(host, Audit::SourceSigning)
}

fn audit_selected(host: &Arc<dyn ReleaseHost>, audit: Audit) -> Vec<(&'static str, Check)> {
    let items = items(host, audit);
    let selected: Vec<_> = ITEMS
        .iter()
        .filter(|(_, kind)| audit.includes(*kind))
        .collect();
    match aterm_converge::run(&items, Mode::Plan, &mut aterm_converge::ReadOnly) {
        Ok(rows) => selected
            .into_iter()
            .map(|&(id, _)| {
                let check = rows.iter().find(|r| r.id == id).map_or_else(
                    || Check::CouldNotRun(format!("the engine reported no row for {id}")),
                    check_of,
                );
                (id, check)
            })
            .collect(),
        Err(refusal) => selected
            .into_iter()
            .map(|&(id, _)| (id, Check::CouldNotRun(refusal.to_string())))
            .collect(),
    }
}

/// What a provision does about the deployer key's need: the step to take, or the line that
/// says why the lab is not asked (`None` where the settings could not be read). A key missing
/// at the catalog path is asked of the lab, a locally valid key meets the need, and a key
/// named somewhere else, or named nowhere, is never asked for, since the vault delivers only
/// to its catalog path and the deploy reads only the path it names.
fn site_key_step(host: &Arc<dyn ReleaseHost>) -> Result<NeedStep, Option<String>> {
    let key = host.site_key().map_err(|_| None)?;
    match key.as_ref().map(|key| validate_site_key(key)) {
        Some(Ok(())) => return Ok(NeedStep::Satisfied),
        Some(Err(why))
            if key
                .as_ref()
                .is_some_and(|key| std::fs::symlink_metadata(key).is_ok()) =>
        {
            return Err(Some(format!(
                "the deployer's key is unusable: {why}; the need is not marked fulfilled"
            )));
        }
        _ => {}
    }
    match (&key, host.catalog_site_key()) {
        (Some(key), Some(catalog)) if same_missing_destination(key, &catalog) => {
            Ok(NeedStep::Raise)
        }
        (Some(key), Some(catalog)) => Err(Some(format!(
            "the deployer's key is named at {}, not at {}, where the vault delivers it: the lab \
             is not asked, so name {} in publish/site.toml's firebase_sa_key, or put the key \
             where it is named",
            key.display(),
            catalog.display(),
            catalog.display()
        ))),
        (None, _) => Err(Some(
            "no key is named for the deploy (FIREBASE_SA_KEY, or publish/site.toml's \
             firebase_sa_key), so a key the vault delivers would go unread: the lab is not asked"
                .to_owned(),
        )),
        (Some(_), None) => Err(Some(
            "this account's home gives the deployer's key no catalog path, so the vault cannot \
             deliver it here: the lab is not asked"
                .to_owned(),
        )),
    }
}

/// Move the deployer key's need after a provision's audit (not `--check`), as
/// [`site_key_step`] decides. The line to show, if any.
pub(crate) fn settle_site_key(host: &Arc<dyn ReleaseHost>) -> Option<String> {
    match site_key_step(host) {
        Ok(step) => match host.site_key_need(step) {
            Ok(line) => line,
            Err(why) => Some(format!("the need ledger: {why}")),
        },
        Err(line) => line,
    }
}

/// What a release's step 1 proves of the writes its publish, cut and after-cut steps make
/// from this Mac, before anything is built, as `(id, check)` in [`ITEMS`] order: the site
/// checkout, the deployer's key and Google's grant to it, the site checkout's push, and the
/// routed identities' push to the repositories and the tap. Each is a read or a dry run.
///
/// A deployer key missing where the vault delivers it is asked of the lab here, by the need
/// `aterm secrets need` raises: a real run raises and posts it, and its row says so with the
/// remedy; a dry run (`dry`) raises nothing and says a real run would. A real run whose key
/// Google accepted marks the need met.
pub(crate) fn distribution(host: &Arc<dyn ReleaseHost>, dry: bool) -> Vec<(&'static str, Check)> {
    let mut rows = audit_selected(host, Audit::Distribution);
    let (vault, line) = match site_key_step(host) {
        Ok(NeedStep::Raise) if dry => {
            let now = match host.site_key_need(NeedStep::Read) {
                Ok(Some(line)) => format!(" (the need now: {line})"),
                Ok(None) => " (no need is on file yet)".to_owned(),
                Err(why) => format!(" (the need ledger: {why})"),
            };
            (
                true,
                Some(format!("a real run asks the lab's vault for it{now}")),
            )
        }
        Ok(NeedStep::Raise) => {
            let asked = match host.site_key_need(NeedStep::Raise) {
                Ok(Some(line)) => format!("asked of the lab's vault now ({line})"),
                Ok(None) => "asked of the lab's vault now".to_owned(),
                Err(why) => format!("the lab's vault could not be asked (the need ledger: {why})"),
            };
            let posted = match host.post_site_key_need() {
                Ok(()) => String::new(),
                Err(why) => format!("; it is posted at this Mac's next pass ({why})"),
            };
            (true, Some(format!("{asked}{posted}")))
        }
        Ok(_) => {
            let accepted = rows
                .iter()
                .any(|(id, check)| *id == "release.site-access" && matches!(check, Check::Pass(_)));
            if !dry && accepted {
                // Best effort: the key works, whatever the ledger says of its need.
                let _ = host.site_key_need(NeedStep::Satisfied);
            }
            (false, None)
        }
        Err(line) => (false, line),
    };
    if let Some(line) = line
        && let Some((_, Check::Fail { what, fix })) =
            rows.iter_mut().find(|(id, _)| *id == SITE_KEY)
    {
        what.push_str("; ");
        what.push_str(&line);
        if vault {
            let at = host
                .catalog_site_key()
                .map(|path| format!(" to {}", path.display()))
                .unwrap_or_default();
            *fix = format!(
                "wait for the vault to deliver the key{at}: a Mac that holds it answers the \
                 request, and `aterm secrets need ls` shows where it stands"
            );
        }
    }
    rows
}

/// Parse the same bounded regular file as deployment, admitting only Google's endpoint.
/// This proves local signing material, not remote authorization.
fn validate_site_key(path: &Path) -> Result<(), String> {
    let key = crate::site::hosting::Key::read(path)?;
    crate::site::hosting::Origins::google()
        .admit(&key.token_uri)
        .map(|_| ())
        .map_err(|_| "the service-account token endpoint is not Google's admitted endpoint".into())
}

/// Resolve existing parents as the deployer would, without opening a key or treating a
/// dangling leaf symlink as a file the vault may replace. Missing parents are not guessed.
fn same_missing_destination(named: &Path, catalog: &Path) -> bool {
    let absent = |path| {
        std::fs::symlink_metadata(path)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    };
    if !absent(named) || !absent(catalog) {
        return false;
    }
    if named == catalog {
        return true;
    }
    let Some(name) = named.file_name() else {
        return false;
    };
    if catalog.file_name() != Some(name) {
        return false;
    }
    match (
        named.parent().and_then(|p| p.canonicalize().ok()),
        catalog.parent().and_then(|p| p.canonicalize().ok()),
    ) {
        (Some(named), Some(catalog)) => named == catalog,
        _ => false,
    }
}

/// This machine.
#[cfg(unix)]
pub(crate) struct System {
    repo: PathBuf,
    home: PathBuf,
    source_helper: Result<Option<PathBuf>, String>,
}

#[cfg(unix)]
impl System {
    /// This machine, for the checkout at `repo` and the account at `home`.
    pub(crate) fn new(repo: &Path, home: &Path) -> Self {
        Self {
            repo: repo.to_path_buf(),
            home: home.to_path_buf(),
            source_helper: Ok(None),
        }
    }

    pub(crate) fn with_source_helper(mut self, helper: Result<Option<PathBuf>, String>) -> Self {
        self.source_helper = helper;
        self
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
/// The engine's registry ([`REGISTRY`]) on the commit of the engine `var` names for the clone
/// at `repo` ([`aterm_verify::engine`]), an origin's main imported through this Mac's native
/// identity for it ([`crate::github_remote::Remote::refresh_main`]): where it was read, and its
/// text.
fn registry_on_commit(
    repo: &Path,
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<(String, String), String> {
    use aterm_verify::engine::{self, Commit};
    let run: engine::RunGit<'_> = &engine::bounded;
    let import = |at: &Path, _: &Path| crate::github_remote::Remote::origin(at)?.refresh_main();
    let commit = engine::named(repo, var)
        .and_then(|named| Commit::read(&named, run, &import))
        .map_err(|e| e.why)?;
    let text = commit.text(REGISTRY, run).map_err(|e| e.why)?;
    Ok((format!("{REGISTRY} on {}", commit.shown()), text))
}

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
            aterm_verify::provision::select(&aterm_verify::provision::Options {
                release: true,
                ..Default::default()
            })
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
        if let Some(named) = std::env::var_os("PUBLICATION_ENGINE").filter(|v| !v.is_empty()) {
            return PathBuf::from(named);
        }
        // What a release of this clone runs once it has made its own engine.
        let copy =
            aterm_verify::engine::engine_copy(&aterm_verify::engine::release_dir(&self.repo));
        if copy.join(".git").exists() {
            return copy;
        }
        self.home.join("publication")
    }

    fn engine_registry(&self) -> Result<(String, String), String> {
        let home = self.home.clone();
        registry_on_commit(&self.repo, &move |name: &str| match name {
            "HOME" => Some(home.clone().into_os_string()),
            _ => std::env::var_os(name),
        })
    }

    fn publication_bootstrap(&self) -> Result<(), Bootstrap> {
        crate::release_steps::engine_bootstrap(&self.repo, &self.publication())
    }

    fn source_signing(&self) -> Result<(), SigningGap> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| SigningGap::Unusable(format!("source signing clock: {e}")))?
            .as_secs();
        let now = i64::try_from(now).unwrap_or(i64::MAX);
        // The release's step 1 refuses on exactly this read, so the audit reads it first.
        crate::source_signing::Tools::authority(&self.home, now).map_err(SigningGap::Ring)?;
        let helper = self
            .source_helper
            .as_ref()
            .map_err(|why| SigningGap::Unusable(why.clone()))?;
        crate::source_signing::Tools::probe(&self.repo, &self.home, now, helper.as_deref())
            .map(|_| ())
            .map_err(SigningGap::Unusable)
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

    fn catalog_site_key(&self) -> Option<PathBuf> {
        aterm_secrets::need::Want::SiteDeployerKey.hold_path(&self.home)
    }

    fn site_key_need(&self, step: NeedStep) -> Result<Option<String>, String> {
        use aterm_secrets::need::{self, Purpose, Want};
        let paths = aterm_secrets::client::Paths::live().map_err(|e| e.to_string())?;
        let want = Want::SiteDeployerKey;
        let purpose = Purpose::ReleaseSiteKey;
        let found = match step {
            NeedStep::Raise => need::raise(&paths, want, purpose).map(Some),
            NeedStep::Satisfied => need::satisfied(&paths, &want, purpose),
            NeedStep::Read => need::peek(&paths).map(|needs| {
                needs
                    .into_iter()
                    .find(|need| need.want == want && need.purpose == purpose)
            }),
        };
        found
            .map(|need| need.map(|need| need.line))
            .map_err(|e| e.to_string())
    }

    fn reach(&self, url: &str) -> Result<(), String> {
        let mut cmd = aterm_uds::spawnfd::command("git");
        cmd.args(["ls-remote", url, "HEAD"])
            .env("GIT_TERMINAL_PROMPT", "0");
        let out = aterm_uds::deadline::output_within(&mut cmd, REACH_WITHIN)
            .map_err(|e| format!("git ls-remote {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map_or_else(
                || format!("git ls-remote exited {:?}", out.status.code()),
                str::to_string,
            ))
    }

    fn site_access(&self, key: &Path) -> Result<String, String> {
        crate::site::access_of(&self.repo, key)
    }

    fn site_push(&self, site: &Path) -> Result<(), String> {
        let refspec = format!("HEAD:{PUSH_PROBE}");
        let mut cmd = aterm_uds::spawnfd::command("git");
        cmd.arg("-C")
            .arg(site)
            .args(["push", "--dry-run", "--quiet", "origin", &refspec])
            .env("GIT_TERMINAL_PROMPT", "0");
        let out = aterm_uds::deadline::output_within(&mut cmd, REACH_WITHIN)
            .map_err(|e| format!("git push --dry-run {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map_or_else(
                || format!("git push --dry-run exited {:?}", out.status.code()),
                str::to_string,
            ))
    }

    fn can_push(&self, slug: &str) -> Result<(), PushGap> {
        crate::github_remote::push_right(slug)
            .map_err(PushGap::Unasked)?
            .map_err(PushGap::Denied)
    }

    fn post_site_key_need(&self) -> Result<(), String> {
        use aterm_secrets::need::{Purpose, Want};
        let aterm = self
            .installed_aterm()
            .ok_or("no installed aterm to run `aterm secrets need`")?;
        let mut cmd = aterm_uds::spawnfd::command(&aterm);
        cmd.args(["secrets", "need"])
            .arg(Want::SiteDeployerKey.key())
            .args(["--purpose", Purpose::ReleaseSiteKey.id()])
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null());
        let out = aterm_uds::deadline::output_within(&mut cmd, REACH_WITHIN)
            .map_err(|e| format!("aterm secrets need {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("aterm secrets need exited {:?}", out.status.code()))
        }
    }
}

/// How long one address's listing may take: a hang detector, not a latency budget.
#[cfg(unix)]
const REACH_WITHIN: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(all(test, unix))]
#[path = "layer_tests.rs"]
mod tests;
