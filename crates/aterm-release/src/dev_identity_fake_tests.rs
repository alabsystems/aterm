// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
                let requirement =
                    self.requirement
                        .clone()
                        .unwrap_or_else(|| {
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
