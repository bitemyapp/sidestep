# Stories

`sidestep-stories` does for sidestep-ui programs what Storybook does for web
components. A story is one named state of a drawing or of a whole program.
It is rendered without a display in chosen knobs (light or dark, high
contrast, scale, accent) and checked against a golden image.

The aim is a desktop environment, Aizome, that can be tested whole in a
Sidestep window: its apps, its shell and, last, its compositor, each in
the states a test puts them in.

## Writing stories

A project gathers its stories in a `Book` and gives the book a binary of its
own. `examples/stories` is Sidestep's:

```rust
let mut book = Book::new(concat!(env!("CARGO_MANIFEST_DIR"), "/golden"));
// A drawing, on sidestep-ui's offscreen canvas.
book.drawn("colors/system", Size::new(420.0, 236.0), |canvas, _knobs| { /* … */ })
    .variants([Knobs::LIGHT, Knobs::DARK, Knobs::DARK.accent(0xe66100).at(2.0)]);
// A whole program, under the null render thread, with steps a person takes.
book.app("ui-gallery/typed", |_knobs| Gallery::new(None))
    .step(|g, d| d.click(g.field_rect().center()))
    .step(|_, d| d.type_text("Hello, stories"))
    .variants([Knobs::LIGHT, Knobs::DARK.at(2.0)]);
book.run()
```

A step gets the program's handler, to ask it where things are, and a
`Driver`: `click`, `move_to`, `type_text`, `key`, `shortcut`, `wait`. Each
step plays once the program has drawn what the last one changed; the
picture is the first window's, once the last step has shown.

## Running them

```sh
cargo run -p stories -- list
cargo run -p stories -- render ui-gallery/typed dark@2x   # into target/stories/
cargo run -p stories -- check                            # against examples/stories/golden
cargo run -p stories -- check --update                   # write the golden images
```

Knobs are named as golden images are: `light@1x`, `dark@2x`, `dark-hc@1x`,
`light+e66100@1.5x`. A program's window takes whole scales only (the null
render thread's); a drawing takes any.

Every render runs in a child process of the book's binary, with no Wayland
or X display and no session bus: one program per process, as the toolkit
wants, and nothing of the desktop the command runs in. A story's pixels
depend only on its knobs and the fonts. `examples/stories/golden` was drawn
with Inter and Noto; until the fonts are pinned (below), run
`check --update` once on a new machine and look at what changed.

## Still to do

1. **A viewer**: a sidestep-ui program with the stories in a list, the
   chosen one live beside it, and the knobs to change.
2. **Aizome's apps and shell as stories**, from `aizome-kit`'s test harness
   and fixtures of the compositor's state.
3. **The whole desktop as a story**: Aizome's compositor run headless, its
   frames shown in the viewer and input sent back.
4. **Pinned fonts and CI**, so golden images match on every machine.
