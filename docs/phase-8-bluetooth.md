# Phase 8: Bluetooth LE

Where phase 8 stands: **done.** iOS Sideband connects to `RNode 7F23`,
pairs with the six digits the panel shows, configures the radio, and brings
the interface online, over the Nordic UART Service, through the same protocol
core and the same modem loop the USB port uses. The bond goes to flash and the
phone gets back in after a reset without being asked again. USB keeps working
alongside — `rnodeconf` validates the device signature with the stack running
— and the panel's title bar shows `BT` while advertising and `BT*` with a
phone on the line.

## Pairing

Both characteristics demand an *authenticated* link: a write to RX or a
subscription to TX from an unpaired phone is refused with "insufficient
authentication", and every phone answers that by pairing. That is the only
trigger there is — a peripheral cannot demand pairing, it can only refuse to
talk until it has happened.

The board is `DisplayOnly`, so the method is Passkey Entry: the host stack
generates six digits, the panel shows them in a box over the status page, the
phone's user types them, and the link is encrypted *and authenticated* — MITM
protected. "Just works" pairing, which anyone in range can do, would only get
encryption, and an RNode's host is the only thing allowed to key its radio.
The stock firmware makes the same choice.

Two things had to be found out on hardware:

* **Bonding is per connection and off by default** in `trouble-host`. The
  first pairing completed authenticated and then reported "the phone did not
  bond": the keys were thrown away with the connection, and the phone would
  have been asked for a passkey every time. `Connection::set_bondable(true)`
  before anything can start pairing is the whole fix.
* **iOS keeps its half of a bond the board has forgotten**, and then refuses
  the device until the user deletes it by hand. So bonds are persisted: the
  device record gained a version and a table of four, oldest reused first,
  written with everything else on the same 250 ms timer, and reloaded into
  the host stack at boot. A version 1 record — every board provisioned before
  phase 8 — still decodes, with no bonds.

Verified: passkey `029717` shown on the panel and accepted by iOS; "paired,
authenticated"; "528 bytes written to 0xea000"; reset; reconnected online with
no passkey asked.

It did not start out that way. The first working image hung on every boot
that followed a flash and came up on every boot that followed the reset
button, and for a long stretch that looked like a race. The bug, the tool that
found it, and the list of things that were ruled out along the way are all
below, because the ruling-out is most of what a next problem of this kind
will reuse.

## How the two transports share one modem

The modem loop in `src/bin/rnode.rs` is the one owner of the protocol and the
radio, exactly as it was for USB alone. Bluetooth is one more place bytes come
from and go to: a pipe in each direction, pumped by `oxinode::nus::pump`, with
its own KISS decoder and its own outbox so a frame in progress on one
transport can never be spliced into a frame on the other. The modem loop
selects on the USB pipe, the Bluetooth pipe, and the radio's interrupt.

Who gets a frame nobody asked for — a received packet, a modem error — is one
rule: **a connected phone is the host.** Answers to commands always go back
the way the command came, so `rnodeconf` over USB keeps working while a phone
is on the line; unsolicited frames go to the phone while there is one, and to
USB otherwise.

The modem loop never waits on the phone. Its Bluetooth outbox drains into
the pipe with `try_write`; what does not fit waits for the next pass, and a
phone that has gone gets its frames dropped and counted rather than a modem
that stops servicing the radio.

## The one thing Sideband had to be told

The first connection with a real radio behind it failed with "radio
configuration failed: the RNode radio is locked because its modem
configuration is incomplete". The board's log said why:

```text
radio stays off: power is above the module's 20 dBm rating
```

iOS Sideband's default transmit power is above what this module is rated for,
and phase 4's rule stands: nothing clamps, because a clamp is a lie the host
cannot detect, and Reticulum refuses an interface whose read-back differs from
what it set in any case. With the app's TX power set to 14 dBm — the figure
`rnodeconf` reports for this board — the interface came online. The refusal
now logs the numbers it refused, so the next person is sent to the settings
screen knowing what to type.

## What works

* **The dependency set.** `trouble-host` 0.8 over `nrf-sdc` 0.4 over `nrf-mpsl`
  0.4. They pin embassy-nrf 0.11, embassy-sync 0.8 and embassy-time 0.5.1 —
  exactly what phases 1 to 7 already use. Nothing had to move, which was the
  single biggest risk going in.
* **Coexistence with USB.** The board enumerates, logs, and takes the
  1200-baud touch with MPSL's critical section, MPSL's interrupt priorities,
  and the shared `CLOCK_POWER` vector serving both. See `oxinode::ble::Vbus`.
* **Everything above the controller:**

  ```text
  ble: address [fd, 1d, 3b, 15, 24, d2] (reversed on the wire), name RNode 7F23
  [host] using packet pool with MTU 251 capacity 16
  [host] filter accept list size: 8
  [host] setting txq to 4, fragmenting at 251
  [host] configuring host buffers (1 packets of size 255)
  [host] initialized
  ble: advertising
  ```

  The address is a static random one derived from the chip's factory device
  address, the name carries the `RNode ` prefix Reticulum scans for, and the
  controller accepted every advertising command it was given.

## The failure, and what it was

`MultiprotocolServiceLayer::new` — Nordic's `mpsl_init` — entered and did not
leave. The board still enumerated over USB and still answered control
transfers, and did nothing else at all.

### What it was not

Each of these was excluded by evidence rather than by argument, and the way
each was excluded is reusable:

| Ruled out | How |
|---|---|
| A crash | A `HardFault` handler and a `DefaultHandler` record the cause in `GPREGRET2` and reboot into the bootloader. The board never landed in DFU. |
| MPSL's own assertion | Its assert path with no handler registered ends in `AIRCR = SYSRESETREQ` — it resets the chip. With a handler registered it panics, and this firmware's panic handler also reboots into the bootloader. Neither happened. |
| The executor, or `embassy-time` | `Instant::now()` twice around a cycle-counted busy wait shows the RTC advancing; the image's heartbeat arrives every 1.0025 s until the bring-up is asked for. |
| A stopped or missing low-frequency clock | `LFCLKSTAT` reads running, source = crystal, `EVENTS_LFCLKSTARTED` set, immediately before the call. |
| `skip_wait_lfclk_started` | All three settings reach the same point. Disassembly explains why: on an already-running clock with a matching source, the wait finds its condition already true. |
| The oscillator | The internal RC hung and worked in exactly the same pattern as the crystal. |
| The order of bring-up against USB | Starting MPSL before anything else runs, as every example does, hung identically. |
| `SEVONPEND` | MPSL's wait helper on this part is a bare `WFE`, and `embassy-executor` does not set `SEVONPEND` where Zephyr does. Setting it changed nothing; it has been removed. |
| An unhandled interrupt MPSL enabled | Covered by the `DefaultHandler` above. |

### What it was

**An interrupt storm on `CLOCK_POWER`, at the lowest priority there is.**

`POWER` and `CLOCK` share one vector and one interrupt-enable word. When the
bootloader starts the application from DFU — every boot after a flash — its
own USB stack has run, and it hands over with `USBDETECTED`, `USBREMOVED` and
`USBPWRRDY` still enabled: `0x380`, read back at boot. After a press of the
reset button the same word reads `0`. `USBPWRRDY` is latched from the moment
the regulator comes up.

Phases 1 to 7 never noticed, because `HardwareVbusDetect` clears those events.
The Bluetooth image bound the vector to MPSL's clock handler alone, which
knows nothing about `POWER`, and the instant `mpsl_init` unmasked the vector
the line was high with nothing to lower it. MPSL puts that interrupt at
priority 7, so USB at priority 2 kept answering the host while everything in
thread mode — the log, the touch, the executor — stopped for good.

The "race" was the boot path. Every hang followed a flash; every success
followed the reset button; and the investigation alternated between them
without knowing it mattered.

### How it was found

By capturing the program counter. `TIMER1` is armed before the call and
disarmed after it; if it fires, its handler — two instructions of assembly,
because the exception frame sits at the stack pointer *at entry* and a
compiled prologue moves it — reads the interrupted `PC`, `LR` and `xPSR` out
of the frame, snapshots the shared enable word and every `POWER`/`CLOCK`
event, writes them to RAM the linker does not zero, and resets into the
application, which reports them on the next keystroke. See
`oxinode::ble::stall`.

The first sample put the `PC` inside `MPSL_IRQ_CLOCK_Handler` with `xPSR`
saying exception 16 was active — not a spin in thread mode at all. The second
added a count: the handler had run **841,653 times** in two seconds, with
`INTENSET = 0x380` and `USBPWRRDY` set.

### The fix

One handler for the one vector: `oxinode::ble::PowerAndClockHandler`
services `POWER` — clears the USB events and folds them into a
`SoftwareVbusDetect`, which is what `embassy-usb` was designed to be fed by
exactly this — and then hands `CLOCK` to MPSL. `Vbus::take` clears the whole
inherited enable word before the vector is ever unmasked, and enables only
the three USB bits it will service.

That also retires the polling VBUS detector that stood in for this while the
conflict was misunderstood: it was a workaround for what turned out to be a
shared line, not a shared owner. And it corrects an earlier claim in this
file and in the README that the interrupt-driven repair "does not exist".

Two hedges added during the hunt — setting `SEVONPEND`, and enabling MPSL's
low-priority interrupt by hand — were each removed and re-tested. Six of six
without them.

## Design notes worth keeping

### Debugging a blob on a board with no probe

Most of this phase's cost was not the bug. It was that a stalled image and a
crashed one and a stopped clock all look the same from the host: the board goes
quiet. Three things fixed that, and all three belong in any image that brings
up a binary blob:

* **Never attempt it on its own.** The bring-up waits for a byte on the log
  port. A board that comes up is therefore always a board that enumerates and
  can be reflashed from the keyboard. This alone turned a double-tap on the
  reset button into a single press, which is worth more than it sounds: half a
  dozen of them were spent before it was in place.
* **Make a crash look different from a hang.** Faults and unhandled interrupts
  reboot into the bootloader, which the host sees. Silence then means a stall
  and nothing else.
* **Keep a channel that does not need the executor.** The blue LED is lit for
  exactly as long as the bring-up runs. When everything else stopped, one look
  at the board said whether the call had returned — which halved the search
  space and cost nothing.

### Reading the blob

The controller ships as an archive of obfuscated symbols, and it is still just
ARM. `rust-objdump -d` on the linked image, plus a script that walks the call
graph from `mpsl_init` and reports tight polling loops, answered several
questions that no amount of reading the headers would have: that the clock
initialiser returns on all three settings, that the assert path resets rather
than halts, that the low-priority interrupt is enabled at the end of
initialisation, and that the wait helper uses a bare `WFE`. It is worth doing
early rather than late.

### The `ble` feature

The Bluetooth build is a separate feature set, not an extra feature, because it
swaps the `critical-section` implementation for the whole image:

```bash
cargo build --release --no-default-features --features ble --bin ble
```

`cortex-m`'s single-core implementation masks every interrupt. That is correct
for phases 1 to 7 and wrong the moment MPSL is running, because the link layer
keeps its timing on `RADIO`, `RTC0` and `TIMER0`. `nrf-mpsl` ships an
implementation that masks everything except those three.

The MPSL one would work in both builds — with MPSL absent those interrupts are
never enabled — but it costs an NVIC mask save and restore on every critical
section, and `embassy-time` takes one per timer operation. So the non-BLE
images keep the cheap one, `src/lib.rs` refuses a build that enables both, and
`tools/test.sh` lints and builds each separately.

### What the controller takes away

* **Peripherals**: `RTC0`, `TIMER0`, `TEMP`, `RADIO`, and PPI channels 17 to
  31. `embassy-time` here runs on `RTC1`, so the clock everything else depends
  on was already clear.
* **Interrupt priorities**: MPSL sets `RADIO`, `RTC0` and `TIMER0` to `P0` and
  its own two to `P4`. It cannot lower *ours*, and the NVIC's reset value is
  the same `P0` the link layer runs at. Two interrupts at equal priority do not
  preempt each other, so a USB transfer left there does not interrupt the radio
  handler — it *delays* it, which is worse. `embassy-nrf` sets a priority for
  exactly two things, GPIOTE and the time driver, both from its `Config`; every
  other bound interrupt keeps whatever the NVIC had.
* **`CLOCK_POWER`**: `POWER` and `CLOCK` share one interrupt on this part,
  and one interrupt-enable register with disjoint bits. Only one handler can
  be bound to the vector, so it is one that does both jobs — see above.

### Debugging tools that stay in the image

`src/bin/ble.rs` keeps four things that were built for this hunt and are
worth keeping for the next one:

* **A keystroke gate.** The bring-up is never attempted on its own. A board
  that comes up is always an enumerated, reflashable board.
* **A breadcrumb** in `GPREGRET2`, written before the bring-up and cleared
  after, so the next boot can say the last one did not come back.
* **Fault and unhandled-interrupt handlers** that record and reboot into the
  bootloader, so a crash is visible from the host and looks different from a
  stall.
* **The stall capture** described above, armed around every bring-up.

### What is still owed

* **`NVMC` stalls the CPU.** A flash page erase takes about 85 ms during which
  the core does not execute, and MPSL cannot hold a connection through that.
  Phase 6 writes the device record with `NVMC` directly. Until that moves onto
  `nrf_mpsl::Flash`, which schedules the write inside a timeslot, a
  provisioning run with a phone connected will drop the connection.
## Licensing

`nrf-sdc-sys` vendors Nordic's SoftDevice Controller as a binary archive under
`LicenseRef-Nordic-5-Clause`, which permits use on Nordic silicon. This is
Nordic silicon. It is worth knowing that this is the one part of oxinode that
is neither MIT nor Apache-2.0 and neither is nor could be built from source.
