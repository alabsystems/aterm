// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const PLANT: &str = "QZP7E7W1";

/// The message a real panic of `act` carries, whatever its payload's type, and the form
/// [`Panic::of`] makes of the payload itself.
fn caught(act: impl FnOnce() + std::panic::UnwindSafe) -> (String, String) {
    let payload = std::panic::catch_unwind(act).expect_err("the act panics");
    let text = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .expect("a text payload");
    (text, Panic::of(payload.as_ref()).to_string())
}

/// Every std panic shape a reader can raise over a string holding the plant: each payload's
/// form names its kind and its length and holds no byte of the message, and an equal-length
/// message with the plant overwritten gives the same form (the law: the text reaches the form
/// only through its length and its kind). `Len` and `word` likewise.
#[test]
fn panic_words_name_the_kind_and_never_the_text() {
    let row = || std::hint::black_box(format!("é {PLANT} row"));
    let cases: Vec<(&str, PanicKind, (String, String))> = vec![
        (
            "a char-boundary slice",
            PanicKind::StrSlice,
            caught(|| {
                let _ = &row()[1..];
            }),
        ),
        (
            "an out-of-bounds slice",
            PanicKind::StrSlice,
            caught(|| {
                let _ = &row()[..999];
            }),
        ),
        (
            "a backwards slice",
            PanicKind::StrSlice,
            caught(|| {
                let s = row();
                let (from, to) = std::hint::black_box((6, 4));
                let _ = &s[from..to];
            }),
        ),
        (
            "an index out of bounds",
            PanicKind::Index,
            caught(|| {
                let v = std::hint::black_box(vec![row()]);
                let _ = &v[9];
            }),
        ),
        (
            "a slice range past the end",
            PanicKind::Index,
            caught(|| {
                let v = std::hint::black_box(row().into_bytes());
                let _ = &v[..999];
            }),
        ),
        (
            "an Option unwrap",
            PanicKind::UnwrapNone,
            caught(|| {
                let _ = std::hint::black_box(None::<String>).unwrap();
            }),
        ),
        (
            "a Result unwrap",
            PanicKind::UnwrapErr,
            caught(|| {
                std::hint::black_box(Err::<(), String>(row())).unwrap();
            }),
        ),
        (
            "an expect",
            PanicKind::Other,
            caught(|| {
                let message = row();
                let _ = std::hint::black_box(None::<u8>).expect(&message);
            }),
        ),
        (
            "an overflow",
            PanicKind::Overflow,
            caught(|| {
                let _ = std::hint::black_box(u8::MAX) + 1;
            }),
        ),
        (
            "an assert_eq",
            PanicKind::Assert,
            caught(|| assert_eq!(row(), "x")),
        ),
        (
            "a formatted panic",
            PanicKind::Other,
            caught(|| panic!("reading {}", row())),
        ),
    ];
    for (what, kind, (text, form)) in &cases {
        assert_eq!(PanicKind::of(text), *kind, "{what}: the kind of {text:?}");
        assert_eq!(
            form,
            &format!("{} panic ({}B)", kind.word(), text.len()),
            "{what}"
        );
        assert!(!form.contains("QZP7E7W"), "{what}: {form}");
        let other = text.replace(PLANT, &"x".repeat(PLANT.len()));
        assert_eq!(
            Panic::of_text(&other).to_string(),
            *form,
            "{what}: the form reads the text past its length and kind"
        );
    }
    let held = cases
        .iter()
        .filter(|(.., (text, _))| text.contains(PLANT))
        .count();
    assert!(
        held >= 4,
        "CONTROL: std's messages quote the plant ({held} of them)"
    );
    // The str-slice shapes of the std before this toolchain's, which quote the string they
    // cut (a stock toolchain's cross builds still print them).
    for older in [
        format!(
            "byte index 1 is not a char boundary; it is inside 'é' (bytes 0..2) of `é {PLANT}`"
        ),
        format!("byte index 999 is out of bounds of `é {PLANT}`"),
        format!("begin <= end (6 <= 4) when slicing `é {PLANT}`"),
    ] {
        let form = Panic::of_text(&older).to_string();
        assert_eq!(
            form,
            format!("str-slice panic ({}B)", older.len()),
            "{older}"
        );
    }
    for kind in PanicKind::ALL {
        assert!(is_word(kind.word()), "{kind:?}");
    }
    let opaque: Box<dyn Any + Send> = Box::new(7_u32);
    assert_eq!(Panic::of(opaque.as_ref()).to_string(), "other panic (0B)");

    let said = format!("font_family {PLANT:?} is not installed here");
    assert_eq!(Len(&said).to_string(), format!("{}B", said.len()));
    for (text, kept) in [
        ("claude-code@2.1.312", true),
        ("harness.resume-ask.s-1", true),
        ("key=value", true),
        ("", false),
        ("two words", false),
        ("Couldn't", false),
        ("é", false),
        ("a/b", false),
    ] {
        assert_eq!(word(text) == text, kept, "{text:?}");
        assert_eq!(word(text) == "?", !kept, "{text:?}");
        let named = WordOrLen(text).to_string();
        assert_eq!(
            named,
            if kept {
                text.to_string()
            } else {
                format!("{}B", text.len())
            },
            "{text:?}"
        );
    }
    let long = "w".repeat(WORD_MAX);
    assert_eq!(word(&long), long);
    assert_eq!(word(&format!("{long}w")), "?");
    assert_eq!(word(&format!("{PLANT} row")), "?");
    assert_eq!(WordOrLen(&format!("{PLANT} row")).to_string(), "12B");
}
