// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// One DER element.
pub(crate) fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = body.len();
    if len < 0x80 {
        out.push(len as u8);
    } else if len < 0x100 {
        out.extend([0x81, len as u8]);
    } else {
        out.extend([0x82, (len >> 8) as u8, len as u8]);
    }
    out.extend_from_slice(body);
    out
}

/// A dotted OID as a DER element.
fn oid(dotted: &str) -> Vec<u8> {
    let arcs: Vec<u64> = dotted.split('.').map(|a| a.parse().unwrap()).collect();
    let mut body = Vec::new();
    for arc in std::iter::once(arcs[0] * 40 + arcs[1]).chain(arcs[2..].iter().copied()) {
        let mut groups = vec![(arc & 0x7f) as u8];
        let mut rest = arc >> 7;
        while rest != 0 {
            groups.push((rest & 0x7f) as u8 | 0x80);
            rest >>= 7;
        }
        groups.reverse();
        body.extend(groups);
    }
    tlv(0x06, &body)
}

/// One attribute of an RDN: its OID, DER string type and text.
pub(crate) type Ava<'a> = (&'a str, u8, &'a str);

/// A DER `Name` of `rdns`, in encoded order (Windows writes the last first).
pub(crate) fn name(rdns: &[&[Ava<'_>]]) -> Vec<u8> {
    let mut seq = Vec::new();
    for rdn in rdns {
        let mut set = Vec::new();
        for (o, tag, text) in *rdn {
            let value = match *tag {
                0x1e => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
                0x14 => text.chars().map(|c| u8::try_from(c).unwrap()).collect(),
                _ => text.as_bytes().to_vec(),
            };
            set.extend(tlv(0x30, &[oid(o), tlv(*tag, &value)].concat()));
        }
        seq.extend(tlv(0x31, &set));
    }
    tlv(0x30, &seq)
}

/// A certificate as far as [`signer_subject`] reads it.
fn cert(serial: &[u8], issuer: &[u8], subject: &[u8]) -> Vec<u8> {
    let alg = tlv(0x30, &oid("1.2.840.113549.1.1.11"));
    let tbs = [
        tlv(0xa0, &tlv(0x02, &[2])),
        tlv(0x02, serial),
        alg.clone(),
        issuer.to_vec(),
        tlv(0x30, &[]),
        subject.to_vec(),
        tlv(0x30, &[]),
    ]
    .concat();
    tlv(
        0x30,
        &[tlv(0x30, &tbs), alg, tlv(0x03, &[0, 1, 2, 3])].concat(),
    )
}

/// `PKCX` and a `SignedData` holding `certs`, signed by the one `sid` names.
fn p7x_with(certs: &[Vec<u8>], sid: Vec<u8>, signers: usize) -> Vec<u8> {
    let signer = tlv(
        0x30,
        &[
            tlv(0x02, &[1]),
            sid,
            tlv(0x30, &oid("2.16.840.1.101.3.4.2.1")),
        ]
        .concat(),
    );
    let signed = [
        tlv(0x02, &[1]),
        tlv(0x31, &[]),
        tlv(0x30, &oid("1.3.6.1.4.1.311.2.1.4")),
        tlv(0xa0, &certs.concat()),
        tlv(0x31, &signer.repeat(signers)),
    ]
    .concat();
    let info = tlv(
        0x30,
        &[oid("1.2.840.113549.1.7.2"), tlv(0xa0, &tlv(0x30, &signed))].concat(),
    );
    [b"PKCX".to_vec(), info].concat()
}

const CN: &str = "2.5.4.3";
const O: &str = "2.5.4.10";
const C: &str = "2.5.4.6";
const L: &str = "2.5.4.7";

/// The issuing CA every test leaf chains to.
fn ca() -> Vec<u8> {
    name(&[
        &[(C, 0x13, "US")],
        &[(O, 0x13, "Microsoft Corporation")],
        &[(CN, 0x13, "Microsoft ID Verified CS EOC CA 01")],
    ])
}

/// A signature by a leaf whose subject is `subject`, with the CA's own certificate beside it
/// under the leaf's serial, so only issuer and serial together pick the leaf.
pub(crate) fn signed_by(subject: &[u8]) -> Vec<u8> {
    let leaf = cert(&[0x33, 0x01], &ca(), subject);
    let root = name(&[&[(CN, 0x13, "Microsoft Identity Verification Root")]]);
    let issuing = cert(&[0x33, 0x01], &root, &ca());
    let sid = tlv(0x30, &[ca(), tlv(0x02, &[0x33, 0x01])].concat());
    p7x_with(&[issuing, leaf], sid, 1)
}

/// A stored (uncompressed) ZIP of `members`.
pub(crate) fn zip(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in members {
        let offset = out.len() as u32;
        let size = data.len() as u32;
        let fixed = |sig: u32, out: &mut Vec<u8>| {
            out.extend(sig.to_le_bytes());
            if sig == 0x0201_4b50 {
                out.extend(20u16.to_le_bytes());
            }
            out.extend(20u16.to_le_bytes());
            out.extend([0u8; 2 + 2 + 4 + 4]);
            out.extend(size.to_le_bytes());
            out.extend(size.to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend(0u16.to_le_bytes());
        };
        fixed(0x0403_4b50, &mut out);
        out.extend(name.as_bytes());
        out.extend(*data);
        fixed(0x0201_4b50, &mut central);
        central.extend([0u8; 2 + 2 + 2 + 4]);
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend(central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((members.len() as u16).to_le_bytes());
    out.extend((members.len() as u16).to_le_bytes());
    out.extend(cd_size.to_le_bytes());
    out.extend(cd_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

/// An `AppxManifest.xml` whose `Identity` carries `publisher` (`None`: no `Publisher`), with a
/// commented-out decoy and a nested `Identity` that are not it.
pub(crate) fn manifest(publisher: Option<&str>) -> String {
    let attr = publisher.map_or_else(String::new, |p| {
        format!(" Publisher=\"{}\"", aterm_codec::xml::escape(p))
    });
    format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <Package xmlns=\"http://schemas.microsoft.com/appx/manifest/foundation/windows10\">\n\
         <!-- <Identity Name=\"x\" Publisher=\"CN=decoy\"/> -->\n\
         <Identity Name=\"aterm\"{attr} Version=\"0.3.0.0\" ProcessorArchitecture=\"x64\"/>\n\
         <Applications><Application Id=\"aterm\"><Identity Publisher=\"CN=nested\"/></Application></Applications>\n\
         </Package>\n"
    )
}

/// The publisher every run-level test signs as: the leaf in `VERIFIED` is `Andrew Yates`.
pub(crate) const PUBLISHER: &str = "CN=Andrew Yates, O=Andrew Yates, L=Seattle, C=US";

/// The DER subject [`PUBLISHER`] spells.
pub(crate) fn publisher_name() -> Vec<u8> {
    name(&[
        &[(C, 0x13, "US")],
        &[(L, 0x13, "Seattle")],
        &[(O, 0x0c, "Andrew Yates")],
        &[(CN, 0x0c, "Andrew Yates")],
    ])
}

#[test]
fn a_subject_is_written_the_way_windows_writes_it() {
    let body = |n: Vec<u8>| Der(&n).expect(0x30).unwrap().body.to_vec();
    assert_eq!(subject_string(&body(publisher_name())).unwrap(), PUBLISHER);
    let ev = name(&[
        &[("1.3.6.1.4.1.311.60.2.1.3", 0x13, "US")],
        &[("2.5.4.15", 0x0c, "Private Organization")],
        &[("2.5.4.5", 0x13, "601 544 101")],
        &[(C, 0x13, "US"), (O, 0x14, "Caf\u{e9}")],
        &[("2.5.4.11", 0x0c, " leading"), ("2.5.4.11", 0x0c, "")],
        &[(O, 0x1e, "Yates, \"Andrew\"")],
        &[("1.2.840.113549.1.9.1", 0x16, "a@example.com")],
        &[(CN, 0x1e, "\u{c548}\u{b155} #1")],
    ]);
    assert_eq!(
        subject_string(&body(ev)).unwrap(),
        "CN=\"\u{c548}\u{b155} #1\", E=a@example.com, O=\"Yates, \"\"Andrew\"\"\", \
         OU=\" leading\" + OU=\"\", C=US + O=Caf\u{e9}, SERIALNUMBER=601 544 101, \
         OID.2.5.4.15=Private Organization, OID.1.3.6.1.4.1.311.60.2.1.3=US"
    );
    let utf8_t61 = tlv(
        0x30,
        &tlv(
            0x31,
            &tlv(0x30, &[oid(O), tlv(0x14, "Caf\u{e9}".as_bytes())].concat()),
        ),
    );
    assert_eq!(
        subject_string(&body(utf8_t61)).unwrap(),
        "O=Caf\u{e9}",
        "a TeletexString holding UTF-8 reads as UTF-8, as CertNameToStr reads it"
    );
    assert!(
        subject_string(&body(name(&[]))).is_err(),
        "an empty subject"
    );
    let odd = tlv(
        0x30,
        &tlv(0x31, &tlv(0x30, &[oid(CN), tlv(0x1e, &[0])].concat())),
    );
    assert!(subject_string(&body(odd)).is_err(), "an odd BMPString");
    let octets = tlv(
        0x30,
        &tlv(0x31, &tlv(0x30, &[oid(CN), tlv(0x04, b"x")].concat())),
    );
    assert!(
        subject_string(&body(octets)).is_err(),
        "not a directory string"
    );
}

#[test]
fn a_publisher_reads_as_windows_reads_a_distinguished_name() {
    let pairs = |v: &[(&str, &str)]| -> Vec<(String, String)> {
        v.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    };
    assert_eq!(
        components(PUBLISHER).unwrap(),
        pairs(&[
            ("CN", "Andrew Yates"),
            ("O", "Andrew Yates"),
            ("L", "Seattle"),
            ("C", "US")
        ])
    );
    assert_eq!(
        components("cn = \"Yates, \"\"Andrew\"\"\" ;o=A+oid.2.5.4.15=B,dnqualifier=q").unwrap(),
        pairs(&[
            ("CN", "Yates, \"Andrew\""),
            ("O", "A"),
            ("OID.2.5.4.15", "B"),
            ("dnQualifier", "q")
        ])
    );
    for bad in [
        "",
        "Andrew Yates",
        "CN=",
        "CN=A,",
        "CN=A, ",
        "XX=A",
        "OID.1=A",
        "OID.01.2=A",
        "CN=A=B",
        "CN=A O=B",
        "CN=\"A",
        "CN=\"A\" O=B",
        "CN=A#1",
    ] {
        assert!(components(bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn the_pin_admits_only_a_publisher_whose_one_cn_is_the_pin() {
    let pin = crate::tier_from("Andrew Yates");
    assert_eq!(admit(&Tier::Inactive, "O=anyone"), Ok(()));
    assert_eq!(admit(&pin, PUBLISHER), Ok(()));
    assert_eq!(admit(&pin, "CN=\"Andrew Yates\""), Ok(()));
    let why = admit(&pin, "CN=Andrew Yate, O=Andrew Yates").unwrap_err();
    assert!(
        why.contains("`Andrew Yate`")
            && why.contains("`Andrew Yates`")
            && why.contains("0x80080204"),
        "{why}"
    );
    assert!(admit(&pin, "O=Andrew Yates").is_err(), "no CN");
    assert!(
        admit(&pin, "CN=Andrew Yates + CN=Other").is_err(),
        "two CNs"
    );
}

#[test]
fn the_identity_is_the_one_under_the_root_and_must_carry_a_publisher() {
    assert_eq!(
        identity_publisher(&manifest(Some("CN=A & B"))).unwrap(),
        "CN=A & B"
    );
    let why = identity_publisher(&manifest(None)).unwrap_err();
    assert!(why.contains("no Publisher"), "{why}");
    assert!(
        identity_publisher("<Package><Properties/></Package>")
            .unwrap_err()
            .contains("no Identity")
    );
    assert!(
        identity_publisher(
            "<Package><Identity Publisher=\"CN=A\"/><Identity Publisher=\"CN=A\"/></Package>"
        )
        .unwrap_err()
        .contains("more than one")
    );
    assert!(identity_publisher("<Package><Identity Publisher=\"CN=A\"></Package>").is_err());
    let bundle = "<Bundle><Identity Name=\"aterm\" Publisher=\"CN=A\"/><Packages/></Bundle>";
    assert_eq!(identity_publisher(bundle).unwrap(), "CN=A");
}

#[test]
fn the_signer_is_its_issuer_and_serial_not_a_position_in_the_set() {
    assert_eq!(
        signer_subject(&signed_by(&publisher_name())).unwrap(),
        PUBLISHER
    );
    let leaf = cert(&[0x33, 0x01], &ca(), &publisher_name());
    let by_key = tlv(0x80, &[1, 2, 3]);
    assert!(
        signer_subject(&p7x_with(std::slice::from_ref(&leaf), by_key, 1))
            .unwrap_err()
            .contains("key id")
    );
    let sid = || tlv(0x30, &[ca(), tlv(0x02, &[0x33, 0x01])].concat());
    assert!(
        signer_subject(&p7x_with(std::slice::from_ref(&leaf), sid(), 2))
            .unwrap_err()
            .contains("more than one signer")
    );
    let stranger = tlv(0x30, &[ca(), tlv(0x02, &[0x44])].concat());
    assert!(
        signer_subject(&p7x_with(&[leaf], stranger, 1))
            .unwrap_err()
            .contains("no certificate for its signer")
    );
    let bare = signed_by(&publisher_name());
    assert!(signer_subject(&bare[4..]).unwrap_err().contains("PKCX"));
    assert!(
        signer_subject(&bare[..bare.len() - 1]).is_err(),
        "truncated"
    );
}

#[test]
fn package_kinds_are_their_extensions() {
    for (file, kind) in [
        ("aterm.msix", Some(Kind::Package)),
        ("aterm.APPX", Some(Kind::Package)),
        ("aterm.msixbundle", Some(Kind::Bundle)),
        ("aterm.appxbundle", Some(Kind::Bundle)),
        ("aterm.exe", None),
        ("msix", None),
    ] {
        assert_eq!(Kind::of(Path::new(file)), kind, "{file}");
    }
    assert_eq!(
        Kind::Bundle.manifest(),
        "AppxMetadata/AppxBundleManifest.xml"
    );
}
