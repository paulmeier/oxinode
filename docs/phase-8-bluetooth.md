# Phase 8: Bluetooth LE

Where phase 8 stands: **the stack builds, links, and has been seen to run all
the way to advertising as `RNode 7F23` — and it does not do so reliably.**
`mpsl_init` sometimes does not return. The same image, the same configuration,
the same board: it comes up one time and hangs the next.

That is written first because it is the honest summary. What follows is what
was established, what was ruled out and how, and what is left.

## What works

* **The dependency set.** `trouble-host` 0.8 over `nrf-sdc` 0.4 over `nrf-mpsl`
  0.4. They pin embassy-nrf 0.11, embassy-sync 0.8 and embassy-time 0.5.1 —
  exactly what phases 1 to 7 already use. Nothing had to move, which was the
  single biggest risk going in.
* **Coexistence with USB.** The board enumerates, logs, and takes the
  1200-baud touch with MPSL's critical section, MPSL's interrupt priorities,
  and `CLOCK_POWER` bound to MPSL rather than to `HardwareVbusDetect`. See
  `oxinode::ble::Vbus` for how VBUS is detected without that interrupt.
* **Everything above the controller.** When `mpsl_init` does return, the rest
  follows without complaint:

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

## The failure

`MultiprotocolServiceLayer::new` — that is, Nordic's `mpsl_init` — enters and
does not leave.

It is **not** any of the following, and each was excluded by evidence rather
than by argument:

| Ruled out | How |
|---|---|
| A crash | A `HardFault` handler and a `DefaultHandler` were added that record the cause in `GPREGRET2` and reboot into the bootloader. The board never lands in DFU. |
| MPSL's own assertion | Its assert path with no handler registered ends in `AIRCR = SYSRESETREQ` — it resets the chip. The board does not reset. With a handler registered it panics, and this firmware's panic handler also reboots into the bootloader. |
| The executor, or `embassy-time` | `Instant::now()` twice around a cycle-counted busy wait shows the RTC advancing correctly, and the image's own heartbeat arrives every 1.0025 s until the bring-up is asked for. |
| A stopped or missing low-frequency clock | `LFCLKSTAT` reads running, source = crystal, and `EVENTS_LFCLKSTARTED` set, immediately before the call. |
| `skip_wait_lfclk_started` | All three settings — false, true, and false after handing the clock back stopped — reach the same point. Disassembly explains why: on an already-running clock with a matching source, the wait finds its condition already true. |
| The order of bring-up against USB | Every published example starts MPSL before anything else is running. Doing it that way here hangs identically, before enumeration instead of after. |
| An unhandled interrupt MPSL enabled | Covered by the `DefaultHandler` above. |

Two things are known that a next attempt should start from.

**It is a race.** The internal RC oscillator was seen to bring the whole stack
up, and then hung on the very next boot with nothing changed. That single
observation is worth more than everything above: it rules out every static
explanation — wrong constant, wrong peripheral, wrong build — and points at
timing or at something that depends on what else is happening.

**MPSL waits with a bare `WFE`.** Disassembling its wait helper out of the
linked image shows, on this part, no `SEV` before it and no `SEVONPEND`
management around it. A bare `WFE` only wakes for an interrupt the NVIC will
actually take, and `mpsl_init` disables its own while it runs. In Nordic's own
environment `SEVONPEND` is set globally, so a *pending* interrupt is enough;
`embassy-executor` has no reason to set it and does not, because its own `WFE`
is woken by the `SEV` its pender issues.

That would explain the race exactly — the bring-up escapes when some unrelated
enabled interrupt happens to fire, and sleeps forever when the port is quiet.
`ble::set_sevonpend` sets it before the call. **It did not fix the hang.** It
may still be necessary; it is not sufficient, and the reasoning behind it is
the best lead there is.

## What to try next

* Capture the program counter. Arm an unused timer at high priority before the
  call, read the stacked `PC` out of the exception frame in its handler, and
  record it across a reset. The wait loops are already located in the
  disassembly; the address would say which one, and that is the one fact this
  investigation never got.
* Compare against a bare-metal build of the same crate versions with no
  SoftDevice and no bootloader in flash, linked at zero. This board boots at
  `0x26000` behind an MBR and a dormant S140, which is the one part of the
  environment that no example shares.
* Ask upstream. `nrf-sdc` is actively maintained and the bare `WFE` is a real
  observation about the shipped binary, whether or not it is the cause here.

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
* **`CLOCK_POWER`**: `POWER` and `CLOCK` share one interrupt on this part.
  `embassy-usb` normally binds it through `HardwareVbusDetect`; MPSL needs it
  for the clock. Only one handler can be bound to a vector, and the repair —
  `SoftwareVbusDetect` fed from MPSL's power events — does not exist, because
  MPSL's clock handler services `CLOCK` and the USB events belong to `POWER`.
  Enabling them in `INTENSET` would deliver them to MPSL's handler, which would
  not clear them: an interrupt that re-fires forever at priority zero. So
  `oxinode::ble::Vbus` reads `USBREGSTATUS` on a 20 ms poll instead. It is one
  load, and it works.

### What is still owed, beyond the hang

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
