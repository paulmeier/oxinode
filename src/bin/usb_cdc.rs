//! Phase 2: enumerate as a USB CDC-ACM serial port.
//!
//! This is the transport the RNode/KISS protocol will eventually run over, so
//! the goal here is only to prove the plumbing: the board appears as a serial
//! device on the host, bytes survive a round trip, and the host can put the
//! board back into its bootloader without anyone reaching for the reset button.
//!
//! Behaviour:
//!   * echoes everything it receives,
//!   * blinks the green LED slowly when no host has opened the port and holds
//!     it on once one has, so the board's state is visible without a terminal,
//!   * reboots into the UF2 bootloader on a 1200-baud open/close ("touch"),
//!     the same convention Arduino and Adafruit boards use.
//!
//! There is no RNode framing here yet -- see the README for what phase 5 adds.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_futures::select::{select, Either};
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, peripherals};
use embassy_time::{Duration, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, Sender, State};
use embassy_usb::driver::EndpointError;
use embassy_usb::{Builder, Config as UsbConfig};
use oxinode::board::{self, Led};
use oxinode::boot;
use oxinode_core::usb as core_usb;
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    // On nRF52 the POWER and CLOCK peripherals share one interrupt line; the
    // USB driver needs it to see VBUS come and go.
    CLOCK_POWER => usb::vbus_detect::InterruptHandler;
});

/// pid.codes' explicitly-for-prototyping VID/PID pair.
///
/// Deliberately not a real allocation: oxinode does not have one yet, and
/// borrowing some other project's identifiers would make host-side device rules
/// lie about what is plugged in. Reticulum does not care -- `rnsd` is pointed at
/// a port by path -- but `rnodeconf`'s autodetect keys off VID/PID, so this will
/// need a proper PID before autodetect can work.
const USB_VID: u16 = 0x1209;
const USB_PID: u16 = 0x0001;

/// Full-speed bulk endpoints are 64 bytes; anything larger is silently clamped.
const MAX_PACKET_SIZE: u16 = 64;

type UsbDriver = Driver<'static, HardwareVbusDetect>;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    boot::relocate_vector_table();
    let p = embassy_nrf::init(board::embassy_config());

    let driver = Driver::new(p.USBD, Irqs, HardwareVbusDetect::new(Irqs));

    let mut config = UsbConfig::new(USB_VID, USB_PID);
    config.manufacturer = Some("oxinode");
    config.product = Some("oxinode RNode");
    config.serial_number = Some(device_serial());
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    // CDC-ACM is two interfaces (control + data) that have to be grouped, so the
    // device advertises itself as a composite device using an IAD rather than
    // claiming a class at the device level.
    config.device_class = 0xEF;
    config.device_sub_class = 0x02;
    config.device_protocol = 0x01;
    config.composite_with_iads = true;

    static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    static STATE: StaticCell<State> = StaticCell::new();

    let mut builder = Builder::new(
        driver,
        config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        &mut [], // no Microsoft OS descriptors; CDC-ACM is class-driver material
        CONTROL_BUF.init([0; 64]),
    );

    let class = CdcAcmClass::new(&mut builder, STATE.init(State::new()), MAX_PACKET_SIZE);
    let (mut tx, mut rx, control) = class.split_with_control();

    let mut usb = builder.build();
    let mut led = Led::new(p.P1_03);

    let run_usb = usb.run();

    let echo = async {
        loop {
            rx.wait_connection().await;
            // A disconnect surfaces as an endpoint error; that is the normal way
            // out of this, not a fault, so just wait for the next host.
            let _ = echo_session(&mut tx, &mut rx, &control).await;
        }
    };

    // Kept separate from the echo loop so that a host sitting idle with the port
    // open still leaves visible evidence the firmware is alive.
    let heartbeat = async {
        loop {
            if control.dtr() {
                led.on();
                Timer::after(Duration::from_millis(200)).await;
            } else {
                led.on();
                Timer::after(Duration::from_millis(60)).await;
                led.off();
                Timer::after(Duration::from_millis(940)).await;
            }
        }
    };

    join3(run_usb, echo, heartbeat).await;
}

/// Echo bytes until the host goes away, watching control traffic as we go.
async fn echo_session(
    tx: &mut Sender<'static, UsbDriver>,
    rx: &mut Receiver<'static, UsbDriver>,
    control: &ControlChanged<'static>,
) -> Result<(), EndpointError> {
    let mut buf = [0u8; MAX_PACKET_SIZE as usize];
    loop {
        // Bind before matching: leaving `select(..)` as the match scrutinee would
        // hold the borrows of `rx` and `buf` alive across the whole match.
        let event = select(rx.read_packet(&mut buf), control.control_changed()).await;
        match event {
            Either::First(result) => {
                let n = result?;
                tx.write_packet(&buf[..n]).await?;
                // A full-size packet needs a zero-length packet behind it, or the
                // host keeps waiting for the rest of the transfer.
                if n == MAX_PACKET_SIZE as usize {
                    tx.write_packet(&[]).await?;
                }
            }
            Either::Second(()) => {
                if is_bootloader_touch(rx, control) {
                    boot::reboot_to_bootloader();
                }
            }
        }
    }
}

/// Read the current line state and ask [`oxinode_core::usb::is_bootloader_touch`]
/// what it means. The rule itself is tested on the host; this is just the part
/// that has to touch the USB stack.
fn is_bootloader_touch(
    rx: &Receiver<'static, UsbDriver>,
    control: &ControlChanged<'static>,
) -> bool {
    core_usb::is_bootloader_touch(rx.line_coding().data_rate(), control.dtr())
}

/// A stable, per-board serial number derived from the nRF52840's factory device
/// ID, so the host names the port consistently and two boards never collide.
fn device_serial() -> &'static str {
    static SERIAL: StaticCell<[u8; 16]> = StaticCell::new();

    let ficr = embassy_nrf::pac::FICR;
    let id = ((ficr.deviceid(1).read() as u64) << 32) | ficr.deviceid(0).read() as u64;

    let buf = SERIAL.init(oxinode_core::serial::hex_u64(id));
    // `hex_u64` only ever emits ASCII hex digits, which its own tests assert.
    core::str::from_utf8(buf).expect("hex_u64 produced non-ASCII")
}
