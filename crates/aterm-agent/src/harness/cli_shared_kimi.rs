// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fresh, device-local Kimi login, checked against its region's `/me` endpoint.

use super::*;
use crate::ai::connector::{Flow, Region};
use crate::harness::{headless_oauth, login_drive};
use aterm_http::{Client, ProxyMode, Trust};
use aterm_json::Value;
use aterm_uds::secret::Secret;

const REPLY_MAX: usize = 256 * 1024;
const PROVIDER: &str = "managed:kimi-code";

/// Kimi's native device flow, with a separate staged home on every machine.
pub struct KimiOauth;

impl KimiOauth {
    pub fn execute_attempt_with_mailbox(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        mailbox: Option<&headless_oauth::MailboxConfig>,
    ) -> Execution {
        let request = match request(context, row, staging) {
            Ok(request) => request,
            Err(why) => return Execution::NotStarted(why),
        };
        let result = if let Some(mailbox) = mailbox {
            let Some(home) = context.env.home.as_deref() else {
                return Execution::NotStarted("Kimi sign-in has no local home".into());
            };
            context
                .host
                .vendor_sign_in_with_mailbox(&request, home, mailbox.clone())
        } else {
            context.host.vendor_sign_in(&request)
        };
        match result.outcome {
            login_drive::Outcome::SignedIn { .. } => Execution::Completed,
            login_drive::Outcome::NotRun(why) => Execution::NotStarted(why),
            _ => Execution::Unknown("device-local Kimi sign-in did not complete".into()),
        }
    }
    /// Full region-scoped provider identity; never an abbreviated display key.
    pub fn principal(region: Region, user: &str) -> Option<[u8; 32]> {
        if user.is_empty() || user.len() > 1024 || user.chars().any(char::is_control) {
            return None;
        }
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm/kimi-provider-principal/1\0");
        for field in [region.as_str(), user] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        Some(hash.finalize())
    }
}

impl Provider for KimiOauth {
    fn execute(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        _browser: &RenewBrowser,
    ) -> Result<(), String> {
        self.execute_attempt_with_mailbox(context, row, staging, None)
            .into_result()
    }

    fn execute_attempt(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        _browser: &RenewBrowser,
    ) -> Execution {
        self.execute_attempt_with_mailbox(context, row, staging, None)
    }

    fn observe(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
    ) -> Result<Option<Box<dyn LoginProof>>, String> {
        let region = region(row)?;
        let email = row
            .expected_identity_email()
            .ok_or("Kimi approval has no identity")?;
        let now = context.host.now().max(context.env.now);
        verify_with(staging, region, email, now, |url, token| {
            let client = Client::new(
                Trust::PlatformVerifier,
                ProxyMode::Direct,
                AUTH_CHECK_BUDGET,
            );
            let reply = client
                .get(url)
                .header("Accept", "application/json")
                .secret_header("Authorization", {
                    let mut header = b"Bearer ".to_vec();
                    header.extend_from_slice(token.as_bytes());
                    Secret::from_vec(header)
                })
                .limit(REPLY_MAX)
                .backstop(AUTH_CHECK_BUDGET)
                .send(&[])
                .map_err(|_| "Kimi identity request did not complete".to_owned())?;
            Ok((reply.status(), reply.body().to_vec()))
        })
        .map(|proof| proof.map(|proof| Box::new(proof) as Box<dyn LoginProof>))
    }
}

fn region(row: &accounts::Row) -> Result<Region, String> {
    if row.vendor.flow.is_some() {
        return Err("Kimi native device flow takes no local flow override".into());
    }
    row.vendor
        .region
        .ok_or_else(|| "Kimi approval requires an account region".into())
}

fn request<'a>(
    context: &'a Context<'_>,
    row: &'a accounts::Row,
    staging: &'a Path,
) -> Result<login_drive::Request<'a>, String> {
    let region = region(row)?;
    if context.store.cli() != Cli::Kimi || row.browser.is_some() {
        return Err("unsupported shared Kimi local route".into());
    }
    let route = Cli::Kimi
        .accounts()
        .signin
        .as_ref()
        .and_then(|signin| signin.route(Some(Flow::Device)))
        .ok_or("Kimi device sign-in is unavailable")?;
    let hosts = route
        .hosts
        .of(Some(region))
        .ok_or("Kimi device sign-in has no approved hosts")?;
    Ok(login_drive::Request {
        cli: Cli::Kimi,
        route,
        row,
        staging,
        store: context.store,
        hosts,
    })
}

fn base(region: Region) -> &'static str {
    match region {
        Region::Global => "https://api.kimi.ai/coding/v1",
        Region::MainlandCn => "https://api.kimi.com/coding/v1",
    }
}

fn issuer(region: Region) -> &'static str {
    match region {
        Region::Global => "https://auth.kimi.ai",
        Region::MainlandCn => "https://auth.kimi.com",
    }
}

fn oauth_key(region: Region) -> String {
    if region == Region::MainlandCn {
        return "oauth/kimi-code".into();
    }
    // Kimi hashes the ordered compact JSON, including these camel-case keys.
    let input = format!(
        r#"{{"oauthHost":"{}","baseUrl":"{}"}}"#,
        issuer(region),
        base(region)
    );
    let digest = aterm_digest::Sha256::digest(input.as_bytes());
    format!(
        "oauth/kimi-code-env-{}",
        aterm_codec::hex::encode_lower(&digest[..8])
    )
}

fn credential_path(staging: &Path, region: Region) -> PathBuf {
    let key = oauth_key(region);
    staging
        .join("credentials")
        .join(format!("{}.json", &key[6..]))
}

struct FileEvidence {
    path: PathBuf,
    bytes: Secret,
    private: bool,
}

impl FileEvidence {
    fn read(path: PathBuf, private: bool) -> Result<Option<Self>, String> {
        Ok(super::read_staged_bytes(&path, private)?.map(|bytes| Self {
            path,
            bytes,
            private,
        }))
    }

    fn check_current(&self) -> Result<(), String> {
        let current = Self::read(self.path.clone(), self.private)?
            .ok_or("Kimi staged evidence disappeared after verification")?;
        if current.bytes.expose() != self.bytes.expose() {
            return Err("Kimi staged evidence changed after verification".into());
        }
        Ok(())
    }
}

struct KimiProof {
    credential: FileEvidence,
    config: FileEvidence,
    email: String,
    principal: [u8; 32],
}

impl LoginProof for KimiProof {
    fn identity(&self) -> &str {
        &self.email
    }
    fn principal(&self) -> [u8; 32] {
        self.principal
    }
    fn subscription(&self) -> Option<&str> {
        None
    }
    fn check_current(&self) -> Result<(), String> {
        self.credential.check_current()?;
        self.config.check_current()
    }
}

fn check_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    aterm_uds::private_file::check_private_dir(path)
        .map_err(|_| "Kimi staging directory is not private and owned")?;
    let meta =
        std::fs::symlink_metadata(path).map_err(|_| "Kimi staging directory is unavailable")?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("Kimi staging directory is not a real directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != aterm_uds::peer::our_uid() || meta.mode() & 0o077 != 0 {
            return Err("Kimi staging directory is not private and owned".into());
        }
    }
    Ok(())
}

fn check_config(bytes: &[u8], region: Region) -> Result<(), String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Kimi staged config is not UTF-8")?;
    let doc: aterm_toml::Value =
        aterm_toml::from_str(text).map_err(|_| "Kimi staged config is invalid")?;
    let provider = doc
        .get("providers")
        .and_then(|providers| providers.get(PROVIDER))
        .ok_or("Kimi staged config has no managed provider")?;
    let fields = provider
        .as_table()
        .ok_or("Kimi staged config has no managed provider")?;
    let word = |key| provider.get(key).and_then(aterm_toml::Value::as_str);
    let oauth = provider
        .get("oauth")
        .and_then(aterm_toml::Value::as_table)
        .ok_or("Kimi staged config has no file credential binding")?;
    let auth_word = |key| oauth.get(key).and_then(aterm_toml::Value::as_str);
    if fields
        .keys()
        .any(|key| !matches!(key.as_str(), "type" | "base_url" | "api_key" | "oauth"))
        || oauth
            .keys()
            .any(|key| !matches!(key.as_str(), "storage" | "key" | "oauth_host"))
        || word("type") != Some("kimi")
        || word("base_url") != Some(base(region))
        || provider
            .get("api_key")
            .is_some_and(|key| key.as_str() != Some(""))
        || auth_word("storage") != Some("file")
        || auth_word("key") != Some(oauth_key(region).as_str())
        || oauth
            .get("oauth_host")
            .is_some_and(|host| host.as_str() != Some(issuer(region)))
        || (region == Region::Global && auth_word("oauth_host") != Some(issuer(region)))
    {
        return Err("Kimi staged provider differs from the approved native route".into());
    }
    let model = doc
        .get("default_model")
        .and_then(aterm_toml::Value::as_str)
        .ok_or("Kimi staged config has no default model")?;
    if doc
        .get("models")
        .and_then(|models| models.get(model))
        .and_then(|model| model.get("provider"))
        .and_then(aterm_toml::Value::as_str)
        != Some(PROVIDER)
    {
        return Err("Kimi default model does not use the staged login".into());
    }
    Ok(())
}

fn verify_with(
    staging: &Path,
    region: Region,
    expected: &str,
    now: i64,
    request: impl FnOnce(&str, &str) -> Result<(u16, Vec<u8>), String>,
) -> Result<Option<KimiProof>, String> {
    check_directory(staging)?;
    let credential_dir = staging.join("credentials");
    if !credential_dir.exists() {
        return Ok(None);
    }
    check_directory(&credential_dir)?;
    let config = FileEvidence::read(staging.join("config.toml"), false)?;
    if let Some(config) = &config {
        check_config(config.bytes.expose(), region)?;
    }
    let Some(credential) = FileEvidence::read(credential_path(staging, region), true)? else {
        return Ok(None);
    };
    let config = config.ok_or("Kimi staged config is absent")?;
    let token: Value = aterm_json::from_slice(credential.bytes.expose())
        .map_err(|_| "Kimi staged credential is invalid")?;
    let access = (|| {
        let access = token["access_token"]
            .as_str()
            .filter(|token| !token.is_empty() && token.bytes().all(|b| (0x21..=0x7e).contains(&b)))
            .ok_or("Kimi staged credential has no valid bearer")?;
        if token["token_type"].as_str() != Some("Bearer")
            || token["expires_at"]
                .as_i64()
                .is_none_or(|expiry| expiry <= now)
        {
            return Err("Kimi staged credential is expired or unsupported");
        }
        Ok(Secret::new(access.as_bytes()))
    })();
    super::wipe_json(token);
    let access = access?;
    let access = std::str::from_utf8(access.expose()).map_err(|_| "invalid Kimi token text")?;
    let (status, body) = request(&format!("{}/me", base(region)), access)?;
    if status != 200 || body.len() > REPLY_MAX {
        return Err("Kimi did not accept the staged identity request".into());
    }
    let identity: Value =
        aterm_json::from_slice(&body).map_err(|_| "Kimi identity response is invalid")?;
    let email = identity["email"]
        .as_str()
        .filter(|email| accounts::email_matches(email, expected))
        .ok_or("Kimi identity does not prove the approved email")?;
    let principal = identity["user_id"]
        .as_str()
        .and_then(|user| KimiOauth::principal(region, user))
        .ok_or("Kimi identity has no full user id")?;
    let proof = KimiProof {
        credential,
        config,
        email: email.to_owned(),
        principal,
    };
    proof.check_current()?;
    Ok(Some(proof))
}

#[cfg(all(test, unix))]
#[path = "cli_shared_kimi_tests.rs"]
mod tests;
