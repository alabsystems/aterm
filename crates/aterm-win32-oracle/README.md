# Windows declaration generator and oracle

Run this development-only driver with Trust:

```sh
targo --unverified run --locked -p aterm-win32-oracle -- gen
targo --unverified run --locked -p aterm-win32-oracle -- check
targo --unverified run --locked -p aterm-win32-oracle -- record
targo --unverified run --locked -p aterm-win32-oracle -- oracle
targo --unverified run --locked -p aterm-win32-oracle -- oracle-052
```

`gen` harvests Cargo's actual Windows consumers, follows their declaration and
feature closure in pinned registry windows-sys 0.61.2, and emits the facade and
independent reference checks. `--out <dir>` writes a private checkout-shaped
output tree. `check` regenerates in a temporary directory and reports drift;
it never blesses a changed declaration or recorded answer. `record` explicitly
runs the upstream host recorder under Trust, updates recorded.json and regenerates.

`oracle` is a **STOCK EXCEPTION**: absolute stable Cargo and rustc compile both
Windows targets, which Trust does not provide. It checks every declaration's
types, layouts, values and signatures, compares imported symbol/DLL pairs, and
requires the planted defects to be caught. It does not execute Windows programs.
The reference workspace has no patch table, so its windows-sys dependency reaches
the registry rather than the facade.

`oracle-052` holds `crates/aterm-windows-sys-052`, the windows-sys 0.52 facade
ring reads on aarch64 Windows (WB.1), to the registry windows-sys 0.52.0 the same
way, from its own reference workspace `crates/aterm-windows-sys-052/tests/oracle`:
on both Windows triples the conformance crate's const-asserts must hold, a call
through each side must import the same symbol from the same DLL, and each of its
eight `plant-*` features must be caught (six by conformance, two by the import).
It is the same STOCK EXCEPTION and reads no harvest or `--upstream` source.

Exit 0 means all requested checks passed; 1 means a finding; 2 is usage; 3 means
the check could not run. A build, clean or inspection failure never counts as a
caught mismatch. `--upstream` (or WINDOWS_SYS_SRC) selects the pinned registry
source; `--stock` selects the rustup toolchain. `--target-dir` (or
ORACLE_TARGET_DIR) owns the oracle build cache; NM selects its object inspector.
The default cache is this checkout's win32-oracle/target (`oracle-052`:
crates/aterm-windows-sys-052/tests/oracle/target). Before each build `oracle-052`
cleans its two path units, so a cache another checkout shares never answers for
this one. See `--help` for options.
