// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Formula and cask models parsed from formulae.brew.sh JSON, plus
//! Homebrew-shaped version comparison (design §3, §6).

use std::cmp::Ordering;

use aterm_json::Value;

/// A formula as abrew needs it. Parsed from the API object; fields
/// abrew does not use are dropped on purpose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Formula {
    pub name: String,
    pub full_name: String,
    pub desc: String,
    pub homepage: String,
    pub license: String,
    pub version: String,
    pub revision: u64,
    pub stable_url: String,
    pub stable_sha256: String,
    /// Bottle files: (tag, url, sha256), e.g. tag `arm64_tahoe`.
    pub bottle_files: Vec<(String, String, String)>,
    /// Tags whose API metadata needs no binary relocation, conditional platform
    /// resolution or post-install recipe. Others use the announced brew fallback.
    pub native_bottle_tags: Vec<String>,
    pub dependencies: Vec<String>,
    pub build_dependencies: Vec<String>,
    pub keg_only: bool,
    pub deprecate: bool,
    pub disable: bool,
    pub caveats: String,
    pub aliases: Vec<String>,
    pub oldnames: Vec<String>,
}

fn s(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_string()
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn empty_metadata(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.is_empty(),
        Some(Value::Object(items)) => items.is_empty(),
        _ => false,
    }
}

impl Formula {
    /// Parse one formulae.brew.sh formula object.
    pub fn from_api(v: &Value) -> Result<Formula, String> {
        let name = s(v.get("name"));
        if name.is_empty() {
            return Err("abrew: formula JSON has no name".to_string());
        }
        let versions = v.get("versions");
        let stable = v.pointer("/urls/stable");
        let mut bottle_files = Vec::new();
        let mut native_bottle_tags = Vec::new();
        let no_conditions = matches!(
            v.get("post_install_defined"),
            None | Some(Value::Bool(false))
        ) && v
            .get("pour_bottle_only_if")
            .is_none_or(|value| *value == Value::Null)
            && empty_metadata(v.get("post_install_steps"))
            && empty_metadata(v.get("requirements"))
            && empty_metadata(v.get("conflicts_with"))
            && empty_metadata(v.get("variations"));
        if let Some(files) = v.pointer("/bottle/stable/files")
            && let Value::Object(map) = files
        {
            for (tag, f) in map {
                bottle_files.push((tag.clone(), s(f.get("url")), s(f.get("sha256"))));
                if no_conditions
                    && f.get("cellar").and_then(Value::as_str) == Some(":any_skip_relocation")
                {
                    native_bottle_tags.push(tag.clone());
                }
            }
        }
        bottle_files.sort();
        Ok(Formula {
            name: name.clone(),
            full_name: {
                let f = s(v.get("full_name"));
                if f.is_empty() { name } else { f }
            },
            desc: s(v.get("desc")),
            homepage: s(v.get("homepage")),
            license: s(v.get("license")),
            version: s(versions.and_then(|x| x.get("stable"))),
            revision: v.get("revision").and_then(Value::as_u64).unwrap_or(0),
            stable_url: s(stable.and_then(|x| x.get("url"))),
            stable_sha256: s(stable.and_then(|x| x.get("checksum"))),
            bottle_files,
            native_bottle_tags,
            dependencies: str_list(v.get("dependencies")),
            build_dependencies: str_list(v.get("build_dependencies")),
            keg_only: v.get("keg_only").and_then(Value::as_bool).unwrap_or(false),
            deprecate: v
                .get("deprecated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            disable: v.get("disabled").and_then(Value::as_bool).unwrap_or(false),
            caveats: s(v.get("caveats")),
            aliases: str_list(v.get("aliases")),
            oldnames: str_list(v.get("oldnames")),
        })
    }

    /// The bottle file for this machine, best tag first (design §3):
    /// the first compatible OS tag supplied by the caller, then `all`.
    #[must_use]
    pub fn bottle_for(&self, tags: &[String]) -> Option<&(String, String, String)> {
        for want in tags {
            if let Some(f) = self.bottle_files.iter().find(|(t, _, _)| t == want) {
                return Some(f);
            }
        }
        self.bottle_files.iter().find(|(t, _, _)| t == "all")
    }

    /// `name`'s short form without a tap prefix.
    #[must_use]
    pub fn short_name(full: &str) -> &str {
        full.rsplit('/').next().unwrap_or(full)
    }
}

/// A cask as abrew needs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cask {
    pub token: String,
    pub deprecated: bool,
    pub disabled: bool,
    pub name: String,
    pub desc: String,
    pub homepage: String,
    pub version: String,
    pub url: String,
    /// `None` when the cask says `no_check`.
    pub sha256: Option<String>,
    pub artifacts: Vec<Artifact>,
    pub auto_updates: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Artifact {
    App(String),
    Binary(String),
    Font(String),
    Pkg(String),
    Manpage(String),
    Other(String),
}

impl Cask {
    /// Parse one formulae.brew.sh cask object.
    pub fn from_api(v: &Value) -> Result<Cask, String> {
        let token = s(v.get("token"));
        if token.is_empty() {
            return Err("abrew: cask JSON has no token".to_string());
        }
        let sha_raw = s(v.get("sha256"));
        let mut artifacts = Vec::new();
        if let Some(arr) = v.get("artifacts").and_then(Value::as_array) {
            for a in arr {
                if let Value::Object(map) = a {
                    for (kind, vals) in map {
                        let simple = vals
                            .as_array()
                            .filter(|items| items.len() == 1)
                            .and_then(|items| items.first())
                            .and_then(Value::as_str);
                        let Some(first) = simple else {
                            artifacts.push(Artifact::Other(format!(
                                "{kind}: unsupported artifact options"
                            )));
                            continue;
                        };
                        let first = first.to_owned();
                        artifacts.push(match kind.as_str() {
                            "app" => Artifact::App(first),
                            "binary" => Artifact::Binary(first),
                            "font" => Artifact::Font(first),
                            "pkg" => Artifact::Pkg(first),
                            "manpage" => Artifact::Manpage(first),
                            other => Artifact::Other(format!("{other}:{first}")),
                        });
                    }
                }
            }
        }
        for field in ["depends_on", "conflicts_with", "container", "variations"] {
            if !empty_metadata(v.get(field)) {
                artifacts.push(Artifact::Other(format!("unsupported {field}")));
            }
        }
        let sha256 = if sha_raw == "no_check" {
            None
        } else if sha_raw.len() == 64 && sha_raw.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(sha_raw)
        } else {
            // Absent or malformed is not `no_check`: never install
            // unverified bytes on a truncated API field.
            return Err(format!(
                "abrew: cask {token} has no usable sha256 (missing or malformed)"
            ));
        };
        let names = str_list(v.get("name"));
        Ok(Cask {
            deprecated: v
                .get("deprecated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            disabled: v.get("disabled").and_then(Value::as_bool).unwrap_or(false),
            token,
            name: names.into_iter().next().unwrap_or_default(),
            desc: s(v.get("desc")),
            homepage: s(v.get("homepage")),
            version: s(v.get("version")),
            url: s(v.get("url")),
            sha256,
            artifacts,
            auto_updates: v
                .get("auto_updates")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// Compatible bottle tags for this machine. An unknown macOS version admits
/// only the formula's `all` fallback; it never guesses a newer OS requirement.
#[must_use]
pub fn bottle_tags() -> Vec<String> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        _ => return Vec::new(),
    };
    if cfg!(target_os = "linux") {
        return vec![format!("{arch}_linux")];
    }
    #[cfg(target_os = "macos")]
    {
        let major = super::fetch::run_capture("/usr/bin/sw_vers", &["-productVersion"])
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|version| version.trim().split('.').next()?.parse().ok());
        macos_bottle_tags(arch, major)
    }
    #[cfg(not(target_os = "macos"))]
    Vec::new()
}

#[cfg(any(target_os = "macos", test))]
fn macos_bottle_tags(arch: &str, major: Option<u32>) -> Vec<String> {
    let Some(major) = major else {
        return Vec::new();
    };
    let prefix = match arch {
        "arm64" => "arm64_",
        "x86_64" => "",
        _ => return Vec::new(),
    };
    [
        (26, "tahoe"),
        (15, "sequoia"),
        (14, "sonoma"),
        (13, "ventura"),
        (12, "monterey"),
        (11, "big_sur"),
    ]
    .into_iter()
    .filter(|(minimum, _)| *minimum <= major)
    .map(|(_, release)| format!("{prefix}{release}"))
    .collect()
}

/// Homebrew-shaped version comparison (design §6): dotted tokens,
/// numeric tokens compare as numbers and outrank alpha tokens at the
/// same slot except pre-release words (alpha < beta < rc < release),
/// `_N`/`-N` revisions compare last. Exotic strings fall back to a
/// tokenwise compare that never claims equality spuriously.
#[must_use]
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let (a_main, a_rev) = split_revision(a);
    let (b_main, b_rev) = split_revision(b);
    let at = tokens(&a_main);
    let bt = tokens(&b_main);
    for i in 0..at.len().max(bt.len()) {
        let x = at.get(i).map_or("", String::as_str);
        let y = bt.get(i).map_or("", String::as_str);
        let ord = token_cmp(x, y);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a_rev.cmp(&b_rev)
}

fn split_revision(v: &str) -> (String, u64) {
    for sep in ['_', '-'] {
        if let Some((head, tail)) = v.rsplit_once(sep)
            && let Ok(n) = tail.parse::<u64>()
            && tail.chars().all(|c| c.is_ascii_digit())
            && !head.is_empty()
        {
            return (head.to_string(), n);
        }
    }
    (v.to_string(), 0)
}

fn tokens(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut digits: Option<bool> = None;
    for c in v.chars() {
        if matches!(c, '.' | '-' | '_' | '+') {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            digits = None;
            continue;
        }
        let d = c.is_ascii_digit();
        if digits.is_some_and(|was| was != d) && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        digits = Some(d);
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn prerelease_rank(t: &str) -> Option<u8> {
    match t.to_ascii_lowercase().as_str() {
        "alpha" | "a" => Some(0),
        "beta" | "b" => Some(1),
        "pre" | "preview" | "rc" => Some(2),
        _ => None,
    }
}

fn token_cmp(x: &str, y: &str) -> Ordering {
    if x == y {
        return Ordering::Equal;
    }
    let xn = x.parse::<u128>().ok();
    let yn = y.parse::<u128>().ok();
    match (xn, yn) {
        (Some(a), Some(b)) => a.cmp(&b),
        // An empty slot is a release: it outranks a pre-release word
        // (1.0 > 1.0rc1) and equals a zero (1.0 == 1.0.0).
        (None, Some(0)) | (Some(0), None) if x.is_empty() || y.is_empty() => Ordering::Equal,
        (Some(_), None) => {
            if y.is_empty() || prerelease_rank(y).is_some() {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (None, Some(_)) => {
            if x.is_empty() || prerelease_rank(x).is_some() {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (None, None) => match (prerelease_rank(x), prerelease_rank(y)) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => x.cmp(y),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottles_never_require_a_newer_os_or_another_architecture() {
        let newer = ("arm64_tahoe".into(), "newer".into(), "hash".into());
        let older = ("arm64_sonoma".into(), "older".into(), "hash".into());
        let universal = ("all".into(), "universal".into(), "hash".into());
        let mut formula = Formula {
            bottle_files: vec![newer, older, universal],
            ..Formula::default()
        };
        assert_eq!(
            formula
                .bottle_for(&macos_bottle_tags("arm64", Some(14)))
                .unwrap()
                .1,
            "older"
        );
        assert_eq!(
            formula
                .bottle_for(&macos_bottle_tags("arm64", Some(26)))
                .unwrap()
                .1,
            "newer"
        );
        assert_eq!(
            formula
                .bottle_for(&macos_bottle_tags("arm64", None))
                .unwrap()
                .1,
            "universal"
        );
        assert_eq!(macos_bottle_tags("x86_64", Some(14))[0], "sonoma");
        assert!(macos_bottle_tags("arm64", Some(10)).is_empty());
        assert!(macos_bottle_tags("unknown", Some(26)).is_empty());
        formula.bottle_files.pop();
        assert!(
            formula
                .bottle_for(&macos_bottle_tags("arm64", None))
                .is_none()
        );
    }

    #[test]
    fn version_ordering_common_shapes() {
        assert_eq!(version_cmp("1.2.3", "1.2.3"), Ordering::Equal);
        assert_eq!(version_cmp("1.10.0", "1.9.9"), Ordering::Greater);
        assert_eq!(version_cmp("1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(version_cmp("2.0rc1", "2.0"), Ordering::Less);
        assert_eq!(version_cmp("2.0beta2", "2.0rc1"), Ordering::Less);
        assert_eq!(version_cmp("1.2.3_1", "1.2.3"), Ordering::Greater);
        assert_eq!(version_cmp("2024.01", "2023.12"), Ordering::Greater);
    }

    #[test]
    fn native_bottles_require_complete_supported_api_metadata() {
        let mut value: Value = aterm_json::from_str(r#"{"name":"demo","bottle":{"stable":{"files":{"all":{"url":"https://example.invalid/demo.tar.gz","sha256":"hash","cellar":":any_skip_relocation"}}}}}"#).unwrap();
        assert_eq!(
            Formula::from_api(&value).unwrap().native_bottle_tags,
            ["all"]
        );
        for unsupported in [
            r#"{"name":"demo","post_install_defined":true,"bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
            r#"{"name":"demo","requirements":[{"name":"macos"}],"bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
            r#"{"name":"demo","variations":{"arm64_sonoma":{}},"bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
            r#"{"name":"demo","pour_bottle_only_if":"condition","bottle":{"stable":{"files":{"all":{"cellar":":any_skip_relocation"}}}}}"#,
        ] {
            value = aterm_json::from_str(unsupported).unwrap();
            assert!(
                Formula::from_api(&value)
                    .unwrap()
                    .native_bottle_tags
                    .is_empty()
            );
        }
        for cellar in [":any", "/opt/homebrew/Cellar", ""] {
            value = aterm_json::from_str(&format!(r#"{{"name":"demo","bottle":{{"stable":{{"files":{{"all":{{"cellar":"{cellar}"}}}}}}}}}}"#)).unwrap();
            assert!(
                Formula::from_api(&value)
                    .unwrap()
                    .native_bottle_tags
                    .is_empty()
            );
        }
    }

    #[test]
    fn cask_options_and_constraints_survive_as_unsupported_work() {
        let simple: Value =
            aterm_json::from_str(r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","artifacts":[{"app":["Demo.app"]}]}"#).unwrap();
        assert_eq!(
            Cask::from_api(&simple).unwrap().artifacts,
            [Artifact::App("Demo.app".into())]
        );
        for json in [
            r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","artifacts":[{"app":["Demo.app",{"target":"Other.app"}]}]}"#,
            r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","artifacts":[{"binary":["demo",{"target":"other"}]}]}"#,
            r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","depends_on":{"macos":[">= :sonoma"]}}"#,
            r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","conflicts_with":{"cask":["other"]}}"#,
            r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","container":{"type":"dmg"}}"#,
            r#"{"token":"demo","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","variations":{"arm64_sonoma":{"url":"different"}}}"#,
        ] {
            let value: Value = aterm_json::from_str(json).unwrap();
            assert!(
                Cask::from_api(&value)
                    .unwrap()
                    .artifacts
                    .iter()
                    .any(|artifact| matches!(artifact, Artifact::Other(_)))
            );
        }
        // Only the literal `no_check` waives verification; a missing or
        // malformed sha256 refuses the document instead of installing
        // unverified bytes.
        for json in [
            r#"{"token":"demo","artifacts":[{"app":["Demo.app"]}]}"#,
            r#"{"token":"demo","sha256":"short","artifacts":[{"app":["Demo.app"]}]}"#,
        ] {
            let value: Value = aterm_json::from_str(json).unwrap();
            assert!(Cask::from_api(&value).is_err());
        }
        let waived: Value =
            aterm_json::from_str(r#"{"token":"demo","sha256":"no_check","artifacts":[]}"#).unwrap();
        assert_eq!(Cask::from_api(&waived).unwrap().sha256, None);
    }

    #[test]
    fn formula_parse_from_api_shape() {
        let v: Value = aterm_json::from_str(
            r#"{"name":"wget","full_name":"wget","desc":"Get","homepage":"https://x","license":"GPL-3.0",
                "versions":{"stable":"1.25.0"},"urls":{"stable":{"url":"https://y/wget.tar.gz","checksum":"abc"}},
                "bottle":{"stable":{"files":{"arm64_tahoe":{"url":"https://ghcr/b","sha256":"def"}}}},
                "dependencies":["openssl@3"],"build_dependencies":["pkgconf"],"keg_only":false}"#,
        )
        .unwrap();
        let f = Formula::from_api(&v).unwrap();
        assert_eq!(f.version, "1.25.0");
        assert_eq!(f.dependencies, ["openssl@3"]);
        assert_eq!(f.bottle_for(&["arm64_tahoe".to_string()]).unwrap().2, "def");
    }
}
