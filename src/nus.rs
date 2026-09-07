//! The Nordic UART Service: the RNode KISS stream over Bluetooth.
//!
//! There is no Bluetooth protocol of its own here. Reticulum's `RNodeInterface`
//! and Sideband connect to a peripheral offering this service and push down
//! it exactly the bytes they would push down a serial port — see
//! [`oxinode_core::ble`] for the constants that decide whether they find it.
//!
//! Two characteristics, named from the *client's* point of view, which is how
//! every NUS implementation names them:
//!
//! * **RX** — the host writes to it. Bytes arrive here in write commands of up
//!   to `ATT_MTU - 3`, with no framing of their own; KISS is the framing.
//! * **TX** — we notify on it. Bytes leave in notifications of up to
//!   `ATT_MTU - 3`, likewise.
//!
//! What sits between them is the same [`kiss::Decoder`], the same
//! [`Protocol`] and the same [`Outbox`] the USB port uses. That is the point:
//! Bluetooth is a second pipe, and everything that is not the pipe is shared.

use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::pipe::{Reader, Writer};
use embassy_time::{with_timeout, Duration};
use heapless::Vec;
use oxinode_core::ble as interop;
use oxinode_core::rnode::command;
use oxinode_core::rnode::kiss;
use oxinode_core::rnode::outbox::Outbox;
use oxinode_core::rnode::protocol::{Action, Protocol};
use trouble_host::prelude::*;

/// Largest write the host can make in one go, and largest notification we
/// send: the attribute ceiling, which is what Reticulum negotiates towards.
pub const VALUE_LEN: usize = interop::MAX_GATT_ATTR_LEN;

/// Space for responses waiting to be notified.
///
/// The same reasoning as the USB outbox in `src/bin/rnode.rs`: a worst-case
/// data frame whole, or a display read, because the outbox drops whole frames
/// rather than truncating them and a frame that could never fit would never be
/// sent at all.
pub const OUTBOX: usize = 4096;
const _: () = assert!(OUTBOX >= 2 * kiss::HW_MTU + 3);
const _: () = assert!(OUTBOX >= 2 * oxinode_core::rnode::display::DISP_LEN + 3);

/// The GATT server: one service, the NUS, and nothing else.
///
/// The Generic Access and Generic Attribute services that every peripheral
/// has are added by the macro. `attribute_table_size` is what it needs for
/// those plus one service with two characteristics and one CCCD; the macro
/// asserts if it is short.
#[gatt_server(connections_max = crate::ble::CONNECTIONS, attribute_table_size = 16)]
pub struct Server {
    pub nus: NusService,
}

/// See the module docs for which direction is which.
#[gatt_service(uuid = interop::NUS_SERVICE)]
pub struct NusService {
    #[characteristic(uuid = interop::NUS_RX_CHARACTERISTIC, write, write_without_response)]
    pub rx: Vec<u8, VALUE_LEN>,
    #[characteristic(uuid = interop::NUS_TX_CHARACTERISTIC, notify)]
    pub tx: Vec<u8, VALUE_LEN>,
}

/// How a transport asks the rest of the modem to act on a command.
///
/// The bridge below decodes frames and runs the protocol; what it cannot do is
/// touch the radio, the flash or the panel, because those belong to whichever
/// image is running it. So every action the protocol produces is handed here,
/// with the outbox, and the image does with it what it does with the same
/// action from USB.
pub trait Act {
    /// Carry out `action`, queueing any response frames in `outbox`.
    fn act(
        &mut self,
        protocol: &mut Protocol,
        action: Action<'_>,
        outbox: &mut Outbox<OUTBOX>,
    ) -> impl core::future::Future<Output = ()>;
}

/// Why a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum End {
    /// The peer disconnected, with the HCI reason.
    Disconnected(u8),
    /// A notification could not be delivered within its timeout; the link is
    /// presumed gone even if the controller has not said so yet.
    Stalled,
}

/// Run one connection to its end.
///
/// Every write to RX is fed through the decoder a byte at a time; every
/// complete frame becomes a command, the protocol answers it, and whatever the
/// answer produces is notified on TX in MTU-sized pieces before the next
/// event is waited for. Unsolicited frames — received packets — reach the
/// outbox through the same `protocol`, from the image, between calls; this
/// flushes them too.
pub async fn session<'a, P: PacketPool, A: Act>(
    conn: &GattConnection<'a, '_, P>,
    server: &Server<'_>,
    protocol: &mut Protocol,
    outbox: &mut Outbox<OUTBOX>,
    act: &mut A,
) -> End {
    let mut decoder = kiss::Decoder::<{ kiss::HW_MTU }>::new();
    let rx_handle = server.nus.rx.handle;
    defmt::info!(
        "nus: rx handle {=u16}, tx handle {=u16}, tx cccd {}",
        rx_handle,
        server.nus.tx.handle,
        server.nus.tx.cccd_handle
    );

    defmt::info!(
        "nus: session with {=[u8; 6]:02x}, att mtu {=u16}",
        conn.raw().peer_address().addr.into_inner(),
        conn.raw().att_mtu()
    );

    loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { reason } => {
                outbox.abandon();
                return End::Disconnected(reason.into_inner());
            }
            GattConnectionEvent::Gatt {
                event: GattEvent::Write(event),
            } => {
                defmt::info!(
                    "nus: write to handle {=u16} ({=usize} bytes)",
                    event.handle(),
                    event.with_data(|_, d| d.len())
                );
                if event.handle() == rx_handle {
                    // Copied out, because the event has to be answered before
                    // anything that awaits, and the protocol's actions await.
                    let mut bytes = Vec::<u8, VALUE_LEN>::new();
                    event.with_data(|_, data| {
                        let _ = bytes.extend_from_slice(data);
                    });
                    match event.accept() {
                        Ok(reply) => reply.send().await,
                        Err(e) => defmt::warn!("nus: could not accept a write: {}", e),
                    }
                    for &byte in bytes.iter() {
                        match decoder.feed(byte) {
                            kiss::Step::Pending => {}
                            kiss::Step::Error(e) => defmt::warn!("nus: kiss: {=str}", e.message()),
                            kiss::Step::Frame => {
                                let cmd = command::decode(decoder.command(), decoder.payload());
                                let action = protocol.handle(cmd, outbox);
                                act.act(protocol, action, outbox).await;
                            }
                        }
                    }
                } else {
                    // A write to something else -- the CCCD, subscribing to
                    // TX. Accepting is what makes the subscription take.
                    match event.accept() {
                        Ok(reply) => reply.send().await,
                        Err(e) => defmt::warn!("nus: could not accept a write: {}", e),
                    }
                }
            }
            GattConnectionEvent::Gatt {
                event: GattEvent::Read(event),
            } => {
                defmt::info!("nus: read of handle {=u16}", event.handle());
                match event.accept() {
                    Ok(reply) => reply.send().await,
                    Err(e) => defmt::warn!("nus: could not accept a read: {}", e),
                }
            }
            GattConnectionEvent::Gatt { event } => {
                // Anything else the host does at the ATT layer gets the
                // server's default answer.
                defmt::info!("nus: other att request");
                match event.accept() {
                    Ok(reply) => reply.send().await,
                    Err(e) => defmt::warn!("nus: {}", e),
                }
            }
            GattConnectionEvent::ConnectionParamsUpdated {
                conn_interval,
                supervision_timeout,
                ..
            } => defmt::info!(
                "nus: interval {=u64} us, supervision {=u64} ms",
                conn_interval.as_micros(),
                supervision_timeout.as_millis()
            ),
            GattConnectionEvent::DataLengthUpdated {
                max_tx_octets,
                max_rx_octets,
                ..
            } => defmt::info!(
                "nus: data length tx {=u16} rx {=u16}",
                max_tx_octets,
                max_rx_octets
            ),
            GattConnectionEvent::PhyUpdated { tx_phy, rx_phy } => {
                defmt::info!("nus: phy tx {=u8} rx {=u8}", tx_phy as u8, rx_phy as u8)
            }
            _ => defmt::info!("nus: some other connection event"),
        }

        if let Err(end) = flush(conn, server, outbox).await {
            return end;
        }
    }
}

/// Notify everything queued, in pieces the link can carry.
///
/// A notification carries `ATT_MTU - 3` bytes. Reticulum negotiates the MTU
/// up to 512; a phone that will not go past 23 gets 20 bytes at a time and a
/// 508-byte packet in twenty-six notifications, which is slow and correct.
pub async fn flush<P: PacketPool>(
    conn: &GattConnection<'_, '_, P>,
    server: &Server<'_>,
    outbox: &mut Outbox<OUTBOX>,
) -> Result<(), End> {
    let piece = interop::notification_payload_len(conn.raw().att_mtu());
    while !outbox.is_empty() {
        let n = outbox.len().min(piece);
        // Bounded, for the same reason the USB flush is: a peer that has
        // stopped acknowledging holds the notification forever, and the
        // modem must not stop servicing the radio because a phone walked
        // out of range.
        match with_timeout(
            Duration::from_millis(2_000),
            server
                .nus
                .tx
                .notify_raw(conn, &outbox.pending()[..n], false),
        )
        .await
        {
            Ok(Ok(())) => outbox.consume(n),
            Ok(Err(e)) => {
                defmt::warn!("nus: notify failed: {}", e);
                outbox.abandon();
                return Err(End::Stalled);
            }
            Err(_) => {
                outbox.abandon();
                return Err(End::Stalled);
            }
        }
    }
    let dropped = outbox.take_dropped();
    if dropped > 0 {
        defmt::warn!("nus: {=u32} frames dropped", dropped);
    }
    Ok(())
}

/// Run one connection as a byte pump between two pipes.
///
/// This is what the product image uses. There, the modem loop is the one
/// owner of the protocol and the radio, exactly as it is for USB, and
/// Bluetooth is one more place bytes come from and go to: writes to RX go
/// into `to_modem`, and whatever the modem loop puts in `from_modem` is
/// notified on TX in pieces the link can carry. Nothing here knows what a
/// KISS frame is.
///
/// # Back-pressure
///
/// Filling `to_modem` waits, which stalls this task and only this task: the
/// phone's writes are NAKed at the link layer until the modem catches up,
/// which is the same thing that happens to a USB host. `from_modem` is the
/// modem loop's problem and is filled without waiting -- see the product
/// image -- so a phone that stops reading costs it frames, not time.
pub async fn pump<P: PacketPool, M: RawMutex, const IN: usize, const OUT: usize>(
    conn: &GattConnection<'_, '_, P>,
    server: &Server<'_>,
    to_modem: &Writer<'_, M, IN>,
    from_modem: &Reader<'_, M, OUT>,
) -> End {
    let rx_handle = server.nus.rx.handle;
    defmt::info!(
        "nus: pumping for {=[u8; 6]:02x}, att mtu {=u16}",
        conn.raw().peer_address().addr.into_inner(),
        conn.raw().att_mtu()
    );
    let mut out = [0u8; VALUE_LEN];
    loop {
        // `Reader::read` on an empty pipe pends, and cancelling it consumes
        // nothing, so racing it against the connection is safe -- which is
        // the property the USB path had to be restructured to get.
        match select(conn.next(), from_modem.read(&mut out)).await {
            Either::Second(n) => {
                let piece = interop::notification_payload_len(conn.raw().att_mtu());
                let mut sent = 0;
                while sent < n {
                    let end = (sent + piece).min(n);
                    match with_timeout(
                        Duration::from_millis(2_000),
                        server.nus.tx.notify_raw(conn, &out[sent..end], false),
                    )
                    .await
                    {
                        Ok(Ok(())) => sent = end,
                        Ok(Err(e)) => {
                            defmt::warn!("nus: notify failed: {}", e);
                            return End::Stalled;
                        }
                        Err(_) => return End::Stalled,
                    }
                }
            }
            Either::First(GattConnectionEvent::Disconnected { reason }) => {
                return End::Disconnected(reason.into_inner());
            }
            Either::First(GattConnectionEvent::Gatt {
                event: GattEvent::Write(event),
            }) => {
                let mut bytes = Vec::<u8, VALUE_LEN>::new();
                let is_rx = event.handle() == rx_handle;
                if is_rx {
                    event.with_data(|_, data| {
                        let _ = bytes.extend_from_slice(data);
                    });
                }
                match event.accept() {
                    Ok(reply) => reply.send().await,
                    Err(e) => defmt::warn!("nus: could not accept a write: {}", e),
                }
                if is_rx {
                    let mut rest: &[u8] = &bytes;
                    while !rest.is_empty() {
                        let n = to_modem.write(rest).await;
                        rest = &rest[n..];
                    }
                }
            }
            Either::First(GattConnectionEvent::Gatt { event }) => match event.accept() {
                Ok(reply) => reply.send().await,
                Err(e) => defmt::warn!("nus: {}", e),
            },
            Either::First(GattConnectionEvent::ConnectionParamsUpdated {
                conn_interval,
                supervision_timeout,
                ..
            }) => defmt::info!(
                "nus: interval {=u64} us, supervision {=u64} ms",
                conn_interval.as_micros(),
                supervision_timeout.as_millis()
            ),
            Either::First(_) => {}
        }
    }
}
