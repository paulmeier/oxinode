//! Conversations written down, for the Messages screen and a thread to draw
//! from without a board.
//!
//! On the board the messages are the node's, and
//! [`render_with`](oxinode_core::screens::render_with) borrows them for the
//! length of a drawing. Here they are read from a text file -- the bundled
//! [`bench`] set, or one given on the command line -- and the scene borrows
//! them the same way. The format is in the bundled file's header; one line a
//! message, so a conversation seen on the bench can be written down and
//! looked at again.

use oxinode_core::screens::{
    ConversationRow, Conversations, Delivery, Direction, Entry, PeerName, Read, Thread, Trust,
    CONVERSATIONS,
};

/// The bundled conversations: `sim/conversations/bench.txt`.
pub const BENCH: &str = include_str!("../conversations/bench.txt");

/// One message, as written down.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
    pub direction: Direction,
    pub age_s: Option<u32>,
    pub content: String,
    pub attachment: bool,
    pub unread: bool,
}

/// One conversation: a peer, and its messages oldest first.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Recorded {
    pub peer: [u8; 16],
    pub name: Option<String>,
    pub messages: Vec<Message>,
}

/// The bundled conversations, parsed.
pub fn bench() -> Vec<Recorded> {
    parse(BENCH).expect("the bundled conversations parse")
}

/// Read conversations from their text. See the bundled file for the format.
pub fn parse(text: &str) -> Result<Vec<Recorded>, String> {
    let mut out: Vec<Recorded> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_end();
        let at = |e: String| format!("line {}: {e}", n + 1);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("peer ") {
            let (hex, name) = rest.split_once(' ').unwrap_or((rest, ""));
            out.push(Recorded {
                peer: address(hex).map_err(at)?,
                name: (!name.trim().is_empty()).then(|| name.trim().to_string()),
                messages: Vec::new(),
            });
            continue;
        }
        let Some(conversation) = out.last_mut() else {
            return Err(at("a message before any `peer` line".into()));
        };
        conversation.messages.push(message(line).map_err(at)?);
    }
    Ok(out)
}

fn address(hex: &str) -> Result<[u8; 16], String> {
    if hex.len() != 32 {
        return Err(format!("`{hex}` is not 32 hex digits"));
    }
    let mut out = [0; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .map_err(|_| format!("`{hex}` is not hex"))?;
    }
    Ok(out)
}

fn message(line: &str) -> Result<Message, String> {
    let mut words = line.splitn(4, ' ');
    let mut next = |what: &str| {
        words
            .next()
            .filter(|w| !w.is_empty())
            .ok_or_else(|| format!("no {what}"))
    };
    let direction = next("direction")?;
    let age = next("age")?;
    let flags = next("state")?;
    let content = words.next().unwrap_or("").replace("\\n", "\n");
    let age_s = match age {
        "-" => None,
        n => Some(n.parse().map_err(|_| format!("`{n}` is not an age"))?),
    };
    let mut flags = flags.split(',');
    let state = flags.next().unwrap_or("");
    let direction = match (direction, state) {
        ("<", "verified") => Direction::Received(Trust::Verified),
        ("<", "unverified") => Direction::Received(Trust::Unverified),
        ("<", "bad") => Direction::Received(Trust::Failed),
        (">", "queued") => Direction::Sent(Delivery::Queued),
        (">", "sent") => Direction::Sent(Delivery::Sent),
        (">", "delivered") => Direction::Sent(Delivery::Delivered),
        (">", "failed") => Direction::Sent(Delivery::Failed),
        (d, s) => return Err(format!("`{d} {s}` is not a direction and a state")),
    };
    let (mut unread, mut attachment) = (false, false);
    for flag in flags {
        match flag {
            "unread" if matches!(direction, Direction::Received(_)) => unread = true,
            "attachment" => attachment = true,
            other => return Err(format!("`{other}` is not a flag here")),
        }
    }
    Ok(Message {
        direction,
        age_s,
        content,
        attachment,
        unread,
    })
}

impl Recorded {
    /// Its messages as the thread draws them.
    pub fn entries(&self) -> Vec<Entry<'_>> {
        self.messages
            .iter()
            .map(|m| Entry {
                direction: m.direction,
                age_s: m.age_s,
                title: "",
                content: &m.content,
                attachment: m.attachment,
                unread: m.unread,
            })
            .collect()
    }

    /// The thread, over `entries` from [`Recorded::entries`].
    pub fn thread<'a>(&self, entries: &'a [Entry<'a>]) -> Thread<'a> {
        Thread {
            peer: self.peer,
            name: self.name.as_deref().map(PeerName::new),
            entries,
        }
    }

    /// Its line on the Messages screen.
    pub fn row(&self) -> ConversationRow {
        ConversationRow {
            peer: self.peer,
            name: self.name.as_deref().map(PeerName::new),
            unread: self.messages.iter().any(|m| m.unread),
            age_s: self.messages.iter().filter_map(|m| m.age_s).min(),
        }
    }

    /// Mark read what the panel has shown, as the node does.
    pub fn read(&mut self, read: Read) {
        if read.peer == self.peer {
            for m in self.messages.iter_mut().take(read.through) {
                m.unread = false;
            }
        }
    }
}

/// The Messages screen over some conversations: the newest first.
pub fn list(recorded: &[Recorded]) -> Conversations {
    let mut rows: Vec<ConversationRow> = recorded.iter().map(Recorded::row).collect();
    rows.sort_by_key(|r| r.age_s.unwrap_or(u32::MAX));
    let mut out = Conversations::default();
    for (slot, row) in out
        .rows
        .iter_mut()
        .zip(rows.into_iter().take(CONVERSATIONS))
    {
        *slot = Some(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_conversations_parse_and_show_one_thing_each() {
        let all = bench();
        let named = |name: &str| {
            all.iter()
                .find(|r| r.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("{name}"))
        };
        let bench = named("Bench peer");
        assert_eq!(
            bench.messages[0].content,
            "Hello from the bench \u{2013} it\u{2019}s \u{201c}Python\u{201d}\u{2026}",
            "the vectors' text"
        );
        assert_eq!(named("Galaxy S9").messages.len(), 1);
        let ana: Vec<Direction> = named("Ana").messages.iter().map(|m| m.direction).collect();
        for delivery in [
            Delivery::Queued,
            Delivery::Sent,
            Delivery::Delivered,
            Delivery::Failed,
        ] {
            assert!(ana.contains(&Direction::Sent(delivery)), "{delivery:?}");
        }
        let stranger = all.iter().find(|r| r.name.is_none()).unwrap();
        assert_eq!(
            stranger.messages[0].direction,
            Direction::Received(Trust::Unverified)
        );
        assert!(named("Long winded").messages[0].content.contains("\n\n"));
        assert!(named("Chatty").messages.len() > 10);
        assert!(named("Chatty").messages.iter().any(|m| m.attachment));
    }

    #[test]
    fn a_bad_line_says_where_and_why() {
        assert_eq!(
            parse("< 1 verified Hi"),
            Err("line 1: a message before any `peer` line".into())
        );
        let peer = "peer 00112233445566778899aabbccddeeff\n";
        for (line, why) in [
            ("< x verified Hi", "is not an age"),
            ("< 1 delivered Hi", "is not a direction and a state"),
            ("> 1 sent,unread Hi", "is not a flag here"),
            ("<", "no age"),
        ] {
            let err = parse(&format!("{peer}{line}")).unwrap_err();
            assert!(err.starts_with("line 2: ") && err.contains(why), "{err}");
        }
        assert!(parse("peer 0011").unwrap_err().contains("32 hex digits"));
    }

    #[test]
    fn the_list_is_newest_first_and_marks_the_unread() {
        let rows = list(&bench());
        let rows: Vec<_> = rows.rows.iter().flatten().collect();
        assert_eq!(rows.len(), bench().len());
        assert!(rows.windows(2).all(|w| w[0].age_s <= w[1].age_s));
        assert_eq!(rows[0].name, Some(PeerName::new("Bench peer")));
        assert!(rows[0].unread);
    }

    #[test]
    fn reading_marks_only_that_peers_messages_that_far() {
        let mut all = bench();
        let chatty = all.iter_mut().find(|r| r.name.as_deref() == Some("Chatty"));
        let chatty = chatty.unwrap();
        let unread = |r: &Recorded| r.messages.iter().filter(|m| m.unread).count();
        assert_eq!(unread(chatty), 2);
        chatty.read(Read {
            peer: [0; 16],
            through: 99,
        });
        assert_eq!(unread(chatty), 2, "another peer's");
        chatty.read(Read {
            peer: chatty.peer,
            through: 11,
        });
        assert_eq!(unread(chatty), 1);
    }
}
