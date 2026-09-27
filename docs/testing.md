# Conformance tests that hold everywhere

The conformance tests run on this Mac against Apple's frameworks, on CI's
macOS runner (a loaded virtual machine with a 1x screen), and on Linux
arm64 and x86_64 against Sidestep. A test that passes on one of them and
not another is usually racing something, or depending on its
surroundings. This file collects what went wrong and the rule each case
left behind.

## Blocks under concurrent enumeration run on several threads

**Status: fixed** in fcba89c ("Make three conformance tests independent of
their surroundings").

`conformance/tests/collections.rs` (`api_surface`) counted a set's members
with a `RefCell<i32>` incremented from an `RcBlock`, passed to both
`enumerateObjectsUsingBlock:` and
`enumerateObjectsWithOptions:usingBlock:` with
`NSEnumerationOptions::Concurrent`, then asserted the total was 4. Apple's
Foundation calls the block of a concurrent enumeration on several threads
at once, so the increments were a data race (undefined behaviour), and the
count sometimes came up short: once `left: 3, right: 4`, in a full
`cargo test --workspace` on 2026-09-26, then six passing reruns.
`conformance/tests/dictionary.rs` (`api_surface`) had the same pattern with
`enumerateKeysAndObjectsWithOptions:usingBlock:`.

Both now count with an `AtomicUsize` (`fetch_add(1, Ordering::Relaxed)`)
and keep their assertions. The other blocks the tests hand to a concurrent
enumeration or sort (`objectsWithOptions:passingTest:` in collections.rs,
`keysOfEntriesWithOptions:passingTest:` in dictionary.rs,
`sortWithOptions:usingComparator:` in collections2.rs) touch no shared
state, so they need nothing.

**Rule:** a block passed with `NSEnumerationOptions::Concurrent` or
`NSSortOptions::Concurrent` may run on any thread, several at once. It
shares state only through atomics or a lock; a `RefCell` or `Cell` it
touches is a race. `grep -n Concurrent conformance/tests/*.rs` lists them.

## Rules for the rest

- **Never ask macOS to open a URL nothing handles.** It puts up an alert
  ("There is no application set to open the URL …") that stays until
  someone dismisses it, one per test run. `conformance/tests/panels.rs`
  opens URLs and files only on Linux, or on macOS with
  `SIDESTEP_CONFORMANCE_OPEN_URLS=1`.
- **Windows are opt-in on macOS** (`SIDESTEP_CONFORMANCE_WINDOWS=1`), and a
  test never activates the application.
- **The default harness runs tests on parallel threads on macOS.** AppKit
  once gave no font to one of several threads asking at once
  (`text_blocks.rs`, `text_layout.rs` now make theirs under a lock);
  serialize what may not like it.
- **CI's macOS screen is 1x**, this Mac's 2x: control geometry differs, so
  `controls.rs` and `control_images.rs` report 1x differences instead of
  failing on them.
- **Never depend on light or dark mode** (this Mac is dark, CI light), or
  on fonts other than the one a test loads: Ubuntu has only DejaVu.
- **Wait with a deadline, never a fixed window.** A loaded runner can take
  far longer than a fast machine; count only what happens inside the run
  being measured, and bound counts from above.
- **Release builds skip the autorelease** of returned objects, so an object
  only an autorelease pool kept alive in debug is gone at once in release.
  Hold what a test checks.
- **The system posts events of its own.** A test draining the event queue
  counts only the event types it posted (`control_events.rs` ignores
  AppKit- and system-defined ones).
- **Only macOS knows the other applications.** Validation of
  `hideOtherApplications:` or `unhideAllApplications:` depends on what else
  is running and hidden, so the tests pin those answers on Sidestep only.
- **Linux tests wait for first frames by their condition**
  (`testing::settle_first_frames`): a window's first frame waits for the
  desktop's appearance, which the null render thread never tells.
