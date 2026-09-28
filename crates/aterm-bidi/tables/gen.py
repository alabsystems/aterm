#!/usr/bin/env python3
# Copyright 2026 Andrew Yates
# SPDX-License-Identifier: Apache-2.0
"""Regenerate crates/aterm-bidi/src/tables.rs from the Unicode Character Database.

    python3 crates/aterm-bidi/tables/gen.py DerivedBidiClass.txt BidiBrackets.txt

Inputs are the UCD's extracted/DerivedBidiClass.txt (including its `@missing`
defaults, which assign R / AL / ET to unassigned code points in the RTL and
currency blocks) and BidiBrackets.txt. It rewrites the crate's src/tables.rs
whole; nothing is printed.
"""

import os
import re
import sys

NAMES = {
    "Left_To_Right": "L", "Right_To_Left": "R", "Arabic_Letter": "AL",
    "European_Number": "EN", "European_Separator": "ES",
    "European_Terminator": "ET", "Arabic_Number": "AN",
    "Common_Separator": "CS", "Nonspacing_Mark": "NSM",
    "Boundary_Neutral": "BN", "Paragraph_Separator": "B",
    "Segment_Separator": "S", "White_Space": "WS", "Other_Neutral": "ON",
    "Left_To_Right_Embedding": "LRE", "Left_To_Right_Override": "LRO",
    "Right_To_Left_Embedding": "RLE", "Right_To_Left_Override": "RLO",
    "Pop_Directional_Format": "PDF", "Left_To_Right_Isolate": "LRI",
    "Right_To_Left_Isolate": "RLI", "First_Strong_Isolate": "FSI",
    "Pop_Directional_Isolate": "PDI",
}
ABBR = set(NAMES.values())
HEADER = """// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// GENERATED — do not edit by hand. Regenerate with
//     python3 crates/aterm-bidi/tables/gen.py <DerivedBidiClass.txt> <BidiBrackets.txt>
// from the Unicode Character Database, whose data is under the Unicode License v3
// (UNICODE-LICENSE.txt at the repo root).

use crate::BidiClass;
use crate::BidiClass::*;

"""


def span(field):
    if ".." in field:
        a, b = field.split("..")
        return int(a, 16), int(b, 16)
    return int(field, 16), int(field, 16)


def main(derived, brackets):
    text = open(derived, encoding="utf-8").read()
    version = re.search(r"DerivedBidiClass-([0-9.]+)\.txt", text).group(1)
    cls = ["L"] * 0x110000
    for line in text.splitlines():
        m = re.match(r"# @missing: ([0-9A-F.]+); (\w+)", line)
        if m:
            a, b = span(m.group(1))
            cls[a : b + 1] = [NAMES[m.group(2)]] * (b - a + 1)
    for line in text.splitlines():
        line = line.split("#")[0].strip()
        if not line:
            continue
        field, value = (x.strip() for x in line.split(";"))
        a, b = span(field)
        value = value if value in ABBR else NAMES[value]
        cls[a : b + 1] = [value] * (b - a + 1)
    ranges, start = [], 0
    for c in range(1, 0x110001):
        if c == 0x110000 or cls[c] != cls[start]:
            if cls[start] != "L":
                ranges.append((start, c - 1, cls[start]))
            start = c
    pairs = []
    for line in open(brackets, encoding="utf-8"):
        line = line.split("#")[0].strip()
        if not line:
            continue
        a, b, kind = (x.strip() for x in line.split(";"))
        if kind == "o":
            pairs.append((int(a, 16), int(b, 16)))
    out = [HEADER]
    out.append(
        f"// Generated from the Unicode Character Database {version} (extracted/DerivedBidiClass.txt,\n"
        "// including its @missing defaults) by crates/aterm-bidi/tables/gen.py. Every code point\n"
        "// not inside a listed range is `L`. Sorted, non-overlapping, binary-searched.\n"
        "pub(crate) const BIDI_CLASS_RANGES: &[(u32, u32, BidiClass)] = &[\n"
    )
    out.extend(f"    (0x{a:04X}, 0x{b:04X}, {v}),\n" for a, b, v in ranges)
    out.append("];\n\n")
    out.append(
        "/// Every opening paired bracket (BidiBrackets.txt, `o`) with its closing pair, sorted by opener.\n"
        "pub(crate) const BRACKET_PAIRS: &[(u32, u32)] = &[\n"
    )
    out.extend(f"    (0x{a:04X}, 0x{b:04X}),\n" for a, b in sorted(pairs))
    out.append("];\n\n")
    out.append(
        "/// The same pairs keyed by closer: `(close, open)`, sorted by closer.\n"
        "pub(crate) const BRACKET_CLOSERS: &[(u32, u32)] = &[\n"
    )
    out.extend(f"    (0x{b:04X}, 0x{a:04X}),\n" for a, b in sorted(pairs, key=lambda p: p[1]))
    out.append("];\n")
    dest = os.path.join(os.path.dirname(__file__), "..", "src", "tables.rs")
    with open(dest, "w", encoding="utf-8") as stream:
        stream.write("".join(out))


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
