# Legal position: building on objc2's published bindings

Drafted 2026-09-25. This is research, not legal advice (see section 6).

## 1. Summary

Sidestep can reasonably depend on the unmodified, published objc2 crates instead of forking them or keeping its own copy of their API surface. What Sidestep takes from Apple through those crates is an interface: class names, selectors, method signatures and constant values. US law treats reimplementing an interface so programmers can carry their skills to a new platform as fair use (*Google v. Oracle*). The Ninth Circuit, where Apple's SDK agreement sends disputes, holds compatibility requirements unprotected; names are not copyrightable; EU law excludes the ideas behind interfaces. That agreement binds only those who accept it. A fork would be worse than a dependency, because it makes Sidestep the redistributor of the Apple-derived material. The risk is not zero: the findings below narrow the margin, so Sidestep should keep a drop-in fallback ready, with independently written declarations under the same Rust names and signatures.

> **Findings that change the premise**
>
> 1. **objc2 flags the question itself.** Its [LICENSE.md](https://github.com/madsmtm/objc2/blob/main/LICENSE.md) (since January 2025) says the crates are derived from Apple SDKs and "it is unclear whether distributing derived works such as these crates are allowed." It justifies ordinary SPDX licences on the ground that linking requires Xcode, so every user has accepted the Xcode licence. On Linux under Sidestep that ground is gone. In June 2026 the maintainer referred to [bigger problems with the legality of these crates](https://github.com/madsmtm/objc2/issues/836#issuecomment-4755524851). Open [PR #808](https://github.com/madsmtm/objc2/pull/808) says he has been talking to the Rust Foundation about this since September 2025.
> 2. **The generated crates hold more than declarations.** `header-translator` copies Apple's header comments into rustdoc. objc2-app-kit 0.3.2 carries about 3,950 lines (about 51,600 words) of that prose, including Objective-C sample code. objc2-foundation 0.3.2 carries about 3,200 lines (29,200 words). In the macOS 27.0 SDK, every AppKit header and nearly every Foundation header carries only an Apple copyright notice, with no licence. The exception is one Foundation header that reproduces ICU tables under ICU's own terms.
> 3. **Licences differ by crate.** objc2, block2 and objc2-foundation are MIT only. objc2-app-kit is Zlib OR Apache-2.0 OR MIT. The published `.crate` files contain no licence text ([#836](https://github.com/madsmtm/objc2/issues/836), closed as won't-fix).
> 4. **The OpenStep specification granted no licence to implement it.** The [1994 text](https://www.gnustep.org/resources/OpenStepSpec/OpenStepSpec.html) allows one copy for study and says "No such license is granted or implied" by publication. GNUstep rests on thirty years of non-enforcement, not on permission.
> 5. **The Xcode agreement cuts both ways.** It lets licensees use the macOS SDK to develop "application and other software", which is broader than we assumed. It also forbids derivative works of the Apple Software and "enabl[ing] others" to make them. The macOS licence counts "interfaces" as Apple Software and bans reverse engineering. In *SAS v. WPL* (4th Cir. 2017), a similar clause was read to cover black-box study aimed at building a similar product.

## 2. What Sidestep copies and what it does not

| Material | Sidestep | How |
|---|---|---|
| API names, selectors, signatures, constant values | Uses | Through objc2-foundation and objc2-app-kit from crates.io, under madsmtm's licence. A Linux build contains selector strings, class names and constants. |
| Header prose inside those crates | Neither uses nor republishes | Stays in the crates.io source. Doc comments are not compiled into binaries. |
| Apple SDK headers and `.tbd` stubs | No | Never vendored, and not needed on Linux. |
| Apple binaries, disassembly, class-dump output, debug symbols | No | Forbidden (section 4). |
| Apple documentation text | No | Read by people for facts, never copied. |
| Objective-C runtime | Matches an ABI | The GNUstep libobjc2 C ABI (MIT), reimplemented rather than copied, through objc2's existing `gnustep-2-1` support. |

On macOS, apps build against the real SDK on an Apple computer, by a developer who has accepted the Xcode agreement. That is objc2's intended use.

## 3. The arguments

**3.1 Reimplementing declarations is fair use.** In [*Google LLC v. Oracle America, Inc.*, 593 U.S. 1 (2021)](https://www.supremecourt.gov/opinions/20pdf/18-956_d18f.pdf) (6–2, Breyer, J.), the Court held that Google's copying of the Java SE declarations was fair use as a matter of law. The Court assumed copyrightability "for argument's sake" and did not decide it. On each factor:

- **Nature of the work.** Declaring code is bound up with uncopyrightable ideas. Its value comes largely from programmers' investment in learning it.
- **Purpose.** Google reimplemented the interface so programmers could use their existing skills in a new environment. It wrote its own implementing code.
- **Amount.** The 11,500 copied lines were 0.4% of 2.86 million, and the copying was tied to a valid purpose.
- **Market.** Android was not a market substitute for Java SE.

[*Andy Warhol Foundation v. Goldsmith*, 598 U.S. 508 (2023)](https://www.supremecourt.gov/opinions/22pdf/21-869_87ad.pdf), notes 8 and 18, restated that reasoning. It stressed that Google copied only what the new purpose needed. Sidestep fits: a new environment (Linux, where Apple sells no AppKit), its own implementations, names that matter because developers already know them, and only a subset of the API.

**3.2 The Federal Circuit rulings do not control.** [*Oracle v. Google*, 750 F.3d 1339 (Fed. Cir. 2014)](https://law.justia.com/cases/federal/appellate-courts/cafc/13-1021/13-1021-2014-05-09.html) held the declarations copyrightable, and 886 F.3d 1179 (Fed. Cir. 2018) rejected fair use. The Federal Circuit heard the case only because of patent claims and applied its reading of Ninth Circuit law, which does not bind the Ninth Circuit or its district courts. The Supreme Court reversed the 2018 decision and only assumed the 2014 holding for argument's sake. The Xcode agreement sends disputes to the Northern District of California (§8.6), which is in the Ninth Circuit.

**3.3 Compatibility requirements are unprotected.** Two Ninth Circuit cases hold this, and *Google* cited both approvingly:

- [*Sega v. Accolade*, 977 F.2d 1510 (9th Cir. 1992, amended 1993)](https://law.resource.org/pub/us/case/reporter/F2/977/977.F2d.1510.92-15655.html) held that the functional requirements for compatibility are not protected (17 U.S.C. §102(b)). It also held that disassembly to reach them was fair use.
- [*Sony v. Connectix*, 203 F.3d 596 (9th Cir. 2000)](https://law.resource.org/pub/us/case/reporter/F3/203/203.F3d.596.99-15852.html) held that an emulator bringing games to a new platform was "modestly transformative" and a legitimate competitor.

Other doctrine points the same way:

- [*Computer Associates v. Altai*, 982 F.2d 693 (2d Cir. 1992)](https://law.justia.com/cases/federal/appellate-courts/F2/982/693/137252/) filters out elements dictated by the compatibility requirements of other programs.
- [*Lotus v. Borland*, 49 F.3d 807 (1st Cir. 1995)](https://law.justia.com/cases/federal/appellate-courts/F3/49/807/551122/), affirmed by an equally divided Court (516 U.S. 233 (1996)), held a command hierarchy to be an uncopyrightable method of operation.
- [*Baker v. Selden*, 101 U.S. 99 (1879)](https://supreme.justia.com/cases/federal/us/101/99/) held that copyright in a description gives no exclusive right to the system it describes.
- [37 C.F.R. §202.1(a)](https://www.law.cornell.edu/cfr/text/37/202.1) excludes words and short phrases, including names.

Each selector is a name, so any claim would rest on selection and arrangement. That is exactly the ground *Google* covers.

**3.4 EU law reaches the same result.** [Directive 2009/24/EC](https://www.legislation.gov.uk/eudr/2009/24/contents) Art. 1(2) excludes the ideas and principles behind a program's interfaces. [*SAS Institute v. World Programming*, C-406/10 (CJEU, 2 May 2012)](https://curia.europa.eu/site/upload/docs/application/pdf/2012-05/cp120053en.pdf) held that functionality, programming languages and data-file formats are not protected expression. Art. 5(3) lets a lawful user observe, study and test a program, and Art. 8 voids contract terms to the contrary. WPL then won in England ([2013] EWHC 69 (Ch); [[2013] EWCA Civ 1482](https://caselaw.nationalarchives.gov.uk/ewca/civ/2013/1482)), apart from limited copying from SAS's manual (verified from secondary sources only). In the US, [*SAS v. WPL* (Fed. Cir. 2023)](https://law.justia.com/cases/federal/appellate-courts/cafc/21-1542/21-1542-2023-04-06.html) affirmed dismissal because SAS could not identify protectable expression after filtration.

**3.5 Contracts bind only their parties.** The Xcode and Apple SDKs Agreement ([EA2002, dated 2026-06-08](https://www.apple.com/legal/sla/docs/xcode.pdf)) is between Apple and whoever accepts it. As [*ProCD v. Zeidenberg*, 86 F.3d 1447 (7th Cir. 1996)](https://law.justia.com/cases/federal/appellate-courts/F3/86/1447/538242/) puts it, contracts bind their parties and "strangers may do as they please". A Linux user who compiles a Sidestep app never touches Apple software. For those who do accept it, §2.2.A(ii) allows macOS SDK use on Apple computers to develop "application and other software", while §2.5 and §2.7 forbid running the SDK on other hardware, redistributing it, or making derivative works of it.

[*Apple v. Psystar*, 658 F.3d 1150 (9th Cir. 2011)](https://law.justia.com/cases/federal/appellate-courts/ca9/10-15113/10-15113-2011-09-28.html) upheld Apple's OS licence because it did not stop others from developing their own operating systems; Psystar lost because it shipped Apple's own OS. [*Apple v. Corellium*](https://law.justia.com/cases/federal/appellate-courts/ca11/21-12835/21-12835-2023-05-08.html) copied iOS itself and still won fair use (S.D. Fla. 2020; unpublished 11th Cir. affirmance 2023; settled December 2023 per press reports). Sidestep ships no Apple code.

**3.6 Depending is not redistributing.** Sidestep's repository and crates contain none of objc2's source. Cargo fetches it from crates.io, where madsmtm publishes it under his licence. A Linux binary contains compiled objc2 code (strings, constants, call sites, no comments): declaring-code material, which *Google* addresses. A fork or vendored copy would make Sidestep a redistributor of everything, prose included, so it is strictly worse. Binary distributors should reproduce objc2's MIT notice.

**3.7 Upstream supports non-Apple runtimes.** objc2-foundation and objc2-app-kit 0.3.2 have `gnustep-1-7` through `gnustep-2-1` features and `gnustep = true` in their translation configs. With those features the crates link `gnustep-base` and `gnustep-gui`. objc2's own documentation says the bindings can be used on Linux or BSD with the GNUstep runtime, while calling that support second-class.

**3.8 There is long, unchallenged precedent.**

| Project | Licence | Where its declarations come from |
|---|---|---|
| [GNUstep](https://github.com/gnustep/libs-gui) (headers dated 1995–96) | LGPL-2+ | Written independently from the OpenStep specification |
| [Cocotron](https://github.com/cjwl/cocotron) (2006–) | MIT | Written independently |
| [WinObjC](https://github.com/microsoft/WinObjC) (Microsoft, 2015–2022) | MIT | Written independently, building on Cocotron and Iconfactory's Chameleon |
| [Apportable Foundation](https://github.com/apportable/Foundation) (2011–2014) | LGPL-2.1 / MPL-2.0 | Unverified |
| [Darling](https://docs.darlinghq.org/contributing/generating-stubs.html) (2012–) | GPL-3.0 | Stubs generated with class-dump from Apple's framework binaries |
| [PyObjC](https://pyobjc.readthedocs.io/en/latest/changelog.html) | MIT | Generated from macOS SDK headers |
| [.NET for macOS](https://github.com/dotnet/macios) (from 2010) | MIT | Hand-written; every selector appears as a string |
| Rust crates objc (2014), cocoa (2015), core-foundation-sys (2015) | MIT; MIT/Apache-2.0 | Hand-written |
| objc2 (2021–); frameworks (2024–) | MIT; trio | Generated from SDK headers |
| [Wine](https://web.archive.org/web/20240121082037/https://wiki.winehq.org/Clean_Room_Guidelines) (1993–) | LGPL-2.1+ | Written independently under a strict clean room |
| [MinGW-w64](https://www.mingw-w64.org/contribute/) | ZPL-2.1 | From MSDN and clean-room work |
| [winapi](https://github.com/retep998/winapi-rs) (2014–) | MIT/Apache-2.0 | Gathered by hand from the Windows 10 SDK |

Microsoft goes further for its own platform. It licenses [win32metadata](https://github.com/microsoft/win32metadata), which is derived from its SDK headers, under MIT, while the headers stay under the SDK licence. I found no reported suit by NeXT or Apple against any project in this table. That is absence of evidence, not a promise.

**3.9 Side issues.**

- **DMCA §1201(f).** [§1201(f)](https://www.law.cornell.edu/uscode/text/17/1201) is tangential, because Sidestep circumvents nothing.
- **Trademarks.** Class names used in code are functional, not trademark uses. Apple [registers](https://www.apple.com/legal/intellectual-property/trademark/appletmlist.html) Cocoa, Objective-C, Xcode, macOS and Swift. AppKit is not on its list, but should be treated as a mark anyway. Apple's [third-party guidelines](https://www.apple.com/legal/intellectual-property/guidelinesfor3rdparties.html) and nominative fair use (*New Kids on the Block v. News America*, 971 F.2d 302 (9th Cir. 1992), verified from secondary sources only) allow referential phrases such as "compatible with". Neither allows Apple marks in product names or any suggestion of endorsement.
- **Patents.** Patents are a separate axis and are not analysed here. objc2's Apache-2.0 patent grant does not cover Apple.

## 4. Residual risks and mitigations

1. **Apple's header prose inside objc2's crates.**
   - Never vendor objc2 sources or ship source bundles that include them.
   - Mark re-exports `#[doc(no_inline)]` so Sidestep's docs link to objc2's docs instead of republishing them.
   - Never copy those comments into Sidestep.
   - Pin versions and watch PR #808, which would import Apple's documentation-bundle text. If it lands, stay on earlier versions or switch to the fallback.
2. **objc2 changes course.** Upstream could relicense, strip bindings or receive a takedown after its Rust Foundation talks. Track LICENSE.md and PR #808; either event triggers the fallback.
3. **The EU source-code caveat.** In *SAS*, the CJEU said that building similar elements with the help of a program's own source code could be prohibited. Header-derived declarations sit closer to that line than independent ones; the fallback removes the issue.
4. **Contributors' own contracts.** Anyone with a Mac has accepted the macOS licence, Xcode users the Xcode agreement too, and both prohibit reverse engineering.
   - The Fourth Circuit read "reverse engineer" to include black-box study aimed at a similar product ([*SAS v. WPL*, 4th Cir. 2017](https://law.justia.com/cases/federal/appellate-courts/ca4/16-1808/16-1808-2017-10-24.html): the breach-of-licence ruling and a $79.1M unfair-trade award were both affirmed).
   - *Bowers v. Baystate*, 320 F.3d 1317 (Fed. Cir. 2003) (verified from secondary sources only), held that such clauses are not preempted by copyright.
   - Mitigation: on macOS, black-box work means writing and running ordinary programs against public APIs, which §2.2.A(ii) permits. Never probe private state. EU contributors also keep their Art. 5(3) rights.
5. **Apple SDK hygiene.**
   - Never vendor, commit or ship Apple SDK headers or `.tbd` files.
   - Never run header-translator (or bindgen) against Apple's SDK for the Linux side.
   - Linux CI and containers never contain the SDK; §2.7 forbids it on non-Apple hardware.
6. **Clean-room policy, modelled on Wine's guidelines.**
   - Allowed: Apple's public documentation read by a person, GNUstep's documentation and behaviour, objc2's documentation, and test programs against public APIs.
   - Not allowed: scraping developer.apple.com (Apple's [website terms](https://www.apple.com/legal/internet-services/terms/site.html) forbid it), disassembly or decompilation of Apple binaries, class-dump, debug symbols, private API or ivar inspection, dumps of internal tables, leaked source, and copying Apple's documentation prose.
   - Apple open source may be used only under its licence and with its notices. Apache-2.0 swift-corelibs-foundation qualifies. Do not translate APSL objc4 code into Sidestep.
7. **Contributor rule.** Require a DCO sign-off plus a provenance statement in CONTRIBUTING that the contributor used none of the forbidden sources. ReactOS froze development in 2006 to audit roughly three million lines after a disassembly allegation ([Linux.com](https://www.linux.com/news/reactos-suspends-development-source-code-review/)); adopt the rule before the first outside contribution.
8. **Fallback plan.**
   - Publish a drop-in declarations crate with the same module paths, type names, method names and signatures as objc2-foundation and objc2-app-kit, covering only the subset Sidestep implements.
   - Write it from public documentation and black-box tests, cross-checked against GNUstep. Or generate it from Sidestep's own implementation tables, so the implementation is the source of truth.
   - Apps switch through one `[patch.crates-io]` entry or a renamed dependency, with no source changes. Prototype the exact Cargo mechanics early.
   - Generating from GNUstep's LGPL headers would likely make the output a translation covered by the LGPL, which is awkward with Rust's static linking. Treat those headers as a reference, not an input.
   - Triggers: an objection or takedown from Apple, PR #808 landing, a licence change upstream, or counsel's advice.

## 5. The Swift-on-Linux angle

Apple publishes [swift-corelibs-foundation](https://github.com/swiftlang/swift-corelibs-foundation) under Apache-2.0 and describes it as "a compatibility implementation of the Foundation API" for platforms without an Objective-C runtime. In the SDK itself, 40 of 47 CoreFoundation headers carry Apache-2.0 notices and 12 of 17 objc runtime headers carry APSL 2.0. So Apple has itself licensed a Linux reimplementation of Foundation's API; nothing comparable exists for AppKit, which is where Sidestep's residual risk concentrates.

## 6. Not legal advice

This is an engineer's research summary, not legal advice, and no lawyer has reviewed it. Case law is summarised from the sources linked; items marked "verified from secondary sources only" were not checked against the opinion text. Before a commercial release, a trademark filing or any response to a demand letter, get advice from counsel qualified in US and EU copyright law.
