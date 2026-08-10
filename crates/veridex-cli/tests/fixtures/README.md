# CLI test fixtures

`demo.mcap` — a small, valid MCAP recording used by the end-to-end CLI test
(`tests/cli.rs::full_check_certify_verify_flow`). It stands in for "a real dataset file on disk", so
the test can exercise the actual `veridex` binary over `check` / `certify` / `verify` without a
network or a live dataset.

Regenerate it with the workspace example:

```sh
cargo run -q -p veridex-core --example make_demo_mcap -- crates/veridex-cli/tests/fixtures/demo.mcap
```

It intentionally contains a cross-stream clock skew, so `veridex check` reports an error and exits
`20` — the fixture proves the failing path end-to-end, not just the happy one.
