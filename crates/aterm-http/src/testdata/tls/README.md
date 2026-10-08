<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# TLS verification fixtures

Certificates for `crates/aterm-http/src/verifier/tests.rs`. All
of them are public test data. **Do not install, trust, or reuse any file here in
any real deployment.**

Every local fixture chains to `root.der`, a throwaway P-256 CA that is in no
machine's trust store anywhere. No private keys are committed — the tests only
verify, they never sign — so nothing here can mint a new certificate; re-run
`mint.sh` (provenance only) to regenerate the set from fresh keys.

| file | what it is | why it exists |
| --- | --- | --- |
| `root.der` | CA, `pathlen:1`, 2025‑01‑01 → 2035‑01‑01 | the anchor every local chain is verified against |
| `inter.der` | CA under `root`, `pathlen:0` | the intermediate the missing‑intermediate case omits |
| `notca.der` | `CA:FALSE`, signed by `root` | an issuer that is not allowed to be one |
| `good.der` | leaf, `root`‑signed, 2026 → 2027, SAN `test.aterm.invalid` | THE positive control |
| `expired.der` | same, 2025 → 2026 | rejected at a `now` past its window, accepted inside it |
| `future.der` | same, 2027 → 2028 | rejected at a `now` before its window, accepted inside it |
| `wronghost.der` | same, SAN `other.aterm.invalid` | rejected for `test.aterm.invalid`, accepted for its own name |
| `viainter.der` | leaf under `inter` | accepted WITH `inter`, rejected without it |
| `vianotca.der` | leaf under `notca` | a `CA:FALSE` certificate must not be able to issue |
| `selfsigned.der` | leaf that is its own issuer | chains to no anchor |
| `nosan.der` | leaf with a CN but no `subjectAltName` | a common‑name match is not a name match |
| `clientonly.der` | leaf with `extendedKeyUsage=clientAuth` | wrong purpose |
| `noeku.der` | leaf with NO `extendedKeyUsage` at all | see the divergence note in `verifier.rs` |
| `keyenciph.der` | leaf with `keyUsage=keyEncipherment` only | no `digitalSignature` bit |
| `ipsan.der` | leaf whose only SAN is `IP:127.0.0.1` | pins the `ServerName::IpAddress` → hostname‑string conversion |
| `tampersig.der` | `good.der` with the last signature byte flipped | a verifier that never checks the signature passes everything else |
| `tampertbs.der` | `good.der` with a SAN byte rewritten inside the signed body | as above, from the other direction |
| `malformed.der` | nine bytes that are not a certificate | the wrong‑reason control: this must report `BadEncoding`, never a trust verdict |
| `gh-leaf.der`, `gh-int0.der`, `gh-int1.der` | github.com's real chain, captured 2026‑08‑29 | proves the machine's real system trust store is consulted |

`mint.sh` regenerates everything except the `gh-*` capture, and its
header records the two properties the LOCAL corpus must never lose (no leaf over
~398 days, and no `authorityInformationAccess` / `crlDistributionPoints`
anywhere).

## The captured chain

It is the only fixture a real system anchor validates, so it is the only
source of an ACCEPT under the system store. Its anchor is Sectigo Public Server
Authentication Root E46 (ECC, to 2038‑01‑18), via USERTrust ECC; github.com
because `aterm-update-core` fetches releases from github.com / api.github.com.

**Time is pinned in every test**, so the chain does not rot when its leaf
expires (2026‑09‑30). Pinning does NOT protect against the anchor leaving the
trust store.

### Recapture

```sh
host=github.com
openssl s_client -connect "$host:443" -servername "$host" -showcerts </dev/null \
  > chain.txt
# split chain.txt into its PEM blocks IN ORDER, then for each block N:
#   openssl x509 -in blockN.pem -outform DER -out gh-leaf.der   # N=0
#   openssl x509 -in blockN.pem -outform DER -out gh-int0.der   # N=1
#   openssl x509 -in blockN.pem -outform DER -out gh-int1.der   # N=2
```

Then update `T_GH` in `src/verifier/tests.rs` to an instant inside the NEW
leaf's validity window (`openssl x509 -noout -dates` prints it).
