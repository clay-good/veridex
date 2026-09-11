# Tasks

## P1 — the pack

- [x] A `CheckPack { name, version, checks }` with a validated name (lowercase, no `/`), so an id
      cannot be ambiguous about where its namespace ends.
- [x] `EngineBuilder::register_pack`, namespacing every id as `pack/check-id` and refusing a pack
      whose namespace or ids collide with the catalog or another pack.
- [x] Refuse a pack that claims a built-in pack name, so a third party cannot impersonate the
      catalog in a report.

## P2 — accountability

- [x] The verdict records each pack's name and version, beside `executed_checks`.
- [x] The pack set is part of the result content hash. No `CANONICAL_VERSION` bump: that versions
      the *CDM* encoder, and a pack changes the verdict rather than the data. An empty pack list is
      skipped in the digest, so every existing result hash is unchanged.
- [x] The certificate and the terminal report state the packs a run used; a run with no packs prints
      nothing new, so existing output and hashes are unchanged.
- [ ] `veridex checks --json` lists pack-provided checks with their pack, so the catalog a run used
      is inspectable before the run. (Not in this slice: `checks` builds the standard catalog and
      takes no pack, so there is nothing yet for it to list.)

## P3 — proof

- [x] A pack whose check fires produces a finding whose id names the pack, through the terminal
      report, the JSON, the SARIF and the certificate.
- [x] A pack colliding with the catalog, with another pack, or claiming a reserved name is refused
      by name rather than silently shadowing.
- [x] Two runs over the same dataset, one with a pack and one without, differ in their verdicts and
      in their result hashes — the property the whole change exists for.
- [x] A pack cannot sign: the `Check` contract returns findings, and the signing path is not
      reachable from it. Stated as a test that a pack's findings reach the certificate only through
      the ordinary verdict.
- [x] `docs/checks.md` says what a pack may and may not do. The separate authoring guide say what a pack may and may not do, with a worked
      example that compiles.
