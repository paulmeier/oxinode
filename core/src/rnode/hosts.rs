//! Two hosts, and the one rule for which of them gets a frame.
//!
//! A board can have a USB host and a phone on the line at once. Answers go
//! back the way the command came, so `rnodeconf` over USB works with a phone
//! connected. A frame nobody asked for — a heard packet, a modem error —
//! goes to the phone while there is one, and to USB otherwise.
//!
//! The second half of that rule is decided here and nowhere else, because a
//! heard packet has two ways of reaching the host. The modem loop receives
//! one while it is idle; and the carrier-sense wait before a transmission is
//! spent in receive, so a packet that arrives during it is handed up from
//! inside `transmit`. The second path used to give the packet to the host
//! that asked for the transmission — the way its answers go — so with a
//! phone connected and a USB host transmitting at that moment, the phone
//! missed a packet the idle path would have given it. Both paths now ask
//! [`Hosts::listener`], whose only input is whether a phone is connected.
//! Who asked for the transmission is not an input, and so cannot be the
//! answer.

use crate::rnode::outbox::Outbox;

/// One of the two ways a host reaches the modem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// The KISS port.
    Usb,
    /// The Nordic UART Service, carrying the same stream.
    Bluetooth,
}

impl Transport {
    /// Who gets a frame nobody asked for: the phone while there is one, USB
    /// otherwise.
    ///
    /// The whole of the routing rule. It is a function of nothing but whether
    /// a phone is connected, and every unsolicited frame goes through it,
    /// whichever path it came up.
    pub const fn listener(phone_connected: bool) -> Self {
        if phone_connected {
            Transport::Bluetooth
        } else {
            Transport::Usb
        }
    }
}

/// An outbox per transport, and the two questions the modem loop asks of
/// them: where an answer goes, and where an unsolicited frame goes.
///
/// One outbox each rather than one shared, so a frame in progress on one
/// transport is never spliced with the other's.
pub struct Hosts<const N: usize> {
    usb: Outbox<N>,
    bluetooth: Outbox<N>,
}

impl<const N: usize> Default for Hosts<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Hosts<N> {
    pub const fn new() -> Self {
        Self {
            usb: Outbox::new(),
            bluetooth: Outbox::new(),
        }
    }

    /// The outbox of one transport: where an answer to a command that came
    /// on it goes, and what its pump drains.
    pub fn outbox(&mut self, transport: Transport) -> &mut Outbox<N> {
        match transport {
            Transport::Usb => &mut self.usb,
            Transport::Bluetooth => &mut self.bluetooth,
        }
    }

    /// Where a frame nobody asked for goes. See [`Transport::listener`].
    pub fn listener(&mut self, phone_connected: bool) -> &mut Outbox<N> {
        self.outbox(Transport::listener(phone_connected))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rnode::command::{cmd, Command, RadioState};
    use crate::rnode::kiss;
    use crate::rnode::protocol::{Action, Protocol, Sink};

    /// The command bytes of every frame queued in an outbox.
    fn commands<const N: usize>(out: &Outbox<N>) -> Vec<u8> {
        let mut dec = kiss::Decoder::<{ kiss::HW_MTU }>::new();
        let mut seen = Vec::new();
        for &b in out.pending() {
            if dec.feed(b) == kiss::Step::Frame {
                seen.push(dec.command());
            }
        }
        seen
    }

    #[test]
    fn an_answer_goes_back_the_way_the_command_came() {
        for via in [Transport::Usb, Transport::Bluetooth] {
            let mut hosts = Hosts::<256>::new();
            hosts.outbox(via).frame(cmd::FW_VERSION, &[1, 52]);
            assert_eq!(commands(hosts.outbox(via)), vec![cmd::FW_VERSION]);
            let other = match via {
                Transport::Usb => Transport::Bluetooth,
                Transport::Bluetooth => Transport::Usb,
            };
            assert!(hosts.outbox(other).is_empty());
        }
    }

    #[test]
    fn a_frame_nobody_asked_for_goes_to_the_phone_while_there_is_one() {
        assert_eq!(Transport::listener(true), Transport::Bluetooth);
        let mut hosts = Hosts::<256>::new();
        hosts.listener(true).frame(cmd::ERROR, &[0x01]);
        assert_eq!(
            commands(hosts.outbox(Transport::Bluetooth)),
            vec![cmd::ERROR]
        );
        assert!(hosts.outbox(Transport::Usb).is_empty());
    }

    #[test]
    fn and_to_usb_otherwise() {
        assert_eq!(Transport::listener(false), Transport::Usb);
        let mut hosts = Hosts::<256>::new();
        hosts.listener(false).frame(cmd::ERROR, &[0x01]);
        assert_eq!(commands(hosts.outbox(Transport::Usb)), vec![cmd::ERROR]);
        assert!(hosts.outbox(Transport::Bluetooth).is_empty());
    }

    /// The bug this module exists to close. A phone is connected and a USB
    /// host asks for a transmission; a packet heard during the wait for a
    /// clear channel is routed exactly as one heard while idle -- to the
    /// phone -- and the USB host gets only the answers to its own command.
    #[test]
    fn a_packet_heard_during_a_transmit_wait_is_routed_like_one_heard_while_idle() {
        let phone_connected = true;
        let mut p = Protocol::new();
        let mut hosts = Hosts::<1024>::new();

        // The USB host brings the radio up and asks for a transmission. Its
        // answers go back to it.
        let via = Transport::Usb;
        p.handle(Command::SetRadioState(RadioState::On), hosts.outbox(via));
        assert_eq!(
            p.handle(Command::Data(b"outgoing"), hosts.outbox(via)),
            Action::Transmit(b"outgoing")
        );

        // During the wait, a packet is on the air. It is handed up from the
        // transmit path, and the transmit path asks the same question the
        // idle path asks.
        p.received(
            -80,
            20,
            b"heard while waiting",
            hosts.listener(phone_connected),
        );
        p.transmitted(hosts.outbox(via));

        // Later, idle, another packet.
        p.received(
            -90,
            12,
            b"heard while idle",
            hosts.listener(phone_connected),
        );

        assert_eq!(
            commands(hosts.outbox(Transport::Bluetooth)),
            vec![
                cmd::STAT_RSSI,
                cmd::STAT_SNR,
                cmd::DATA,
                cmd::STAT_RSSI,
                cmd::STAT_SNR,
                cmd::DATA
            ],
            "both heard packets reach the phone"
        );
        assert_eq!(
            commands(hosts.outbox(Transport::Usb)),
            vec![cmd::RADIO_STATE, cmd::READY],
            "the USB host gets the answers to its own commands and nothing else"
        );
    }
}
