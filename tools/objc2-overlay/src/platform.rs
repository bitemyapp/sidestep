//! Which platforms objc2's libraries support, and the `cfg` the generator
//! derives from that for an item or a dependency.
//!
//! `Cfg` is a port of `PlatformCfg` in objc2's header-translator
//! (`crates/header-translator/src/cfgs.rs` at `frameworks-0.3.2`, the same
//! on objc2's `main`), with its unit tests, used under objc2's licence (see
//! LICENSE-objc2), plus `Cfg::apple_only`, which fork commit 3 adds to it on
//! the fork's `sidestep-main` branch for toll-free bridging. The platform
//! table is taken from the translation configs (`translation-config.toml`,
//! `configs/*.toml`) of the libraries the patched crates mention, at
//! `frameworks-0.3.2`, which the published 0.3.2 crates were generated from.

/// The platforms a library is available on (Mac Catalyst follows iOS, and
/// the generator ignores it for now, see `Cfg::from_library`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Platforms {
    pub macos: bool,
    pub ios: bool,
    pub tvos: bool,
    pub watchos: bool,
    pub visionos: bool,
    pub gnustep: bool,
}

const fn apple(gnustep: bool) -> Platforms {
    Platforms { macos: true, ios: true, tvos: true, watchos: true, visionos: true, gnustep }
}

const fn no_watchos(gnustep: bool) -> Platforms {
    Platforms { macos: true, ios: true, tvos: true, watchos: false, visionos: true, gnustep }
}

const fn macos(gnustep: bool) -> Platforms {
    Platforms { macos: true, ios: false, tvos: false, watchos: false, visionos: false, gnustep }
}

/// Which translation configs to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// The configs the published crates were generated with.
    Published,
    /// With the fork's `gnustep = true` in CoreGraphics, QuartzCore,
    /// CoreText and ImageIO (fork commit 3).
    Fork,
}

/// The crates fork commit 3 marks as supporting GNUStep.
pub const NEWLY_GNUSTEP: &[&str] = &["objc2-core-graphics", "objc2-quartz-core", "objc2-core-text", "objc2-image-io"];

/// The Apple-only part of `libc` (Mach ports, `cpu_type_t`, `boolean_t`,
/// malloc zones): `configs/libc-darwin.toml` in fork commit 3.
pub const LIBC_DARWIN: Platforms = apple(false);

/// The platforms of the library behind a crate (or a crate feature of the
/// same name), or `None` if it isn't one of the libraries this tool knows.
pub fn library(krate: &str, model: Model) -> Option<Platforms> {
    let fork = model == Model::Fork;
    Some(match krate {
        // Pseudo-libraries and objc2's own crates: everywhere.
        "objc2" | "block2" | "dispatch2" | "libc" | "bitflags" => apple(true),
        "objc2-core-foundation" | "objc2-foundation" => apple(true),
        "objc2-app-kit" => macos(true),
        // Fork commit 3.
        "objc2-core-graphics" | "objc2-core-text" | "objc2-image-io" => apple(fork),
        "objc2-quartz-core" => no_watchos(fork),
        // Apple only.
        "objc2-cloud-kit" | "objc2-core-data" | "objc2-core-services" | "objc2-core-video" => apple(false),
        "objc2-uniform-type-identifiers" => apple(false),
        "objc2-core-image" | "objc2-io-surface" | "objc2-metal" => no_watchos(false),
        "objc2-open-gl" => macos(false),
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    ShouldGate,
    /// Whether a `cfg` is emitted or not is irrelevant: it is already gated
    /// at a higher level.
    #[default]
    AlreadyGated,
    Omit,
}

impl State {
    fn new(enabled: bool) -> Self {
        if enabled { State::ShouldGate } else { State::AlreadyGated }
    }

    fn dependency(&mut self, available: bool) {
        *self = match (*self, available) {
            (State::ShouldGate, true) => State::ShouldGate,
            (State::ShouldGate, false) => State::Omit,
            (state, _) => state,
        };
    }

    fn implied(&mut self, implied: bool) {
        *self = match (*self, implied) {
            (State::ShouldGate, false) => State::ShouldGate,
            (_, false) => State::AlreadyGated,
            (state, true) => state,
        };
    }

    fn active(self) -> bool {
        self == State::ShouldGate
    }

    fn allowed(self) -> bool {
        matches!(self, State::ShouldGate | State::AlreadyGated)
    }
}

/// `PlatformCfg`: starts from the emitting library's platforms, is narrowed
/// by what an item requires, and widened again by what its surroundings
/// already imply.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cfg {
    macos: State,
    maccatalyst: State,
    ios: State,
    tvos: State,
    watchos: State,
    visionos: State,
    gnustep: State,
}

impl Cfg {
    pub fn from_library(lib: Platforms) -> Self {
        Cfg {
            macos: State::new(lib.macos),
            // The generator doesn't handle Mac Catalyst yet (MSRV).
            maccatalyst: State::AlreadyGated,
            ios: State::new(lib.ios),
            tvos: State::new(lib.tvos),
            watchos: State::new(lib.watchos),
            visionos: State::new(lib.visionos),
            gnustep: State::new(lib.gnustep),
        }
    }

    /// The item requires `lib`.
    pub fn dependency(&mut self, lib: Platforms) {
        self.macos.dependency(lib.macos);
        self.ios.dependency(lib.ios);
        self.tvos.dependency(lib.tvos);
        self.watchos.dependency(lib.watchos);
        self.visionos.dependency(lib.visionos);
        self.gnustep.dependency(lib.gnustep);
    }

    /// Restrict to Apple platforms (toll-free bridging outside
    /// CoreFoundation, fork commit 3 on `sidestep-main`).
    pub fn apple_only(&mut self) {
        self.gnustep.dependency(false);
    }

    /// The item's surroundings already require `lib`.
    pub fn implied(&mut self, lib: Platforms) {
        self.macos.implied(lib.macos);
        self.ios.implied(lib.ios);
        self.tvos.implied(lib.tvos);
        self.watchos.implied(lib.watchos);
        self.visionos.implied(lib.visionos);
        self.gnustep.implied(lib.gnustep);
    }

    /// The `cfg` predicate to emit, if any, spelled as the generator spells
    /// it.
    pub fn cfgs(&self) -> Option<String> {
        let allowed = (
            self.macos.allowed(),
            self.maccatalyst.allowed(),
            self.ios.allowed(),
            self.tvos.allowed(),
            self.watchos.allowed(),
            self.visionos.allowed(),
            self.gnustep.allowed(),
        );
        let special = match allowed {
            (true, true, true, true, true, true, true) => return None,
            (true, true, true, true, true, true, false) => Some(r#"target_vendor = "apple""#),
            (true, false, true, true, true, true, true) => Some(r#"not(target_abi = "macabi")"#),
            (true, true, true, false, true, true, true) => Some(r#"not(target_os = "tvos")"#),
            (true, true, true, true, false, true, true) => Some(r#"not(target_os = "watchos")"#),
            (true, true, true, false, false, true, true) => {
                Some(r#"not(any(target_os = "tvos", target_os = "watchos"))"#)
            }
            (true, true, true, true, true, false, true) => Some(r#"not(target_os = "visionos")"#),
            _ => None,
        };
        if let Some(special) = special {
            return Some(special.to_string());
        }

        let mut cfgs: Vec<&str> = Vec::new();
        if self.macos.active() {
            cfgs.push(r#"target_os = "macos""#);
        }
        match (self.ios, self.maccatalyst) {
            (State::ShouldGate, State::ShouldGate | State::AlreadyGated) => cfgs.push(r#"target_os = "ios""#),
            (State::ShouldGate, State::Omit) => cfgs.push(r#"all(target_os = "ios", not(target_abi = "macabi"))"#),
            (State::AlreadyGated, State::ShouldGate) => cfgs.push(r#"target_os = "ios""#),
            (State::Omit, State::ShouldGate) => cfgs.push(r#"target_abi = "macabi""#),
            _ => {}
        }
        if self.tvos.active() {
            cfgs.push(r#"target_os = "tvos""#);
        }
        if self.watchos.active() {
            cfgs.push(r#"target_os = "watchos""#);
        }
        if self.visionos.active() {
            cfgs.push(r#"target_os = "visionos""#);
        }
        if self.gnustep.active() {
            cfgs.push(r#"feature = "gnustep-1-7""#);
        }
        Some(match &*cfgs {
            [] => "any()".to_string(),
            [cfg] => cfg.to_string(),
            cfgs => format!("any({})", cfgs.join(", ")),
        })
    }
}

/// Spelling-insensitive form of a `cfg` predicate: whitespace outside
/// string literals removed, so a predicate rustfmt wrapped over several
/// lines compares equal to the generator's one-line spelling.
pub fn canonical(predicate: &str) -> String {
    let mut out = String::with_capacity(predicate.len());
    let mut in_string = false;
    let mut escaped = false;
    for c in predicate.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if !c.is_whitespace() {
            out.push(c);
        }
    }
    // rustfmt may leave a trailing comma in a wrapped list.
    out.replace(",)", ")")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn assert_cfgs(cfg: &Cfg, expected: &str) {
        let actual = cfg.cfgs().unwrap_or_else(|| "all()".to_string());
        assert_eq!(expected, actual, "{cfg:?}");
    }

    // The tests below are header-translator's own (`cfgs.rs`), ported.

    #[test]
    fn basic() {
        let mut cfg = Cfg::default();
        assert_cfgs(&cfg, r#"all()"#);
        cfg.gnustep = State::Omit;
        assert_cfgs(&cfg, r#"target_vendor = "apple""#);
        cfg.tvos = State::Omit;
        assert_cfgs(&cfg, r#"any()"#);
        cfg.visionos = State::ShouldGate;
        assert_cfgs(&cfg, r#"target_os = "visionos""#);
        cfg.watchos = State::ShouldGate;
        assert_cfgs(&cfg, r#"any(target_os = "watchos", target_os = "visionos")"#);
    }

    #[test]
    fn maccatalyst() {
        let mut cfg = Cfg { macos: State::Omit, tvos: State::Omit, ..Cfg::default() };

        cfg.ios = State::ShouldGate;
        cfg.maccatalyst = State::ShouldGate;
        assert_cfgs(&cfg, r#"target_os = "ios""#);
        cfg.maccatalyst = State::AlreadyGated;
        assert_cfgs(&cfg, r#"target_os = "ios""#);
        cfg.maccatalyst = State::Omit;
        assert_cfgs(&cfg, r#"all(target_os = "ios", not(target_abi = "macabi"))"#);

        cfg.ios = State::AlreadyGated;
        cfg.maccatalyst = State::ShouldGate;
        assert_cfgs(&cfg, r#"target_os = "ios""#);
        cfg.maccatalyst = State::AlreadyGated;
        assert_cfgs(&cfg, r#"any()"#);
        cfg.maccatalyst = State::Omit;
        assert_cfgs(&cfg, r#"any()"#);

        cfg.ios = State::Omit;
        cfg.maccatalyst = State::ShouldGate;
        assert_cfgs(&cfg, r#"target_abi = "macabi""#);
        cfg.maccatalyst = State::AlreadyGated;
        assert_cfgs(&cfg, r#"any()"#);
        cfg.maccatalyst = State::Omit;
        assert_cfgs(&cfg, r#"any()"#);
    }

    #[test]
    fn systematic() {
        #[rustfmt::skip]
        let tests = [
            ((true,  true),  (true,  true),  "all()"),
            ((true,  false), (true,  true),  "target_os = \"ios\""),
            ((false, true),  (true,  true),  "all()"),
            ((false, false), (true,  true),  "all()"),
            ((true,  true),  (true,  false), "target_os = \"macos\""),
            ((true,  false), (true,  false), "any()"),
            ((false, true),  (true,  false), "any()"),
            ((false, false), (true,  false), "any()"),
            ((true,  true),  (false, true),  "all()"),
            ((true,  false), (false, true),  "any()"),
            ((false, true),  (false, true),  "all()"),
            ((false, false), (false, true),  "all()"),
            ((true,  true),  (false, false), "all()"),
            ((true,  false), (false, false), "any()"),
            ((false, true),  (false, false), "all()"),
            ((false, false), (false, false), "all()"),
        ];
        for ((macos_used, macos_avail), (ios_used, ios_avail), expected) in tests {
            let mut cfg = Cfg { macos: State::new(macos_used), ios: State::new(ios_used), ..Cfg::default() };
            cfg.macos.dependency(macos_avail);
            cfg.ios.dependency(ios_avail);
            cfg.maccatalyst = cfg.ios;
            assert_cfgs(&cfg, expected);
        }
    }

    fn dependency_cfg(krate: &str, dep: &str, model: Model) -> Option<String> {
        let mut cfg = Cfg::from_library(library(krate, model).unwrap());
        cfg.dependency(library(dep, model).unwrap());
        cfg.cfgs()
    }

    #[test]
    fn dependency_tables() {
        use Model::*;
        let apple = Some(r#"target_vendor = "apple""#.to_string());
        let any4 = Some(
            r#"any(target_os = "macos", target_os = "ios", target_os = "tvos", target_os = "visionos")"#.to_string(),
        );
        // As published.
        assert_eq!(dependency_cfg("objc2-app-kit", "objc2-core-graphics", Published), apple);
        assert_eq!(
            dependency_cfg("objc2-core-graphics", "objc2-metal", Published),
            Some(r#"not(target_os = "watchos")"#.to_string())
        );
        assert_eq!(dependency_cfg("objc2-quartz-core", "objc2-metal", Published), None);
        assert_eq!(
            dependency_cfg("objc2-quartz-core", "objc2-open-gl", Published),
            Some(r#"target_os = "macos""#.to_string())
        );
        // With the fork.
        assert_eq!(dependency_cfg("objc2-app-kit", "objc2-core-graphics", Fork), None);
        assert_eq!(dependency_cfg("objc2-app-kit", "objc2-core-image", Fork), apple);
        assert_eq!(dependency_cfg("objc2-core-graphics", "objc2-metal", Fork), any4);
        assert_eq!(dependency_cfg("objc2-quartz-core", "objc2-metal", Fork), apple);
        assert_eq!(
            dependency_cfg("objc2-quartz-core", "objc2-open-gl", Fork),
            Some(r#"target_os = "macos""#.to_string())
        );
    }

    #[test]
    fn implied_platforms() {
        // A method needing CoreVideo in a QuartzCore class of AppKit: the
        // class already implies Apple as published, not with the fork.
        for (model, expected) in [(Model::Published, None), (Model::Fork, Some(r#"target_vendor = "apple""#))] {
            let mut cfg = Cfg::from_library(library("objc2-app-kit", model).unwrap());
            cfg.dependency(library("objc2-core-video", model).unwrap());
            cfg.implied(library("objc2-quartz-core", model).unwrap());
            cfg.implied(library("objc2-app-kit", model).unwrap());
            assert_eq!(cfg.cfgs().as_deref(), expected);
        }
    }

    #[test]
    fn canonical_spelling() {
        assert_eq!(canonical("all(\n    feature = \"A\",\n    feature = \"B\"\n)"), r#"all(feature="A",feature="B")"#);
        assert_eq!(canonical(r#"any(target_os = "macos", )"#), r#"any(target_os="macos")"#);
        assert_eq!(canonical(r#"feature = "a b""#), r#"feature="a b""#);
    }
}
