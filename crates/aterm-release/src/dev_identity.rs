// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Self-signed `aterm dev` code-signing identity for `ship dev-app`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use aterm_uds::deadline::RunError;
#[cfg(test)]
use aterm_uds::spawnfd::Command;

use crate::dev_app::{DEV_BUNDLE_ID, DEV_DISPLAY_NAME};
pub use aterm_secrets::keychain_tools::{Bounded, Run, WAIT};
pub(crate) use aterm_secrets::keychain_tools::{protected, search_list_with, unquote_paths};

type Arg<'a> = &'a dyn AsRef<OsStr>;

/// The identity's certificate common name, and the name codesign finds it by.
pub const NAME: &str = "aterm dev";

const DAYS: &str = "3650";

const CLEANUP_WAIT: Duration = Duration::from_secs(10);

const OPENSSL: &str = aterm_uds::os_path!("openssl");
const SECURITY: &str = aterm_uds::os_path!("security");
pub const CODESIGN: &str = aterm_uds::os_path!("codesign");

/// Where the identity lives unless `--keychain` names another file.
#[must_use]
pub fn default_keychain(home: &Path) -> PathBuf {
    home.join("Library/Keychains/aterm-dev.keychain-db")
}

/// Why a keychain step stopped.
#[derive(Debug)]
enum Stop {
    /// macOS's keychain service did not answer within [`WAIT`].
    Unanswered,
    /// Anything else, said in full.
    Refused(String),
}

impl From<String> for Stop {
    fn from(why: String) -> Self {
        Self::Refused(why)
    }
}

/// One-line report for a service that did not answer.
fn unanswered(state: &str) -> String {
    format!(
        "macOS's keychain service did not answer within {} s (trustd/securityd); {state} — \
         try again once it answers (a restart clears a wedged trustd)",
        WAIT.as_secs()
    )
}

/// Every SHA-1 of an identity named exactly [`NAME`] in `find-identity -p codesigning` output.
fn identity_hashes(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| {
            let (number, rest) = line.trim_start().split_once(") ")?;
            if !number.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let (hash, name) = rest.split_once(' ')?;
            let hex = hash.len() == 40
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b));
            let matches_name = name
                .strip_prefix('"')
                .and_then(|s| s.strip_prefix(NAME))
                .is_some_and(|s| s.starts_with('"'));
            (hex && matches_name).then(|| hash.to_string())
        })
        .collect()
}

struct Keychain<'a> {
    path: &'a Path,
    run: &'a dyn Run,
}

impl Keychain<'_> {
    fn call(&self, program: &str, args: &[Arg<'_>], wait: Duration) -> Result<Output, Stop> {
        let mut cmd = aterm_uds::spawnfd::command(program);
        cmd.args(args.iter().map(|a| a.as_ref()));
        match self.run.run(&mut cmd, wait) {
            Ok(out) => Ok(out),
            Err(RunError::TimedOut) => Err(Stop::Unanswered),
            Err(e) => Err(Stop::Refused(format!("{program} {e}"))),
        }
    }

    fn security(&self, args: &[Arg<'_>]) -> Result<Output, Stop> {
        self.call(SECURITY, args, WAIT)
    }

    fn security_ok(&self, args: &[Arg<'_>], what: &str) -> Result<Output, Stop> {
        let out = self.security(args)?;
        if out.status.success() {
            Ok(out)
        } else {
            Err(Stop::Refused(format!(
                "security {what} on {} failed: {}",
                self.path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    fn named_keychains(&self) -> Result<Vec<PathBuf>, Stop> {
        let mut named = Vec::new();
        for args in [
            &["login-keychain", "-d", "user"][..],
            &["login-keychain"],
            &["default-keychain", "-d", "user"],
            &["default-keychain"],
        ] {
            let args: Vec<Arg<'_>> = args.iter().map(|a| a as Arg<'_>).collect();
            let out = self.security(&args)?;
            named.extend(unquote_paths(&String::from_utf8_lossy(&out.stdout)));
        }
        Ok(named)
    }

    /// Refused when this is a login or default keychain ([`protected`]).
    fn refuse_protected(&self) -> Result<(), Stop> {
        if protected(self.path, &self.named_keychains()?) {
            return Err(Stop::Refused(format!(
                "{} is the login or default keychain, or wears one of their names (case \
                 ignored, links followed) — refusing to touch it (`ship dev-app identity` \
                 creates, locks, opens and deletes only a keychain it made)",
                self.path.display()
            )));
        }
        Ok(())
    }

    fn unlocked_with_empty_password(&self) -> Result<bool, Stop> {
        let out = self.security(&[&"unlock-keychain", &"-p", &"", &self.path])?;
        Ok(out.status.success())
    }

    fn open_locked(&self) -> Result<(), Stop> {
        self.security(&[&"lock-keychain", &self.path])?;
        if self.unlocked_with_empty_password()? {
            return Ok(());
        }
        Err(Stop::Refused(format!(
            "{} did not unlock with the empty password `ship dev-app identity` sets: it is not \
             ours (not made here, or re-passworded), and it is now LOCKED — locking and opening \
             it is how the password is proved. `--remove` deletes only what it can open, so \
             move it aside or delete it yourself (Keychain Access, or `security \
             delete-keychain '{}'`), then run `ship dev-app identity` again",
            self.path.display(),
            self.path.display()
        )))
    }

    fn identities(&self) -> Result<Vec<String>, Stop> {
        let out = self.security(&[&"find-identity", &"-p", &"codesigning", &self.path])?;
        Ok(identity_hashes(&String::from_utf8_lossy(&out.stdout)))
    }

    fn search_list(&self) -> Result<Vec<PathBuf>, Stop> {
        let out = self.security(&[&"list-keychains", &"-d", &"user"])?;
        Ok(unquote_paths(&String::from_utf8_lossy(&out.stdout)))
    }

    /// Put this keychain on the user's search list ([`search_list_with`]).
    fn ensure_on_search_list(&self) -> Result<(), Stop> {
        let Some(list) = search_list_with(&self.search_list()?, self.path)? else {
            return Ok(());
        };
        let mut args: Vec<Arg<'_>> = vec![&"list-keychains", &"-d", &"user", &"-s"];
        args.extend(list.iter().map(|p| p as Arg<'_>));
        self.security_ok(&args, "list-keychains -s")?;
        eprintln!(
            "ship dev-app: added {} to the keychain search list",
            self.path.display()
        );
        Ok(())
    }
}

/// Unlock the default identity's keychain for a build and return the identity's SHA-1.
/// # Errors
/// The keychain exists and cannot be used now (foreign, protected, empty, off the search list, or
/// its service did not answer), with the reason.
pub fn arm(
    keychain: &Path,
    manage_search_list: bool,
    run: &dyn Run,
) -> Result<Option<String>, String> {
    if !keychain.is_file() {
        return Ok(None);
    }
    let k = Keychain {
        path: keychain,
        run,
    };
    let armed = (|| {
        k.refuse_protected()?;
        if !k.unlocked_with_empty_password()? {
            k.open_locked()?;
        }
        let Some(hash) = k.identities()?.into_iter().next() else {
            return Err(Stop::Refused(format!(
                "{} exists but holds no \"{NAME}\" identity; run `ship dev-app identity` \
                 (making it again is harmless)",
                keychain.display()
            )));
        };
        if manage_search_list {
            k.ensure_on_search_list().map_err(|stop| match stop {
                Stop::Refused(why) => Stop::Refused(format!(
                    "{why}; could not put {} on the keychain search list, and codesign reads \
                     no other",
                    keychain.display()
                )),
                other => other,
            })?;
        }
        Ok(hash)
    })();
    match armed {
        Ok(hash) => Ok(Some(hash)),
        Err(Stop::Refused(why)) => Err(why),
        Err(Stop::Unanswered) => Err(unanswered("nothing was changed")),
    }
}

/// Make the "aterm dev" identity in `keychain`.
/// # Errors
/// Refusal ([`protected`], not ours, a tool that failed), or the service did not answer.
pub fn create(keychain: &Path, manage_search_list: bool, run: &dyn Run) -> Result<(), String> {
    let k = Keychain {
        path: keychain,
        run,
    };
    let mut progress = Progress::Untouched;
    match create_steps(&k, manage_search_list, &mut progress) {
        Ok(()) => Ok(()),
        Err(Stop::Refused(why)) => Err(why),
        Err(Stop::Unanswered) => Err(unanswered(&progress.after_unanswered(&k))),
    }
}

enum Progress {
    Untouched,
    /// The keychain file was made by this run and holds no usable identity yet.
    Begun,
    /// The identity is usable; only the self-check had not finished.
    Usable,
}

impl Progress {
    fn after_unanswered(&self, k: &Keychain<'_>) -> String {
        let shown = k.path.display();
        match self {
            Self::Untouched => "nothing was changed".to_string(),
            Self::Usable => {
                format!("\"{NAME}\" is made in {shown}; its signing self-check did not finish")
            }
            Self::Begun if !k.path.exists() => "nothing was changed".to_string(),
            Self::Begun => {
                let deleted = k
                    .call(SECURITY, &[&"delete-keychain", &k.path], CLEANUP_WAIT)
                    .is_ok_and(|out| out.status.success());
                if deleted {
                    format!("the keychain this run had begun ({shown}) was deleted")
                } else {
                    let _ = std::fs::remove_file(k.path);
                    format!(
                        "the keychain this run had begun ({shown}) was removed by hand; a \
                         search-list entry for it may remain"
                    )
                }
            }
        }
    }
}

fn create_steps(
    k: &Keychain<'_>,
    manage_search_list: bool,
    progress: &mut Progress,
) -> Result<(), Stop> {
    k.refuse_protected()?;
    if k.path.is_file() {
        k.open_locked()?;
    } else {
        println!(
            "==> creating {} (empty password, never auto-locks)",
            k.path.display()
        );
        *progress = Progress::Begun;
        k.security_ok(
            &[&"create-keychain", &"-p", &"", &k.path],
            "create-keychain",
        )?;
    }
    k.security_ok(
        &[&"set-keychain-settings", &k.path],
        "set-keychain-settings",
    )?;
    let scratch = PrivateDir::create()?;
    let hash = match k.identities()?.into_iter().next() {
        Some(hash) => {
            println!(
                "ship dev-app: \"{NAME}\" is already in {} (leaf {hash}) — nothing to create",
                k.path.display()
            );
            hash
        }
        None => {
            let (p12, password) = generate(k, scratch.path())?;
            println!("==> importing into {}", k.path.display());
            k.security_ok(
                &[
                    &"import", &p12, &"-k", &k.path, &"-P", &password, &"-T", &CODESIGN, &"-T",
                    &SECURITY,
                ],
                "import",
            )?;
            let Some(hash) = k.identities()?.into_iter().next() else {
                return Err(Stop::Refused(format!(
                    "imported, but `security find-identity -p codesigning` does not list \
                     \"{NAME}\" in {}",
                    k.path.display()
                )));
            };
            println!(
                "ship dev-app: created \"{NAME}\" in {} (leaf {hash})",
                k.path.display()
            );
            hash
        }
    };
    k.security_ok(
        &[
            &"set-key-partition-list",
            &"-S",
            &"apple-tool:,apple:,codesign:",
            &"-s",
            &"-l",
            &NAME,
            &"-k",
            &"",
            &k.path,
        ],
        "set-key-partition-list",
    )?;
    if manage_search_list {
        k.ensure_on_search_list()?;
    }
    *progress = Progress::Usable;
    if !k.search_list()?.iter().any(|entry| entry == k.path) {
        println!(
            "ship dev-app: note — {} is not on the keychain search list, so codesign cannot use \
             \"{NAME}\" from it; signing self-check skipped",
            k.path.display()
        );
        return Ok(());
    }
    self_check(k, &hash, scratch.path())
}

/// A certificate request config: RSA leaf ([`NAME`]).
fn request_config() -> String {
    format!(
        "[req]\ndistinguished_name = dn\nx509_extensions = codesign\nprompt = no\n\
         [dn]\nCN = {NAME}\n\
         [codesign]\nbasicConstraints = critical,CA:false\n\
         keyUsage = critical,digitalSignature\nextendedKeyUsage = critical,codeSigning\n\
         subjectKeyIdentifier = hash\n"
    )
}

fn generate(k: &Keychain<'_>, dir: &Path) -> Result<(PathBuf, String), Stop> {
    if !Path::new(OPENSSL).is_file() {
        return Err(Stop::Refused(format!(
            "no {OPENSSL} — the dev signing identity is macOS-only"
        )));
    }
    let config = dir.join("req.cnf");
    let key = dir.join("key.pem");
    let cert = dir.join("cert.pem");
    let p12 = dir.join("id.p12");
    crate::durable::write_fresh(&config, request_config())
        .map_err(|e| format!("write {}: {e}", config.display()))?;
    println!("==> generating \"{NAME}\" (RSA-2048, {DAYS} days) with {OPENSSL}");
    let ok = |out: Output, what: &str| {
        if out.status.success() {
            Ok(out)
        } else {
            Err(Stop::Refused(format!(
                "openssl {what} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    };
    ok(
        k.call(
            OPENSSL,
            &[
                &"req",
                &"-x509",
                &"-newkey",
                &"rsa:2048",
                &"-nodes",
                &"-days",
                &DAYS,
                &"-config",
                &config,
                &"-keyout",
                &key,
                &"-out",
                &cert,
            ],
            WAIT,
        )?,
        "req",
    )?;
    let rand = ok(k.call(OPENSSL, &[&"rand", &"-hex", &"16"], WAIT)?, "rand")?;
    let password = String::from_utf8_lossy(&rand.stdout).trim().to_string();
    if password.is_empty() {
        return Err(Stop::Refused("openssl rand printed no passphrase".into()));
    }
    let passout = format!("pass:{password}");
    ok(
        k.call(
            OPENSSL,
            &[
                &"pkcs12",
                &"-export",
                &"-inkey",
                &key,
                &"-in",
                &cert,
                &"-name",
                &NAME,
                &"-passout",
                &passout,
                &"-out",
                &p12,
            ],
            WAIT,
        )?,
        "pkcs12",
    )?;
    Ok((p12, password))
}

fn self_check(k: &Keychain<'_>, hash: &str, dir: &Path) -> Result<(), Stop> {
    let app = dir.join("probe.app");
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).map_err(|e| format!("create {}: {e}", macos.display()))?;
    crate::durable::copy_regular_fresh("/bin/echo", macos.join("probe"))
        .map_err(|e| format!("copy /bin/echo into the probe bundle: {e}"))?;
    crate::durable::write_fresh(
        &app.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD \
             PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist \
             version=\"1.0\"><dict>\n\t<key>CFBundleIdentifier</key><string>{DEV_BUNDLE_ID}</string>\n\
             \t<key>CFBundleName</key><string>{DEV_DISPLAY_NAME}</string>\n\
             \t<key>CFBundleExecutable</key><string>probe</string>\n\
             \t<key>CFBundlePackageType</key><string>APPL</string>\n</dict></plist>\n"
        ),
    )
    .map_err(|e| format!("write the probe bundle's Info.plist: {e}"))?;
    println!("==> self-check: codesign --sign {hash} (\"{NAME}\") on a scratch bundle");
    let signed = k.call(CODESIGN, &[&"--force", &"--sign", &hash, &app], WAIT)?;
    if !signed.status.success() {
        return Err(Stop::Refused(format!(
            "codesign could not use \"{NAME}\": {}. The identity exists; the key is what it \
             could not reach, although its partition list was just set. If macOS raised \
             \"codesign wants to access key …\", answer Always Allow and run `ship dev-app \
             identity` again; `security dump-keychain -a '{}'` shows the key's ACL",
            String::from_utf8_lossy(&signed.stderr).trim(),
            k.path.display()
        )));
    }
    let verified = k.call(CODESIGN, &[&"--verify", &"--strict", &app], WAIT)?;
    if !verified.status.success() {
        return Err(Stop::Refused(format!(
            "the self-check bundle signed with \"{NAME}\" does not verify: {}",
            String::from_utf8_lossy(&verified.stderr).trim()
        )));
    }
    let read = k.call(CODESIGN, &[&"-d", &"-r-", &app], WAIT)?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&read.stdout),
        String::from_utf8_lossy(&read.stderr)
    );
    let requirement = crate::sign::designated_requirement(&text).unwrap_or_default();
    if !names_leaf(&text, hash) {
        return Err(Stop::Refused(format!(
            "self-check FAILED — the designated requirement is not this identity's: \
             {requirement}"
        )));
    }
    println!("ship dev-app: self-check passed — verified --strict; designated requirement:");
    println!("    {requirement}");
    println!(
        "ship dev-app: `ship dev-app` now signs with \"{NAME}\" by default (--sign-id - signs ad hoc)."
    );
    Ok(())
}

/// Whether `codesign -d -r-` output pins the leaf `hash`.
fn names_leaf(codesign_output: &str, hash: &str) -> bool {
    const LEAF_MARKER: &[u8] = b"certificate leaf";
    let hash_bytes = hash.as_bytes();
    let window_len = hash_bytes.len() + 3;
    crate::sign::classify_dr(codesign_output) == crate::sign::DrClass::Identity
        && crate::sign::designated_requirement(codesign_output).is_some_and(|dr| {
            dr.as_bytes()
                .windows(LEAF_MARKER.len())
                .position(|w| w.eq_ignore_ascii_case(LEAF_MARKER))
                .and_then(|pos| dr.as_bytes().get(pos + LEAF_MARKER.len()..))
                .is_some_and(|pinned| {
                    pinned.windows(window_len).any(|w| {
                        matches!(w[0], b'h' | b'H')
                            && w[1] == b'"'
                            && w[window_len - 1] == b'"'
                            && w[2..window_len - 1].eq_ignore_ascii_case(hash_bytes)
                    })
                })
        })
}

/// Delete `keychain` and its identity, only when it opens with the empty password.
/// # Errors
/// Refused ([`protected`], not ours), or the service did not answer.
pub fn remove(keychain: &Path, run: &dyn Run) -> Result<(), String> {
    if !keychain.is_file() {
        println!(
            "ship dev-app: no keychain at {} — nothing to remove",
            keychain.display()
        );
        return Ok(());
    }
    let k = Keychain {
        path: keychain,
        run,
    };
    let mut asked = false;
    let removed = (|| {
        k.refuse_protected()?;
        k.open_locked()?;
        asked = true;
        k.security_ok(&[&"delete-keychain", &k.path], "delete-keychain")?;
        Ok(())
    })();
    match removed {
        Ok(()) => {
            println!(
                "ship dev-app: deleted {} (and its \"{NAME}\" identity); grants made under it are \
                 now stale, and the next dev build asks once more",
                keychain.display()
            );
            Ok(())
        }
        Err(Stop::Refused(why)) => Err(why),
        Err(Stop::Unanswered) if asked => Err(unanswered(&format!(
            "whether {} was deleted is not known",
            keychain.display()
        ))),
        Err(Stop::Unanswered) => Err(unanswered("nothing was changed")),
    }
}

struct PrivateDir(aterm_tempfile::TempDir);

impl PrivateDir {
    fn create() -> Result<Self, Stop> {
        aterm_tempfile::Builder::new()
            .prefix("aterm-dev-identity.")
            .suffix(".noindex")
            .tempdir()
            .map(Self)
            .map_err(|e| Stop::Refused(format!("create private dir: {e}")))
    }

    fn path(&self) -> &Path {
        self.0.path()
    }
}

/// A stand-in for the tools [`Run`] starts.
#[cfg(test)]
#[path = "dev_identity_fake_tests.rs"]
pub(crate) mod fake;

#[cfg(test)]
#[path = "dev_identity_tests.rs"]
mod tests;
