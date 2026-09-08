//! A small on-device interface for monochrome panels.
//!
//! Screens with a title and an icon, a strip of those icons along the foot
//! of the panel saying where you are, a popup menu of actions over each,
//! scrolling content, and a third level the application can open when one
//! value needs editing. It draws with its own 5 × 7 font onto anything that
//! can set a pixel, and it depends on nothing.
//!
//! It came out of a LoRa modem's firmware, where it was written as pure code
//! so it could be tested on the host and held to golden images by a
//! simulator; this crate is that interface with the modem taken out of it.
//! What the modem had to supply, any application has to supply:
//!
//! * **A [`Canvas`].** Width, height, set a pixel. A framebuffer in the
//!   display controller's own layout is the natural one; a
//!   [`Bitmap`] is here for tests and simulators; and with the
//!   `embedded-graphics` feature any monochrome `DrawTarget` is one too
//!   ([`eg::Display`]), which is most display drivers there are.
//! * **The screens.** A slice of [`Screen`] descriptors -- title, icon, menu
//!   -- each menu item carrying an action of the application's own type. The
//!   navigator hands an action back when the user picks one and never
//!   interprets it.
//! * **The content.** A screen's content is a list of lines, formatted by
//!   the application into its own buffers and handed to [`page`], which
//!   draws the ones that fit from the scroll position with a scrollbar when
//!   there are more.
//! * **Optionally, a [`Modal`].** Something the application opens over a
//!   screen that takes the keys until it is done: an editor, a notice.
//!
//! Everything about *where* things go is a [`Layout`] derived from the
//! canvas size, so the same page lands on a 128 × 128 panel and a 128 × 64
//! one, with more or fewer lines between the same two bars.
//!
//! # An application in thirty lines
//!
//! ```
//! use monopanel::{page, Bitmap, Input, Item, Nav, Screen};
//!
//! #[derive(Copy, Clone, PartialEq, Debug)]
//! enum Act { Beep, Reboot }
//!
//! static SCREENS: [Screen<Act>; 2] = [
//!     Screen {
//!         title: "Home",
//!         menu_title: "Home Action",
//!         icon: [0x78, 0x7c, 0x06, 0x77, 0x06, 0x7c, 0x78],
//!         menu: &[Item::BACK, Item::new("Beep", Act::Beep)],
//!     },
//!     Screen {
//!         title: "System",
//!         menu_title: "System Action",
//!         icon: [0x1c, 0x3e, 0x77, 0x22, 0x77, 0x3e, 0x1c],
//!         menu: &[Item::BACK, Item::new("Reboot", Act::Reboot)],
//!     },
//! ];
//!
//! let mut nav: Nav<Act> = Nav::new(&SCREENS);
//! let mut canvas = Bitmap::<128, 64>::new();
//!
//! // Right to System, select to open its menu, down onto Reboot.
//! nav.handle(Input::Right);
//! nav.handle(Input::Select);
//! nav.handle(Input::Down);
//! page(&mut canvas, &mut nav, "91%", "12:45", &["Uptime  1:02:03"]);
//! assert_eq!(nav.title(), "System");
//!
//! // Select hands the action back, once, with the menu shut.
//! assert_eq!(nav.handle(Input::Select), Some(Act::Reboot));
//! assert!(!nav.menu_is_open());
//! ```
//!
//! # What it is not
//!
//! Not a widget toolkit, not a layout engine, and not a display driver. It
//! draws one shape of interface well on a small panel with a six-way pad,
//! and it does that with no allocator, no `std`, and no dependency.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod canvas;
pub mod draw;
pub mod font;
pub mod layout;
pub mod nav;

#[cfg(feature = "embedded-graphics")]
pub mod eg;

pub use canvas::{Bitmap, Canvas, Readable};
pub use draw::{draw_icon, icon_bar, menu, menu_window, page, scrollbar, title_bar};
pub use layout::Layout;
pub use nav::{Icon, Input, Item, Modal, Nav, NoModal, Outcome, Screen, ICON_CELL, ICON_H, ICON_W};
