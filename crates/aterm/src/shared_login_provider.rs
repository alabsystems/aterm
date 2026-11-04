// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed provider and credential handling for current root account decisions.

use super::*;
use aterm_agent::harness::cli::shared_provider::{
    ClaudeOauth, CodexOauth, Context, Execution, KimiOauth, LoginProof, MailboxInputs, Provider,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Claude,
    Codex,
    Kimi,
}

impl Kind {
    fn of(value: &aterm_json::Value, has_credential: bool) -> Result<Self, String> {
        let kind = match (value["provider"].as_str(), value["method"].as_str()) {
            (Some("anthropic"), Some("oauth")) => Self::Claude,
            (Some("openai" | "codex"), Some("oauth" | "browser" | "device")) => Self::Codex,
            (Some("kimi" | "moonshot"), Some("device" | "oauth")) => {
                kimi_region(value)?;
                Self::Kimi
            }
            _ => {
                return Err("shared account has no supported device-local provider adapter".into());
            }
        };
        if has_credential && requirement(value, "login-input-format")? != Some("mailbox-v1") {
            return Err("shared credential needs a supported login-input format; OAuth sessions cannot be copied".into());
        }
        Ok(kind)
    }
    fn cli(self) -> accounts::Cli {
        match self {
            Self::Claude => accounts::Cli::Claude,
            Self::Codex => accounts::Cli::Codex,
            Self::Kimi => accounts::Cli::Kimi,
        }
    }
}

pub(super) fn candidate(
    approved: std::sync::Arc<Approved>,
) -> Result<aterm_agent::harness::accounts_sync::shared_renewal::Candidate, String> {
    let row = approved.candidate()?;
    let prepared = approved.prepared()?;
    let kind = Kind::of(
        prepared.configuration().account(),
        prepared.credential().is_some(),
    )?;
    Ok(
        aterm_agent::harness::accounts_sync::shared_renewal::Candidate {
            row,
            cli: kind.cli(),
            authority: approved.clone(),
            provider: approved,
            ordinary_renewal: ordinary_renewal(&prepared),
        },
    )
}

pub(super) fn ordinary_renewal(prepared: &aterm_db::accounts::activations::Preparation) -> bool {
    prepared.credential().is_none() && claude_checkpoint(prepared)
}

pub(super) fn claude_checkpoint(prepared: &aterm_db::accounts::activations::Preparation) -> bool {
    Kind::of(
        prepared.configuration().account(),
        prepared.credential().is_some(),
    )
    .is_ok_and(|kind| kind == Kind::Claude)
}

impl Provider for Approved {
    fn execute_attempt(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &std::path::Path,
        browser: &cli::RenewBrowser,
    ) -> Execution {
        let prepared = match self.read(row).and_then(|_| self.prepared()) {
            Ok(prepared) => prepared,
            Err(why) => return Execution::NotStarted(why),
        };
        let kind = match Kind::of(
            prepared.configuration().account(),
            prepared.credential().is_some(),
        ) {
            Ok(kind) => kind,
            Err(why) => return Execution::NotStarted(why),
        };
        let stopped = || {
            self.read(row).is_err()
                || (context.host.automatic_sign_in()
                    && context.env.config.as_deref().is_none_or(|config| {
                        !aterm_agent::harness::accounts_sync::shared_preparation_enabled(config)
                    }))
        };
        if let Err(why) = prepare_shared_dependencies(context, &stopped) {
            return Execution::NotStarted(why);
        }
        match kind {
            Kind::Claude => {
                if let Err(why) = prepare_claude_stage(context, staging).and_then(|()| {
                    if stopped() {
                        Err("shared sign-in approval or local policy changed".into())
                    } else {
                        Ok(())
                    }
                }) {
                    return Execution::NotStarted(why);
                }
                match row
                    .email
                    .as_deref()
                    .ok_or_else(|| "missing sign-in email".to_owned())
                    .and_then(|email| inputs(&prepared, email))
                {
                    Ok(Some(inputs)) => ClaudeOauth
                        .execute_attempt_with_inputs(context, row, staging, browser, &inputs),
                    Ok(None) => ClaudeOauth.execute_attempt(context, row, staging, browser),
                    Err(why) => Execution::NotStarted(why),
                }
            }
            Kind::Codex => match row
                .email
                .as_deref()
                .ok_or_else(|| "missing sign-in email".to_owned())
                .and_then(|email| inputs(&prepared, email))
            {
                Ok(Some(inputs)) => {
                    CodexOauth.execute_attempt_with_inputs(context, row, staging, &inputs)
                }
                Ok(None) => CodexOauth.execute_attempt(context, row, staging, browser),
                Err(why) => Execution::NotStarted(why),
            },
            Kind::Kimi => match row
                .email
                .as_deref()
                .ok_or_else(|| "missing sign-in email".to_owned())
                .and_then(|email| inputs(&prepared, email))
            {
                Ok(inputs) => {
                    let mailbox = inputs.map(|inputs| inputs.mailbox());
                    KimiOauth.execute_attempt_with_mailbox(context, row, staging, mailbox.as_ref())
                }
                Err(why) => Execution::NotStarted(why),
            },
        }
    }

    fn execute(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &std::path::Path,
        browser: &cli::RenewBrowser,
    ) -> Result<(), String> {
        match self.execute_attempt(context, row, staging, browser) {
            Execution::Completed => Ok(()),
            Execution::NotStarted(why) | Execution::Unknown(why) => Err(why),
        }
    }
    fn observe(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &std::path::Path,
    ) -> Result<Option<Box<dyn LoginProof>>, String> {
        self.read(row)?;
        let prepared = self.prepared()?;
        match Kind::of(
            prepared.configuration().account(),
            prepared.credential().is_some(),
        )? {
            Kind::Claude => claude_observe(context, row, staging),
            Kind::Codex => CodexOauth.observe(context, row, staging),
            Kind::Kimi => KimiOauth.observe(context, row, staging),
        }
    }
}

fn prepare_shared_dependencies(
    context: &Context<'_>,
    stopped: &dyn Fn() -> bool,
) -> Result<(), String> {
    if context.host.native_credential_setup() {
        aterm_agent::harness::headless_fetch::ensure_headless_browser(
            context.env.home.as_deref(),
            &mut aterm_agent::harness::headless_fetch::Curl,
            stopped,
        )?;
    }
    if stopped() {
        return Err("shared sign-in approval or local policy changed".into());
    }
    Ok(())
}

fn prepare_claude_stage(context: &Context<'_>, staging: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if context.host.native_credential_setup() {
        return aterm_machines::port::claude_keychain::prepare_claude_stage(
            staging,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .map_err(|why| format!("native staged credential setup unavailable: {why}"));
    }
    let _ = (context, staging);
    Ok(())
}

fn claude_observe(
    context: &Context<'_>,
    row: &accounts::Row,
    staging: &std::path::Path,
) -> Result<Option<Box<dyn LoginProof>>, String> {
    #[cfg(target_os = "macos")]
    {
        claude_observe_with_reader(
            context,
            row,
            staging,
            std::sync::Arc::new(|stage, deadline| {
                aterm_machines::port::claude_keychain::read_claude_stage(stage, deadline)
                    .map(|bytes| {
                        bytes.map(|bytes| aterm_uds::secret::Secret::new(bytes.as_slice()))
                    })
                    .map_err(|why| format!("staged Keychain credential unavailable: {why}"))
            }),
        )
    }
    #[cfg(not(target_os = "macos"))]
    ClaudeOauth.observe(context, row, staging)
}

#[cfg(target_os = "macos")]
fn claude_observe_with_reader(
    context: &Context<'_>,
    row: &accounts::Row,
    staging: &std::path::Path,
    reader: std::sync::Arc<cli::shared_provider::KeychainReader>,
) -> Result<Option<Box<dyn LoginProof>>, String> {
    if !context.host.native_credential_setup() {
        return ClaudeOauth.observe(context, row, staging);
    }
    ClaudeOauth.observe_with_keychain(context, row, staging, reader)
}

impl Approved {
    pub(super) fn candidate(&self) -> Result<accounts::Row, String> {
        let prepared = self.prepared()?;
        let value = prepared.configuration().account();
        let email = value["email"]
            .as_str()
            .ok_or("shared account has no direct email")?;
        let identity_email = value
            .get("details")
            .and_then(|d| d.get("provider-identity"))
            .map(|detail| {
                if detail["kind"].as_str() != Some("email-ref") {
                    return Err("provider identity is not an email reference");
                }
                let key = detail["value"]
                    .as_str()
                    .ok_or("invalid provider identity reference")?;
                prepared
                    .configuration()
                    .get(
                        "email-addresses",
                        key.strip_prefix("email-addresses:").unwrap_or(key),
                    )
                    .and_then(|contact| contact["address"].as_str())
                    .map(str::to_owned)
                    .ok_or("provider identity is absent")
            })
            .transpose()?;
        let class = match requirement(value, "local-class")? {
            None | Some("dedicated") => accounts::Class::Preferred,
            Some("shared") => accounts::Class::Pool,
            Some(_) => return Err("unsupported required account class".into()),
        };
        let kind = Kind::of(value, prepared.credential().is_some())?;
        let row = accounts::Row {
            label: format!(
                "shared-{}",
                aterm_codec::hex::encode_lower(&self.account)[..40].to_owned()
            ),
            class,
            email: Some(email.to_owned()),
            identity_email,
            browser: requirement(value, "browser-profile")?.map(str::to_owned),
            vendor: accounts::Vendor {
                region: if kind == Kind::Kimi {
                    Some(kimi_region(value)?)
                } else {
                    None
                },
                flow: (kind == Kind::Codex).then_some(
                    if value["method"].as_str() == Some("device") {
                        aterm_agent::ai::connector::Flow::Device
                    } else {
                        aterm_agent::ai::connector::Flow::Browser
                    },
                ),
            },
            ..Default::default()
        };
        self.read(&row)?;
        Ok(row)
    }

    pub(super) fn prepared(&self) -> Result<aterm_db::accounts::activations::Preparation, String> {
        if client::config(&self.paths)
            .map_err(|e| e.to_string())?
            .as_ref()
            != Some(&self.expected)
        {
            return Err("configured vault changed during shared renewal".into());
        }
        let prepared = Activations::at(self.paths.clone())
            .prepare(self.account, &self.expected)
            .map_err(|e| e.to_string())?;
        let record = prepared.decision();
        if record.id() != self.decision {
            return Err("shared renewal decision is no longer current".into());
        }
        Ok(prepared)
    }

    pub(super) fn read(&self, row: &accounts::Row) -> Result<Grant, String> {
        let prepared = self.prepared()?;
        let record = prepared.decision();
        let configuration = prepared.configuration();
        let value = configuration.account();
        let kind = Kind::of(value, prepared.credential().is_some())?;
        if has_unsupported_requirements(
            value,
            &[
                "local-class",
                "browser-profile",
                "provider-account-id",
                "provider-user-id",
                "login-input-format",
                "region",
            ],
        ) {
            return Err("shared renewal contains an unsupported required detail".into());
        }
        let email = value["email"]
            .as_str()
            .filter(|s| accounts::email_refusal(s).is_none())
            .ok_or_else(|| "shared renewal requires a direct approved email".to_owned())?;
        let _ = inputs(&prepared, email)?;
        let identity = match value
            .get("details")
            .and_then(|d| d.get("provider-identity"))
        {
            None => email,
            Some(detail) => {
                if detail["kind"].as_str() != Some("email-ref") {
                    return Err("provider-identity must be an email-ref".into());
                }
                let key = detail["value"]
                    .as_str()
                    .ok_or_else(|| "invalid approved identity reference".to_owned())?;
                let key = key.strip_prefix("email-addresses:").unwrap_or(key);
                let absent =
                    || "approved provider identity is absent from its checked closure".to_owned();
                let contact = configuration
                    .get("email-addresses", key)
                    .ok_or_else(absent)?;
                let address = contact["address"]
                    .as_str()
                    .filter(|s| accounts::email_refusal(s).is_none())
                    .ok_or_else(absent)?;
                if has_unsupported_requirements(contact, &[]) {
                    return Err(
                        "shared renewal cannot enforce a required provider identity detail".into(),
                    );
                }
                address
            }
        };
        if !row
            .email
            .as_deref()
            .is_some_and(|e| accounts::email_matches(e, email))
            || !row
                .expected_identity_email()
                .is_some_and(|e| accounts::email_matches(e, identity))
        {
            return Err(
                "local row does not match the approved sign-in and provider identity".into(),
            );
        }
        let browser = requirement(value, "browser-profile")?;
        if kind == Kind::Codex {
            if browser.is_some() {
                return Err("shared Codex does not support a required browser profile".into());
            }
            let expected_flow = if value["method"].as_str() == Some("device") {
                aterm_agent::ai::connector::Flow::Device
            } else {
                aterm_agent::ai::connector::Flow::Browser
            };
            if row.flow(accounts::Cli::Codex) != Some(expected_flow) || row.vendor.region.is_some()
            {
                return Err("local Codex route differs from the approved preparation".into());
            }
        }
        if kind == Kind::Kimi {
            if row.vendor.region != Some(kimi_region(value)?)
                || row.vendor.flow.is_some()
                || browser.is_some()
            {
                return Err("local Kimi route differs from the approved preparation".into());
            }
        } else if requirement(value, "region")?.is_some() {
            return Err("provider does not support a region requirement".into());
        }
        if browser.is_some_and(|browser| browser.trim().is_empty()) {
            return Err("invalid required browser profile".into());
        }
        if requirement(value, "local-class")?.is_some_and(|class| class != row.class.legacy_word())
            || browser.is_some_and(|browser| Some(browser) != row.browser.as_deref())
        {
            return Err("local account policy differs from the approved configuration".into());
        }
        if (kind != Kind::Kimi && value.get("realm").is_some_and(|v| !v.is_null()))
            || value
                .get("auth")
                .is_some_and(|routes| routes.as_array().is_none_or(|routes| !routes.is_empty()))
        {
            return Err(
                "shared renewal does not support custom realms or additional authentication routes"
                    .into(),
            );
        }
        let principal = match (
            kind,
            requirement(value, "provider-account-id")?,
            requirement(value, "provider-user-id")?,
        ) {
            (Kind::Claude, account, None) => account
                .map(|value| {
                    aterm_agent::harness::profile::principal_id(value)
                        .ok_or_else(|| "invalid approved provider account id".to_owned())
                })
                .transpose()?,
            (Kind::Codex, None, None) => None,
            (Kind::Codex, Some(account), Some(user)) => Some(
                aterm_agent::harness::codex_app_login::shared::principal(account, user)
                    .ok_or("invalid approved Codex account and user identity")?,
            ),
            (Kind::Kimi, None, None) => None,
            (Kind::Kimi, Some(user), None) | (Kind::Kimi, None, Some(user)) => Some(
                KimiOauth::principal(kimi_region(value)?, user)
                    .ok_or("invalid Kimi user identity")?,
            ),
            _ => return Err("approved provider identity requirements are incomplete".into()),
        };
        let trust = record.trust();
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm/shared-login-network/1\0");
        hash.update(trust.network);
        hash.update(trust.authority);
        hash.update(trust.genesis);
        Ok(Grant {
            source: accounts::shared::Source {
                network: hash.finalize(),
                account: self.account,
            },
            decision: record.id(),
            generation: record.request().generation,
            configuration: prepared.configuration().digest(),
            identity: identity.into(),
            expected_principal: principal,
            required_browser_profile: browser.map(str::to_owned),
        })
    }
}

fn kimi_region(value: &aterm_json::Value) -> Result<aterm_agent::ai::connector::Region, String> {
    let declared = requirement(value, "region")?;
    let realm = value
        .get("realm")
        .filter(|v| !v.is_null())
        .map(|v| v.as_str().ok_or("invalid Kimi realm"))
        .transpose()?;
    if declared.zip(realm).is_some_and(|(a, b)| a != b) {
        return Err("Kimi region and realm disagree".into());
    }
    declared
        .or(realm)
        .and_then(aterm_agent::ai::connector::Region::parse)
        .ok_or_else(|| "Kimi requires an explicit global or mainland-cn region".into())
}

fn inputs(
    prepared: &aterm_db::accounts::activations::Preparation,
    target: &str,
) -> Result<Option<MailboxInputs>, String> {
    match (
        requirement(prepared.configuration().account(), "login-input-format")?,
        prepared.credential(),
    ) {
        (None, None) => Ok(None),
        (Some("mailbox-v1"), Some(entry)) => {
            MailboxInputs::decode(entry.value().as_slice(), target).map(Some)
        }
        _ => Err("approved login inputs are missing or use an unsupported format".into()),
    }
}

#[cfg(test)]
#[path = "shared_login_provider_tests.rs"]
mod tests;
