// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::nouns::{DRAWS, NOUNS, draw};
use super::*;

fn n(value: u32) -> Number {
    Number::new(value).unwrap()
}

#[test]
fn a_typed_number_reads_with_or_without_its_m() {
    for typed in ["8", "m8", "M8", " m8 "] {
        assert_eq!(Number::parse(typed), Ok(n(8)), "{typed:?}");
    }
    assert_eq!(n(8).to_string(), "m8");
    assert_eq!(Number::parse("m999999"), Ok(n(999_999)));
    assert_eq!(Number::parse("m0"), Err(Refusal::Unnumbered));
    assert_eq!(Number::parse("0"), Err(Refusal::Unnumbered));
    assert_eq!(Number::parse("m08"), Err(Refusal::LeadingZero));
    assert_eq!(Number::parse("m1234567"), Err(Refusal::TooManyDigits));
    assert_eq!(Number::parse(""), Err(Refusal::Empty));
    for word in ["m", "x8", "m8a", "m 8", "m-8", "m8.2"] {
        assert_eq!(Number::parse(word), Err(Refusal::NotANumber), "{word:?}");
    }
    assert_eq!(Number::new(0), None);
    assert_eq!(Number::new(Number::MAX + 1), None);
}

#[test]
fn a_stored_word_gives_a_number_only_at_its_start() {
    let reads = [
        ("m8-macbook-pro-m5-128gb-8tb", Some(8)),
        ("M22.local", Some(22)),
        ("m4 (2)", Some(4)),
        ("m10 MacBook Pro", Some(10)),
        ("m100-studio", Some(100)),
        ("m40-3fa2", Some(40)),
        (" m7 ", Some(7)),
        ("m999999", Some(999_999)),
        ("Mac-mini-M4", None),
        ("macbook-m3", None),
        ("m3pro", None),
        ("m3a.local", None),
        ("10.0.1.20", None),
        ("8", None),
        ("m0", None),
        ("m08-x", None),
        ("m1234567", None),
        ("", None),
    ];
    for (word, want) in reads {
        assert_eq!(Number::leading(word).map(Number::get), want, "{word:?}");
    }
}

#[test]
fn preference_words_are_m_number_dot_rank() {
    for word in ["m2.2", "M2.10", "m40.1"] {
        assert!(is_preference_word(word), "{word:?}");
    }
    for word in ["m2.", "m0.1", "2.2", "m2.x", "m2", "studio.2"] {
        assert!(!is_preference_word(word), "{word:?}");
    }
}

#[test]
fn nicknames_follow_their_grammar() {
    for word in [
        "Heron",
        "heron",
        "Old Otter",
        "Kestrel-2",
        "Puffin's",
        "Élan",
        "Ab",
    ] {
        assert_eq!(
            Nickname::parse(word).map(|nick| nick.as_str().to_owned()),
            Ok(word.to_owned()),
            "{word:?}"
        );
    }
    let refused = [
        ("", Refusal::Empty),
        ("Abcdefghijklmnopqrstuvwxy", Refusal::TooLong),
        ("m0", Refusal::Unnumbered),
        ("M0", Refusal::Unnumbered),
        ("m12", Refusal::NumberShaped),
        ("12", Refusal::NumberShaped),
        ("m8.2", Refusal::NumberShaped),
        ("m4 Studio", Refusal::NumberShaped),
        ("-dash", Refusal::StartsWithoutLetter),
        ("a@b", Refusal::Characters),
        ("two  spaces", Refusal::Characters),
        ("trailing ", Refusal::Characters),
        ("node-1a2b", Refusal::NodeWord),
        ("n-1a2b3c4d5e6f7a8b", Refusal::NodeWord),
        ("deadbeef", Refusal::HexRun),
        ("self", Refusal::Reserved),
        ("All", Refusal::Reserved),
        ("localhost", Refusal::Reserved),
    ];
    for (word, why) in refused {
        assert_eq!(Nickname::parse(word), Err(why), "{word:?}");
    }
    assert!(
        Nickname::parse("n-deer").is_ok(),
        "n- before a non-hex word"
    );
    // Twenty-four two-byte letters are 48 bytes; twenty-five refuse by count.
    assert!(Nickname::parse(&"é".repeat(24)).is_ok());
    assert_eq!(Nickname::parse(&"é".repeat(25)), Err(Refusal::TooLong));
}

#[test]
fn a_lowercase_nickname_shows_capitalised_and_any_other_as_typed() {
    let shown = |word: &str| Nickname::parse(word).unwrap().display();
    assert_eq!(shown("heron"), "Heron");
    assert_eq!(shown("old otter"), "Old otter");
    assert_eq!(shown("macOS Fan"), "macOS Fan");
    assert_eq!(shown("élan"), "élan");
}

#[test]
fn aliases_are_dns_labels_that_shadow_no_node_or_number() {
    for (word, stored) in [
        ("m40-3fa2", "m40-3fa2"),
        ("studio-2", "studio-2"),
        ("Studio-2.local", "studio-2"),
        ("m123", "m123"),
        (" desk3 ", "desk3"),
    ] {
        assert_eq!(
            Alias::parse(word).map(|alias| alias.as_str().to_owned()),
            Ok(stored.to_owned()),
            "{word:?}"
        );
    }
    let refused = [
        ("", Refusal::Empty),
        (".local", Refusal::Empty),
        ("m2.2", Refusal::PreferenceWord),
        ("m0", Refusal::Unnumbered),
        ("m08", Refusal::LeadingZero),
        ("m1234567-x", Refusal::TooManyDigits),
        ("12", Refusal::NumberShaped),
        ("node-x", Refusal::NodeWord),
        ("n-1a2b3c4d5e6f7a8b", Refusal::NodeWord),
        ("deadbeef01", Refusal::HexRun),
        ("self", Refusal::Reserved),
        ("-x", Refusal::Characters),
        ("x-", Refusal::Characters),
        ("a_b", Refusal::Characters),
        ("x y", Refusal::Characters),
    ];
    for (word, why) in refused {
        assert_eq!(Alias::parse(word), Err(why), "{word:?}");
    }
    assert_eq!(Alias::parse(&"z".repeat(64)), Err(Refusal::TooLong));
    assert!(Alias::parse(&"z".repeat(63)).is_ok());
}

#[test]
fn an_alias_is_neither_its_own_name_nor_a_live_machines() {
    let alias = |word: &str| Alias::parse(word).unwrap();
    let live = [n(9), n(40)];
    assert_eq!(alias("m8").check(Some(n(8)), &live), Err(Refusal::OwnName));
    assert_eq!(
        alias("m9").check(Some(n(8)), &live),
        Err(Refusal::LiveName(n(9)))
    );
    assert_eq!(alias("m123").check(Some(n(8)), &live), Ok(()));
    assert_eq!(alias("m40-3fa2").check(Some(n(8)), &live), Ok(()));
    assert_eq!(alias("studio-2").check(None, &live), Ok(()));
}

/// The fewest single-letter edits that turn `a` into `b`.
fn edits(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut next = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            next.push(
                (row[j + 1] + 1)
                    .min(next[j] + 1)
                    .min(row[j] + usize::from(ca != *cb)),
            );
        }
        row = next;
    }
    row[b.len()]
}

#[test]
fn nouns_are_well_formed() {
    let model_words: Vec<&str> = MODEL_WORDS
        .iter()
        .flat_map(|(_, word)| word.split('-'))
        .collect();
    for (i, noun) in NOUNS.iter().enumerate() {
        assert!((3..=7).contains(&noun.len()), "{noun}: 3 to 7 letters");
        assert!(
            noun.bytes().all(|b| b.is_ascii_lowercase()),
            "{noun}: lowercase ASCII"
        );
        assert!(
            !noun.bytes().all(|b| b.is_ascii_hexdigit()),
            "{noun}: reads as hex"
        );
        assert!(!model_words.contains(noun), "{noun}: a model word");
        assert!(!RESERVED_WORDS.contains(noun), "{noun}: reserved");
        let nick = Nickname::parse(noun);
        assert!(nick.is_ok(), "{noun}: not a nickname: {nick:?}");
        for other in &NOUNS[i + 1..] {
            assert!(
                edits(noun, other) >= 2,
                "{noun} and {other} are one edit apart"
            );
            assert!(
                !noun.starts_with(other) && !other.starts_with(noun),
                "{noun} and {other}: one is a prefix of the other"
            );
        }
    }
}

#[test]
fn the_draw_skips_nouns_in_use_at_most_eight_times() {
    let sequence = |values: Vec<u32>| {
        let mut values = values.into_iter();
        move || values.next().unwrap()
    };
    assert_eq!(draw(sequence(vec![0]), |_| false), NOUNS[0]);
    // The index wraps at the list's length, so every u32 maps to one noun.
    assert_eq!(
        draw(
            sequence(vec![u32::try_from(NOUNS.len() + 3).unwrap()]),
            |_| false
        ),
        NOUNS[3]
    );
    let taken = [NOUNS[1], NOUNS[2]];
    assert_eq!(
        draw(sequence(vec![1, 2, 5]), |noun| taken.contains(&noun)),
        NOUNS[5]
    );
    let mut drawn = 0;
    let last = draw(
        || {
            drawn += 1;
            u32::try_from(drawn).unwrap()
        },
        |_| true,
    );
    assert_eq!(drawn, DRAWS, "at most eight draws");
    assert_eq!(last, NOUNS[DRAWS], "the last draw stands");
}

const GIB: u64 = 1 << 30;

fn mac(model: &str, chip: &str, memory_gib: u64, disk_bytes: u64) -> MachineFacts {
    MachineFacts {
        hw_model: Some("Mac17,6".into()),
        marketing: Some(model.into()),
        chip: (!chip.is_empty()).then(|| chip.into()),
        memory_bytes: (memory_gib > 0).then_some(memory_gib * GIB),
        disk: (disk_bytes > 0).then_some(Disk {
            bytes: disk_bytes,
            rotational: false,
        }),
        ..MachineFacts::default()
    }
}

#[test]
fn the_long_form_matches_the_designs_worked_examples() {
    let thinkpad = MachineFacts {
        vendor: Some("LENOVO".into()),
        product: Some("21HMCTO1WW".into()),
        family: Some("ThinkPad X1 Carbon Gen 11".into()),
        chip: Some("13th Gen Intel(R) Core(TM) i7-1365U".into()),
        memory_bytes: Some(32 * GIB),
        disk: Some(Disk {
            bytes: 1_024_209_543_168,
            rotational: false,
        }),
        ..MachineFacts::default()
    };
    let rows = [
        (
            8,
            mac("MacBook Pro", "Apple M5 Max", 128, 8_001_563_222_016),
            "m8-macbook-pro-m5-128gb-8tb",
        ),
        (
            16,
            mac("MacBook Pro", "Apple M5 Max", 128, 8_001_563_222_016),
            "m16-macbook-pro-m5-128gb-8tb",
        ),
        (
            4,
            mac(
                "MacBook Pro",
                "Intel(R) Core(TM) i7-7920HQ CPU @ 3.10GHz",
                16,
                1_000_555_581_440,
            ),
            "m4-macbook-pro-i7-16gb-1tb",
        ),
        (
            1,
            mac("MacBook Air", "Apple M2", 24, 1_000_555_581_440),
            "m1-macbook-air-m2-24gb-1tb",
        ),
        (118, mac("Mac Studio", "", 0, 0), "m118-mac-studio"),
        (9, MachineFacts::default(), "m9"),
        (130, thinkpad, "m130-thinkpad-x1-carbon-i7-32gb-1tb"),
    ];
    for (number, facts, want) in rows {
        assert_eq!(long_name(n(number), &facts), want);
    }
    assert_eq!(
        long_name(
            n(8),
            &mac("MacBook Pro", "Apple M5 Max", 128, 8_001_563_222_016)
        )
        .len(),
        27
    );
}

#[test]
fn model_words_come_from_the_table_or_three_slugged_words() {
    let facts =
        |marketing: Option<&str>, vendor: Option<&str>, product: Option<&str>| MachineFacts {
            marketing: marketing.map(Into::into),
            vendor: vendor.map(Into::into),
            product: product.map(Into::into),
            ..MachineFacts::default()
        };
    for (name, word) in MODEL_WORDS {
        assert_eq!(
            model_word(&facts(Some(name), None, None)).as_deref(),
            Some(word)
        );
    }
    let rows = [
        (facts(Some("MacBook Neo"), None, None), Some("macbook-neo")),
        (
            facts(None, Some("HP"), Some("HP EliteBook 840 G8 Notebook PC")),
            Some("elitebook-840-g8"),
        ),
        (
            facts(None, Some("Dell Inc."), Some("XPS 13 9310")),
            Some("xps-13-9310"),
        ),
        (
            facts(None, Some("Microsoft Corporation"), Some("Surface Pro 9")),
            Some("surface-pro-9"),
        ),
        (
            facts(None, Some("ASUSTeK"), Some("System Product Name")),
            None,
        ),
        (facts(None, None, Some("To Be Filled By O.E.M.")), None),
        (facts(None, None, Some("0123456789")), None),
    ];
    for (facts, want) in rows {
        assert_eq!(model_word(&facts).as_deref(), want, "{facts:?}");
    }
}

#[test]
fn a_hypervisors_identity_is_a_vm_and_a_physical_machine_is_not() {
    let dmi = |vendor: &str, product: &str| MachineFacts {
        vendor: Some(vendor.into()),
        product: Some(product.into()),
        ..MachineFacts::default()
    };
    let virtual_machines = [
        MachineFacts {
            hw_model: Some("VirtualMac2,1".into()),
            marketing: Some("Virtual Machine".into()),
            ..MachineFacts::default()
        },
        dmi("QEMU", "Standard PC (Q35 + ICH9, 2009)"),
        dmi("Red Hat", "KVM"),
        dmi("VMware, Inc.", "VMware Virtual Platform"),
        dmi("innotek GmbH", "VirtualBox"),
        dmi(
            "Parallels Software International Inc.",
            "Parallels Virtual Platform",
        ),
        dmi("Xen", "HVM domU"),
        dmi("Microsoft Corporation", "Virtual Machine"),
        dmi("Apple Inc.", "Apple Virtualization Generic Platform"),
        dmi("QEMU", "Standard PC (i440FX + PIIX, 1996)"),
    ];
    for facts in &virtual_machines {
        assert!(is_virtual(facts), "{facts:?}");
        assert_eq!(model_word(facts).as_deref(), Some("vm"), "{facts:?}");
    }
    let physical = [
        MachineFacts {
            vendor: Some("LENOVO".into()),
            family: Some("ThinkPad X1 Carbon Gen 11".into()),
            product: Some("21HMCTO1WW".into()),
            ..MachineFacts::default()
        },
        dmi("Microsoft Corporation", "Surface Pro 9"),
        dmi("Xenon Systems", "Workstation 5"),
        mac("MacBook Pro", "Apple M5 Max", 128, 0),
    ];
    for facts in &physical {
        assert!(!is_virtual(facts), "{facts:?}");
    }
}

#[test]
fn chips_are_named_by_family_never_by_clock() {
    let rows = [
        ("Apple M5 Max", Some("m5")),
        ("Apple M2", Some("m2")),
        ("M4 Pro", Some("m4")),
        ("Intel(R) Core(TM) i7-8559U CPU @ 2.70GHz", Some("i7")),
        ("Intel(R) Xeon(R) W-3245 CPU @ 3.20GHz", Some("xeon")),
        ("Intel(R) Core(TM) Ultra 7 155H", None),
        ("AMD Ryzen 9 7950X 16-Core Processor", Some("ryzen-9")),
        (
            "AMD Ryzen Threadripper PRO 5995WX 64-Cores",
            Some("threadripper"),
        ),
        ("AMD EPYC 7763 64-Core Processor", Some("epyc")),
        ("ARM Cortex-A72", None),
        ("", None),
    ];
    for (brand, want) in rows {
        assert_eq!(chip_word(brand).as_deref(), want, "{brand:?}");
    }
}

#[test]
fn a_disk_that_is_not_whole_terabytes_is_named_in_gigabytes() {
    let disk = |bytes: u64, rotational: bool| MachineFacts {
        disk: Some(Disk { bytes, rotational }),
        ..MachineFacts::default()
    };
    assert_eq!(long_name(n(5), &disk(500_107_862_016, false)), "m5-512gb");
    assert_eq!(long_name(n(5), &disk(1_500_301_910_016, true)), "m5-1500gb");
    assert_eq!(long_name(n(5), &disk(3_000_592_982_016, true)), "m5-3tb");
}

/// Whether `value` is one macOS's name rule v2 allows for ComputerName:
/// `m[1-9][0-9]{0,5}` alone or followed by `-` and `[a-z0-9-]*`.
fn rule_v2_allows(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('m') else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let (number, tail) = rest.split_at(digits);
    (1..=NUMBER_MAX_DIGITS).contains(&number.len())
        && !number.starts_with('0')
        && (tail.is_empty()
            || tail.strip_prefix('-').is_some_and(|t| {
                t.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            }))
}

#[test]
fn the_fit_drops_disk_memory_chip_then_model_and_never_the_number() {
    let facts = MachineFacts {
        marketing: Some("Workstationextraordinaire Longmodelnamewithoutequal Third".into()),
        chip: Some("AMD Ryzen 9 7950X".into()),
        memory_bytes: Some(128 * GIB),
        disk: Some(Disk {
            bytes: 8_001_563_222_016,
            rotational: false,
        }),
        ..MachineFacts::default()
    };
    let model = "workstationextraordinaire-longmodelnamewithoutequal-third";
    let full = format!("m999999-{model}-ryzen-9-128gb-8tb");
    let number = n(999_999);
    let fits = |limit: usize| long_name_within(number, &facts, limit);
    assert_eq!(fits(full.len()), full);
    assert_eq!(
        fits(full.len() - 1),
        format!("m999999-{model}-ryzen-9-128gb")
    );
    assert_eq!(fits(full.len() - 5), format!("m999999-{model}-ryzen-9"));
    assert_eq!(fits(full.len() - 13), format!("m999999-{model}"));
    assert_eq!(fits(model.len()), "m999999");
    assert_eq!(fits(1), "m999999", "the number is never dropped");
    for limit in [DNS_LABEL_MAX, WINDOWS_DESCRIPTION_MAX] {
        let name = fits(limit);
        assert!(name.len() <= limit && rule_v2_allows(&name), "{name}");
    }
}

#[test]
fn each_os_gets_the_bare_number_where_it_addresses_and_the_long_form_where_it_describes() {
    let facts = mac("MacBook Pro", "Apple M5 Max", 128, 8_001_563_222_016);
    let long = "m8-macbook-pro-m5-128gb-8tb";
    assert_eq!(
        os_slots(NameOs::MacOs, n(8), &facts, true),
        vec![
            (OsSlot::MacHostName, "m8".to_owned()),
            (OsSlot::MacLocalHostName, "m8".to_owned()),
            (OsSlot::MacComputerName, long.to_owned()),
        ]
    );
    assert_eq!(
        os_slots(NameOs::MacOs, n(8), &facts, false)[2],
        (OsSlot::MacComputerName, "m8".to_owned()),
        "before rule v2 the ComputerName is the bare number"
    );
    assert_eq!(
        os_slots(NameOs::Linux, n(8), &facts, true),
        vec![
            (OsSlot::LinuxStatic, "m8".to_owned()),
            (OsSlot::LinuxPretty, long.to_owned()),
            (OsSlot::LinuxHostsLine, "m8".to_owned()),
        ]
    );
    let windows = os_slots(NameOs::Windows, n(8), &facts, true);
    assert_eq!(windows[0], (OsSlot::WindowsHostName, "m8".to_owned()));
    assert_eq!(windows[1], (OsSlot::WindowsDescription, long.to_owned()));
    for os in [NameOs::MacOs, NameOs::Linux, NameOs::Windows] {
        for (slot, value) in os_slots(os, n(999_999), &facts, true) {
            assert!(value.len() <= slot.limit(), "{slot:?} {value}");
            assert!(rule_v2_allows(&value), "{slot:?} {value}");
        }
    }
}
