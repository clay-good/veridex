# Security

Veridex is trust infrastructure, so it holds itself to a clear, testable security model. This
document states what Veridex guarantees, what it does **not**, and how signing keys are handled.

## What Veridex guarantees

- **Deterministic verdicts.** The same dataset bytes and the same Veridex version always produce the
  same verdict and the same certificate content hash. Parallelism never changes results.
- **Content binding.** A certificate is bound to the exact CDM content hash of the dataset it was
  issued for. Presenting it against a different dataset fails verification (transplant rejection).
  The content hash covers the dataset's actual content (episodes, streams, frames, stored/observed
  stats, provenance) **and every source-declared value a check reads** — including the per-episode
  declared frame count, whose disagreement with the frames ingested is itself a finding. Anything a
  check can fail on has to be bound, or two datasets that disagree on the verdict could share a hash
  and one's certificate would attest the other. The hash is independent of the order those
  collections happen to be listed in, and that ordering is total: episodes with a duplicate index and
  streams with a duplicate name — both faults Veridex reports — are ordered by their full content, so
  the hash never depends on the order they arrived in.
- **Offline verification.** Certificates are Ed25519-signed and verify against the issuer public key
  with no network dependency. Any change to a signed certificate fails signature verification
  (tamper rejection), and a signed certificate has exactly one byte form — the hex and algorithm
  fields must be in the canonical spelling this crate writes, so the same certificate cannot be
  presented as two different files that both verify. Verification also rejects a certificate signed by an untrusted issuer key, and
  one that declares a signature algorithm this build cannot verify (rather than assuming Ed25519).
- **Non-mutation.** Veridex only reads datasets and writes its own outputs to caller-specified
  paths. It never modifies, repairs, or deletes a user's dataset.
- **No wall-clock in core.** Issuance timestamps are caller-supplied, so signing is reproducible and
  testable; the core reads no ambient clock.
- **Memory safety.** `veridex-core` is compiled with `#![forbid(unsafe_code)]`.
- **Bounded ingestion.** Reading a dataset is reading untrusted input, so ingestion is bounded twice
  before it allocates: a **frame budget** (20M frames by default, `--max-frames`) on what a file can
  materialize, and a **decompression budget** (100x the file's own size, `--max-decompression-ratio`)
  on how far a compressed container may expand. Both are charged against what the file *declares*,
  and the decompression budget again against what actually arrives — so a header that understates
  its expansion buys nothing. A file past either budget is refused with an error naming the limit.

## What Veridex does NOT guarantee

- **It is not a malware or code scanner.** Veridex checks dataset *quality, timing, and provenance*,
  not executable safety. A clean verdict does not mean a file is safe to execute.
- **A grade is not an endorsement.** A certificate is a nutrition label — a statement of facts
  (checks run, findings, score, provenance coverage), not a blessing that a dataset is "good."
- **Provenance is best-effort and attested, never proof.** Extracted provenance reflects what the
  source encoded; asserted provenance reflects what a producer signed. Veridex never fabricates or
  infers provenance, and an `asserted` value is only as trustworthy as the key that signed it.
- **Trust scores compare only within a rubric version.** Scores under different `rubric_version`s
  are not comparable.

## Signing keys

- The issuer **secret key** grants the ability to issue certificates in that issuer's name. Keep it
  private; never commit it. The repository `.gitignore` excludes `*.key` and `*.pem`, and generated
  certificates (`*.veridex.json`).
- `veridex keygen <path>` writes the secret to `<path>` and the public key to `<path>.pub`. Share
  only the public key; verifiers use it to check certificates. `keygen` refuses to overwrite an
  existing key file unless `--force` is given, so an accidental re-run cannot destroy a signing key.
  On Unix the secret key file is created with `0600` (owner-only) permissions so another local user
  cannot read it.
- Rotating an issuer key invalidates trust in future certificates signed by the old key; already
  issued certificates remain verifiable against the public key they embed.

## Reporting a vulnerability

Please report suspected vulnerabilities privately to the maintainers rather than opening a public
issue, and allow reasonable time for a fix before disclosure.
