// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The package `Publisher` check: an MSIX (or bundle) installs only when its manifest's
//! `Identity` `Publisher` is the subject of the certificate that signed it, byte for byte as
//! Windows writes that subject; otherwise `Add-AppxPackage` refuses it with `0x80080204`.
//!
//! `sign` checks it twice. Before signtool runs, [`admit`] reads the manifest and, while Tier
//! WINDOWS is active, refuses a `Publisher` whose CN is not the pinned publisher. After the
//! signature, [`signed_as_published`] reads the signer's leaf back off the package's
//! `AppxSignature.p7x` and refuses a `Publisher` that is not its subject. A package is a ZIP;
//! [`aterm_codec::zip`] and [`aterm_codec::xml`] read it, and only the two members named here.

use std::path::Path;

use crate::{Host, Tier};

/// What kind of package a file is, by its extension (any case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `.msix` or `.appx`: `AppxManifest.xml` at the root.
    Package,
    /// `.msixbundle` or `.appxbundle`: `AppxMetadata/AppxBundleManifest.xml`.
    Bundle,
}

impl Kind {
    /// The package kind `path` names, or `None` for any other file (an exe, a dll).
    #[must_use]
    pub fn of(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "msix" | "appx" => Some(Self::Package),
            "msixbundle" | "appxbundle" => Some(Self::Bundle),
            _ => None,
        }
    }

    /// The member that carries the package's `Identity`.
    #[must_use]
    pub const fn manifest(self) -> &'static str {
        match self {
            Self::Package => "AppxManifest.xml",
            Self::Bundle => "AppxMetadata/AppxBundleManifest.xml",
        }
    }
}

/// The package's signature member: `PKCX` and a DER PKCS #7 `SignedData`.
pub const SIGNATURE: &str = "AppxSignature.p7x";

/// The most a manifest may hold.
const MANIFEST_MAX: u64 = 4 << 20;
/// The most a signature member may hold.
const SIGNATURE_MAX: u64 = 1 << 20;

/// One member of the package at `path`.
fn member(host: &dyn Host, path: &Path, name: &str, max: u64) -> Result<Vec<u8>, String> {
    let reader = host.open(path)?;
    let mut zip = aterm_codec::zip::Archive::open(reader).map_err(|e| format!("{e}"))?;
    let entry = zip
        .find(name.as_bytes())
        .map_err(|e| format!("{e}"))?
        .ok_or_else(|| format!("the package holds no {name}"))?;
    zip.read(&entry, max).map_err(|e| format!("{name}: {e}"))
}

/// The manifest's `Identity` `Publisher`, as written (entities resolved).
///
/// # Errors
/// The package or its manifest could not be read, or the manifest is not well-formed XML, has
/// no single `Identity` under its root, or that `Identity` has no `Publisher` or one that is not
/// a distinguished name ([`components`]).
pub fn publisher(host: &dyn Host, path: &Path, kind: Kind) -> Result<String, String> {
    let bytes = member(host, path, kind.manifest(), MANIFEST_MAX)?;
    let text =
        std::str::from_utf8(&bytes).map_err(|_| format!("{} is not UTF-8", kind.manifest()))?;
    let publisher = identity_publisher(text.strip_prefix('\u{feff}').unwrap_or(text))
        .map_err(|why| format!("{}: {why}", kind.manifest()))?;
    components(&publisher).map_err(|why| {
        format!(
            "{}: Identity Publisher `{publisher}` is not a distinguished name: {why}",
            kind.manifest()
        )
    })?;
    Ok(publisher)
}

/// The `Publisher` of the one `Identity` element directly under the document's root.
fn identity_publisher(text: &str) -> Result<String, String> {
    use aterm_codec::xml::{Event, Reader};
    let mut reader = Reader::new(text);
    let mut depth = 0usize;
    let mut found: Option<Option<String>> = None;
    while let Some(event) = reader.next_event().map_err(|e| format!("{e}"))? {
        match event {
            Event::Start(tag) => {
                if depth == 1 && tag.name == "Identity" {
                    if found.is_some() {
                        return Err("more than one Identity element".into());
                    }
                    found = Some(tag.attr("Publisher").map(str::to_owned));
                }
                if !tag.empty {
                    depth = depth.saturating_add(1);
                }
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Text(_) => {}
        }
    }
    match found {
        None => Err("no Identity element under the root".into()),
        Some(None) => Err("the Identity element has no Publisher".into()),
        Some(Some(p)) => Ok(p),
    }
}

/// The attribute names Windows writes a subject with (`CertNameToStr`, X.500 form), each beside
/// its OID; any other attribute is written `OID.<dotted>`. The same names, and `OID.<dotted>`, are
/// the ones a manifest `Publisher` may use.
const ATTRIBUTES: [(&str, &str); 20] = [
    ("2.5.4.3", "CN"),
    ("2.5.4.4", "SN"),
    ("2.5.4.5", "SERIALNUMBER"),
    ("2.5.4.6", "C"),
    ("2.5.4.7", "L"),
    ("2.5.4.8", "S"),
    ("2.5.4.9", "STREET"),
    ("2.5.4.10", "O"),
    ("2.5.4.11", "OU"),
    ("2.5.4.12", "T"),
    ("2.5.4.13", "Description"),
    ("2.5.4.17", "PostalCode"),
    ("2.5.4.18", "POBox"),
    ("2.5.4.20", "Phone"),
    ("2.5.4.24", "X21Address"),
    ("2.5.4.42", "G"),
    ("2.5.4.43", "I"),
    ("2.5.4.46", "dnQualifier"),
    ("1.2.840.113549.1.9.1", "E"),
    ("0.9.2342.19200300.100.1.25", "DC"),
];

/// Whether `s` is a dotted OID: two or more arcs of decimal digits, no leading zero but a lone 0.
fn is_dotted(s: &str) -> bool {
    let arcs: Vec<&str> = s.split('.').collect();
    arcs.len() >= 2
        && arcs.iter().all(|a| {
            !a.is_empty()
                && a.bytes().all(|b| b.is_ascii_digit())
                && (a.len() == 1 || !a.starts_with('0'))
        })
}

/// The attribute name `key` spells, in Windows' own case, or `OID.<dotted>`.
fn canonical_key(key: &str) -> Option<String> {
    if let Some((_, name)) = ATTRIBUTES.iter().find(|(_, n)| n.eq_ignore_ascii_case(key)) {
        return Some((*name).to_owned());
    }
    let dotted = key.get(..4).filter(|p| p.eq_ignore_ascii_case("OID."))?;
    let arcs = key.get(dotted.len()..)?;
    is_dotted(arcs).then(|| format!("OID.{arcs}"))
}

/// The `(attribute, value)` pairs of a distinguished name written the way a manifest
/// `Publisher` is: `KEY=value` separated by `,` or `;` (or `+` inside one RDN), a value either
/// bare (none of `,+="<>#;`, outer spaces dropped) or quoted (`""` for a quote).
///
/// # Errors
/// An empty name, an unknown attribute, a missing `=`, an empty or unterminated value, or a bare
/// value holding a character that must be quoted.
pub fn components(dn: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut rest = dn;
    loop {
        rest = rest.trim_start_matches(' ');
        let eq = rest
            .find('=')
            .ok_or_else(|| format!("`{rest}` has no `=`"))?;
        let (raw_key, tail) = rest.split_at(eq);
        let key = canonical_key(raw_key.trim_matches(' '))
            .ok_or_else(|| format!("`{}` is not an attribute Windows names", raw_key.trim()))?;
        let tail = tail.get(1..).unwrap_or("").trim_start_matches(' ');
        let (value, after) = if let Some(quoted) = tail.strip_prefix('"') {
            let mut value = String::new();
            let mut chars = quoted.char_indices();
            let end = loop {
                match chars.next() {
                    None => return Err(format!("{key}'s quoted value is unterminated")),
                    Some((i, '"')) => {
                        if quoted.get(i + 1..).is_some_and(|t| t.starts_with('"')) {
                            value.push('"');
                            chars.next();
                        } else {
                            break i + 1;
                        }
                    }
                    Some((_, c)) => value.push(c),
                }
            };
            (
                value,
                quoted.get(end..).unwrap_or("").trim_start_matches(' '),
            )
        } else {
            let stop = tail.find([',', ';', '+']).unwrap_or(tail.len());
            let (bare, after) = tail.split_at(stop);
            let bare = bare.trim_end_matches(' ');
            if bare.is_empty() {
                return Err(format!("{key} has an empty value"));
            }
            if let Some(c) = bare
                .chars()
                .find(|c| matches!(c, '=' | '"' | '<' | '>' | '#'))
            {
                return Err(format!("{key}'s value holds `{c}` unquoted"));
            }
            (bare.to_owned(), after)
        };
        out.push((key, value));
        let Some(sep) = after.chars().next() else {
            return Ok(out);
        };
        if !matches!(sep, ',' | ';' | '+') {
            return Err(format!("`{after}` follows a value without a separator"));
        }
        rest = after.get(1..).unwrap_or("");
        if rest.trim_start_matches(' ').is_empty() {
            return Err("the name ends in a separator".into());
        }
    }
}

/// Why a package may not be signed: its `Publisher` would not install under the pinned
/// publisher. Read before signtool runs.
///
/// # Errors
/// Under an active tier, a `Publisher` whose single CN is not the pin (or that names no CN, or
/// more than one). `publisher` has already passed [`components`].
pub fn admit(tier: &Tier, publisher: &str) -> Result<(), String> {
    let Tier::Active { publisher: pin } = tier else {
        return Ok(());
    };
    let parts = components(publisher)?;
    let mut cns = parts.iter().filter(|(k, _)| k == "CN").map(|(_, v)| v);
    let (Some(cn), None) = (cns.next(), cns.next()) else {
        return Err(format!(
            "the manifest Publisher `{publisher}` must name exactly one CN, and pins::WINDOWS_SIGNING_PUBLISHER is `{pin}`"
        ));
    };
    if cn != pin {
        return Err(format!(
            "the manifest Publisher `{publisher}` has CN `{cn}`, but pins::WINDOWS_SIGNING_PUBLISHER is `{pin}`; \
             the signed package would not install (0x80080204). Pack it with -Publisher naming the signer's subject"
        ));
    }
    Ok(())
}

/// The signer's subject, read back off the signed package, when it is the manifest's
/// `Publisher`.
///
/// # Errors
/// The package, its manifest or its signature could not be read, or the subject Windows
/// writes for the signing leaf is not the `Publisher` byte for byte (both are named).
pub fn signed_as_published(host: &dyn Host, path: &Path, kind: Kind) -> Result<String, String> {
    let publisher = publisher(host, path, kind)?;
    let p7x = member(host, path, SIGNATURE, SIGNATURE_MAX)?;
    let subject = signer_subject(&p7x).map_err(|why| format!("{SIGNATURE}: {why}"))?;
    if subject != publisher {
        return Err(format!(
            "the manifest Publisher is `{publisher}`, but the package is signed by `{subject}`; \
             Windows refuses it (0x80080204). Pack it with -Publisher '{subject}' and sign it again"
        ));
    }
    Ok(subject)
}

/// One DER element: its tag, its contents, and the whole encoding.
struct Tlv<'a> {
    tag: u8,
    body: &'a [u8],
    whole: &'a [u8],
}

/// A cursor over consecutive DER elements.
struct Der<'a>(&'a [u8]);

impl<'a> Der<'a> {
    fn peek(&self) -> Option<u8> {
        self.0.first().copied()
    }

    fn next(&mut self) -> Result<Tlv<'a>, String> {
        let all = self.0;
        let (&tag, rest) = all.split_first().ok_or("truncated DER")?;
        if tag & 0x1f == 0x1f {
            return Err("a multi-byte DER tag".into());
        }
        let (&first, mut rest) = rest.split_first().ok_or("truncated DER")?;
        let len = match first {
            0..=0x7f => usize::from(first),
            0x80 => return Err("an indefinite DER length".into()),
            _ => {
                let n = usize::from(first & 0x7f);
                if n > 4 {
                    return Err("a DER length past 32 bits".into());
                }
                let (bytes, tail) = rest.split_at_checked(n).ok_or("truncated DER")?;
                rest = tail;
                bytes
                    .iter()
                    .fold(0usize, |acc, b| (acc << 8) | usize::from(*b))
            }
        };
        let (body, tail) = rest.split_at_checked(len).ok_or("truncated DER")?;
        let used = all.len() - tail.len();
        self.0 = tail;
        Ok(Tlv {
            tag,
            body,
            whole: all.get(..used).unwrap_or(all),
        })
    }

    fn expect(&mut self, tag: u8) -> Result<Tlv<'a>, String> {
        let tlv = self.next()?;
        if tlv.tag != tag {
            return Err(format!(
                "expected DER tag {tag:#04x}, found {:#04x}",
                tlv.tag
            ));
        }
        Ok(tlv)
    }
}

/// `1.2.840.113549.1.7.2`, PKCS #7 `signedData`, as DER contents.
const SIGNED_DATA: [u8; 9] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02];

/// The subject of the certificate that made the signature in an `AppxSignature.p7x`, written as
/// Windows writes it ([`subject_string`]).
///
/// # Errors
/// Not `PKCX` and a DER `SignedData`, not exactly one signer, a signer named by key id rather
/// than issuer and serial, or no certificate in the set that is that signer.
pub fn signer_subject(p7x: &[u8]) -> Result<String, String> {
    let der = p7x
        .strip_prefix(b"PKCX")
        .ok_or("does not start with PKCX")?;
    let mut top = Der(der);
    let info = top.expect(0x30)?;
    let mut info = Der(info.body);
    if info.expect(0x06)?.body != SIGNED_DATA {
        return Err("is not PKCS #7 signedData".into());
    }
    let mut explicit = Der(info.expect(0xa0)?.body);
    let mut signed = Der(explicit.expect(0x30)?.body);
    signed.expect(0x02)?;
    signed.expect(0x31)?;
    signed.expect(0x30)?;
    let certificates = if signed.peek() == Some(0xa0) {
        signed.next()?.body
    } else {
        &[]
    };
    if signed.peek() == Some(0xa1) {
        signed.next()?;
    }
    let mut signers = Der(signed.expect(0x31)?.body);
    let mut signer = Der(signers.expect(0x30)?.body);
    if signers.peek().is_some() {
        return Err("carries more than one signer".into());
    }
    signer.expect(0x02)?;
    let sid = signer.next()?;
    if sid.tag != 0x30 {
        return Err("names its signer by key id; winsign reads an issuer-and-serial signer".into());
    }
    let mut sid = Der(sid.body);
    let issuer = sid.expect(0x30)?.whole;
    let serial = sid.expect(0x02)?.body;
    let mut certs = Der(certificates);
    while certs.peek().is_some() {
        let cert = certs.expect(0x30)?;
        let mut tbs = Der(Der(cert.body).expect(0x30)?.body);
        if tbs.peek() == Some(0xa0) {
            tbs.next()?;
        }
        let cert_serial = tbs.expect(0x02)?.body;
        tbs.expect(0x30)?;
        let cert_issuer = tbs.expect(0x30)?.whole;
        tbs.expect(0x30)?;
        let subject = tbs.expect(0x30)?;
        if cert_serial == serial && cert_issuer == issuer {
            return subject_string(subject.body);
        }
    }
    Err("holds no certificate for its signer".into())
}

/// A DER OID's contents in dotted form.
fn dotted(body: &[u8]) -> Result<String, String> {
    let mut arcs: Vec<u64> = Vec::new();
    let mut acc: u64 = 0;
    let mut open = false;
    for &b in body {
        if acc > (u64::MAX >> 7) {
            return Err("an OID arc past 64 bits".into());
        }
        acc = (acc << 7) | u64::from(b & 0x7f);
        open = b & 0x80 != 0;
        if !open {
            if arcs.is_empty() {
                let first = (acc / 40).min(2);
                arcs.extend([first, acc - first * 40]);
            } else {
                arcs.push(acc);
            }
            acc = 0;
        }
    }
    if open || arcs.is_empty() {
        return Err("a truncated OID".into());
    }
    Ok(arcs
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join("."))
}

/// A directory string's text, by its DER string type.
fn text(tlv: &Tlv<'_>) -> Result<String, String> {
    let b = tlv.body;
    match tlv.tag {
        0x0c => std::str::from_utf8(b)
            .map(str::to_owned)
            .map_err(|_| "a UTF8String that is not UTF-8".into()),
        0x12 | 0x13 | 0x16 | 0x1a => b
            .is_ascii()
            .then(|| b.iter().map(|&c| char::from(c)).collect::<String>())
            .ok_or_else(|| "an ASCII string type holding a non-ASCII byte".into()),
        // `CertNameToStr` reads a TeletexString as UTF-8 first and as 8-bit only when it is not.
        0x14 => Ok(std::str::from_utf8(b).map_or_else(
            |_| b.iter().map(|&c| char::from(c)).collect(),
            str::to_owned,
        )),
        0x1e => {
            let units: Vec<u16> = b
                .chunks(2)
                .map(|c| match c {
                    [hi, lo] => Ok(u16::from_be_bytes([*hi, *lo])),
                    _ => Err("a BMPString of odd length".to_owned()),
                })
                .collect::<Result<_, _>>()?;
            String::from_utf16(&units).map_err(|_| "a BMPString that is not UTF-16".into())
        }
        0x1c => b
            .chunks(4)
            .map(|c| {
                <[u8; 4]>::try_from(c)
                    .ok()
                    .and_then(|q| char::from_u32(u32::from_be_bytes(q)))
                    .ok_or_else(|| "a UniversalString that is not UTF-32".to_owned())
            })
            .collect(),
        t => Err(format!("an attribute value of DER type {t:#04x}")),
    }
}

/// `value` quoted the way `CertNameToStr` quotes one: when it is empty, starts or ends with a
/// space, or holds any of `,+="<>#;` or a line feed, it is wrapped in `"` with each `"` doubled.
fn quoted(value: &str) -> String {
    let needs = value.is_empty()
        || value.starts_with(' ')
        || value.ends_with(' ')
        || value.contains([',', '+', '=', '"', '\n', '<', '>', '#', ';']);
    if needs {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// A DER `Name`'s contents written as Windows writes a certificate subject (`CertNameToStr` with
/// `CERT_X500_NAME_STR | CERT_NAME_STR_REVERSE_FLAG`, .NET's `X509Certificate2.Subject`): the
/// RDNs last first, joined with `, `; the attributes of one RDN joined with ` + `; each
/// `KEY=value` with [`ATTRIBUTES`]' names and [`quoted`] values.
///
/// # Errors
/// A malformed `Name`, an empty one, or an attribute value that is not a directory string.
pub fn subject_string(name: &[u8]) -> Result<String, String> {
    let mut rdns = Vec::new();
    let mut seq = Der(name);
    while seq.peek().is_some() {
        let mut set = Der(seq.expect(0x31)?.body);
        let mut avas = Vec::new();
        while set.peek().is_some() {
            let mut ava = Der(set.expect(0x30)?.body);
            let oid = dotted(ava.expect(0x06)?.body)?;
            let value = text(&ava.next()?)?;
            let key = ATTRIBUTES
                .iter()
                .find(|(o, _)| *o == oid)
                .map_or_else(|| format!("OID.{oid}"), |(_, k)| (*k).to_owned());
            avas.push(format!("{key}={}", quoted(&value)));
        }
        if avas.is_empty() {
            return Err("an empty RDN".into());
        }
        rdns.push(avas.join(" + "));
    }
    if rdns.is_empty() {
        return Err("an empty subject".into());
    }
    rdns.reverse();
    Ok(rdns.join(", "))
}

#[cfg(test)]
#[path = "msix_tests.rs"]
pub(crate) mod tests;
