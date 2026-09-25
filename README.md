# Sidestep

Write a Rust GUI app once against [objc2], and build it for macOS or Linux.

On macOS, objc2 talks to Apple's runtime and frameworks as usual. On Linux,
Sidestep provides them: an Objective-C runtime written in Rust, and Foundation
and AppKit classes written in Rust with objc2's own `define_class!`. Your code,
objc2 and its framework crates stay unmodified.

**Status:** early. The runtime is complete enough for objc2's core features
(classes, subclassing, ivars, `super`, reference counting, autorelease pools,
weak references, protocols, blocks), and Foundation has `NSString` and
`NSThread`. AppKit is next; see the [roadmap](docs/roadmap.md).

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

That `use` line is the only change an app needs. Sidestep switches objc2 to
its GNUstep ABI on Linux by itself, and classes it hasn't implemented yet show
up as link errors, not crashes.

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
