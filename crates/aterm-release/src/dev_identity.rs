// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The self-signed "aterm dev" code-signing identity `ship dev-app` signs with (`ship dev-app identity` makes it), so a dev bundle's macOS privacy grants survive a rebuild: tccd keys a grant to the code's designated requirement, an ad-hoc one is a bare `cdhash` (the exact bytes), and a certificate-backed one names `identifier "com.aterm.aterm.dev" and certificate leaf = H"<sha1>"`, no bytes (docs/DESIGN-macos-tcc-prompts-2026-08-30.md §3.1 and S6 have the measurements).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use aterm_uds::deadline::RunError;
use aterm_uds::spawnfd::Command;

use crate::dev_app::{DEV_BUNDLE_ID, DEV_DISPLAY_NAME};

/// A tool argument: a word, a path, a secret held as a `String`.
type Arg<'a> = &'a dyn AsRef<OsStr>;

/// The identity's certificate common name, and the name codesign finds it by.
pub const NAME: &str = "aterm dev";

/// How long the certificate lasts; codesign refuses it after, and `--remove` then a
/// fresh `identity` makes another.
const DAYS: &str = "3650";

/// The one hang detector every tool call runs under: an honest call answers in well under a second.
pub const WAIT: Duration = Duration::from_secs(60);

/// A begun keychain is deleted under this shorter bound once the service stopped
/// answering: it will likely not answer this either.
const CLEANUP_WAIT: Duration = Duration::from_secs(10);

const OPENSSL: &str = "/usr/bin/openssl";
const SECURITY: &str = "/usr/bin/security";
pub const CODESIGN: &str = "/usr/bin/codesign";

/// The basenames macOS gives the keychains this module never touches, compared
/// case-folded (a Mac volume folds case unless made otherwise).
const PROTECTED_BASENAMES: [&str; 3] = ["login.keychain-db", "login.keychain", "System.keychain"];

/// Where the identity lives unless `--keychain` names another file.
#[must_use]
pub fn default_keychain(home: &Path) -> PathBuf {
    home.join("Library/Keychains/aterm-dev.keychain-db")
}

/// How a tool child runs: [`Bounded`] in the product, a scripted stand-in in tests.
pub trait Run {
    /// `cmd`'s output, if it finishes within `wait`.
    /// # Errors
    /// [`RunError`]: it could not start, or did not finish in time.
    fn run(&self, cmd: &mut Command, wait: Duration) -> Result<Output, RunError>;
}

/// [`aterm_uds::deadline::output_until`]: the child leads its own group, killed (`SIGKILL`) at the deadline.
pub struct Bounded;

impl Run for Bounded {
    fn run(&self, cmd: &mut Command, wait: Duration) -> Result<Output, RunError> {
        aterm_uds::deadline::output_until(cmd, Instant::now() + wait)
    }
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

/// The one line a service that did not answer is reported in; `state` says what the
/// run left behind.
fn unanswered(state: &str) -> String {
    format!(
        "macOS's keychain service did not answer within {} s (trustd/securityd); {state} — \
         try again once it answers (a restart clears a wedged trustd)",
        WAIT.as_secs()
    )
}

/// Every quoted path in `security`'s keychain listings, one per line.
fn unquote_paths(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .map(PathBuf::from)
        })
        .collect()
}

/// Every SHA-1 of an identity named exactly [`NAME`] in `find-identity -p codesigning`
/// output (`  1) <40 hex> "aterm dev"`), in order; "aterm dev 2" is another name.
fn identity_hashes(listing: &str) -> Vec<String> {
    let quoted = format!("\"{NAME}\"");
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
            (hex && name.starts_with(&quoted)).then(|| hash.to_string())
        })
        .collect()
}

/// The search list to write so `keychain` is on it: `None` when it is already there,
/// else every entry read back, in order, with `keychain` last.
fn search_list_with(current: &[PathBuf], keychain: &Path) -> Result<Option<Vec<PathBuf>>, String> {
    if current.iter().any(|entry| entry == keychain) {
        return Ok(None);
    }
    if current.is_empty() {
        return Err(format!(
            "the keychain search list read back empty; not rewriting it with {} alone (that \
             would drop login from it)",
            keychain.display()
        ));
    }
    let mut list = current.to_vec();
    list.push(keychain.to_path_buf());
    Ok(Some(list))
}

/// `path` with a symlink in its LAST component followed, link by link (a relative
/// target read from the link's own directory).
fn follow(path: &Path) -> Option<PathBuf> {
    let mut path = path.to_path_buf();
    for _ in 0..=32 {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let target = std::fs::read_link(&path).ok()?;
                path = if target.is_absolute() {
                    target
                } else {
                    path.parent().unwrap_or(Path::new("")).join(target)
                };
            }
            _ => return Some(path),
        }
    }
    None
}

/// `path` with its directory resolved (`.`, `..`, symlinked directories) when that
/// directory exists, else as given.
fn canon(path: &Path) -> PathBuf {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return path.to_path_buf();
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    std::fs::canonicalize(dir).map_or_else(|_| path.to_path_buf(), |dir| dir.join(name))
}

/// The file `path` names, as (device, inode), a symlink followed: a hard link has no
/// other tell.
fn file_id(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

fn folded(path: &Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}

/// Whether `path` is a login or default keychain, every compare case-folded: by its basename or the one a symlink in its last component names, by the path `named` (what `security` answers for the login and default keychains) holds, or as the same file as one of those.
#[must_use]
pub fn protected(path: &Path, named: &[PathBuf]) -> bool {
    let Some(real) = follow(path) else {
        return true;
    };
    let protected_name = |p: &Path| {
        p.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            PROTECTED_BASENAMES
                .iter()
                .any(|b| b.eq_ignore_ascii_case(&name))
        })
    };
    if protected_name(path) || protected_name(&real) {
        return true;
    }
    let want = folded(&canon(&real));
    let id = file_id(path);
    named.iter().any(|entry| {
        let resolved = follow(entry).unwrap_or_else(|| entry.clone());
        folded(&canon(&resolved)) == want || (id.is_some() && file_id(entry) == id)
    })
}

/// One keychain file and the way its tools are run.
struct Keychain<'a> {
    path: &'a Path,
    run: &'a dyn Run,
}

impl Keychain<'_> {
    /// `program args…` under `wait`; a status that failed is the caller's to judge.
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

    /// `security args…`, which must succeed; `what` names it in the refusal.
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

    /// The login and default keychains, as `security` names them in the user domain and
    /// the default one (one of the two can answer nothing).
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

    /// Refused when this is a login or default keychain ([`protected`]), before any
    /// lock, open, create or delete.
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

    /// Lock, then open with the empty password: the ownership proof, and the cure for a
    /// refused bare unlock.
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

    /// Put this keychain on the user's search list, keeping every entry already there in
    /// its order ([`search_list_with`]).
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

/// Unlock the default identity's keychain for a build and return the identity's SHA-1 (what codesign signs with: unambiguous even if another keychain holds the name).
/// # Errors
/// The keychain EXISTS and cannot be used now (foreign, protected, empty, off the search list, or its service did not answer), with the reason: a build stops on that rather than sign ad hoc and orphan the grants the identity keeps.
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

/// Make the "aterm dev" identity in `keychain` (idempotent), partition its key for codesign, put the keychain on the search list when `manage_search_list`, and prove it by signing a scratch bundle.
/// # Errors
/// Any refusal ([`protected`], not ours, a tool that failed), or the service did not answer — said in one line.
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

/// How far a create run got, for what a service that stopped answering leaves behind.
enum Progress {
    Untouched,
    /// The keychain file was made by this run and holds no usable identity yet.
    Begun,
    /// The identity is usable; only the self-check had not finished.
    Usable,
}

impl Progress {
    /// What the run left, after deleting a begun keychain (bounded shorter; by hand when
    /// even that does not answer, which can leave a search-list entry).
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
        // An existing keychain is ours only if it opens with the empty password.
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

/// A certificate request config: RSA leaf, CN [`NAME`], code signing only, every
/// extension critical; a config section works on LibreSSL and OpenSSL alike.
fn request_config() -> String {
    format!(
        "[req]\ndistinguished_name = dn\nx509_extensions = codesign\nprompt = no\n\
         [dn]\nCN = {NAME}\n\
         [codesign]\nbasicConstraints = critical,CA:false\n\
         keyUsage = critical,digitalSignature\nextendedKeyUsage = critical,codeSigning\n\
         subjectKeyIdentifier = hash\n"
    )
}

/// Generate the key and self-signed certificate in `dir` (private) and wrap them in a
/// PKCS#12 under a one-shot passphrase: `(p12 path, passphrase)`.
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
    std::fs::write(&config, request_config())
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

/// Sign a scratch bundle with the dev bundle id by hash, as `ship dev-app` signs, and
/// read it back: it must verify `--strict` and its requirement must name THIS leaf.
fn self_check(k: &Keychain<'_>, hash: &str, dir: &Path) -> Result<(), Stop> {
    let app = dir.join("probe.app");
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).map_err(|e| format!("create {}: {e}", macos.display()))?;
    std::fs::copy("/bin/echo", macos.join("probe"))
        .map_err(|e| format!("copy /bin/echo into the probe bundle: {e}"))?;
    std::fs::write(
        app.join("Contents/Info.plist"),
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

/// Whether `codesign -d -r-` output's designated requirement is identity-based and pins
/// the leaf `hash` (`certificate leaf = H"<hash>"`).
fn names_leaf(codesign_output: &str, hash: &str) -> bool {
    let leaf = format!("h\"{}\"", hash.to_ascii_lowercase());
    crate::sign::classify_dr(codesign_output) == crate::sign::DrClass::Identity
        && crate::sign::designated_requirement(codesign_output).is_some_and(|dr| {
            let dr = dr.to_ascii_lowercase();
            dr.split_once("certificate leaf")
                .is_some_and(|(_, pinned)| pinned.contains(&leaf))
        })
}

/// Delete `keychain` and its identity, only when it opens with the empty password; no keychain is nothing to do.
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

/// A 0700 scratch directory, removed with everything in it on drop: it holds the
/// private key and its PKCS#12 only until `security import` has them.
struct PrivateDir(PathBuf);

impl PrivateDir {
    /// A fresh directory: a name a dead process of the same pid left is passed over.
    fn create() -> Result<Self, Stop> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        loop {
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("aterm-dev-identity.{}.{n}", std::process::id()));
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1024 => {}
                Err(e) => {
                    return Err(Stop::Refused(format!("create {}: {e}", path.display())));
                }
            }
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A stand-in for the tools [`Run`] starts: a small model of macOS's keychains (files, passwords, lock state, identities, the user's search list) behind `security`, and scripted `openssl`, `codesign`, `lsregister` and `--version` answers.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::os::unix::process::ExitStatusExt as _;

    /// One modelled keychain.
    #[derive(Default)]
    pub struct Kc {
        pub password: String,
        pub locked: bool,
        pub identities: Vec<String>,
    }

    /// What the stand-in tools see and answer.
    #[derive(Default)]
    pub struct World {
        pub keychains: BTreeMap<PathBuf, Kc>,
        pub search_list: Vec<PathBuf>,
        /// What `security login-keychain` / `default-keychain` answer.
        pub named: Vec<PathBuf>,
        /// Every call: the tool's file name, then its arguments.
        pub log: Vec<Vec<String>>,
        /// How many of the next unlocks are refused as securityd once refused one.
        pub refuse_unlocks: usize,
        /// `"<tool> <first argument>"` that never answers.
        pub hang_on: Option<String>,
        /// `"<tool> <first argument>"` that answers with a failure.
        pub fail_on: Option<String>,
        /// What `codesign -d -r-` answers; `None` names the leaf last signed with.
        pub requirement: Option<String>,
        pub signed_with: Option<String>,
        /// What `<program> --version` prints, by program.
        pub versions: BTreeMap<PathBuf, String>,
        pub minted: u64,
    }

    pub struct Fake(pub RefCell<World>);

    impl Fake {
        pub fn new(world: World) -> Self {
            Self(RefCell::new(world))
        }

        /// The logged calls of `tool` whose first argument is `first`.
        pub fn calls(&self, tool: &str, first: &str) -> Vec<Vec<String>> {
            self.0
                .borrow()
                .log
                .iter()
                .filter(|c| c[0] == tool && c.get(1).map(String::as_str) == Some(first))
                .cloned()
                .collect()
        }
    }

    pub fn out(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    fn quoted(paths: &[PathBuf]) -> String {
        paths
            .iter()
            .map(|p| format!("    \"{}\"\n", p.display()))
            .collect()
    }

    impl World {
        fn security(&mut self, sub: &str, rest: &[String]) -> Output {
            let path = rest.last().map(PathBuf::from).unwrap_or_default();
            match sub {
                "login-keychain" | "default-keychain" => out(0, &quoted(&self.named), ""),
                "list-keychains" => match rest.iter().position(|a| a == "-s") {
                    Some(at) => {
                        self.search_list = rest[at + 1..].iter().map(PathBuf::from).collect();
                        out(0, "", "")
                    }
                    None => out(0, &quoted(&self.search_list), ""),
                },
                "create-keychain" => {
                    if self.keychains.contains_key(&path) {
                        return out(48, "", "A keychain with the same name already exists.");
                    }
                    let password = rest[1].clone();
                    std::fs::write(&path, b"").expect("the modelled keychain's file");
                    self.keychains.insert(
                        path,
                        Kc {
                            password,
                            ..Kc::default()
                        },
                    );
                    out(0, "", "")
                }
                "lock-keychain" => match self.keychains.get_mut(&path) {
                    Some(kc) => {
                        kc.locked = true;
                        out(0, "", "")
                    }
                    None => out(50, "", "The specified keychain could not be found."),
                },
                "unlock-keychain" => {
                    if self.refuse_unlocks > 0 {
                        self.refuse_unlocks -= 1;
                        return out(
                            51,
                            "",
                            "The user name or passphrase you entered is not correct.",
                        );
                    }
                    match self.keychains.get_mut(&path) {
                        Some(kc) if !kc.locked || kc.password == rest[1] => {
                            kc.locked = false;
                            out(0, "", "")
                        }
                        _ => out(
                            51,
                            "",
                            "The user name or passphrase you entered is not correct.",
                        ),
                    }
                }
                "find-identity" => {
                    let listing = self.keychains.get(&path).map_or_else(String::new, |kc| {
                        let mut text = String::from("\nPolicy: Code Signing\n  Matching identities\n");
                        for (n, hash) in kc.identities.iter().enumerate() {
                            text.push_str(&format!(
                                "  {}) {hash} \"{NAME}\" (CSSMERR_TP_NOT_TRUSTED)\n",
                                n + 1
                            ));
                        }
                        text.push_str("     1 identities found\n\n  Valid identities only\n     0 valid identities found\n");
                        text
                    });
                    out(0, &listing, "")
                }
                "import" => {
                    let at = rest
                        .iter()
                        .position(|a| a == "-k")
                        .expect("import names -k");
                    let target = PathBuf::from(&rest[at + 1]);
                    self.minted += 1;
                    let hash = format!("{:040X}", 0xA7E5_0000 + self.minted);
                    match self.keychains.get_mut(&target) {
                        Some(kc) if !kc.locked => {
                            kc.identities.push(hash);
                            out(0, "1 identity imported.", "")
                        }
                        _ => out(1, "", "security: SecKeychainItemImport: failed"),
                    }
                }
                "set-keychain-settings" | "set-key-partition-list" => {
                    if self.keychains.contains_key(&path) {
                        out(0, "", "")
                    } else {
                        out(50, "", "The specified keychain could not be found.")
                    }
                }
                "delete-keychain" => {
                    if self.keychains.remove(&path).is_none() {
                        return out(50, "", "The specified keychain could not be found.");
                    }
                    self.search_list.retain(|p| *p != path);
                    let _ = std::fs::remove_file(&path);
                    out(0, "", "")
                }
                _ => out(2, "", "unmodelled security subcommand"),
            }
        }

        fn codesign(&mut self, args: &[String]) -> Output {
            match args.first().map(String::as_str) {
                Some("--force") => {
                    self.signed_with = args.get(2).cloned();
                    out(0, "", "")
                }
                Some("-d") => {
                    let requirement = self.requirement.clone().unwrap_or_else(|| {
                        match self.signed_with.as_deref() {
                            Some("-") | None => {
                                "# designated => cdhash H\"240c6c45749cff92effe2dbe2782cd3ac8bee253\"\n"
                                    .to_string()
                            }
                            Some(id) => format!(
                                "designated => identifier \"{DEV_BUNDLE_ID}\" and certificate \
                                 leaf = H\"{}\"\n",
                                id.to_ascii_lowercase()
                            ),
                        }
                    });
                    out(0, &requirement, "Executable=/fake\n")
                }
                _ => out(0, "", ""),
            }
        }
    }

    impl Run for Fake {
        fn run(&self, cmd: &mut Command, _wait: Duration) -> Result<Output, RunError> {
            let program = PathBuf::from(cmd.get_program());
            let tool = program
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let args: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let mut world = self.0.borrow_mut();
            let mut call = vec![tool.clone()];
            call.extend(args.iter().cloned());
            world.log.push(call);
            let first = args.first().map(String::as_str).unwrap_or_default();
            let key = format!("{tool} {first}");
            if world.hang_on.as_deref() == Some(&key) {
                return Err(RunError::TimedOut);
            }
            if world.fail_on.as_deref() == Some(&key) {
                return Ok(out(1, "", &format!("{tool}: scripted failure")));
            }
            Ok(match (tool.as_str(), first) {
                ("security", sub) => world.security(sub, &args[1..]),
                ("openssl", "rand") => out(0, "00112233445566778899aabbccddeeff\n", ""),
                ("openssl", _) | ("lsregister", _) => out(0, "", ""),
                ("codesign", _) => world.codesign(&args),
                (_, "--version") => match world.versions.get(&program) {
                    Some(line) => out(0, &format!("{line}\n"), ""),
                    None => out(1, "", "no such program"),
                },
                _ => out(127, "", "unmodelled tool"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Fake, Kc, World};
    use super::*;

    fn scratch() -> (aterm_tempfile::TempDir, PathBuf) {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let path = std::fs::canonicalize(dir.path()).expect("canonical scratch");
        (dir, path)
    }

    /// The (un)lock calls a run made, in order: `unlock`/`lock` and the password an
    /// unlock gave.
    fn lock_calls(fake: &Fake) -> Vec<String> {
        fake.0
            .borrow()
            .log
            .iter()
            .filter(|c| c[0] == "security" && c[1].ends_with("lock-keychain"))
            .map(|c| c[1..].join(" "))
            .collect()
    }

    fn mutating_calls(fake: &Fake) -> Vec<String> {
        fake.0
            .borrow()
            .log
            .iter()
            .filter(|c| {
                c[0] == "security"
                    && [
                        "lock-keychain",
                        "unlock-keychain",
                        "create-keychain",
                        "delete-keychain",
                    ]
                    .contains(&c[1].as_str())
            })
            .map(|c| c.join(" "))
            .collect()
    }

    #[test]
    fn arming_an_absent_keychain_is_silent_and_calls_nothing() {
        let (_dir, root) = scratch();
        let fake = Fake::new(World::default());
        assert_eq!(arm(&root.join("absent.keychain-db"), true, &fake), Ok(None));
        assert!(fake.0.borrow().log.is_empty());
    }

    /// Create makes one identity in a never-locking keychain whose key codesign may use
    /// without a dialog; arming finds it, on the keychain as left and after each lock
    /// (a bare unlock with the empty password opens it); a second create is a no-op.
    #[test]
    fn create_makes_one_identity_that_arming_finds_after_a_lock() {
        let (_dir, root) = scratch();
        let k = root.join("dev.keychain-db");
        let fake = Fake::new(World::default());
        create(&k, false, &fake).expect("create");
        assert!(k.is_file());
        {
            let world = fake.0.borrow();
            let kc = &world.keychains[&k];
            assert_eq!(kc.password, "", "the empty password is the ownership proof");
            assert_eq!(kc.identities.len(), 1);
            assert!(
                world.search_list.is_empty(),
                "--keychain never touches the list"
            );
        }
        let shown = k.display().to_string();
        let argv = |words: &[&str]| -> Vec<Vec<String>> {
            vec![words.iter().map(|w| (*w).to_string()).collect()]
        };
        assert_eq!(
            fake.calls("security", "set-keychain-settings"),
            argv(&["security", "set-keychain-settings", &shown])
        );
        assert_eq!(
            fake.calls("security", "set-key-partition-list"),
            argv(&[
                "security",
                "set-key-partition-list",
                "-S",
                "apple-tool:,apple:,codesign:",
                "-s",
                "-l",
                NAME,
                "-k",
                "",
                &shown,
            ])
        );
        assert_eq!(
            fake.calls("codesign", "--force"),
            Vec::<Vec<String>>::new(),
            "off the search list codesign cannot sign with it, so the self-check is skipped"
        );
        let leaf = fake.0.borrow().keychains[&k].identities[0].clone();

        assert_eq!(arm(&k, false, &fake), Ok(Some(leaf.clone())));
        for _ in 0..2 {
            fake.0.borrow_mut().keychains.get_mut(&k).unwrap().locked = true;
            fake.0.borrow_mut().log.clear();
            assert_eq!(arm(&k, false, &fake), Ok(Some(leaf.clone())));
            assert_eq!(
                lock_calls(&fake),
                [format!("unlock-keychain -p  {}", k.display())]
            );
        }

        create(&k, false, &fake).expect("create again");
        assert_eq!(
            fake.0.borrow().keychains[&k].identities,
            std::slice::from_ref(&leaf)
        );
        assert!(
            fake.calls("security", "import").is_empty(),
            "a second create imports nothing"
        );
    }

    /// Our own keychain can refuse a bare unlock; arming locks and reopens it once with
    /// the empty password and carries on.
    #[test]
    fn a_refused_bare_unlock_is_healed_by_lock_and_reopen() {
        let (_dir, root) = scratch();
        let k = root.join("dev.keychain-db");
        let fake = Fake::new(World::default());
        create(&k, false, &fake).expect("create");
        let leaf = fake.0.borrow().keychains[&k].identities[0].clone();
        {
            let mut world = fake.0.borrow_mut();
            world.keychains.get_mut(&k).unwrap().locked = true;
            world.refuse_unlocks = 1;
            world.log.clear();
        }
        assert_eq!(arm(&k, false, &fake), Ok(Some(leaf)));
        let shown = k.display();
        assert_eq!(
            lock_calls(&fake),
            [
                format!("unlock-keychain -p  {shown}"),
                format!("lock-keychain {shown}"),
                format!("unlock-keychain -p  {shown}"),
            ]
        );
        assert!(!fake.0.borrow().keychains[&k].locked, "left open");
    }

    /// A keychain with a password is not ours: arming refuses it (present, unusable),
    /// create imports nothing into it, and remove leaves it — each having locked it to
    /// prove the password, and said so.
    #[test]
    fn a_keychain_that_is_not_ours_is_refused_by_every_mode() {
        let (_dir, root) = scratch();
        let theirs = root.join("theirs.keychain-db");
        std::fs::write(&theirs, b"").unwrap();
        let mut world = World::default();
        world.keychains.insert(
            theirs.clone(),
            Kc {
                password: "not-ours".into(),
                locked: true,
                identities: Vec::new(),
            },
        );
        let fake = Fake::new(world);
        let refused = arm(&theirs, false, &fake).unwrap_err();
        assert!(
            refused.contains("did not unlock with the empty password"),
            "{refused}"
        );
        assert!(refused.contains("now LOCKED"), "{refused}");

        fake.0
            .borrow_mut()
            .keychains
            .get_mut(&theirs)
            .unwrap()
            .locked = false;
        let refused = create(&theirs, false, &fake).unwrap_err();
        assert!(refused.contains("not ours"), "{refused}");
        assert!(fake.calls("security", "import").is_empty());
        assert!(fake.0.borrow().keychains[&theirs].identities.is_empty());

        fake.0
            .borrow_mut()
            .keychains
            .get_mut(&theirs)
            .unwrap()
            .locked = false;
        let refused = remove(&theirs, &fake).unwrap_err();
        assert!(refused.contains("not ours"), "{refused}");
        assert!(
            theirs.is_file(),
            "remove deleted a keychain that is not ours"
        );
        assert!(fake.calls("security", "delete-keychain").is_empty());
    }

    /// A keychain wearing a protected name is refused by every mode BEFORE anything locks,
    /// opens, creates or deletes it — as named, through a link, in another case — and a
    /// login name is refused by create even where no file is yet.
    #[test]
    fn protected_keychains_are_refused_before_anything_touches_them() {
        let (_dir, root) = scratch();
        let system = root.join("System.keychain");
        std::fs::write(&system, b"").unwrap();
        let alias = root.join("alias.keychain-db");
        std::os::unix::fs::symlink(&system, &alias).unwrap();
        let mut spellings = vec![system.clone(), alias];
        let upper = root.join("SYSTEM.KEYCHAIN");
        if upper.exists() {
            spellings.push(upper);
        }
        for path in &spellings {
            let fake = Fake::new(World::default());
            let modes: [(&str, Result<(), String>); 3] = [
                ("arm", arm(path, true, &fake).map(|_| ())),
                ("create", create(path, true, &fake)),
                ("remove", remove(path, &fake)),
            ];
            for (mode, result) in modes {
                let refused = result.unwrap_err();
                assert!(
                    refused.contains("is the login or default keychain"),
                    "{mode} {}: {refused}",
                    path.display()
                );
            }
            assert_eq!(
                mutating_calls(&fake),
                Vec::<String>::new(),
                "{}",
                path.display()
            );
            assert!(system.is_file() && path.exists());
        }
        let fresh = root.join("fresh");
        std::fs::create_dir(&fresh).unwrap();
        for name in ["login.keychain-db", "login.keychain"] {
            let fake = Fake::new(World::default());
            let refused = create(&fresh.join(name), true, &fake).unwrap_err();
            assert!(
                refused.contains("is the login or default keychain"),
                "{refused}"
            );
            assert!(fake.calls("security", "create-keychain").is_empty());
            assert_eq!(std::fs::read_dir(&fresh).unwrap().count(), 0);
        }
    }

    /// The guard's other half: the file `security` names for the login or default
    /// keychain, seen through a hard link, a symlink, a relative chain, `..` and another
    /// case — and, with no file there, through another case and a dangling link — but
    /// never in a sibling.
    #[test]
    fn the_guard_sees_one_keychain_through_every_spelling() {
        let (_dir, g) = scratch();
        std::fs::create_dir(g.join("sub")).unwrap();
        let work = g.join("work.keychain-db");
        std::fs::write(&work, b"").unwrap();
        std::fs::write(g.join("other.keychain-db"), b"").unwrap();
        std::fs::hard_link(&work, g.join("hard.keychain-db")).unwrap();
        std::os::unix::fs::symlink(&work, g.join("sym.keychain-db")).unwrap();
        std::os::unix::fs::symlink("../sym.keychain-db", g.join("sub/chain.keychain-db")).unwrap();
        std::os::unix::fs::symlink("gone.keychain-db", g.join("dangling.keychain-db")).unwrap();
        let named = [work.clone()];
        for spelling in [
            "work.keychain-db",
            "hard.keychain-db",
            "sym.keychain-db",
            "sub/chain.keychain-db",
            "sub/../work.keychain-db",
            "WORK.KEYCHAIN-DB",
        ] {
            assert!(protected(&g.join(spelling), &named), "{spelling}");
        }
        let gone = [g.join("gone.keychain-db")];
        for spelling in ["GONE.KEYCHAIN-DB", "dangling.keychain-db"] {
            assert!(protected(&g.join(spelling), &gone), "{spelling}");
        }
        assert!(!protected(&g.join("other.keychain-db"), &named));
        for login in ["LOGIN.KEYCHAIN-DB", "Login.Keychain", "login.keychain-db"] {
            assert!(protected(&g.join("fresh").join(login), &[]), "{login}");
        }
        assert!(!protected(&g.join("aterm-dev.keychain-db"), &named));
    }

    /// The search list is re-issued in order with the keychain last, and an empty read
    /// is refused rather than written back as a list of one (which would drop login).
    #[test]
    fn the_search_list_is_rewritten_in_order_and_never_from_an_empty_read() {
        let k = Path::new("/k/aterm-dev.keychain-db");
        let login = PathBuf::from("/Users//nobody/Library/Keychains/login.keychain-db");
        let other = PathBuf::from("/Library/Keychains/Other.keychain");
        assert!(
            search_list_with(&[], k)
                .unwrap_err()
                .contains("read back empty")
        );
        assert_eq!(
            search_list_with(&[login.clone(), other.clone()], k),
            Ok(Some(vec![login.clone(), other.clone(), k.to_path_buf()]))
        );
        assert_eq!(
            search_list_with(&[login.clone(), k.to_path_buf()], k),
            Ok(None)
        );

        let (_dir, root) = scratch();
        let k = root.join("aterm-dev.keychain-db");
        let fake = Fake::new(World::default());
        create(&k, false, &fake).expect("create");
        let refused = arm(&k, true, &fake).unwrap_err();
        assert!(refused.contains("read back empty"), "{refused}");
        assert!(refused.contains("could not put"), "{refused}");
        let rewrites = |fake: &Fake| {
            fake.calls("security", "list-keychains")
                .into_iter()
                .filter(|c| c.contains(&"-s".to_string()))
                .collect::<Vec<_>>()
        };
        assert!(
            rewrites(&fake).is_empty(),
            "an empty read is never written back"
        );

        fake.0.borrow_mut().search_list = vec![login.clone(), other.clone()];
        let leaf = fake.0.borrow().keychains[&k].identities[0].clone();
        assert_eq!(arm(&k, true, &fake), Ok(Some(leaf)));
        assert_eq!(fake.0.borrow().search_list, [login, other, k.clone()]);
        assert_eq!(rewrites(&fake).len(), 1);
    }

    /// On the default keychain create ends on the search list and proves the identity
    /// by signing a scratch bundle; a requirement that does not name its leaf fails.
    #[test]
    fn create_on_the_search_list_signs_its_self_check_with_the_new_leaf() {
        let (_dir, root) = scratch();
        let k = root.join("aterm-dev.keychain-db");
        let login = root.join("login.keychain-db");
        let fake = Fake::new(World {
            search_list: vec![login.clone()],
            ..World::default()
        });
        create(&k, true, &fake).expect("create");
        let leaf = fake.0.borrow().keychains[&k].identities[0].clone();
        assert_eq!(fake.0.borrow().search_list, [login, k.clone()]);
        let signed = fake.calls("codesign", "--force");
        assert_eq!(signed.len(), 1);
        assert_eq!(signed[0][3], leaf, "signed by the leaf's hash");
        assert_eq!(fake.calls("codesign", "--verify").len(), 1);

        fake.0.borrow_mut().requirement = Some(
            "designated => identifier \"com.aterm.aterm.dev\" and certificate leaf = \
             H\"0000000000000000000000000000000000000000\"\n"
                .into(),
        );
        let refused = create(&k, true, &fake).unwrap_err();
        assert!(refused.contains("self-check FAILED"), "{refused}");
        assert!(!names_leaf("# designated => cdhash H\"240c\"\n", &leaf));
        let anchored = |clause: &str| {
            format!(
                "designated => identifier \"{DEV_BUNDLE_ID}\" and {clause} = H\"{}\"\n",
                leaf.to_ascii_lowercase()
            )
        };
        assert!(names_leaf(&anchored("certificate leaf"), &leaf));
        assert!(!names_leaf(&anchored("certificate root"), &leaf));
    }

    /// A service that stops answering stops the run with one line: arming says nothing
    /// was changed, and a create deletes the keychain it had begun.
    #[test]
    fn a_service_that_does_not_answer_stops_in_one_line() {
        let (_dir, root) = scratch();
        let k = root.join("dev.keychain-db");
        let fake = Fake::new(World::default());
        create(&k, false, &fake).expect("create");
        fake.0.borrow_mut().hang_on = Some("security unlock-keychain".into());
        let stopped = arm(&k, false, &fake).unwrap_err();
        assert!(!stopped.contains('\n'), "{stopped}");
        assert!(
            stopped.contains("did not answer within 60 s (trustd/securityd); nothing was changed"),
            "{stopped}"
        );

        let half = root.join("half.keychain-db");
        fake.0.borrow_mut().hang_on = Some("security set-keychain-settings".into());
        let stopped = create(&half, false, &fake).unwrap_err();
        assert!(!stopped.contains('\n'), "{stopped}");
        assert!(
            stopped.contains(&format!(
                "the keychain this run had begun ({}) was deleted",
                half.display()
            )),
            "{stopped}"
        );
        assert!(!half.exists(), "no half-made keychain is left");
        assert!(!fake.0.borrow().keychains.contains_key(&half));
    }

    #[test]
    fn remove_deletes_only_ours_and_a_second_remove_is_a_no_op() {
        let (_dir, root) = scratch();
        let k = root.join("dev.keychain-db");
        let fake = Fake::new(World::default());
        create(&k, false, &fake).expect("create");
        fake.0.borrow_mut().keychains.get_mut(&k).unwrap().locked = true;
        remove(&k, &fake).expect("remove");
        assert!(!k.exists() && fake.0.borrow().keychains.is_empty());
        assert_eq!(
            arm(&k, false, &fake),
            Ok(None),
            "nothing to arm after a remove"
        );
        fake.0.borrow_mut().log.clear();
        remove(&k, &fake).expect("a second remove");
        assert!(fake.0.borrow().log.is_empty());
    }

    #[test]
    fn the_identity_listing_matches_the_name_exactly() {
        let hash = "0123456789ABCDEF0123456789ABCDEF01234567";
        let listing = format!(
            "\nPolicy: Code Signing\n  Matching identities\n  1) {hash} \"aterm dev 2\"\n  \
             2) {hash} \"aterm dev\" (CSSMERR_TP_NOT_TRUSTED)\n  3) {} \"aterm dev\"\n     3 \
             identities found\n\n  Valid identities only\n     0 valid identities found\n",
            hash.to_ascii_lowercase()
        );
        assert_eq!(identity_hashes(&listing), [hash]);
        assert!(identity_hashes("").is_empty());
    }

    /// The certificate the real /usr/bin/openssl makes from [`request_config`] is a
    /// code-signing leaf: CN "aterm dev", the code-signing EKU, digitalSignature and
    /// CA:false.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_certificate_is_a_code_signing_leaf() {
        let (_dir, root) = scratch();
        let k = Keychain {
            path: &root.join("unused.keychain-db"),
            run: &Bounded,
        };
        let (p12, password) = generate(&k, &root).expect("generate");
        assert!(p12.is_file() && password.len() == 32);
        let mut cmd = aterm_uds::spawnfd::command(OPENSSL);
        cmd.args(["x509", "-noout", "-text", "-in"])
            .arg(root.join("cert.pem"));
        let text = Bounded.run(&mut cmd, WAIT).expect("openssl x509");
        let text = String::from_utf8_lossy(&text.stdout);
        for want in [
            "CN=aterm dev",
            "Code Signing",
            "Digital Signature",
            "CA:FALSE",
        ] {
            assert!(
                text.contains(want),
                "the certificate lacks {want:?}: {text}"
            );
        }
    }
}
