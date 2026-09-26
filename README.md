# Sidestep

Write a Rust GUI app once against [objc2], and build it for macOS or Linux.

On macOS, objc2 talks to Apple's runtime and frameworks as usual. On Linux,
Sidestep provides them: an Objective-C runtime written in Rust, and Foundation
and AppKit classes written in Rust with objc2's own `define_class!`. Your code,
objc2 and its framework crates stay unmodified.

**Status:** early. The runtime is complete enough for objc2's core features
(classes, subclassing, ivars, `super`, reference counting, autorelease pools,
weak references, protocols, blocks). Foundation has strings, a dictionary,
timers and threads. AppKit runs a first slice on Wayland: an application,
windows (with GNOME-style decorations where the compositor leaves them to
the program), views with `drawRect:`, keyboard, mouse and scroll events,
scroll views, fills and paths at any display scale, text with fonts from
fontconfig, shaping, bidi, fallback and color emoji, and the clipboard for
strings. See the [roadmap](docs/roadmap.md).

[objc2]: https://github.com/madsmtm/objc2

## Using it

```toml
[dependencies]
objc2 = "0.6.4"
objc2-foundation = "0.3.2"
sidestep = { git = "https://github.com/bitemyapp/sidestep" }
```

```rust
use objc2_foundation::NSString;

// Links Sidestep's runtime and frameworks on Linux; empty on macOS.
use sidestep as _;

fn main() {
    println!("{}", NSString::from_str("hello from objc2"));
}
```

That `use` line is the only change an app needs. For the fastest message
sends on Linux, build releases with `lto = "fat"`: Sidestep's method lookup
then inlines into every call site (see
[docs/architecture.md](docs/architecture.md#message-dispatch)). Sidestep needs
Rust 1.95 or later. Sidestep switches objc2 to
its GNUstep ABI on Linux by itself, and classes it hasn't implemented yet show
up as link errors, not crashes.

## Test drive

`examples/appkit-slice` is an AppKit program written only against
objc2-app-kit: a window showing a page of code with a caret you can move by
clicking. `SCENARIO` chooses what it does:

| `SCENARIO` | What you see |
|---|---|
| `caret` (default) | the page, with the caret blinking twice a second |
| `idle` | the page, with nothing moving |
| `anim` | the page, with a spinner turning at 60 fps in the top right corner |
| `scroll` | a 2000-line list in an `NSScrollView`, scrolling at 120 px/s |

`SLICE_QUIT_AFTER=<seconds>` makes it quit by itself. `examples/text-demo`
shows text: weights, kerning and ligatures, CJK, emoji, right-to-left and
mixed scripts, alignment, wrapping and truncation; with
`SCENARIO=attributes`, underline styles, strikethrough, outlined and
slanted text, baseline offsets, tab stops and wrapped mixed-direction
paragraphs.

**macOS** runs it on Apple's AppKit:

```sh
cargo run -p appkit-slice
SCENARIO=scroll cargo run -p appkit-slice
```

**Linux** runs the same source on Sidestep. It needs a Wayland session (X11
isn't supported yet). Fonts come from the system's fontconfig, loaded when
the program runs, as every desktop has it; `SIDESTEP_FONT` and
`SIDESTEP_MONO_FONT` can name a sans and a monospaced font file to use
instead. Nothing else is needed: no libobjc, no GNUstep, and nothing to
build against beyond the C runtime every Rust program links.

```sh
cargo run --release -p appkit-slice
SCENARIO=anim cargo run --release -p appkit-slice
```

**Linux from a Mac, or without a desktop.** With Docker, `scripts/linux-cargo`
builds in a Linux container and `scripts/headless-wayland` runs a program
under a headless sway compositor. With `SHOT`, it saves a screenshot
`SHOT_AFTER` seconds in (default 2) and stops the program. The repository is
mounted at `/work` and the build output at `/target`:

```sh
scripts/linux-cargo build --release -p appkit-slice
SCENARIO=anim SHOT=/work/target/anim.png scripts/linux-run scripts/headless-wayland /target/release/appkit-slice
open target/anim.png
```

The first run builds the container image, which takes a few minutes.

`SCALE=2` (or `1.5`) renders at that output scale, with `SIZE` in pixels
(`SIDESTEP_NO_FRACTIONAL_SCALE=1` makes a program use integer scales only);
`FLOATING=1` floats windows as most desktops do; and
`SIDESTEP_DECORATIONS=client` makes the program draw its own title bar, as
it does on GNOME (sway draws one otherwise):

```sh
SIDESTEP_DECORATIONS=client FLOATING=1 SCALE=2 SIZE=2560x1600 SHOT=/work/target/hidpi.png \
  scripts/linux-run scripts/headless-wayland /target/release/appkit-slice
```

`examples/appkit-input` prints every key, mouse and window event it gets and
exercises the clipboard, several windows and child windows, tracking areas,
cursors, input methods, nested and modal event loops. `THEN` runs a command (an input injector,
`wl-copy`) while the program runs.

On Linux, drawing is recorded on the main thread and rasterized on a
separate render thread, and scroll views are tiled onto Wayland subsurfaces;
see [docs/architecture.md](docs/architecture.md#appkit-a-main-thread-and-a-render-thread).

## How it works

objc2 already supports GNUstep's runtime ABI. Sidestep implements that ABI in
Rust instead of shipping libobjc2 and GNUstep, and provides each framework
class as the linker symbol objc2 refers to. See
[docs/architecture.md](docs/architecture.md) and the exact contract in
[docs/abi.md](docs/abi.md).

## Developing

```sh
cargo test --workspace                  # conformance tests on macOS (Apple's runtime)
scripts/linux-cargo test --workspace    # the same tests on Linux (Sidestep)
scripts/linux-cargo run -p hello
```

Please read [CONTRIBUTING.md](CONTRIBUTING.md) before contributing. It sets out
where behavior may and may not be learned from.

## Legal

Sidestep reimplements interfaces and ships no Apple code. The reasoning, its
limits and a fallback plan are in [docs/legal.md](docs/legal.md); it is
research, not legal advice. Sidestep is not affiliated with or endorsed by
Apple. AppKit, Cocoa and macOS are Apple's trademarks, used here only to
describe compatibility.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in Sidestep by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
