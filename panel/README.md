# monopanel

A small on-device interface for monochrome panels: screens with a title and
an icon, a strip of those icons along the foot saying where you are, a popup
menu of actions over each, scrolling content, lists to pick from, and two
levels the application opens over a screen: a modal when one value needs
editing, and a detail for something longer than a screen. It draws with its
own 5 × 7 font onto anything that can set a pixel, and it depends on
nothing.

It came out of [oxinode](https://github.com/paulmeier/oxinode), a LoRa modem
firmware, where it was written as pure code so it could be tested on the host
and held to golden images by a simulator. This crate is that interface with
the modem taken out of it. It lives in oxinode's workspace until a second
device wants it.

## What you supply

* **A `Canvas`.** Width, height, set a pixel. Your framebuffer, in your
  display controller's own layout, is the natural one. `Bitmap` is here for
  tests. With the `embedded-graphics` feature, any monochrome `DrawTarget` is
  a canvas too, through `eg::Display`.
* **The screens.** A `&'static [Screen<A>]`: title, menu title, icon, menu.
  Each menu item carries an action of your own type `A`, which the navigator
  hands back from `handle` when the user picks it and never interprets.
* **The content.** A screen's content is a slice of `&str` lines, formatted
  into your own buffers and handed to `page`, which draws the ones that fit
  from the scroll position with a scrollbar when there are more.
* **Optionally, a `Modal`.** Something you open over a screen that takes
  the keys until it is done -- an editor, a notice. `NoModal` if you have
  none.
* **Optionally, lists and a `Detail`.** A screen with `open` set is a list:
  up and down move a highlight over its lines and select hands `open` back
  for the highlighted one. A `Detail` is something you open over a screen
  that is browsed like one and is longer than one -- a conversation, a log
  -- with its own title, scroll and menu, left and right moving it by its
  parts; `page_of` draws a window of it. `NoDetail` if you have none.

```rust
use monopanel::{page, Bitmap, Input, Item, Nav, Screen};

#[derive(Copy, Clone)]
enum Act { Beep, Reboot }

static SCREENS: [Screen<Act>; 2] = [
    Screen {
        title: "Home",
        menu_title: "Home Action",
        icon: [0x78, 0x7c, 0x06, 0x77, 0x06, 0x7c, 0x78],
        menu: &[Item::BACK, Item::new("Beep", Act::Beep)],
        open: None,
    },
    Screen {
        title: "System",
        menu_title: "System Action",
        icon: [0x1c, 0x3e, 0x77, 0x22, 0x77, 0x3e, 0x1c],
        menu: &[Item::BACK, Item::new("Reboot", Act::Reboot)],
        open: None,
    },
];

let mut nav: Nav<Act> = Nav::new(&SCREENS);
let mut canvas = Bitmap::<128, 64>::new();

// Right to System, select to open its menu, down onto Reboot.
nav.handle(Input::Right);
nav.handle(Input::Select);
nav.handle(Input::Down);
page(&mut canvas, &mut nav, "91%", "12:45", &["Uptime  1:02:03"]);

// Select hands the action back, once, with the menu shut; what a reboot
// is, is the caller's business.
assert_eq!(nav.handle(Input::Select), Some(Act::Reboot));
```

The same example is a doctest on the crate root, so it is compiled and run
by `cargo test`.

Everything about *where* things go is a `Layout` derived from the canvas
size, so the same page lands on a 128 × 128 panel and a 128 × 64 one, with
more or fewer lines between the same two bars, and a menu too tall for the
panel is shown a window at a time around its highlight.

## What it is not

Not a widget toolkit, not a layout engine, and not a display driver. It
draws one shape of interface well on a small panel with a six-way pad, and it
does that with no allocator, no `std`, and no dependency.

## License

The [Reticulum License](../LICENSE), as oxinode is.
