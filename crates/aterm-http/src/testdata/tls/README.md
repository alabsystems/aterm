<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# TLS verification fixtures

Certificates for `src/verifier/tests.rs`; other `aterm-http` and `aterm-gui`
tests borrow `root.der`, `inter.der`, `good.der` and the `gh-*` chain. All of
them are public test data: never install or trust any of them.

Every local fixture chains to `root.der`, a throwaway P-256 CA in no trust
store. No private keys are committed (the tests only verify), so nothing here
can sign. `mint.sh` regenerates the local set from fresh keys; run it from this
directory with OpenSSL 3. Its header states the two rules the corpus keeps: no
leaf valid for more than ~398 days, and no `authorityInformationAccess` or
`crlDistributionPoints` anywhere.

| file | what it is | why it exists |
| --- | --- | --- |
| `root.der` | CA, `pathlen:1`, 2025‑01‑01 → 2035‑01‑01 | the anchor of every local chain |
| `inter.der` | CA under `root`, `pathlen:0` | the intermediate the missing‑intermediate case omits |
| `notca.der` | `CA:FALSE`, signed by `root` | an issuer that is not allowed to be one |
| `good.der` | leaf under `root`, 2026 → 2027, SAN `test.aterm.invalid` | the positive control |
| `expired.der` | as `good`, 2025 → 2026 | rejected at a `now` past its window, accepted inside it |
| `future.der` | as `good`, 2027 → 2028 | rejected at a `now` before its window, accepted inside it |
| `wronghost.der` | as `good`, SAN `other.aterm.invalid` | rejected for `test.aterm.invalid`, accepted for its own name |
| `viainter.der` | leaf under `inter` | accepted with `inter`, rejected without it |
| `vianotca.der` | leaf under `notca` | a `CA:FALSE` certificate must not be able to issue |
| `selfsigned.der` | leaf that is its own issuer | chains to no anchor |
| `nosan.der` | leaf with a CN but no `subjectAltName` | a common‑name match is not a name match |
| `clientonly.der` | leaf with `extendedKeyUsage=clientAuth` | wrong purpose |
| `noeku.der` | leaf with no `extendedKeyUsage` | Apple rejects it, webpki accepts it (see the `tests.rs` header) |
| `keyenciph.der` | leaf with `keyUsage=keyEncipherment` only | pins the verdict without a `digitalSignature` bit (both accept) |
| `ipsan.der` | leaf whose only SAN is `IP:127.0.0.1` | pins the `ServerName::IpAddress` → string conversion |
| `tampersig.der` | `good.der` with the last signature byte flipped | catches a verifier that never checks the signature |
| `tampertbs.der` | `good.der` with a SAN byte rewritten inside the signed body | as above, from the other direction |
| `malformed.der` | nine bytes that are not a certificate | must report `BadEncoding`, never a trust verdict |
| `gh-leaf.der`, `gh-int0.der`, `gh-int1.der` | github.com's real chain | proves the system trust store is consulted |

## The captured chain

The `gh-*` chain is the only fixture a real system anchor validates, so it is
the only source of an accept under the system store. It runs through the
Sectigo Public Server Authentication Root E46 certificate cross-signed by
USERTrust ECC Certification Authority (valid to 2038‑01‑18). It is github.com
because `aterm-update-core` fetches releases from github.com and
api.github.com.

Every test pins `now` (`T_GH` in `src/verifier/tests.rs`), so the leaf's expiry
(2026‑09‑30) does not break it. Recapture only when that anchor leaves the
platform trust stores.

### Recapture

```sh
host=github.com
openssl s_client -connect "$host:443" -servername "$host" -showcerts </dev/null \
  > chain.txt
# split chain.txt into its PEM blocks in order, then for each block N:
#   openssl x509 -in blockN.pem -outform DER -out gh-leaf.der   # N=0
#   openssl x509 -in blockN.pem -outform DER -out gh-int0.der   # N=1
#   openssl x509 -in blockN.pem -outform DER -out gh-int1.der   # N=2
```

Then set `T_GH` to an instant inside the new leaf's validity window
(`openssl x509 -noout -dates` prints it).
