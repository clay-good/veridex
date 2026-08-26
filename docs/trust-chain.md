# The trust chain: attest, certify, verify, label

A certificate is how a verdict outlives the command that produced it. This page is the whole chain —
who signs what, what each signature does and does not prove, and what a reader can conclude offline.

```sh
veridex keygen issuer                                        # an Ed25519 issuer keypair
veridex certify my-dataset/ --key issuer --out c.json        # sign the verdict
veridex verify  my-dataset/ --certificate c.json --key issuer.pub
veridex label   --certificate c.json --key issuer.pub        # Markdown for a dataset card
```

The certificate binds to the dataset's CDM content hash and is Ed25519-signed: `verify` succeeds
offline, and rejects a tampered certificate (signature mismatch) or one presented against a
different dataset (content-hash mismatch). **`verify` requires a trusted issuer key**: a valid
signature only proves a certificate is self-consistent, and anyone can mint one about data they
hold — so you either name the issuer with `--key`, or say `--allow-any-issuer` and get a printed
warning (and `issuer_verified: false` in `--json`) instead of an implied endorsement.
On success `verify` reports what the certificate actually
attests — the hash it is bound to, the trust score, and, for a certificate issued with
`--profile world-model-ready`, each readiness criterion's verdict (`--json` for the machine-readable
form). Every line printed is covered by the signature that just verified, so a doctored readiness
block fails verification rather than being read back.


## Producer attestation

Most of what provenance means is not in the file: no format records who operated the robot, which
calibration was in force, or what upstream a merge drew from. Veridex will not infer any of it, so a
producer signs for it instead:

```sh
veridex attest my-dataset/ --key producer.key \
  --set clock=ptp-grandmaster --set annotator=dana --set upstream=acme/raw-v1 \
  --out attestation.json

veridex check   my-dataset/ --attestation attestation.json
veridex certify my-dataset/ --key issuer --attestation attestation.json --out c.json
```

The attestation is signed with the **producer's** key — not the issuer's — and bound to the dataset's
CDM content hash, so it cannot be moved to other data. Three rules keep it honest:

- **Nothing attested enters the CDM.** The content hash describes the data; a claim about the data
  must not change what the data is.
- **The run says a signature is why.** `PROVENANCE.ATTESTED` names the producer key and every element
  it supplied, because provenance coverage is 30% of the trust score and a reader who does not trust
  that key has to be able to subtract exactly those. The certificate records the same, signed; and
  `verify`, `label`, and `provenance --emit` all show it.
- **An attested value never overrides a recorded one.** A contradiction is reported
  (`PROVENANCE.ATTESTATION_CONFLICT`), not resolved in the signature's favour.

A key may repeat only where more than one value is a fact rather than a contradiction — `upstream`,
for a merge. Two licenses in one document is refused at signing time.

## What a certificate is not

It is a statement of fact about a specific dataset — what was checked, what was found, under which
configuration — and not an endorsement of that dataset. Every rendered label ends with that sentence
for the same reason it is written here.
