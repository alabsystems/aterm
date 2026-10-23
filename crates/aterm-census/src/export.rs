// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Export file [`Selection`] and text [`rewrite`] transforms for publication content guards.

use aterm_regex::Regex;

/// The export's allowlist, relative to the checkout.
pub const MANIFEST: &str = "publish/manifest.txt";

/// The top-level paths the engine withholds from every export whatever a manifest says.
pub const CENTRAL_DENY: &[&str] = &["docs", "publish", ".github"];

/// The export's file selection.
#[derive(Debug)]
pub struct Selection {
    include: Vec<Spec>,
    exclude: Vec<Spec>,
}

#[derive(Debug)]
enum Spec {
    Root,
    Literal(String),
    Glob(Regex),
}

impl Spec {
    fn new(text: &str) -> Result<Self, String> {
        if text.starts_with('/') {
            return Err(format!(
                "{text:?} is an absolute pathspec; spell `.` for the checkout root"
            ));
        }
        let text = text.trim_start_matches("./").trim_end_matches('/');
        if text.is_empty() || text == "." {
            return Ok(Self::Root);
        }
        if text.contains(['[', '\\']) || text.starts_with(':') {
            return Err(format!(
                "{text:?} is a pathspec this guard does not read (a class, an escape or \
                 git magic), so it cannot say what the export selects"
            ));
        }
        if !text.contains(['*', '?']) {
            return Ok(Self::Literal(text.to_string()));
        }
        let mut pattern = String::from("(?s)^");
        for c in text.chars() {
            match c {
                '*' => pattern.push_str(".*"),
                '?' => pattern.push('.'),
                c => pattern.push_str(&aterm_regex::escape(c.encode_utf8(&mut [0; 4]))),
            }
        }
        pattern.push('$');
        Regex::new(&pattern)
            .map(Self::Glob)
            .map_err(|e| format!("{text:?}: {e}"))
    }

    fn matches(&self, rel: &str) -> bool {
        match self {
            Self::Root => true,
            Self::Literal(spec) => under(rel, spec),
            Self::Glob(re) => re.is_match(rel),
        }
    }
}

/// Whether `rel` is the literal path `spec` or under it.
fn under(rel: &str, spec: &str) -> bool {
    rel.strip_prefix(spec)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

impl Selection {
    /// Parses the manifest's lines (`#` to the end of a line is a comment).
    pub fn parse(manifest: &str) -> Result<Self, (usize, String)> {
        let mut include = Vec::new();
        let mut exclude: Vec<Spec> = CENTRAL_DENY
            .iter()
            .map(|d| Spec::Literal((*d).to_string()))
            .collect();
        for (i, raw) in manifest.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (excluding, text) = match line.strip_prefix('!') {
                Some(body) => (true, body.trim()),
                None => (false, line),
            };
            let spec = text.strip_prefix("./").unwrap_or(text);
            if spec.is_empty() {
                return Err((
                    i + 1,
                    format!(
                        "{line:?} names nothing once the engine strips its `./`, so the engine \
                         refuses it (a bare `!`, or an empty pathspec git refuses)"
                    ),
                ));
            }
            let spec = Spec::new(text).map_err(|why| (i + 1, why))?;
            if excluding {
                exclude.push(spec);
            } else {
                include.push(spec);
            }
        }
        Ok(Self { include, exclude })
    }

    /// Every path outside the central denies.
    #[must_use]
    pub fn everything() -> Self {
        Self::parse("*").unwrap_or_else(|_| unreachable!("`*` is a pathspec this reads"))
    }

    /// Whether the export carries the checkout's file at `rel` (`/`-separated).
    #[must_use]
    pub fn selects(&self, rel: &str) -> bool {
        self.include.iter().any(|s| s.matches(rel)) && !self.exclude.iter().any(|s| s.matches(rel))
    }
}

/// The developer account the export renames.
const ACCOUNT: &str = concat!("aya", "tes");

/// The private org, which becomes the public one.
const PRIVATE_ORG: &str = concat!("andrew", "yatesai");

/// The toolchain checkouts a home path names.
const TILDE_TRUST: &str = concat!("~", "/trust");
const TILDE_CLEAN: &str = concat!("~", "/clean");

/// Replace every `from` in `text` with `to`.
fn replace_all(text: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(text[i]);
            i += 1;
        }
    }
    out
}

/// The export's text rewrites.
#[must_use]
pub fn rewrite(text: &[u8]) -> Vec<u8> {
    let home = format!("/Users/{ACCOUNT}");
    let at = format!("{ACCOUNT}@");
    let mut t = replace_all(text, PRIVATE_ORG.as_bytes(), b"alabsystems");
    t = replace_all(&t, home.as_bytes(), b"/Users//example");
    let mut doubled = Vec::with_capacity(t.len());
    let mut i = 0;
    while i < t.len() {
        if t[i..].starts_with(b"/Users/") && t.get(i + 7).is_some_and(u8::is_ascii_alphabetic) {
            doubled.extend_from_slice(b"/Users//");
            i += 7;
        } else {
            doubled.push(t[i]);
            i += 1;
        }
    }
    t = replace_all(&doubled, TILDE_TRUST.as_bytes(), b"$HOME/trust");
    t = replace_all(&t, TILDE_CLEAN.as_bytes(), b"$HOME/clean");
    replace_all(&t, at.as_bytes(), b"user@")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rewrites_take_exactly_the_guards_shapes() {
        let acct = ACCOUNT;
        let case = |input: String, want: &str| {
            assert_eq!(
                String::from_utf8(rewrite(input.as_bytes())).unwrap(),
                want,
                "{input}"
            );
        };
        case(
            format!("https://github.com/{PRIVATE_ORG}/aterm"),
            "https://github.com/alabsystems/aterm",
        );
        case(
            format!("/Users/{acct}/aterm/target"),
            "/Users//example/aterm/target",
        );
        case("/Users//someone/Library".into(), "/Users//someone/Library");
        case(format!("{TILDE_TRUST}/build"), "$HOME/trust/build");
        case(format!("{TILDE_CLEAN}/bin"), "$HOME/clean/bin");
        case(format!("b\"{acct}@mac ~ % \""), "b\"user@mac ~ % \"");
        case(
            "path.starts_with(\"/Users/\")".into(),
            "path.starts_with(\"/Users/\")",
        );
        case("version = \"0.104.7\"".into(), "version = \"0.104.7\"");
    }

    #[test]
    fn the_selection_reads_the_manifest_as_git_reads_pathspecs() {
        let s = Selection::parse(
            "# head\nREADME.md\ncrates # the crates\ndocs\n!crates/x/proofs\n\
             !crates/x/perf-*.tsv\n!crates/x/s2_??.toml\n!vendor/*/.github\nvendor\n",
        )
        .expect("parses");
        for (rel, want) in [
            ("README.md", true),
            ("README.mdx", false),
            ("crates/x/src/lib.rs", true),
            ("cratesx/lib.rs", false),
            ("docs/a.md", false),
            ("crates/x/proofs/a.json", false),
            ("crates/x/proofsx/a.json", true),
            ("crates/x/perf-a.tsv", false),
            ("crates/x/sub/perf-a.tsv", true),
            ("crates/x/s2_01.toml", false),
            ("crates/x/s2_011.toml", true),
            ("vendor/y/.github", false),
            ("vendor/y/.github/w.yml", true),
            ("publish/manifest.txt", false),
            ("tools/install.sh", false),
        ] {
            assert_eq!(s.selects(rel), want, "{rel}");
        }
        assert!(Selection::everything().selects("tools/install.sh"));
        assert!(!Selection::everything().selects(".github/workflows/a.yml"));
        for root in [".", "././", "././."] {
            let selected = Selection::parse(&format!("{root}\n!./crates/x/proofs/\n"))
                .expect("root include with a directory exclusion");
            for (rel, want) in [
                ("README.md", true),
                (".root", true),
                ("tools/install.sh", true),
                ("crates/x/src/lib.rs", true),
                ("crates/x/proofs/a.json", false),
                ("docs/a.md", false),
                ("publish/manifest.txt", false),
                (".github/workflows/a.yml", false),
            ] {
                assert_eq!(selected.selects(rel), want, "{root}: {rel}");
            }
        }
        for root in [".", "././", "././."] {
            let excluded = Selection::parse(&format!("*\n!{root}\n")).expect("root exclusion");
            for rel in ["README.md", ".root", "crates/x/src/lib.rs"] {
                assert!(!excluded.selects(rel), "{root}: {rel}");
            }
        }
        assert_eq!(Selection::parse("/\n").unwrap_err().0, 1);
        assert_eq!(
            Selection::parse("crates\n!crates/[ab]\n")
                .map(|_| ())
                .unwrap_err()
                .0,
            2
        );
    }

    #[test]
    fn a_line_the_engine_refuses_is_refused() {
        for (manifest, line) in [
            ("crates\n!\n", 2),
            ("crates\n! \n", 2),
            ("crates\n!./\n", 2),
            ("crates\n! ./ # the root\n", 2),
            ("./\n", 1),
        ] {
            assert_eq!(
                Selection::parse(manifest).map(|_| ()).unwrap_err().0,
                line,
                "{manifest:?}"
            );
        }
        for manifest in [
            "crates\n!.\n",
            "crates\n!././\n",
            "././docs\n",
            "docsx\n",
            "",
        ] {
            assert!(Selection::parse(manifest).is_ok(), "{manifest:?}");
        }
    }
}
