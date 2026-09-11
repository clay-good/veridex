# Tasks

## P1 — the pack

- [ ] A `CheckPack { name, version, checks }` with a validated name (lowercase, no `/`), so an id
      cannot be ambiguous about where its namespace ends.
- [ ] `EngineBuilder::register_pack`, namespacing every id as `pack/check-id` and refusing a pack
      whose namespace or ids collide with the catalog or another pack.
- [ ] Refuse a pack that claims a built-in pack name, so a third party cannot impersonate the
      catalog in a report.

## P2 — accountability

- [ ] The verdict records each pack's name and version, beside `executed_checks`.
- [ ] The pack set is part of the result content hash (bump `CANONICAL_VERSION`, re-pin the golden
      vector).
- [ ] The certificate and every renderer state the packs a run used; a run with no packs prints
      nothing new, so existing output and hashes are unchanged.
- [ ] `veridex checks --json` lists pack-provided checks with their pack, so the catalog a run used
      is inspectable before the run.

## P3 — proof

- [ ] A pack whose check fires produces a finding whose id names the pack, through the terminal
      report, the JSON, the SARIF and the certificate.
- [ ] A pack colliding with the catalog, with another pack, or claiming a reserved name is refused
      by name rather than silently shadowing.
- [ ] Two runs over the same dataset, one with a pack and one without, differ in their verdicts and
      in their result hashes — the property the whole change exists for.
- [ ] A pack cannot sign: the `Check` contract returns findings, and the signing path is not
      reachable from it. Stated as a test that a pack's findings reach the certificate only through
      the ordinary verdict.
- [ ] `docs/checks.md` and the authoring guide say what a pack may and may not do, with a worked
      example that compiles.
