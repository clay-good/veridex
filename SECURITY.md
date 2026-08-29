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
  with no network dependency. Any change to a signed certificate's **content** fails signature
  verification (tamper rejection): the bytes signed are the certificate's canonical JSON, re-derived
  from the parsed document at verification time, so altering any field — the verdict, the score, the
  content hash, the scope — breaks the signature. What that does *not* pin is presentation. Two
  byte-distinct files that parse to the same certificate both verify: reordered keys, different
  whitespace, or a re-added field whose default is omitted. Treat the signature as attesting the
  parsed document, not the file. One consequence worth knowing: serde rejects a duplicated *struct*
  field, but a duplicated key inside a map-valued field (such as `findings_summary.by_category`)
  resolves last-wins, so a hand-read of the raw JSON can disagree with what verification used —
  read the output of `veridex verify`, not the file. The hex and algorithm fields must still be in
  the canonical spelling this crate writes. Verification also rejects a certificate signed by an
  untrusted issuer key, and one that declares a signature algorithm this build cannot verify (rather
  than assuming Ed25519).
- **Attestation binding and domain separation.** A producer attestation (`veridex attest`) is
  Ed25519-signed by the **producer's** key — a different key from the certificate issuer's — and is
  bound to the dataset's CDM content hash, so an attestation cannot be moved to other data. It signs
  under its own domain tag (`veridex.attestation.sig.v1\0`, distinct from the certificate's), so a
  signature over one document can never verify as the other even if their bytes coincided. Applying
  an attestation raises **provenance coverage only**: attested elements never enter the CDM, so the
  content hash still describes the data and nothing else, and the run discloses the producer key and
  every element it supplied (`PROVENANCE.ATTESTED`) — which a certificate records, signed, and
  `veridex verify` prints. An attested value that contradicts what the dataset records is reported
  (`PROVENANCE.ATTESTATION_CONFLICT`), never allowed to overwrite it.
- **Non-mutation.** Veridex only reads datasets and writes its own outputs to caller-specified
  paths. It never modifies, repairs, or deletes a user's dataset.
- **No wall-clock in core.** Issuance timestamps are caller-supplied, so signing is reproducible and
  testable; the core reads no ambient clock.
- **Memory safety.** `veridex-core` is compiled with `#![forbid(unsafe_code)]`.
- **Bounded ingestion.** Reading a dataset is reading untrusted input, so ingestion is bounded three
  ways before it allocates: a **frame budget** (20M frames by default, `--max-frames`) on what a file
  can materialize, a **decompression budget** (100x the file's own size, with a
  64 MiB floor so a small file still gets a workable allowance; `--max-decompression-ratio`)
  on how far a compressed container may expand, and a **source-size ceiling** (4 GiB,
  `--max-source-bytes`) on one file read whole into memory. All are charged against what the file
  *declares* — the size ceiling on `stat`, before the read — and the decompression budget again
  against what actually arrives, so a header that understates its expansion buys nothing. A file past
  any of the three is refused with an error naming the limit and what to do about it. The size
  ceiling exists because MCAP, MF4 and a rosbag2 `.db3` are random-access containers and are read
  whole by design: past a certain size the run does not fail with a verdict, it fails with the
  process, since a failed allocation aborts and the OOM killer does not wait for that.

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
- A **producer key** is a separate key with a separate purpose: it signs attestations about
  provenance, not certificates about verdicts. The same `keygen` produces one, and the same care
  applies. A verifier decides independently whether it trusts the issuer and whether it trusts the
  producer — a certificate can be from an issuer you trust while carrying provenance attested by a
  key you do not, which is exactly why the certificate records the producer key rather than folding
  its claims into the data.
- Rotating an issuer key invalidates trust in future certificates signed by the old key; already
  issued certificates remain verifiable against the public key they embed.
- **In memory**, secret key material is scrubbed rather than left in freed allocations: the
  `SigningKey` itself through `ed25519-dalek`'s `zeroize` feature, and the wrappers around it — the
  decoded seed, the hex encoding, and the key file's contents as read by the CLI — through
  `Zeroizing`. This is defense in depth against a core dump or a swapped page, not a defense against
  an attacker who can already read the process's memory as it runs. One copy is deliberately outside
  it: `veridex.keygen()` returns the secret to Python, where strings are immutable and cannot be
  scrubbed. A caller who needs that guarantee should use the CLI, which writes the key to a `0600`
  file and scrubs its own copies.

## Reporting a vulnerability

Please report suspected vulnerabilities privately to the maintainers rather than opening a public
issue, and allow reasonable time for a fix before disclosure.
