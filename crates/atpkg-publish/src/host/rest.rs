// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`RestHost`]: GitHub REST API and git implementation of [`ReleaseHost`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
#[cfg(any(test, debug_assertions))]
use std::sync::Arc;
use std::time::Duration;

use aterm_json::Value;
use aterm_uds::spawnfd::Command;
#[cfg(any(test, debug_assertions))]
use aterm_update_core::github_rest::Spawn;
use aterm_update_core::github_rest::{Client, Method, Origins, RestError, Token};

use super::{HostError, Probe, RETRY_PAUSE, Release, ReleaseHost, Replace, Slug};

const PER_PAGE: usize = 100;

/// Suffix used when staging a replacement asset under [`Replace::Clobber`].
pub const STAGED: &str = ".atpkg-staged";

/// Environment variable from which git's credential helper reads the token.
pub const GIT_TOKEN_ENV: &str = "ATPKG_PUBLISH_GIT_TOKEN";

/// GitHub's REST API, as one identity.
#[derive(Clone)]
pub struct RestHost {
    /// The token file this identity reads; `None` reads anonymously and writes nothing.
    pub token_file: Option<PathBuf>,
    origins: Origins,
    /// Where git finds `{owner}/{repo}.git`: the web origin, or a test's server or directory
    /// of bare repositories.
    git_base: String,
    retry_pause: Duration,
    /// Tests: what starts curl (a recorder of each spawn).
    #[cfg(any(test, debug_assertions))]
    spawn: Option<Arc<dyn Spawn>>,
    /// Tests: variables set on each git (a scratch global config).
    #[cfg(any(test, debug_assertions))]
    git_env: Vec<(OsString, OsString)>,
}

impl std::fmt::Debug for RestHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestHost")
            .field("token_file", &self.token_file)
            .field("origins", &self.origins)
            .field("git_base", &self.git_base)
            .finish_non_exhaustive()
    }
}

impl RestHost {
    /// github.com, speaking as the token in `token_file`, or anonymously.
    #[must_use]
    pub fn new(token_file: Option<PathBuf>) -> Self {
        let origins = Origins::github();
        Self {
            token_file,
            git_base: origins.web.clone(),
            origins,
            retry_pause: RETRY_PAUSE,
            #[cfg(any(test, debug_assertions))]
            spawn: None,
            #[cfg(any(test, debug_assertions))]
            git_env: Vec::new(),
        }
    }

    /// Constructs a host for custom `origins` and `git_base` with zero retry pause.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn at(token_file: Option<PathBuf>, origins: Origins, git_base: String) -> Self {
        Self {
            token_file,
            origins,
            git_base,
            retry_pause: Duration::ZERO,
            spawn: None,
            git_env: Vec::new(),
        }
    }

    /// Tests: the same host, starting each curl through `spawn` (a recorder).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_spawn(self, spawn: Arc<dyn Spawn>) -> Self {
        Self {
            spawn: Some(spawn),
            ..self
        }
    }

    /// Sets extra environment variables on every spawned `git` command.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_git_env(self, vars: &[(&str, &std::ffi::OsStr)]) -> Self {
        Self {
            git_env: vars
                .iter()
                .map(|(k, v)| (OsString::from(k), v.to_os_string()))
                .collect(),
            ..self
        }
    }

    fn token(&self) -> Result<Option<Token>, HostError> {
        match &self.token_file {
            None => Ok(None),
            Some(file) => Token::from_file(file).map_err(HostError::Refused),
        }
    }

    fn client(&self, token: Option<Token>) -> Client {
        let client = Client::new(self.origins.clone(), token);
        #[cfg(any(test, debug_assertions))]
        let client = match &self.spawn {
            Some(spawn) => client.with_spawn(Arc::clone(spawn)),
            None => client,
        };
        client
    }

    fn reader(&self) -> Result<Client, HostError> {
        Ok(self.client(self.token()?))
    }

    fn writer(&self) -> Result<Client, HostError> {
        match self.token()? {
            Some(token) => Ok(self.client(Some(token))),
            None => Err(HostError::Refused(
                "no token file is configured for this owner (it was absent or empty when the \
                 run started), and a write never goes anonymously"
                    .to_string(),
            )),
        }
    }

    fn object(&self, client: &Client, slug: &Slug, tag: &str) -> Result<Value, HostError> {
        let by_tag = format!("/repos/{slug}/releases/tags/{}", segment(tag));
        match client.read(&by_tag) {
            Ok(object) => return Ok(object),
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(host_error(&e)),
        }
        let drafts: Vec<Value> = self
            .listing(client, slug)?
            .into_iter()
            .filter(|r| r["tag_name"].as_str() == Some(tag) && r["draft"].as_bool() == Some(true))
            .collect();
        match <[Value; 1]>::try_from(drafts) {
            Ok([draft]) => Ok(draft),
            Err(drafts) if drafts.is_empty() => Err(HostError::NotFound),
            Err(drafts) => Err(HostError::Refused(format!(
                "ambiguous: {} drafts have tag {tag} on {slug}",
                drafts.len()
            ))),
        }
    }

    fn listing(&self, client: &Client, slug: &Slug) -> Result<Vec<Value>, HostError> {
        client
            .pages(
                &format!("/repos/{slug}/releases?per_page={PER_PAGE}"),
                PER_PAGE,
            )
            .map_err(|e| match e {
                RestError::Status { status: 404, .. } => {
                    HostError::Unreachable(format!("{slug} is not visible to this identity ({e})"))
                }
                e => host_error(&e),
            })
    }

    fn id_of(object: &Value) -> Result<u64, HostError> {
        object["id"]
            .as_u64()
            .ok_or_else(|| HostError::Unreachable("GitHub answered a release with no id".into()))
    }

    fn publish(client: &Client, slug: &Slug, id: u64) -> Result<(), HostError> {
        let body = aterm_json::json!({"draft": false, "prerelease": true, "make_latest": "false"});
        client
            .send(
                Method::Patch,
                &format!("/repos/{slug}/releases/{id}"),
                Some(&body),
            )
            .and_then(|a| a.success())
            .map(|_| ())
            .map_err(|e| host_error(&e))
    }

    fn upload_one(
        client: &Client,
        slug: &Slug,
        id: u64,
        file: &Path,
        name: &str,
    ) -> Result<u64, HostError> {
        let url = client.uploads(&format!(
            "/repos/{slug}/releases/{id}/assets?name={}",
            segment(name)
        ));
        let asset = client
            .upload(&url, file)
            .and_then(|a| a.success())
            .and_then(|a| a.json())
            .map_err(|e| host_error(&e))?;
        asset["id"].as_u64().ok_or_else(|| {
            HostError::Unreachable(format!("GitHub answered the upload of {name} with no id"))
        })
    }

    fn delete_asset(client: &Client, slug: &Slug, asset: u64) -> Result<(), HostError> {
        let gone = client
            .send(
                Method::Delete,
                &format!("/repos/{slug}/releases/assets/{asset}"),
                None,
            )
            .and_then(|a| a.success());
        match gone {
            Ok(_) => Ok(()),
            Err(e) if e.is_not_found() => Ok(()),
            Err(e) => Err(host_error(&e)),
        }
    }

    fn drop_staged(
        client: &Client,
        slug: &Slug,
        object: &Value,
        name: &str,
    ) -> Result<(), HostError> {
        match asset_of(object, &format!("{name}{STAGED}")) {
            Ok((staged, _)) => Self::delete_asset(client, slug, staged),
            Err(HostError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn replace_one(
        client: &Client,
        slug: &Slug,
        object: &Value,
        id: u64,
        old: u64,
        file: &Path,
        name: &str,
    ) -> Result<(), HostError> {
        Self::drop_staged(client, slug, object, name)?;
        let staged = Self::upload_one(client, slug, id, file, &format!("{name}{STAGED}"))?;
        Self::delete_asset(client, slug, old)?;
        client
            .send(
                Method::Patch,
                &format!("/repos/{slug}/releases/assets/{staged}"),
                Some(&aterm_json::json!({ "name": name })),
            )
            .and_then(|a| a.success())
            .map(|_| ())
            .map_err(|e| host_error(&e))
    }

    fn git(&self, args: &[OsString]) -> Result<(), HostError> {
        let out = self
            .git_command(args)?
            .output()
            .map_err(|e| HostError::Unreachable(format!("spawn git: {e}")))?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(HostError::Unreachable(format!(
            "git: {}",
            stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("(no message)")
        )))
    }

    fn git_command(&self, args: &[OsString]) -> Result<Command, HostError> {
        let token = self.token()?;
        let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
        cmd.args(["-c", "credential.helper="])
            .env_remove(GIT_TOKEN_ENV)
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN");
        if let (Some(token), Some(origin)) = (token.as_ref(), credential_origin(&self.git_base)) {
            let text = std::str::from_utf8(token.expose())
                .map_err(|_| HostError::Refused("the token is not UTF-8".into()))?;
            cmd.arg("-c")
                .arg(format!("credential.{origin}.helper={}", git_helper()))
                .env(GIT_TOKEN_ENV, text);
        }
        #[cfg(any(test, debug_assertions))]
        for (name, value) in &self.git_env {
            cmd.env(name, value);
        }
        cmd.args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "");
        Ok(cmd)
    }

    fn git_url(&self, slug: &Slug) -> String {
        format!("{}/{slug}.git", self.git_base)
    }
}

fn credential_origin(base: &str) -> Option<&str> {
    let (scheme, host) = base.split_once("://")?;
    (matches!(scheme, "https" | "http")
        && !host.is_empty()
        && !host.contains(['/', '\n', '\0', '"']))
    .then_some(base)
}

fn git_helper() -> String {
    format!(
        "!f() {{ if test \"$1\" = get; then printf 'username=x-access-token\\npassword=%s\\n' \
         \"${GIT_TOKEN_ENV}\"; fi; }}; f"
    )
}

fn segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn file_name(file: &Path) -> Result<&str, HostError> {
    file.file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| HostError::Refused(format!("{} has no asset name", file.display())))
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && !name.ends_with(PART)
}

const PART: &str = ".atpkg-part";

fn host_error(e: &RestError) -> HostError {
    match e {
        RestError::Status { status: 404, .. } => HostError::NotFound,
        RestError::Status { message, .. } if message.contains("already_exists") => {
            HostError::Refused(e.to_string())
        }
        RestError::Local(why) => HostError::Refused(why.clone()),
        _ => HostError::Unreachable(e.to_string()),
    }
}

fn release_of(object: &Value) -> Result<Release, HostError> {
    let unreadable = || {
        HostError::Unreachable(format!(
            "GitHub answered a release this cannot read: {object}"
        ))
    };
    let tag = object["tag_name"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(unreadable)?;
    let draft = object["draft"].as_bool().ok_or_else(unreadable)?;
    let prerelease = object["prerelease"].as_bool().ok_or_else(unreadable)?;
    let assets = object["assets"]
        .as_array()
        .ok_or_else(unreadable)?
        .iter()
        .map(|a| {
            a["name"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(unreadable)
        })
        .collect::<Result<_, _>>()?;
    Ok(Release {
        tag: tag.to_string(),
        draft,
        prerelease,
        assets,
    })
}

fn asset_of(object: &Value, name: &str) -> Result<(u64, u64), HostError> {
    let named: Vec<&Value> = object["assets"]
        .as_array()
        .map(|assets| {
            assets
                .iter()
                .filter(|a| a["name"].as_str() == Some(name))
                .collect()
        })
        .unwrap_or_default();
    match named.as_slice() {
        [] => Err(HostError::NotFound),
        [asset] => match (asset["id"].as_u64(), asset["size"].as_u64()) {
            (Some(id), Some(size)) => Ok((id, size)),
            _ => Err(HostError::Unreachable(format!(
                "GitHub answered asset {name} with no id or size"
            ))),
        },
        several => Err(HostError::Refused(format!(
            "ambiguous: {} assets are named {name}",
            several.len()
        ))),
    }
}

impl ReleaseHost for RestHost {
    fn releases(&self, slug: &Slug) -> Result<Vec<Release>, HostError> {
        let client = self.reader()?;
        self.listing(&client, slug)?
            .iter()
            .map(release_of)
            .collect()
    }

    fn release(&self, slug: &Slug, tag: &str) -> Result<Release, HostError> {
        let client = self.reader()?;
        release_of(&self.object(&client, slug, tag)?)
    }

    fn download(
        &self,
        slug: &Slug,
        tag: &str,
        assets: &[&str],
        dir: &Path,
    ) -> Result<(), HostError> {
        if let Some(bad) = assets.iter().find(|a| !plain_name(a)) {
            return Err(HostError::Refused(format!(
                "{bad:?} is not a plain asset name; nothing was downloaded"
            )));
        }
        let client = self.reader()?;
        let object = self.object(&client, slug, tag)?;
        let wanted = assets
            .iter()
            .map(|name| asset_of(&object, name).map(|found| (*name, found)))
            .collect::<Result<Vec<_>, _>>()?;
        std::fs::create_dir_all(dir)
            .map_err(|e| HostError::Refused(format!("mkdir {}: {e}", dir.display())))?;
        for (name, (id, size)) in wanted {
            let part = dir.join(format!("{name}{PART}"));
            let path = format!("/repos/{slug}/releases/assets/{id}");
            client
                .download_asset(&path, &part, size.max(1))
                .map_err(|e| host_error(&e))?;
            let got = std::fs::metadata(&part)
                .map(|m| m.len())
                .unwrap_or(u64::MAX);
            if got != size {
                let _ = std::fs::remove_file(&part);
                return Err(HostError::Unreachable(format!(
                    "{slug} {tag} {name}: GitHub lists {size} bytes, the download holds {got}"
                )));
            }
            std::fs::rename(&part, dir.join(name)).map_err(|e| {
                HostError::Refused(format!("place {name} in {}: {e}", dir.display()))
            })?;
        }
        Ok(())
    }

    fn create(
        &self,
        slug: &Slug,
        tag: &str,
        title: &str,
        notes: &str,
        files: &[PathBuf],
    ) -> Result<(), HostError> {
        let client = self.writer()?;
        for file in files {
            file_name(file)?;
        }
        let body = aterm_json::json!({
            "tag_name": tag,
            "name": title,
            "body": notes,
            "draft": true,
            "prerelease": true,
            "make_latest": "false",
        });
        let made = client
            .send(
                Method::Post,
                &format!("/repos/{slug}/releases"),
                Some(&body),
            )
            .and_then(|a| a.success())
            .and_then(|a| a.json())
            .map_err(|e| host_error(&e))?;
        let id = Self::id_of(&made)?;
        for file in files {
            Self::upload_one(&client, slug, id, file, file_name(file)?)?;
        }
        Self::publish(&client, slug, id)
    }

    fn upload(
        &self,
        slug: &Slug,
        tag: &str,
        files: &[PathBuf],
        replace: Replace,
    ) -> Result<(), HostError> {
        let client = self.writer()?;
        for file in files {
            if file_name(file)?.ends_with(STAGED) {
                return Err(HostError::Refused(format!(
                    "{} ends in {STAGED}, the name a replace stages under",
                    file.display()
                )));
            }
        }
        let object = self.object(&client, slug, tag)?;
        let id = Self::id_of(&object)?;
        for file in files {
            let name = file_name(file)?;
            match (asset_of(&object, name), replace) {
                (Err(HostError::NotFound), Replace::Never) => {
                    Self::upload_one(&client, slug, id, file, name)?;
                }
                (Err(HostError::NotFound), Replace::Clobber) => {
                    Self::drop_staged(&client, slug, &object, name)?;
                    Self::upload_one(&client, slug, id, file, name)?;
                }
                (Ok(_), Replace::Never) => {
                    return Err(HostError::Refused(format!(
                        "{slug} {tag} already has an asset {name} (already_exists)"
                    )));
                }
                (Ok((old, _)), Replace::Clobber) => {
                    Self::replace_one(&client, slug, &object, id, old, file, name)?;
                }
                (Err(e), _) => return Err(e),
            }
        }
        Ok(())
    }

    fn publish_draft(&self, slug: &Slug, tag: &str) -> Result<(), HostError> {
        let client = self.writer()?;
        let object = self.object(&client, slug, tag)?;
        Self::publish(&client, slug, Self::id_of(&object)?)
    }

    fn probe_public(&self, slug: &Slug, tag: &str, asset: &str) -> Probe {
        match aterm_update_core::head_no_redirect(&self.public_url(slug, tag, asset)) {
            Ok(answer) => probe_of(answer.code),
            Err(e) => Probe::Unknown(format!("no answer from the download host: {e:?}")),
        }
    }

    fn fetch_public(&self, slug: &Slug, tag: &str, asset: &str, dest: &Path, cap: u64) -> Probe {
        if let Some(parent) = dest.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return Probe::Unknown(format!("mkdir {}: {e}", parent.display()));
        }
        match aterm_update_core::download_to(&self.public_url(slug, tag, asset), dest, cap) {
            Ok(()) => Probe::Present,
            Err(e) => {
                let _ = std::fs::remove_file(dest);
                if aterm_update_core::download_error_is_not_found(&e) {
                    Probe::Absent
                } else {
                    Probe::Unknown(e)
                }
            }
        }
    }

    fn clone_repo(&self, slug: &Slug, dest: &Path) -> Result<(), HostError> {
        let mut args: Vec<OsString> = vec!["clone".into(), "--quiet".into(), "--".into()];
        args.push(self.git_url(slug).into());
        args.push(dest.as_os_str().to_owned());
        self.git(&args)
    }

    fn retry_pause(&self) -> Duration {
        self.retry_pause
    }

    fn reads_anonymously(&self) -> bool {
        matches!(self.token(), Ok(None))
    }

    fn fetch_repo(&self, slug: &Slug, dir: &Path) -> Result<(), HostError> {
        let mut args: Vec<OsString> = vec!["-C".into(), dir.as_os_str().to_owned()];
        args.extend(
            ["fetch", "--quiet", "--tags", "--force"]
                .iter()
                .map(OsString::from),
        );
        args.push(self.git_url(slug).into());
        args.push("+refs/heads/*:refs/remotes/origin/*".into());
        self.git(&args).map_err(|e| match e {
            HostError::Unreachable(why) => {
                HostError::Unreachable(format!("fetch of {slug}: {why}"))
            }
            other => other,
        })
    }
}

impl RestHost {
    fn public_url(&self, slug: &Slug, tag: &str, asset: &str) -> String {
        format!(
            "{}/{}/{}/releases/download/{tag}/{asset}",
            self.origins.web, slug.owner, slug.repo
        )
    }
}

/// Maps an HTTP status code from the download host to a [`Probe`].
#[must_use]
pub fn probe_of(code: u16) -> Probe {
    match code {
        200 | 302 => Probe::Present,
        404 => Probe::Absent,
        other => Probe::Unknown(format!("the download host answered HTTP {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_one_segment_and_a_credential_is_scoped_to_an_http_origin() {
        assert_eq!(segment("pkg/v1 a&b"), "pkg%2Fv1%20a%26b");
        assert_eq!(segment("atpkg-index-7.tar.zst"), "atpkg-index-7.tar.zst");
        assert_eq!(
            credential_origin("https://github.com"),
            Some("https://github.com")
        );
        assert_eq!(
            credential_origin("http://127.0.0.1:8080"),
            Some("http://127.0.0.1:8080")
        );
        assert!(credential_origin("file:///tmp/remotes").is_none());
        assert!(credential_origin("https://github.com/x").is_none());
        for bad in ["", ".", "..", "../x", "a/b", "x.atpkg-part"] {
            assert!(!plain_name(bad), "{bad:?}");
        }
        assert!(plain_name("a.bin"));
    }

    fn text(s: &std::ffi::OsStr) -> String {
        s.to_string_lossy().into_owned()
    }

    fn shows(cmd: &Command, secret: &str) -> Vec<String> {
        let mut found: Vec<String> = cmd
            .get_args()
            .map(text)
            .filter(|a| a.contains(secret))
            .map(|a| format!("argv: {a}"))
            .collect();
        found.extend(
            cmd.get_envs()
                .filter(|(_, v)| v.is_some_and(|v| text(v).contains(secret)))
                .map(|(k, _)| format!("env: {}", text(k))),
        );
        found
    }

    #[test]
    fn gits_token_is_in_its_environment_alone_never_its_argv_or_a_file() {
        let dir = aterm_tempfile::TempDir::new().expect("scratch");
        let secret = "github_pat_fixture0123";
        let token_file = dir.path().join("token");
        std::fs::write(&token_file, format!("{secret}\n")).expect("the token file");
        let host = RestHost::at(
            Some(token_file),
            Origins::github(),
            "https://github.com".to_string(),
        );
        let cmd = host.git_command(&["fetch".into()]).expect("the command");
        assert_eq!(shows(&cmd, secret), [format!("env: {GIT_TOKEN_ENV}")]);
        let args: Vec<String> = cmd.get_args().map(text).collect();
        assert_eq!(
            args[..2],
            ["-c", "credential.helper="],
            "every helper reset first"
        );
        let helper = format!("credential.https://github.com.helper={}", git_helper());
        assert_eq!(
            args[2..4],
            ["-c".to_string(), helper],
            "then the one, scoped"
        );
        assert!(git_helper().contains(&format!("\"${GIT_TOKEN_ENV}\"")));
        for gone in ["GITHUB_TOKEN", "GH_TOKEN"] {
            assert!(
                cmd.get_envs().any(|(k, v)| k == gone && v.is_none()),
                "{gone} is removed"
            );
        }
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("scratch").count(),
            1,
            "no file but the token's own"
        );
        let mut planted = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
        planted.env("GIT_PASSWORD", secret);
        planted.arg(format!("https://x:{secret}@github.com/o/r.git"));
        assert_eq!(shows(&planted, secret).len(), 2);
        let local = RestHost::at(
            host.token_file.clone(),
            Origins::github(),
            "file:///r".into(),
        );
        let cmd = local.git_command(&["fetch".into()]).expect("the command");
        assert_eq!(shows(&cmd, secret), Vec::<String>::new());
        assert!(!cmd.get_args().any(|a| text(a).contains("helper=!")));
        assert!(
            cmd.get_envs()
                .any(|(k, v)| k == GIT_TOKEN_ENV && v.is_none())
        );

        let anon = RestHost::at(None, Origins::github(), "https://github.com".into());
        assert!(crate::host::ReleaseHost::reads_anonymously(&anon));
        let absent = RestHost::at(
            Some(dir.path().join("missing-token")),
            Origins::github(),
            "https://github.com".into(),
        );
        assert!(crate::host::ReleaseHost::reads_anonymously(&absent));
        assert!(!crate::host::ReleaseHost::reads_anonymously(&host));

        let blank = dir.path().join("blank-token");
        std::fs::write(&blank, "  \n\t\r\n").expect("blank token");
        let blank_host = RestHost::at(Some(blank), Origins::github(), "https://github.com".into());
        assert!(
            crate::host::ReleaseHost::reads_anonymously(&blank_host),
            "whitespace-only token file reads anonymously through CDN probes"
        );

        let corrupt = dir.path().join("corrupt-token");
        std::fs::write(&corrupt, b"ghp_bad\x00token\n").expect("corrupt token");
        let corrupt_host = RestHost::at(
            Some(corrupt),
            Origins::github(),
            "https://github.com".into(),
        );
        assert!(
            !crate::host::ReleaseHost::reads_anonymously(&corrupt_host),
            "corrupt token file must not silently fall back to anonymous reads"
        );
    }
}
