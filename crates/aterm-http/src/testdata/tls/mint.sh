#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrew Yates
#
# Reproduces every local fixture .der in this directory. PROVENANCE ONLY: the
# DERs are committed, so nothing in the build or the test run executes this.
#
# Requires OpenSSL 3.x for `-not_before`/`-not_after` (used to mint certificates
# whose validity window is in the past or the future). /usr/bin/openssl on macOS
# is LibreSSL 3.3.6 and has NEITHER flag, so it CANNOT mint this corpus; on m21
# the working binary is /opt/homebrew/bin/openssl (3.6.3).
#
# TWO RULES THIS CORPUS MUST KEEP, both learned the hard way:
#
#  1. NO leaf may exceed ~398 days of validity. Apple rejects a longer-lived TLS
#     server certificate outright (errSecCertificateValidityPeriodTooLong,
#     -67901) BEFORE it reaches the reason a negative case is testing — which
#     turns a suite of reject-cases all green for entirely the wrong reason. The
#     repo's habit elsewhere of minting century-long test certs
#     (crates/aterm-net/src/testdata/cert.der runs to 2126) breaks here.
#  2. NO leaf and NO CA may carry authorityInformationAccess or
#     crlDistributionPoints. With neither extension there is no URL for macOS
#     `trustd` or Windows `crypt32` to fetch, so a chain this corpus deliberately
#     broke cannot be silently REPAIRED by a network round-trip mid-test.
#     `verifier::tests::every_local_fixture_is_hermetic` asserts this on every run.
#
# The private keys are NOT committed: nothing in the tests needs to sign, only to
# verify. Re-running this script mints a fresh, unrelated key for every subject.
set -e
OSSL=${OSSL:-/opt/homebrew/bin/openssl}
HOST=test.aterm.invalid
OTHER=other.aterm.invalid

# --- anchors ---------------------------------------------------------------
$OSSL req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout root.key -out root.pem \
  -not_before 20250101000000Z -not_after 20350101000000Z -subj "/CN=aterm oracle root CA" \
  -addext "basicConstraints=critical,CA:TRUE,pathlen:1" -addext "keyUsage=critical,keyCertSign,cRLSign"
$OSSL req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout inter.key -out inter.csr \
  -subj "/CN=aterm oracle intermediate CA"
printf 'basicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\n' > inter.ext
$OSSL x509 -req -in inter.csr -CA root.pem -CAkey root.key -set_serial 0x02 \
  -not_before 20250101000000Z -not_after 20350101000000Z -extfile inter.ext -out inter.pem
# A CA:FALSE certificate that is nevertheless used to SIGN a leaf below.
$OSSL req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout notca.key -out notca.csr \
  -subj "/CN=aterm oracle NOT-a-CA"
printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\n' > notca.ext
$OSSL x509 -req -in notca.csr -CA root.pem -CAkey root.key -set_serial 0x23 \
  -not_before 20250101000000Z -not_after 20350101000000Z -extfile notca.ext -out notca.pem

# --- leaves ----------------------------------------------------------------
# leaf <name> <signer.pem> <signer.key> <serial> <notBefore> <notAfter> <cn>
#      [eku_line] [san_line] [ku_line]
leaf() {
  n=$1; sp=$2; sk=$3; ser=$4; nb=$5; na=$6; cn=$7
  eku=${8:-"extendedKeyUsage=serverAuth"}
  san=${9:-"subjectAltName=DNS:$cn"}
  ku=${10:-"keyUsage=critical,digitalSignature"}
  $OSSL req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout $n.key -out $n.csr -subj "/CN=$cn"
  printf 'basicConstraints=critical,CA:FALSE\n%s\n%s\n%s\n' "$ku" "$eku" "$san" > $n.ext
  $OSSL x509 -req -in $n.csr -CA $sp -CAkey $sk -set_serial $ser \
    -not_before $nb -not_after $na -extfile $n.ext -out $n.pem
}
leaf good       root.pem  root.key  0x11 20260101000000Z 20270101000000Z $HOST
leaf expired    root.pem  root.key  0x12 20250101000000Z 20260101000000Z $HOST
leaf future     root.pem  root.key  0x13 20270101000000Z 20280101000000Z $HOST
leaf wronghost  root.pem  root.key  0x14 20260101000000Z 20270101000000Z $OTHER
leaf viainter   inter.pem inter.key 0x15 20260101000000Z 20270101000000Z $HOST
leaf nosan      root.pem  root.key  0x21 20260101000000Z 20270101000000Z $HOST \
     "extendedKeyUsage=serverAuth" "# deliberately no subjectAltName"
leaf clientonly root.pem  root.key  0x22 20260101000000Z 20270101000000Z $HOST \
     "extendedKeyUsage=clientAuth"
leaf vianotca   notca.pem notca.key 0x24 20260101000000Z 20270101000000Z $HOST
leaf ipsan      root.pem  root.key  0x31 20260101000000Z 20270101000000Z 127.0.0.1 \
     "extendedKeyUsage=serverAuth" "subjectAltName=IP:127.0.0.1"
leaf keyenciph  root.pem  root.key  0x32 20260101000000Z 20270101000000Z $HOST \
     "extendedKeyUsage=serverAuth" "subjectAltName=DNS:$HOST" "keyUsage=critical,keyEncipherment"
leaf noeku      root.pem  root.key  0x33 20260101000000Z 20270101000000Z $HOST \
     "# deliberately no extendedKeyUsage"
# A leaf that is its own issuer: chains to nothing, anchored by nothing.
$OSSL req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout selfsigned.key -out selfsigned.pem \
  -not_before 20260101000000Z -not_after 20270101000000Z -subj "/CN=$HOST" \
  -addext "basicConstraints=critical,CA:FALSE" -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=serverAuth" -addext "subjectAltName=DNS:$HOST"

for f in root inter notca good expired future wronghost viainter vianotca selfsigned \
         nosan clientonly ipsan keyenciph noeku; do
  $OSSL x509 -in $f.pem -outform DER -out $f.der
done

# --- corruptions and the wrong-reason control -------------------------------
# `tampersig` flips a bit in the signature; `tampertbs` rewrites a byte of the
# SAN INSIDE the signed body. Both are the trap for a verifier that checks dates
# and names but never actually checks the signature.
python3 -c "b=bytearray(open('good.der','rb').read()); b[-1]^=1; open('tampersig.der','wb').write(bytes(b))"
python3 -c "b=bytearray(open('good.der','rb').read()); i=bytes(b).find(b'test.aterm.invalid'); b[i]=ord('b'); open('tampertbs.der','wb').write(bytes(b))"
# Nine bytes that are not a certificate at all: the control that proves a DER
# PARSE failure is reported as BadEncoding and never counted as a trust verdict.
printf '\x30\x82\x00\x05\xde\xad\xbe\xef\x00' > malformed.der

rm -f *.pem *.csr *.ext *.key

# --- the captured real chains (gh-*.der, cl-*.der) --------------------------
# NOT minted here. Captured ONCE, on 2026-08-29, with the single command below,
# then split into the three DERs. It is the ONLY fixture whose acceptance
# depends on the machine's real system trust store, and the only reason the
# suite can prove the OS store is consulted at all. Recapture procedure, needed
# only when the anchor (USERTrust ECC Certification Authority, via Sectigo
# Public Server Authentication Root E46) eventually leaves the platform stores:
#   openssl s_client -connect github.com:443 -servername github.com -showcerts \
#     </dev/null > gh.txt   # then DER-encode each PEM block in order
# github.com is the deliberate choice: `aterm-update-core` fetches from
# github.com / api.github.com, so this control tracks a host aterm really uses.
#
# There is a SECOND capture, cl-*.der, taken the same day and the same way from
# downloads.claude.ai (allow-listed at crates/atpkg/src/vendor.rs:59):
#   openssl s_client -connect downloads.claude.ai:443 \
#     -servername downloads.claude.ai -showcerts </dev/null > cl.txt
# Its anchor is Google Trust Services' GTS Root R1 (RSA, cross-signed by
# GlobalSign Root CA) rather than Sectigo's ECC root, and that independence is
# the entire point: these two chains are the ONLY inputs in the whole corpus a
# real system anchor validates, so they are the only thing that can catch a
# verifier which rejects everything. One root leaving the platform trust store
# must not be able to disarm the suite. See README.md for the recapture
# procedure and what to do about the pinned instant afterwards.
