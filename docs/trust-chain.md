# The trust chain: attest, certify, verify, label

A certificate is how a verdict outlives the command that produced it. This page is the whole chain —
who signs what, what each signature does and does not prove, and what a reader can conclude offline.

```sh
veridex keygen issuer                                        # an Ed25519 issuer keypair
veridex certify my-dataset/ --key issuer --out c.json        # sign the verdict
veridex verify  my-dataset/ --certificate c.json --key issuer.pub
veridex label   --certificate c.json --key issuer.pub        # Markdown for a dataset card
veridex supersedes earlier.json later.json --key issuer.pub   # which one describes it now
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

## What the run could not measure

A certificate names its findings **by code**, not only by severity and family. That distinction is
the whole point: `statistical: 1` beside "46 checks run, no families skipped" is what a
single-episode dataset whose streams hold no summarizable values used to sign as, on the
forty-six-check catalog of the day — while all five statistical checks had nothing to measure and
seven cross-episode checks had nothing to compare. Twelve of those forty-six presented as clean
executed checks, and the catalog has only grown since.

Veridex emits an informational finding whenever a check had no evidence to work on
(`STATISTICAL.UNMEASURED_VALUES`, `STRUCTURAL.UNCOMPARED_EPISODES`, `TEMPORAL.UNMEASURED_CLOCK`, and
the rest — see [docs/checks.md](checks.md)), precisely so a pass cannot mean "nothing was asked". The
family rollup flattened those back out, and the offline reader is the one person who cannot re-run
Veridex to find out. So `verify` prints a `findings:` line naming each code and its count, `--json`
carries the same map as `findings_by_code`, and both come from the signed document.

Finding *codes* are declared by checks, so the map is bounded by the catalog and never by the
dataset. Finding *messages* are not carried: a message is sized by the input — a colliding stream
group can name thousands of streams — and a signed document must not be.

The **label** names them in a `Could not measure` row, beside the two neighbouring cases it already
disclosed — a check that *crashed* (`Checks that failed to run`) and a family that ran nothing
(`Families not run`). This third case is the one a clean result hides best: a check with no evidence
produces byte-for-byte what a flawless dataset produces.

| Could not measure | STATISTICAL.UNMEASURED_VALUES (1), STRUCTURAL.UNCOMPARED_EPISODES (1) |

Which codes those are is declared per check (`Check::abstention_codes`) and held to the catalog by a
test, so a new abstention code cannot be added without something able to recognize it. The row is
classified from the reading build's catalog rather than from the signed document: the codes are in
the document, but what they *mean* is the catalog's to say — so a code this build does not know is
left unnamed rather than guessed at, and the row never claims to be exhaustive.

A certificate issued before this field existed carries no code map, and the readers print no line
rather than an empty one: absent means unknown, not "no findings". Its bytes, and the signature over
them, are unchanged.

A content hash only means something within one **canonical encoding**, and that encoding changes
when Veridex starts binding a field it did not before. So a certificate records the encoding version
its hash was computed under, and `verify` uses it to tell two failures apart that look identical
otherwise:

- *the data is not what was signed* — the hashes are comparable and they differ. That is a
  transplant or an alteration, and `verify` says so.
- *Veridex hashes differently now* — the certificate was issued under an older encoding, so the two
  numbers were never comparable. `verify` says exactly that, names both versions, and points at
  `veridex certify` to re-issue. It deliberately does **not** lead with "content-hash mismatch",
  because that phrase is read as an accusation whatever follows it, and this failure says nothing
  about the data at all.

The declared encoding sits inside the signed payload and the signature is checked first, so a
certificate cannot claim an encoding it was not issued under. A certificate predating this field
still verifies unchanged — the field is omitted rather than defaulted, so its bytes, and the
signature over them, are exactly what they were — and when *its* hash does not match, `verify` says
the comparability is unknown rather than picking one of the two stories.


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

## Which certificate is current

A certificate binds a content hash, so re-recording a dataset, fixing the fault a certificate
reported, or re-exporting a shard produces a different dataset by the only identity a verifier has.
Both certificates then stay valid offline forever — that is what signing them is for — and a reader
is left holding two true documents about "the same dataset" with nothing to say which describes it
now.

```sh
veridex supersedes earlier.json later.json --key issuer.pub
```

It verifies both documents first (a relation computed from a document nobody verified is worth
nothing — the whole question is which signed statement to trust), then relates them from what they
already carry: the key each signature verified against, the dataset identity, the content hash each
is bound to, and the issuance timestamp each records. It exits **0** when the later certificate
supersedes the earlier one and **20** when it does not, so a release gate can ask the question
directly, and `--json` prints the same answer for a script.

| Relation | What it means |
|---|---|
| `supersedes` | Same issuer, same dataset, different bytes, issued afterwards |
| `reissue` | Both bound to the **same** content hash: the dataset was certified twice, and what differs is what the two runs claimed rather than the data |
| `out-of-order` | The document given as the later one was issued first — pass them the other way round |
| `unordered` | An issuance timestamp is not an RFC 3339 UTC instant, so the two cannot be ordered. Veridex does not guess at a local offset: an ordering guessed wrong names the wrong certificate as current |
| `different-issuer` | Different signing keys. A supersession is an issuer restating what it certified, and no issuer speaks for another's documents |
| `unrelated` | Different datasets |

The answer also carries the trust-score and status movement between the two, so the reader learns
whether the dataset got better or worse rather than only which document is newer.

**A supersession is not a revocation.** The superseded certificate remains a true statement about
the bytes it was issued over, and it keeps verifying: nothing offline can be withdrawn, and Veridex
does not pretend otherwise. What a supersession says is which document describes the dataset now.

## The issuance log

A certificate is a portable document, and once it leaves the machine that made it, the history of its
making is gone. An issuer who runs `certify` twenty times and keeps the twentieth has told nobody
about the other nineteen; a verifier who could not get a certificate to check out has told nobody
either. `--audit-log` keeps that history where the issuer can publish it and anyone can check it.

```sh
veridex certify dataset/ --key issuer.key --out cert.json --audit-log issuance.jsonl
veridex verify  dataset/ --certificate cert.json --key issuer.pub --audit-log issuance.jsonl
veridex audit   issuance.jsonl
```

Each line is one JSON entry: what happened (`issued` or `verified`), the dataset id, the content hash
and the encoding version it was computed under, the outcome as it was reported, the key involved, a
hash of the certificate document itself, and the timestamp the writing machine supplied. Every entry
also names the **hash of the entry before it**, so the file is a chain, and `veridex audit` walks it:
**0** the chain is intact, **20** it is broken or its head is not the one you pinned, **2** the file
is not an issuance log at all — a mistyped path is not evidence of tampering, and a gate that
conflated the two would be ignored within a week.

What it proves, and what it does not, stated here because "audit log" sounds like more than any one
file can be:

| | |
|---|---|
| **Altering a past entry** | Detected, with the line. An entry written by `certify` is also **signed** by the issuer key that signed the certificate, so re-chaining a forged history needs the secret key, not just a text editor |
| **Dropping entries from the end** | *Not* detected by the file alone: a truncated chain is a valid chain, one entry shorter. `audit` prints the chain **head** for you to pin (`--expect-head`), publish or commit — a value the log's holder cannot change without being caught |
| **The time in an entry** | The writing machine's, exactly as in a certificate's issuance block. A machine that lies about its clock writes a lie here. The *order* is the chain's, which no clock can change |
| **A failed verification** | Recorded, with the reason. A log holding only the successes would be worse than none: how often a certificate failed to check out is the question an auditor is asking |

There is a second question a log answers, and it is the one a *consumer* holding a certificate asks:

```sh
veridex audit issuance.jsonl --certificate cert.json
```

Does the issuer's own record name **this document**? Matched by digest over the certificate's bytes,
because that is what an entry holds — so the answer is about a document rather than about a claim,
and three outcomes are distinguished rather than two. The document is recorded (exit 0). It is not,
though the log holds entries for the same dataset (exit 20) — a certificate re-issued over the same
bytes, or this file re-serialized since it was logged, and a reader should know which they have. Or
the log says nothing about the dataset at all (exit 20). The chain being intact is a separate
question, reported separately, because an intact log that does not name your certificate is still an
answer of "no".

`veridex audit --json` prints the same answers for a script, and the Python binding
`veridex.audit(log_text, expect_head=None, certificate_json=None)` returns that JSON byte-for-byte.

## What a certificate is not

It is a statement of fact about a specific dataset — what was checked, what was found, under which
configuration — and not an endorsement of that dataset. Every rendered label ends with that sentence
for the same reason it is written here.
