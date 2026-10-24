// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The files a deploy uploads: `public/`, less what `hosting.ignore` names, each as the
//! gzip member [`aterm_png::gzip`] writes and the SHA-256 of that member.

use std::collections::BTreeMap;
use std::path::Path;

/// Characters an ignore pattern may not hold: minimatch reads each as syntax (a class, a
/// brace set, an extglob, a negation, an escape) that this matcher does not implement, so a
/// pattern with one is refused by name rather than matched otherwise than firebase-tools would.
const UNSUPPORTED: [char; 8] = ['[', ']', '{', '}', '(', ')', '!', '\\'];

/// One file of the site as the version serves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// Its path on the site: `/` and its path under `public/`.
    pub path: String,
    /// Its length before compression.
    pub bytes: u64,
    /// The gzip member uploaded for it.
    pub gz: Vec<u8>,
    /// The lowercase hex SHA-256 of [`File::gz`], which `populateFiles` names it by.
    pub hash: String,
}

impl File {
    /// `data`, served at `path`.
    #[must_use]
    pub fn new(path: String, data: &[u8]) -> File {
        let gz = aterm_png::gzip(data);
        let hash = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&gz));
        File {
            path,
            bytes: data.len() as u64,
            gz,
            hash,
        }
    }
}

/// A pattern of `hosting.ignore`, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern(Vec<String>);

impl Pattern {
    /// `text` as a pattern relative to `public/`: segments of literal characters, `*`
    /// (any run within one segment) and `?` (one character), or a whole `**` segment (any
    /// number of segments). Every pattern matches dot files, as firebase-tools' ignore does.
    /// # Errors
    /// An empty or absolute pattern, or one holding a character of [`UNSUPPORTED`].
    pub fn new(text: &str) -> Result<Pattern, String> {
        if text.is_empty() {
            return Err("hosting.ignore holds an empty pattern".into());
        }
        if text.starts_with('/') {
            return Err(format!(
                "hosting.ignore pattern {text:?} is absolute: patterns are relative to public/"
            ));
        }
        if let Some(c) = text.chars().find(|c| UNSUPPORTED.contains(c)) {
            return Err(format!(
                "hosting.ignore pattern {text:?} uses {c:?}, which ship site does not match \
                 (it reads *, ? and **)"
            ));
        }
        Ok(Pattern(
            text.trim_end_matches('/')
                .split('/')
                .map(str::to_string)
                .collect(),
        ))
    }

    /// Whether the pattern matches `path`, a `/`-joined path under `public/`.
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        let pattern: Vec<&str> = self.0.iter().map(String::as_str).collect();
        segments(&pattern, &parts)
    }

    /// Whether the pattern ignores everything under the directory `dir`: it is `X/**` and
    /// `X` matches `dir` (glob's rule for an ignore pattern that ends in `/**`).
    #[must_use]
    pub fn prunes(&self, dir: &str) -> bool {
        match self.0.split_last() {
            Some((last, head)) if last == "**" && !head.is_empty() => {
                let parts: Vec<&str> = dir.split('/').collect();
                let head: Vec<&str> = head.iter().map(String::as_str).collect();
                segments(&head, &parts)
            }
            _ => false,
        }
    }
}

/// Whether `pattern`'s segments match `path`'s.
fn segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| segments(rest, &path[skip..])),
        Some((first, rest)) => path.split_first().is_some_and(|(seg, more)| {
            segment(first.as_bytes(), seg.as_bytes()) && segments(rest, more)
        }),
    }
}

/// Whether one pattern segment (`*` and `?` its only wildcards) matches one path segment.
fn segment(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|skip| segment(rest, &name[skip..])),
        Some((b'?', rest)) => {
            // `?` is one character: one UTF-8 scalar, never half of one.
            let width = std::str::from_utf8(name)
                .ok()
                .and_then(|s| s.chars().next())
                .map_or(0, char::len_utf8);
            width > 0 && segment(rest, &name[width..])
        }
        Some((c, rest)) => name.first() == Some(c) && segment(rest, &name[1..]),
    }
}

/// `patterns`, each checked.
/// # Errors
/// The first [`Pattern::new`] refuses.
pub fn patterns(patterns: &[String]) -> Result<Vec<Pattern>, String> {
    patterns.iter().map(|p| Pattern::new(p)).collect()
}

/// Every file under `public` that `ignore` leaves, by its path under `public/`, sorted, with
/// its bytes: `overlay`'s for a path it names (a dry run's planned content, which the
/// listing also gains where the file does not exist yet), else the file's own.
/// # Errors
/// A directory or file that cannot be read; a symbolic link, or anything else that is not a
/// regular file or directory (a deploy never follows a link out of `public/`); a name that is
/// not UTF-8; a credential ([`super::key::holds_a_private_key`]), which the site would serve
/// to anyone.
pub fn list(
    public: &Path,
    ignore: &[Pattern],
    overlay: &BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let ignored = |rel: &str| ignore.iter().any(|p| p.matches(rel));
    let mut found = BTreeMap::new();
    let mut dirs = vec![String::new()];
    while let Some(dir) = dirs.pop() {
        let at = if dir.is_empty() {
            public.to_path_buf()
        } else {
            public.join(&dir)
        };
        let entries =
            std::fs::read_dir(&at).map_err(|e| format!("cannot list {}: {e}", at.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot list {}: {e}", at.display()))?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                format!(
                    "{} has a name that is not UTF-8: a site path is text",
                    entry.path().display()
                )
            })?;
            let rel = if dir.is_empty() {
                name.to_string()
            } else {
                format!("{dir}/{name}")
            };
            let kind = entry
                .file_type()
                .map_err(|e| format!("cannot read {}: {e}", entry.path().display()))?;
            if kind.is_dir() {
                if !ignore.iter().any(|p| p.prunes(&rel)) {
                    dirs.push(rel);
                }
                continue;
            }
            if ignored(&rel) {
                continue;
            }
            if kind.is_file() {
                let bytes = match overlay.get(&rel) {
                    Some(bytes) => bytes.clone(),
                    None => std::fs::read(entry.path())
                        .map_err(|e| format!("cannot read {}: {e}", entry.path().display()))?,
                };
                if super::key::holds_a_private_key(&bytes) {
                    return Err(format!(
                        "{} is a private key (a JSON key with a private_key, such as the \
                         deployer's service-account key): the site would serve it to anyone — \
                         move it out of public/ or name it in firebase.json's hosting.ignore",
                        entry.path().display()
                    ));
                }
                found.insert(rel, bytes);
            } else {
                return Err(format!(
                    "{} is not a regular file (a symbolic link or a special file): ship site \
                     deploys the files under public/ themselves",
                    entry.path().display()
                ));
            }
        }
    }
    for (rel, bytes) in overlay {
        if !ignored(rel) {
            found.entry(rel.clone()).or_insert_with(|| bytes.clone());
        }
    }
    Ok(found)
}

/// `listed` as the version's files.
#[must_use]
pub fn files(listed: BTreeMap<String, Vec<u8>>) -> Vec<File> {
    listed
        .into_iter()
        .map(|(rel, bytes)| File::new(format!("/{rel}"), &bytes))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Pattern {
        Pattern::new(text).unwrap()
    }

    /// The site's own ignore list and firebase-tools' always-ignored names match as glob's
    /// ignore does (dot files included). NEGATIVE CONTROL: each also leaves the paths it
    /// must not take, so a matcher that took everything fails.
    #[test]
    fn the_ignore_patterns_match_as_firebase_tools_ignores() {
        let cases: [(&str, &[&str], &[&str]); 6] = [
            (
                "firebase.json",
                &["firebase.json"],
                &["assets/firebase.json", "firebase.jsonx"],
            ),
            (
                "**/.*",
                &[".DS_Store", "assets/.hidden", ".well-known"],
                &["index.html", ".well-known/assetlinks.json", "a.b"],
            ),
            (
                "**/node_modules/**",
                &["node_modules/x.js", "a/node_modules/b/c.js", "node_modules"],
                &["a/node_modules_x/c.js", "node_modulesx/c.js"],
            ),
            (
                "**/firebase-debug.*.log",
                &["firebase-debug.1.log", "a/firebase-debug.x.log"],
                &["firebase-debug.log", "a/firebase-debug.log.txt"],
            ),
            (
                ".firebase/*",
                &[".firebase/hosting.cache"],
                &[".firebase/a/b", "x/.firebase/y"],
            ),
            (
                "assets/term/aterm_wasm.?.js",
                &["assets/term/aterm_wasm.a.js", "assets/term/aterm_wasm.é.js"],
                &["assets/term/aterm_wasm.ab.js", "assets/term/aterm_wasm..js"],
            ),
        ];
        for (pattern, hits, misses) in cases {
            let pat = p(pattern);
            for hit in hits {
                assert!(pat.matches(hit), "{pattern} matches {hit}");
            }
            for miss in misses {
                assert!(!pat.matches(miss), "{pattern} leaves {miss}");
            }
        }
        assert!(p("**/node_modules/**").prunes("a/node_modules"));
        assert!(!p("**/node_modules/**").prunes("a/node"));
        assert!(!p("**/.*").prunes(".well-known"), "only X/** prunes");
    }

    /// A pattern this matcher does not implement is refused by name, never matched some
    /// other way. NEGATIVE CONTROL: the patterns it implements are accepted.
    #[test]
    fn an_unsupported_pattern_is_refused() {
        for bad in [
            "**/*.{js,css}",
            "[ab].html",
            "!keep.html",
            "@(a|b)",
            "/abs",
            "",
            "a\\*",
        ] {
            assert!(Pattern::new(bad).is_err(), "{bad:?} refused");
        }
        for good in ["*", "**", "a/**/b?.html", "**/.*"] {
            assert!(Pattern::new(good).is_ok(), "{good:?} accepted");
        }
    }

    /// The listing walks `public/`, prunes an ignored directory, skips ignored files, refuses a
    /// symbolic link, and lays a dry run's planned content over what is on disk.
    /// NEGATIVE CONTROL: with no ignore list every file is listed, the dot file included.
    #[cfg(unix)]
    #[test]
    fn the_listing_leaves_what_ignore_names_and_refuses_a_link() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-hosting-list-")
            .tempdir()
            .unwrap();
        let public = dir.path().join("public");
        for (rel, text) in [
            ("index.html", "home"),
            (".DS_Store", "junk"),
            ("assets/a.css", "css"),
            ("node_modules/pkg/x.js", "dep"),
            ("firebase.json", "{}"),
        ] {
            let path = public.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let ignore = patterns(&[
            "firebase.json".into(),
            "**/.*".into(),
            "**/node_modules/**".into(),
        ])
        .unwrap();
        let mut overlay = BTreeMap::new();
        overlay.insert("index.html".to_string(), b"planned".to_vec());
        overlay.insert("releases.html".to_string(), b"new".to_vec());
        let listed = list(&public, &ignore, &overlay).unwrap();
        assert_eq!(
            listed.keys().collect::<Vec<_>>(),
            ["assets/a.css", "index.html", "releases.html"]
        );
        assert_eq!(listed["index.html"], b"planned");
        let all = list(&public, &[], &BTreeMap::new()).unwrap();
        assert_eq!(all.len(), 5, "{:?}", all.keys().collect::<Vec<_>>());
        std::os::unix::fs::symlink("/etc/hosts", public.join("hosts")).unwrap();
        let why = list(&public, &ignore, &BTreeMap::new()).unwrap_err();
        assert!(
            why.contains("hosts") && why.contains("not a regular file"),
            "{why}"
        );
    }

    /// A service-account key under `public/` is refused by name, wherever it sits and whatever
    /// it is called, and its bytes are never quoted; an ignored one is never read. NEGATIVE
    /// CONTROL: JSON that only mentions `private_key`, or holds one that is not a string, is
    /// listed.
    #[test]
    fn a_key_under_public_is_never_published() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-hosting-key-")
            .tempdir()
            .unwrap();
        let public = dir.path().join("public");
        std::fs::create_dir_all(public.join("assets")).unwrap();
        std::fs::write(public.join("index.html"), "home").unwrap();
        std::fs::write(
            public.join("notes.json"),
            r#"{"doc": "a key's \"private_key\" field never ships", "private_key": 7}"#,
        )
        .unwrap();
        let listed = list(&public, &[], &BTreeMap::new()).unwrap();
        assert_eq!(
            listed.keys().collect::<Vec<_>>(),
            ["index.html", "notes.json"]
        );
        let key = super::super::key::tests::key_json("https://oauth2.googleapis.com/token");
        std::fs::write(public.join("assets/deployer.txt"), &key).unwrap();
        let why = list(&public, &[], &BTreeMap::new()).unwrap_err();
        assert!(
            why.contains("deployer.txt") && why.contains("private key"),
            "{why}"
        );
        let body = aterm_codec::base64::encode(super::super::key::tests::TEST_PKCS8).unwrap();
        assert!(
            key.contains(&body[130..170]),
            "the probe is the key's own bytes"
        );
        assert!(!why.contains(&body[130..170]), "{why}");
        let ignored = list(
            &public,
            &patterns(&["assets/deployer.txt".into()]).unwrap(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(!ignored.contains_key("assets/deployer.txt"));
    }

    /// A file is named by the SHA-256 of its gzip member, and the member is the same bytes
    /// for the same content. NEGATIVE CONTROL: other content is another hash.
    #[test]
    fn a_file_is_named_by_the_hash_of_its_member() {
        let f = File::new("/index.html".into(), b"<p>x</p>");
        assert_eq!(f.bytes, 8);
        assert_eq!(
            f.hash,
            aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&f.gz))
        );
        assert_eq!(f, File::new("/index.html".into(), b"<p>x</p>"));
        assert_ne!(f.hash, File::new("/index.html".into(), b"<p>y</p>").hash);
    }
}
