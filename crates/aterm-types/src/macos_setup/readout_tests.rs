// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Mac;

impl power::Reads for Mac {
    fn pmset(&self, args: &[&str]) -> Result<String, String> {
        match args {
            ["-g", "custom"] => Ok("AC Power:\n sleep 10\n".to_owned()),
            _ => Err("pmset: exit 1\ntwo lines".to_owned()),
        }
    }
}

impl updates::Reads for Mac {
    fn schedule(&self) -> Result<String, String> {
        Ok("Automatic checking for updates is turned on.\n".to_owned())
    }

    fn pref(&self, key: &str) -> Result<updates::Held, String> {
        match key {
            "AutomaticDownload" => Ok(updates::Held::Switch(true)),
            "AutomaticallyInstallMacOSUpdates" => Ok(updates::Held::Switch(false)),
            "CriticalUpdateInstall" => Err("defaults export: ü timed out".to_owned()),
            _ => Ok(updates::Held::Other("yes\nplease".to_owned())),
        }
    }
}

/// What the machines side read comes back through the text answer for answer, failures and
/// multi-line, non-ASCII bodies included.
/// CONTROL: a truncated text, an unknown outcome and a read the text does not hold are refused
/// or reported, never read as a value.
#[test]
fn a_readout_reads_back_what_it_recorded() {
    use power::Reads as _;
    let read = Readout::read(&Mac, &Mac);
    let text = read.render();
    let back = Readout::parse(&text).expect("its own text parses");
    assert_eq!(back, read);
    assert_eq!(back.pmset(&["-g", "custom"]), Mac.pmset(&["-g", "custom"]));
    assert_eq!(back.pmset(&["-g", "cap"]), Mac.pmset(&["-g", "cap"]));
    assert_eq!(updates::now(&back), updates::now(&Mac));

    assert!(
        Readout::parse(&text[..text.len() - 3]).is_err(),
        "CONTROL: truncated"
    );
    assert!(
        Readout::parse("pmset -g cap\tmaybe\t0\n\n").is_err(),
        "CONTROL: outcome"
    );
    assert!(
        Readout::parse("pmset -g cap\tok\t9\nshort\n").is_err(),
        "CONTROL: length"
    );
    use updates::Reads as _;
    assert!(
        Readout::parse("pref AutomaticDownload\tok\t5\nmaybe\n")
            .expect("a record")
            .pref("AutomaticDownload")
            .is_err(),
        "CONTROL: a pref answer that is none of a pref's"
    );
    let empty = Readout::parse("").expect("no records");
    assert!(
        empty.pmset(&["-g", "custom"]).is_err(),
        "CONTROL: an unread read"
    );
}
