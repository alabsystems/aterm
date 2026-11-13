// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

fn polymod(values: &[u8]) -> u32 {
    const GEN: [u32; 5] = [
        0x3b6a_57b2,
        0x2650_8e6d,
        0x1ea1_19fa,
        0x3d42_33dd,
        0x2a14_62b3,
    ];
    let mut check: u32 = 1;
    for value in values {
        let top = check >> 25;
        check = ((check & 0x01ff_ffff) << 5) ^ u32::from(*value);
        for (i, g) in GEN.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                check ^= g;
            }
        }
    }
    check
}

/// BIP 173 Bech32 of `data` under `hrp` (a real checksum, so these recipients also pass
/// the full age decoder once it replaces the structural check).
fn bech32(hrp: &str, data: &[u8]) -> String {
    let mut five = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for byte in data {
        acc = ((acc << 8) | u32::from(*byte)) & 0xffff;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            five.push(((acc >> bits) & 31) as u8);
        }
    }
    if bits > 0 {
        five.push(((acc << (5 - bits)) & 31) as u8);
    }
    let mut values: Vec<u8> = hrp.bytes().map(|c| c >> 5).collect();
    values.push(0);
    values.extend(hrp.bytes().map(|c| c & 31));
    values.extend(&five);
    values.extend([0u8; 6]);
    let check = polymod(&values) ^ 1;
    let sum = (0..6).map(|i| ((check >> (5 * (5 - i))) & 31) as u8);
    let tail: String = five
        .iter()
        .copied()
        .chain(sum)
        .map(|v| CHARSET[usize::from(v)] as char)
        .collect();
    format!("{hrp}1{tail}")
}

/// A synthetic recipient from a fixed seed.
fn recipient(seed: u8) -> String {
    let key: [u8; 32] = std::array::from_fn(|i| {
        seed.wrapping_mul(29)
            .wrapping_add((i as u8).wrapping_mul(7))
    });
    bech32("age", &key)
}

fn vault() -> String {
    format!(
        "# comment\n\
         \n\
         [machines]\n\
         m22 = {}\n\
         m3={}\n\
         \x20 m9   =   {}\n\
         \n\
         [admins]\n\
         m22\n\
         \n\
         [everyone]\n\
         gh_token=a value with spaces # and = signs  \n\
         notes.txt<<END\n\
         # not a comment\n\
         [not a header]\n\
         \n\
         END\n\
         \n\
         [m22]\n\
         ~/.aterm/release.toml<<END\n\
         key = 1\n\
         END\n\
         gh_token=m22 override\n\
         blob<<END base64\n\
         AAEC\n\
         /w==\n\
         END\n\
         \n\
         [m3 m22]\n\
         shared=both\n",
        recipient(1),
        recipient(2),
        recipient(3)
    )
}

fn doc() -> Document {
    Document::parse(&vault()).expect("valid vault")
}

fn text(document: &Document) -> String {
    String::from_utf8(document.render().as_slice().to_vec()).expect("utf-8")
}

fn hosts(names: &[&str]) -> Audience {
    Audience::Hosts(names.iter().map(|h| (*h).to_string()).collect())
}

fn value(entries: &[Entry], name: &str) -> Vec<u8> {
    entries
        .iter()
        .find(|e| e.name == name)
        .expect(name)
        .value
        .as_slice()
        .to_vec()
}

fn error(text: &str) -> TextError {
    Document::parse(text).expect_err("refused")
}

/// A minimal valid vault with `extra` appended.
fn base(extra: &str) -> String {
    format!(
        "[machines]\nm22 = {}\nm3 = {}\n[admins]\nm22\n{extra}",
        recipient(1),
        recipient(2)
    )
}

#[test]
fn an_unedited_document_renders_byte_for_byte() {
    let source = vault();
    assert_eq!(text(&doc()), source);
    let crlf = source.replace('\n', "\r\n");
    assert_eq!(text(&Document::parse(&crlf).expect("crlf")), source);
    let unterminated = source.strip_suffix('\n').expect("final LF");
    assert_eq!(
        text(&Document::parse(unterminated).expect("no final LF")),
        source
    );
}

#[test]
fn machines_admins_and_entries_are_read() {
    let d = doc();
    let machines = d.machines();
    assert_eq!(
        machines.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        ["m22", "m3", "m9"]
    );
    assert_eq!(machines[2].recipient, recipient(3));
    assert_eq!(d.admins(), ["m22"]);
    let entries = d.entries();
    let names: Vec<(&str, &Audience)> = entries
        .iter()
        .map(|e| (e.name.as_str(), &e.audience))
        .collect();
    assert_eq!(
        names,
        [
            ("gh_token", &Audience::Everyone),
            ("notes.txt", &Audience::Everyone),
            ("~/.aterm/release.toml", &hosts(&["m22"])),
            ("gh_token", &hosts(&["m22"])),
            ("blob", &hosts(&["m22"])),
            ("shared", &hosts(&["m22", "m3"])),
        ]
    );
    assert_eq!(
        entries[0].value.as_slice(),
        b"a value with spaces # and = signs  "
    );
    assert_eq!(
        entries[1].value.as_slice(),
        b"# not a comment\n[not a header]\n\n"
    );
    assert_eq!(entries[2].value.as_slice(), b"key = 1\n");
    assert_eq!(entries[4].value.as_slice(), [0u8, 1, 2, 0xff]);
}

#[test]
fn host_headers_are_sorted_and_deduplicated() {
    let d = Document::parse(&base("[m3 m22 m3]\na=1\n")).expect("valid");
    assert_eq!(d.entries()[0].audience, hosts(&["m22", "m3"]));
    assert_eq!(hosts(&["m22", "m3"]).header(), "[m22 m3]");
    assert_eq!(Audience::Everyone.header(), "[everyone]");
    assert!(Audience::Everyone.includes("anything"));
    assert!(hosts(&["m22", "m3"]).includes("m3"));
    assert!(!hosts(&["m22"]).includes("m3"));
}

#[test]
fn heredocs_empty_and_one_empty_line() {
    let d = Document::parse(&base(
        "[everyone]\nnone<<END\nEND\nblank<<END\n\nEND\nb64<<END base64\nEND\nempty=\n",
    ))
    .expect("valid");
    let entries = d.entries();
    assert_eq!(value(&entries, "none"), b"");
    assert_eq!(value(&entries, "blank"), b"\n");
    assert_eq!(value(&entries, "b64"), b"");
    assert_eq!(value(&entries, "empty"), b"");
}

#[test]
fn entries_for_a_host_override_everyone_by_name() {
    let d = doc();
    let m22 = merge_for("m22", d.entries()).expect("merges");
    assert_eq!(
        m22.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        [
            "blob",
            "gh_token",
            "notes.txt",
            "shared",
            "~/.aterm/release.toml"
        ]
    );
    assert_eq!(value(&m22, "gh_token"), b"m22 override");
    let m3 = merge_for("m3", d.entries()).expect("merges");
    assert_eq!(
        m3.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        ["gh_token", "notes.txt", "shared"]
    );
    assert_eq!(
        value(&m3, "gh_token"),
        b"a value with spaces # and = signs  "
    );
    let m9 = merge_for("m9", d.entries()).expect("merges");
    assert_eq!(
        m9.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        ["gh_token", "notes.txt"]
    );
}

#[test]
fn lockers_are_everyone_first_then_sorted_audiences_and_read_back() {
    let d = doc();
    let plans = d.lockers();
    let audiences: Vec<&Audience> = plans.iter().map(|p| &p.audience).collect();
    assert_eq!(
        audiences,
        [
            &Audience::Everyone,
            &hosts(&["m22"]),
            &hosts(&["m22", "m3"])
        ]
    );
    let mut all = vec![recipient(1), recipient(2), recipient(3)];
    all.sort();
    assert_eq!(plans[0].recipients, all);
    assert_eq!(plans[1].recipients, [recipient(1)]);
    let mut two = vec![recipient(1), recipient(2)];
    two.sort();
    assert_eq!(plans[2].recipients, two);
    let entries = d.entries();
    for plan in &plans {
        let back = parse_locker(plan.text.as_slice()).expect("locker parses");
        let want: Vec<&Entry> = entries
            .iter()
            .filter(|e| e.audience == plan.audience)
            .collect();
        assert_eq!(
            back.iter().collect::<Vec<_>>(),
            want,
            "{}",
            plan.audience.header()
        );
        assert!(
            plan.text
                .as_slice()
                .starts_with(plan.audience.header().as_bytes())
        );
    }
    let merged: Vec<Entry> = plans
        .iter()
        .filter(|p| p.audience.includes("m22"))
        .flat_map(|p| parse_locker(p.text.as_slice()).expect("parses"))
        .collect();
    assert_eq!(
        merge_for("m22", merged).expect("merges"),
        merge_for("m22", d.entries()).expect("merges")
    );
}

#[test]
fn the_everyone_locker_exists_without_entries_and_sections_accumulate() {
    let d = Document::parse(&base("[m3]\na=1\n[m22]\nb=2\n[m3]\nc=3\n")).expect("valid");
    let plans = d.lockers();
    assert_eq!(plans[0].audience, Audience::Everyone);
    assert_eq!(plans[0].text.as_slice(), b"[everyone]\n");
    assert_eq!(plans.len(), 3);
    assert_eq!(plans[2].audience, hosts(&["m3"]));
    assert_eq!(plans[2].text.as_slice(), b"[m3]\na=1\nc=3\n");
}

#[test]
fn a_locker_refuses_machines_and_admins_and_does_not_check_hosts() {
    let entries = parse_locker(b"[nobody-known]\na=1\n").expect("hosts unchecked");
    assert_eq!(entries[0].audience, hosts(&["nobody-known"]));
    assert_eq!(parse_locker(b"[machines]\n").expect_err("refused").line, 1);
    assert_eq!(
        parse_locker(b"[everyone]\n[admins]\n")
            .expect_err("refused")
            .line,
        2
    );
    assert!(parse_locker(b"").expect("empty").is_empty());
    let bad = parse_locker(b"[everyone]\na=1\nb=\xff\n").expect_err("not utf-8");
    assert_eq!(bad.line, 3);
}

#[test]
fn merge_for_reports_a_clash_between_two_host_audiences() {
    let entry = |name: &str, v: &str, audience: Audience| Entry {
        name: name.to_string(),
        value: SecretBytes::from(v.as_bytes()),
        audience,
        if_absent: false,
        anchor: None,
    };
    let clash = merge_for(
        "m3",
        vec![
            entry("a", "x", hosts(&["m3"])),
            entry("a", "y", hosts(&["m22", "m3"])),
        ],
    )
    .expect_err("clash");
    assert!(clash.why.contains("`a`"), "{clash}");
    let merged = merge_for(
        "m22",
        vec![
            entry("a", "x", hosts(&["m3"])),
            entry("a", "y", hosts(&["m22", "m3"])),
            entry("a", "z", Audience::Everyone),
            entry("b", "w", Audience::Everyone),
        ],
    )
    .expect("only one includes m22");
    assert_eq!(merged.len(), 2);
    assert_eq!(value(&merged, "a"), b"y");
    assert!(
        merge_for(
            "m22",
            vec![
                entry("a", "1", Audience::Everyone),
                entry("a", "2", Audience::Everyone)
            ]
        )
        .is_err()
    );
}

#[test]
fn structural_errors_name_their_line() {
    let cases: Vec<(String, usize, &str)> = vec![
        ("a=1\n[machines]\n".to_string(), 1, "before the first"),
        (
            base("[foo bar baz]\n"),
            6,
            "host `bar` is not in [machines]",
        ),
        (base("[Bad]\n"), 6, "invalid host name"),
        (base("[]\n"), 6, "empty section"),
        (base("[m3 everyone]\n"), 6, "section of its own"),
        (base("[m3\n"), 6, "section header"),
        (base("[everyone] # x\n"), 6, "section header"),
        (base("[m99]\na=1\n"), 6, "host `m99` is not in [machines]"),
        (
            base("[everyone]\na=1\na=2\n"),
            8,
            "`a` appears twice in [everyone] (lines 7 and 8)",
        ),
        (
            base("[m3 m22]\na=1\n[m22 m3]\na=2\n"),
            9,
            "`a` appears twice in [m22 m3]",
        ),
        (
            base("[m3]\na=1\n[m3 m22]\na=2\n"),
            9,
            "`a` is set for host `m3` by both line 7 and line 9",
        ),
        (
            base("[everyone]\nno equals sign\n"),
            7,
            "expected name=value",
        ),
        (base("[everyone]\n a=1\n"), 7, "invalid entry name"),
        (base("[everyone]\na =1\n"), 7, "invalid entry name"),
        (base("[everyone]\na<b\n"), 7, "expected name=value"),
        (base("[everyone]\na<<end\nend\n"), 7, "TAG"),
        (base("[everyone]\na<<END text\nEND\n"), 7, "heredoc opens"),
        (base("[everyone]\nb=1\na<<END\nx\n"), 8, "never ends"),
        (base("[everyone]\na<<END base64\nZg\nEND\n"), 7, "base64"),
        (base("[everyone]\na<<END base64\nZh==\nEND\n"), 7, "base64"),
        (base("[everyone]\na=x\ry\n"), 7, "carriage return"),
        (base("[everyone]\na=x\0\n"), 7, "NUL"),
        (base("[machines]\nm4\n"), 7, "expected `name = age1…`"),
        (base("[machines]\nEvery = x\n"), 7, "invalid machine name"),
        (
            base("[machines]\neveryone = x\n"),
            7,
            "invalid machine name",
        ),
        (base("[admins]\nm3 m22\n"), 7, "one machine name per line"),
        (
            base("[admins]\nm22\n"),
            7,
            "admin `m22` is listed twice (lines 5 and 7)",
        ),
        (
            base("[admins]\nm77\n"),
            7,
            "admin `m77` is not in [machines]",
        ),
        (base("[machines]\nm22 = x\n"), 7, "invalid recipient"),
    ];
    for (source, line, why) in cases {
        let e = error(&source);
        assert_eq!(e.line, line, "{e} for {source:?}");
        assert!(e.why.contains(why), "{e} for {source:?}");
        assert_eq!(format!("{e}"), format!("line {line}: {}", e.why));
    }
}

#[test]
fn machine_lines_are_checked() {
    let dup_name = format!(
        "[machines]\nm22 = {}\nm22 = {}\n[admins]\nm22\n",
        recipient(1),
        recipient(2)
    );
    assert!(
        error(&dup_name)
            .why
            .contains("declared twice (lines 2 and 3)")
    );
    let dup_key = format!(
        "[machines]\nm22 = {}\nm3 = {}\n[admins]\nm22\n",
        recipient(1),
        recipient(1)
    );
    assert!(error(&dup_key).why.contains("same recipient"));
    let r = recipient(1);
    for bad in [
        r.to_uppercase(),
        r[..61].to_string(),
        format!("{r}q"),
        format!("{}b{}", &r[..10], &r[11..]),
        format!("age2{}", &r[4..]),
        format!("{r} # inline comment"),
    ] {
        let source = format!("[machines]\nm22 = {bad}\n[admins]\nm22\n");
        let e = error(&source);
        assert_eq!(e.line, 2, "{e}");
        assert!(e.why.contains("invalid recipient"), "{e}");
    }
}

#[test]
fn a_vault_needs_an_admin_but_a_locker_does_not() {
    let e = error(&format!("[machines]\nm22 = {}\n", recipient(1)));
    assert_eq!(e.line, 0);
    assert!(e.why.contains("no admin"), "{e}");
    assert_eq!(format!("{e}"), e.why);
    assert!(parse_locker(b"[everyone]\na=1\n").is_ok());
}

#[test]
fn sizes_are_bounded() {
    let big = "x".repeat(MAX_VALUE + 1);
    let e = error(&base(&format!("[everyone]\na={big}\n")));
    assert_eq!(e.line, 7);
    assert!(e.why.contains("larger"), "{e}");
    let fits = "x".repeat(MAX_VALUE);
    assert!(Document::parse(&base(&format!("[everyone]\na={fits}\n"))).is_ok());
    let heredoc = format!("[everyone]\na<<END\n{}\nEND\n", "y".repeat(MAX_VALUE));
    assert!(
        error(&base(&heredoc)).why.contains("larger"),
        "the final LF makes it one too many"
    );
    let encoded = crate::b64::encode_padded(&vec![7u8; MAX_VALUE + 1]);
    assert!(
        error(&base(&format!(
            "[everyone]\na<<END base64\n{encoded}\nEND\n"
        )))
        .why
        .contains("larger")
    );
    let encoded = crate::b64::encode_padded(&vec![7u8; MAX_VALUE]);
    let d = Document::parse(&base(&format!(
        "[everyone]\na<<END base64\n{encoded}\nEND\n"
    )))
    .expect("fits");
    assert_eq!(d.entries()[0].value.len(), MAX_VALUE);
    let huge = "#".repeat(MAX_TEXT + 1);
    assert_eq!(error(&huge).line, 0);
    assert!(error(&huge).why.contains("larger"));
}

#[test]
fn a_vault_names_as_many_sets_of_hosts_as_one_version_seals() {
    let names: Vec<String> = (1..=MAX_HOST_AUDIENCES + 1)
        .map(|n| format!("m{n}"))
        .collect();
    let mut source = String::from("[machines]\n");
    for (n, name) in names.iter().enumerate() {
        source.push_str(&format!("{name} = {}\n", recipient(n as u8)));
    }
    source.push_str("[admins]\nm1\n[m1 m2]\nshared=1\n");
    for name in &names[..MAX_HOST_AUDIENCES - 1] {
        source.push_str(&format!("[{name}]\nown=1\n"));
    }
    source.push_str("[m2 m1]\nalso=1\n");
    let mut d = Document::parse(&source).expect("62 sets of hosts");
    assert_eq!(
        d.lockers().len() + 1,
        crate::envelope::MAX_LOCKERS,
        "with the admins', a full version"
    );
    let before = text(&d);
    let last = Audience::Hosts(vec![names[MAX_HOST_AUDIENCES - 1].clone()]);
    let e = d.set_entry(&last, "own", b"1").expect_err("a 63rd");
    assert!(e.why.contains("distinct sets of hosts"), "{e}");
    assert_eq!(text(&d), before, "changes nothing");
    let e = error(&format!("{source}[m62]\nown=1\n"));
    assert_eq!(e.line, before.lines().count() + 1);
}

/// FAIL CLOSED: a process that registered no connector rows ([`on_cli_login`]), or whose
/// rows name no login at a path, still refuses each AI CLI's login in its default home, a
/// Claude Code `.credentials.json` a row keeps elsewhere, a Kimi Code credential file and a
/// pool row's login; a name the rows know keeps the rows' answer. A CLI's settings, a lock
/// beside a login and Claude Code's profile (`~/.claude.json`, no token) are entries.
#[test]
fn a_process_that_registered_no_rows_still_refuses_the_default_homes_logins() {
    let none_here: fn(&str) -> Option<&'static str> = |_| None;
    for registered in [None, Some(none_here)] {
        for login in [
            "~/.codex/auth.json",
            "~/.Codex/Auth.json",
            "~/.codex/auth.json.refreshed",
            "~/.claude/.credentials.json",
            "~/work/claude-2/.credentials.json",
            "~/.grok/auth.json",
            "~/.kimi-code/credentials/kimi-code.json",
            "~/.kimi-code/credentials/other.json",
            "~/.config/muse/auth.json",
            "~/.aterm/logins/codex/work/auth.json",
        ] {
            assert!(login_by(registered, login).is_some(), "{login}");
        }
        for entry in [
            "auth.json",
            "credentials.json",
            "~/.codex/config.toml",
            "~/.codex/auth.json.lock",
            "~/.claude/settings.json",
            "~/.claude.json",
            "~/.grok/settings.json",
            "~/.kimi-code/config.toml",
            "~/.kimi-code/credentials",
            "~/.aterm/release-credentials.toml",
            "~/.aterm/logins",
            "~/.config/codex/auth.json",
        ] {
            assert_eq!(login_by(registered, entry), None, "{entry}");
        }
    }
    let rows: fn(&str) -> Option<&'static str> =
        |name| (name == "~/.codex/auth.json").then_some("the rows' Codex");
    assert_eq!(
        login_by(Some(rows), "~/.codex/auth.json"),
        Some("the rows' Codex")
    );
}

#[test]
fn an_ai_clis_login_file_is_refused_as_an_entry_and_still_parses_where_one_stands() {
    let mut document = Document::parse(&base("")).expect("valid");
    for name in ["~/.codex/auth.json", "~/.Codex/Auth.json"] {
        assert!(
            document
                .set_entry(&Audience::Everyone, name, b"{}")
                .is_err(),
            "{name} is a CLI's login, no entry"
        );
    }
    let empty = document.clone();
    document
        .set_entry(
            &Audience::Everyone,
            "~/.codex/config.toml",
            b"model = \"x\"",
        )
        .expect("a document holds a CLI's settings path");
    assert!(
        document.added_refusal(&empty).is_some(),
        "a home path aterm does not name: no vault text adds it"
    );
    let held = Document::parse(&base("[everyone]\n~/.codex/auth.json={}\n"))
        .expect("a vault an older admin wrote one into still parses");
    assert!(held.added_refusal(&held).is_none(), "one it holds is kept");
    assert!(
        held.added_refusal(&document).is_some(),
        "one an edit adds is refused"
    );
}

/// No vault text adds or changes a name set apart or a Claude Code token, or makes two names
/// one file on a Mac's disk (DESIGN-claude-setup-token §3.1, §4.4); one a text an older build
/// wrote holds as it is stays, so its vault still opens. A token entry such a text holds is
/// in no text this build renders, no locker included.
#[test]
fn a_text_adds_no_name_set_apart_no_token_and_no_folded_pair() {
    const TOKEN: &str = "aterm-claude-token-ac-0123456789abcdef";
    let older = Document::parse(&base(&format!(
        "[everyone]\nfoo=1\nGH_ACCESS_TOKEN_ALABSYSTEMS=x\n{TOKEN}=inert-fixture\nbar=2\n"
    )))
    .expect("a text an older build wrote parses");
    assert!(older.added_refusal(&older).is_none(), "what it holds stays");
    let rendered = String::from_utf8(older.render().as_slice().to_vec()).unwrap();
    assert!(
        !rendered.contains(TOKEN) && !rendered.contains("inert-fixture"),
        "{rendered}"
    );
    assert!(
        rendered.contains("foo=1") && rendered.contains("bar=2"),
        "{rendered}"
    );
    for plan in older.lockers() {
        let text = String::from_utf8(plan.text.as_slice().to_vec()).unwrap();
        assert!(!text.contains(TOKEN), "{text}");
    }
    let refused = |change: &dyn Fn(&mut Document)| {
        let mut next = older.clone();
        change(&mut next);
        next.added_refusal(&older)
    };
    let set = |name: &'static str| {
        move |doc: &mut Document| doc.set_entry(&Audience::Everyone, name, b"v").unwrap()
    };
    let why = refused(&set("Foo")).expect("a folded pair");
    assert!(why.contains("`Foo` and `foo`"), "{why}");
    for name in ["~/.zshrc", "Release-Notary", "GH_ACCESS_TOKEN_ALABSYSTEMS"] {
        assert!(refused(&set(name)).is_some(), "{name}");
    }
    assert!(refused(&set("baz")).is_none(), "CONTROL: a shared name");
    assert!(
        refused(&set("gh_access_token_alabsystems")).is_none(),
        "the admin route keeps Held names"
    );
    let mut next = older.clone();
    assert!(next.set_entry(&Audience::Everyone, TOKEN, b"v").is_err());
    let typed = Document::parse(&base(&format!("[everyone]\n{TOKEN}=typed\n"))).unwrap();
    let why = typed.added_refusal(&older).expect("typed in");
    assert!(why.contains("Claude Code token"), "{why}");
}

#[test]
fn entry_and_machine_names() {
    for good in [
        "a",
        "gh_access_token",
        "accounts.txt",
        "A-1.b_c",
        "~/.aterm/release-credentials.toml",
        "~/x",
        "~/.ssh/config",
        "~/a.../b",
        "~/.config/aterm/other",
        "~/.config/aterm/secretsfoo",
    ] {
        assert!(valid_entry_name(good), "{good}");
    }
    let long = "a".repeat(129);
    let long_path = format!("~/{}", vec!["abcdefgh"; 120].join("/"));
    for bad in [
        "",
        ".hidden",
        "-dash",
        "_under",
        "a b",
        "a/b",
        "a=b",
        "~",
        "~/",
        "~/a/",
        "~//a",
        "~/./a",
        "~/../a",
        "~/a/..",
        "~/-a",
        "~/.secrets/x",
        "~/.Secrets/x",
        "~/.secrets",
        "~/.config/aterm/secrets/identity",
        "~/.CONFIG/Aterm/SECRETS/state.toml",
        long.as_str(),
        long_path.as_str(),
        "~/a\\b",
        "é",
    ] {
        assert!(!valid_entry_name(bad), "{bad:?}");
    }
    assert!(valid_entry_name(&"a".repeat(128)));
    for good in ["m22", "m3", "0", "a-b", "x".repeat(63).as_str()] {
        assert!(valid_machine_name(good), "{good}");
    }
    for bad in [
        "",
        "M22",
        "-a",
        "a_b",
        "a.b",
        "machines",
        "admins",
        "everyone",
        "x".repeat(64).as_str(),
    ] {
        assert!(!valid_machine_name(bad), "{bad}");
    }
}

#[test]
fn set_entry_replaces_in_place_and_keeps_comments() {
    let mut d = doc();
    d.set_entry(&Audience::Everyone, "gh_token", b"new")
        .expect("set");
    let want = vault().replace(
        "gh_token=a value with spaces # and = signs  \n",
        "gh_token=new\n",
    );
    assert_eq!(text(&d), want);
    d.set_entry(&Audience::Everyone, "notes.txt", b"one line")
        .expect("set");
    assert!(
        text(&d).contains("gh_token=new\nnotes.txt=one line\n\n[m22]\n"),
        "{}",
        text(&d)
    );
    assert!(text(&d).starts_with("# comment\n\n[machines]\n"));
}

#[test]
fn set_entry_appends_to_the_first_section_or_makes_one() {
    let mut d = Document::parse(&base(
        "[everyone]\na=1\n# trailing comment\n\n[m3]\nb=2\n[everyone]\nc=3\n",
    ))
    .expect("valid");
    d.set_entry(&Audience::Everyone, "new", b"v").expect("set");
    assert!(
        text(&d).contains("[everyone]\na=1\nnew=v\n# trailing comment\n"),
        "{}",
        text(&d)
    );
    d.set_entry(&hosts(&["m3", "m22", "m3"]), "pair", b"p")
        .expect("set");
    assert!(
        text(&d).ends_with("c=3\n\n[m22 m3]\npair=p\n"),
        "{}",
        text(&d)
    );
    let entries = merge_for("m3", d.entries()).expect("merges");
    assert_eq!(value(&entries, "pair"), b"p");
    assert_eq!(value(&entries, "new"), b"v");
}

#[test]
fn set_entry_spells_each_kind_of_value_so_it_reads_back() {
    let values: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"plain".to_vec(),
        b" spaced = # value ".to_vec(),
        b"line\n".to_vec(),
        b"\n".to_vec(),
        b"two\nlines\n".to_vec(),
        b"END\nEND1\nx\n".to_vec(),
        b"no final newline\nhere".to_vec(),
        b"crlf\r\n".to_vec(),
        b"nul\0".to_vec(),
        vec![0xff, 0xfe, 0, 1],
        (0..=255u8).cycle().take(1000).collect(),
    ];
    for (i, v) in values.iter().enumerate() {
        let mut d = Document::parse(&base("[everyone]\n")).expect("valid");
        let name = format!("v{i}");
        d.set_entry(&Audience::Everyone, &name, v).expect("set");
        let back = Document::parse(std::str::from_utf8(d.render().as_slice()).expect("utf-8"))
            .expect("re-parses");
        assert_eq!(value(&back.entries(), &name), *v, "value {i}");
        assert_eq!(value(&d.entries(), &name), *v, "value {i}");
    }
    let mut d = Document::parse(&base("[everyone]\n")).expect("valid");
    d.set_entry(&Audience::Everyone, "t", b"END\nEND1\nx\n")
        .expect("set");
    assert!(
        text(&d).contains("t<<END2\nEND\nEND1\nx\nEND2\n"),
        "{}",
        text(&d)
    );
    d.set_entry(&Audience::Everyone, "b", &[0u8; 100])
        .expect("set");
    let rendered = text(&d);
    let b64: Vec<&str> = rendered
        .split('\n')
        .skip_while(|l| *l != "b<<END base64")
        .skip(1)
        .take_while(|l| *l != "END")
        .collect();
    assert_eq!(b64.iter().map(|l| l.len()).collect::<Vec<_>>(), [64, 64, 8]);
}

#[test]
fn set_entry_refuses_what_would_break_the_rules_and_changes_nothing() {
    let mut d = Document::parse(&base("[m3]\na=1\n")).expect("valid");
    let before = text(&d);
    let e = d
        .set_entry(&hosts(&["m22", "m3"]), "a", b"2")
        .expect_err("overlap");
    assert!(e.why.contains("`a`"), "{e}");
    assert!(
        d.set_entry(&hosts(&["m99"]), "b", b"1").is_err(),
        "unknown host"
    );
    assert!(d.set_entry(&hosts(&[]), "b", b"1").is_err());
    assert!(d.set_entry(&hosts(&["Bad"]), "b", b"1").is_err());
    assert!(d.set_entry(&Audience::Everyone, "bad name", b"1").is_err());
    assert!(
        d.set_entry(&Audience::Everyone, "big", &vec![b'x'; MAX_VALUE + 1])
            .is_err()
    );
    assert_eq!(text(&d), before);
    assert_eq!(d.entries().len(), 1);
}

#[test]
fn unset_entry_removes_every_line_of_it() {
    let mut d = doc();
    assert!(d.unset_entry(&hosts(&["m22"]), "blob").expect("unset"));
    assert!(!text(&d).contains("AAEC"));
    assert!(
        d.unset_entry(&Audience::Everyone, "notes.txt")
            .expect("unset")
    );
    assert!(!text(&d).contains("not a comment"));
    assert!(
        !d.unset_entry(&Audience::Everyone, "notes.txt")
            .expect("gone")
    );
    assert!(
        !d.unset_entry(&hosts(&["m3"]), "shared")
            .expect("other audience")
    );
    assert!(!d.unset_entry(&hosts(&[]), "shared").expect("no audience"));
    assert!(
        d.unset_entry(&hosts(&["m3", "m22"]), "shared")
            .expect("unset")
    );
    let names: Vec<String> = d.entries().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["gh_token", "~/.aterm/release.toml", "gh_token"]);
    assert!(Document::parse(&text(&d)).is_ok());
}

#[test]
fn add_machine_appends_and_refuses_duplicates() {
    let mut d = doc();
    d.add_machine("m40", &recipient(40)).expect("added");
    assert!(text(&d).contains(&format!(
        "m9   =   {}\nm40 = {}\n\n[admins]",
        recipient(3),
        recipient(40)
    )));
    assert_eq!(
        d.machines().last().map(|m| m.name.clone()),
        Some("m40".to_string())
    );
    assert_eq!(d.lockers()[0].recipients.len(), 4);
    let before = text(&d);
    assert!(d.add_machine("m40", &recipient(41)).is_err());
    assert!(d.add_machine("m41", &recipient(40)).is_err());
    assert!(d.add_machine("M41", &recipient(41)).is_err());
    assert!(d.add_machine("everyone", &recipient(41)).is_err());
    assert!(d.add_machine("m41", "age1short").is_err());
    assert_eq!(text(&d), before);
}

#[test]
fn remove_machine_edits_headers_and_refuses_single_host_sections() {
    let mut d = doc();
    let before = text(&d);
    let e = d
        .remove_machine("m22", false)
        .expect_err("[m22] names only m22");
    assert_eq!(e.line, 19);
    assert_eq!(text(&d), before, "changes nothing");
    assert!(!d.remove_machine("m99", false).expect("absent"));
    assert!(d.remove_machine("m3", false).expect("removed"));
    let after = text(&d);
    assert!(!after.contains(&recipient(2)));
    assert!(after.contains("\n[m22]\nshared=both\n"), "{after}");
    assert_eq!(d.lockers().len(), 2);
    assert!(
        merge_for("m3", d.entries())
            .expect("merges")
            .iter()
            .all(|e| e.audience == Audience::Everyone)
    );
}

#[test]
fn remove_machine_can_drop_its_own_sections() {
    let mut d = Document::parse(&base(
        "[everyone]\nk=1\n\n[m3]\nmine=1\n# inside\nmore<<END\nx\nEND\n# next\n[m22]\ns=2\n",
    ))
    .expect("valid");
    d.add_admin("m3").expect("admin");
    assert!(d.remove_machine("m3", true).expect("removed"));
    let after = text(&d);
    assert!(
        !after.contains("mine=") && !after.contains("more<<") && !after.contains("# inside"),
        "{after}"
    );
    assert!(after.contains("k=1\n\n# next\n[m22]\ns=2\n"), "{after}");
    assert_eq!(d.admins(), ["m22"]);
    assert_eq!(d.machines().len(), 1);
}

/// `unset` of a host section's last entry leaves its header.
#[test]
fn a_section_its_entries_left_goes_with_the_machine() {
    let mut d = Document::parse(&base(
        "[everyone]\nk=1\n\n[m3]\ntoken=1\n# a note\n[m22]\ns=2\n",
    ))
    .expect("valid");
    assert!(d.unset_entry(&hosts(&["m3"]), "token").expect("unset"));
    assert!(text(&d).contains("\n[m3]\n"), "unset leaves the header");
    assert!(d.remove_machine("m3", false).expect("nothing left to lose"));
    let after = text(&d);
    assert!(!after.contains("[m3]"), "{after}");
    assert!(after.contains("k=1\n\n# a note\n[m22]\ns=2\n"), "{after}");
    assert_eq!(d.machines().len(), 1);
}

#[test]
fn the_last_admin_is_never_removed() {
    let mut d = Document::parse(&base("")).expect("valid");
    assert!(
        d.remove_machine("m22", false)
            .expect_err("last admin")
            .why
            .contains("no admin")
    );
    assert!(
        d.remove_admin("m22")
            .expect_err("last admin")
            .why
            .contains("last admin")
    );
    assert!(!d.remove_admin("m3").expect("not an admin"));
    d.add_admin("m3").expect("added");
    d.add_admin("m3").expect("no-op");
    assert_eq!(d.admins(), ["m22", "m3"]);
    assert!(text(&d).contains("[admins]\nm22\nm3\n"), "{}", text(&d));
    assert!(
        d.add_admin("m99")
            .expect_err("not a machine")
            .why
            .contains("not in [machines]")
    );
    assert!(d.remove_admin("m22").expect("removed"));
    assert_eq!(d.admins(), ["m3"]);
    assert!(d.remove_machine("m22", false).expect("no longer an admin"));
}

#[test]
fn a_new_vault_explains_itself_and_parses() {
    let d = Document::new_vault("m22", &recipient(1));
    let rendered = text(&d);
    assert!(rendered.starts_with("# aterm secrets vault"));
    assert!(rendered.ends_with("[everyone]\n"));
    let back = Document::parse(&rendered).expect("valid");
    assert_eq!(
        back.machines(),
        [Machine {
            name: "m22".into(),
            recipient: recipient(1)
        }]
    );
    assert_eq!(back.admins(), ["m22"]);
    assert!(back.entries().is_empty());
    let plans = d.lockers();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].recipients, [recipient(1)]);
    let mut d = d;
    d.set_entry(&Audience::Everyone, "first", b"1")
        .expect("set");
    assert!(text(&d).ends_with("[everyone]\nfirst=1\n"));
    let invalid = Document::new_vault("M22", &recipient(1));
    assert!(Document::parse(&text(&invalid)).is_err());
    let mut invalid = invalid;
    assert!(invalid.set_entry(&Audience::Everyone, "a", b"1").is_err());
}

#[test]
fn no_error_or_debug_output_ever_holds_a_value() {
    let sentinel = "Zq9!sentinel+value";
    let r = recipient(1);
    let head = format!(
        "[machines]\nm22 = {r}\nm3 = {}\n[admins]\nm22\n",
        recipient(2)
    );
    let cases = vec![
        format!("{sentinel}\n{head}"),
        format!("{head}[everyone]\na={sentinel}\na={sentinel}\n"),
        format!("{head}[everyone]\n{sentinel}\n"),
        format!("{head}[everyone]\n  a={sentinel}\n"),
        format!("{head}[everyone]\na<<END\n{sentinel}\n"),
        format!("{head}[everyone]\na<<END base64\n{sentinel}\nEND\n"),
        format!(
            "{head}[everyone]\na={}\n",
            sentinel.repeat(MAX_VALUE / sentinel.len() + 1)
        ),
        format!("{head}[everyone]\na={sentinel}\r{sentinel}\n"),
        format!("{head}[everyone]\na={sentinel}\0\n"),
        format!("{head}[machines]\nm4 = {sentinel}\n"),
        format!("{head}[machines]\n{sentinel}\n"),
        format!("{head}[admins]\n{sentinel}\n"),
        format!("{head}[{sentinel}]\n"),
        format!("{head}[everyone] {sentinel}\n"),
        format!("{head}[m3]\na={sentinel}\n[m3 m22]\na={sentinel}\n"),
        format!("{head}[m99]\na={sentinel}\n"),
        format!("[machines]\nm22 = {r}\n[everyone]\na={sentinel}\n"),
        format!("{head}[everyone]\na<<END if-absent=b\n{sentinel}\nEND\n"),
        format!("{head}[everyone]\nb={sentinel}\na<<END if-absent=b\n{sentinel}\nEND\n"),
        format!("{head}[everyone]\na<<END if-absent={sentinel}\nx\nEND\n"),
    ];
    let mut shown = Vec::new();
    for source in &cases {
        let e = Document::parse(source).expect_err("refused");
        shown.push(format!("{e} {e:?}"));
        let locker = source
            .replace("[machines]", "[m1]")
            .replace("[admins]", "[m2]");
        if let Err(e) = parse_locker(locker.as_bytes()) {
            shown.push(format!("{e} {e:?}"));
        }
    }
    let mut d = Document::parse(&format!("{head}[m3]\na={sentinel}\n")).expect("valid");
    shown.push(format!("{d:?}"));
    shown.push(format!("{:?}", d.entries()));
    shown.push(format!(
        "{:?}",
        merge_for("m3", d.entries()).expect("merges")
    ));
    shown.push(format!("{:?}", d.lockers()));
    let e = d
        .set_entry(&hosts(&["m3", "m22"]), "a", sentinel.as_bytes())
        .expect_err("overlap");
    shown.push(format!("{e} {e:?}"));
    let entries = parse_locker(format!("[m3]\na={sentinel}\n[m3 m22]\na={sentinel}\n").as_bytes())
        .expect_err("overlap in a locker");
    shown.push(format!("{entries} {entries:?}"));
    let everyone = Entry {
        name: "a".into(),
        value: SecretBytes::from(sentinel.as_bytes()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let e = merge_for("m3", vec![everyone.clone(), everyone]).expect_err("clash");
    shown.push(format!("{e} {e:?}"));
    for line in shown {
        assert!(!line.contains(sentinel), "{line}");
        assert!(!line.contains("sentinel"), "{line}");
    }
}

/// A join password `strength` accepts, made at run time.
fn strong(tag: &str) -> String {
    format!("{tag}-{}", "kmnpq".repeat(3))
}

#[test]
fn a_join_password_is_set_rendered_parsed_replaced_and_removed() {
    let mut d = doc();
    assert!(d.join_password().is_none());
    let first = strong("first");
    d.set_join_password(Some(format!("  {first}\n").as_bytes()))
        .expect("set");
    assert_eq!(
        d.join_password().expect("held").as_slice(),
        first.as_bytes()
    );
    let rendered = text(&d);
    assert_eq!(
        rendered,
        format!("{}\n[join]\npassword = {first}\n", vault())
    );
    let back = Document::parse(&rendered).expect("re-parses");
    assert_eq!(
        back.join_password().expect("held").as_slice(),
        first.as_bytes()
    );
    assert_eq!(back.entries(), d.entries());
    assert!(!format!("{d:?} {back:?} {:?}", d.join_password()).contains(&first));

    let second = format!("= #[x] {}", strong("second"));
    d.set_join_password(Some(second.as_bytes()))
        .expect("replaced");
    let rendered = text(&d);
    assert_eq!(rendered.matches("password = ").count(), 1, "{rendered}");
    let back = Document::parse(&rendered).expect("re-parses");
    assert_eq!(
        back.join_password().expect("held").as_slice(),
        second.as_bytes()
    );

    d.set_join_password(None).expect("removed");
    assert!(d.join_password().is_none());
    assert_eq!(text(&d), format!("{}\n[join]\n", vault()));
    assert!(
        Document::parse(&text(&d))
            .expect("re-parses")
            .join_password()
            .is_none()
    );
    d.set_join_password(None).expect("nothing to remove");
    d.set_join_password(Some(first.as_bytes())).expect("again");
    assert_eq!(
        text(&d),
        format!("{}\n[join]\npassword = {first}\n", vault())
    );

    let padded =
        Document::parse(&base(&format!("[join]\n\t password=\t{first} \x0c\n"))).expect("valid");
    assert_eq!(
        padded.join_password().expect("held").as_slice(),
        first.as_bytes()
    );
}

#[test]
fn weak_join_passwords_are_refused_without_quoting_them() {
    let mut d = doc();
    let before = text(&d);
    let long = format!("Zq9!{}", "x".repeat(300));
    let weak: Vec<Vec<u8>> = vec![
        b"Zq9!sent".to_vec(),
        b"   Zq9!sent   \n".to_vec(),
        b"Zq9!sentinel\nnext-line".to_vec(),
        b"Zq9!sentinel\x07bell".to_vec(),
        b"\xffZq9!sentinel-x".to_vec(),
        long.into_bytes(),
        Vec::new(),
    ];
    for password in &weak {
        let e = d.set_join_password(Some(password)).expect_err("refused");
        let shown = format!("{e} {e:?}");
        assert!(!shown.contains("Zq9") && !shown.contains("sent"), "{shown}");
        assert_eq!(e.line, 0);
    }
    assert_eq!(text(&d), before, "a refused password changes nothing");
    assert!(d.join_password().is_none());

    let e = error(&base(&format!("[join]\npassword = {}\n", "Zq9!sent")));
    assert_eq!(e.line, 7);
    assert!(e.why.contains("at least"), "{e}");
    assert!(!format!("{e} {e:?}").contains("Zq9"));
    let pw = strong("k");
    for item in [
        format!("passwd = {pw}"),
        format!("Password = {pw}"),
        pw.clone(),
        "a=1".to_string(),
        "a<<END\nx\nEND".to_string(),
    ] {
        let e = error(&base(&format!("[join]\n{item}\n")));
        assert_eq!(e.line, 7, "{e}");
        assert!(e.why.contains("expected `password = …`"), "{e}");
        assert!(!format!("{e:?}").contains(&pw));
    }
}

#[test]
fn a_vault_holds_one_join_password_at_most() {
    let (a, b) = (strong("a"), strong("b"));
    let e = error(&base(&format!("[join]\npassword = {a}\npassword = {b}\n")));
    assert_eq!(e.line, 8);
    assert!(e.why.contains("one password (lines 7 and 8)"), "{e}");
    assert!(!format!("{e:?}").contains(&a) && !format!("{e:?}").contains(&b));
    let e = error(&base(&format!(
        "[join]\npassword = {a}\n[everyone]\nx=1\n[join]\npassword = {b}\n"
    )));
    assert_eq!(e.line, 11);
    let mut d = Document::parse(&base(&format!(
        "[join]\n# none here\n[everyone]\nx=1\n[join]\npassword = {a}\n"
    )))
    .expect("valid");
    assert_eq!(d.join_password().expect("held").as_slice(), a.as_bytes());
    d.set_join_password(Some(b.as_bytes())).expect("replaced");
    assert!(
        text(&d).ends_with(&format!("[join]\npassword = {b}\n")),
        "{}",
        text(&d)
    );
    assert!(text(&d).contains("[join]\n# none here\n[everyone]"));
}

#[test]
fn an_open_vault_is_set_rendered_parsed_and_closed() {
    let pw = strong("pw");
    let mut d = Document::parse(&base(&format!("[join]\npassword = {pw}\n"))).expect("valid");
    assert!(!d.open());
    d.set_open(true).expect("open");
    let note = format!("# {OPEN_NOTE}");
    assert!(
        text(&d).ends_with(&format!("[join]\n{note}\n")),
        "{}",
        text(&d)
    );
    assert!(d.join_password().is_none());
    let reread = Document::parse(&text(&d)).expect("parses");
    assert!(reread.open());
    assert_eq!(reread.render().as_slice(), d.render().as_slice());
    let mut again = reread.clone();
    again.set_open(true).expect("same");
    assert_eq!(text(&again), text(&reread));
    // An earlier aterm's line, however spelled, is read open and written as the note.
    let mut spelled = Document::parse(&base("[join]\n  open=yes \n")).expect("valid");
    assert!(spelled.open() && spelled.open_line());
    spelled.set_open(true).expect("noted");
    assert!(spelled.open() && !spelled.open_line());
    assert_eq!(text(&spelled), base(&format!("[join]\n{note}\n")));
    d.set_join_password(Some(pw.as_bytes())).expect("set");
    assert!(!d.open());
    assert!(text(&d).ends_with(&format!("[join]\npassword = {pw}\n")));
    d.set_open(false).expect("not open");
    assert!(d.join_password().is_some());
    let mut open = Document::parse(&base("[join]\nopen = yes\n[everyone]\nx=1\n")).expect("valid");
    open.set_open(false).expect("close");
    assert_eq!(text(&open), base("[join]\n[everyone]\nx=1\n"));
    assert!(!open.open());
}

/// The open note ([`OPEN_NOTE`]) opens a vault only word for word, only in `[join]` and only
/// with no join password beside it, and is a comment an earlier aterm reads past (it reads
/// no `open` line): setting a password drops it, opening drops the password or line beside
/// it, and closing drops every note and the line.
///
/// NEGATIVE CONTROLS: with `set_open(true)` writing `open = yes` again, `open_line` reads
/// true and this test fails at its first assert; with `open` reading a note whatever stands
/// beside it, the note beside a password reads open and it fails.
#[test]
fn an_open_note_opens_only_this_aterm_and_gives_way_to_a_password() {
    let pw = strong("pw");
    let note = format!("# {OPEN_NOTE}");
    let mut d = doc();
    d.set_open(true).expect("open");
    assert!(d.open() && !d.open_line());
    assert!(
        !text(&d)
            .lines()
            .any(|line| line.trim_start().starts_with("open")),
        "{}",
        text(&d)
    );
    let beside = Document::parse(&base(&format!("[join]\n{note}\npassword = {pw}\n")))
        .expect("a note beside a password parses");
    assert!(!beside.open());
    assert_eq!(
        Some(pw.as_bytes()),
        beside.join_password().as_ref().map(SecretBytes::as_slice)
    );
    let mut opened = beside.clone();
    opened.set_open(true).expect("open");
    assert!(opened.open() && opened.join_password().is_none());
    assert_eq!(text(&opened), base(&format!("[join]\n{note}\n")));
    let mut locked = opened.clone();
    locked
        .set_join_password(Some(pw.as_bytes()))
        .expect("password");
    assert!(!locked.open());
    assert_eq!(text(&locked), base(&format!("[join]\npassword = {pw}\n")));
    for elsewhere in [
        format!("[everyone]\n{note}\nx=1\n"),
        "[join]\n# open = yes\n".to_string(),
        format!("[join]\n# {OPEN_NOTE} please\n"),
    ] {
        assert!(
            !Document::parse(&base(&elsewhere)).expect("parses").open(),
            "{elsewhere}"
        );
    }
    let mut both = Document::parse(&base(&format!(
        "[join]\nopen = yes\n{note}\n{note}\n[everyone]\nx=1\n"
    )))
    .expect("valid");
    let mut noted = both.clone();
    noted.set_open(true).expect("open");
    assert!(noted.open() && !noted.open_line());
    both.set_open(false).expect("close");
    assert!(!both.open());
    assert_eq!(text(&both), base("[join]\n[everyone]\nx=1\n"));
}

#[test]
fn an_open_line_is_checked_and_never_beside_a_password() {
    let pw = strong("pw");
    let e = error(&base(&format!("[join]\nopen = yes\npassword = {pw}\n")));
    assert_eq!(e.line, 8);
    assert!(e.why.contains("not both (lines 7 and 8)"), "{e}");
    assert!(!format!("{e:?}").contains(&pw));
    let e = error(&base("[join]\nopen = yes\n[join]\nopen = yes\n"));
    assert!(e.why.contains("one `open` line"), "{e}");
    for bad in [
        "open = no",
        "open = YES",
        "open =",
        "open = yes please",
        "open = after 5",
        "opened = yes",
    ] {
        let e = error(&base(&format!("[join]\n{bad}\n")));
        assert_eq!(e.line, 7, "{bad}");
        assert!(e.why.contains("open = yes"), "{bad}: {e}");
    }
    assert!(parse_locker(b"[join]\nopen = yes\n").is_err());
    let mut d = doc();
    d.set_open(true).expect("open");
    for plan in d.lockers() {
        let body = std::str::from_utf8(plan.text.as_slice()).expect("utf-8");
        assert!(!body.contains("open") && !body.contains("[join]"), "{body}");
    }
}

#[test]
fn a_code_only_key_is_named_once_and_dropped_when_it_is_let_in() {
    let pw = strong("pw");
    let gone = recipient(9);
    let mut d = Document::parse(&base(&format!("[join]\npassword = {pw}\n"))).expect("valid");
    assert!(d.code_only().is_empty());
    d.add_code_only(&gone).expect("code-only");
    d.add_code_only(&gone).expect("again");
    assert_eq!(d.code_only(), std::slice::from_ref(&gone));
    assert!(text(&d).ends_with(&format!("[join]\npassword = {pw}\ncode-only = {gone}\n")));
    assert_eq!(
        Document::parse(&text(&d)).expect("parses").code_only(),
        std::slice::from_ref(&gone)
    );
    d.set_open(true).expect("open");
    assert!(text(&d).ends_with(&format!("[join]\n# {OPEN_NOTE}\ncode-only = {gone}\n")));
    d.set_open(false).expect("close");
    assert_eq!(d.code_only(), std::slice::from_ref(&gone));
    d.set_join_password(Some(pw.as_bytes())).expect("set");
    assert_eq!(d.code_only(), std::slice::from_ref(&gone));
    let before = text(&d);
    assert!(d.add_code_only("age1nope").is_err());
    assert_eq!(text(&d), before);
    let e = error(&base("[join]\ncode-only = age1nope\n"));
    assert_eq!(e.line, 7);
    assert!(e.why.contains("invalid recipient"), "{e}");
    let e = error(&base(&format!(
        "[join]\ncode-only = {gone}\ncode-only={gone}\n"
    )));
    assert_eq!(e.line, 8);
    assert!(e.why.contains("(lines 7 and 8)"), "{e}");
    let other = recipient(8);
    d.add_code_only(&other).expect("code-only");
    d.add_machine("back", &gone).expect("add");
    assert_eq!(d.code_only(), std::slice::from_ref(&other));
    assert!(!text(&d).contains(&format!("code-only = {gone}")));
    for plan in d.lockers() {
        let body = std::str::from_utf8(plan.text.as_slice()).expect("utf-8");
        assert!(
            !body.contains("code-only") && !body.contains(&other),
            "{body}"
        );
    }
}

#[test]
fn a_code_only_note_is_a_comment_a_vault_reads_as_before() {
    let pw = strong("pw");
    let (gone, held) = (recipient(9), recipient(8));
    let source = base(&format!(
        "[join]\npassword = {pw}\ncode-only = {gone}\ncode-only = {held}\n\n[everyone]\na=1\n"
    ));
    let mut d = Document::parse(&source).expect("valid");
    d.set_code_only_notes(
        &BTreeMap::from([(gone.clone(), 7), (held.clone(), 3)]),
        Some((2, 9)),
    )
    .expect("noted");
    let block = format!(
        "# code-only {gone} named in version 7\n\
         # code-only {held} named in version 3\n\
         # code-only lines noted from version 2 through version 9\n"
    );
    assert_eq!(
        text(&d),
        source.replace("\n\n[everyone]", &format!("\n{block}\n[everyone]"))
    );

    // Every parse reads past them: the vault reads as it does with no comment at all.
    let noted = Document::parse(&text(&d)).expect("parses");
    let bare: String = text(&d)
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .map(|line| format!("{line}\n"))
        .collect();
    let bare = Document::parse(&bare).expect("parses");
    assert_eq!(noted.machines(), bare.machines());
    assert_eq!(noted.admins(), bare.admins());
    assert_eq!(
        noted.join_password().map(|p| p.as_slice().to_vec()),
        bare.join_password().map(|p| p.as_slice().to_vec())
    );
    assert_eq!(noted.open(), bare.open());
    assert_eq!(noted.code_only(), bare.code_only());
    assert_eq!(noted.entries(), bare.entries());
    assert_eq!(
        noted.code_only_named_in(),
        BTreeMap::from([(gone.clone(), 7), (held.clone(), 3)])
    );
    assert_eq!(noted.code_only_noted(), Some((2, 9)));
    assert_eq!(bare.code_only_named_in(), BTreeMap::new());
    assert_eq!(bare.code_only_noted(), None);

    // A second set rewrites them in place, a key it does not name with no note, and none
    // takes them away.
    d.set_code_only_notes(&BTreeMap::from([(held.clone(), 4)]), Some((4, 10)))
        .expect("noted again");
    let block = format!(
        "# code-only {held} named in version 4\n\
         # code-only lines noted from version 4 through version 10\n"
    );
    assert_eq!(
        text(&d),
        source.replace("\n\n[everyone]", &format!("\n{block}\n[everyone]"))
    );
    d.set_code_only_notes(&BTreeMap::new(), None).expect("none");
    assert_eq!(text(&d), source);
    assert!(!d.code_only_notes());

    // With no `code-only` line there is nothing to note.
    let mut plain = Document::parse(&base(&format!("[join]\npassword = {pw}\n"))).expect("valid");
    let before = text(&plain);
    plain
        .set_code_only_notes(&BTreeMap::from([(gone.clone(), 1)]), Some((1, 1)))
        .expect("nothing");
    assert_eq!(text(&plain), before);

    // These are comments, not notes.
    let note = |n: &str| format!("# code-only {gone} named in version {n}");
    let run =
        |a: &str, b: &str| format!("# code-only lines noted from version {a} through version {b}");
    for comment in [
        format!("# code-only = {gone}"),
        "# code-only age1nope named in version 7".to_string(),
        format!("{} now", note("7")),
        note("seven"),
        note("+7"),
        note("123456789012345678901"),
        note("99999999999999999999"),
        run("9", "2"),
        run("2", "x"),
        format!("# code-only {held} named in version 7"),
    ] {
        let source = base(&format!(
            "[join]\npassword = {pw}\ncode-only = {gone}\n{comment}\n"
        ));
        let d = Document::parse(&source).expect("valid");
        assert_eq!(d.code_only_named_in(), BTreeMap::new(), "{comment}");
        assert_eq!(d.code_only_noted(), None, "{comment}");
    }
    let elsewhere = base(&format!(
        "[join]\ncode-only = {gone}\n[everyone]\n{}\n{}\n",
        note("7"),
        run("1", "7")
    ));
    let d = Document::parse(&elsewhere).expect("valid");
    assert_eq!(d.code_only_named_in(), BTreeMap::new(), "under [everyone]");
    assert_eq!(d.code_only_noted(), None, "under [everyone]");
    assert!(!d.code_only_notes(), "under [everyone]");
}

#[test]
fn the_join_password_is_a_whole_vaults_only() {
    let pw = strong("pw");
    let mut d = doc();
    d.set_join_password(Some(pw.as_bytes())).expect("set");
    d.set_entry(&hosts(&["m3"]), "mine", b"v").expect("set");
    let plans = d.lockers();
    assert_eq!(plans.len(), 4);
    for plan in &plans {
        let body = std::str::from_utf8(plan.text.as_slice()).expect("utf-8");
        assert!(
            !body.contains(&pw) && !body.contains("[join]") && !body.contains("password"),
            "{body}"
        );
        parse_locker(plan.text.as_slice()).expect("parses");
    }
    assert!(
        d.entries()
            .iter()
            .all(|e| e.value.as_slice() != pw.as_bytes())
    );

    let e = parse_locker(format!("[join]\npassword = {pw}\n").as_bytes()).expect_err("refused");
    assert_eq!(e.line, 1);
    let e = parse_locker(format!("[everyone]\na=1\n[join]\npassword = {pw}\n").as_bytes())
        .expect_err("refused");
    assert_eq!(e.line, 3);
    assert!(e.why.contains("[join]"), "{e}");
    assert!(!format!("{e} {e:?}").contains(&pw));

    let plain = parse_plain(format!("a=1\n[join]\npassword = {pw}\n[everyone]\nb=2\n").as_bytes())
        .expect("plain");
    assert_eq!(
        plain.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    let e = parse_plain(format!("[join]\npassword = {pw}\nc=3\n").as_bytes())
        .expect_err("an entry under [join]");
    assert_eq!(e.line, 3);
}

#[test]
fn no_machine_may_be_named_join() {
    let r = recipient(40);
    let e = error(&base(&format!("[machines]\njoin = {r}\n")));
    assert_eq!(e.line, 7);
    assert!(
        e.why.contains("not machines, admins, everyone or join"),
        "{e}"
    );
    assert_eq!(error(&base("[admins]\njoin\n")).line, 7);
    let e = error(&base("[m3 join]\na=1\n"));
    assert_eq!(e.line, 6);
    assert!(e.why.contains("`join` is a section of its own"), "{e}");
    assert!(!valid_machine_name("join"));
    assert!(valid_machine_name("join1") && valid_machine_name("joiner"));
    let mut d = doc();
    let before = text(&d);
    let e = d.add_machine("join", &r).expect_err("reserved");
    assert!(e.why.contains("join"), "{e}");
    assert!(d.set_entry(&hosts(&["join"]), "a", b"1").is_err());
    assert!(d.add_admin("join").is_err());
    assert!(!d.remove_machine("join", false).expect("no such machine"));
    assert_eq!(text(&d), before);
}

#[test]
fn if_absent_entries_round_trip_in_every_spelling() {
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "one",
            b"single line".to_vec(),
            "one<<END base64 if-absent\nc2luZ2xlIGxpbmU=\nEND\n",
        ),
        (
            "text",
            b"two\nlines\n".to_vec(),
            "text<<END if-absent\ntwo\nlines\nEND\n",
        ),
        ("empty", Vec::new(), "empty<<END base64 if-absent\nEND\n"),
        (
            "crlf",
            b"x\r\n".to_vec(),
            "crlf<<END base64 if-absent\neA0K\nEND\n",
        ),
        (
            "bin",
            vec![0xff, 0],
            "bin<<END base64 if-absent\n/wA=\nEND\n",
        ),
        (
            "tagged",
            b"END\n".to_vec(),
            "tagged<<END1 if-absent\nEND\nEND1\n",
        ),
    ];
    for (name, value, spelled) in &cases {
        let mut d = Document::parse(&base("[everyone]\n")).expect("valid");
        d.set_entry_with(&Audience::Everyone, name, value, true, None)
            .expect("set");
        let rendered = text(&d);
        assert!(
            rendered.ends_with(&format!("[everyone]\n{spelled}")),
            "{rendered}"
        );
        let back = Document::parse(&rendered).expect("re-parses");
        let entry = back
            .entries()
            .into_iter()
            .find(|e| e.name == *name)
            .expect(name);
        assert!(entry.if_absent, "{name}");
        assert_eq!(entry.value.as_slice(), value.as_slice(), "{name}");
        assert_eq!(back.entries(), d.entries(), "{name}");
    }
}

#[test]
fn if_absent_is_set_and_cleared_in_place_and_reaches_the_lockers() {
    let mut d = doc();
    d.set_entry_with(&hosts(&["m22"]), "~/.aterm/machine.key", b"k\n", true, None)
        .expect("set");
    d.set_entry_with(&Audience::Everyone, "gh_token", b"t", true, None)
        .expect("set");
    assert!(
        text(&d)
            .contains("[everyone]\ngh_token<<END base64 if-absent\ndA==\nEND\nnotes.txt<<END\n"),
        "{}",
        text(&d)
    );
    assert!(
        text(&d).contains("/w==\nEND\n~/.aterm/machine.key<<END if-absent\nk\nEND\n"),
        "{}",
        text(&d)
    );
    for plan in d.lockers() {
        let back = parse_locker(plan.text.as_slice()).expect("locker");
        let want: Vec<Entry> = d
            .entries()
            .into_iter()
            .filter(|e| e.audience == plan.audience)
            .collect();
        assert_eq!(back, want, "{}", plan.audience.header());
    }
    let m22: Vec<Entry> = d
        .lockers()
        .iter()
        .filter(|p| p.audience.includes("m22"))
        .flat_map(|p| parse_locker(p.text.as_slice()).expect("parses"))
        .collect();
    let merged = merge_for("m22", m22).expect("merges");
    let find = |name: &str| merged.iter().find(|e| e.name == name).expect(name);
    assert!(find("~/.aterm/machine.key").if_absent);
    assert_eq!(find("~/.aterm/machine.key").value.as_slice(), b"k\n");
    assert!(
        !find("gh_token").if_absent,
        "m22's own gh_token overrides the if-absent one"
    );
    assert!(
        merge_for("m3", d.entries())
            .expect("merges")
            .iter()
            .any(|e| e.name == "gh_token" && e.if_absent)
    );

    d.set_entry(&Audience::Everyone, "gh_token", b"t")
        .expect("set");
    assert!(
        text(&d).contains("[everyone]\ngh_token=t\nnotes.txt<<END\n"),
        "{}",
        text(&d)
    );
    assert!(!d.entries()[0].if_absent);
}

#[test]
fn the_if_absent_modifier_has_one_place() {
    for (extra, why) in [
        (
            "[everyone]\na<<END if-absent base64\nEND\n",
            "heredoc opens",
        ),
        ("[everyone]\na<<END  if-absent\nEND\n", "heredoc opens"),
        (
            "[everyone]\na<<END if-absent if-absent\nEND\n",
            "heredoc opens",
        ),
        ("[everyone]\na<<END IF-ABSENT\nEND\n", "heredoc opens"),
        ("[everyone]\na<<END\tif-absent\nEND\n", "TAG"),
        ("[everyone]\na<<if-absent\nEND\n", "TAG"),
    ] {
        let e = error(&base(extra));
        assert_eq!(e.line, 7, "{extra:?}");
        assert!(e.why.contains(why), "{e} for {extra:?}");
    }
    let d = Document::parse(&base(
        "[everyone]\na=v if-absent\nb<<END if-absent \t\nx\nEND\n",
    ))
    .expect("valid");
    let entries = d.entries();
    assert_eq!(entries[0].value.as_slice(), b"v if-absent");
    assert!(!entries[0].if_absent);
    assert!(
        entries[1].if_absent,
        "blanks after the modifier are dropped"
    );
    assert_eq!(entries[1].value.as_slice(), b"x\n");
}

#[test]
fn a_new_vault_takes_a_join_password_and_if_absent_entries() {
    let mut d = Document::new_vault("m22", &recipient(1));
    let pw = strong("new");
    d.set_join_password(Some(pw.as_bytes())).expect("password");
    d.set_entry_with(
        &Audience::Everyone,
        "~/.aterm/machine.key",
        &[0, 1, 2],
        true,
        None,
    )
    .expect("entry");
    d.add_machine("m3", &recipient(2)).expect("machine");
    let rendered = text(&d);
    assert!(rendered.starts_with("# aterm secrets vault"));
    assert!(
        rendered.ends_with(&format!(
            "[everyone]\n~/.aterm/machine.key<<END base64 if-absent\nAAEC\nEND\n\n[join]\npassword = {pw}\n"
        )),
        "{rendered}"
    );
    let back = Document::parse(&rendered).expect("valid");
    assert_eq!(
        back.join_password().expect("held").as_slice(),
        pw.as_bytes()
    );
    assert_eq!(back.entries(), d.entries());
    assert!(back.entries()[0].if_absent);
    assert_eq!(back.machines().len(), 2);
    assert_eq!(text(&back), rendered);
    assert_eq!(back.lockers()[0].recipients.len(), 2);
}

const KEY: &str = "~/.aterm/machine.key";
const PROFILE: &str = "~/.aterm/release-credentials.toml";

#[test]
fn anchored_entries_are_spelled_parsed_and_reach_the_lockers() {
    let mut d = Document::parse(&base("[everyone]\n")).expect("valid");
    d.set_entry_with(&Audience::Everyone, KEY, &[0xff, 0], true, None)
        .expect("key");
    d.set_entry_with(&Audience::Everyone, PROFILE, b"id = 1\n", true, Some(KEY))
        .expect("profile");
    d.set_entry_with(&Audience::Everyone, KEY, &[0xff, 0], true, Some(PROFILE))
        .expect("key anchored back");
    d.set_entry_with(
        &Audience::Everyone,
        "~/.aterm/machine.toml",
        b"one line",
        true,
        Some(KEY),
    )
    .expect("machine.toml");
    let rendered = text(&d);
    assert!(
        rendered.ends_with(&format!(
            "[everyone]\n\
             {KEY}<<END base64 if-absent={PROFILE}\n/wA=\nEND\n\
             {PROFILE}<<END if-absent={KEY}\nid = 1\nEND\n\
             ~/.aterm/machine.toml<<END base64 if-absent={KEY}\nb25lIGxpbmU=\nEND\n"
        )),
        "{rendered}"
    );
    let back = Document::parse(&rendered).expect("re-parses");
    assert_eq!(back.entries(), d.entries());
    let anchors: Vec<(String, Option<String>, bool)> = back
        .entries()
        .into_iter()
        .map(|e| (e.name, e.anchor, e.if_absent))
        .collect();
    assert_eq!(
        anchors,
        [
            (KEY.to_string(), Some(PROFILE.to_string()), true),
            (PROFILE.to_string(), Some(KEY.to_string()), true),
            (
                "~/.aterm/machine.toml".to_string(),
                Some(KEY.to_string()),
                true
            ),
        ]
    );
    assert_eq!(back.entries()[0].value.as_slice(), [0xff, 0]);
    for plan in d.lockers() {
        let entries = parse_locker(plan.text.as_slice()).expect("locker");
        assert_eq!(entries, d.entries());
        assert_eq!(
            merge_for("m3", entries).expect("merges"),
            merge_for("m3", d.entries()).expect("merges")
        );
    }

    d.set_entry_with(
        &Audience::Everyone,
        "~/.aterm/machine.toml",
        b"x\n",
        true,
        None,
    )
    .expect("unanchored");
    assert!(text(&d).ends_with("~/.aterm/machine.toml<<END if-absent\nx\nEND\n"));
    d.set_entry(&Audience::Everyone, "~/.aterm/machine.toml", b"x")
        .expect("plain");
    assert!(text(&d).ends_with("~/.aterm/machine.toml=x\n"));
}

#[test]
fn an_anchor_is_another_if_absent_entry_its_receivers_all_get() {
    let refused = [
        (
            "[everyone]\na<<END if-absent=b\nx\nEND\n",
            7,
            "anchored to `b`",
        ),
        (
            "[everyone]\nb=1\na<<END if-absent=b\nx\nEND\n",
            8,
            "anchored to `b`",
        ),
        (
            "[everyone]\nb<<END\n1\nEND\na<<END if-absent=b\nx\nEND\n",
            10,
            "anchored to `b`",
        ),
        (
            "[everyone]\na<<END if-absent=a\nx\nEND\n",
            7,
            "names another entry",
        ),
        (
            "[everyone]\na<<END if-absent=\nx\nEND\n",
            7,
            "names another entry",
        ),
        (
            "[everyone]\na<<END if-absent=../b\nx\nEND\n",
            7,
            "names another entry",
        ),
        (
            "[everyone]\na<<END if-absent=~/.secrets/b\nx\nEND\n",
            7,
            "names another entry",
        ),
        (
            "[m22]\nb<<END if-absent\n1\nEND\n[everyone]\na<<END if-absent=b\nx\nEND\n",
            11,
            "[everyone]",
        ),
        (
            "[everyone]\nb<<END if-absent\n1\nEND\na<<END if-absent=b\nx\nEND\n[m3]\nb=own\n",
            10,
            "host `m3`",
        ),
        (
            "[everyone]\nb<<END if-absent\n1\nEND\na<<END if-absent=b base64\nEND\n",
            10,
            "heredoc opens",
        ),
        (
            "[everyone]\nb<<END if-absent\n1\nEND\na<<END IF-ABSENT=b\nEND\n",
            10,
            "heredoc opens",
        ),
    ];
    for (extra, line, why) in refused {
        let e = error(&base(extra));
        assert_eq!(e.line, line, "{e} for {extra:?}");
        assert!(e.why.contains(why), "{e} for {extra:?}");
    }
    let d = Document::parse(&base(
        "[everyone]\nb<<END if-absent\n1\nEND\n[m22]\na<<END base64 if-absent=b\nAAEC\nEND\n",
    ))
    .expect("valid");
    assert_eq!(d.entries()[1].anchor.as_deref(), Some("b"));
    assert_eq!(merge_for("m22", d.entries()).expect("merges").len(), 2);
    assert_eq!(merge_for("m3", d.entries()).expect("merges").len(), 1);

    let mut d = Document::parse(&base(
        "[everyone]\nb<<END if-absent\n1\nEND\na<<END if-absent=b\nx\nEND\nc=1\n",
    ))
    .expect("valid");
    let before = text(&d);
    for result in [
        d.set_entry_with(&Audience::Everyone, "z", b"v\n", true, Some("c")),
        d.set_entry_with(&Audience::Everyone, "z", b"v\n", true, Some("nothing")),
        d.set_entry_with(&Audience::Everyone, "z", b"v\n", false, Some("b")),
        d.set_entry_with(&Audience::Everyone, "z", b"v\n", true, Some("z")),
        d.set_entry(&Audience::Everyone, "b", b"no longer if-absent"),
        d.unset_entry(&Audience::Everyone, "b").map(|_| ()),
    ] {
        assert!(result.is_err(), "{result:?}");
    }
    assert_eq!(text(&d), before);
    let locker = parse_locker(b"[m22]\na<<END if-absent=b\nx\nEND\n").expect("locker");
    let e = merge_for("m22", locker).expect_err("dangling");
    assert!(e.why.contains("`a` is anchored to `b`"), "{e}");
    let e = parse_plain(b"a<<END if-absent=b\nx\nEND\n").expect_err("dangling");
    assert_eq!(e.line, 1, "{e}");
}
