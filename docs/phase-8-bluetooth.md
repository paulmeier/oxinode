# Phase 8: Bluetooth LE

Where phase 8 stands: **the stack builds, links, and comes up every time** —
twenty fresh boots out of twenty reached `ble: advertising` as `RNode 7F23`,
sixteen on the crystal and four on the internal RC oscillator as a control.

It did not start out that way. The first working image hung on every boot
that followed a flash and came up on every boot that followed the reset
button, and for a long stretch that looked like a race. The bug, the tool that
found it, and the list of things that were ruled out along the way are all
below, because the ruling-out is most of what a next problem of this kind
will reuse.

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
* **Pairing.** LE Secure Connections with MITM protection and a six-digit
  passkey, which is why phase 7 came first. `trouble-host`'s `security` feature
  and the `security-p256-cortex-m4` backend are the pieces; neither is enabled
  yet, and enabling them changes the memory sizing.
* **Nothing to talk to.** There is no GATT server in this image on purpose.
  Whether the controller runs at all is one question and whether the Nordic
  UART Service definition is right is another, and answering them in separate
  images means a failure says which.

## Licensing

`nrf-sdc-sys` vendors Nordic's SoftDevice Controller as a binary archive under
`LicenseRef-Nordic-5-Clause`, which permits use on Nordic silicon. This is
Nordic silicon. It is worth knowing that this is the one part of oxinode that
is neither MIT nor Apache-2.0 and neither is nor could be built from source.
