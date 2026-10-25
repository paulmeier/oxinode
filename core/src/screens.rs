//! What each screen is allowed to know, and how it is drawn from that.
//!
//! The navigator can move between the screens (five, or seven with the node)
//! and the pad can drive it; this is what the screens *say*. The division the navigator enforces holds:
//! [`crate::ui`] never decides what is true, and neither does this module. It
//! is handed a [`State`] -- plain values, copied out of the modem loop by the
//! caller -- and formats and draws it. The render path takes state by borrow
//! and reaches into nothing: no protocol, no clock, no pin.
//!
//! That is what makes every screen a host test. The same [`State`] rendered on
//! the board is rendered by the simulator, and a golden image of each screen,
//! empty and populated, is what says the layout is right.
//!
//! # The rule about empty state
//!
//! A screen with no data says it has no data. A field that is not known is an
//! `Option`, and `None` draws as a dash or a sentence, never as a plausible
//! zero: a `Position` reading `0.000000` is a claim to be in the Gulf of
//! Guinea, and a battery reading `0%` with no cell fitted is a claim the board
//! is about to die. The same principle makes the radio refuse a power it
//! cannot produce rather than clamping it -- a value the user cannot tell is
//! fake is worse than an honest gap.
//!
//! # Lines, not layouts
//!
//! Each screen is a list of text lines, and [`monopanel::page`] draws them
//! from the scroll position with a scrollbar when there are more than fit.
//! That keeps every screen scrollable for free and every screen the same
//! shape; a `label value` row is right-aligned by padding, because the font is
//! fixed-width and a column of values is what makes a page of numbers readable
//! at a glance.

use core::fmt::Write;

use monopanel::{font, page, page_of, Canvas, Content, Layout};

use crate::battery;
use crate::ble::NAME_LEN;
use crate::edit::{Editor, Field, Lock, BAND_2G4, BAND_SUB_GHZ, FREQ_POINT};
use crate::gps;
use crate::lr1121::config::{ConfigError, RadioConfig};
use crate::lr1121::pa;
pub use crate::mode::Mode;
use crate::rnode::command::{FW_VERSION_MAJOR, FW_VERSION_MINOR};
use crate::rnode::store::MAX_BONDS;
use crate::status::{Bluetooth as Link, Text};
use crate::ui::{self, Nav, NavExt, Refusal, Screen};

/// Characters that fit on one content line of this board's panel.
///
/// The line buffers are sized for the panel the board has, from
/// [`ui::LAYOUT`]; a canvas of another width shows what fits of the same
/// lines. Height is the only thing a second panel size changes for these
/// screens, and that is the interface crate's to handle.
pub const LINE_CHARS: usize = ui::LAYOUT.line_chars();

/// The most lines any screen produces, and one more. A full Peers screen is
/// the longest, at two lines a peer; the radio screen with a refusal wrapped
/// under it is the next, at under twenty.
pub const MAX_LINES: usize = 2 * PEERS + 1;

/// How many peers the Peers screen lists: every one the node keeps.
pub const PEERS: usize = 16;

/// How many conversations the Messages screen lists: one with each peer the
/// node keeps, at most.
pub const CONVERSATIONS: usize = PEERS;

/// The most messages one conversation shows, both ways together: as many as
/// the node keeps received, and as many again for the ones it sends.
pub const THREAD: usize = 32;

/// Which host has the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Host {
    /// Nobody has the port open and no phone is connected.
    #[default]
    None,
    /// A host has the KISS port open.
    Usb,
    /// A phone is connected; it is the host while it is.
    Bluetooth,
}

/// What the radio is doing, as distinct from what it was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Air {
    /// Configured off, or not yet configured.
    #[default]
    Off,
    /// Configured, and listening.
    Receiving,
    /// The host asked for something this radio cannot do, and it stayed off.
    Refused(ConfigError),
    /// The configuration was valid but the chip would not take it.
    Failed,
    /// The radio never came up. This board is a modem in name only.
    NoRadio,
}

impl Air {
    /// The word on the screen.
    pub const fn word(self) -> &'static str {
        match self {
            Air::Off => "off",
            Air::Receiving => "receiving",
            Air::Refused(_) => "refused",
            Air::Failed => "failed",
            Air::NoRadio => "no radio",
        }
    }
}

impl Host {
    /// Who has the radio, if it is not the panel: the host-ownership rule, in
    /// one place. A live session on either transport owns the live
    /// configuration.
    pub const fn lock(self) -> Option<Lock> {
        match self {
            Host::None => None,
            Host::Usb => Some(Lock::Usb),
            Host::Bluetooth => Some(Lock::Bluetooth),
        }
    }
}

/// What [`Screen::Home`] knows: what the modem is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Home {
    pub host: Host,
    /// Whether the board is a host's RNode or a node of its own, in an image
    /// with the node. `None` is an image without one, which is always an
    /// RNode and draws no row for it.
    pub mode: Option<Mode>,
    /// Messages received and not yet read, in an image with the node. The
    /// row is drawn while there are some: it is what lights when a message
    /// comes, and Home has room for it then by scrolling a line rather than
    /// always. `None` is an image without the node.
    pub unread: Option<u16>,
    /// A frame passed between the modem and its host in the last few seconds.
    /// Meaningless with no host, and not drawn then.
    pub talking: bool,
    pub air: Air,
    pub rx_count: u32,
    pub tx_count: u32,
    /// The last packet's signal, if there has been one.
    pub last_rssi_dbm: Option<i16>,
    /// Quarter-decibels, as the chip and the protocol both carry it.
    pub last_snr_quarter_db: Option<i8>,
    /// Seconds since boot. `None` only where there is no clock, which is the
    /// simulator; a board always knows.
    pub uptime_s: Option<u32>,
    /// The cell, if there is one to read.
    pub battery: Option<battery::Reading>,
    /// What the charger says it is doing.
    pub charger: battery::Charger,
}

/// What [`Screen::Radio`] knows: the configuration, and whether it took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Radio {
    /// The protocol's current configuration -- the same values the host reads
    /// back, which is the point: what the screen says is what `rnsd` sees.
    pub config: RadioConfig,
    pub air: Air,
    /// A stored configuration is in force: TNC mode.
    pub tnc: bool,
}

impl Default for Radio {
    /// The configuration a modem boots with. Not a placeholder: it is what the
    /// radio would be given if the host said "on" without setting anything.
    fn default() -> Self {
        Radio {
            config: crate::lr1121::config::DEFAULT,
            air: Air::Off,
            tnc: false,
        }
    }
}

/// What [`Screen::Bluetooth`] knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BluetoothScreen {
    pub link: Link,
    /// The advertised name, once the stack is up.
    pub name: Option<[u8; NAME_LEN]>,
    /// A pairing in progress: the six digits the phone has to be told.
    pub passkey: Option<u32>,
    /// Phones remembered, out of [`MAX_BONDS`].
    pub bonded: u8,
}

/// What [`Screen::Position`] knows: whether the receiver is on, what it can
/// see, and where it last said it was. Assembled by [`crate::gps::Receiver`]
/// from the sentences the module sends; every value is an `Option`, and the
/// status says why when they are `None`.
pub use crate::gps::Position;

/// What the device's identity amounts to, as far as the device can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Identity {
    /// `rnodeconf --rom` has never been run.
    #[default]
    None,
    /// Locked, but the checksum does not match the identity block.
    BadChecksum,
    /// Locked and self-consistent, but no signature has been stored.
    Unsigned,
    /// Locked, self-consistent, and carrying a signature.
    ///
    /// *Carrying*, not *validated*: the signature is RSA, made with the
    /// host's private key, and the device has no public key to check it
    /// against. Only the host can validate it, and does, on every connect.
    /// What the device can verify is the checksum, and that is what
    /// [`Identity::BadChecksum`] reports.
    Signed,
}

impl Identity {
    /// The word on the screen.
    pub const fn word(self) -> &'static str {
        match self {
            Identity::None => "none",
            Identity::BadChecksum => "bad checksum",
            Identity::Unsigned => "unsigned",
            Identity::Signed => "signed",
        }
    }
}

/// What the board is called on the network, when it is a node of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeHashes {
    /// The identity's own hash.
    pub identity: [u8; 16],
    /// `lxmf.delivery` for this identity: the address a peer sends to, and
    /// what Sideband and MeshChat show as its LXMF address.
    pub lxmf: [u8; 16],
}

/// What [`Screen::System`] knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct System {
    /// oxinode's own version, from its manifest.
    pub version: &'static str,
    /// The commit the image was built from: a short hash, `-dirty` if the
    /// tree had uncommitted changes, `unknown` outside a checkout. The
    /// version says which release a board is near; this says which build
    /// it is running.
    pub build: &'static str,
    /// The device serial, as the host names the port: sixteen hex digits of
    /// the chip's factory ID.
    pub serial: Option<[u8; 16]>,
    pub identity: Identity,
    /// The board's own address and identity, in an image with the node.
    /// `None` is an image without one, and draws no rows at all rather than
    /// dashes: a dash says "not known yet", and an RNode has nothing to know.
    pub node: Option<NodeHashes>,
    /// The least time between two of the board's announces, by the announce
    /// budget at the current configuration, in seconds. Drawn only with a
    /// node; `None` there is a radio with no configuration.
    pub announce_gap_s: Option<u32>,
    /// Bytes between the top of static data and the stack pointer.
    pub free_ram: Option<u32>,
}

impl Default for System {
    fn default() -> Self {
        System {
            version: "-",
            build: "-",
            serial: None,
            identity: Identity::None,
            node: None,
            announce_gap_s: None,
            free_ram: None,
        }
    }
}

/// What stands in for a character the panel cannot draw: a byte the font
/// has no glyph for, so it draws as one hollow box.
pub const UNDRAWABLE: u8 = 0x7f;

/// Text as the panel can draw it, one byte per cell: printable ASCII as it
/// is, the punctuation phones type in place of ASCII folded to what the font
/// draws, and every other character as [`UNDRAWABLE`].
///
/// The fold is for what people send without meaning anything by it: a
/// phone's keyboard turns `'` into `’` and `--` into `–` on its own. So
/// curly single quotes and primes become `'`; dashes and the minus sign
/// become `-`; an ellipsis becomes `...`; and the no-break and thin spaces
/// become a space. Double quotes of every kind, the straight one included,
/// become `'`, because the font has `'` and no `"`: a quotation that reads
/// is better than one in two boxes. Only what is drawn is folded; the text
/// a message carries is kept as it came.
pub fn drawable(text: &str) -> impl Iterator<Item = u8> + '_ {
    text.chars().flat_map(|c| {
        let one = |b: u8| ([b, 0, 0], 1);
        let (bytes, len) = match c {
            '"' | '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' | '\u{2033}' => one(b'\''),
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' | '\u{2032}' => one(b'\''),
            '\u{2010}'..='\u{2015}' | '\u{2212}' => one(b'-'),
            '\u{00a0}' | '\u{2009}' | '\u{202f}' => one(b' '),
            '\u{2026}' => (*b"...", 3),
            ' '..='~' => one(c as u8),
            _ => one(UNDRAWABLE),
        };
        bytes.into_iter().take(len)
    })
}

/// A peer's display name as the panel can draw it ([`drawable`]), cut at a
/// row's width; a name is never wider than the row it sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerName {
    bytes: [u8; LINE_CHARS],
    len: u8,
}

impl PeerName {
    /// What stands in for a character the panel cannot draw.
    pub const UNDRAWABLE: u8 = UNDRAWABLE;

    pub fn new(name: &str) -> Self {
        let mut out = PeerName {
            bytes: [0; LINE_CHARS],
            len: 0,
        };
        for (slot, b) in out.bytes.iter_mut().zip(drawable(name)) {
            *slot = b;
            out.len += 1;
        }
        out
    }

    pub fn as_str(&self) -> &str {
        // ASCII only, by construction.
        core::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("?")
    }
}

/// One line of [`Screen::Peers`](crate::ui::Screen): a peer, as the node last
/// heard it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerRow {
    /// Its LXMF address, which stands in for a name it did not give.
    pub destination: [u8; 16],
    pub name: Option<PeerName>,
    /// Seconds since it was last heard.
    pub age_s: Option<u32>,
    /// Hops its last announce came, one being straight off the air.
    pub hops: Option<u8>,
    /// The signal of its last announce, if that came straight off the air.
    pub signal: Option<crate::rnode::air::Signal>,
}

/// What the Peers screen knows: the peers the node keeps, the one heard most
/// recently first. Only an image with the node has the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Peers {
    pub rows: [Option<PeerRow>; PEERS],
}

/// One line of the Messages screen: a peer the board has a conversation
/// with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationRow {
    /// Its LXMF address: what the conversation is opened by, and what
    /// stands in for a name it did not give.
    pub peer: [u8; 16],
    pub name: Option<PeerName>,
    /// Something from it has not been read.
    pub unread: bool,
    /// Seconds since the last message, either way.
    pub age_s: Option<u32>,
}

/// What the Messages screen knows: the conversations, the one with the
/// newest message first. Only an image with the node has the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Conversations {
    pub rows: [Option<ConversationRow>; CONVERSATIONS],
}

/// What the board believes about who sent a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// Signed by the key the sender announced.
    Verified,
    /// The sender's key has not been heard; it has been asked for.
    Unverified,
    /// The sender's key was heard after the message came, and did not sign
    /// it. Marked so for good.
    Failed,
}

/// Where a message the board sends has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Waiting to go, and what for.
    Queued(Wait),
    /// On the air this many times, and not yet proved.
    Sent(u8),
    /// The recipient's proof came back.
    Delivered,
    /// It was not delivered, and why.
    Failed(Failure),
}

/// What a queued message waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Its turn on the channel: nothing else.
    Turn,
    /// The recipient's key, which its path brings; it has been asked for.
    Path,
    /// A host has the radio, and the board sends nothing of its own under
    /// one.
    Host,
    /// No host, and no configuration stored for the board to be a node on.
    Idle,
}

/// Why a message the board sent was not delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// No answer to asking for the recipient's path, so no key to encrypt to.
    NoPath,
    /// Sent every time it may be, and no proof came back.
    NoProof,
}

impl Delivery {
    /// Every state, with every reason: what the golden images cover.
    pub const EVERY: [Delivery; 10] = [
        Delivery::Queued(Wait::Turn),
        Delivery::Queued(Wait::Path),
        Delivery::Queued(Wait::Host),
        Delivery::Queued(Wait::Idle),
        Delivery::Sent(1),
        Delivery::Sent(2),
        Delivery::Sent(3),
        Delivery::Delivered,
        Delivery::Failed(Failure::NoPath),
        Delivery::Failed(Failure::NoProof),
    ];

    /// The word on the screen.
    pub const fn word(self) -> &'static str {
        match self {
            Delivery::Queued(_) => "queued",
            Delivery::Sent(_) => "sent",
            Delivery::Delivered => "delivered",
            Delivery::Failed(_) => "failed",
        }
    }

    /// What the thread says under the header, when the word is not all of
    /// it: what a queued one waits for, how often one has gone, and why one
    /// failed.
    pub const fn note(self) -> Option<&'static str> {
        match self {
            Delivery::Queued(Wait::Turn) | Delivery::Sent(0 | 1) | Delivery::Delivered => None,
            Delivery::Queued(Wait::Path) => Some("(finding a path)"),
            Delivery::Queued(Wait::Host) => Some("(held for the host)"),
            Delivery::Queued(Wait::Idle) => Some("(no radio config)"),
            Delivery::Sent(2) => Some("(sent twice)"),
            Delivery::Sent(_) => Some("(sent again)"),
            Delivery::Failed(Failure::NoPath) => Some("(no path found)"),
            Delivery::Failed(Failure::NoProof) => Some("(no proof came back)"),
        }
    }
}

/// Which way a message went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// From the peer, with what the board believes about that.
    Received(Trust),
    /// From the board, and where it has got to.
    Sent(Delivery),
}

/// One message of a conversation, as the thread draws it: borrowed from
/// wherever the messages are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub direction: Direction,
    /// Seconds since it arrived, or since it was written.
    pub age_s: Option<u32>,
    /// LXMF's title, which phones send empty.
    pub title: &'a str,
    pub content: &'a str,
    /// It carried something the board does not show.
    pub attachment: bool,
    /// Received and not yet read.
    pub unread: bool,
}

/// One conversation: the peer, and its messages oldest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thread<'a> {
    pub peer: [u8; 16],
    pub name: Option<PeerName>,
    pub entries: &'a [Entry<'a>],
}

/// How far into a conversation the panel has shown: the first `through` of
/// its entries have had their last line in view. What the caller marks
/// read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Read {
    pub peer: [u8; 16],
    pub through: usize,
}

/// Everything the screens know between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    /// The four hex digits that name the board: the suffix of the name it
    /// advertises, which is how a phone lists it. Known from the first
    /// moment, which is why it is not on the Bluetooth screen's `name`.
    pub name: Option<[u8; 4]>,
    pub home: Home,
    pub radio: Radio,
    pub bluetooth: BluetoothScreen,
    pub position: Position,
    pub system: System,
    pub peers: Peers,
    pub conversations: Conversations,
}

/// The free RAM figure: what lies between the end of static data and the
/// current stack pointer.
///
/// That is the only honest number on a board with no allocator. The stack
/// grows down from the top of RAM and everything else is laid out from the
/// bottom, so the gap is what is left for the stack to grow into -- and a gap
/// that has closed is `None` rather than a wrapped number.
pub const fn free_ram(heap_start: u32, stack_pointer: u32) -> Option<u32> {
    stack_pointer.checked_sub(heap_start)
}

/// A screen's content, one formatted line at a time.
///
/// Fixed buffers, because the board has no allocator and this is built on
/// its stack once per redraw.
pub struct Lines {
    rows: [Text<LINE_CHARS>; MAX_LINES],
    len: usize,
    /// A line was handed in wider than the row and lost its tail. `Text`
    /// truncates silently, which is right on the board -- a line one
    /// character too long should lose that character, not vanish -- and
    /// wrong in a test, where the loss is the bug. So it is recorded.
    overflowed: bool,
}

impl Default for Lines {
    fn default() -> Self {
        Self::new()
    }
}

impl Lines {
    pub fn new() -> Self {
        Lines {
            rows: core::array::from_fn(|_| Text::new()),
            len: 0,
            overflowed: false,
        }
    }

    /// Whether any line handed in was too wide for the row and was cut.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// How many lines there are.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The lines, for [`monopanel::page`].
    pub fn as_strs(&self) -> [&str; MAX_LINES] {
        core::array::from_fn(|i| self.rows[i].as_str())
    }

    /// The line at `n`.
    pub fn get(&self, n: usize) -> Option<&str> {
        (n < self.len).then(|| self.rows[n].as_str())
    }

    /// Add one line as given. Past [`MAX_LINES`] it is dropped, which is a
    /// bug in the screen that produced it and is asserted against below.
    pub fn line(&mut self, text: &str) {
        if self.len == MAX_LINES {
            return;
        }
        if text.len() > LINE_CHARS {
            self.overflowed = true;
        }
        self.rows[self.len].clear();
        let _ = self.rows[self.len].write_str(text);
        self.len += 1;
    }

    /// Add a `label value` row with the value flush right.
    pub fn row(&mut self, label: &str, value: &str) {
        if self.len == MAX_LINES {
            return;
        }
        let row = &mut self.rows[self.len];
        row.clear();
        let width = LINE_CHARS.saturating_sub(label.len());
        // A value wider than the room it has pushes the label off rather than
        // being cut: `{value:>width$}` never truncates, and the row then
        // loses its tail to `Text`.
        if value.len() > width {
            self.overflowed = true;
        }
        let _ = write!(row, "{label}{value:>width$}");
        self.len += 1;
    }

    /// Add a sentence, wrapped at word boundaries to the line width.
    pub fn wrapped(&mut self, text: &str) {
        for line in wrap(text, LINE_CHARS) {
            self.line(line);
        }
    }
}

/// Break `text` into pieces no longer than `width`, at spaces where it can and
/// mid-word where it must.
pub fn wrap(text: &str, width: usize) -> impl Iterator<Item = &str> {
    let mut rest = text.trim();
    core::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        if rest.len() <= width {
            let line = rest;
            rest = "";
            return Some(line);
        }
        // The last space inside the width, or a hard break if there is none.
        let cut = rest[..=width].rfind(' ').unwrap_or(width);
        let (line, tail) = rest.split_at(cut);
        rest = tail.trim_start();
        Some(line.trim_end())
    })
}

/// How many cells of the panel a character takes once [`drawable`] has
/// folded it: three for an ellipsis, one for everything else.
fn cells(c: char) -> usize {
    if c == '\u{2026}' {
        3
    } else {
        1
    }
}

/// Break what somebody wrote into lines no wider than `width` cells as the
/// panel draws them ([`drawable`]): at every newline, at a space where it
/// can, and mid-word where it must. A blank line between two newlines is
/// kept as a blank line; spaces at the ends of a line are not, because a
/// leading space would read as the indent a sent message has.
///
/// [`wrap`] is for the screens' own sentences, which are ASCII; this is for
/// text from anywhere, so it measures in characters and never cuts one.
pub fn wrap_drawn(text: &str, width: usize) -> impl Iterator<Item = &str> {
    text.trim()
        .split('\n')
        .flat_map(move |paragraph| Paragraph {
            rest: paragraph.trim_end_matches('\r').trim_matches(' '),
            width: width.max(3),
            started: false,
        })
}

/// One paragraph of [`wrap_drawn`], a line at a time.
struct Paragraph<'a> {
    rest: &'a str,
    width: usize,
    started: bool,
}

impl<'a> Iterator for Paragraph<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.rest.is_empty() {
            // An empty paragraph is a blank line, once.
            let first = !self.started;
            self.started = true;
            return first.then_some("");
        }
        self.started = true;
        let mut used = 0;
        let mut space = None;
        let mut cut = None;
        for (i, c) in self.rest.char_indices() {
            if used + cells(c) > self.width {
                cut = Some(i);
                break;
            }
            if c == ' ' {
                space = Some(i);
            }
            used += cells(c);
        }
        let Some(cut) = cut else {
            return Some(core::mem::take(&mut self.rest));
        };
        // A space just past the width is as good a place as one inside it.
        let at = if self.rest[cut..].starts_with(' ') {
            cut
        } else {
            space.unwrap_or(cut)
        };
        // Never nothing: a character wider than the line still moves on.
        let at = if at == 0 {
            self.rest.chars().next().map_or(1, char::len_utf8)
        } else {
            at
        };
        let (line, tail) = self.rest.split_at(at);
        self.rest = tail.trim_start_matches(' ');
        Some(line.trim_end_matches(' '))
    }
}

/// A frequency to the kilohertz: `915.000 MHz`, `2478.000 MHz`.
fn megahertz(out: &mut Text<LINE_CHARS>, hz: u32) {
    let _ = write!(
        out,
        "{}.{:03} MHz",
        hz / 1_000_000,
        (hz % 1_000_000) / 1_000
    );
}

/// A bandwidth in kilohertz, to as many places as it needs and no more:
/// `125.0 kHz`, `62.5 kHz`, `31.25 kHz`, `203.125 kHz`.
///
/// The 2.4 GHz bandwidths are 1625 kHz over a power of two, and a screen
/// that rounded 203.125 to 203.1 would name a bandwidth the chip does not
/// have -- the same mistake the docs warn a host against. One place stays
/// the floor, because `125.0` is what every picture of the screen has shown.
fn kilohertz(out: &mut Text<LINE_CHARS>, hz: u32) {
    let whole = hz / 1_000;
    let frac = hz % 1_000;
    let _ = if frac % 100 == 0 {
        write!(out, "{whole}.{} kHz", frac / 100)
    } else if frac % 10 == 0 {
        write!(out, "{whole}.{:02} kHz", frac / 10)
    } else {
        write!(out, "{whole}.{frac:03} kHz")
    };
}

/// Seconds since boot as something readable.
///
/// `h:mm:ss` under a day, `Nd hh:mm` from then on: at a day the seconds stop
/// meaning anything and the width has to stay inside the row.
pub fn uptime(out: &mut Text<LINE_CHARS>, secs: u32) {
    let (d, h, m, s) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        let _ = write!(out, "{d}d {h:02}:{m:02}");
    } else {
        let _ = write!(out, "{h}:{m:02}:{s:02}");
    }
}

/// Micro-degrees to six places, signed: `47.376887`, `-6.505620`.
fn degrees(out: &mut Text<LINE_CHARS>, udeg: i32) {
    let sign = if udeg < 0 { "-" } else { "" };
    let abs = udeg.unsigned_abs();
    let _ = write!(out, "{sign}{}.{:06}", abs / 1_000_000, abs % 1_000_000);
}

/// Seconds since the fix as something readable: seconds under a minute,
/// minutes under an hour, hours after that. Past a day the number is still
/// hours, because a fix that old is a fix that is gone and the width of the
/// row matters more than the unit.
pub fn age(out: &mut Text<LINE_CHARS>, secs: u32) {
    if secs < 60 {
        let _ = write!(out, "{secs} s");
    } else if secs < 3_600 {
        let _ = write!(out, "{} min", secs / 60);
    } else {
        let _ = write!(out, "{} h", secs / 3_600);
    }
}

/// Quarter-decibels to one decimal without floating point, keeping the sign
/// on a value between zero and minus one -- see the status page's test for
/// the case that loses it.
fn snr(out: &mut Text<LINE_CHARS>, quarter_db: i8) {
    let whole = quarter_db as i32 / 4;
    let tenths = ((quarter_db as i32 % 4).abs() * 25) / 10;
    if quarter_db < 0 && whole == 0 {
        let _ = write!(out, "-0.{tenths} dB");
    } else {
        let _ = write!(out, "{whole}.{tenths} dB");
    }
}

impl State {
    /// The lines one screen shows.
    pub fn lines(&self, screen: Screen, out: &mut Lines) {
        match screen {
            Screen::Home => self.home.lines(out),
            #[cfg(feature = "node")]
            Screen::Messages => {
                self.conversations.lines(out);
            }
            #[cfg(feature = "node")]
            Screen::Peers => self.peers.lines(out),
            Screen::Radio => self.radio.lines(out),
            Screen::Bluetooth => self.bluetooth.lines(out),
            Screen::Position => self.position.lines(out),
            Screen::System => self.system.lines(out),
        }
    }
}

impl Home {
    pub fn lines(&self, out: &mut Lines) {
        let mut v = Text::<LINE_CHARS>::new();
        out.row(
            "Host",
            match self.host {
                Host::None => "none",
                Host::Usb => "USB",
                Host::Bluetooth => "Bluetooth",
            },
        );
        if let Some(mode) = self.mode {
            out.row("Mode", mode.word());
        }
        if let Some(unread) = self.unread.filter(|&n| n > 0) {
            let _ = write!(v, "{unread}");
            out.row("Unread", v.as_str());
            v.clear();
        }
        out.row(
            "Link",
            match (self.host, self.talking) {
                (Host::None, _) => "-",
                (_, true) => "talking",
                (_, false) => "quiet",
            },
        );
        out.row("Radio", self.air.word());
        let _ = write!(v, "{}", self.rx_count);
        out.row("RX", v.as_str());
        v.clear();
        let _ = write!(v, "{}", self.tx_count);
        out.row("TX", v.as_str());
        v.clear();
        match self.last_rssi_dbm {
            Some(dbm) => {
                let _ = write!(v, "{dbm} dBm");
            }
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("RSSI", v.as_str());
        v.clear();
        match self.last_snr_quarter_db {
            Some(q) => snr(&mut v, q),
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("SNR", v.as_str());
        v.clear();
        match self.uptime_s {
            Some(secs) => uptime(&mut v, secs),
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("Uptime", v.as_str());
        v.clear();
        match self.battery {
            Some(cell) => {
                let _ = write!(
                    v,
                    "{}.{:02} V {}%",
                    cell.millivolts / 1000,
                    (cell.millivolts % 1000) / 10,
                    cell.percent
                );
            }
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("Battery", v.as_str());
        out.row("Charger", self.charger.word());
    }
}

impl Peers {
    /// Two lines a peer: its name with how long ago it was heard, then how
    /// it was heard -- the signal if it came straight off the air, the hops
    /// if it was relayed.
    pub fn lines(&self, out: &mut Lines) {
        let mut any = false;
        for row in self.rows.iter().flatten() {
            any = true;
            row.lines(out);
        }
        if !any {
            out.wrapped("No peers heard yet.");
        }
    }
}

impl PeerRow {
    fn lines(&self, out: &mut Lines) {
        let mut when = Text::<LINE_CHARS>::new();
        match self.age_s {
            Some(s) => age(&mut when, s),
            None => {
                let _ = when.write_str("-");
            }
        }
        // The name has what the age leaves, and a space between them.
        let room = LINE_CHARS - when.as_str().len() - 1;
        let name = label(self.name.as_ref(), &self.destination);
        // ASCII either way, so any byte is a character boundary.
        let name = name.as_str();
        out.row(&name[..name.len().min(room)], when.as_str());
        let mut how = Text::<LINE_CHARS>::new();
        let _ = how.write_str("  ");
        match (self.signal, self.hops) {
            (Some(s), _) => {
                let _ = write!(how, "{} dBm ", s.rssi_dbm);
                snr(&mut how, s.snr_quarter_db);
            }
            (None, Some(1)) => {
                let _ = how.write_str("1 hop");
            }
            (None, Some(h)) => {
                let _ = write!(how, "{h} hops");
            }
            (None, None) => {
                let _ = how.write_str("-");
            }
        }
        out.line(how.as_str());
    }
}

/// What a peer is called on the panel: its name, or with none the start
/// of its address, marked as not a name.
fn label(name: Option<&PeerName>, destination: &[u8; 16]) -> Text<LINE_CHARS> {
    let mut out = Text::<LINE_CHARS>::new();
    match name {
        Some(n) => {
            let _ = out.write_str(n.as_str());
        }
        None => {
            let _ = out.write_str("<");
            for b in &destination[..5] {
                let _ = write!(out, "{b:02x}");
            }
            let _ = out.write_str(">");
        }
    }
    out
}

/// An age as a value: [`age`], or a dash.
fn age_or_dash(secs: Option<u32>) -> Text<LINE_CHARS> {
    let mut out = Text::<LINE_CHARS>::new();
    match secs {
        Some(s) => age(&mut out, s),
        None => {
            let _ = out.write_str("-");
        }
    }
    out
}

impl Conversations {
    /// One line a conversation: a mark if something in it is unread, the
    /// peer's name, and how long ago the last message was. Returns how many
    /// there are, which is how many of the lines are entries to open; with
    /// none, a sentence that says so.
    pub fn lines(&self, out: &mut Lines) -> usize {
        let mut entries = 0;
        for row in self.rows.iter().flatten() {
            entries += 1;
            let when = age_or_dash(row.age_s);
            // The mark, the name in what the age leaves, a space, the age.
            let room = LINE_CHARS - 1 - when.as_str().len() - 1;
            let name = label(row.name.as_ref(), &row.peer);
            let name = name.as_str();
            let mut left = Text::<LINE_CHARS>::new();
            let _ = left.write_str(if row.unread { "*" } else { " " });
            let _ = left.write_str(&name[..name.len().min(room)]);
            out.row(left.as_str(), when.as_str());
        }
        if entries == 0 {
            out.wrapped("No messages yet.");
        }
        entries
    }
}

/// How far a sent message's lines are set in from a received one's: the
/// two are told apart by where they stand, not by colour.
const SENT_INDENT: usize = 2;

/// A line of a thread before it is drawn, a little wider than a line so
/// that one too wide is seen to be rather than cut.
type ThreadLine = Text<{ LINE_CHARS + 8 }>;

impl Entry<'_> {
    /// Its first line: who it is from, and how long ago; for one the board
    /// sent, where it has got to. The name gives way to the rest.
    fn header(&self, thread: &Thread<'_>, out: &mut ThreadLine) {
        out.clear();
        let when = age_or_dash(self.age_s);
        let mut right = Text::<LINE_CHARS>::new();
        let who = match self.direction {
            Direction::Received(_) => label(thread.name.as_ref(), &thread.peer),
            Direction::Sent(delivery) => {
                let _ = write!(right, "{} ", delivery.word());
                let mut you = Text::<LINE_CHARS>::new();
                let _ = you.write_str("You");
                you
            }
        };
        let _ = right.write_str(when.as_str());
        let room = LINE_CHARS.saturating_sub(right.as_str().len() + 1);
        let who = who.as_str();
        let who = &who[..who.len().min(room)];
        let width = LINE_CHARS - who.len();
        let _ = write!(out, "{who}{:>width$}", right.as_str());
    }

    /// What the board says under the header, if anything: about the sender,
    /// who is trusted only once its signature has been checked; or, for one
    /// the board sent, what its state does not say in a word.
    fn note(&self) -> Option<&'static str> {
        match self.direction {
            Direction::Received(Trust::Unverified) => Some("(unverified sender)"),
            Direction::Received(Trust::Failed) => Some("(bad signature)"),
            Direction::Received(Trust::Verified) => None,
            Direction::Sent(delivery) => delivery.note(),
        }
    }

    fn indent(&self) -> usize {
        match self.direction {
            Direction::Received(_) => 0,
            Direction::Sent(_) => SENT_INDENT,
        }
    }

    /// Its body, a line at a time, folded for the panel: the title if it has
    /// one, the content, and a word for what is not shown.
    fn body(&self, out: &mut ThreadLine, mut f: impl FnMut(&str)) {
        let indent = self.indent();
        let width = LINE_CHARS - indent;
        let empty = self.title.trim().is_empty() && self.content.trim().is_empty();
        let mut emit = |text: &str, fold: bool| {
            out.clear();
            for _ in 0..indent {
                let _ = out.write_char(' ');
            }
            if fold {
                for b in drawable(text) {
                    let _ = out.write_char(char::from(b));
                }
            } else {
                let _ = out.write_str(text);
            }
            f(out.as_str());
        };
        for text in [self.title, self.content] {
            if !text.trim().is_empty() {
                for line in wrap_drawn(text, width) {
                    emit(line, true);
                }
            }
        }
        if self.attachment {
            emit("(attachment)", false);
        }
        if empty && !self.attachment {
            emit("(empty)", false);
        }
    }
}

/// One line of a thread, as [`Thread::each`] hands it out.
struct Line<'l> {
    /// The entry it belongs to; a blank line belongs to the entry above it.
    entry: usize,
    /// It is an entry's header, where left and right stop.
    header: bool,
    /// It is the last line of its entry's text.
    last: bool,
    text: &'l str,
}

impl Thread<'_> {
    /// Every line of the thread, in order: per message, the header, a note
    /// about the sender if there is one, the body, and a blank line before
    /// the next. The one place the thread is laid out, so its length, its
    /// stops and its window cannot disagree. Called through `dyn`, so the
    /// layout is in the image once rather than once for each caller.
    fn each(&self, f: &mut dyn FnMut(Line<'_>)) {
        if self.entries.is_empty() {
            f(Line {
                entry: 0,
                header: false,
                last: false,
                text: "No messages.",
            });
            return;
        }
        let mut out = ThreadLine::new();
        let mut pending = ThreadLine::new();
        for (n, entry) in self.entries.iter().enumerate() {
            if n > 0 {
                f(Line {
                    entry: n - 1,
                    header: false,
                    last: false,
                    text: "",
                });
            }
            entry.header(self, &mut out);
            f(Line {
                entry: n,
                header: true,
                last: false,
                text: out.as_str(),
            });
            if let Some(note) = entry.note() {
                f(Line {
                    entry: n,
                    header: false,
                    last: false,
                    text: note,
                });
            }
            // One line behind, so the last one can be said to be.
            let mut held = false;
            entry.body(&mut out, |line| {
                if held {
                    f(Line {
                        entry: n,
                        header: false,
                        last: false,
                        text: pending.as_str(),
                    });
                }
                pending.clear();
                let _ = pending.write_str(line);
                held = true;
            });
            f(Line {
                entry: n,
                header: false,
                last: true,
                text: pending.as_str(),
            });
        }
    }

    /// How many lines it has.
    pub fn len(&self) -> usize {
        let mut n = 0;
        self.each(&mut |_| n += 1);
        n
    }

    /// Whether it has no messages.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Where each message starts, as a line, into `out`; how many there were.
    pub fn stops(&self, out: &mut [u16]) -> usize {
        let (mut line, mut n) = (0, 0);
        self.each(&mut |l| {
            if l.header && n < out.len() {
                out[n] = line;
                n += 1;
            }
            line = line.saturating_add(1);
        });
        n
    }

    /// The line the first unread message starts on, if one is unread.
    pub fn first_unread(&self) -> Option<usize> {
        let first = self.entries.iter().position(|e| e.unread)?;
        let (mut line, mut found) = (0, None);
        self.each(&mut |l| {
            if l.header && l.entry == first && found.is_none() {
                found = Some(line);
            }
            line += 1;
        });
        found
    }

    /// The lines from `first`, as many as `rows`, into `out`.
    pub fn window(&self, first: usize, rows: usize, out: &mut Lines) {
        let mut line = 0;
        self.each(&mut |l| {
            if (first..first + rows).contains(&line) {
                out.line(l.text);
            }
            line += 1;
        });
    }

    /// How many messages, from the first, have had their last line shown by
    /// a view whose top is `first` and which shows `rows`. Everything above
    /// the view's foot has been passed, which counts as shown.
    pub fn read_through(&self, first: usize, rows: usize) -> usize {
        let foot = first + rows;
        let (mut line, mut through) = (0, 0);
        self.each(&mut |l| {
            if l.last && line < foot {
                through = l.entry + 1;
            }
            line += 1;
        });
        through
    }
}

impl Radio {
    pub fn lines(&self, out: &mut Lines) {
        let c = &self.config;
        let mut v = Text::<LINE_CHARS>::new();
        // The band first, because a configuration has one before it has
        // anything else: it says which connector the antenna belongs on, and
        // which set the bandwidth and the power are judged against. A dash
        // for a frequency in neither, which is a configuration a host can
        // leave and the radio refuses.
        out.row(
            "Band",
            match c.band() {
                Some(band) => band.name(),
                None => "-",
            },
        );
        megahertz(&mut v, c.frequency_hz);
        out.row("Freq", v.as_str());
        // What the chip is actually told, which differs by the reference
        // correction. Shown always rather than only when it differs, so the
        // line does not come and go with a configuration flag.
        v.clear();
        megahertz(&mut v, c.commanded_frequency_hz());
        out.row("Tuned", v.as_str());
        v.clear();
        kilohertz(&mut v, c.bandwidth_hz);
        out.row("BW", v.as_str());
        v.clear();
        let _ = write!(v, "{}", c.spreading_factor);
        out.row("SF", v.as_str());
        v.clear();
        let _ = write!(v, "4/{}", c.coding_rate);
        out.row("CR", v.as_str());
        v.clear();
        let _ = write!(v, "{} dBm", c.tx_power_dbm);
        out.row("Power", v.as_str());
        v.clear();
        match c.bitrate_bps() {
            0 => {
                let _ = v.write_str("-");
            }
            bps => {
                let _ = write!(v, "{bps} bps");
            }
        }
        out.row("Rate", v.as_str());
        out.row("Radio", self.air.word());
        if let Air::Refused(reason) = self.air {
            out.wrapped(reason.message());
        }
        out.row("Mode", if self.tnc { "TNC" } else { "host" });
        v.clear();
        let _ = write!(v, "{} sym", c.preamble_symbols);
        out.row("Preamble", v.as_str());
        v.clear();
        let _ = write!(v, "0x{:02x}", c.sync_word);
        out.row("Sync", v.as_str());
        out.row("CRC", if c.crc { "on" } else { "off" });
        out.row(
            "Header",
            if c.implicit_header {
                "implicit"
            } else {
                "explicit"
            },
        );
        out.row("IQ", if c.invert_iq { "inverted" } else { "normal" });
    }
}

impl BluetoothScreen {
    pub fn lines(&self, out: &mut Lines) {
        out.row(
            "State",
            match self.link {
                Link::Absent => "absent",
                Link::Advertising => "advertising",
                Link::Connected => "connected",
            },
        );
        match &self.name {
            Some(name) => out.row("Name", core::str::from_utf8(name).unwrap_or("?")),
            None => out.row("Name", "-"),
        }
        let mut v = Text::<LINE_CHARS>::new();
        match self.passkey {
            Some(key) => {
                let _ = write!(v, "{:06}", key % 1_000_000);
            }
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("Passkey", v.as_str());
        v.clear();
        let _ = write!(v, "{} of {}", self.bonded, MAX_BONDS);
        out.row("Bonded", v.as_str());
    }
}

impl Position {
    /// The receiver's state first, because whether it is on is the thing
    /// the power question needs to be obvious; then what it can see; then
    /// the fix, with its age, kept after it is lost.
    pub fn lines(&self, out: &mut Lines) {
        let mut v = Text::<LINE_CHARS>::new();
        out.row("GPS", self.status.word());
        match (self.sats_used, self.sats_in_view) {
            (None, None) => {
                let _ = v.write_str("-");
            }
            (Some(used), None) => {
                let _ = write!(v, "{used}");
            }
            (None, Some(seen)) => {
                let _ = write!(v, "-/{seen}");
            }
            (Some(used), Some(seen)) => {
                let _ = write!(v, "{used}/{seen}");
            }
        }
        out.row("Sats", v.as_str());
        v.clear();
        match self.fix_age_s {
            Some(secs) => age(&mut v, secs),
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("Fix age", v.as_str());
        v.clear();
        match self.time {
            Some(t) => {
                let _ = write!(v, "{:02}:{:02}:{:02}", t.hour, t.minute, t.second);
            }
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("UTC", v.as_str());
        v.clear();
        match self.date {
            Some(d) => {
                let _ = write!(v, "{:04}-{:02}-{:02}", d.year, d.month, d.day);
            }
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("Date", v.as_str());
        v.clear();
        match self.fix {
            Some(fix) => degrees(&mut v, fix.lat_udeg),
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("Lat", v.as_str());
        v.clear();
        match self.fix {
            Some(fix) => degrees(&mut v, fix.lon_udeg),
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("Lon", v.as_str());
        v.clear();
        match self.fix.and_then(|f| f.alt_dm) {
            Some(dm) => {
                let _ = write!(
                    v,
                    "{}{}.{} m",
                    if dm < 0 { "-" } else { "" },
                    dm.abs() / 10,
                    dm.abs() % 10
                );
            }
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("Alt", v.as_str());
        if self.status == gps::Status::Off {
            out.line("");
            out.wrapped("The switch, or the menu, turns it on.");
        }
    }
}

impl System {
    pub fn lines(&self, out: &mut Lines) {
        out.row("oxinode", self.version);
        out.row("build", self.build);
        let mut v = Text::<LINE_CHARS>::new();
        let _ = write!(v, "{FW_VERSION_MAJOR}.{FW_VERSION_MINOR}");
        out.row("RNode", v.as_str());
        match &self.serial {
            // Sixteen digits and a label do not share a row.
            Some(serial) => {
                out.line("Serial");
                out.line(core::str::from_utf8(serial).unwrap_or("?"));
            }
            None => out.row("Serial", "-"),
        }
        out.row("Identity", self.identity.word());
        if let Some(node) = &self.node {
            // Thirty-two hex digits each, as Reticulum writes a hash, two
            // rows of sixteen under the label, as the serial is.
            hash(out, "LXMF address", &node.lxmf);
            hash(out, "RNS identity", &node.identity);
            let mut gap = Text::<LINE_CHARS>::new();
            match self.announce_gap_s {
                Some(s) => {
                    let _ = gap.write_str("1 per ");
                    duration(&mut gap, s);
                }
                None => {
                    let _ = gap.write_str("-");
                }
            }
            out.row("Announce", gap.as_str());
        }
        v.clear();
        match self.free_ram {
            Some(bytes) => {
                let _ = write!(v, "{}.{} KB", bytes / 1024, (bytes % 1024) * 10 / 1024);
            }
            None => {
                let _ = v.write_str("-");
            }
        }
        out.row("RAM free", v.as_str());
    }
}

/// A span in the fewest words that say it: `45 s`, `4m12s`, `3h05m`.
fn duration(out: &mut Text<LINE_CHARS>, s: u32) {
    let _ = if s < 60 {
        write!(out, "{s} s")
    } else if s < 3_600 {
        write!(out, "{}m{:02}s", s / 60, s % 60)
    } else {
        write!(out, "{}h{:02}m", s / 3_600, s % 3_600 / 60)
    };
}

/// A 16-byte hash under its label: two rows of sixteen lowercase hex digits,
/// so it can be read against a phone that shows it on one line.
fn hash(out: &mut Lines, label: &str, bytes: &[u8; 16]) {
    out.line(label);
    for half in bytes.chunks(8) {
        let mut v = Text::<LINE_CHARS>::new();
        for b in half {
            let _ = write!(v, "{b:02x}");
        }
        out.line(v.as_str());
    }
}

/// One field's value in the words the Radio screen uses for it, so the
/// editor and the screen never disagree about how a number reads.
///
/// The frequency is the one exception, and only in width: the editor's
/// digits are fixed columns with a cursor under one of them, so the
/// megahertz are padded to [`FREQ_POINT`] places and a sub-GHz frequency
/// shows a blank where its thousands digit would be. The words are the
/// screen's; ` 915.000 MHz` reads as `915.000 MHz` does.
fn field_value(out: &mut Text<LINE_CHARS>, field: Field, value: i64) {
    match field {
        // The screen's word for the band and the connector it puts the
        // antenna on, because moving the band is moving the antenna.
        Field::Band => {
            let band = match value {
                BAND_SUB_GHZ => Some(pa::Band::SubGhz),
                BAND_2G4 => Some(pa::Band::HighFrequency),
                _ => None,
            };
            let _ = match band {
                Some(band) => write!(out, "{}, {}", band.name(), band.connector()),
                None => out.write_str("-"),
            };
        }
        Field::Frequency => {
            // The candidate can exceed what a `u32` holds -- see
            // `Field::setting` -- and the digits shown are the digits typed.
            let khz = value / 1_000;
            let _ = write!(
                out,
                "{:>width$}.{:03} MHz",
                khz / 1_000,
                khz % 1_000,
                width = FREQ_POINT
            );
        }
        Field::Bandwidth => kilohertz(out, value as u32),
        Field::SpreadingFactor => {
            let _ = write!(out, "{value}");
        }
        Field::CodingRate => {
            let _ = write!(out, "4/{value}");
        }
        Field::TxPower => {
            let _ = write!(out, "{value} dBm");
        }
    }
}

/// Where the editor's value starts on its line, in characters: after the
/// label and its gap. The cursor line is built to the same column.
const EDIT_VALUE_COL: usize = 6;

impl Editor {
    /// The editor as lines: what it was, what it is now, how to work it,
    /// and why the last confirmation was refused, if it was.
    ///
    /// Left-aligned rather than in `label value` rows, because the frequency
    /// editor puts a cursor under one digit and the cursor line has to land
    /// on the same column as the digit whatever the value's width.
    ///
    /// Every line: what the board's own panel shows. See [`Editor::lines_in`]
    /// for a panel with fewer rows.
    pub fn lines(&self, out: &mut Lines) {
        self.lines_in(out, MAX_LINES);
    }

    /// The editor in at most `rows` lines. A modal does not scroll, so on a
    /// short panel the least important lines go, whole, until the rest fit;
    /// see [`Fit`] for the order. A refusal's reason outlasts everything but
    /// the value it refused.
    pub fn lines_in(&self, out: &mut Lines, rows: usize) {
        let mut fit = Fit::new();
        let mut v = Text::<LINE_CHARS>::new();
        let mut line = Text::<LINE_CHARS>::new();

        field_value(&mut v, self.field(), self.original());
        let _ = write!(
            line,
            "{:<width$}{}",
            "Was",
            v.as_str(),
            width = EDIT_VALUE_COL
        );
        fit.line(Fit::CONTEXT, line.as_str());

        v.clear();
        line.clear();
        field_value(&mut v, self.field(), self.candidate());
        let _ = write!(
            line,
            "{:<width$}{}",
            "Now",
            v.as_str(),
            width = EDIT_VALUE_COL
        );
        fit.line(Fit::VALUE, line.as_str());

        if self.field() == Field::Band && self.changed() {
            // Where the other band lands, because the editor moves three
            // values and shows one. Two lines where the other steppers have
            // a blank, so a refusal still fits under it.
            let c = self.candidate_config();
            v.clear();
            line.clear();
            megahertz(&mut v, c.frequency_hz);
            let _ = write!(
                line,
                "{:<width$}{}",
                "Sets",
                v.as_str(),
                width = EDIT_VALUE_COL
            );
            fit.line(Fit::POSITION, line.as_str());
            line.clear();
            kilohertz(&mut line, c.bandwidth_hz);
            let _ = write!(line, ", {} dBm", c.tx_power_dbm);
            fit.join(line.as_str());
            fit.line(Fit::HELP, "Up/Down: change");
        } else if self.is_stepper() {
            fit.line(Fit::SPACING, "");
            fit.line(Fit::HELP, "Up/Down: change");
        } else {
            // `MMMM.kkk`: the cursor's column skips the point. An underscore
            // rather than a caret, because the font has one and not the
            // other, and an underline under a digit reads as a cursor -- and
            // under the blank a sub-GHz frequency has for its thousands
            // digit, it reads as the place where one could go.
            let at = self.cursor();
            let col = EDIT_VALUE_COL + at + usize::from(at >= FREQ_POINT);
            line.clear();
            let _ = write!(line, "{:>width$}", "_", width = col + 1);
            fit.line(Fit::POSITION, line.as_str());
            fit.line(Fit::HELP, "Up/Down: digit");
            fit.line(Fit::HELP, "Left/Right: move");
        }
        fit.line(Fit::CONTEXT, "OK: set Back: cancel");
        if let Some(reason) = self.refused() {
            fit.line(Fit::SPACING, "");
            fit.line(Fit::LABEL, "Refused:");
            fit.wrapped(Fit::REASON, reason.message());
        }
        fit.emit(out, rows);
    }
}

impl Lock {
    /// The notice as lines: who has the radio, and what to do about it.
    pub fn lines(self, out: &mut Lines) {
        self.lines_in(out, MAX_LINES);
    }

    /// The notice in at most `rows` lines: on a short panel, who has the
    /// radio and what to do about it outlast why.
    pub fn lines_in(self, out: &mut Lines, rows: usize) {
        let mut fit = Fit::new();
        fit.line(Fit::VALUE, self.headline());
        fit.line(Fit::SPACING, "");
        fit.wrapped(
            Fit::HELP,
            "It set the radio up and believes what it set, so the panel will not change it underneath.",
        );
        fit.line(Fit::SPACING, "");
        fit.wrapped(
            Fit::REASON,
            match self {
                Lock::Usb => "Close the port to change settings here.",
                Lock::Bluetooth => "Disconnect the phone to change settings here.",
            },
        );
        fit.emit(out, rows);
    }
}

impl Refusal {
    /// The notice as lines: what was not sent, and why.
    pub fn lines(self, out: &mut Lines) {
        self.lines_in(out, MAX_LINES);
    }

    /// The notice in at most `rows` lines.
    pub fn lines_in(self, out: &mut Lines, rows: usize) {
        let mut fit = Fit::new();
        fit.line(Fit::VALUE, self.headline());
        fit.line(Fit::SPACING, "");
        fit.wrapped(Fit::REASON, self.reason());
        fit.emit(out, rows);
    }
}

/// How old a fix may be and still be sent as where the board is, with no
/// age beside it: what the receiver itself calls a fix ([`gps::Status::Fix`]).
///
/// [`gps::Status::Fix`]: crate::gps::Status::Fix
const FRESH: gps::Status = gps::Status::Fix;

/// The board's position as a message: `My position: 47.376887, 8.541694`,
/// in decimal degrees to six places, as a phone's map takes them. A fix the
/// receiver no longer confirms is sent with its age, `(fix 12 min old)`;
/// with no fix at all there is nothing to send, and it is refused rather
/// than sent as zeros.
pub fn position_text(position: &gps::Position, out: &mut impl Write) -> Result<(), Refusal> {
    let fix = position.fix.ok_or(Refusal::NoFix)?;
    let mut lat = Text::<LINE_CHARS>::new();
    let mut lon = Text::<LINE_CHARS>::new();
    degrees(&mut lat, fix.lat_udeg);
    degrees(&mut lon, fix.lon_udeg);
    let _ = write!(out, "My position: {}, {}", lat.as_str(), lon.as_str());
    if position.status != FRESH {
        let _ = write!(
            out,
            " (fix {} old)",
            age_or_dash(position.fix_age_s).as_str()
        );
    }
    Ok(())
}

/// A modal's lines, each in a group with a priority, cut to the rows a panel
/// has by dropping whole groups, least important first.
///
/// A modal does not scroll -- what it says has to fit -- and the board's own
/// panel shows every line. A 128 × 64 panel has four rows, fewer than the
/// shortest editor, and the first thing to fall off its bottom used to be
/// the reason a value was refused (issue #21). So the order is explicit:
/// lower priorities go first, and of two at the same priority the later one
/// goes first. A wrapped sentence is one group, because half a reason is
/// the failure this exists to remove.
struct Fit {
    all: Lines,
    /// The group each line belongs to.
    group: [u8; MAX_LINES],
    /// Each group's priority.
    priority: [u8; MAX_LINES],
    groups: usize,
}

impl Fit {
    /// The value being edited, and a notice's headline.
    const VALUE: u8 = 6;
    /// Why a value was refused, and what to do about a notice.
    const REASON: u8 = 5;
    /// Where the value is: the cursor, and where a band change lands.
    const POSITION: u8 = 4;
    /// What it was, and how to leave.
    const CONTEXT: u8 = 3;
    /// Which keys do what, and why a notice is up.
    const HELP: u8 = 2;
    /// The word over a refusal's reason.
    const LABEL: u8 = 1;
    /// Blank lines.
    const SPACING: u8 = 0;

    fn new() -> Self {
        Fit {
            all: Lines::new(),
            group: [0; MAX_LINES],
            priority: [0; MAX_LINES],
            groups: 0,
        }
    }

    /// A line in a group of its own.
    fn line(&mut self, priority: u8, text: &str) {
        if self.groups < MAX_LINES {
            self.priority[self.groups] = priority;
            self.groups += 1;
        }
        self.join(text);
    }

    /// A line in the same group as the one before it: kept or dropped with it.
    fn join(&mut self, text: &str) {
        let n = self.all.len();
        self.all.line(text);
        if self.all.len() > n {
            self.group[n] = self.groups.saturating_sub(1) as u8;
        }
    }

    /// A sentence, wrapped, as one group.
    fn wrapped(&mut self, priority: u8, text: &str) {
        let mut first = true;
        for piece in wrap(text, LINE_CHARS) {
            if first {
                self.line(priority, piece);
                first = false;
            } else {
                self.join(piece);
            }
        }
    }

    /// The lines that fit in `rows`, in their order.
    fn emit(self, out: &mut Lines, rows: usize) {
        let mut dropped = [false; MAX_LINES];
        let size = |g: usize| {
            (0..self.all.len())
                .filter(|&n| self.group[n] as usize == g)
                .count()
        };
        let mut total = self.all.len();
        while total > rows {
            let Some(g) = (0..self.groups)
                .filter(|&g| !dropped[g])
                .min_by_key(|&g| (self.priority[g], core::cmp::Reverse(g)))
            else {
                break;
            };
            dropped[g] = true;
            total -= size(g);
        }
        for n in 0..self.all.len() {
            if !dropped[self.group[n] as usize] {
                out.line(self.all.rows[n].as_str());
            }
        }
        out.overflowed |= self.all.overflowed;
    }
}

/// Draw the pairing box over the content: a filled panel with the six digits
/// knocked out of it, big enough to read across a table.
///
/// Over every screen, not only the Bluetooth one. Pairing is started from the
/// phone, and the person holding the phone has to read the digits off the
/// board whatever it happened to be showing.
pub fn passkey_box(canvas: &mut impl Canvas, key: u32) {
    let layout = Layout::for_canvas(canvas);
    let (left, right) = (
        layout.content_left,
        layout.width.saturating_sub(layout.content_left),
    );
    let height = 40;
    let top = layout.content_top + layout.content_h().saturating_sub(height) / 2;
    canvas.rect(left, top, right.saturating_sub(left), height, true);
    font::draw(canvas, left + 4, top + 5, "PAIR WITH", false);
    let mut digits = Text::<8>::new();
    let _ = write!(digits, "{:06}", key % 1_000_000);
    // Spaced out: each digit gets two cells, so it reads as a code and not
    // as a number.
    let mut x = left + 4;
    for c in digits.as_str().bytes() {
        font::draw_char(canvas, x, top + 22, c, false);
        x += font::ADVANCE * 2 + 1;
    }
}

/// Draw the current screen: chrome, its lines, the scrollbar, a menu if one
/// is open, and the pairing box if a phone is waiting for its digits.
///
/// This is the one composition the firmware and the simulator share, on top
/// of [`monopanel::page`]. The title bar's corners are the battery and the
/// board's name, which are the two things worth seeing on every screen: the
/// one says how long it will last, the other says which board this is when
/// there are several on the table. What Bluetooth is doing is on the
/// Bluetooth screen and nowhere else.
/// Any canvas will do: the board's frame, or a bitmap of another size.
///
/// With a conversation open, [`render_with`] is the one to call.
pub fn render(canvas: &mut impl Canvas, nav: &mut Nav, state: &State) {
    render_with(canvas, nav, state, None);
}

/// [`render`], with the conversation that is open, if one is: its messages
/// are not part of [`State`], because they are the node's and are borrowed
/// rather than copied. A conversation open with no thread handed in, or
/// with another peer's, draws as one with no messages.
///
/// Returns how far into the conversation the panel now shows, for the
/// caller to mark read -- on a panel that is lit, since one that is out
/// shows nobody anything. The first time a conversation is drawn it is
/// placed at its first unread message, or at its end if none is.
pub fn render_with(
    canvas: &mut impl Canvas,
    nav: &mut Nav,
    state: &State,
    thread: Option<&Thread<'_>>,
) -> Option<Read> {
    let mut lines = Lines::new();
    // An editor or a notice replaces the screen's lines; the chrome stays.
    // Cut to the rows this canvas has, because a modal does not scroll.
    let rows = Layout::for_canvas(canvas).visible_lines();
    let mut left = Text::<8>::new();
    if let Some(cell) = state.home.battery {
        let _ = write!(left, "{}%", cell.percent);
    }
    let name = match &state.name {
        Some(name) => core::str::from_utf8(name).unwrap_or("?"),
        None => "",
    };
    let mut read = None;
    if let Some(editor) = nav.editor() {
        editor.lines_in(&mut lines, rows);
        let strs = lines.as_strs();
        page(canvas, nav, left.as_str(), name, &strs[..lines.len()]);
    } else if let Some(lock) = nav.notice_shown() {
        lock.lines_in(&mut lines, rows);
        let strs = lines.as_strs();
        page(canvas, nav, left.as_str(), name, &strs[..lines.len()]);
    } else if let Some(refusal) = nav.refusal_shown() {
        refusal.lines_in(&mut lines, rows);
        let strs = lines.as_strs();
        page(canvas, nav, left.as_str(), name, &strs[..lines.len()]);
    } else if let Some(typing) = nav.typing().copied() {
        // The chrome with no lines, and the keyboard in the content area.
        page(canvas, nav, left.as_str(), name, &[]);
        monopanel::keyboard::draw(canvas, &typing);
    } else if let Some(open) = nav.conversation().copied() {
        let none = Thread {
            peer: open.peer(),
            name: None,
            entries: &[],
        };
        let thread = thread.filter(|t| t.peer == open.peer()).unwrap_or(&none);
        let total = thread.len();
        nav.set_content(total, rows);
        if !open.placed() {
            nav.set_scroll(thread.first_unread().unwrap_or(usize::MAX));
        } else if open.wants_end() {
            nav.set_scroll(usize::MAX);
        }
        if let Some(open) = nav.conversation_mut() {
            open.lay_out(thread);
        }
        let first = nav.scroll();
        thread.window(first, rows, &mut lines);
        let strs = lines.as_strs();
        page_of(
            canvas,
            nav,
            left.as_str(),
            name,
            Content::Window {
                total,
                lines: &strs[..lines.len()],
            },
        );
        // Under a menu, the middle of the thread is covered.
        if !nav.menu_is_open() {
            read = Some(Read {
                peer: thread.peer,
                through: thread.read_through(first, rows),
            });
        }
    } else {
        let screen = nav.current();
        state.lines(screen, &mut lines);
        let strs = lines.as_strs();
        let strs = &strs[..lines.len()];
        #[cfg(feature = "node")]
        if screen == Screen::Messages && state.conversations.rows.iter().all(Option::is_none) {
            page_of(canvas, nav, left.as_str(), name, Content::Empty(strs));
        } else {
            page(canvas, nav, left.as_str(), name, strs);
        }
        #[cfg(not(feature = "node"))]
        page(canvas, nav, left.as_str(), name, strs);
    }
    if let Some(key) = state.bluetooth.passkey {
        passkey_box(canvas, key);
    }
    read
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rnode::hosts::Transport;
    use crate::sh1107::{self, Frame};
    use crate::ui::{Input, Screen, LAYOUT};

    /// A navigator on the Radio screen, however many screens are before it.
    fn on_radio() -> Nav {
        let mut nav = ui::nav();
        while nav.current() != Screen::Radio {
            nav.handle(Input::Right);
        }
        nav
    }

    /// A board mid-session: every field known, every option `Some`.
    fn populated() -> State {
        State {
            name: Some(*b"7F23"),
            home: Home {
                host: Host::Usb,
                mode: Some(Mode::Rnode(Transport::Usb)),
                unread: Some(2),
                talking: true,
                air: Air::Receiving,
                rx_count: 42,
                tx_count: 7,
                last_rssi_dbm: Some(-69),
                last_snr_quarter_db: Some(45),
                uptime_s: Some(3_723),
                battery: Some(battery::Reading {
                    millivolts: 4_020,
                    percent: 91,
                }),
                charger: battery::Charger::Charging,
            },
            radio: Radio {
                config: RadioConfig {
                    frequency_hz: 915_000_000,
                    bandwidth_hz: 125_000,
                    spreading_factor: 8,
                    coding_rate: 5,
                    tx_power_dbm: 17,
                    ..crate::lr1121::config::DEFAULT
                },
                air: Air::Receiving,
                tnc: true,
            },
            bluetooth: BluetoothScreen {
                link: Link::Advertising,
                name: Some(*b"RNode 7F23"),
                passkey: None,
                bonded: 2,
            },
            position: fixed(),
            system: System {
                version: "0.0.0",
                build: "0000000",
                serial: Some(*b"0123456789ABCDEF"),
                identity: Identity::Signed,
                node: Some(NodeHashes {
                    identity: [0xa1; 16],
                    lxmf: core::array::from_fn(|i| 0x10 + i as u8),
                }),
                announce_gap_s: Some(252),
                free_ram: Some(123_456),
            },
            peers: three_peers(),
            conversations: two_conversations(),
        }
    }

    /// Three peers: one straight off the air, one relayed, one with no name.
    fn three_peers() -> Peers {
        let mut peers = Peers::default();
        peers.rows[0] = Some(PeerRow {
            destination: [0x11; 16],
            name: Some(PeerName::new("Ana")),
            age_s: Some(12),
            hops: Some(1),
            signal: Some(crate::rnode::air::Signal {
                rssi_dbm: -61,
                snr_quarter_db: 38,
            }),
        });
        peers.rows[1] = Some(PeerRow {
            destination: [0x22; 16],
            name: Some(PeerName::new("oxinode 354B")),
            age_s: Some(300),
            hops: Some(3),
            signal: None,
        });
        peers.rows[2] = Some(PeerRow {
            destination: core::array::from_fn(|i| 0xa0 + i as u8),
            name: None,
            age_s: Some(7_200),
            hops: Some(1),
            signal: None,
        });
        peers
    }

    fn peer_lines(peers: &Peers) -> Vec<String> {
        let mut out = Lines::new();
        peers.lines(&mut out);
        assert!(!out.overflowed(), "a peer's line was cut");
        (0..out.len())
            .map(|i| out.get(i).unwrap().to_string())
            .collect()
    }

    #[test]
    fn no_peers_says_so() {
        assert_eq!(peer_lines(&Peers::default()), ["No peers heard yet."]);
    }

    #[test]
    fn a_peer_is_its_name_and_age_then_how_it_was_heard() {
        assert_eq!(
            peer_lines(&three_peers()),
            [
                "Ana             12 s",
                "  -61 dBm 9.5 dB",
                "oxinode 354B   5 min",
                "  3 hops",
                "<a0a1a2a3a4>     2 h",
                "  1 hop",
            ]
        );
    }

    #[test]
    fn a_peer_with_nothing_known_but_its_address_is_dashes() {
        let mut peers = Peers::default();
        peers.rows[0] = Some(PeerRow {
            destination: [0xff; 16],
            name: None,
            age_s: None,
            hops: None,
            signal: None,
        });
        assert_eq!(peer_lines(&peers), ["<ffffffffff>       -", "  -"]);
    }

    #[test]
    fn a_peer_at_its_widest_fits_and_a_long_name_gives_way_to_the_age() {
        let mut peers = Peers::default();
        peers.rows[0] = Some(PeerRow {
            destination: [0; 16],
            name: Some(PeerName::new("A name far too long for any row")),
            age_s: Some(u32::MAX),
            hops: Some(1),
            signal: Some(crate::rnode::air::Signal {
                rssi_dbm: -128,
                snr_quarter_db: i8::MIN,
            }),
        });
        peers.rows[1] = Some(PeerRow {
            name: None,
            hops: Some(u8::MAX),
            signal: None,
            ..peers.rows[0].unwrap()
        });
        assert_eq!(
            peer_lines(&peers),
            [
                "A name far 1193046 h",
                "  -128 dBm -32.0 dB",
                "<000000000 1193046 h",
                "  255 hops",
            ]
        );
    }

    #[test]
    fn a_name_the_font_cannot_draw_is_one_box_a_character() {
        let name = PeerName::new("Zoë 🙂!");
        assert_eq!(name.as_str(), "Zo\x7f \x7f!");
        assert_eq!(
            PeerName::new("a\tb\n").as_str(),
            "a\x7fb\x7f",
            "control characters too"
        );
        assert_eq!(monopanel::font::glyph(PeerName::UNDRAWABLE), None, "a box");
        assert_eq!(PeerName::new(&"x".repeat(40)).as_str().len(), LINE_CHARS);
    }

    fn drawn(text: &str) -> String {
        drawable(text).map(char::from).collect()
    }

    #[test]
    fn what_a_phone_types_for_punctuation_is_folded_to_what_the_font_draws() {
        // The text of the message Python LXMF sent in the node's vectors.
        let sent = "Hello from the bench \u{2013} it\u{2019}s \u{201c}Python\u{201d}\u{2026}";
        let shown = drawn(sent);
        assert_eq!(shown, "Hello from the bench - it's 'Python'...");
        for c in shown.bytes() {
            assert!(
                monopanel::font::glyph(c).is_some(),
                "{:?} draws as a box",
                c as char
            );
        }
        assert_eq!(drawn("\u{2018}a\u{2019} \u{2032}"), "'a' '");
        assert_eq!(
            drawn("\"\u{201e}\u{201f}\u{2033}"),
            "''''",
            "the font has no \""
        );
        assert_eq!(
            drawn("\u{2010}\u{2011}\u{2012}\u{2013}\u{2014}\u{2015}\u{2212}"),
            "-------"
        );
        assert_eq!(drawn("a\u{00a0}b\u{2009}c\u{202f}d"), "a b c d");
        // What is not punctuation stays a box: it is not a guess at a letter.
        assert_eq!(drawn("\u{00e9}\u{2016}\u{2027}"), "\x7f\x7f\x7f");
    }

    #[test]
    fn a_folded_name_is_still_cut_at_the_row() {
        let name = PeerName::new(&"\u{2026}".repeat(LINE_CHARS));
        assert_eq!(name.as_str(), ".".repeat(LINE_CHARS));
        assert_eq!(PeerName::new("it\u{2019}s").as_str(), "it's");
    }

    #[test]
    fn a_full_peers_screen_fits_the_line_buffer() {
        let row = three_peers().rows[0];
        let full = Peers { rows: [row; PEERS] };
        let lines = peer_lines(&full);
        assert_eq!(lines.len(), 2 * PEERS);
        assert!(lines.len() < MAX_LINES);
    }

    /// Every field of a peer, changed on its own, changes a line.
    #[test]
    fn changing_any_peer_field_changes_a_line() {
        type Mutation = (&'static str, fn(&mut PeerRow));
        let mutations: [Mutation; 6] = [
            ("name", |r| r.name = Some(PeerName::new("Bea"))),
            ("no name", |r| r.name = None),
            ("age", |r| r.age_s = Some(13)),
            ("hops", |r| {
                r.signal = None;
                r.hops = Some(2);
            }),
            ("rssi", |r| r.signal.as_mut().unwrap().rssi_dbm = -60),
            ("snr", |r| r.signal.as_mut().unwrap().snr_quarter_db = 39),
        ];
        let base = three_peers();
        for (name, mutate) in mutations {
            let mut changed = base;
            mutate(changed.rows[0].as_mut().unwrap());
            assert_ne!(peer_lines(&changed), peer_lines(&base), "{name}");
        }
        // The address shows only with no name.
        let mut changed = base;
        changed.rows[2].as_mut().unwrap().destination[0] = 0;
        assert_ne!(peer_lines(&changed), peer_lines(&base), "destination");
    }

    /// Two conversations: one with something unread, and one with a peer
    /// that gave no name, read.
    fn two_conversations() -> Conversations {
        let mut c = Conversations::default();
        c.rows[0] = Some(ConversationRow {
            peer: [0x11; 16],
            name: Some(PeerName::new("Ana")),
            unread: true,
            age_s: Some(12),
        });
        c.rows[1] = Some(ConversationRow {
            peer: core::array::from_fn(|i| 0xa0 + i as u8),
            name: None,
            unread: false,
            age_s: Some(7_200),
        });
        c
    }

    fn conversation_lines(c: &Conversations) -> (Vec<String>, usize) {
        let mut out = Lines::new();
        let entries = c.lines(&mut out);
        assert!(!out.overflowed(), "a conversation's line was cut");
        let lines = (0..out.len())
            .map(|i| out.get(i).unwrap().to_string())
            .collect();
        (lines, entries)
    }

    #[test]
    fn no_conversations_says_so_and_has_nothing_to_open() {
        let (lines, entries) = conversation_lines(&Conversations::default());
        assert_eq!(entries, 0);
        assert_eq!(lines, ["No messages yet."]);
    }

    #[test]
    fn a_conversation_is_a_mark_a_name_and_an_age() {
        let (lines, entries) = conversation_lines(&two_conversations());
        assert_eq!(entries, 2);
        assert_eq!(lines, ["*Ana            12 s", " <a0a1a2a3a4>    2 h"]);
    }

    #[test]
    fn a_conversation_at_its_widest_fits_and_the_name_gives_way() {
        let mut c = Conversations::default();
        let row = ConversationRow {
            peer: [0; 16],
            name: Some(PeerName::new("A name far too long for any row")),
            unread: true,
            age_s: Some(u32::MAX),
        };
        c.rows = [Some(row); CONVERSATIONS];
        c.rows[1] = Some(ConversationRow { age_s: None, ..row });
        let (lines, entries) = conversation_lines(&c);
        assert_eq!(entries, CONVERSATIONS);
        assert_eq!(lines[0], "*A name fa 1193046 h");
        assert_eq!(lines[1], "*A name far too lo -");
        assert!(lines.len() < MAX_LINES);
    }

    /// Every field of a conversation, changed on its own, changes its line.
    #[test]
    fn changing_any_conversation_field_changes_a_line() {
        type Mutation = (&'static str, fn(&mut ConversationRow));
        let mutations: [Mutation; 4] = [
            ("name", |r| r.name = Some(PeerName::new("Bea"))),
            ("no name", |r| r.name = None),
            ("unread", |r| r.unread = false),
            ("age", |r| r.age_s = Some(13)),
        ];
        let base = two_conversations();
        for (name, mutate) in mutations {
            let mut changed = base;
            mutate(changed.rows[0].as_mut().unwrap());
            assert_ne!(
                conversation_lines(&changed),
                conversation_lines(&base),
                "{name}"
            );
        }
        let mut changed = base;
        changed.rows[1].as_mut().unwrap().peer[0] = 0;
        assert_ne!(
            conversation_lines(&changed),
            conversation_lines(&base),
            "peer"
        );
    }

    // ---- the thread -------------------------------------------------------

    fn wrapped(text: &str, width: usize) -> Vec<String> {
        wrap_drawn(text, width).map(str::to_string).collect()
    }

    /// Text from anywhere is wrapped by what the panel draws: newlines kept,
    /// blank lines kept, spaces at the ends dropped, and no character cut.
    #[test]
    fn what_somebody_wrote_wraps_to_the_line() {
        assert_eq!(wrapped("one two three", 7), ["one two", "three"]);
        assert_eq!(
            wrapped("one\ntwo\r\n\nthree  ", 20),
            ["one", "two", "", "three"]
        );
        assert_eq!(wrapped("  indented", 20), ["indented"]);
        assert_eq!(wrapped("", 20), [""; 1], "an empty text is one blank line");
        // Measured as drawn: an ellipsis is three cells, an é is one.
        assert_eq!(wrapped("ab\u{2026}c", 5), ["ab\u{2026}", "c"]);
        assert_eq!(wrapped("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}", 5).len(), 2);
        // A word longer than the line is cut, at a character.
        let long = "\u{1f642}".repeat(25);
        let lines = wrapped(&long, 20);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].chars().count(), 20);
        // Every word is kept, every line fits.
        let text = "The quick brown fox jumps over the lazy dog, and then over it again \u{2026}";
        let lines = wrapped(text, 18);
        assert_eq!(lines.join(" "), text);
        for line in &lines {
            assert!(drawable(line).count() <= 18, "`{line}`");
        }
    }

    const HELLO: &str = "Hello from the bench \u{2013} it\u{2019}s \u{201c}Python\u{201d}\u{2026}";

    /// A conversation with Ana: what the bench sent, a reply delivered, and
    /// a question from a sender not yet verified, unread.
    fn entries() -> [Entry<'static>; 3] {
        [
            Entry {
                direction: Direction::Received(Trust::Verified),
                age_s: Some(420),
                title: "",
                content: HELLO,
                attachment: false,
                unread: false,
            },
            Entry {
                direction: Direction::Sent(Delivery::Delivered),
                age_s: Some(300),
                title: "",
                content: "On my way",
                attachment: false,
                unread: false,
            },
            Entry {
                direction: Direction::Received(Trust::Unverified),
                age_s: Some(12),
                title: "",
                content: "Where are you?",
                attachment: false,
                unread: true,
            },
        ]
    }

    fn thread<'a>(entries: &'a [Entry<'a>]) -> Thread<'a> {
        Thread {
            peer: [0x11; 16],
            name: Some(PeerName::new("Ana")),
            entries,
        }
    }

    fn thread_lines(thread: &Thread<'_>) -> Vec<String> {
        let mut out = Lines::new();
        let all = thread.len();
        assert!(
            all < MAX_LINES,
            "too long for one buffer: test it in windows"
        );
        thread.window(0, all, &mut out);
        assert!(!out.overflowed(), "a thread line was cut");
        (0..out.len())
            .map(|i| out.get(i).unwrap().to_string())
            .collect()
    }

    #[test]
    fn a_thread_tells_received_from_sent_by_where_they_stand() {
        let e = entries();
        assert_eq!(
            thread_lines(&thread(&e)),
            [
                "Ana            7 min",
                "Hello from the bench",
                "- it's 'Python'...",
                "",
                "You  delivered 5 min",
                "  On my way",
                "",
                "Ana             12 s",
                "(unverified sender)",
                "Where are you?",
            ]
        );
    }

    #[test]
    fn every_outgoing_state_has_its_word() {
        let mut e = entries();
        let mut seen = Vec::new();
        for delivery in Delivery::EVERY {
            e[1].direction = Direction::Sent(delivery);
            let lines = thread_lines(&thread(&e));
            let header = lines[4].clone();
            assert!(header.starts_with("You "), "{header}");
            assert!(
                header.ends_with(&format!("{} 5 min", delivery.word())),
                "{header}"
            );
            // The reason, if any, under the header; then the text.
            match delivery.note() {
                Some(note) => {
                    assert_eq!(lines[5], note);
                    assert!(note.len() <= LINE_CHARS, "{note}");
                }
                None => assert_eq!(lines[5], "  On my way"),
            }
            seen.push(lines[4..6].join("|"));
        }
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), Delivery::EVERY.len(), "{seen:?}");
        // At the widest age, the state outlasts the name.
        e[1].direction = Direction::Sent(Delivery::Delivered);
        e[1].age_s = Some(u32::MAX);
        assert_eq!(thread_lines(&thread(&e))[4], " delivered 1193046 h");
    }

    #[test]
    fn a_sender_is_trusted_only_when_verified() {
        let mut e = entries();
        e[0].direction = Direction::Received(Trust::Failed);
        let lines = thread_lines(&thread(&e));
        assert_eq!(lines[1], "(bad signature)");
        e[0].direction = Direction::Received(Trust::Verified);
        let lines = thread_lines(&thread(&e));
        assert!(!lines[1].starts_with('('), "{lines:?}");
    }

    #[test]
    fn what_is_not_shown_is_said() {
        let mut e = entries();
        e[1].content = "";
        e[1].attachment = true;
        assert_eq!(thread_lines(&thread(&e))[5], "  (attachment)");
        e[1].attachment = false;
        e[1].content = "   ";
        assert_eq!(thread_lines(&thread(&e))[5], "  (empty)");
        e[1].title = "Plans";
        e[1].content = "Lunch?";
        let lines = thread_lines(&thread(&e));
        assert_eq!(&lines[5..7], ["  Plans", "  Lunch?"]);
        let none = thread(&[]);
        assert_eq!(thread_lines(&none), ["No messages."]);
        assert!(none.is_empty());
    }

    #[test]
    fn a_name_the_font_lacks_and_text_it_lacks_draw_as_boxes() {
        let mut e = entries();
        e[0].content = "Gr\u{fc}\u{df}e \u{1f642}";
        let mut t = thread(&e);
        t.name = Some(PeerName::new("Zo\u{eb}"));
        let lines = thread_lines(&t);
        assert_eq!(lines[0], "Zo\x7f            7 min");
        assert_eq!(lines[1], "Gr\x7f\x7fe \x7f");
    }

    /// A message longer than the screen wraps to as many lines as it needs,
    /// every one inside the row, every word kept; the window shows it a
    /// screenful at a time, exactly as the whole would.
    #[test]
    fn a_long_message_wraps_and_is_never_cut() {
        let long: String = (0..120).map(|n| format!("word{n} ")).collect();
        let e = [Entry {
            content: &long,
            ..entries()[0]
        }];
        let t = Thread {
            peer: [0; 16],
            name: None,
            entries: &e,
        };
        let total = t.len();
        assert!(total > 2 * LAYOUT.visible_lines(), "{total}");
        let mut all = Vec::new();
        for first in (0..total).step_by(LAYOUT.visible_lines()) {
            let mut out = Lines::new();
            t.window(first, LAYOUT.visible_lines(), &mut out);
            assert!(!out.overflowed());
            all.extend((0..out.len()).map(|i| out.get(i).unwrap().to_string()));
        }
        assert_eq!(all.len(), total);
        assert_eq!(all[1..].join(" "), long.trim());
    }

    #[test]
    fn a_thread_knows_where_its_messages_start_and_what_has_been_shown() {
        let e = entries();
        let t = thread(&e);
        let mut stops = [0u16; THREAD];
        assert_eq!(t.stops(&mut stops), 3);
        assert_eq!(&stops[..3], [0, 4, 7]);
        assert_eq!(t.first_unread(), Some(7));
        // The first's last line is line 2; the third's is line 9.
        assert_eq!(t.read_through(0, 2), 0);
        assert_eq!(t.read_through(0, 3), 1);
        assert_eq!(t.read_through(0, 6), 2);
        assert_eq!(t.read_through(0, 9), 2);
        assert_eq!(t.read_through(0, 10), 3);
        assert_eq!(t.read_through(5, 5), 3, "passed counts as shown");
        let mut none = entries();
        none[2].unread = false;
        assert_eq!(thread(&none).first_unread(), None);
    }

    /// Every field of a message, changed on its own, changes the thread.
    #[test]
    fn changing_any_entry_field_changes_a_line() {
        type Mutation = (&'static str, fn(&mut [Entry<'static>; 3]));
        let mutations: [Mutation; 9] = [
            ("trust", |e| {
                e[2].direction = Direction::Received(Trust::Verified)
            }),
            ("delivery", |e| {
                e[1].direction = Direction::Sent(Delivery::Queued(Wait::Turn))
            }),
            ("direction", |e| {
                e[0].direction = Direction::Sent(Delivery::Sent(1))
            }),
            ("age", |e| e[0].age_s = Some(421 + 60)),
            ("no age", |e| e[0].age_s = None),
            ("title", |e| e[0].title = "Hi"),
            ("content", |e| e[1].content = "On my way!"),
            ("attachment", |e| e[1].attachment = true),
            ("unread", |e| e[0].unread = true),
        ];
        let base = entries();
        for (name, mutate) in mutations {
            let mut changed = base;
            mutate(&mut changed);
            let (a, b) = (thread(&changed), thread(&base));
            let lines_differ = thread_lines(&a) != thread_lines(&b);
            // Unread is not drawn: it says where the thread opens.
            let opens_differ = a.first_unread() != b.first_unread();
            assert!(lines_differ || opens_differ, "{name}");
        }
        let mut t = thread(&base);
        t.name = None;
        assert_ne!(thread_lines(&t), thread_lines(&thread(&base)), "name");
        t.peer[0] = 0;
        let mut u = thread(&base);
        u.name = None;
        assert_ne!(thread_lines(&t), thread_lines(&u), "peer");
    }

    /// A navigator with the conversation with Ana open over Messages.
    #[cfg(feature = "node")]
    fn in_conversation() -> Nav {
        let mut nav = ui::nav();
        nav.handle(Input::Right);
        nav.open_conversation([0x11; 16]);
        nav
    }

    #[cfg(feature = "node")]
    fn draw_thread(nav: &mut Nav, thread: Option<&Thread<'_>>) -> (Frame, Option<Read>) {
        let mut frame = Frame::new();
        let read = render_with(&mut frame, nav, &populated(), thread);
        (frame, read)
    }

    /// A conversation opens at its first unread message, and the panel says
    /// how far it has shown; with nothing unread it opens at its end.
    #[cfg(feature = "node")]
    #[test]
    fn a_conversation_opens_at_the_first_unread_message() {
        let long: String = (0..30).map(|n| format!("word{n} ")).collect();
        let mut e = entries();
        e[0].content = &long;
        let t = thread(&e);
        let unread = t.first_unread().unwrap();
        assert!(unread > LAYOUT.visible_lines(), "below the first screen");
        let mut nav = in_conversation();
        let (_, read) = draw_thread(&mut nav, Some(&t));
        assert_eq!(nav.scroll(), unread.min(t.len() - LAYOUT.visible_lines()));
        assert!(nav.conversation().unwrap().placed());
        assert_eq!(
            read,
            Some(Read {
                peer: [0x11; 16],
                through: 3
            })
        );
        // Once placed, it stays where it is put.
        nav.handle(Input::Left);
        nav.handle(Input::Left);
        let at = nav.scroll();
        draw_thread(&mut nav, Some(&t));
        assert_eq!(nav.scroll(), at);
        assert!(at < unread);

        e[2].unread = false;
        let t = thread(&e);
        let mut nav = in_conversation();
        draw_thread(&mut nav, Some(&t));
        assert_eq!(nav.scroll(), t.len() - LAYOUT.visible_lines(), "the end");
    }

    /// A reply just written is at the end of a thread that was drawn
    /// without it: the next drawing shows it, once, then the thread stays
    /// where it is put again.
    #[cfg(feature = "node")]
    #[test]
    fn a_reply_written_is_shown_at_the_end() {
        let long: String = (0..30).map(|n| format!("word{n} ")).collect();
        let mut e = entries().to_vec();
        e[0].content = &long;
        for entry in &mut e {
            entry.unread = false;
        }
        let mut nav = in_conversation();
        draw_thread(&mut nav, Some(&thread(&e)));
        nav.handle(Input::Left);
        nav.handle(Input::Left);
        let before = nav.scroll();
        let mut more = e.clone();
        more.push(Entry {
            direction: Direction::Sent(Delivery::Queued(Wait::Turn)),
            age_s: Some(0),
            title: "",
            content: "Yes",
            attachment: false,
            unread: false,
        });
        let t = thread(&more);
        nav.conversation_mut().unwrap().show_end();
        draw_thread(&mut nav, Some(&t));
        assert_eq!(nav.scroll(), t.len() - LAYOUT.visible_lines());
        assert!(nav.scroll() > before);
        assert!(!nav.conversation().unwrap().wants_end(), "once");
        nav.handle(Input::Up);
        let at = nav.scroll();
        draw_thread(&mut nav, Some(&t));
        assert_eq!(nav.scroll(), at);
    }

    /// Sideways moves by message, landing on each header in turn.
    #[cfg(feature = "node")]
    #[test]
    fn a_conversation_is_walked_by_message() {
        let long: String = (0..40).map(|n| format!("w{n} ")).collect();
        let mut e = entries();
        e[0].content = &long;
        e[1].content = &long;
        e[2].unread = false;
        let t = thread(&e);
        let mut stops = [0u16; THREAD];
        let n = t.stops(&mut stops);
        let mut nav = in_conversation();
        draw_thread(&mut nav, Some(&t));
        for _ in 0..5 {
            nav.handle(Input::Left);
        }
        assert_eq!(nav.scroll(), 0);
        nav.handle(Input::Right);
        assert_eq!(nav.scroll(), usize::from(stops[1]));
        let mut out = Lines::new();
        t.window(nav.scroll(), 1, &mut out);
        assert!(out.get(0).unwrap().starts_with("You "), "a header");
        nav.handle(Input::Right);
        let max = t.len() - LAYOUT.visible_lines();
        assert_eq!(nav.scroll(), usize::from(stops[n - 1]).min(max));
    }

    /// Under a menu nothing is counted as shown; a thread for another peer,
    /// or none, draws as one with no messages.
    #[cfg(feature = "node")]
    #[test]
    fn a_conversation_reads_only_what_it_shows() {
        let e = entries();
        let t = thread(&e);
        let mut nav = in_conversation();
        nav.handle(Input::Select);
        assert!(nav.menu_is_open());
        assert_eq!(draw_thread(&mut nav, Some(&t)).1, None);
        nav.handle(Input::Back);
        let mut other = thread(&e);
        other.peer = [0x22; 16];
        let (frame, read) = draw_thread(&mut nav, Some(&other));
        assert_eq!(read.map(|r| r.through), Some(0));
        let (none, _) = draw_thread(&mut nav, None);
        assert_eq!(frame.as_bytes(), none.as_bytes());
        let (some, _) = draw_thread(&mut nav, Some(&t));
        assert_ne!(some.as_bytes(), none.as_bytes());
    }

    /// The Messages screen with nothing in it opens nothing; with
    /// conversations, its first line is highlighted and select asks for it.
    #[cfg(feature = "node")]
    #[test]
    fn the_messages_screen_highlights_what_select_opens() {
        let empty = State {
            home: populated().home,
            ..State::default()
        };
        let mut nav = ui::nav();
        nav.handle(Input::Right);
        render(&mut Frame::new(), &mut nav, &empty);
        assert_eq!(nav.selected(), None);
        assert_eq!(nav.handle(Input::Select), None);
        render(&mut Frame::new(), &mut nav, &populated());
        assert_eq!(nav.selected(), Some(0));
        nav.handle(Input::Down);
        assert_eq!(nav.handle(Input::Select), Some(ui::Action::Open));
        assert_eq!(nav.selected(), Some(1));
    }

    /// A receiver with a fix, at a place nobody would mistake for a default.
    fn fixed() -> Position {
        Position {
            status: gps::Status::Fix,
            sats_used: Some(7),
            sats_in_view: Some(11),
            fix: Some(gps::Fix {
                lat_udeg: 47_376_887,
                lon_udeg: 8_541_694,
                alt_dm: Some(4_080),
            }),
            fix_age_s: Some(3),
            time: Some(gps::Time {
                hour: 13,
                minute: 47,
                second: 9,
            }),
            date: Some(gps::Date {
                year: 2026,
                month: 9,
                day: 8,
            }),
        }
    }

    fn lines_of(state: &State, screen: Screen) -> Vec<String> {
        let mut out = Lines::new();
        state.lines(screen, &mut out);
        (0..out.len())
            .map(|n| out.get(n).unwrap().to_string())
            .collect()
    }

    fn frame_of(state: &State, screen: Screen) -> Frame {
        let mut nav = ui::nav();
        for _ in 0..screen.index() {
            nav.handle(Input::Right);
        }
        let mut frame = Frame::new();
        render(&mut frame, &mut nav, state);
        frame
    }

    /// Every line of every screen, empty and populated, fits the row.
    ///
    /// The row format never truncates a value, so a value that is too wide
    /// pushes its label off the left edge -- invisible in any test but this.
    #[test]
    fn every_line_fits_the_panel() {
        let width = LAYOUT.line_width();
        for state in [State::default(), populated()] {
            for screen in Screen::ALL {
                for line in lines_of(&state, screen) {
                    assert!(
                        line.len() <= LINE_CHARS && font::width_of(&line) <= width,
                        "{screen:?}: `{line}` is {} chars",
                        line.len()
                    );
                }
                // The rows are the right width by construction; what the
                // check above cannot see is a line that was *cut* to fit.
                let mut out = Lines::new();
                state.lines(screen, &mut out);
                assert!(!out.overflowed(), "{screen:?} lost the tail of a line");
            }
        }
    }

    /// A line too wide for the row is recorded as such, which is the only
    /// way a test can tell a cut line from one that fitted.
    #[test]
    fn a_line_that_is_cut_is_reported() {
        let mut out = Lines::new();
        out.line("exactly twenty chars");
        assert!(!out.overflowed());
        out.line("twenty-one characters");
        assert!(out.overflowed());
        let mut out = Lines::new();
        out.row("Label", "a value that is far too wide");
        assert!(out.overflowed());
    }

    /// No screen produces more lines than the buffer holds.
    #[test]
    fn no_screen_overflows_the_line_buffer() {
        let mut refused = populated();
        refused.radio.air = Air::Refused(ConfigError::UnsupportedBandwidth);
        for state in [State::default(), populated(), refused] {
            for screen in Screen::ALL {
                let mut out = Lines::new();
                state.lines(screen, &mut out);
                assert!(out.len() < MAX_LINES, "{screen:?} has {} lines", out.len());
            }
        }
    }

    /// **The rule about empty state.** Nothing unknown draws as a number.
    #[test]
    fn an_empty_state_shows_dashes_rather_than_zeroes() {
        let home = lines_of(&State::default(), Screen::Home);
        assert!(home
            .iter()
            .any(|l| l.starts_with("RSSI") && l.ends_with('-')));
        assert!(home
            .iter()
            .any(|l| l.starts_with("SNR") && l.ends_with('-')));
        assert!(home
            .iter()
            .any(|l| l.starts_with("Battery") && l.ends_with('-')));
        assert!(home
            .iter()
            .any(|l| l.starts_with("Uptime") && l.ends_with('-')));
        assert!(!home.iter().any(|l| l.contains("0%")), "{home:?}");
        let system = lines_of(&State::default(), Screen::System);
        assert!(system
            .iter()
            .any(|l| l.starts_with("Serial") && l.ends_with('-')));
        assert!(system
            .iter()
            .any(|l| l.starts_with("RAM") && l.ends_with('-')));
        assert!(system.iter().any(|l| l.ends_with("none")));
        let bt = lines_of(&State::default(), Screen::Bluetooth);
        assert!(bt.iter().any(|l| l.starts_with("Name") && l.ends_with('-')));
        assert!(bt.iter().any(|l| l.ends_with("absent")));
    }

    /// **Unread messages are counted on Home**, under the mode, while there
    /// are any; none, or an image without the node, draws no row.
    #[test]
    fn home_counts_unread_messages_while_there_are_any() {
        let mut state = populated();
        let lines = lines_of(&state, Screen::Home);
        let mode = lines.iter().position(|l| l.starts_with("Mode")).unwrap();
        assert_eq!(
            lines[mode + 1],
            format!("Unread{:>w$}", "2", w = LINE_CHARS - 6)
        );
        let no_row = |state: &State| {
            !lines_of(state, Screen::Home)
                .iter()
                .any(|l| l.starts_with("Unread"))
        };
        state.home.unread = Some(0);
        assert!(no_row(&state), "nothing unread");
        state.home.unread = None;
        assert!(no_row(&state), "no node");
        // With none unread, Home fits the panel without scrolling.
        assert!(lines_of(&state, Screen::Home).len() <= LAYOUT.visible_lines());
    }

    /// **The node's address is on the System screen exactly as a phone
    /// shows it**, in two rows of sixteen, and an image without a node shows
    /// no row for it at all.
    #[test]
    fn the_system_screen_shows_the_node_address_or_nothing() {
        let mut state = populated();
        let lines = lines_of(&state, Screen::System);
        let at = lines.iter().position(|l| l == "LXMF address").unwrap();
        assert_eq!(lines[at + 1], "1011121314151617");
        assert_eq!(lines[at + 2], "18191a1b1c1d1e1f");
        assert_eq!(lines[at + 3], "RNS identity");
        assert_eq!(lines[at + 4], "a1".repeat(8));
        assert_eq!(lines[at + 5], "a1".repeat(8));
        assert_eq!(lines[at + 6], "Announce 1 per 4m12s");

        state.system.node = None;
        let rnode = lines_of(&state, Screen::System);
        assert!(
            !rnode
                .iter()
                .any(|l| l.contains("LXMF") || l.contains("RNS")),
            "{rnode:?}"
        );
        assert!(!rnode.iter().any(|l| l.starts_with("Announce")));
        assert_eq!(rnode.len(), lines.len() - 7);
    }

    /// **The position screen with no fix says so, and shows no coordinate.**
    /// Off, silent and searching each name themselves on the first row,
    /// and no number stands where a coordinate would.
    #[test]
    fn the_position_screen_without_a_fix_says_so() {
        let lines = lines_of(&State::default(), Screen::Position);
        let text = lines.join(" ");
        assert!(
            lines[0].starts_with("GPS") && lines[0].ends_with("off"),
            "{text}"
        );
        assert!(text.contains("turns it on"), "{text}");
        assert!(!text.contains('0'), "a coordinate crept in: {text}");
        for row in ["Sats", "Fix age", "UTC", "Date", "Lat", "Lon", "Alt"] {
            assert!(
                lines.iter().any(|l| l.starts_with(row) && l.ends_with('-')),
                "{row}: {text}"
            );
        }

        let mut state = State::default();
        state.position.status = gps::Status::Silent;
        let lines = lines_of(&state, Screen::Position);
        assert!(lines[0].ends_with("no data"), "{lines:?}");
        assert!(
            !lines.join(" ").contains("turns it on"),
            "the hint is for off"
        );

        // Searching: satellites in view but none used, a time, no fix.
        state.position.status = gps::Status::Searching;
        state.position.sats_used = Some(0);
        state.position.sats_in_view = Some(5);
        state.position.time = Some(gps::Time {
            hour: 1,
            minute: 33,
            second: 0,
        });
        let lines = lines_of(&state, Screen::Position);
        assert!(lines[0].ends_with("searching"), "{lines:?}");
        assert!(lines
            .iter()
            .any(|l| l.starts_with("Sats") && l.ends_with("0/5")));
        assert!(lines
            .iter()
            .any(|l| l.starts_with("UTC") && l.ends_with("01:33:00")));
        for row in ["Fix age", "Lat", "Lon", "Alt"] {
            assert!(
                lines.iter().any(|l| l.starts_with(row) && l.ends_with('-')),
                "{row}: {lines:?}"
            );
        }
    }

    /// A fix is drawn to six places with its sign, the altitude to a tenth,
    /// and the age in the unit that fits.
    #[test]
    fn the_position_screen_with_a_fix_shows_it() {
        let lines = lines_of(&populated(), Screen::Position);
        let text = lines.join(" ");
        assert!(lines[0].ends_with("fix"), "{text}");
        let ends = |label: &str, value: &str| {
            assert!(
                lines
                    .iter()
                    .any(|l| l.starts_with(label) && l.ends_with(value)),
                "{label} {value}: {text}"
            );
        };
        ends("Sats", "7/11");
        ends("Fix age", "3 s");
        ends("UTC", "13:47:09");
        ends("Date", "2026-09-08");
        ends("Lat", "47.376887");
        ends("Lon", "8.541694");
        ends("Alt", "408.0 m");
        assert!(!text.contains("turns it on"));

        // The other hemispheres, below sea level, a lost fix an hour old,
        // and a count in view with nothing used yet.
        let mut state = populated();
        state.position.fix = Some(gps::Fix {
            lat_udeg: -33_867_972,
            lon_udeg: -151_210_062,
            alt_dm: Some(-24),
        });
        state.position.fix_age_s = Some(3_600 * 26);
        state.position.sats_used = None;
        let lines = lines_of(&state, Screen::Position);
        let text = lines.join(" ");
        let ends = |label: &str, value: &str| {
            assert!(
                lines
                    .iter()
                    .any(|l| l.starts_with(label) && l.ends_with(value)),
                "{label} {value}: {text}"
            );
        };
        ends("Lat", "-33.867972");
        ends("Lon", "-151.210062");
        ends("Alt", "-2.4 m");
        ends("Fix age", "26 h");
        ends("Sats", "-/11");
        state.position.fix_age_s = Some(59);
        assert!(lines_of(&state, Screen::Position)
            .iter()
            .any(|l| l.ends_with("59 s")));
        state.position.fix_age_s = Some(60);
        assert!(lines_of(&state, Screen::Position)
            .iter()
            .any(|l| l.ends_with("1 min")));
        state.position.fix_age_s = Some(3_599);
        assert!(lines_of(&state, Screen::Position)
            .iter()
            .any(|l| l.ends_with("59 min")));
        // The widest a coordinate gets fits its row.
        state.position.fix = Some(gps::Fix {
            lat_udeg: -90_000_000,
            lon_udeg: -180_000_000,
            alt_dm: Some(-99_999),
        });
        state.position.fix_age_s = Some(u32::MAX);
        let mut out = Lines::new();
        state.position.lines(&mut out);
        assert!(!out.overflowed(), "a coordinate at its widest was cut");
    }

    /// The whole populated state reaches the screen: every field, changed on
    /// its own, changes some line of some screen.
    #[test]
    fn changing_any_field_changes_a_line() {
        type Mutation = (&'static str, fn(&mut State));
        let mutations: [Mutation; 37] = [
            ("host", |s| s.home.host = Host::Bluetooth),
            ("mode", |s| s.home.mode = Some(Mode::Node)),
            ("no node", |s| s.home.mode = None),
            ("unread", |s| s.home.unread = Some(3)),
            ("no messages", |s| s.home.unread = None),
            ("talking", |s| s.home.talking = false),
            ("home air", |s| s.home.air = Air::Off),
            ("rx", |s| s.home.rx_count += 1),
            ("tx", |s| s.home.tx_count += 1),
            ("rssi", |s| s.home.last_rssi_dbm = Some(-45)),
            ("snr", |s| s.home.last_snr_quarter_db = Some(-2)),
            ("uptime", |s| s.home.uptime_s = Some(90_000)),
            ("battery", |s| s.home.battery = None),
            ("charger", |s| s.home.charger = battery::Charger::Full),
            ("frequency", |s| s.radio.config.frequency_hz = 916_000_000),
            ("bandwidth", |s| s.radio.config.bandwidth_hz = 250_000),
            ("sf", |s| s.radio.config.spreading_factor = 9),
            ("cr", |s| s.radio.config.coding_rate = 8),
            ("power", |s| s.radio.config.tx_power_dbm = 2),
            ("preamble", |s| s.radio.config.preamble_symbols = 12),
            ("sync", |s| s.radio.config.sync_word = 0x2b),
            ("crc", |s| s.radio.config.crc = false),
            ("header", |s| s.radio.config.implicit_header = true),
            ("iq", |s| s.radio.config.invert_iq = true),
            ("correction", |s| s.radio.config.correct_reference = false),
            ("radio air", |s| {
                s.radio.air = Air::Refused(ConfigError::PowerAboveModuleRating)
            }),
            ("tnc", |s| s.radio.tnc = false),
            ("link", |s| s.bluetooth.link = Link::Connected),
            ("bonded", |s| s.bluetooth.bonded = 3),
            ("gps status", |s| s.position.status = gps::Status::Searching),
            ("sats used", |s| s.position.sats_used = Some(8)),
            ("sats in view", |s| s.position.sats_in_view = None),
            ("fix", |s| s.position.fix = None),
            ("latitude", |s| {
                s.position.fix.as_mut().unwrap().lat_udeg += 1;
            }),
            ("altitude", |s| {
                s.position.fix.as_mut().unwrap().alt_dm = None;
            }),
            ("fix age", |s| s.position.fix_age_s = Some(4)),
            ("time", |s| s.position.time = None),
        ];
        let base = populated();
        let all = |s: &State| -> Vec<Vec<String>> {
            Screen::ALL.iter().map(|&sc| lines_of(s, sc)).collect()
        };
        let reference = all(&base);
        for (name, mutate) in mutations {
            let mut changed = base;
            mutate(&mut changed);
            assert_ne!(all(&changed), reference, "changing {name} changed no line");
        }
        // And the rest, which change the picture rather than a line.
        let system: [Mutation; 9] = [
            ("version", |s| s.system.version = "9.9.9"),
            ("build", |s| s.system.build = "fffffff-dirty"),
            ("serial", |s| s.system.serial = None),
            ("identity", |s| s.system.identity = Identity::Unsigned),
            ("node", |s| s.system.node = None),
            ("lxmf address", |s| {
                s.system.node.as_mut().unwrap().lxmf[15] ^= 1
            }),
            ("rns identity", |s| {
                s.system.node.as_mut().unwrap().identity[0] ^= 1
            }),
            ("announce gap", |s| s.system.announce_gap_s = Some(45)),
            ("ram", |s| s.system.free_ram = Some(1)),
        ];
        for (name, mutate) in system {
            let mut changed = base;
            mutate(&mut changed);
            assert_ne!(
                lines_of(&changed, Screen::System),
                lines_of(&base, Screen::System),
                "{name}"
            );
        }
        // The conversations, which only an image with the node draws.
        #[cfg(feature = "node")]
        for (name, mutate) in [
            (
                "conversation",
                (|s| s.conversations.rows[0].as_mut().unwrap().unread = false) as fn(&mut State),
            ),
            ("no conversations", |s| {
                s.conversations = Conversations::default()
            }),
        ] {
            let mut changed = base;
            mutate(&mut changed);
            assert_ne!(all(&changed), reference, "changing {name} changed no line");
        }
        for (name, mutate) in [
            ("name", (|s| s.bluetooth.name = None) as fn(&mut State)),
            ("passkey", |s| s.bluetooth.passkey = Some(29_717)),
        ] {
            let mut changed = base;
            mutate(&mut changed);
            assert_ne!(
                frame_of(&changed, Screen::Bluetooth).as_bytes(),
                frame_of(&base, Screen::Bluetooth).as_bytes(),
                "{name}"
            );
        }
    }

    /// **The Radio screen says which band it is on.** The band decides the
    /// connector and the tables, so it is the first row; a frequency in
    /// neither band is a dash, not a guess.
    #[test]
    fn the_radio_screen_names_the_band() {
        let first = |state: &State| lines_of(state, Screen::Radio)[0].clone();
        assert!(first(&populated()).starts_with("Band"));
        assert!(first(&populated()).ends_with("sub-GHz"));
        assert!(first(&State::default()).ends_with("sub-GHz"));

        let mut hf = populated();
        hf.radio.config = crate::lr1121::config::BENCH_2G4;
        let lines = lines_of(&hf, Screen::Radio);
        assert!(lines[0].ends_with("2.4 GHz"), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("Freq") && l.ends_with("2478.000 MHz")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("BW") && l.ends_with("812.5 kHz")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("Power") && l.ends_with("11 dBm")),
            "{lines:?}"
        );
        let mut out = Lines::new();
        hf.radio.lines(&mut out);
        assert!(!out.overflowed(), "a 2.4 GHz row was cut");

        let mut nowhere = populated();
        nowhere.radio.config.frequency_hz = 1_500_000_000;
        assert!(first(&nowhere).ends_with('-'));
    }

    /// The 2.4 GHz bandwidths are shown exactly, and the sub-GHz ones as
    /// they always were.
    #[test]
    fn bandwidths_are_shown_to_the_places_they_need() {
        let fmt = |hz| {
            let mut t = Text::<LINE_CHARS>::new();
            kilohertz(&mut t, hz);
            t.as_str().to_string()
        };
        assert_eq!(fmt(125_000), "125.0 kHz");
        assert_eq!(fmt(62_500), "62.5 kHz");
        assert_eq!(fmt(41_700), "41.7 kHz");
        assert_eq!(fmt(31_250), "31.25 kHz");
        assert_eq!(fmt(203_125), "203.125 kHz");
        assert_eq!(fmt(406_250), "406.25 kHz");
        assert_eq!(fmt(812_500), "812.5 kHz");
        assert_eq!(fmt(7_800), "7.8 kHz");
    }

    /// A refused configuration says why, in words that fit.
    #[test]
    fn a_refusal_carries_its_reason() {
        let mut state = populated();
        state.radio.air = Air::Refused(ConfigError::PowerAboveModuleRating);
        let lines = lines_of(&state, Screen::Radio);
        let text = lines.join(" ");
        assert!(text.contains("refused"));
        assert!(
            text.contains("power is above the module's 20 dBm rating"),
            "{text}"
        );
        assert!(
            lines.len() > LAYOUT.visible_lines(),
            "long enough to scroll"
        );
    }

    /// The radio screen is longer than the panel, so it scrolls and the
    /// scrollbar shows.
    #[test]
    fn the_radio_screen_scrolls() {
        let state = populated();
        let mut nav = on_radio();
        let mut top = Frame::new();
        render(&mut top, &mut nav, &state);
        assert!(lines_of(&state, Screen::Radio).len() > LAYOUT.visible_lines());
        let bar = |f: &Frame| {
            (LAYOUT.content_top..LAYOUT.content_bottom).any(|y| f.pixel(sh1107::WIDTH - 1, y))
        };
        assert!(bar(&top), "a scrollbar");
        nav.handle(Input::Down);
        let mut down = Frame::new();
        render(&mut down, &mut nav, &state);
        assert_eq!(nav.scroll(), 1);
        assert_ne!(top.as_bytes(), down.as_bytes());
    }

    /// Uptime formats as read, and stays inside the row past a day.
    #[test]
    fn uptime_is_readable_at_every_scale() {
        let fmt = |secs| {
            let mut t = Text::<LINE_CHARS>::new();
            uptime(&mut t, secs);
            t.as_str().to_string()
        };
        assert_eq!(fmt(0), "0:00:00");
        assert_eq!(fmt(59), "0:00:59");
        assert_eq!(fmt(3_723), "1:02:03");
        assert_eq!(fmt(86_399), "23:59:59");
        assert_eq!(fmt(86_400), "1d 00:00");
        assert_eq!(fmt(90_061), "1d 01:01");
        assert_eq!(fmt(u32::MAX), "49710d 06:28");
        assert!("Uptime".len() + fmt(u32::MAX).len() <= LINE_CHARS);
    }

    /// Wrapping breaks at spaces, never leaves a line too long, and loses no
    /// words.
    #[test]
    fn wrapping_keeps_every_word_and_every_line_short() {
        let text = "power is above the module's 20 dBm rating";
        let lines: Vec<&str> = wrap(text, LINE_CHARS).collect();
        assert!(lines.iter().all(|l| l.len() <= LINE_CHARS), "{lines:?}");
        assert!(lines
            .iter()
            .all(|l| !l.starts_with(' ') && !l.ends_with(' ')));
        assert_eq!(lines.join(" "), text);
        assert_eq!(wrap("", 10).count(), 0);
        assert_eq!(wrap("short", 10).collect::<Vec<_>>(), ["short"]);
        // A word longer than the width is cut rather than looping forever.
        let long: Vec<&str> = wrap("abcdefghijklmnopqrstuvwxyz", 10).collect();
        assert_eq!(long, ["abcdefghij", "klmnopqrst", "uvwxyz"]);
    }

    /// A row's value sits flush right.
    #[test]
    fn rows_are_right_aligned() {
        let mut out = Lines::new();
        out.row("RX", "42");
        assert_eq!(out.get(0).unwrap().len(), LINE_CHARS);
        assert!(out.get(0).unwrap().ends_with("42"));
        assert!(out.get(0).unwrap().starts_with("RX "));
    }

    /// The free-RAM arithmetic does not wrap when the stack has eaten the gap.
    #[test]
    fn the_announce_gap_reads_at_every_scale() {
        let gap = |s| {
            let mut v = Text::<LINE_CHARS>::new();
            duration(&mut v, s);
            v.as_str().to_owned()
        };
        assert_eq!(gap(45), "45 s");
        assert_eq!(gap(60), "1m00s");
        assert_eq!(gap(252), "4m12s");
        assert_eq!(gap(3_599), "59m59s");
        assert_eq!(gap(3_600), "1h00m");
        assert_eq!(gap(11_100), "3h05m");
        // Hours stay one digit at the slowest configuration the radio
        // accepts (`node::announce` holds it under ten), and that still fits
        // beside its label with a space between.
        assert!("Announce 1 per ".len() + gap(9 * 3_600 + 59 * 60).len() <= LINE_CHARS);
    }

    #[test]
    fn free_ram_is_the_gap_or_nothing() {
        assert_eq!(free_ram(0x2000_1000, 0x2003_F000), Some(0x3E000));
        assert_eq!(free_ram(0x2000_1000, 0x2000_1000), Some(0));
        assert_eq!(free_ram(0x2000_1000, 0x2000_0FFF), None);
    }

    /// The pairing box appears over any screen while a passkey is set.
    #[test]
    fn the_passkey_is_shown_over_every_screen() {
        let mut pairing = populated();
        pairing.bluetooth.passkey = Some(29_717);
        for screen in Screen::ALL {
            let with = frame_of(&pairing, screen);
            let without = frame_of(&populated(), screen);
            assert_ne!(with.as_bytes(), without.as_bytes(), "{screen:?}");
            // The box is a filled block inside the content area.
            let mid = LAYOUT.content_top + LAYOUT.content_h() / 2;
            let lit = (LAYOUT.content_left..sh1107::WIDTH - LAYOUT.content_left)
                .filter(|&x| with.pixel(x, mid))
                .count();
            assert!(lit > 80, "{screen:?}: box row has {lit} lit");
        }
    }

    /// The title bar carries the battery and the board's name, and only
    /// when known. What Bluetooth is doing is not in it: that is the
    /// Bluetooth screen's, and a badge that changed with every phone that
    /// came and went was the one thing on the bar nobody needed on every
    /// screen.
    #[test]
    fn the_title_bar_corners_are_the_battery_and_the_name() {
        let mut bare = State::default();
        let mut with = populated();
        with.bluetooth.link = Link::Connected;
        let mut chrome_bare = Frame::new();
        chrome_bare.fill(false);
        monopanel::title_bar(&mut chrome_bare, "", Screen::Home.title(), "");
        let mut chrome_with = Frame::new();
        chrome_with.fill(false);
        monopanel::title_bar(&mut chrome_with, "91%", Screen::Home.title(), "7F23");
        let row = |f: &Frame| {
            (0..sh1107::WIDTH)
                .map(|x| f.pixel(x, 4))
                .collect::<Vec<_>>()
        };
        assert_eq!(row(&frame_of(&bare, Screen::Home)), row(&chrome_bare));
        assert_eq!(row(&frame_of(&with, Screen::Home)), row(&chrome_with));
        for link in [Link::Absent, Link::Advertising, Link::Connected] {
            bare.bluetooth.link = link;
            assert_eq!(
                row(&frame_of(&bare, Screen::Home)),
                row(&chrome_bare),
                "{link:?} reached the title bar"
            );
        }
        bare.name = Some(*b"BEEF");
        assert_ne!(row(&frame_of(&bare, Screen::Home)), row(&chrome_bare));
    }

    /// **Redraw stays incremental.** The same state redrawn dirties nothing;
    /// a screen change dirties the pages that changed and not the ones that
    /// did not.
    #[test]
    fn a_redraw_costs_only_the_pages_that_changed() {
        let state = populated();
        let mut nav = ui::nav();
        let mut live = Frame::new();
        let mut scratch = Frame::new();
        render(&mut scratch, &mut nav, &state);
        live.copy_from(&scratch);
        for page in 0..sh1107::PAGES {
            live.mark_sent(page);
        }
        assert!(live.is_clean());

        // A tick with nothing new.
        render(&mut scratch, &mut nav, &state);
        live.copy_from(&scratch);
        assert!(live.is_clean(), "an unchanged screen dirtied a page");

        // One counter ticks: the line it is on, and nothing else.
        let mut ticked = state;
        ticked.home.rx_count += 1;
        render(&mut scratch, &mut nav, &ticked);
        live.copy_from(&scratch);
        let dirty = (0..sh1107::PAGES).filter(|&p| live.is_dirty(p)).count();
        assert!((1..=2).contains(&dirty), "one line touched {dirty} pages");
        for page in 0..sh1107::PAGES {
            live.mark_sent(page);
        }

        // A screen change between two short screens: the chrome and the
        // content, but the blank rows between the last line and the icon
        // strip stay as they were. (Home to Radio does redraw every page,
        // because the radio screen fills every content row; what this
        // guards is that nothing marks the whole panel dirty on principle.)
        while nav.current() != Screen::Bluetooth {
            nav.handle(Input::Right);
        }
        render(&mut scratch, &mut nav, &ticked);
        live.copy_from(&scratch);
        for page in 0..sh1107::PAGES {
            live.mark_sent(page);
        }
        assert_eq!(nav.current(), Screen::Bluetooth);
        nav.handle(Input::Right);
        render(&mut scratch, &mut nav, &ticked);
        live.copy_from(&scratch);
        let dirty = (0..sh1107::PAGES).filter(|&p| live.is_dirty(p)).count();
        assert!(dirty < sh1107::PAGES, "a screen change flushed every page");
        assert!(dirty > 2, "a screen change touched only {dirty} pages");
    }

    // ---- the editor -------------------------------------------------------

    use crate::edit::{Editor, Field, Lock};

    fn editor_lines(editor: &Editor) -> Vec<String> {
        let mut out = Lines::new();
        editor.lines(&mut out);
        (0..out.len())
            .map(|n| out.get(n).unwrap().to_string())
            .collect()
    }

    /// On a panel with fewer rows than an editor has lines, every editor
    /// still fits, and a refused one still shows the value and the whole of
    /// its reason (issue #21). The board's own panel is the case above.
    #[test]
    fn every_editor_fits_a_short_panel_and_keeps_its_refusal() {
        let short = Layout::of(128, 64).visible_lines();
        assert_eq!(short, 4);
        let configs = [populated().radio.config, crate::lr1121::config::BENCH_2G4];
        let mut refusals = 0;
        for (config, field) in configs.iter().flat_map(|c| Field::ALL.map(|f| (*c, f))) {
            for pass in ["fresh", "up", "down", "changed"] {
                let mut e = Editor::open(field, config);
                match pass {
                    "up" => {
                        (0..12).for_each(|_| e.up());
                        e.confirm();
                    }
                    "down" => {
                        (0..50).for_each(|_| e.down());
                        e.confirm();
                    }
                    "changed" => e.up(),
                    _ => {}
                }
                for rows in [short, LAYOUT.visible_lines()] {
                    let mut out = Lines::new();
                    e.lines_in(&mut out, rows);
                    let lines: Vec<&str> = (0..out.len()).map(|n| out.get(n).unwrap()).collect();
                    assert!(lines.len() <= rows, "{field:?} {pass} in {rows}: {lines:?}");
                    assert!(!out.overflowed(), "{field:?} {pass}: a line was cut");
                    assert!(
                        lines.iter().any(|l| l.starts_with("Now")),
                        "{field:?} {pass}: {lines:?}"
                    );
                    if let Some(reason) = e.refused() {
                        refusals += 1;
                        let shown = lines.join(" ");
                        assert!(
                            shown.contains(reason.message()),
                            "{field:?} {pass} in {rows}: reason cut: {lines:?}"
                        );
                    }
                }
            }
        }
        assert!(refusals > 0, "no editor was driven to a refusal");
        for lock in [Lock::Usb, Lock::Bluetooth] {
            let mut out = Lines::new();
            lock.lines_in(&mut out, short);
            assert!(out.len() <= short, "{lock:?}");
            assert!(out.get(0).unwrap().contains("has the radio"), "{lock:?}");
            let shown = (0..out.len())
                .map(|n| out.get(n).unwrap())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(shown.contains("settings here."), "{lock:?}: {shown}");
        }
        // A refusal keeps what was not sent and the whole of why.
        for refusal in REFUSALS {
            let mut out = Lines::new();
            refusal.lines_in(&mut out, short);
            assert!(out.len() <= short, "{refusal:?}");
            let shown = (0..out.len())
                .map(|n| out.get(n).unwrap())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                shown.starts_with(refusal.headline()),
                "{refusal:?}: {shown}"
            );
            assert!(shown.ends_with(refusal.reason()), "{refusal:?}: {shown}");
        }
    }

    const REFUSALS: [Refusal; 4] = [
        Refusal::NoFix,
        Refusal::Full,
        Refusal::TooLong,
        Refusal::NothingFailed,
    ];

    /// Whole groups go, lowest priority first, the later of a tie first,
    /// and what stays keeps its order.
    #[test]
    fn fit_drops_whole_groups_lowest_first() {
        let fitted = |rows| {
            let mut fit = Fit::new();
            fit.line(Fit::CONTEXT, "a");
            fit.line(Fit::SPACING, "");
            fit.wrapped(
                Fit::REASON,
                "one two three four five six seven eight nine ten",
            );
            fit.line(Fit::CONTEXT, "b");
            let mut out = Lines::new();
            fit.emit(&mut out, rows);
            (0..out.len())
                .map(|n| out.get(n).unwrap().to_string())
                .collect::<Vec<_>>()
        };
        let reason = ["one two three four", "five six seven eight", "nine ten"];
        assert_eq!(fitted(6), ["a", "", reason[0], reason[1], reason[2], "b"]);
        assert_eq!(fitted(5), ["a", reason[0], reason[1], reason[2], "b"]);
        assert_eq!(fitted(4), ["a", reason[0], reason[1], reason[2]]);
        assert_eq!(fitted(3), reason);
        // Two rows cannot hold the reason, and half of one is not shown.
        assert!(fitted(2).is_empty());
    }

    /// Every editor, fresh and refused, fits the panel in width and height,
    /// on either band. It cannot scroll, so a line past the eleventh would
    /// be invisible.
    #[test]
    fn every_editor_fits_without_scrolling() {
        let width = LAYOUT.line_width();
        let configs = [populated().radio.config, crate::lr1121::config::BENCH_2G4];
        for (config, field) in configs.iter().flat_map(|c| Field::ALL.map(|f| (*c, f))) {
            let mut e = Editor::open(field, config);
            for pass in ["fresh", "refused up", "refused down"] {
                match pass {
                    // Drive every field to a refusal it can reach; two of
                    // them cannot, and stay legal.
                    "refused up" => {
                        for _ in 0..12 {
                            e.up();
                        }
                        e.confirm();
                    }
                    "refused down" => {
                        for _ in 0..50 {
                            e.down();
                        }
                        e.confirm();
                    }
                    _ => {}
                }
                let lines = editor_lines(&e);
                assert!(
                    lines.len() <= LAYOUT.visible_lines(),
                    "{field:?} {pass} on {} Hz: {} lines: {lines:?}",
                    config.frequency_hz,
                    lines.len()
                );
                let mut out = Lines::new();
                e.lines(&mut out);
                assert!(!out.overflowed(), "{field:?} {pass}: a line was cut");
                for line in &lines {
                    assert!(
                        line.len() <= LINE_CHARS && font::width_of(line) <= width,
                        "{field:?} {pass}: `{line}`"
                    );
                }
            }
        }
        // Every refusal's reason fits under an editor: the eight lines the
        // editor needs leave three for the reason.
        for reason in [
            ConfigError::FrequencyOutOfBand,
            ConfigError::UnsupportedBandwidth,
            ConfigError::BandwidthNotInBand,
            ConfigError::SpreadingFactorOutOfRange,
            ConfigError::CodingRateOutOfRange,
            ConfigError::PowerUnreachable,
            ConfigError::PowerAboveModuleRating,
            ConfigError::PowerAbove2G4Rating,
            ConfigError::PreambleTooShort,
        ] {
            let lines = wrap(reason.message(), LINE_CHARS).count();
            assert!(lines <= 3, "{reason:?} wraps to {lines} lines");
        }
        // And the notices.
        for lock in [Lock::Usb, Lock::Bluetooth] {
            let mut out = Lines::new();
            lock.lines(&mut out);
            assert!(!out.overflowed(), "{lock:?}: a line was cut");
            assert!(out.len() <= LAYOUT.visible_lines());
            for n in 0..out.len() {
                assert!(out.get(n).unwrap().len() <= LINE_CHARS);
            }
        }
        for refusal in REFUSALS {
            let mut out = Lines::new();
            refusal.lines(&mut out);
            assert!(!out.overflowed(), "{refusal:?}: a line was cut");
            assert!(out.len() <= LAYOUT.visible_lines());
            assert!(refusal.headline().len() <= LINE_CHARS, "{refusal:?}");
        }
    }

    // ---- the position reply -----------------------------------------------

    fn position_reply(position: &gps::Position) -> Result<String, Refusal> {
        let mut out = String::new();
        position_text(position, &mut out).map(|()| out)
    }

    fn with_fix(status: gps::Status, fix_age_s: Option<u32>) -> gps::Position {
        gps::Position {
            status,
            fix: Some(gps::Fix {
                lat_udeg: 47_376_887,
                lon_udeg: -6_505_620,
                alt_dm: Some(4_080),
            }),
            fix_age_s,
            ..gps::Position::OFF
        }
    }

    #[test]
    fn a_fresh_fix_is_sent_as_where_the_board_is() {
        assert_eq!(
            position_reply(&with_fix(gps::Status::Fix, Some(0))),
            Ok("My position: 47.376887, -6.505620".into())
        );
    }

    #[test]
    fn a_stale_fix_is_sent_with_its_age() {
        assert_eq!(
            position_reply(&with_fix(gps::Status::Searching, Some(12 * 60 + 5))),
            Ok("My position: 47.376887, -6.505620 (fix 12 min old)".into())
        );
        // The receiver switched off keeps the fix, aged; an unknown age is a
        // dash, never a zero.
        assert_eq!(
            position_reply(&with_fix(gps::Status::Off, None)),
            Ok("My position: 47.376887, -6.505620 (fix - old)".into())
        );
    }

    #[test]
    fn no_fix_is_refused_never_sent_as_zeros() {
        for status in [
            gps::Status::Off,
            gps::Status::Silent,
            gps::Status::Searching,
        ] {
            let none = gps::Position {
                status,
                ..gps::Position::OFF
            };
            assert_eq!(position_reply(&none), Err(Refusal::NoFix));
        }
    }

    #[test]
    fn the_position_reply_always_fits_a_packet() {
        // The widest coordinates and the widest age.
        let mut far = with_fix(gps::Status::Silent, Some(u32::MAX));
        far.fix = Some(gps::Fix {
            lat_udeg: -90_000_000,
            lon_udeg: -180_000_000,
            alt_dm: None,
        });
        let text = position_reply(&far).unwrap();
        assert_eq!(
            text,
            "My position: -90.000000, -180.000000 (fix 1193046 h old)"
        );
        assert!(text.len() < 64);
    }

    /// The editor says what the value was and what it is now, in the same
    /// words the Radio screen uses for it.
    #[test]
    fn the_editor_shows_was_and_now_in_the_screens_words() {
        let mut e = Editor::open(Field::TxPower, populated().radio.config);
        e.up();
        let lines = editor_lines(&e);
        assert_eq!(lines[0], "Was   17 dBm");
        assert_eq!(lines[1], "Now   18 dBm");
        let radio = lines_of(&populated(), Screen::Radio);
        assert!(radio.iter().any(|l| l.ends_with("17 dBm")));

        let mut e = Editor::open(Field::Bandwidth, populated().radio.config);
        e.up(); // the other band's 203.125 kHz sits between 125 and 250
        let lines = editor_lines(&e);
        assert_eq!(lines[0], "Was   125.0 kHz");
        assert_eq!(lines[1], "Now   203.125 kHz");
        e.up();
        assert_eq!(editor_lines(&e)[1], "Now   250.0 kHz");
        let e = Editor::open(Field::CodingRate, populated().radio.config);
        assert_eq!(editor_lines(&e)[1], "Now   4/5");
        let e = Editor::open(Field::SpreadingFactor, populated().radio.config);
        assert_eq!(editor_lines(&e)[1], "Now   8");
    }

    /// **The band editor says where the board lands.** Was and Now name the
    /// band and its connector; once the band differs, the two lines under
    /// them are the frequency, bandwidth and power it will be set to, in the
    /// Radio screen's words. Unchanged, it reads like any other stepper.
    #[test]
    fn the_band_editor_shows_the_band_the_connector_and_the_landing() {
        let config = populated().radio.config;
        let e = Editor::open(Field::Band, config);
        let lines = editor_lines(&e);
        assert_eq!(lines[0], "Was   sub-GHz, SMA");
        assert_eq!(lines[1], "Now   sub-GHz, SMA");
        assert_eq!(lines[2], "");
        assert_eq!(lines[3], "Up/Down: change");

        let mut e = Editor::open(Field::Band, config);
        e.up();
        let lines = editor_lines(&e);
        assert_eq!(
            lines,
            [
                "Was   sub-GHz, SMA",
                "Now   2.4 GHz, u.FL",
                "Sets  2478.000 MHz",
                "406.25 kHz, 11 dBm",
                "Up/Down: change",
                "OK: set Back: cancel",
            ]
        );

        let mut e = Editor::open(Field::Band, crate::lr1121::config::BENCH_2G4);
        e.down();
        let lines = editor_lines(&e);
        assert_eq!(lines[1], "Now   sub-GHz, SMA");
        assert_eq!(lines[2], "Sets  915.000 MHz");
        assert_eq!(lines[3], "125.0 kHz, 14 dBm");

        let lost = RadioConfig {
            frequency_hz: 868_000_000,
            ..config
        };
        assert_eq!(editor_lines(&Editor::open(Field::Band, lost))[0], "Was   -");
    }

    /// A band editor with its landing showing and a refusal under it -- the
    /// longest it gets -- still fits the panel without scrolling.
    #[test]
    fn a_refused_band_editor_fits() {
        let odd = RadioConfig {
            coding_rate: 9,
            ..populated().radio.config
        };
        let mut e = Editor::open(Field::Band, odd);
        e.up();
        assert!(matches!(e.confirm(), crate::edit::Confirm::Refused(_)));
        // Bound by the longest a reason may wrap to (three lines, held by
        // `every_editor_fits_without_scrolling`), not by this one's.
        let reason = wrap(e.refused().unwrap().message(), LINE_CHARS).count();
        let lines = editor_lines(&e);
        assert_eq!(lines[2], "Sets  2478.000 MHz", "the landing is shown");
        assert!(
            lines.len() - reason + 3 <= LAYOUT.visible_lines(),
            "{lines:?}"
        );
        for line in &lines {
            assert!(line.len() <= LINE_CHARS, "`{line}`");
        }
    }

    /// The frequency editor's cursor sits under the digit it edits, on both
    /// sides of the decimal point -- and on both bands, where the value is a
    /// digit wider. A sub-GHz frequency's thousands digit is a blank, and
    /// the cursor sits under the blank.
    #[test]
    fn the_cursor_is_under_the_digit_it_edits() {
        use crate::edit::FREQ_DIGITS;
        let cases = [
            (populated().radio.config, "Now    915.000 MHz", " 915.000"),
            (
                crate::lr1121::config::BENCH_2G4,
                "Now   2478.000 MHz",
                "2478.000",
            ),
        ];
        for (config, now, value) in cases {
            let mut e = Editor::open(Field::Frequency, config);
            // Where each of the seven digits is on the line: after the
            // label, skipping the point.
            let columns: Vec<usize> = value
                .char_indices()
                .filter(|(_, c)| *c != '.')
                .map(|(i, _)| EDIT_VALUE_COL + i)
                .collect();
            assert_eq!(columns.len(), FREQ_DIGITS);
            for (expect, &column) in columns.iter().enumerate() {
                let lines = editor_lines(&e);
                assert_eq!(lines[1], now);
                let caret = lines[2].find('_').expect("a cursor line");
                assert_eq!(caret, column, "cursor {expect} on {now}");
                let under = lines[1].as_bytes()[caret];
                assert!(
                    under.is_ascii_digit() || (expect == 0 && under == b' '),
                    "cursor {expect} is under `{}`",
                    under as char
                );
                e.right();
            }
        }
        // Editing the blank fills it: the leading zero was always there.
        let mut e = Editor::open(Field::Frequency, populated().radio.config);
        e.up();
        assert_eq!(editor_lines(&e)[1], "Now   1915.000 MHz");
        assert_eq!(editor_lines(&e)[0], "Was    915.000 MHz");
    }

    /// A refused value says so, with the same reason the host path gives.
    #[test]
    fn a_refused_editor_says_why() {
        let mut e = Editor::open(Field::TxPower, populated().radio.config);
        for _ in 0..5 {
            e.up(); // 17 -> 22
        }
        assert_eq!(
            e.confirm(),
            crate::edit::Confirm::Refused(ConfigError::PowerAboveModuleRating)
        );
        let text = editor_lines(&e).join(" ");
        assert!(text.contains("Now   22 dBm"), "{text}");
        assert!(text.contains("Refused:"), "{text}");
        assert!(
            text.contains(ConfigError::PowerAboveModuleRating.message()),
            "{text}"
        );
        e.down();
        assert!(!editor_lines(&e).join(" ").contains("Refused"));
    }

    /// The notice names the host that has the radio.
    #[test]
    fn the_notice_names_the_host() {
        let mut out = Lines::new();
        Lock::Usb.lines(&mut out);
        assert!(out.get(0).unwrap().starts_with("USB"));
        let mut out = Lines::new();
        Lock::Bluetooth.lines(&mut out);
        assert!(out.get(0).unwrap().starts_with("Phone"));
        assert_eq!(Host::None.lock(), None);
        assert_eq!(Host::Usb.lock(), Some(Lock::Usb));
        assert_eq!(Host::Bluetooth.lock(), Some(Lock::Bluetooth));
    }

    /// Rendering with an editor open draws the editor, the field's title,
    /// and the same chrome; closing it puts the screen back exactly.
    #[test]
    fn rendering_an_editor_replaces_the_content_and_nothing_else() {
        let state = populated();
        let mut nav = on_radio();
        let mut before = Frame::new();
        render(&mut before, &mut nav, &state);
        nav.edit(Editor::open(Field::Frequency, state.radio.config));
        let mut editing = Frame::new();
        render(&mut editing, &mut nav, &state);
        assert_ne!(before.as_bytes(), editing.as_bytes());
        // The icon strip is the same.
        for y in LAYOUT.content_bottom..sh1107::HEIGHT {
            for x in 0..sh1107::WIDTH {
                assert_eq!(
                    before.pixel(x, y),
                    editing.pixel(x, y),
                    "strip at ({x}, {y})"
                );
            }
        }
        nav.handle(Input::Back);
        let mut after = Frame::new();
        render(&mut after, &mut nav, &state);
        assert_eq!(before.as_bytes(), after.as_bytes());

        nav.notice(Lock::Usb);
        let mut notice = Frame::new();
        render(&mut notice, &mut nav, &state);
        assert_ne!(notice.as_bytes(), before.as_bytes());
        assert_ne!(notice.as_bytes(), editing.as_bytes());
    }

    /// What the simulator and the board draw is the same composition.
    #[test]
    fn render_is_page_plus_the_pairing_box() {
        let state = populated();
        let mut nav = ui::nav();
        let mut got = Frame::new();
        render(&mut got, &mut nav, &state);

        let mut lines = Lines::new();
        state.lines(Screen::Home, &mut lines);
        let strs = lines.as_strs();
        let mut want_nav = ui::nav();
        let mut want = Frame::new();
        page(
            &mut want,
            &mut want_nav,
            "91%",
            "7F23",
            &strs[..lines.len()],
        );
        assert_eq!(got.as_bytes(), want.as_bytes());
    }
}
