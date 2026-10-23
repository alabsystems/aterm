// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The three pages that name a download.

use std::fmt;

use super::html_escape;
use super::releases::{Facts, LINUX_ARCHES, LinuxKind};
use super::toolchain::{Levels, MAC_ARCHES};

/// Where alab.systems serves the installer (`public/install.sh`).
pub const INSTALL_URL: &str = "https://alab.systems/install.sh";

/// The one command a Linux visitor runs, when `arch` has a signed executable.
#[must_use]
pub fn linux_install_command(facts: &Facts, arch: &str) -> Option<String> {
    let d = facts.linux(arch)?;
    if !matches!(d.kind, LinuxKind::SignedRaw { .. }) {
        return None;
    }
    let pipe = format!("curl -fsSL {INSTALL_URL} | bash");
    Some(if d.tag == facts.head {
        pipe
    } else {
        format!("{pipe} -s -- --version {}", d.version)
    })
}

/// The pages a sync rewrites, in the order it rewrites them.
pub const PAGES: [&str; 3] = ["index.html", "aterm.html", "terminal.html"];

/// A rewrite rule, named in a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// `<a class="download" href="…"` on `/` and `/aterm`.
    DownloadAnchor,
    /// `<p class="install-note"…><code>NAME</code> · <code>SIZE</code>`.
    NoteLine,
    /// `/aterm`'s `<a href="…">release notes</a>`.
    ReleaseNotesLink,
    /// `/terminal`'s `<a class="get download" href="…"`.
    HeaderAnchor,
    /// `/terminal`'s `<span class="title">aterm vX.Y.Z&nbsp;`: the version the page runs.
    HeaderTitle,
    /// The download anchor's whole opening tag on `/aterm` and `/terminal`.
    PlatformAnchor,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::DownloadAnchor => "the download anchor",
            Self::NoteLine => {
                "the note line (<p class=\"install-note\"…><code>NAME</code> · <code>SIZE</code>)"
            }
            Self::ReleaseNotesLink => "the 'release notes' link",
            Self::HeaderAnchor => "the header download anchor",
            Self::HeaderTitle => "the header title version (<span class=\"title\">aterm vX.Y.Z …)",
            Self::PlatformAnchor => "platform download anchor",
        })
    }
}

/// Why a page could not be rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageError {
    /// A rule matched `found` times, not once: the page changed shape.
    Shape {
        page: String,
        rule: Rule,
        found: usize,
    },
    /// A platform attribute appears twice on one anchor.
    Duplicate { page: String, attr: &'static str },
}

impl fmt::Display for PageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shape { page, rule, found } => write!(
                f,
                "{page}: expected exactly one match for {rule}, found {found} — the page \
                 changed shape; update the site rules (crates/aterm-release/src/site/pages.rs) \
                 alongside it"
            ),
            Self::Duplicate { page, attr } => write!(f, "{page}: duplicate {attr}"),
        }
    }
}

/// One match: the span of the part a rule replaces (an href, a version, a note body,
/// an opening tag).
#[derive(Debug, Clone, Copy)]
struct Hit {
    inner: (usize, usize),
}

/// `PREFIX` `[^"]*` `SUFFIX`, every non-overlapping match left to right; the inner
/// span is the quoted value.
fn quoted(text: &str, prefix: &str, suffix: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(prefix) {
        let start = from + at;
        let value = start + prefix.len();
        let Some(len) = text[value..].find('"') else {
            break;
        };
        let close = value + len;
        if text[close..].starts_with(suffix) {
            let end = close + suffix.len();
            hits.push(Hit {
                inner: (value, close),
            });
            from = end;
        } else {
            from = start + 1;
        }
    }
    hits
}

const ANCHOR: &str = "<a class=\"download\" href=\"";
const GET_ANCHOR: &str = "<a class=\"get download\" href=\"";

/// The download anchors' hrefs (`<a class="download" href="…"`).
fn anchors(text: &str) -> Vec<Hit> {
    quoted(text, ANCHOR, "\"")
}

/// `/terminal`'s header anchors' hrefs (`<a class="get download" href="…"`).
fn get_anchors(text: &str) -> Vec<Hit> {
    quoted(text, GET_ANCHOR, "\"")
}

/// `\d+` at `at`: its end.
fn digits(text: &str, at: usize) -> Option<usize> {
    let n = text[at..].bytes().take_while(u8::is_ascii_digit).count();
    (n > 0).then_some(at + n)
}

/// `\d+\.\d+\.\d+` at `at`: its end.
fn triple(text: &str, at: usize) -> Option<usize> {
    let a = digits(text, at)?;
    let b = digits(text, a + text[a..].strip_prefix('.').map(|_| 1)?)?;
    digits(text, b + text[b..].strip_prefix('.').map(|_| 1)?)
}

/// The note lines: `<p class="install-note"[^>]*>` then `<code>aterm[^<]*</code>`,
/// then optionally a version cell (an older page shape) and optionally a size cell.
/// Both are consumed, so the rewrite drops a version cell. The inner span is everything
/// after the `<p …>` opening tag.
fn notes(text: &str) -> Vec<Hit> {
    const OPEN: &str = "<p class=\"install-note\"";
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(OPEN) {
        let start = from + at;
        let matched = (|| {
            let gt = start + OPEN.len() + text[start + OPEN.len()..].find('>')?;
            let body = gt + 1;
            let rest = text[body..].strip_prefix("<code>aterm")?;
            let name_end = text.len() - rest.len();
            let close = name_end + text[name_end..].find('<')?;
            let mut end = close + text[close..].strip_prefix("</code>").map(|_| 7)?;
            if let Some(v) = text[end..].strip_prefix(" &middot; <code>") {
                let vs = text.len() - v.len();
                if let Some(ve) = triple(text, vs)
                    && text[ve..].starts_with("</code>")
                {
                    end = ve + 7;
                }
            }
            if let Some(s) = text[end..].strip_prefix(" &middot; <code>") {
                let mut at = text.len() - s.len();
                at += usize::from(text[at..].starts_with('~'));
                if let Some(mut n) = digits(text, at) {
                    if text[n..].starts_with('.')
                        && let Some(f) = digits(text, n + 1)
                    {
                        n = f;
                    }
                    for unit in ["&nbsp;MB</code>", "&nbsp;GB</code>"] {
                        if text[n..].starts_with(unit) {
                            end = n + unit.len();
                            break;
                        }
                    }
                }
            }
            Some((body, end))
        })();
        match matched {
            Some((body, end)) => {
                hits.push(Hit { inner: (body, end) });
                from = end;
            }
            None => from = start + 1,
        }
    }
    hits
}

/// `<span class="title">aterm v` `\d+\.\d+\.\d+` `&nbsp;`; the inner span is the
/// version.
fn titles(text: &str) -> Vec<Hit> {
    const OPEN: &str = "<span class=\"title\">aterm v";
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(OPEN) {
        let start = from + at;
        let v = start + OPEN.len();
        match triple(text, v).filter(|e| text[*e..].starts_with("&nbsp;")) {
            Some(ve) => {
                hits.push(Hit { inner: (v, ve) });
                from = ve + 6;
            }
            None => from = start + 1,
        }
    }
    hits
}

/// The download anchor's whole opening tag: `<a class="(get )?download" href="…"…>`.
fn platform_anchors(text: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut from = 0;
    loop {
        let next = [ANCHOR, GET_ANCHOR]
            .iter()
            .filter_map(|p| text[from..].find(p).map(|i| (from + i, p.len())))
            .min();
        let Some((start, len)) = next else {
            return hits;
        };
        let value = start + len;
        let tag_end = text[value..]
            .find('"')
            .map(|q| value + q + 1)
            .and_then(|after| text[after..].find('>').map(|g| after + g + 1));
        match tag_end {
            Some(end) => {
                hits.push(Hit {
                    inner: (start, end),
                });
                from = end;
            }
            None => from = start + 1,
        }
    }
}

/// `text` with the one hit of `hits` replaced through `with`, or the refusal.
fn exactly_once(
    page: &str,
    text: &str,
    rule: Rule,
    hits: &[Hit],
    with: impl FnOnce(&str) -> Result<String, PageError>,
) -> Result<String, PageError> {
    let [hit] = hits else {
        return Err(PageError::Shape {
            page: page.into(),
            rule,
            found: hits.len(),
        });
    };
    let (a, b) = hit.inner;
    Ok(format!(
        "{}{}{}",
        &text[..a],
        with(&text[a..b])?,
        &text[b..]
    ))
}

/// The platform attributes the `/aterm` and `/terminal` anchors carry, in the order they are
/// written, each with its value — `None` removes it.
#[must_use]
pub fn platform_attributes(
    facts: &Facts,
    toolchain: Option<&Levels>,
) -> Vec<(&'static str, Option<String>)> {
    let mut out: Vec<(&'static str, Option<String>)> = Vec::new();
    for (arch, attr) in LINUX_ARCHES
        .iter()
        .zip(["data-href-linux-x86_64", "data-href-linux-aarch64"])
    {
        out.push((attr, facts.linux(arch).map(|d| d.url.clone())));
    }
    for (arch, attr) in LINUX_ARCHES
        .iter()
        .zip(["data-install-linux-x86_64", "data-install-linux-aarch64"])
    {
        out.push((attr, linux_install_command(facts, arch)));
    }
    out.push(("data-intel-proof", facts.intel_proof.clone()));
    for (slot, attr) in ["data-toolchain-x86_64", "data-toolchain-aarch64"]
        .into_iter()
        .enumerate()
    {
        debug_assert_eq!(attr, format!("data-toolchain-{}", MAC_ARCHES[slot]));
        out.push((attr, toolchain.map(|l| l[slot].as_str().to_string())));
    }
    out
}

/// `\s+ATTR="[^"]*"` in `tag`: each match's span (from the first whitespace before).
fn attribute_spans(tag: &str, attr: &str) -> Vec<(usize, usize)> {
    let needle = format!("{attr}=\"");
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(at) = tag[from..].find(&needle) {
        let name = from + at;
        let ws = tag[..name]
            .chars()
            .rev()
            .take_while(|c| c.is_whitespace())
            .map(char::len_utf8)
            .sum::<usize>();
        let value = name + needle.len();
        match tag[value..].find('"') {
            Some(q) if ws > 0 => {
                spans.push((name - ws, value + q + 1));
                from = value + q + 1;
            }
            _ => from = name + 1,
        }
    }
    spans
}

/// The anchor's opening `tag` with every platform attribute removed and the known
/// ones written once, at the end.
fn with_platform_attributes(
    page: &str,
    tag: &str,
    attrs: &[(&'static str, Option<String>)],
) -> Result<String, PageError> {
    let mut tag = tag.to_string();
    for (attr, value) in attrs {
        let spans = attribute_spans(&tag, attr);
        if spans.len() > 1 {
            return Err(PageError::Duplicate {
                page: page.into(),
                attr,
            });
        }
        for (a, b) in spans.into_iter().rev() {
            tag.replace_range(a..b, "");
        }
        if let Some(v) = value.as_deref().filter(|v| !v.is_empty()) {
            tag.pop();
            tag.push_str(&format!(" {attr}=\"{}\">", html_escape(v)));
        }
    }
    Ok(tag)
}

/// `page` (one of [`PAGES`]) rewritten for `facts` and the toolchain levels.
/// # Errors
/// A rule that does not match exactly once, or a duplicated platform attribute.
pub fn rewrite(
    page: &str,
    text: &str,
    facts: &Facts,
    toolchain: Option<&Levels>,
) -> Result<String, PageError> {
    let mac = &facts.mac;
    let mut text = text.to_string();
    if matches!(page, "index.html" | "aterm.html") {
        text = exactly_once(page, &text, Rule::DownloadAnchor, &anchors(&text), |_| {
            Ok(mac.url.clone())
        })?;
        let size = mac
            .size_label()
            .map(|s| format!(" &middot; <code>{}</code>", s.replace(' ', "&nbsp;")))
            .unwrap_or_default();
        text = exactly_once(page, &text, Rule::NoteLine, &notes(&text), |_| {
            Ok(format!("<code>{}</code>{size}", mac.label))
        })?;
    }
    if page == "aterm.html" {
        let links = quoted(&text, "<a href=\"", "\">release notes</a>");
        text = exactly_once(page, &text, Rule::ReleaseNotesLink, &links, |_| {
            Ok("/releases".into())
        })?;
    }
    if page == "terminal.html" {
        text = exactly_once(page, &text, Rule::HeaderAnchor, &get_anchors(&text), |_| {
            Ok(mac.url.clone())
        })?;
        text = exactly_once(page, &text, Rule::HeaderTitle, &titles(&text), |_| {
            Ok(mac.version.clone())
        })?;
    }
    if matches!(page, "aterm.html" | "terminal.html") {
        let attrs = platform_attributes(facts, toolchain);
        let tags = platform_anchors(&text);
        text = exactly_once(page, &text, Rule::PlatformAnchor, &tags, |tag| {
            with_platform_attributes(page, tag, &attrs)
        })?;
    }
    Ok(text)
}

/// The download anchors' hrefs on `page`, as [`rewrite`] writes them.
#[must_use]
pub fn hrefs(page: &str, text: &str) -> Vec<String> {
    let hits = if page == "terminal.html" {
        get_anchors(text)
    } else {
        anchors(text)
    };
    hits.iter()
        .map(|h| text[h.inner.0..h.inner.1].to_string())
        .collect()
}

/// Every `ATTR="…"` value in `text`, in order.
#[must_use]
pub fn attribute_values(text: &str, attr: &str) -> Vec<String> {
    quoted(text, &format!("{attr}=\""), "\"")
        .iter()
        .map(|h| text[h.inner.0..h.inner.1].to_string())
        .collect()
}

/// The note lines' cells after their opening tag (`<code>NAME</code> &middot; …`).
#[cfg(test)]
pub(crate) fn note_cells(text: &str) -> Vec<String> {
    notes(text)
        .iter()
        .map(|h| text[h.inner.0..h.inner.1].to_string())
        .collect()
}

/// The versions `/terminal`'s header titles name.
#[cfg(test)]
pub(crate) fn title_versions(text: &str) -> Vec<String> {
    titles(text)
        .iter()
        .map(|h| text[h.inner.0..h.inner.1].to_string())
        .collect()
}

/// One way the live site differs from the checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Difference {
    EngineNotFetched,
    /// The live `engine.js` points at another wasm pair than the checkout's.
    EngineDiffers,
    PageNotFetched(&'static str),
    Href {
        page: &'static str,
        live: Vec<String>,
        want: String,
    },
    Attribute {
        page: &'static str,
        attr: &'static str,
        live: Vec<String>,
        want: Vec<String>,
    },
    ReleasesNotFetched,
    ReleasesDiffer,
    /// The checkout serves `/install.sh` and the live site does not.
    InstallerNotFetched,
    /// The live `/install.sh` is not the checkout's.
    InstallerDiffers,
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EngineNotFetched => f.write_str("engine.js was not fetched"),
            Self::EngineDiffers => {
                f.write_str("live engine.js does not point at the pair the checkout holds")
            }
            Self::PageNotFetched(p) => write!(f, "{p} was not fetched"),
            Self::Href { page, live, want } => {
                write!(f, "{page} download href is {live:?}, expected {want}")
            }
            Self::Attribute {
                page,
                attr,
                live,
                want,
            } => write!(f, "{page} {attr} is {live:?}, expected {want:?}"),
            Self::ReleasesNotFetched => f.write_str("releases.html was not fetched"),
            Self::ReleasesDiffer => f.write_str("live /releases differs from the checkout"),
            Self::InstallerNotFetched => f.write_str("/install.sh was not fetched"),
            Self::InstallerDiffers => {
                f.write_str("live /install.sh differs from the checkout's public/install.sh")
            }
        }
    }
}

/// What the live site and the checkout hold, by file.
pub struct Copies<'a> {
    pub get: &'a dyn Fn(&str) -> Option<String>,
}

/// How the live site differs from the checkout.
#[must_use]
pub fn compare(facts: &Facts, local: &Copies<'_>, live: &Copies<'_>) -> Vec<Difference> {
    let mut out = Vec::new();
    match (live.get)("engine.js") {
        None => out.push(Difference::EngineNotFetched),
        Some(e) if Some(&e) != (local.get)("engine.js").as_ref() => {
            out.push(Difference::EngineDiffers);
        }
        Some(_) => {}
    }
    let attrs = platform_attributes(facts, None);
    for page in PAGES {
        let Some(text) = (live.get)(page) else {
            out.push(Difference::PageNotFetched(page));
            continue;
        };
        let live_hrefs = hrefs(page, &text);
        if live_hrefs != [facts.mac.url.clone()] {
            out.push(Difference::Href {
                page,
                live: live_hrefs,
                want: facts.mac.url.clone(),
            });
        }
        if page != "index.html" {
            let checkout = (local.get)(page).unwrap_or_default();
            for (attr, _) in &attrs {
                let want = attribute_values(&checkout, attr);
                let got = attribute_values(&text, attr);
                if got != want {
                    out.push(Difference::Attribute {
                        page,
                        attr,
                        live: got,
                        want,
                    });
                }
            }
        }
    }
    if let Some(local_notes) = (local.get)("releases.html") {
        match (live.get)("releases.html") {
            None => out.push(Difference::ReleasesNotFetched),
            Some(l) if l != local_notes => out.push(Difference::ReleasesDiffer),
            Some(_) => {}
        }
    }
    if let Some(local_installer) = (local.get)("install.sh") {
        match (live.get)("install.sh") {
            None => out.push(Difference::InstallerNotFetched),
            Some(l) if l != local_installer => out.push(Difference::InstallerDiffers),
            Some(_) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::releases::{LinuxDownload, LinuxKind, MacDownload};
    use crate::site::toolchain::Coverage;
    use std::collections::BTreeMap;
    use std::path::Path;

    fn page(name: &str) -> String {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site/public");
        std::fs::read_to_string(dir.join(name)).unwrap()
    }

    const DMG63: &str =
        "https://github.com/alabsystems/aterm/releases/download/v0.63.0/aterm-0.63.0.dmg";

    fn facts(size: Option<u64>) -> Facts {
        Facts {
            slug: "alabsystems/aterm".into(),
            mac: MacDownload {
                tag: "v0.63.0".into(),
                version: "0.63.0".into(),
                label: "aterm-0.63.0.dmg".into(),
                url: DMG63.into(),
                size_bytes: size,
            },
            head: "v0.63.0".into(),
            intel_proof: None,
            linux: [None, None],
        }
    }

    fn tar(v: &str) -> LinuxDownload {
        let label = format!("aterm-{v}-linux-x86_64.tar.gz");
        LinuxDownload {
            tag: format!("v{v}"),
            version: v.into(),
            url: format!("https://github.com/alabsystems/aterm/releases/download/v{v}/{label}"),
            label,
            kind: LinuxKind::LegacyTar,
        }
    }

    #[test]
    fn the_buttons_and_the_note_name_the_dmg_and_its_true_size() {
        let f = facts(Some(1_067_044_990));
        for name in ["index.html", "aterm.html"] {
            let out = rewrite(name, &page(name), &f, None).unwrap();
            assert_eq!(hrefs(name, &out), [DMG63], "{name}");
            assert_eq!(
                note_cells(&out),
                ["<code>aterm-0.63.0.dmg</code> &middot; <code>1.07&nbsp;GB</code>"],
                "{name}"
            );
        }
        let out = rewrite("terminal.html", &page("terminal.html"), &f, None).unwrap();
        assert_eq!(hrefs("terminal.html", &out), [DMG63]);
        assert_eq!(title_versions(&out), ["0.63.0"]);
        assert_eq!(
            quoted(&out, "<a class=\"get register\" href=\"", "\"").len(),
            1
        );
    }

    #[test]
    fn an_unknown_size_leaves_the_name_alone_never_a_guess() {
        let out = rewrite("index.html", &page("index.html"), &facts(None), None).unwrap();
        assert_eq!(note_cells(&out), ["<code>aterm-0.63.0.dmg</code>"]);
    }

    #[test]
    fn the_release_notes_link_points_at_releases() {
        let out = rewrite("aterm.html", &page("aterm.html"), &facts(None), None).unwrap();
        let links = quoted(&out, "<a href=\"", "\">release notes</a>");
        assert_eq!(&out[links[0].inner.0..links[0].inner.1], "/releases");
    }

    #[test]
    fn the_platform_attributes_follow_the_facts_and_never_reach_the_homepage() {
        let mut f = facts(Some(1));
        f.linux[0] = Some(tar("0.63.0"));
        f.intel_proof = Some("https://example.invalid/probe.txt".into());
        let levels = [Coverage::Partial, Coverage::Full];
        for name in ["aterm.html", "terminal.html"] {
            let out = rewrite(name, &page(name), &f, Some(&levels)).unwrap();
            let one = |attr| attribute_values(&out, attr);
            assert_eq!(one("data-href-linux-x86_64"), [tar("0.63.0").url], "{name}");
            assert_eq!(
                one("data-href-linux-aarch64"),
                Vec::<String>::new(),
                "{name}"
            );
            assert_eq!(
                one("data-intel-proof"),
                ["https://example.invalid/probe.txt"]
            );
            assert_eq!(one("data-toolchain-x86_64"), ["partial"]);
            assert_eq!(one("data-toolchain-aarch64"), ["full"]);
            assert_eq!(
                rewrite(name, &out, &f, Some(&levels)).unwrap(),
                out,
                "{name}"
            );
            let bare = rewrite(name, &out, &facts(Some(1)), None).unwrap();
            for (attr, _) in platform_attributes(&f, Some(&levels)) {
                assert!(attribute_values(&bare, attr).is_empty(), "{name} {attr}");
            }
        }
        let home = rewrite("index.html", &page("index.html"), &f, Some(&levels)).unwrap();
        for (attr, _) in platform_attributes(&f, Some(&levels)) {
            assert!(attribute_values(&home, attr).is_empty(), "{attr}");
        }
    }

    fn signed(v: &str, arch: &str) -> LinuxDownload {
        let label = format!("aterm-{v}-linux-{arch}");
        LinuxDownload {
            tag: format!("v{v}"),
            version: v.into(),
            url: format!("https://github.com/alabsystems/aterm/releases/download/v{v}/{label}"),
            label,
            kind: LinuxKind::SignedRaw {
                sha256: "b".repeat(64),
                size: 9000,
            },
        }
    }

    /// A Linux pick is the installer command, written only for an architecture with a
    /// signed executable — plain when that executable is the head's (what an unpinned
    /// install.sh installs), pinned to its release when it trails the head — and never
    /// for a checksum-only tarball, which the installer does not install.
    #[test]
    fn the_install_command_is_written_only_for_a_signed_executable() {
        let mut f = facts(Some(1));
        f.linux = [Some(tar("0.61.0")), Some(signed("0.63.0", "aarch64"))];
        let plain = "curl -fsSL https://alab.systems/install.sh | bash";
        assert_eq!(linux_install_command(&f, "x86_64"), None);
        assert_eq!(linux_install_command(&f, "aarch64").as_deref(), Some(plain));
        for name in ["aterm.html", "terminal.html"] {
            let out = rewrite(name, &page(name), &f, None).unwrap();
            assert_eq!(
                attribute_values(&out, "data-install-linux-aarch64"),
                [plain],
                "{name}"
            );
            assert!(
                attribute_values(&out, "data-install-linux-x86_64").is_empty(),
                "{name}"
            );
            assert_eq!(
                attribute_values(&out, "data-href-linux-aarch64"),
                [signed("0.63.0", "aarch64").url],
                "{name}"
            );
        }
        f.linux[0] = Some(signed("0.62.0", "x86_64"));
        assert_eq!(
            linux_install_command(&f, "x86_64").as_deref(),
            Some("curl -fsSL https://alab.systems/install.sh | bash -s -- --version 0.62.0")
        );
        let home = rewrite("index.html", &page("index.html"), &f, None).unwrap();
        assert!(attribute_values(&home, "data-install-linux-aarch64").is_empty());
    }

    #[test]
    fn older_note_shapes_converge_on_name_and_size_and_a_rewrite_is_idempotent() {
        let f = facts(Some(1_067_044_990));
        let old = page("aterm.html").replace(
            "<code>aterm-0.61.0.dmg</code> &middot; <code>31&nbsp;MB</code>",
            "<code>aterm-0.61.0-mac.zip</code> &middot; <code>0.61.0</code> &middot; \
             <code>~27&nbsp;MB</code>",
        );
        let name_only = page("aterm.html").replace(" &middot; <code>31&nbsp;MB</code>", "");
        assert_ne!(old, page("aterm.html"));
        assert_ne!(name_only, page("aterm.html"));
        let a = rewrite("aterm.html", &old, &f, None).unwrap();
        let b = rewrite("aterm.html", &name_only, &f, None).unwrap();
        assert_eq!(a, b);
        assert_eq!(rewrite("aterm.html", &a, &f, None).unwrap(), a);
        assert!(!a.contains("~27") && !a.contains("mac.zip"));
    }

    #[test]
    fn a_page_that_lost_a_rules_shape_is_refused_by_name() {
        let f = facts(Some(1));
        let renamed = page("index.html").replace("class=\"download\"", "class=\"dl\"");
        assert_eq!(
            rewrite("index.html", &renamed, &f, None).unwrap_err(),
            PageError::Shape {
                page: "index.html".into(),
                rule: Rule::DownloadAnchor,
                found: 0
            }
        );
        let devtitle = page("terminal.html").replace("aterm v0.61.0&nbsp;", "aterm (dev) ");
        assert_eq!(
            rewrite("terminal.html", &devtitle, &f, None).unwrap_err(),
            PageError::Shape {
                page: "terminal.html".into(),
                rule: Rule::HeaderTitle,
                found: 0
            }
        );
        let twice = page("aterm.html").replace(
            "data-href-linux-x86_64=",
            "data-intel-proof=\"a\" data-intel-proof=\"b\" data-href-linux-x86_64=",
        );
        assert_eq!(
            rewrite("aterm.html", &twice, &f, None).unwrap_err(),
            PageError::Duplicate {
                page: "aterm.html".into(),
                attr: "data-intel-proof"
            }
        );
    }

    #[test]
    fn the_live_comparison_names_each_difference() {
        let mut f = facts(Some(1));
        f.linux[0] = Some(tar("0.63.0"));
        let levels = [Coverage::Partial, Coverage::Full];
        let mut local = BTreeMap::new();
        for name in PAGES {
            local.insert(name, rewrite(name, &page(name), &f, Some(&levels)).unwrap());
        }
        local.insert("engine.js", "engine\n".into());
        let mut live = local.clone();
        let lookup = |m: &BTreeMap<&'static str, String>| {
            let m = m.clone();
            move |k: &str| m.get(k).cloned()
        };
        let (l, v) = (lookup(&local), lookup(&live));
        assert_eq!(compare(&f, &Copies { get: &l }, &Copies { get: &v }), []);
        local.insert("install.sh", "#!/usr/bin/env bash\n".into());
        let l = lookup(&local);
        assert_eq!(
            compare(&f, &Copies { get: &l }, &Copies { get: &v }),
            [Difference::InstallerNotFetched]
        );
        live.insert("install.sh", "#!/bin/sh\n".into());
        let v = lookup(&live);
        assert_eq!(
            compare(&f, &Copies { get: &l }, &Copies { get: &v }),
            [Difference::InstallerDiffers]
        );
        live.insert("install.sh", local["install.sh"].clone());
        live.insert(
            "aterm.html",
            local["aterm.html"].replace(
                "data-toolchain-x86_64=\"partial\"",
                "data-toolchain-x86_64=\"full\"",
            ),
        );
        live.insert("engine.js", "stale\n".into());
        live.remove("terminal.html");
        let v = lookup(&live);
        assert_eq!(
            compare(&f, &Copies { get: &l }, &Copies { get: &v }),
            [
                Difference::EngineDiffers,
                Difference::Attribute {
                    page: "aterm.html",
                    attr: "data-toolchain-x86_64",
                    live: vec!["full".into()],
                    want: vec!["partial".into()]
                },
                Difference::PageNotFetched("terminal.html"),
            ]
        );
    }
}
