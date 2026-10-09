# Roadmap

Each milestone ends with conformance tests that pass on macOS and Linux.

## 0. Runtime (done)

Objective-C runtime with libobjc2's C ABI, blocks runtime, `NSObject`,
`NSString`, `NSThread`, and 20 conformance tests: classes and subclasses,
ivars and `Drop`, `super`, autorelease pools, weak references under thread
contention, protocols, introspection (including before a class's first
message), blocks, strings. Passing on macOS, and on Linux aarch64 and x86_64.

Since then: every framework class findable by name before its first use,
message forwarding through `-forwardingTargetForSelector:`, the autorelease
return-value handoff, declared properties, methods implemented by blocks,
`objc_msgSend` for direct callers, every association policy, and side
tables (weak references, associated objects, `@synchronized`) sharded so
threads don't contend. Allocating and freeing an object costs about 4 ns
beyond `malloc` and `free`.

Then: categories linked into the program (`category!`), attached before a
class's first use; `NSMethodSignature`, `NSInvocation` and forwarding
through `-forwardInvocation:`, with calls laid out for the aarch64 and
x86_64 calling conventions; `NSProxy`; `objc_msgSend` probing the method
cache in assembly (0.94 ns, against Apple's 1.17); `+load` for framework
classes; methods made from blocks without writable-then-executable
memory.

- Categories' own `+load`; `long double` returns through `NSInvocation` on
  x86_64 (the x87 stack).

## 1. Foundation core

Done so far: constant strings from static memory, an immutable
`NSDictionary` (faster than Apple's on the same Mac, see
`examples/dictbench`), and the system services: `NSRunLoop` and
CFRunLoop with modes, timers, observers, sources and cross-thread
handoff; `NSTimer`, `NSDate`, `NSNotification` and
`NSNotificationCenter`; `NSThread` and the `performSelector…` family;
libdispatch (queues, groups, semaphores, `after`, timer, data and vnode
sources) and `NSOperationQueue` with dependencies; `NSData`, `NSURL`, `NSURLComponents`,
`NSError`, `NSFileManager` over XDG directories, `NSBundle`,
`NSProcessInfo`, `NSUUID`, the runtime lookup functions,
`NSUserDefaults`, property lists, `NSJSONSerialization`, the locks,
`NSDateFormatter` with `NSLocale` and `NSTimeZone`, and toll-free
CoreFoundation for strings, data, dates, errors, URLs, dictionaries and
preferences. `examples/servicebench` measures the run loop and the
notification center.

Collections and values: `NSArray`, `NSMutableArray`, `NSMutableDictionary`,
`NSSet`, `NSMutableSet`, `NSIndexSet`, `NSMutableIndexSet`, `NSEnumerator`
and fast enumeration with mutation detection, `NSNumber`, `NSValue` and
`NSNull`, with Foundation's descriptions, copy-on-write copies, and its
failures (out-of-range indexes, nil elements) as panics carrying its
messages. Mutable collections may be read from several threads at once, as
in Foundation; mutable arrays change at either end in constant time, so
they work as queues; app subclasses that implement only the primitive
methods get the rest. Faster than Apple's on the same Mac in nearly every
operation `examples/arraybench` and `dictbench` measure; creating a small
`NSNumber` is the exception (Apple's are tagged pointers).

Then `NSOrderedSet` and `NSMutableOrderedSet` (with the live `-array` and
`-set` views), `NSSortDescriptor` and sorting arrays, sets and ordered sets
by descriptors (key paths through `-valueForKey:` where objects answer it,
else their getters), `NSHashTable`, `NSMapTable` and `NSPointerArray` with
strong, weak and unretained members and object, address and integer
personalities (weak members disappear when their objects deallocate),
`NSPointerFunctions` options, `NSCountedSet`, and a thread-safe `NSCache`
with count and cost limits, least-recently-used eviction and its delegate.
Faster than Apple's in every operation `arraybench` measures for them
but two: a weak table's `-count`, which Sidestep keeps exact, and a
single change deep inside a large mutable ordered set (1.7 times Apple's
time at a random place among 20,000 members), which renumbers its index;
changes at its ends and changes of many members at once are faster.

Strings: `NSString` and `NSMutableString` with encodings (and to and
from `NSData`), comparison, search, case mapping, normalization, lines and
enumeration, paths and numbers; `NSAttributedString` and
`NSMutableAttributedString`; `NSCharacterSet`, `NSScanner`,
`NSRegularExpression` and `NSTextCheckingResult`; the `NSRange` and
`NSGeometry` functions (see `examples/strbench`).

- Strings, still to do: initializers and writers for files and URLs;
  encodings beyond ASCII, Latin-1, Windows-1252, Mac Roman and the UTF
  forms; locale tailoring for case mapping and collation, and
  `NSLocale` arguments generally; dictionary-based word breaks for CJK and
  Thai; in regular expressions, character names (`\N{…}`), `\G`, full case
  folding and exact hit-end flags.
- `-description` on NSObject before Foundation's `NSString` has loaded.
- Collections: key-value coding on collections (beyond `NSDictionary`'s
  `-valueForKey:`), `NSCoding`; class factory methods (`+array`,
  `+dictionary`) that return the receiving subclass; the C functions of
  `NSHashTable` and `NSMapTable` (`NSHashGet`, `NSMapInsert`, …), custom
  `NSPointerFunctions` functions, and C-string and struct personalities;
  `NSDiscardableContent` in `NSCache`; ordered collection differences.
- `NSAutoreleasePool`; `NSCalendar`, `NSDateComponents` and
  `NSNumberFormatter`; locale data beyond English.
- `NSStream`; `NSURLSession`; `dispatch_io`, `dispatch_data` and
  dispatch blocks (`dispatch_block_create`).

## 2. AppKit skeleton

The first slice runs `examples/appkit-slice` unchanged on Wayland: one
window, a flipped view drawing text with `drawRect:`, mouse clicks moving a
blinking caret, a spinner at 60 fps, and a 2000-line list in an
`NSScrollView`. It has `NSApplication` and its delegate, `NSWindow`,
`NSView` (frames, bounds, flipping, conversion, hit testing, autoresizing),
`NSClipView`, `NSScrollView`, mouse and scroll `NSEvent`s through the
responder chain, `NSColor`, `NSFont`, `NSBezierPath` fills and string
drawing. The design is in [architecture.md](architecture.md).

The platform layer since (`examples/appkit-input` shows it):

- Keyboard: keymaps compiled in pure Rust (kbvm), compose and dead keys,
  key repeat, key-down, key-up and flags-changed events with characters
  and modifier flags, key equivalents, AppKit's standard key bindings
  (`interpretKeyEvents:`, `insertText:`, `doCommandBySelector:`; a table of
  AppKit's answers for every key of a US keyboard with every combination of
  modifiers holds both platforms to the same bindings), key and main window
  focus and application activation, input methods (zwp_text_input_v3 into
  `NSTextInputContext` and `NSTextInputClient`: marked text, committed
  text, the caret rectangle), dead keys shown as marked text.
- Pointer: enter and leave, all buttons, click counts, horizontal and
  high-resolution scrolling (Shift turns a wheel sideways, natural
  scrolling is reported), `NSCursor`'s standard cursors as cursor shapes
  with a themed fallback, `NSTrackingArea`s and cursor rectangles,
  touchpad scroll phases and momentum paced by frames, pinch to magnify and
  rotate.
- Windows: several at once, client-side decorations where the compositor
  wants them (GNOME), light or dark as the desktop prefers, resizing by the
  user, size limits, zoom, full screen, miniaturize, occlusion, borderless
  child windows as popups, activation, window settings (background color,
  click-through, movable, title visibility, initial first responder),
  dragging a window by its content.
- HiDPI at integer and fractional scales.
- Nested event loops (`nextEventMatchingMask:…`, posted events), modal
  loops (`runModalForWindow:`), local event monitors; target/action through
  the responder chain.
- The general `NSPasteboard` as the Wayland clipboard, bounded so a client
  that never answers can't stall the main thread more than once a copy:
  `NSPasteboardItem`s, any type (text, HTML, images, URL lists as an item
  per file, types of Sidestep's own), the old type names,
  `declareTypes:owner:` and data providers asked on demand (by other
  programs too, without a new selection), `writeObjects:` and
  `readObjectsForClasses:options:` for items, strings, URLs and the
  program's own classes, type conformance (in `availableTypeFromArray:`
  too), `canReadObjectForClasses:options:` from the types alone.
  Pasteboards made by name stay in the process, so they don't pay for a
  pasteboard server as Apple's do: a copy and paste costs 0.1 µs against
  100 µs (`conformance/tests/pasteboard.rs` times them).
- Drag and drop into windows: `registerForDraggedTypes:` (files and links
  told apart), the destination messages (periodic updates too) on views
  and windows, safe against nested event loops, the dragging info, its
  dragging items and the drag pasteboard over wl_data_device.
- `NSScreen`: the outputs, their frames, work areas and scales, a window's
  screen, and the delegate calls when they change.
- Views and containers (`examples/containers` shows them): the NSView
  contract (hierarchy messages in AppKit's order, identifiers, bounds
  sizes stored, scrolling helpers), a layout pass before each frame, Auto
  Layout on kasuari (constraints, anchors, layout guides, autoresizing
  masks as constraints, intrinsic sizes, priorities as tiers, content
  that resizes its window, `fittingSize`, ambiguity), `NSStackView`,
  `NSSplitView` (dragged dividers, autosave), `NSTabView`, and a
  view-based `NSTableView` (the plain, inset, full-width and source list
  styles' geometry; views only for rows near the visible ones, reused by
  identifier; variable heights; selection following rows; column
  notifications; clicks; selections emphasized in the key window's
  focused table and gray elsewhere).
- Cells on a table's selection, as AppKit's: row views give their interior
  background style to cell views and to the cells of the controls under
  them (as subviews are added, and before the row next draws after a
  change), cell views pass theirs down, and controls answer their cell's;
  interior styles follow each cell's own background (a bezeled field's or
  a push button's content is on its bezel, a borderless button's on the
  row); on an emphasized background text fields turn the label colors
  light, in their text color and attributed runs alike (and a text color
  with the label color's value, however it was made), while other
  colors and the placeholder stay, plain cells and buttons draw their
  titles light (a content tint too), and push buttons' bezels take a
  light wash. Snapshots lay out and send `viewWillDraw` first, as macOS
  does.
- Scroll views (`examples/containers` shows them: `SCENARIO=overlay`,
  `transparent`, `nested`, `hscroll`, `fling`, `stream`, `idle`):
  `NSClipView` (document rectangles, content insets, constraining,
  backgrounds, the document cursor), `NSScrollView` (tiling with borders,
  both scroller styles and insets, reflecting, autohiding, replacing its
  parts, the size helpers, line and page amounts, `pageUp:` and
  `pageDown:`, magnification kept, clamped and anchored as AppKit does),
  `NSScroller` (AppKit's widths and parts at every style and size, hit
  testing, knob drags and paging from mouse events, overlay scrollers that
  fade and widen), wheel, touchpad and scroller scrolling with live scroll
  notifications and nested scroll views passing on what they can't use,
  clip views following their documents as AppKit's do (frame
  notifications held back, `viewFrameChanged:`, documents leaving), and
  views' `layerContentsRedrawPolicy` defaults. On screen, clip views get layers
  of their own when it pays: nested ones too, scrolling both ways,
  transparent or opaque, with overlays for what is drawn over them, tiles
  anchored so growing documents keep their pixels, uploads of only what
  changed and commits of only what moved, and counters behind
  `SIDESTEP_TRACE_FRAMES`.

Drawing (`examples/drawing-gallery` shows it, and draws the same
pictures on macOS for comparison):

- `NSGraphicsContext` with a graphics state (transforms, clips, compositing
  operations, antialiasing, shadows), bitmap contexts in every layout
  AppKit draws into (RGBA, gray, RGB padded to four samples, alpha first,
  16-bit and floating-point samples), and AppKit's drawing functions
  (`NSRectFill`, `NSFrameRect`, `NSRectClip`, …); `NSAffineTransform`.
- All of `NSBezierPath`: curves, arcs, rounded rectangles, strokes with
  caps, joins and dashes, hairlines, winding rules, hit testing, clipping,
  flattening and reversing, with AppKit's element structure.
- `NSColor` (components, system and dynamic colors, derived colors) and
  `NSColorSpace`; `NSAppearance` following the desktop's light, dark and
  high-contrast settings and accent color, inherited by windows and views.
- `NSImage`, `NSBitmapImageRep` and `NSCustomImageRep`: pure-Rust codecs
  for PNG, JPEG, GIF, WebP, BMP, TIFF and ICO, decoding off the main
  thread, a texture cache with mipmaps, representation choice by device
  pixels, drawing handlers, `lockFocus`, view snapshots
  (`cacheDisplayInRect:toBitmapImageRep:`); symbol images drawn by
  Sidestep; animated GIFs' frames (`NSImageFrameCount`,
  `NSImageCurrentFrame` and their durations); images read from and
  written to pasteboards (TIFF, the other image types, image files).
- `NSGradient`, `NSShadow`, `NSVisualEffectView` (an opaque material),
  `alphaValue`, and `NSAnimationContext` (a Core Animation transaction, in
  which layer-backed views' frame and alpha changes animate while it
  allows implicit animation, as do changes through a view's `animator`;
  a window's apply at once).
- CoreGraphics (`coregraphics/`, the gallery's `SCENARIO=cg` page):
  `CGContext` sharing `NSGraphicsContext`'s graphics state (paths, fills,
  strokes, dashes, clips and clips to masks, blend modes, alpha, shadows,
  transparency layers, gradients and shadings, images, the text state),
  bitmap contexts in CoreGraphics' layouts, `CGPath` with CoreGraphics'
  element structure, `CGColor` and `CGColorSpace` (named, indexed, ICC and
  extended spaces, by model), `CGImage` in every layout it reads (masks,
  parts, masking), data providers and consumers, `CGGradient`,
  `CGFunction`, `CGShading`, `CGFont`, the geometry and affine functions,
  and AppKit's bridges (`-[NSColor CGColor]`, `-[NSGraphicsContext
  CGContext]`, `-[NSImage CGImageForProposedRect:context:hints:]`,
  `-[NSBitmapImageRep CGImage]`, `-[NSBezierPath CGPath]` and their
  inverses).
- CoreText (`coretext/`, the gallery's `SCENARIO=ct` page), on the text
  engine: `CTFont` as `NSFont` (by
  name, from descriptors and from `CGFont` font files, metrics, glyphs for
  characters, advances, bounds, outlines as `CGPath`s, names, tables,
  traits, feature settings, variations), `CTFontDescriptor` as
  `NSFontDescriptor`, the
  font manager and font collections, `CTLine` and `CTRun` (glyphs,
  positions, advances, indices, typographic and image bounds, carets,
  truncation, justification), `CTTypesetter`, `CTFramesetter` and
  `CTFrame`, `CTParagraphStyle`, drawing glyphs and lines into CGContexts
  (as glyph runs where they stay upright, as outlines otherwise),
  CoreGraphics' glyph drawing, `CFAttributedString` over
  `NSAttributedString`, and the string constants.
- Core Animation (`quartzcore/`, `examples/layer-demo`): `CALayer` with
  macOS's geometry, tree, conversions, hit testing, contents and delegate
  drawing, `CAShapeLayer` and `CAGradientLayer`, the media timing of
  `CABasicAnimation`, `CAKeyframeAnimation` (values or a path, every
  calculation mode), `CASpringAnimation`, `CATransition` and
  `CAAnimationGroup`, `CAMediaTimingFunction`, implicit actions,
  `CATransaction` (nesting, completion groups, the implicit transaction,
  the order of delegate calls and blocks), presentation layers,
  `CAValueFunction`, `CATransform3D` and `NSValue`'s (affine transforms
  interpolating as macOS's do, mirrors and half turns included),
  `onOrderIn`/`onOrderOut`/`onLayout` actions, masks, `CADisplayLink` from
  views, windows and screens, layer-backed views (`wantsLayer`,
  `setLayer:`, `updateLayer` at commits, their frame and alpha animating
  in implicit-animation groups) and `renderInContext:`. The render thread
  composites and animates committed layer trees by itself: a running
  animation costs the main thread nothing, and nothing that shows moving
  (paused, hidden or not yet begun) draws no frames.

Events and window behaviour since (`examples/appkit-events` shows sheets,
modal windows and tooltips; `examples/eventbench` measures):

- AppKit runs on Foundation's run loop: the render thread's messages are a
  source and the display pass an observer in the common modes, which
  include `NSModalPanelRunLoopMode` and `NSEventTrackingRunLoopMode` (and in
  any other mode a loop looks for events in); every loop runs in its mode,
  so timers fire where they were added, and only AppKit's loops take
  events, so a program running the loop itself enters no handler.
- Window and application notifications with every name exported, and
  delegates that hear through the notification center; close,
  `releasedWhenClosed`, moves, resizes, live resizes, backing scale;
  launching, `terminate:` with now, cancel and later answers, the question
  after the last window closes; view frame and bounds notifications.
- Keys to the key window, the window's Tab, Shift-Tab and Escape, the key
  view loop (links, valid key views, selection, recalculation),
  `noResponderFor:`, first mouse, moving windows by their background,
  `+sharedApplication` for subclasses.
- Sheets (attached inside their parent as subsurfaces), modal sessions,
  `NSPanel`, tooltips, frame autosave, `NSViewController` and
  `NSWindowController` (without nibs), periodic events.
- A render thread without a display (`SIDESTEP_BACKEND=null`) and a testing
  module that plays the compositor for input.

What an application reaches at link time and at launch (a census of a
real application's API use found the gaps; `conformance/tests/contract_sweep.rs`,
`funnels.rs`, `constants.rs` and `census_selectors.rs` pin them):

- Every extern constant and function objc2's Foundation, AppKit and
  CoreFoundation crates declare under Sidestep's features is exported
  (string constants with macOS's values, `NSApp`, `kCFBooleanTrue` and
  `kCFBooleanFalse` as the numbers `+numberWithBool:` hands out, the zone,
  page and extra-reference-count functions, `NSApplicationMain` and the
  rest), but for the known gaps in [abi.md](abi.md#known-gaps); a test
  links them all.
- `NSFontManager` (trait, weight and size conversions),
  `NSHapticFeedbackManager` (does nothing), `NSAccessibilityElement` and
  `NSAccessibilityCustomAction` (stored), accessibility children, custom
  actions and parents on views, and posted accessibility notifications.
- The application's Window menu and its list of windows; `clipsToBounds`
  (off, as on macOS 14 and later, with drawing following it);
  `inLiveResize` and the live resize hooks sent to every view;
  `-[NSWindow center]` and `constrainFrameRect:toScreen:`; a default
  content view in every window; `quickLookWithEvent:`; the running
  application's bundle identifier; a text view taking dropped text.
- Funnel points (see [architecture.md](architecture.md#funnel-points)):
  AppKit's methods that other methods reach by message now are, so
  overrides and swizzles see each call.
- Foundation: collections from property-list files, a file URL's resource
  values, `+[NSThread callStackSymbols]`.

Still to do there: the `NSHashTable`/`NSMapTable` C functions,
CoreFoundation's run loop sources, the old bezel functions; `availableFonts`
and `availableMembersOfFontFamily:` on the font manager; dragging text out
of a text view (writing the selection for a drag); swipe and smart
magnify gestures (no Wayland event source); publishing the accessibility
store through AccessKit.

Menus, alerts and panels since (`examples/menus-panels` shows them; see
[architecture.md](architecture.md#menus-alerts-and-panels)):

- `NSMenu` and `NSMenuItem`: the model with its notifications, validation
  (`update`, `validateMenuItem:`, `validateUserInterfaceItem:`), key
  equivalents matched by a rule of Linux's own and labelled as the
  platform names keys, and the main menu in the key-equivalent phase.
- Pop-up and context menus (`popUpMenuPositioningItem:atLocation:inView:`,
  `+popUpContextMenu:withEvent:forView:`, `menuForEvent:` and the default
  `rightMouseDown:`), submenus, AppKit's tracking loop in
  `NSEventTrackingRunLoopMode` with keyboard navigation and the delegate's
  calls, shown as grabbing xdg_popups.
- The main menu as a bar in every titled window, part of the title bar to
  the frame arithmetic (`+[NSMenu setMenuBarVisible:NO]` or
  `SIDESTEP_MENUBAR=hidden` turn it off).
- `NSPopUpButton` and `NSPopUpButtonCell`, pop-up and pull-down.
- `NSAlert`: modal and as a sheet, with its buttons' key equivalents, an
  accessory view, the suppression check box and icons.
- `NSSavePanel` and `NSOpenPanel` through xdg-desktop-portal's file
  chooser (zenity or kdialog without a portal), modal as on macOS and
  cancellable, `NSWorkspace` opening URLs (`NSWorkspaceOpenConfiguration`
  too) and showing files, `NSRunningApplication.currentApplication`,
  `NSBeep`.

Next:

- Popups of sheets (menus, tooltips over a sheet); `windowWillResize:toSize:`
  during a live resize; `NSApplicationWillUpdateNotification` and
  `DidUpdate`; content under a client-side title bar
  (`NSWindowStyleMaskFullSizeContentView`, a transparent title bar); view
  controllers' appearance callbacks; tracking areas' callouts through the
  event queue.
- Input methods: surrounding text (and so deleting around the caret),
  content types from the client.
- Image cursors; dragging from our windows (drag sources, so an editable
  image view's image can't be dragged out yet), and `NSColor` on
  pasteboards.
- Drawing: text under a rotated transform (glyph runs take a translation
  only), pattern colors (and `patternImage` on threads other than the
  image's), `-[NSView lockFocus]`, a window's `animator` animating, blur behind
  visual effect views (no Wayland protocol yet), and batching a bitmap
  context's operations instead of rasterizing each at once (layouts other
  than RGBA are unpacked and packed for each).
- CoreText: vertical text, font matrices, ruby and run delegates in
  layout, ligature carets from `GDEF`, language extents from the font
  (they're DejaVu Sans's for any font), rules fitted to more fonts (the
  cap and x heights of fonts without H and O, the slant trait, underline
  placement), `CTFontCopyFeatures`' names and exclusive groups, runs split
  where the script changes, and a cache of laid-out lines (a line of 26
  characters takes 6.5 µs to make, macOS's 2.3 µs).
- CoreGraphics: pattern colors, conic gradients, the path
  set operations, `CGLayer`, PDF, CMYK and 5-bit bitmap contexts, 16-bit
  and float contexts at their full precision, dithered gradients, color
  management by profile (CMYK, contexts in wide spaces, images and bitmaps
  in calibrated spaces both ways), images of files in their own layouts,
  macOS's upscaling filter, and radial gradients whose circles cross.
- Core Animation: perspective (an affine approximation now, without depth
  between layers), `rotationMode`, `contentsCenter`, filters, conic
  gradients, the continuous corner's exact curve, keyframes' tension,
  continuity and bias, overdamped springs as macOS moves them, a layer's
  own duration and repeats, a window's `animator` animating, `CATextLayer`,
  `CAReplicatorLayer`, `CAScrollLayer`, `CATransformLayer`, `CATiledLayer`
  and `CAEmitterLayer` (`CAMetalLayer` and `CAOpenGLLayer` wait for a GPU
  renderer), and drawing layer trees with a GPU (see architecture.md's
  known differences).
- Scroll views: rubber-banding (Linux desktops don't), rulers
  (`NSRulerView`), the find bar, animated `pageDown:`, drawing at a
  magnification (it scales the clip view's bounds, which drawing doesn't
  follow yet), `addFloatingSubview:forAxis:` kept still while scrolling,
  keeping a layer's pixels through a width change that moves its tile
  columns, and drawing tiles ahead on an idle timer rather than right
  after each frame.
- Menus still to do: menus taller than the screen (scrolling), the
  pointer's path to an open submenu (a submenu closes 150 ms after the
  pointer leaves its item), alternate items (Option), attributed titles
  and views as items, `NSStatusItem` (the StatusNotifierItem protocol),
  the main menu's key equivalents while a modal window is up (they go to
  the menu as they do outside), and menus over sheets. The bar has no
  overflow for more menus than fit.
- Panels still to do: the portal's parent window (xdg-foreign), opening
  files through `OpenURI.OpenFile` (it takes a file descriptor; files open
  with `xdg-open`), `allowedContentTypes` (`UTType`), the panels'
  delegate calls, and the workspace hearing the desktop's settings change.
- Containers: the visual format language; bounds scaling in drawing, hit
  testing and conversion; the table's header in its scroll view, column
  dragging and resizing from the header, hidden rows, type select, drag
  and drop; row views' `previousRowSelected`/`nextRowSelected` and group
  rows' own look; template images and symbols tinted light on an
  emphasized background (the hook, `controls::cell::template_ink`, waits
  for `NSImageView`/`NSImageCell` and images in buttons);
  `NSOutlineView`, `NSCollectionView`; changing a constraint's constant
  in place and removing constraints without scanning the solver (kasuari
  needs ways to), or a solver per independent group of views; a window's
  minimum and maximum sizes from its content's constraints.
- X11, after Wayland is solid.

## 3. Text

Done so far: fonts from the system's fontconfig with fallback, shaping,
bidi, line breaking and color emoji (parley, fontique, swash); `NSFont`
with weights, names, metrics, text styles and descriptors
(`NSFontDescriptor` with symbolic traits, system designs and feature
settings); `NSParagraphStyle` and `NSMutableParagraphStyle`; string
drawing and measuring (`drawAtPoint:`, `drawInRect:`,
`drawWithRect:options:`, `sizeWithAttributes:`,
`boundingRectWithSize:options:`) with fonts, colors, backgrounds,
paragraph styles, kerning, underlines and strikethroughs (with dot and
dash patterns, and by word), baseline offsets, ligatures, strokes,
obliqueness, shadows (without blur) and tab stops (`NSTextTab`, read back
with `tabStops`, laid out in the order set as on macOS); all attribute
name constants; the same for attributed strings (`size`, `drawAtPoint:`,
`drawInRect:`, `drawWithRect:options:context:`,
`boundingRectWithSize:options:context:`), run by run and paragraph by
paragraph, and `NSStringDrawingContext`'s bounds. Rich text interchange:
attributed strings read and written as RTF, flat RTFD, HTML and plain
text (`initWithData:options:documentAttributes:error:`,
`dataFromRange:documentAttributes:error:`, `RTFFromRange:…`,
`initWithRTF:…`, `initWithHTML:…`, `readFromData:…` and the rest) with
AppKit's document attributes and errors, by an RTF reader and writer
written from Microsoft's specification and AppKit's output, and an HTML
reader (no browser engine) and writer shaped as AppKit's; attributed
strings on the pasteboard (`NSPasteboardReading` and `…Writing`: RTF,
HTML, text), and rich copy and paste in text views. The line layout
TextKit will stand on: lines, clusters and carets with UTF-16 ranges and
directions, a paragraph or a few lines at a time on any thread, hit
testing, caret and selection geometry, and relayout of only what an edit
touched. See
[architecture.md](architecture.md#text). Text editing: TextKit 1
(`NSTextStorage` over a paragraph tree, `NSLayoutManager` laying out a
paragraph at a time with idle layout, `NSTextContainer`), text blocks and
tables, `NSText` and `NSTextView` with AppKit's edit transactions,
delegate calls and notifications, selection by grapheme, word and
paragraph, about eighty key-binding commands, the clipboard, input
methods (`NSTextInputClient`) and coalesced typing undo; `NSUndoManager`;
the window's field editor with the API controls start and end editing
with; a keystroke and its layout in 11 MB of text in 0.07 ms at p99
(drawing not measured). `setString:` as lazy as AppKit's: long text is
copied once and cut into paragraphs as reading and layout reach it, and
attributes are fixed lazily as macOS fixes them (edits of 64 K units or
more, a stretch at a time when asked for), so setting 11 MB takes 2 ms
(1.8 ms on macOS; 25 ms before) and 1 MB 0.09 ms. TextKit 2 over the same
storage and line layout: `NSTextContentStorage` and its elements (a
delegate's paragraphs and a subclass's elements included),
`NSTextLayoutManager` with layout fragments and line fragments (a
delegate's fragment subclasses laid out, placed by their frames and drawn
through their `drawAtPoint:inContext:`), estimates for what isn't laid out,
the viewport controller, selections and navigation, `NSTextRange` and
countable locations; text views in TextKit 2 mode by default as on macOS
(and the switch to TextKit 1 when a program asks for the layout manager),
laying out and drawing their viewport: 11 MB, a scroll step and its
layout in 0.1 ms. Text attachments (`NSTextAttachment` with images,
bounds, contents, file wrappers and cells, `NSTextAttachmentCell`) laid
out as inline boxes and drawn in string drawing, TextKit 1 and TextKit 2,
edited as one character, and carried in flat RTFD and on the pasteboard;
`NSFileWrapper`. See [text.md](text.md).

- `NSExpansion` (advances scaled before line breaking); shadow blur (a
  blurred glyph op); tabs in right-to-left paragraphs, measured from the
  right; descriptors' `fontAttributes` with numbers (`NSNumber`);
  `NSStringDrawingContext`'s `minimumScaleFactor` (text isn't shrunk to
  fit: `actualScaleFactor` is always 1).
- Rich text still to do: pictures in RTF (`\pict`) and HTML (`<img>`)
  as attachments; `RTFDFileWrapperFromRange:` and
  `initWithRTFDFileWrapper:` (attachments travel in flat RTFD and
  packages read from URLs); file wrappers' serialized representations
  (flat RTFD on macOS); Word, Word XML,
  OpenDocument and web archive documents; double-byte code pages in RTF
  (Shift-JIS, GBK, Big5, EUC-KR: text beside their `\u` escapes reads
  rightly); `NSTextList` for HTML lists (read as text with markers) and
  `NSTextTable` for HTML tables (cells read as paragraphs); CSS selectors
  with combinators, attributes or pseudo-classes, and `line-height`;
  `fontAttributesInRange:` and `rulerAttributesInRange:`; the character-
  and paragraph-formatting pasteboard types (Copy Style).
- Bidi: clusters give their direction, not the bidi level, for numbers in
  right-to-left text of a left-to-right paragraph (level 2, shown as 0:
  parley keeps levels to itself); explicit embeddings and isolates open
  where a paragraph is laid out from a line aren't carried into it;
  deleting in a paragraph that mixes directions and has brackets lays it
  out whole (tracking bracket pairs across the edit would keep more).
- Two spaces where a line wraps: parley hangs the first and starts the
  next line with the second, where AppKit hangs both.
- The desktop's own interface font where fontconfig's `system-ui` doesn't
  name it (GNOME keeps it in GSettings); dictionary line breaking for Thai,
  Lao, Khmer and Myanmar (parley's `complex-scripts`, several megabytes of
  data).
- Vertical text, hyphenation, `allowsDefaultTighteningForTruncation`.
- Text editing still to do: the find bar and `NSTextFinder`, spelling and
  substitutions, `NSTextList`, several text containers,
  text block height dimensions, vertical alignment, row spans and
  `hidesEmptyCells`, temporary attributes other than a background color
  drawn, `CGGlyphAtIndex:` and `getGlyphsInRange:…`, laying a long paragraph
  out again from the edited line rather than whole (the text engine's frames
  do; the layout manager needs a paragraph entry point for it); once text
  left to fix has been fixed in several stretches, AppKit's effective ranges
  sometimes end between equal attributes where Sidestep's go on; the layout
  manager's copy of a subclass's text (one with text of its own) is still
  read whole through its primitives.
- Text attachments still to do: view providers
  (`NSTextAttachmentViewProvider`: attachments are drawn, never views;
  `allowsTextAttachmentView` and the provider registry are kept but
  nothing asks them); clicks in a text view passed to an attachment's
  cell (`wantsToTrackMouse`, `trackMouse:…`); `lineLayoutPadding` (kept,
  not laid out); the glyph position a subclass's sizing method is told
  (the line's start, where AppKit asks again with the pen's place: text
  is measured once for each set of attributes, before anything is
  placed); archiving (no keyed archiver yet).
- TextKit 2 still to do: text blocks and lists (`NSTextListElement`) in
  fragments; rendering attributes are kept,
  moved by edits and enumerated but not drawn, and the rendering
  attributes validator is kept but not called; elements the content
  storage's delegate filters out of enumeration are still laid out;
  `NSTextContentManager` subclasses whose locations aren't countable
  (elements are indexed by UTF-16 offsets); `textSelectionNavigation`'s
  visual moves in mixed-direction text. The known differences from macOS
  are listed in [text.md](text.md#textkit-2).

## 4. Controls and services

Done so far (`examples/controls-gallery` shows them): `NSCell`,
`NSActionCell` and `NSControl` with real cells, so programs that subclass
either get macOS's behavior; values and their conversions, target and
action, the mouse-tracking loops in AppKit's order of calls,
`performClick:`, copying, hit testing, and the sizes and rectangles a
program can read, as measured on macOS; `NSButton` (push buttons of every
bezel style at every control size, check boxes, radio groups, the default
button and key equivalents), with images in every position at every
bezel (the layout measured on macOS for thousands of cases), template
images tinted by state and look, and alternate images; `NSImageView` and
`NSImageCell` (every scaling, alignment and frame style, as measured;
templates tinted; editable views taking dropped, pasted and deleted
images; animated GIFs);
`NSTextField` as a label, a wrapping label and a field, with
`NSSecureTextField` and `NSSearchField` (display and sizing; editing waits
for the field editor); `NSBox`; `NSProgressIndicator`, animated by the
window's frames; `NSSegmentedControl`, `NSStepper`, `NSSlider` and
`NSSwitch`; intrinsic sizes; focus rings and full keyboard access; and
accessibility properties that views and cells keep but nothing reads yet.
They are drawn in an Adwaita-like theme, light or dark. See
[architecture.md](architecture.md#controls).

Next:

- Editing text fields (the field editor, with the text-editing work).
- Images in buttons: AppKit's layout for a push button with its image
  above or below the title (squeezed into its fixed height), for a
  toolbar button's image above, below or over it, for badge buttons with
  images, for scalings other than proportionally down on titled buttons,
  and for bounds smaller than a button's own size, which it answers in
  ways not yet pinned; check boxes' and radio buttons' own images
  (`image` and `alternateImage` are AppKit's box images there);
  alternate titles, which aren't drawn yet; textured and toolbar
  templates follow AppKit's default blue accent's shades, not yet
  checked with other accents.
- `NSButton`'s factories (`buttonWithTitle:target:action:` and the rest)
  make an `NSButton` even when sent to a subclass, where AppKit makes one
  of the subclass (`+[NSImageView imageViewWithImage:]` does).
- Animated images: GIFs only; other animated formats aren't animated.
- Numbers in cells read as Foundation's `-[NSNumber descriptionWithLocale:]`
  gives them, which on Linux doesn't yet group digits or print "NaN" and
  "∞" as macOS does.
- Check boxes' and radio buttons' `cellSizeForBounds:` (macOS wraps the
  title into the width given) and the width a wrapping label keeps at
  `maximumNumberOfLines`.
- Accessibility through AccessKit, from the store (views' and cells'
  properties, custom elements and actions, and posted notifications such
  as announcements).

## 5. A real app

Omperor, the motivating application, building for Linux with no source
changes beyond `use sidestep as _;`.

## Native toolkit

`sidestep-ui`: Linux programs on Sidestep's engine without AppKit or the
Objective-C runtime, toward a desktop environment's own programs.

Done: the engine carved out of AppKit (`sidestep-engine`, which AppKit
now builds on, unchanged to objc2 programs); an application loop with
timers and cross-thread wakes, applications one after another in a
process; toplevel windows and popups (placed as the positioner allows:
anchor corner, gravity, offset, flip, slide, resize) with their
compositor-owned size and state, decorations where the desktop wants
them, cursors, size limits and requests, hidden and shown again; keys
through XKB with compose sequences, pointer buttons, wheels and touchpad
scrolling (coasting after a flick, with AppKit's physics, now shared) and
pinches, input methods; a canvas over the render thread's ops
(rectangles, shapes, strokes and dashes, gradients, images, groups,
clips, shadows, blend modes, transforms, text upright from the glyph
cache, turned as outlines, synthesized bold thickened, color glyphs as
pictures); text layout with styles, wrapping, alignment, truncation, hit
testing, carets and selections; fonts from the system and from the
program's own files, installed or loaded as one face; the clipboard's
text; drops, the type taken as the program asks, with periodic updates;
outputs, read without waiting; the desktop's appearance and system
colors. Tested headless through the null render thread and under a real
compositor (`tests/wayland_system.rs`: a virtual pointer, `wtype`,
`wl-clipboard`), which CI's `wayland` job runs with AppKit's.

Next, roughly in order:

- **Scrolling layers.** The tiled scroll layers AppKit's scroll views use
  (`PlaceLayer`, tile paints drawn ahead and rasterized between frames),
  as a native `Layer`: content that scrolls without redrawing, which
  lists and editors need. AppKit's `layers.rs` decides which tiles to
  paint and keep; its policy should move into the engine so both toolkits
  share it.
- **Animation.** The engine composites and animates Core Animation's
  layer trees on the render thread; a native layer tree on it, with
  implicit transitions and display-link ticks (`FrameTicks`), so
  animations don't cost the main thread a frame each.
- **Widgets.** A retained view tree with layout, focus and the key view
  loop, hit testing and the theme's painting of controls (AppKit's
  `theme` paints from data, and could move into the engine as AppKit's
  controls' look), then controls: buttons, text fields and text views on
  `TextLayout`, lists, scroll views, menus.
- **Desktop surfaces**, for a desktop environment's own programs:
  wlr-layer-shell panels, docks, wallpapers and overlays; session lock;
  output management; foreign toplevel lists for task switchers.
- **Services**: the portals AppKit's panels and workspace use (file
  choosers, opening URIs), notifications, drags out of the program, the
  clipboard's other types, rich text.
- **AppKit on the toolkit.** As the native toolkit grows, AppKit's
  window, event and layer machinery can become wrappers of its, so the
  two share one implementation instead of two clients of the engine.

## Swift

Research, after Rust works. Swift on Linux is normally built without
Objective-C interop. The questions are whether `-enable-objc-interop` can
target a runtime with this ABI, and what Clang-emitted class structures (the
libobjc2 v2 ABI's `__objc_load` path) the runtime would then need to accept.

## Standing work

- Send the objc2 fork's commits upstream from its `sidestep-main` branch
  (see [abi.md](abi.md#fixed-in-the-objc2-fork-pending-upstream); commit 1
  is already on objc2's `main`, and commit 9 only matters to the 0.6
  releases) once the maintainer agrees, and drop each of the overlay's
  patches and rules as a release includes it.
- Keep the fallback declarations crate from [legal.md](legal.md) ready:
  prototype the Cargo mechanics early.
- CI on Linux x86_64 and aarch64 and on macOS.
- ImageIO's sources and destinations are done (see
  [architecture.md](architecture.md#imageio)); still to do there:
  `CGImageMetadata`, `CGAnimateImage…`, auxiliary data, TIFF pages, ICO
  images and APNG frames past the first, metadata and profiles in files
  written, and HEIC/AVIF (codecs the `image` crate lacks).
- The fork keeps objc2's safe bridging between AppKit's and CoreText's
  font types Apple-only; Sidestep's `NSFont` is a `CTFont` (raw casts
  work), so the bridging could be offered on GNUstep too.
- Ask objc2 to link objc2-core-services and -natural-language (and any
  framework crate without GNUstep support) to their frameworks only on
  Apple targets; needs the maintainer's go-ahead like the fork's commits.
