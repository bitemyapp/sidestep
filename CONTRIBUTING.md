# Contributing

## Where behavior comes from

Sidestep reimplements Apple's interfaces from the outside. Its legal footing
([docs/legal.md](docs/legal.md)) depends on keeping it that way, so these rules
apply to every contribution.

You may use:

- Apple's public documentation, read by a person for facts. Don't copy its
  prose, and don't scrape developer.apple.com.
- GNUstep's documentation and observed behavior, and objc2's documentation.
- Programs you write against public APIs and run on your own Mac, such as the
  tests in `conformance/`.
- Apple open source under its own licence, with its notices kept, when that
  licence allows it (Apache-2.0 swift-corelibs-foundation, for example).

You may not use:

- Disassembly or decompilation of Apple binaries, class-dump output, debug
  symbols, or inspection of private API, private instance variables or
  internal tables.
- Leaked or otherwise non-public Apple source.
- APSL-licensed Apple code (such as objc4) translated into Sidestep.
- Apple SDK headers or `.tbd` files, in the repository or in Linux CI, or as
  input to header-translator or bindgen for the Linux side.
- Doc comments copied out of objc2's generated crates (they come from Apple's
  headers).

## Sign-off

Every commit carries a [DCO](https://developercertificate.org/) sign-off
(`git commit -s`). By signing off you also state that you used none of the
sources listed under "You may not use" above.

## Tests

Behavior changes come with a test in `conformance/` that passes on macOS
against Apple's runtime before it is expected to pass on Linux. If the two
platforms legitimately differ, the test shouldn't assert the difference.
[docs/testing.md](docs/testing.md) has the rules that keep a test passing
on every machine that runs it.

```sh
cargo run --release --manifest-path tools/objc2-overlay/Cargo.toml   # first
cargo test --workspace                     # macOS: Apple's runtime
scripts/linux-cargo test --workspace       # Linux: Sidestep's runtime
scripts/linux-cargo clippy --workspace --all-targets
cargo fmt --all
```

`scripts/linux-cargo` needs Docker (OrbStack works), and runs
`tools/objc2-overlay` itself. If cargo says it failed to read
`.objc2-overlay/<crate>/Cargo.toml`, run the first line.

## Fixes to objc2

The workspace builds against objc2's published crates with a few fixes that
upstream hasn't released, from `.objc2-overlay/` (see
[docs/abi.md](docs/abi.md#fixed-in-the-objc2-fork-pending-upstream)). A new
fix goes to Sidestep's objc2 fork first (github.com/bitemyapp/objc2), as one
commit written to be sent upstream, on branch `sidestep-main` (objc2's
`main`) and on `sidestep` (the released objc2 0.6.4), then into
`tools/objc2-overlay`: a patch of objc2's hand-written code, or a rule for
what regenerating the bindings would change. Patches never touch or quote
the generated bindings (`src/generated/`), rules work from their structure
(attribute lines and feature names) and never copy from them, and nobody
runs header-translator against Apple's SDK for this (see
[docs/legal.md](docs/legal.md) §3.6). The tool's README says how.
