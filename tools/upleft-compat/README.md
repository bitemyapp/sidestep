# Upleft through objc2

This runner archives Upleft's committed HEAD, applies Linux platform adapters,
and links it to this checkout of Sidestep. It does not edit Upleft, its vendor
submodules, or its test assertions. All framework calls remain objc2 calls.

It needs Sidestep's normal Linux build dependencies, Python 3.12 or newer,
Git, Cargo, and Upleft's initialized vendor submodules. Run from the Sidestep
checkout:

```sh
python3 tools/upleft-compat/run.py --upleft ../upleft
```

The default runs the original `upleft-app` file-watcher tests. Other Cargo
commands can follow `--`:

```sh
python3 tools/upleft-compat/run.py --upleft ../upleft -- check -p upleft-app --all-targets
python3 tools/upleft-compat/run.py --upleft ../upleft --smoke /path/to/document.md
```

If another checkout already has the reference submodules initialized, use
`--vendor-root /path/to/upleft/vendor`. The runner verifies their commits
against the selected Upleft revision. For the current local Nix environment:

```sh
nix develop ../omperor -c python3 tools/upleft-compat/run.py \
  --vendor-root ../omperor/vendor/upleft/vendor
```

Prepared sources live in a content-addressed directory under `target/`;
`--prepare-only` prints its path. Builds use `target/upleft-objc2-build`.
The runner clears display sockets and defaults to the null backend, four
build jobs, no debug info and no incremental compilation. It does not
activate or show windows. Set up an isolated compositor with
`scripts/headless-wayland` for any later interactive checks.

## Platform adapters

Patches 1–4 are the existing Omperor Linux adaptations for Upleft's atomic
file I/O, theme-directory notifications, sentence segmentation and numeric
locale. Patch 5 makes framework linking conditional, reports FoundationModels,
AppIntents and speech synthesis unavailable on Linux, retains timer/manual
updater checks without Network.framework wake-ups, and handles libc/encoding
ABI differences. Patch 6 uses Linux's monotonic clock for timing. Patch 7 makes
the optional Sparkle classes resolve dynamically, matching the existing
availability check when Sparkle is absent; patch 8 does the same for the four
Sparkle classes `sparkle_bridge_tests` allocates, so it links without Sparkle
and reports the cases that need it as skipped. These are consumer
adaptations, not Sidestep API replacements.
The patches contain Upleft code under [its MIT license](LICENSE-upleft).

## Verified and remaining coverage

On Upleft `6d465855c9c9121594dcd02b5e193b5765485fe5`:

- The full app and all its test targets pass `cargo check` with these adapters.
- The original app `file_watcher_tests` links and passes all 14 tests.
- The original `release_watch_probe_tests` passes all 3 tests, exercising
  NSURLSession with a custom NSURLProtocol.
- Sidestep has public-API tests for dispatch work-item ownership, cancellation,
  completion and barriers; UTType factories, tags, copying, conformance and
  bundle declarations; and atomic replacement, backups and error preservation.
- The full GUI executable links, and its original `UPLEFT_HEADLESS_SMOKE`
  document-open path completes. The checked fixture reports 9 menu items,
  89 document characters, title `test.md`, and a 1020×728 content area.
  The same check passes from a fresh archive prepared by this runner.
- Additional framework conformance covers HTTP and cancellation, file handles,
  directory enumeration, snapshot compression, calendar DST arithmetic,
  formatters, cross-process notifications, container ownership, text-layer
  pixels, PDF saving, toolbars, safe areas, gestures, sharing and window KVO.
  See [the implementation limits](../../docs/abi.md#framework-classes-used-by-upleft).

The headless check still emits required-layout-constraint warnings. It does
not establish visual correctness or complete interactive GUI compatibility.
An unbundled archive has no welcome resources or bundle identifier, and
Sparkle is unavailable on Linux; the smoke result reports those absences.
The broader Upleft library failures recorded during native-branch testing are
unchanged baseline failures, not covered by this targeted runner. New
conformance tests have been run on Linux; their macOS runs remain unverified.
