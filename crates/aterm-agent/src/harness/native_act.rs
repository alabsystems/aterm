// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed native mutation plans. An operation never grants general terminal input.

use super::contract::Scope;
use std::collections::BTreeMap;

/// The additive native-operation protocol, separate from a supervisor claim's contract.
pub const CONTRACT: u32 = 1;
/// Maximum bytes of one native-operation request.
pub const MAX_REQUEST: usize = 64 * 1024;
/// Maximum bytes of a path retained by a native plan.
pub const MAX_PATH: usize = 4096;
/// Maximum bytes of the exact private host continuation retained by a plan.
pub const MAX_RESUME: usize = 16 * 1024;
/// Maximum arguments or environment entries in one retained native command.
pub const MAX_COMMAND_ITEMS: usize = 256;

/// The billing authority used when planning a deferred model launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ColdBilling {
    /// Keep the existing model without a billing move.
    Keep,
    /// A model move must preserve the captured standard profile.
    PreserveProfile,
    /// A model move relies on usage credits remaining enabled.
    UsageCredits,
}

/// Captured account intent; this never changes the immutable seed command.
#[derive(Clone, PartialEq, Eq)]
pub struct ColdIntent {
    pub login: Option<String>,
    pub home: Option<String>,
    pub auth_observed: bool,
    /// How the captured process authenticated ([`super::accounts::AuthRoute`]).
    pub route: super::accounts::AuthRoute,
    pub conversation: Option<String>,
    pub profile: Option<super::profile::Context>,
    pub mode: super::accounts::Mode,
    pub billing: ColdBilling,
}

impl std::fmt::Debug for ColdIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ColdIntent(redacted)")
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ColdIntentWire {
    login: Option<String>,
    home: Option<String>,
    auth_observed: bool,
    /// The route ([`legacy_route`]); absent from an intent v0.157.0 and older wrote, which
    /// carried `stored_login`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route: Option<super::accounts::AuthRoute>,
    /// The route as v0.157.0 and older wrote it, read where `route` is absent.
    /// Reproduced for receipt identity, replay and captured older readers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stored_login: Option<bool>,
    conversation: Option<String>,
    profile: Option<ColdProfileWire>,
    mode: String,
    billing: ColdBilling,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ColdProfileWire {
    plain: String,
    records: String,
    selection: super::profile::Selection,
    home: Option<String>,
    config: Option<String>,
    login: Option<String>,
}

/// A cold record's route: its `route`, or, from a record v0.157.0 and older wrote (an operation
/// in flight across the update that brought the token route), its `stored_login` (`true` a
/// stored login, `false` outside), as `ClaudeAuthRestore` reads the same field. Both, or neither,
/// is no record.
fn legacy_route(
    route: Option<super::accounts::AuthRoute>,
    stored_login: Option<bool>,
) -> Option<super::accounts::AuthRoute> {
    match (route, stored_login) {
        (Some(route), None) => Some(route),
        (None, Some(true)) => Some(super::accounts::AuthRoute::Stored),
        (None, Some(false)) => Some(super::accounts::AuthRoute::Outside),
        (Some(_), Some(_)) | (None, None) => None,
    }
}

fn bounded_cold_text(value: &str) -> bool {
    value.len() <= MAX_PATH && !value.contains(['\0', '\r', '\n'])
}

impl ColdIntent {
    pub fn encode(&self) -> Option<String> {
        self.encode_route(Some(self.route), None)
    }

    /// Encode the intent for the captured launcher, which can predate this host.
    /// Stored and outside routes have an exact common encoding. Vault tokens
    /// require the current format; older launchers cannot describe that route.
    #[must_use]
    pub fn encode_for_launcher(&self) -> Option<String> {
        self.encode_legacy().or_else(|| self.encode())
    }

    fn encode_legacy(&self) -> Option<String> {
        use super::accounts::AuthRoute;
        let stored = match self.route {
            AuthRoute::Stored => true,
            AuthRoute::Outside => false,
            AuthRoute::VaultToken { .. } => return None,
        };
        self.encode_route(None, Some(stored))
    }

    fn encode_route(
        &self,
        route: Option<super::accounts::AuthRoute>,
        stored_login: Option<bool>,
    ) -> Option<String> {
        let profile = match &self.profile {
            None => None,
            Some(p) => Some(ColdProfileWire {
                plain: p.plain.to_str()?.to_owned(),
                records: p.records.to_str()?.to_owned(),
                selection: p.selection,
                home: p.home.clone(),
                config: p.config.clone(),
                login: p.login.clone(),
            }),
        };
        let wire = ColdIntentWire {
            login: self.login.clone(),
            home: self.home.clone(),
            auth_observed: self.auth_observed,
            route,
            stored_login,
            conversation: self.conversation.clone(),
            profile,
            mode: self.mode.as_str().into(),
            billing: self.billing,
        };
        let text = aterm_json::to_string(&wire).ok()?;
        (text.len() <= MAX_RESUME && Self::parse(&text).as_ref() == Some(self)).then_some(text)
    }
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > MAX_RESUME {
            return None;
        }
        let w: ColdIntentWire = aterm_json::from_str(text).ok()?;
        for s in [&w.login, &w.home, &w.conversation].into_iter().flatten() {
            if !bounded_cold_text(s) {
                return None;
            }
        }
        let profile = match w.profile {
            None => None,
            Some(p) => {
                if ![&p.plain, &p.records]
                    .into_iter()
                    .all(|s| bounded_cold_text(s))
                    || [&p.home, &p.config, &p.login]
                        .into_iter()
                        .flatten()
                        .any(|s| !bounded_cold_text(s))
                {
                    return None;
                }
                Some(super::profile::Context {
                    plain: p.plain.into(),
                    records: p.records.into(),
                    selection: p.selection,
                    home: p.home,
                    config: p.config,
                    login: p.login,
                })
            }
        };
        Some(Self {
            login: w.login,
            home: w.home,
            auth_observed: w.auth_observed,
            route: legacy_route(w.route, w.stored_login)?,
            conversation: w.conversation,
            profile,
            mode: super::accounts::Mode::parse(&w.mode)?,
            billing: w.billing,
        })
    }
}

/// Policy facts derived by the exact owned helper; credential values never cross the wire.
///
/// It is read through a wire form of its own, which also reads an older helper's route.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub struct ColdEnvironmentFacts {
    pub login: Option<String>,
    pub default_home: bool,
    /// How the environment authenticates ([`super::accounts::ProcessLogin::of_env`]).
    pub route: super::accounts::AuthRoute,
    pub home: Option<String>,
    /// Actual CLAUDE_CONFIG_DIR; distinct from the Anthropic profile store.
    pub config: Option<String>,
    pub profile_configs: Option<String>,
    pub selection: super::profile::Selection,
    pub inherited_named: bool,
    pub cwd: String,
}
/// [`ColdEnvironmentFacts`] as read: its fields, with the route also read from the
/// `stored_login` a v0.157.0 or older helper wrote ([`legacy_route`]).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ColdEnvironmentFactsWire {
    login: Option<String>,
    default_home: bool,
    #[serde(default)]
    route: Option<super::accounts::AuthRoute>,
    #[serde(default)]
    stored_login: Option<bool>,
    home: Option<String>,
    config: Option<String>,
    profile_configs: Option<String>,
    selection: super::profile::Selection,
    inherited_named: bool,
    cwd: String,
}

impl ColdEnvironmentFactsWire {
    /// The facts, where they carry one route, as `route` or `stored_login`.
    fn facts(self) -> Option<ColdEnvironmentFacts> {
        Some(ColdEnvironmentFacts {
            login: self.login,
            default_home: self.default_home,
            route: legacy_route(self.route, self.stored_login)?,
            home: self.home,
            config: self.config,
            profile_configs: self.profile_configs,
            selection: self.selection,
            inherited_named: self.inherited_named,
            cwd: self.cwd,
        })
    }
}

impl std::fmt::Debug for ColdEnvironmentFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ColdEnvironmentFacts(redacted)")
    }
}
impl ColdEnvironmentFacts {
    pub fn encode(&self) -> Option<String> {
        let text = aterm_json::to_string(self).ok()?;
        (text.len() <= MAX_RESUME && Self::parse(&text).is_some()).then_some(text)
    }
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > MAX_RESUME {
            return None;
        }
        let v = aterm_json::from_str::<ColdEnvironmentFactsWire>(text)
            .ok()?
            .facts()?;
        if !std::path::Path::new(&v.cwd).is_absolute()
            || !bounded_cold_text(&v.cwd)
            || v.default_home != v.config.as_ref().is_none_or(|value| value.is_empty())
            || [&v.login, &v.home, &v.config, &v.profile_configs]
                .into_iter()
                .flatten()
                .any(|s| !bounded_cold_text(s))
        {
            return None;
        }
        Some(v)
    }
    pub fn observe(env: &[String], intent: &ColdIntent, cwd: &std::path::Path) -> Option<Self> {
        let mut names = std::collections::BTreeSet::new();
        for entry in env {
            let (name, value) = entry.split_once('=')?;
            if name.is_empty() || name.contains('\0') || value.contains('\0') || !names.insert(name)
            {
                return None;
            }
        }
        let cwd = cwd.to_str()?.to_owned();
        let mut effective = env.to_vec();
        let inherited_named = effective
            .iter()
            .filter_map(|s| s.split_once('='))
            .any(|(k, v)| k == super::accounts::LOGIN_ENV && !v.is_empty());
        if intent.auth_observed || intent.login.is_some() {
            effective.retain(|s| {
                s.split_once('=')
                    .is_none_or(|(k, _)| k != super::accounts::LOGIN_ENV)
            });
            effective.push(format!(
                "{}={}",
                super::accounts::LOGIN_ENV,
                intent.login.as_deref().unwrap_or_default()
            ));
        }
        if let Some(home) = &intent.home {
            effective.retain(|s| {
                s.split_once('=')
                    .is_none_or(|(k, _)| k != super::accounts::HOME_ENV)
            });
            effective.push(format!("{}={home}", super::accounts::HOME_ENV));
        }
        let who = super::accounts::ProcessLogin::of_env(
            effective.iter().filter_map(|s| s.split_once('=')),
        );
        let home = effective
            .iter()
            .filter_map(|s| s.split_once('='))
            .find(|(k, _)| *k == super::accounts::HOMEDIR_ENV)
            .map(|(_, v)| v.to_owned());
        let profile_configs = super::accounts::profile_configs_dir(
            effective.iter().filter_map(|s| s.split_once('=')),
        )
        .map(|p| p.to_str().map(str::to_owned))
        .map_or(Some(None), |v| v.map(Some))?;
        let selection =
            super::profile::Selection::of_env(effective.iter().filter_map(|s| s.split_once('=')));
        let result = Self {
            login: who.login,
            default_home: who.default_home,
            route: who.route,
            home,
            config: effective
                .iter()
                .filter_map(|s| s.split_once('='))
                .find(|(key, _)| *key == super::accounts::HOME_ENV)
                .map(|(_, value)| value.to_owned()),
            profile_configs,
            selection,
            inherited_named,
            cwd,
        };
        result.encode()?;
        Some(result)
    }
    /// Interpret the environment the immutable seed will actually execute under.
    /// Raw inherited exports remain local and are not changed by this observation.
    pub fn observe_for_command(
        raw: &[String],
        intent: &ColdIntent,
        seed: &RestartCommand,
        cwd: &std::path::Path,
    ) -> Option<Self> {
        let mut names = std::collections::BTreeSet::new();
        for entry in raw {
            let (key, value) = entry.split_once('=')?;
            if key.is_empty() || key.contains('\0') || value.contains('\0') || !names.insert(key) {
                return None;
            }
        }
        let inherited_named = raw
            .iter()
            .filter_map(|s| s.split_once('='))
            .any(|(key, value)| key == super::accounts::LOGIN_ENV && !value.is_empty());
        let mut effective = raw.to_vec();
        if let Some(unset) = &seed.unset {
            effective.retain(|entry| {
                entry
                    .split_once('=')
                    .is_none_or(|(key, _)| !unset.iter().any(|name| name == key))
            });
        }
        for (key, value) in &seed.env {
            if key == super::accounts::LOGIN_ENV
                && (intent.auth_observed || intent.login.is_some())
                && value != intent.login.as_deref().unwrap_or_default()
            {
                return None;
            }
            if key == super::accounts::HOME_ENV
                && intent.home.as_ref().is_some_and(|home| home != value)
            {
                return None;
            }
            effective.retain(|entry| entry.split_once('=').is_none_or(|(name, _)| name != key));
            effective.push(format!("{key}={value}"));
        }
        let mut facts = Self::observe(&effective, intent, cwd)?;
        facts.inherited_named = inherited_named;
        Some(facts)
    }
    pub fn process_login(&self) -> super::accounts::ProcessLogin {
        super::accounts::ProcessLogin {
            login: self.login.clone(),
            default_home: self.default_home,
            route: self.route,
        }
    }
}

/// A constructor's selected command, before shell quoting. Both the legacy line and the
/// one-shot launcher use this exact selection, including line-length/shim/tap fallbacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestartCommand {
    /// Exact expected executable identity.
    pub image: Image,
    /// The selected spelling used for argv[0] and the legacy line, before canonical identity.
    pub program: String,
    /// Arguments after argv[0], never parsed from shell syntax.
    pub argv: Vec<String>,
    /// The exact selected exports, in their original order.
    pub env: Vec<(String, String)>,
    /// Inherited variables explicitly removed by the selected command.
    pub unset: Option<Vec<String>>,
    /// The selected shell-local directory change.
    pub cwd: Option<String>,
    /// The existing managed shell hook sourced before the command.
    pub heal: Option<String>,
    /// The original shell's quoting/command dialect.
    pub dialect: super::upgrade::Dialect,
    /// Account identity bound to this exact command; absent legacy attribution grants nothing.
    pub account_intent: Option<Box<super::accounts_pool::ExecutionIntent>>,
    /// The image `image` execs into in the same process, where `image` is aterm's token wrapper
    /// (DESIGN-claude-setup-token §5.4: `aterm harness accounts token-run … -- <build>` execs the
    /// store build in its own pid), captured when the command was planned. `None`: the program
    /// runs as `image` itself.
    pub settles: Option<Image>,
}

impl RestartCommand {
    /// The image a launched child of this command settles on: [`Self::settles`], else
    /// [`Self::image`]. A host holds the child to this, never to a wrapper it has left.
    #[must_use]
    pub fn settled(&self) -> &Image {
        self.settles.as_ref().unwrap_or(&self.image)
    }

    /// Render the exact selected legacy representation, without another store-env fallback.
    #[must_use]
    pub fn legacy_line(&self) -> Option<String> {
        self.line_at(&self.program, &self.argv)
    }

    fn line_at(&self, path: &str, argv: &[String]) -> Option<String> {
        let env = self
            .env
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        let unset = self
            .unset
            .as_ref()
            .map(|names| names.iter().map(String::as_str).collect::<Vec<_>>());
        super::upgrade::relaunch_line_fixed(
            self.dialect,
            self.heal.as_deref().map(std::path::Path::new),
            self.cwd.as_deref(),
            &env,
            unset.as_deref(),
            std::path::Path::new(path),
            argv,
        )
        .ok()
    }

    /// Render a deferred launcher without applying planned account exports before observation.
    #[must_use]
    pub fn cold_launcher_line(
        &self,
        launcher: &Image,
        socket: &str,
        sid: &str,
        binding: &Binding,
        phase: ImagePhase,
    ) -> Option<String> {
        let mut seed = self.clone();
        seed.env.clear();
        seed.unset = None;
        seed.launcher_line(launcher, socket, sid, binding, phase)
    }

    /// Only a server-selected login may differ from the immutable seed.
    #[must_use]
    pub fn cold_resolution_agrees(&self, resolved: &Self) -> bool {
        let mut expected = self.clone();
        let mut actual = resolved.clone();
        let Some(intent) = resolved.account_intent.as_deref() else {
            return false;
        };
        if super::accounts_pool::ExecutionIntent::from_value(&intent.to_value()).as_ref()
            != Some(intent)
        {
            return false;
        }
        // Deferred seeds have no account authority; resolution supplies the canonical intent.
        // A seed already carrying authority cannot silently replace that retained intent.
        if expected.account_intent.is_none() {
            actual.account_intent = None;
        }

        expected
            .env
            .retain(|(k, _)| k != super::accounts::LOGIN_ENV);
        actual.env.retain(|(k, _)| k != super::accounts::LOGIN_ENV);
        // Preserve the optional-vector shape and every non-selector unset entry.
        if let Some(names) = &mut expected.unset {
            names.retain(|name| name != super::accounts::LOGIN_ENV);
        }
        if let Some(names) = &mut actual.unset {
            names.retain(|name| name != super::accounts::LOGIN_ENV);
        }
        let login_exports = resolved
            .env
            .iter()
            .filter(|(k, _)| k == super::accounts::LOGIN_ENV)
            .count();
        let login_unsets = resolved.unset.as_ref().map_or(0, |names| {
            names
                .iter()
                .filter(|name| name.as_str() == super::accounts::LOGIN_ENV)
                .count()
        });
        expected == actual
            && login_exports <= 1
            && login_unsets <= 1
            && (login_exports == 0 || login_unsets == 0)
    }

    /// The fixed physical helper command. Its argv carries only the original operation,
    /// phase and endpoint; the helper obtains its captured vendor command from that host.
    #[must_use]
    pub fn launcher_line(
        &self,
        launcher: &Image,
        socket: &str,
        sid: &str,
        binding: &Binding,
        phase: ImagePhase,
    ) -> Option<String> {
        let argv = vec![
            "harness".into(),
            "native-launch".into(),
            "contract=1".into(),
            format!(
                "socket={}",
                aterm_codec::hex::encode_lower(socket.as_bytes())
            ),
            format!("session={sid}"),
            format!("id={}", binding.id),
            format!("cap={}", binding.capability),
            format!("phase={}", phase.word()),
        ];
        self.line_at(std::str::from_utf8(&launcher.path).ok()?, &argv)
    }

    /// Canonical bounded command storage. It contains data, never a replacement line supplied
    /// by a later Resume/Launch request.
    #[must_use]
    pub fn encode(&self) -> Option<String> {
        use std::fmt::Write;
        let bytes = self
            .program
            .len()
            .saturating_add(self.argv.iter().map(String::len).sum::<usize>())
            .saturating_add(
                self.env
                    .iter()
                    .map(|(name, value)| name.len().saturating_add(value.len()))
                    .sum::<usize>(),
            )
            .saturating_add(
                self.unset
                    .as_ref()
                    .map_or(0, |names| names.iter().map(String::len).sum::<usize>()),
            )
            .saturating_add(self.cwd.as_ref().map_or(0, String::len))
            .saturating_add(self.heal.as_ref().map_or(0, String::len));
        if bytes > MAX_RESUME {
            return None;
        }
        let line = self.legacy_line()?;
        if self.argv.len() > MAX_COMMAND_ITEMS
            || self.env.len() > MAX_COMMAND_ITEMS
            || self
                .unset
                .as_ref()
                .is_some_and(|names| names.len() > MAX_COMMAND_ITEMS)
            || line.len() > MAX_RESUME
            || line.contains(['\r', '\n', '\0'])
            || self.image.path.first() != Some(&b'/')
            || self.image.path.len() > MAX_PATH
            || self.image.path.contains(&0)
            || !std::path::Path::new(&self.program).is_absolute()
        {
            return None;
        }
        if self
            .settles
            .as_ref()
            .is_some_and(|image| image.path.first() != Some(&b'/') || image.path.contains(&0))
        {
            return None;
        }
        let mut out = String::new();
        encode_image(&mut out, "exec", &self.image);
        if let Some(settles) = &self.settles {
            encode_image(&mut out, "settle", settles);
        }
        let json = |value: &str| aterm_codec::hex::encode_lower(value.as_bytes());
        let optional = |value: Option<&str>| value.map_or_else(|| "-".into(), json);
        write!(
            out,
            " program={} argv={} env={} unset={} cwd={} heal={} dialect={}",
            json(&self.program),
            json(&aterm_json::to_string(&self.argv).ok()?),
            json(&aterm_json::to_string(&self.env).ok()?),
            json(&aterm_json::to_string(&self.unset).ok()?),
            optional(self.cwd.as_deref()),
            optional(self.heal.as_deref()),
            self.dialect.hook_ext(),
        )
        .ok()?;
        if let Some(intent) = &self.account_intent {
            let value = intent.to_value();
            if super::accounts_pool::ExecutionIntent::from_value(&value).as_ref()
                != Some(intent.as_ref())
            {
                return None;
            }
            write!(out, " account={}", json(&value.to_string())).ok()?;
        }
        (out.len() <= MAX_REQUEST).then(|| out.trim_start().to_string())
    }

    /// Decode only a canonical captured command; duplicate/unknown fields and alternate JSON
    /// representations refuse instead of changing the retained command's identity.
    #[must_use]
    pub fn parse(encoded: &str) -> Option<Self> {
        if encoded.len() > MAX_REQUEST || encoded.contains(['\n', '\r', '\0']) {
            return None;
        }
        let mut words = BTreeMap::new();
        for word in encoded.split_ascii_whitespace() {
            let (key, value) = word.split_once('=')?;
            if words.insert(key, value).is_some() {
                return None;
            }
        }
        let image = read_image(&mut words, "exec")?;
        let settles = if words.contains_key("settle-path") {
            Some(read_image(&mut words, "settle")?)
        } else {
            None
        };
        let program = text_blob(take(&mut words, "program")?, MAX_PATH)?;
        let argv =
            aterm_json::from_str(&text_blob(take(&mut words, "argv")?, MAX_REQUEST)?).ok()?;
        let env = aterm_json::from_str(&text_blob(take(&mut words, "env")?, MAX_REQUEST)?).ok()?;
        let unset =
            aterm_json::from_str(&text_blob(take(&mut words, "unset")?, MAX_REQUEST)?).ok()?;
        let optional = |value| {
            if value == "-" {
                Some(None)
            } else {
                text_blob(value, MAX_PATH).map(Some)
            }
        };
        let cwd = optional(take(&mut words, "cwd")?)?;
        let heal = optional(take(&mut words, "heal")?)?;
        let dialect = super::upgrade::Dialect::from_exe_name(take(&mut words, "dialect")?)?;
        let account_intent = match words.remove("account") {
            None => None,
            Some(encoded) => {
                let value = aterm_json::from_str(&text_blob(encoded, MAX_RESUME)?).ok()?;
                Some(Box::new(super::accounts_pool::ExecutionIntent::from_value(
                    &value,
                )?))
            }
        };
        let command = Self {
            image,
            program,
            argv,
            env,
            unset,
            cwd,
            heal,
            dialect,
            account_intent,
            settles,
        };
        (words.is_empty() && command.encode().as_deref() == Some(encoded)).then_some(command)
    }
}

/// A mapped executable identity. Paths preserve Unix bytes and never become shell syntax.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// The kernel-named or immutable managed image path.
    pub path: Vec<u8>,
    /// Its filesystem device.
    pub device: u64,
    /// Its filesystem inode.
    pub inode: u64,
}

/// A process identity observed independently by the host before admission and completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    /// A canonical pid above one.
    pub pid: u32,
    /// Kernel birth: epoch microseconds on Darwin and Windows (its creation `FILETIME`), boot
    /// clock ticks on Linux.
    pub birth: u64,
    /// Its actual parent: on Windows the pid it was created by, 0 where a younger process holds
    /// that pid now.
    pub parent: u32,
    /// Its process group; 0 on Windows, which keeps none, so no leader check passes there.
    pub group: u32,
    /// Its kernel session (`getsid`); 0 on Windows, which keeps no terminal session, as
    /// `aterm_sysprobe`'s sweep reads it there.
    pub session: u32,
    /// Its mapped image.
    pub image: Image,
}

impl Image {
    /// Capture an executable file identity without making it shell syntax.
    #[must_use]
    pub fn at(path: &std::path::Path) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::MetadataExt;
            let path = std::fs::canonicalize(path).ok()?;
            let metadata = std::fs::metadata(&path).ok()?;
            (metadata.is_file() && metadata.mode() & 0o111 != 0).then(|| Self {
                path: path.as_os_str().as_bytes().to_vec(),
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(windows)]
        {
            windows_image(path)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            None
        }
    }
}

/// [`Image::at`] on Windows, which keeps no execute bit: the regular file `path` names, by its
/// canonical path (UTF-8, or none) and its volume serial and file index
/// ([`super::agent_image::FileId`]), which every path to it shares.
#[cfg(windows)]
fn windows_image(path: &std::path::Path) -> Option<Image> {
    let path = std::fs::canonicalize(path).ok()?;
    if !std::fs::metadata(&path).ok()?.is_file() {
        return None;
    }
    let id = super::agent_image::FileId::of(&path)?;
    Some(Image {
        path: path.to_str()?.as_bytes().to_vec(),
        device: id.dev,
        inode: id.ino,
    })
}

impl Process {
    /// Birth-bracket kernel process/group/session and actual mapped image facts. An unread
    /// process or a changed mapping refuses; a pathname's replacement never proves the old image.
    #[must_use]
    pub fn observe(pid: u32) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::MetadataExt;
            let signed = i32::try_from(pid).ok().filter(|p| *p > 1)?;
            let birth = aterm_uds::process::read_birth(signed)?;
            let parent = aterm_uds::process::read_process_parent(signed)?;
            let path = aterm_uds::process::read_process_path(signed)?;
            #[cfg(target_os = "linux")]
            let metadata = std::fs::metadata(format!("/proc/{pid}/exe")).ok()?;
            #[cfg(not(target_os = "linux"))]
            let metadata = std::fs::metadata(&path).ok()?;
            #[cfg(target_os = "macos")]
            if aterm_sysprobe::kproc::image_inode(pid)? != metadata.ino() {
                return None;
            }
            // SAFETY: positive pid, no pointers; syscall errors refuse rather than fabricate facts.
            let group = u32::try_from(unsafe { libc::getpgid(signed) }).ok()?;
            // SAFETY: same positive pid, independently birth-bracketed; errors refuse.
            let session = u32::try_from(unsafe { libc::getsid(signed) }).ok()?;
            if aterm_uds::process::read_birth(signed)? != birth {
                return None;
            }
            Some(Self {
                pid,
                birth: birth.0,
                parent,
                group,
                session,
                image: Image {
                    path: path.as_os_str().as_bytes().to_vec(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                },
            })
        }
        // One sweep of the kernel's table names the parent, its row tied to this process by the
        // creation time read before and after it, so a pid handed to a younger process between
        // the reads refuses. Windows lets no one write or delete a mapped image and names it by
        // its current path, so the file opened there is the one the process maps.
        #[cfg(windows)]
        {
            let signed = i32::try_from(pid).ok().filter(|p| *p > 1)?;
            let created = aterm_uds::process::read_creation_filetime(signed)?;
            let birth = aterm_uds::process::read_birth(signed)?;
            let row = aterm_sysprobe::processes()?
                .procs
                .into_iter()
                .find(|row| row.pid == pid && row.start == Some(created))?;
            let image = windows_image(&aterm_sysprobe::process_path(pid)?)?;
            if aterm_uds::process::read_creation_filetime(signed)? != created
                || aterm_uds::process::read_birth(signed)? != birth
            {
                return None;
            }
            Some(Self {
                pid,
                birth: birth.0,
                parent: row.ppid,
                group: 0,
                session: 0,
                image,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = pid;
            None
        }
    }
}

/// Mint a fresh id before preparing one move. Retries retain it; they never call this again.
/// # Errors
/// The operating system entropy source is unavailable.
pub fn operation_id() -> std::io::Result<String> {
    let mut bytes = [0; 16];
    aterm_uds::rand::fill(&mut bytes)?;
    Ok(aterm_codec::hex::encode_lower(&bytes))
}

/// Actual constructor kinds, discovered separately from protocol grammar and roster rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Constructor {
    /// An external supervisor's own-image constructor.
    OwnImage,
    /// The private built-in's retained signal/launcher/rollback constructor.
    PrivateRestart,
    /// A sealed built-in cold relaunch with owned-helper account resolution.
    ColdRestart,
    /// A fresh built-in Upgrade qualified by the host's settled current native binding.
    AskedUpgrade,
    /// A checked live Memory restart under the built-in's scoped authorization.
    AskedRelaunch,
}

impl Constructor {
    /// The canonical discovery order. A capability is not an action ticket.
    pub const ALL: [Self; 5] = [
        Self::OwnImage,
        Self::PrivateRestart,
        Self::ColdRestart,
        Self::AskedUpgrade,
        Self::AskedRelaunch,
    ];

    /// This constructor's strict wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::OwnImage => "own-image",
            Self::PrivateRestart => "private-restart",
            Self::ColdRestart => "cold-restart",
            Self::AskedUpgrade => "asked-upgrade",
            Self::AskedRelaunch => "asked-relaunch",
        }
    }
}

/// The complete permitted mutation, retained by the host before its first side effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativePlan {
    /// Replace the authenticated supervisor's own image, preserving its child and PTY.
    OwnImage {
        /// The same live vendor child before and after the move.
        child: Process,
        /// The exact immutable supervisor image the real exec constructor will enter.
        target: Image,
        /// The compiled version independently probed by the actual constructor.
        version: String,
    },
    /// A private built-in constructor's fixed signal and subsequent resume submission.
    /// External harnesses cannot submit this plan or obtain its terminal continuation.
    Restart {
        /// The exact process to retire, including its birth and original image.
        process: Process,
        /// A passive retained-death proof for crash entry, never an old mutation ticket.
        observation: Option<super::native_observe::ObservationBinding>,
        /// The foreground shell in which the fixed continuation may run.
        shell: Process,
        /// The exact aterm image that registers and execs the retained vendor command.
        launcher: Image,
        /// The same local host endpoint on which this operation was prepared.
        socket: String,
        /// The exact selected forward command, captured before the first signal.
        forward: Box<RestartCommand>,
        /// Deferred account policy evaluated only by the owned launcher.
        cold: Option<Box<ColdIntent>>,
        /// At most one old-image rollback, also captured before the first signal.
        rollback: Option<Box<RestartCommand>>,
    },
}

impl NativePlan {
    /// The actual constructor required before this plan can receive new admission.
    #[must_use]
    pub const fn constructor(&self) -> Constructor {
        match self {
            Self::OwnImage { .. } => Constructor::OwnImage,
            Self::Restart { cold: Some(_), .. } => Constructor::ColdRestart,
            Self::Restart { cold: None, .. } => Constructor::PrivateRestart,
        }
    }
}

/// A preparation's authority expectation. The server independently derives principal,
/// connection and kernel peer, and checks its immutable claim under the metadata lock.
/// An exact predecessor name is data: only the host can qualify a fresh asked operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskedUpgradeIntent {
    /// The completed predecessor in the same host epoch; never a mutation capability.
    pub predecessor: String,
}

/// A Memory restart request. Its purpose and ask are data, never mutation authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskedRelaunchIntent {
    /// Same-host positively settled predecessor.
    pub predecessor: String,
    /// Exact source-owned restart ask.
    pub ask: String,
}

/// Mutually exclusive typed succession purposes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AskedIntent {
    /// Fresh upgrade under existing settlement qualification.
    Upgrade(AskedUpgradeIntent),
    /// Fresh live restart, additionally requiring scoped checked-producer authorization.
    Relaunch(AskedRelaunchIntent),
}

impl AskedIntent {
    /// The exact passive predecessor name, not an authority token.
    #[must_use]
    pub fn predecessor(&self) -> &str {
        match self {
            Self::Upgrade(intent) => &intent.predecessor,
            Self::Relaunch(intent) => &intent.predecessor,
        }
    }
}

/// An explicit receipt protocol. Recognition grants no permission to write this format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptMode {
    /// Complete forward and rollback recipient history (ANG5).
    Recipients5,
}

/// A host's separate writer-protocol advertisement, never reader evidence or
/// permission to admit, signal or execute a particular operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipientWriterSupport {
    /// Complete-recipient admission is unavailable on this host.
    Unavailable,
    /// The native admission/Execute path durably records each possible recipient
    /// in the independently bound journal. A particular admission still requires
    /// current authority, identity and image checks.
    BoundRecipientsV1,
}

impl RecipientWriterSupport {
    /// Exact discovery token. This is deliberately distinct from a Prepare mode.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Unavailable => "none",
            Self::BoundRecipientsV1 => "bound-recipients-v1",
        }
    }
}

/// A preparation's complete data-only expectation; the host independently derives authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepare {
    /// Caller-generated idempotency key, exactly 32 lowercase hexadecimal digits.
    pub id: String,
    /// Fresh host-process epoch obtained from `native-act info`. An app crash/replacement
    /// cannot silently turn recovery of an old operation into a new admission.
    pub host_epoch: String,
    /// The host's authenticated journal domain, obtained before preparation.
    /// Older hosts reject this marker; unmarked recipes cannot use cross-epoch rejection.
    pub rejection_domain: Option<String>,
    /// Explicit complete-recipient mode, included in the immutable recipe digest.
    /// `None` retains the existing issuer protocol and its exact wire spelling.
    pub receipt_mode: Option<ReceiptMode>,
    /// The actual holder; a private built-in always uses its own holder.
    pub holder: String,
    /// The exact claim revision under which the constructor begins.
    pub revision: u64,
    /// Private Restart's sequence from the same fresh status whose person policy passed.
    /// OwnImage carries none and acquires no quiet/person gate.
    pub expected_human_seq: Option<u64>,
    /// One absolute CLOCK_MONOTONIC deadline in microseconds, fixed before prepare. It bounds
    /// every effect: a Signal, a Resume, the first input write and an Execute delivery. A
    /// restart's close ([`Self::close_us`]), and an own-image move's deadline itself, bound every
    /// request that can change the outcome: Recover, Precommit, Commit and Reconcile. Polling,
    /// exec, recovery and rollback never extend either.
    pub deadline_us: u64,
    /// Explicit native semantic duty, Upgrade or Relaunch only.
    pub duty: Scope,
    /// A typed request for fresh asked admission, distinct from cold/crash continuation.
    pub asked: Option<AskedIntent>,
    /// The complete planned mutation.
    pub plan: NativePlan,
    /// The machine and boot of the host this prepare is for, as `boot=`. A host admits a
    /// stamped prepare only when the stamp is its own current machine and boot.
    pub boot: Option<BootStamp>,
    /// Custody this prepare is for, as `custody=<64 lowercase hex>`. Legacy mode names
    /// the issuer journal's established genesis (`native_receipts::custody`); complete
    /// mode names its independently enrolled recipient journal. Admission must check
    /// the stamp against the corresponding current custody.
    pub custody: Option<String>,
}

/// The machine and boot a prepare names, `boot=<host_id>:<boot_id>`: the host id of
/// [`aterm_uds::process::host_id`] (`<os>:<32 lowercase hex>`) and the boot id of
/// [`aterm_uds::process::boot_id`] (32 lowercase hex). It is data the host checks against its
/// own; it grants nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootStamp {
    /// This machine's stable identity, independent of a boot.
    pub host: String,
    /// The boot this machine is in.
    pub boot: String,
}

impl BootStamp {
    /// This process's machine and boot, or `None` when either does not read canonically.
    #[must_use]
    pub fn current() -> Option<Self> {
        let stamp = Self {
            host: aterm_uds::process::host_id()?,
            boot: aterm_uds::process::boot_id()?,
        };
        stamp.canonical().then_some(stamp)
    }

    pub(super) fn canonical(&self) -> bool {
        self.host.split_once(':').is_some_and(|(os, id)| {
            !os.is_empty()
                && os.bytes().all(|b| b.is_ascii_lowercase())
                && aterm_codec::hex::is_lower(id, 32)
        }) && aterm_codec::hex::is_lower(&self.boot, 32)
    }

    fn parse(word: &str) -> Option<Self> {
        let (host, boot) = word.rsplit_once(':')?;
        let stamp = Self {
            host: host.to_string(),
            boot: boot.to_string(),
        };
        stamp.canonical().then_some(stamp)
    }

    fn word(&self) -> String {
        format!("{}:{}", self.host, self.boot)
    }
}

/// What the issuer journal holds for one exact restart prepare, read without a lock and
/// without a capability (`native_receipts::observe_closure`). It grants nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Closure {
    /// No record names the prepare's operation id. This carries the custody digest of the
    /// journal that was read (the `custody=` spelling), or `None` when none is established.
    Absent(Option<String>),
    /// The record of this exact prepare (its digest, or its legacy cold digest).
    Exact(Recorded),
    /// Complete per-recipient history for this exact boot-stamped prepare.
    /// The reader must still establish the clock, machine and retirement facts.
    #[cfg(unix)]
    Recipients(super::native_receipts::RecipientHistory),
    /// A record names the id under another prepare, an account relation or a fence.
    Other,
}

/// The issuer state of an exact prepare's record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// Admitted, with no possible execution recorded.
    Prepared,
    /// Possible execution recorded, not yet closed.
    MayExecute,
    /// Positively closed with no execution grant.
    NoExec,
    /// Closed without a decision; `may_execute` is whether possible execution was recorded.
    Sealed { may_execute: bool },
}

/// How long past its deadline a restart may still reach its outcome, in microseconds: its close
/// is [`Prepare::deadline_us`] plus this. A grant delivered just before the deadline still has
/// this long to be observed and committed, longer than a constructor waits for its new agent
/// ([`super::Timings::PRODUCTION`]'s `held`, 90 s). Past the close no host epoch commits,
/// precommits, recovers or reconciles the restart; it reads [`Decision::Closed`] instead. An
/// own-image move has no grace: one still open at its deadline reads [`Decision::Unknown`]. It is
/// a protocol constant: the wire and the journal carry only the deadline, from which every host
/// and constructor derives the same close.
pub const COMPLETION_US: u64 = 120_000_000;

impl Prepare {
    /// The absolute CLOCK_MONOTONIC microsecond at which this operation closes if it is a
    /// restart: [`Self::deadline_us`] plus [`COMPLETION_US`], saturating. From it on nothing
    /// changes a restart's outcome in any host epoch ([`Decision::Closed`]). An own-image move
    /// has no close: one still open at its deadline reads [`Decision::Unknown`].
    #[must_use]
    pub const fn close_us(&self) -> u64 {
        self.deadline_us.saturating_add(COMPLETION_US)
    }

    /// Supported asked Restart shape only. This does not qualify its predecessor,
    /// current source, person sequence or producer, and grants no execution authority.
    #[must_use]
    pub fn asked_restart_shape(&self) -> bool {
        matches!(
            (&self.asked, self.duty),
            (Some(AskedIntent::Upgrade(_)), Scope::Upgrade)
                | (Some(AskedIntent::Relaunch(_)), Scope::Relaunch)
        ) && self
            .asked
            .as_ref()
            .is_some_and(|asked| aterm_codec::hex::is_lower(asked.predecessor(), 32))
            && matches!(
                self.plan,
                NativePlan::Restart {
                    observation: None,
                    cold: None,
                    ..
                }
            )
            && (!matches!(self.asked, Some(AskedIntent::Relaunch(_)))
                || matches!(&self.plan, NativePlan::Restart { forward, .. }
                    if forward.settles.is_none() && !matches!(forward.account_intent.as_deref(), Some(super::accounts_pool::ExecutionIntent::Token { .. }))))
    }

    /// Required constructor capability; this is not an admission qualification.
    #[must_use]
    pub const fn constructor(&self) -> Constructor {
        match &self.asked {
            Some(AskedIntent::Upgrade(_)) => Constructor::AskedUpgrade,
            Some(AskedIntent::Relaunch(_)) => Constructor::AskedRelaunch,
            None => self.plan.constructor(),
        }
    }
}

/// The exact original preparation and the supervisor image captured BEFORE sending it.
/// The original image is essential after bounded operation history has been reclaimed:
/// a process already in the target image cannot turn table absence into rollback proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelPrepare {
    /// The complete original request, with its unchanged id/epoch/plan/deadline.
    pub prepare: Prepare,
    /// The original kernel actor; its outer parent is diagnostic across reparenting.
    pub actor: Process,
}

/// Opaque host-issued capability identifying this operation, not a supervisor claim.
#[derive(Clone, PartialEq, Eq)]
pub struct Binding {
    /// The original idempotency key.
    pub id: String,
    /// Host-issued unpredictable capability, exactly 64 lowercase hexadecimal digits.
    pub capability: String,
}

impl std::fmt::Debug for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binding")
            .field("id", &self.id)
            .field("capability", &"redacted")
            .finish()
    }
}

/// Reproof does not commit a claim rebind or release native custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImagePhase {
    /// The actual constructor entered its proposed new image.
    Forward,
    /// The actual constructor returned to the original image.
    Rollback,
}

/// A final request's result. Dispatching commit makes transport ambiguity irreversible:
/// the constructor must query this same operation rather than assume rollback is allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The host observed the planned outcome and completed the native operation. Its separate
    /// claim receipt says whether authority survived; this never revives an invalidated claim.
    Committed,
    /// The host proved no commit occurred, retains the operation and explicitly permits rollback.
    ProvenNotCommittedRollbackSafe,
    /// Outcome is unresolved. Retain all possibly authoritative descriptors; begin no new move.
    Unknown,
    /// The failed plan is terminal: no future vendor effect can be admitted under its cap.
    /// Ungranted helpers may still exit later; this is never a successful image completion.
    Retired(Retirement),
    /// The operation reached its close ([`Prepare::close_us`]) uncommitted: no host epoch will
    /// commit, roll back or recover it. An exec grant may have been delivered and its process may
    /// still run; this never says whether the grant took effect, so a constructor learns what
    /// runs by observing it, never from this decision.
    Closed,
}

/// Why a failed native plan can release custody after its exact input prefix drains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retirement {
    /// No child exec was granted; terminal closure rejects every delayed Attach/Execute.
    NotGranted,
    /// Every granted child (or OwnImage actor/child) has a retained, retired kernel cohort.
    /// This names those exact admitted processes/groups, never arbitrary detached descendants.
    Exited,
}

impl Retirement {
    /// The exact terminal receipt word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::NotGranted => "no-exec-grant",
            Self::Exited => "cohort-exited",
        }
    }
}

/// The authority observation atomically recorded with native completion, never a new grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// The original scoped claim survived and its image version was rebound at this revision.
    Bound(u64),
    /// Release, expiry or replacement invalidated the claim. A later claim must follow the
    /// normal current-authority rules, after this native operation has completed.
    Lost,
    /// A private built-in continuation completed; it rebinds no external claim.
    Builtin,
}

/// The bounded wire requests the actual constructors may issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Read the host-process epoch and its native protocol without claiming or moving anything.
    Info,
    /// Discover enabled actual constructor kinds before preparing or claiming anything.
    Constructors,
    /// Read the host's durable rejection domain before creating a restart recipe.
    RejectionDomain,
    /// Discover atomic initial-prepare cancellation before creating a scoped claim.
    /// This probe leaves the frozen native protocol's Info reply unchanged.
    CancelSupport,
    /// Discover the host's complete-recipient writer protocol separately from
    /// frozen Info fields and signed bundle reader capabilities.
    RecipientWriterSupport,
    /// Admit a complete plan, atomically with its actor and revision.
    Prepare(Box<Prepare>),
    /// Seal this exact original OwnImage preparation against future admission. A fresh
    /// connection supplies new kernel proof; this request grants no action or claim.
    CancelPrepare(Box<CancelPrepare>),
    /// Reprove the new image while explicitly retaining rollback custody.
    Precommit(Binding, ImagePhase),
    /// The real constructor has finished all fallible local adoption and sealed mutation.
    Commit(Binding, ImagePhase, String),
    /// Recover the same idempotent operation after any ambiguous acknowledgement.
    Query(Binding),
    /// Seal an admitted operation before its first mutation, explicitly abandoning its plan.
    Abort(Binding),
    /// Reconstruct only this native-operation channel after its previous channel closed.
    /// This never binds a supervisor claim or admits a new operation.
    Recover(Binding),
    /// Execute only the fixed terminating signal of a private Restart plan.
    Signal(Binding),
    /// Submit only the fixed resume bytes retained in a private Restart plan.
    Resume(Binding, ImagePhase, String, u64),
    /// Register the exact one-shot aterm child and retained kernel exit witness before exec.
    Attach(Binding, ImagePhase),
    /// Supply derived policy facts from the exact attached helper.
    Resolve(Binding, ImagePhase, ColdEnvironmentFacts),
    /// Recheck current guards and the original deadline immediately before the fixed exec.
    Execute(Binding, ImagePhase),
    /// Record only the bound one-shot child's actual exec errno, never arbitrary outcome text.
    ExecFailed(Binding, ImagePhase, i32),
    /// Observe kernel extinction without granting a continuation or re-creating an operation.
    Reconcile(Binding),
}

fn canonical_number<T: std::str::FromStr + ToString>(text: &str) -> Option<T> {
    let value: T = text.parse().ok()?;
    (value.to_string() == text).then_some(value)
}

fn blob(text: &str, max: usize) -> Option<Vec<u8>> {
    if text.len() > max.checked_mul(2)? || !aterm_codec::hex::is_lower(text, text.len()) {
        return None;
    }
    aterm_codec::hex::decode_vec(text.as_bytes())
}

fn text_blob(text: &str, max: usize) -> Option<String> {
    String::from_utf8(blob(text, max)?).ok()
}

fn take<'a>(words: &mut BTreeMap<&str, &'a str>, key: &str) -> Option<&'a str> {
    words.remove(key)
}

fn read_image(words: &mut BTreeMap<&str, &str>, prefix: &str) -> Option<Image> {
    let path = blob(take(words, &format!("{prefix}-path"))?, MAX_PATH)?;
    if path.first() != Some(&b'/') || path.contains(&0) {
        return None;
    }
    let device = canonical_number(take(words, &format!("{prefix}-dev"))?)?;
    let inode = canonical_number(take(words, &format!("{prefix}-ino"))?)?;
    Some(Image {
        path,
        device,
        inode,
    })
}

fn read_process(words: &mut BTreeMap<&str, &str>, prefix: &str) -> Option<Process> {
    let pid = canonical_number::<u32>(take(words, &format!("{prefix}-pid"))?)?;
    let birth = canonical_number(take(words, &format!("{prefix}-birth"))?)?;
    let parent = canonical_number(take(words, &format!("{prefix}-parent"))?)?;
    let group = canonical_number(take(words, &format!("{prefix}-group"))?)?;
    let session = canonical_number(take(words, &format!("{prefix}-session"))?)?;
    (pid > 1).then_some(Process {
        pid,
        birth,
        parent,
        group,
        session,
        image: read_image(words, prefix)?,
    })
}

fn read_binding(words: &mut BTreeMap<&str, &str>) -> Option<Binding> {
    let id = take(words, "id")?;
    let capability = take(words, "cap")?;
    (aterm_codec::hex::is_lower(id, 32) && aterm_codec::hex::is_lower(capability, 64)).then(|| {
        Binding {
            id: id.to_string(),
            capability: capability.to_string(),
        }
    })
}

fn read_phase(words: &mut BTreeMap<&str, &str>) -> Option<ImagePhase> {
    match take(words, "phase")? {
        "forward" => Some(ImagePhase::Forward),
        "rollback" => Some(ImagePhase::Rollback),
        _ => None,
    }
}

impl Request {
    /// Own a complete data-only preparation without putting its retained recipes on every
    /// request's stack. This constructs words; only the host can admit the operation.
    #[must_use]
    pub fn prepare(prepare: Prepare) -> Self {
        Self::Prepare(Box::new(prepare))
    }

    /// Own the retained original words and old kernel actor, without sending or granting
    /// anything. Only a matching positive host receipt proves cancellation.
    #[must_use]
    pub fn cancel_prepare(prepare: Prepare, actor: Process) -> Self {
        Self::CancelPrepare(Box::new(CancelPrepare { prepare, actor }))
    }
    /// Decode one verb's tail. Unknown/duplicate fields, noncanonical numbers, invalid paths
    /// or capabilities, and non-native semantic duties refuse the complete request.
    #[must_use]
    pub fn parse(tail: &str) -> Option<Self> {
        if tail.len() > MAX_REQUEST || tail.contains(['\n', '\r', '\0']) {
            return None;
        }
        let mut split = tail.split_ascii_whitespace();
        let verb = split.next()?;
        let mut words = BTreeMap::new();
        for word in split {
            let (key, value) = word.split_once('=')?;
            if value.is_empty() || words.insert(key, value).is_some() {
                return None;
            }
        }
        let request = match verb {
            "info" => Self::Info,
            "constructors" => Self::Constructors,
            "rejection-domain" => Self::RejectionDomain,
            "cancel-support" => Self::CancelSupport,
            "recipient-writer-support" => Self::RecipientWriterSupport,
            "prepare" | "cancel-prepare" => {
                let id = take(&mut words, "id")?.to_string();
                if !aterm_codec::hex::is_lower(&id, 32) {
                    return None;
                }
                let host_epoch = take(&mut words, "epoch")?.to_string();
                if !aterm_codec::hex::is_lower(&host_epoch, 32) {
                    return None;
                }
                let rejection_domain = match words.remove("reject") {
                    None => None,
                    Some(value) if aterm_codec::hex::is_lower(value, 64) => Some(value.to_owned()),
                    Some(_) => return None,
                };
                let holder = text_blob(take(&mut words, "holder")?, 1024)?;
                let revision = canonical_number(take(&mut words, "revision")?)?;
                let expected_human_seq = match words.remove("human-seq") {
                    None => None,
                    Some(value) => Some(canonical_number(value)?),
                };
                let deadline_us = canonical_number(take(&mut words, "deadline-us")?)?;
                let duty = Scope::from_word(take(&mut words, "duty")?)?;
                if !matches!(duty, Scope::Upgrade | Scope::Relaunch) {
                    return None;
                }
                let plan = match take(&mut words, "plan")? {
                    "own-image" if duty == Scope::Upgrade => NativePlan::OwnImage {
                        child: read_process(&mut words, "child")?,
                        target: read_image(&mut words, "target")?,
                        version: text_blob(take(&mut words, "version")?, 256)?,
                    },
                    "restart" => NativePlan::Restart {
                        process: read_process(&mut words, "process")?,
                        observation: match take(&mut words, "observation")? {
                            "-" => None,
                            encoded => {
                                let super::native_observe::Request::Refresh(binding) =
                                    super::native_observe::Request::parse(&text_blob(
                                        encoded,
                                        MAX_REQUEST,
                                    )?)?
                                else {
                                    return None;
                                };
                                Some(binding)
                            }
                        },
                        shell: read_process(&mut words, "shell")?,
                        launcher: read_image(&mut words, "launcher")?,
                        socket: text_blob(take(&mut words, "socket")?, MAX_PATH)?,
                        cold: match words.remove("cold") {
                            None | Some("-") => None,
                            Some(text) => {
                                Some(Box::new(ColdIntent::parse(&text_blob(text, MAX_RESUME)?)?))
                            }
                        },
                        forward: Box::new(RestartCommand::parse(&text_blob(
                            take(&mut words, "forward")?,
                            MAX_REQUEST,
                        )?)?),
                        rollback: match take(&mut words, "rollback")? {
                            "-" => None,
                            encoded => Some(Box::new(RestartCommand::parse(&text_blob(
                                encoded,
                                MAX_REQUEST,
                            )?)?)),
                        },
                    },
                    _ => return None,
                };
                let asked = match words.remove("asked-predecessor") {
                    None => None,
                    Some(predecessor)
                        if aterm_codec::hex::is_lower(predecessor, 32)
                            && matches!(
                                &plan,
                                NativePlan::Restart {
                                    observation: None,
                                    cold: None,
                                    ..
                                }
                            ) =>
                    {
                        let predecessor = predecessor.to_owned();
                        match words.remove("asked-relaunch") {
                            None if duty == Scope::Upgrade => {
                                Some(AskedIntent::Upgrade(AskedUpgradeIntent { predecessor }))
                            }
                            Some(kind) if duty == Scope::Relaunch => {
                                let ask = text_blob(take(&mut words, "asked-ask")?, 256)?;
                                if ask.is_empty() || ask.contains(['\r', '\n', '\0']) {
                                    return None;
                                }
                                if kind != "memory" {
                                    return None;
                                }
                                Some(AskedIntent::Relaunch(AskedRelaunchIntent {
                                    predecessor,
                                    ask,
                                }))
                            }
                            _ => return None,
                        }
                    }
                    Some(_) => return None,
                };
                let boot = match words.remove("boot") {
                    None => None,
                    Some(word) => Some(BootStamp::parse(word)?),
                };
                let custody = match words.remove("custody") {
                    None => None,
                    Some(digest) if aterm_codec::hex::is_lower(digest, 64) => {
                        Some(digest.to_string())
                    }
                    Some(_) => return None,
                };
                let receipt_mode = match words.remove("receipts") {
                    None => None,
                    Some("recipients5") => Some(ReceiptMode::Recipients5),
                    Some(_) => return None,
                };
                if receipt_mode.is_some()
                    && (!matches!(plan, NativePlan::Restart { .. })
                        || boot.is_none()
                        || custody.is_none()
                        || rejection_domain.is_none())
                {
                    return None;
                }
                if holder.is_empty() {
                    return None;
                }
                if matches!(plan, NativePlan::Restart { .. }) != expected_human_seq.is_some() {
                    return None;
                }
                if let NativePlan::Restart { socket, .. } = &plan
                    && (!std::path::Path::new(socket).is_absolute()
                        || socket.contains(['\r', '\n', '\0']))
                {
                    return None;
                }
                let prepare = Prepare {
                    id,
                    host_epoch,
                    rejection_domain,
                    receipt_mode,
                    holder,
                    revision,
                    expected_human_seq,
                    deadline_us,
                    duty,
                    asked,
                    plan,
                    boot,
                    custody,
                };
                if verb == "cancel-prepare" {
                    if !matches!(prepare.plan, NativePlan::OwnImage { .. }) {
                        return None;
                    }
                    let actor = read_process(&mut words, "actor")?;
                    Self::cancel_prepare(prepare, actor)
                } else {
                    Self::prepare(prepare)
                }
            }
            "precommit" => Self::Precommit(read_binding(&mut words)?, read_phase(&mut words)?),
            "commit" => Self::Commit(
                read_binding(&mut words)?,
                read_phase(&mut words)?,
                text_blob(take(&mut words, "version")?, 256)?,
            ),
            "query" => Self::Query(read_binding(&mut words)?),
            "abort" => Self::Abort(read_binding(&mut words)?),
            "recover" => Self::Recover(read_binding(&mut words)?),
            "signal" => Self::Signal(read_binding(&mut words)?),
            "attach" => Self::Attach(read_binding(&mut words)?, read_phase(&mut words)?),
            "resolve" => Self::Resolve(
                read_binding(&mut words)?,
                read_phase(&mut words)?,
                ColdEnvironmentFacts::parse(&text_blob(take(&mut words, "facts")?, MAX_RESUME)?)?,
            ),
            "execute" => Self::Execute(read_binding(&mut words)?, read_phase(&mut words)?),
            "exec-failed" => {
                let binding = read_binding(&mut words)?;
                let phase = read_phase(&mut words)?;
                let errno = canonical_number::<i32>(take(&mut words, "errno")?)?;
                if errno <= 0 {
                    return None;
                }
                Self::ExecFailed(binding, phase, errno)
            }
            "reconcile" => Self::Reconcile(read_binding(&mut words)?),
            "resume" => {
                let binding = read_binding(&mut words)?;
                let phase = read_phase(&mut words)?;
                let generation = take(&mut words, "generation")?;
                let human_seq = canonical_number(take(&mut words, "human-seq")?)?;
                aterm_types::control_verbs::ScreenGen::parse(generation)?;
                Self::Resume(binding, phase, generation.to_string(), human_seq)
            }
            _ => return None,
        };
        words.is_empty().then_some(request)
    }

    /// Encode the complete request canonically. The decoder validates constructed values too;
    /// an invalid request is never partially sent to a control stream.
    #[must_use]
    pub fn encode(&self) -> Option<String> {
        self.encode_cold(false)
    }

    /// The exact older spelling of a supported in-flight cold preparation.
    /// New token routes, settled images and stamps had no representation in that protocol.
    pub(super) fn encode_legacy_cold(&self) -> Option<String> {
        let Self::Prepare(prepare) = self else {
            return None;
        };
        if prepare.receipt_mode.is_some() || prepare.boot.is_some() || prepare.custody.is_some() {
            return None;
        }
        let NativePlan::Restart {
            cold: Some(_),
            forward,
            rollback,
            ..
        } = &prepare.plan
        else {
            return None;
        };
        let old_command = |command: &RestartCommand| {
            command.settles.is_none()
                && !matches!(
                    command.account_intent.as_deref(),
                    Some(super::accounts_pool::ExecutionIntent::Token { .. })
                )
        };
        if !old_command(forward) || !rollback.as_deref().is_none_or(old_command) {
            return None;
        }
        self.encode_cold(true)
    }

    pub(super) fn matches_retained(&self, retained: &str) -> bool {
        matches!(self, Self::Prepare(_))
            && (self.encode().as_deref() == Some(retained)
                || self.encode_legacy_cold().as_deref() == Some(retained))
    }

    fn encode_cold(&self, legacy_cold: bool) -> Option<String> {
        use std::fmt::Write;
        let mut out = String::new();
        match self {
            Self::Info => out.push_str("info"),
            Self::Constructors => out.push_str("constructors"),
            Self::RejectionDomain => out.push_str("rejection-domain"),
            Self::CancelSupport => out.push_str("cancel-support"),
            Self::RecipientWriterSupport => out.push_str("recipient-writer-support"),
            Self::Prepare(_) | Self::CancelPrepare(_) => {
                let p = match self {
                    Self::Prepare(prepare) => &**prepare,
                    Self::CancelPrepare(cancel) => &cancel.prepare,
                    _ => unreachable!(),
                };
                let verb = if matches!(self, Self::CancelPrepare(_)) {
                    "cancel-prepare"
                } else {
                    "prepare"
                };
                write!(
                    out,
                    "{verb} id={} epoch={} holder={} revision={} deadline-us={} duty={}",
                    p.id,
                    p.host_epoch,
                    aterm_codec::hex::encode_lower(p.holder.as_bytes()),
                    p.revision,
                    p.deadline_us,
                    p.duty.word()
                )
                .ok()?;
                if let Some(domain) = &p.rejection_domain {
                    if !aterm_codec::hex::is_lower(domain, 64) {
                        return None;
                    }
                    write!(out, " reject={domain}").ok()?;
                }
                if let Some(ReceiptMode::Recipients5) = p.receipt_mode {
                    out.push_str(" receipts=recipients5");
                }
                if let Some(seq) = p.expected_human_seq {
                    write!(out, " human-seq={seq}").ok()?;
                }
                if let Some(asked) = &p.asked {
                    write!(out, " asked-predecessor={}", asked.predecessor()).ok()?;
                    if let AskedIntent::Relaunch(intent) = asked {
                        write!(
                            out,
                            " asked-relaunch=memory asked-ask={}",
                            aterm_codec::hex::encode_lower(intent.ask.as_bytes())
                        )
                        .ok()?;
                    }
                }
                match &p.plan {
                    NativePlan::OwnImage {
                        child,
                        target,
                        version,
                    } => {
                        out.push_str(" plan=own-image");
                        encode_process(&mut out, "child", child);
                        encode_image(&mut out, "target", target);
                        write!(
                            out,
                            " version={}",
                            aterm_codec::hex::encode_lower(version.as_bytes())
                        )
                        .ok()?;
                    }
                    NativePlan::Restart {
                        process,
                        observation,
                        shell,
                        launcher,
                        socket,
                        forward,
                        cold,
                        rollback,
                    } => {
                        out.push_str(" plan=restart");
                        encode_process(&mut out, "process", process);
                        encode_process(&mut out, "shell", shell);
                        encode_image(&mut out, "launcher", launcher);
                        write!(
                            out,
                            " observation={} socket={} forward={} rollback={}",
                            match observation {
                                None => "-".into(),
                                Some(binding) => aterm_codec::hex::encode_lower(
                                    super::native_observe::Request::Refresh(binding.clone())
                                        .encode()?
                                        .as_bytes()
                                ),
                            },
                            aterm_codec::hex::encode_lower(socket.as_bytes()),
                            aterm_codec::hex::encode_lower(forward.encode()?.as_bytes()),
                            match rollback {
                                None => "-".into(),
                                Some(command) =>
                                    aterm_codec::hex::encode_lower(command.encode()?.as_bytes()),
                            },
                        )
                        .ok()?;
                        if let Some(intent) = cold {
                            let encoded = if legacy_cold {
                                intent.encode_legacy()?
                            } else {
                                intent.encode()?
                            };
                            write!(
                                out,
                                " cold={}",
                                aterm_codec::hex::encode_lower(encoded.as_bytes())
                            )
                            .ok()?;
                        }
                    }
                }
                // Stamps are written only when present, so an unstamped prepare keeps the
                // exact spelling (and issuer digest) it had before stamps existed.
                if let Some(boot) = &p.boot {
                    write!(out, " boot={}", boot.word()).ok()?;
                }
                if let Some(custody) = &p.custody {
                    write!(out, " custody={custody}").ok()?;
                }
                if let Self::CancelPrepare(cancel) = self {
                    encode_process(&mut out, "actor", &cancel.actor);
                }
            }
            Self::Precommit(b, phase) => {
                write!(out, "precommit {} phase={}", b.words(), phase.word()).ok()?
            }
            Self::Commit(b, phase, version) => write!(
                out,
                "commit {} phase={} version={}",
                b.words(),
                phase.word(),
                aterm_codec::hex::encode_lower(version.as_bytes())
            )
            .ok()?,
            Self::Query(b) => write!(out, "query {}", b.words()).ok()?,
            Self::Abort(b) => write!(out, "abort {}", b.words()).ok()?,
            Self::Recover(b) => write!(out, "recover {}", b.words()).ok()?,
            Self::Signal(b) => write!(out, "signal {}", b.words()).ok()?,
            Self::Attach(b, phase) => {
                write!(out, "attach {} phase={}", b.words(), phase.word()).ok()?
            }
            Self::Resolve(b, phase, facts) => {
                write!(
                    out,
                    "resolve {} phase={} facts={}",
                    b.words(),
                    phase.word(),
                    aterm_codec::hex::encode_lower(facts.encode()?.as_bytes())
                )
                .ok()?;
            }
            Self::Execute(b, phase) => {
                write!(out, "execute {} phase={}", b.words(), phase.word()).ok()?
            }
            Self::ExecFailed(b, phase, errno) => write!(
                out,
                "exec-failed {} phase={} errno={errno}",
                b.words(),
                phase.word()
            )
            .ok()?,
            Self::Reconcile(b) => write!(out, "reconcile {}", b.words()).ok()?,
            Self::Resume(b, phase, generation, human_seq) => write!(
                out,
                "resume {} phase={} generation={generation} human-seq={human_seq}",
                b.words(),
                phase.word()
            )
            .ok()?,
        }
        (Self::parse(&out).as_ref() == Some(self)).then_some(out)
    }
}

fn encode_image(out: &mut String, prefix: &str, image: &Image) {
    use std::fmt::Write;
    let _ = write!(
        out,
        " {prefix}-path={} {prefix}-dev={} {prefix}-ino={}",
        aterm_codec::hex::encode_lower(&image.path),
        image.device,
        image.inode
    );
}

fn encode_process(out: &mut String, prefix: &str, process: &Process) {
    use std::fmt::Write;
    let _ = write!(
        out,
        " {prefix}-pid={} {prefix}-birth={} {prefix}-parent={} {prefix}-group={} {prefix}-session={}",
        process.pid, process.birth, process.parent, process.group, process.session
    );
    encode_image(out, prefix, &process.image);
}

impl Binding {
    fn words(&self) -> String {
        format!("id={} cap={}", self.id, self.capability)
    }
}

impl ImagePhase {
    /// The exact wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Rollback => "rollback",
        }
    }
}

/// A native-operation reply. No reply grants terminal input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Enabled actual constructors in canonical order. An empty set is explicit disabled
    /// support; this observation grants no plan, mutation, typing duty or supervisor claim.
    Constructors(Vec<Constructor>),
    /// Authenticated journal namespace, without any execution or signing capability.
    RejectionDomain(String),
    /// This host can atomically seal an exact initial OwnImage preparation. Check this
    /// separate discovery reply before claiming; native-act=1 alone is insufficient.
    CancelSupport,
    /// Writer protocol support only; this grants no operation or capability.
    RecipientWriterSupport(RecipientWriterSupport),
    /// No mutation committed and no future original Prepare can admit this exact plan.
    /// The echoed original request binds all fields, including its unchanged deadline.
    Cancelled(Box<CancelPrepare>),
    /// This host supports native-act=1, blocks app handoff through unresolved native work until
    /// each operation's close ([`Prepare::close_us`]), and uses absolute CLOCK_MONOTONIC
    /// microseconds. It carries no action permission. A host that spells its handoff `blocked`
    /// held a granted operation's hold with no bound; it is refused.
    Info {
        /// The fresh process epoch required by Prepare.
        epoch: String,
        /// Exact roster IDs accepted for OwnImage/Upgrade. Check this before any claim;
        /// an absent row must never be probed by creating an ordinary foreign claim.
        own_image: Vec<String>,
    },
    /// The same operation is still waiting for its already-started predecessors.
    Pending(Binding),
    /// The same retained operation may now begin its exact mutation.
    Admitted(Binding),
    /// Image proof succeeded; rollback custody and the original operation remain intact.
    Precommitted,
    /// Final or unresolved outcome of this same operation.
    Decision(Decision),
    /// Idempotent native completion plus the exact claim observation at that completion.
    Committed(ClaimOutcome),
    /// A private builtin completion also retained this positively observed new child for
    /// later crash admission. The frozen external OwnImage receipt is unchanged.
    RestartCommitted(Box<super::native_observe::SavedObservation>),
    /// Historical cold completion retains the executed account intent even without a new watch.
    ColdRestartCommitted {
        claim: ClaimOutcome,
        observation: Option<Box<super::native_observe::SavedObservation>>,
        intent: Box<super::accounts_pool::ExecutionIntent>,
    },
    /// The retained fixed signal was delivered, or its exact process had already exited.
    Signalled,
    /// The retained fixed continuation reached the kernel in full.
    Resumed(ImagePhase),
    /// The bound launcher's fixed command data, before permission for its first exec.
    Launch {
        command: RestartCommand,
        deadline_us: u64,
    },
    /// Request private derived facts before choosing a deferred command.
    NeedEnvironment {
        seed: RestartCommand,
        intent: Box<ColdIntent>,
        deadline_us: u64,
    },
    /// The server-selected deferred command, after account admission.
    ResolvedLaunch {
        command: RestartCommand,
        deadline_us: u64,
    },
    /// Permission for this already registered child's one fixed exec, under the old deadline.
    Execute { deadline_us: u64 },
    /// The bound child reported its fixed exec failure; kernel death still owns retirement.
    ExecFailed,
    /// The sealed attempt has no pending writes and accepted no bytes; the same plan may
    /// be tried at a fresh guarded prompt without minting another native operation.
    Unwritten,
    /// This exact phase's attached helper died, its group is gone and its input
    /// attempt settled, all without an exec grant. The same plan may retry.
    Unissued(ImagePhase),
    /// The host never admitted this restart operation id, and no later replay can: its
    /// deadline passed on this host epoch's clock with no table or issuer record,
    /// or the issuer durably rejected this exact never-admitted recipe. Only a
    /// constructor that saved this exact prepare without a binding
    /// may drop it; it records nothing completed.
    Unadmitted(String),
    /// No execution grant was or can be issued under this restart prepare: the host read its
    /// issuer record (or, for a stamped prepare, its absence from the stamped custody) after
    /// its clock passed the deadline, or across a reboot, or read a seal that never recorded
    /// possible execution. It claims nothing about the old operation's other resources.
    Lapsed(String),
    /// The operation `id` reached its close uncommitted ([`Decision::Closed`]): its old host
    /// sealed it so, or a later host epoch of the same boot read its possible execution after
    /// the close. It claims nothing about whether that execution happened.
    Closed(String),
}

/// The host's [`Reply::Unadmitted`] line for operation `id`, or `None` for a malformed id.
#[must_use]
pub fn unadmitted_reply(id: &str) -> Option<String> {
    aterm_codec::hex::is_lower(id, 32).then(|| format!("OK native-act state=unadmitted id={id}\n"))
}

/// The host's [`Reply::Lapsed`] line for operation `id`, or `None` for a malformed id.
#[must_use]
pub fn lapsed_reply(id: &str) -> Option<String> {
    aterm_codec::hex::is_lower(id, 32).then(|| format!("OK native-act state=lapsed id={id}\n"))
}

/// The host's [`Reply::Closed`] line for operation `id`, or `None` for a malformed id.
#[must_use]
pub fn closed_reply(id: &str) -> Option<String> {
    aterm_codec::hex::is_lower(id, 32).then(|| format!("OK native-act state=closed id={id}\n"))
}

impl Reply {
    /// Parse a complete reply. Unknown fields are refused rather than guessed into a grant.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let mut words = line.trim_end_matches(['\r', '\n']).split_ascii_whitespace();
        if words.next()? != "OK" || words.next()? != "native-act" {
            return None;
        }
        let state = words.next()?.strip_prefix("state=")?;
        let reply = match state {
            "constructors" => {
                let plans = words.next()?.strip_prefix("plans=")?;
                let enabled = if plans == "-" {
                    Vec::new()
                } else {
                    let enabled = Constructor::ALL
                        .into_iter()
                        .filter(|kind| plans.split(',').any(|word| word == kind.word()))
                        .collect::<Vec<_>>();
                    if enabled.is_empty()
                        || enabled
                            .iter()
                            .map(|kind| kind.word())
                            .collect::<Vec<_>>()
                            .join(",")
                            != plans
                    {
                        return None;
                    }
                    enabled
                };
                Self::Constructors(enabled)
            }
            "cancel-support" => {
                if words.next()? != "cancel-prepare=1" {
                    return None;
                }
                Self::CancelSupport
            }
            "recipient-writer-support" => {
                let support = match words.next()? {
                    "writer=none" => RecipientWriterSupport::Unavailable,
                    "writer=bound-recipients-v1" => RecipientWriterSupport::BoundRecipientsV1,
                    _ => return None,
                };
                Self::RecipientWriterSupport(support)
            }
            "rejection-domain" => {
                let domain = words.next()?.strip_prefix("domain=")?;
                if !aterm_codec::hex::is_lower(domain, 64) {
                    return None;
                }
                Self::RejectionDomain(domain.to_owned())
            }
            "cancelled" => {
                let encoded = words.next()?.strip_prefix("request=")?;
                let original = text_blob(encoded, MAX_REQUEST)?;
                let Request::CancelPrepare(cancel) = Request::parse(&original)? else {
                    return None;
                };
                if Request::CancelPrepare(cancel.clone()).encode()?.as_str() != original {
                    return None;
                }
                Self::Cancelled(cancel)
            }
            "info" => {
                let epoch = words.next()?.strip_prefix("epoch=")?;
                if !aterm_codec::hex::is_lower(epoch, 32)
                    || words.next()? != "native-act=1"
                    || words.next()? != "handoff=close"
                    || words.next()? != "clock=monotonic-us"
                {
                    return None;
                }
                let rows = words.next()?.strip_prefix("own-image=")?;
                let mut own_image = Vec::new();
                if rows != "-" {
                    for row in rows.split(',') {
                        let (id, scope) = row.split_once(':')?;
                        if scope != "upgrade"
                            || id.is_empty()
                            || id.len() > 64
                            || !id
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                            || own_image.iter().any(|old| old == id)
                        {
                            return None;
                        }
                        own_image.push(id.to_string());
                    }
                }
                Self::Info {
                    epoch: epoch.to_string(),
                    own_image,
                }
            }
            "admitted" | "pending" => {
                let mut fields = BTreeMap::new();
                for word in words.by_ref() {
                    let (key, value) = word.split_once('=')?;
                    if fields.insert(key, value).is_some() {
                        return None;
                    }
                }
                let binding = read_binding(&mut fields)?;
                if !fields.is_empty() {
                    return None;
                }
                if state == "admitted" {
                    Self::Admitted(binding)
                } else {
                    Self::Pending(binding)
                }
            }
            "precommitted" => Self::Precommitted,
            "committed" => {
                let claim = words.next()?.strip_prefix("claim=")?;
                let revision = words.next()?.strip_prefix("revision=")?;
                let outcome = match (claim, revision) {
                    ("bound", revision) => ClaimOutcome::Bound(canonical_number(revision)?),
                    ("lost", "-") => ClaimOutcome::Lost,
                    ("builtin", "-") => ClaimOutcome::Builtin,
                    _ => return None,
                };
                match words.next() {
                    None => Self::Committed(outcome),
                    Some(word) if outcome == ClaimOutcome::Builtin => {
                        if let Some(encoded) = word.strip_prefix("account-intent=") {
                            let text = text_blob(encoded, MAX_REQUEST)?;
                            let value: aterm_json::Value = aterm_json::from_str(&text).ok()?;
                            let intent = super::accounts_pool::ExecutionIntent::from_value(&value)?;
                            if aterm_json::to_string(&intent.to_value()).ok()? != text {
                                return None;
                            }
                            let observation = match words.next() {
                                None => None,
                                Some(word) => Some(Box::new(
                                    super::native_observe::SavedObservation::parse(&text_blob(
                                        word.strip_prefix("observation=")?,
                                        MAX_REQUEST,
                                    )?)?,
                                )),
                            };
                            Self::ColdRestartCommitted {
                                claim: outcome,
                                observation,
                                intent: Box::new(intent),
                            }
                        } else {
                            let saved = super::native_observe::SavedObservation::parse(
                                &text_blob(word.strip_prefix("observation=")?, MAX_REQUEST)?,
                            )?;
                            Self::RestartCommitted(Box::new(saved))
                        }
                    }
                    Some(_) => return None,
                }
            }
            "rollback-safe" => Self::Decision(Decision::ProvenNotCommittedRollbackSafe),
            "unknown" => Self::Decision(Decision::Unknown),
            "signalled" => Self::Signalled,
            "resumed" => Self::Resumed(match words.next()?.strip_prefix("phase=")? {
                "forward" => ImagePhase::Forward,
                "rollback" => ImagePhase::Rollback,
                _ => return None,
            }),
            "need-environment" => {
                let deadline_us = canonical_number(words.next()?.strip_prefix("deadline-us=")?)?;
                let seed = RestartCommand::parse(&text_blob(
                    words.next()?.strip_prefix("seed=")?,
                    MAX_REQUEST,
                )?)?;
                let intent = ColdIntent::parse(&text_blob(
                    words.next()?.strip_prefix("intent=")?,
                    MAX_RESUME,
                )?)?;
                Self::NeedEnvironment {
                    seed,
                    intent: Box::new(intent),
                    deadline_us,
                }
            }
            "resolved-launch" => {
                let deadline_us = canonical_number(words.next()?.strip_prefix("deadline-us=")?)?;
                let command = RestartCommand::parse(&text_blob(
                    words.next()?.strip_prefix("command=")?,
                    MAX_REQUEST,
                )?)?;
                Self::ResolvedLaunch {
                    command,
                    deadline_us,
                }
            }
            "launch" => {
                let deadline_us = canonical_number(words.next()?.strip_prefix("deadline-us=")?)?;
                let command = RestartCommand::parse(&text_blob(
                    words.next()?.strip_prefix("command=")?,
                    MAX_REQUEST,
                )?)?;
                Self::Launch {
                    command,
                    deadline_us,
                }
            }
            "execute" => Self::Execute {
                deadline_us: canonical_number(words.next()?.strip_prefix("deadline-us=")?)?,
            },
            "exec-failed" => Self::ExecFailed,
            "retired" => Self::Decision(Decision::Retired(
                match words.next()?.strip_prefix("reason=")? {
                    "no-exec-grant" => Retirement::NotGranted,
                    "cohort-exited" => Retirement::Exited,
                    _ => return None,
                },
            )),
            "unwritten" => Self::Unwritten,
            "unadmitted" | "lapsed" | "closed" => {
                let id = words.next()?.strip_prefix("id=")?;
                if !aterm_codec::hex::is_lower(id, 32) {
                    return None;
                }
                match state {
                    "lapsed" => Self::Lapsed(id.to_string()),
                    "closed" => Self::Closed(id.to_string()),
                    _ => Self::Unadmitted(id.to_string()),
                }
            }
            "unissued" => Self::Unissued(match words.next()?.strip_prefix("phase=")? {
                "forward" => ImagePhase::Forward,
                "rollback" => ImagePhase::Rollback,
                _ => return None,
            }),
            _ => return None,
        };
        words.next().is_none().then_some(reply)
    }
}

/// Execute one typed request on the constructor's retained authoritative control stream.
/// The caller owns the original deadline and retains the stream on transport ambiguity.
/// # Errors
/// Invalid canonical request/session, a refused or malformed reply, or transport failure.
pub fn exchange<S: std::io::Read + std::io::Write>(
    client: &mut crate::RelayClient<S>,
    sid: &str,
    request: &Request,
) -> std::io::Result<Reply> {
    let tail = request.encode().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid native plan")
    })?;
    exchange_encoded(client, sid, &tail)
}

/// Replay only a complete retained preparation with a supported exact encoding.
/// This preserves an older receiver's wire format without accepting raw commands.
pub(super) fn exchange_retained<S: std::io::Read + std::io::Write>(
    client: &mut crate::RelayClient<S>,
    sid: &str,
    request: &Request,
    retained: &str,
) -> std::io::Result<Reply> {
    if !request.matches_retained(retained) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid retained native preparation",
        ));
    }
    exchange_encoded(client, sid, retained)
}

fn exchange_encoded<S: std::io::Read + std::io::Write>(
    client: &mut crate::RelayClient<S>,
    sid: &str,
    tail: &str,
) -> std::io::Result<Reply> {
    if sid.is_empty()
        || !sid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid native session",
        ));
    }
    let line = client.request_line(&format!("@{sid} native-act {tail}"))?;
    Reply::parse(&line)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::PermissionDenied, line))
}

/// Dispatch final commit once. Every transport/refusal ambiguity after dispatch is Unknown;
/// recovery queries the original binding and never automatically rolls the image back.
pub fn commit<S: std::io::Read + std::io::Write>(
    client: &mut crate::RelayClient<S>,
    sid: &str,
    binding: &Binding,
    phase: ImagePhase,
    version: &str,
) -> Decision {
    match exchange(
        client,
        sid,
        &Request::Commit(binding.clone(), phase, version.to_string()),
    ) {
        Ok(
            Reply::Committed(_) | Reply::RestartCommitted(_) | Reply::ColdRestartCommitted { .. },
        ) => Decision::Committed,
        Ok(Reply::Decision(decision)) => decision,
        Ok(Reply::Closed(id)) if id == binding.id => Decision::Closed,
        Ok(_) | Err(_) => Decision::Unknown,
    }
}

/// The transaction clock shared across exec on one host. This is CLOCK_MONOTONIC (on Windows the
/// performance counter, one count since boot for every process), not wall time, an Instant
/// encoding, a process-relative timer or a claim's metadata clock.
#[must_use]
pub fn monotonic_us() -> Option<u64> {
    #[cfg(unix)]
    {
        let mut value = std::mem::MaybeUninit::<libc::timespec>::uninit();
        // SAFETY: clock_gettime initializes the writable timespec on success.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, value.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: the successful call initialized the complete value.
        let value = unsafe { value.assume_init() };
        u64::try_from(value.tv_sec)
            .ok()?
            .checked_mul(1_000_000)?
            .checked_add(u64::try_from(value.tv_nsec).ok()? / 1000)
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn QueryPerformanceCounter(count: *mut i64) -> i32;
            fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
        }
        let (mut count, mut frequency) = (0_i64, 0_i64);
        // SAFETY: the call writes one i64 through a pointer to a live local and reads nothing.
        if unsafe { QueryPerformanceFrequency(&raw mut frequency) } == 0 {
            return None;
        }
        // SAFETY: as above.
        if unsafe { QueryPerformanceCounter(&raw mut count) } == 0 {
            return None;
        }
        let frequency = u128::try_from(frequency).ok().filter(|hz| *hz > 0)?;
        u64::try_from(u128::try_from(count).ok()? * 1_000_000 / frequency).ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

// A restart plan carries a Unix image path, which only a Unix host observes ([`Image::at`],
// [`Process::observe`] name a Windows path on Windows); its wire refuses any other.
#[cfg(all(test, unix))]
#[path = "native_asked_relaunch_wire_tests.rs"]
mod asked_relaunch_wire_tests;
#[cfg(all(test, unix))]
#[path = "native_asked_upgrade_wire_tests.rs"]
mod asked_upgrade_wire_tests;

#[cfg(test)]
#[path = "native_act_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "native_act_cold_wire_tests.rs"]
mod cold_wire_tests;

#[cfg(test)]
#[path = "native_act_cold_completion_receipt_tests.rs"]
mod cold_completion_receipt_tests;
