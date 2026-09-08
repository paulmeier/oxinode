# oxinode

RNode-compatible LoRa modem firmware in Rust for the
[muzi.works](https://muzi.works) **Base Duo** (Nordic nRF52840 + Semtech
LR1121), with the Super IO expansion board attached.

The goal is that a host running [Reticulum](https://reticulum.network/)'s `rnsd`
sees the board as an ordinary RNode over USB serial — no custom interface
driver, no patched Reticulum — and that the Super IO's OLED eventually shows
local status. It replaces the Meshtastic firmware the board ships with.

## Status: a provisioned RNode, over USB or paired Bluetooth, with settings on the panel, the panel's interface a crate of its own, and the GPS heard

Being precise about that:

| Phase | What it is | State |
|---|---|---|
| 0 | Pin/hardware recon | done, verified against upstream |
| 1 | Blink an LED, prove flashing | **running on hardware** |
| 2 | USB CDC-ACM enumeration | **running on hardware** |
| 3 | LR1121 bring-up over SPI, read chip ID, basic TX | **done** — verified on hardware, [notes](docs/phase-3-radio.md) |
| 4 | Runtime-configurable freq/SF/BW/power over `lr11xx` | **done** — verified on hardware, [notes](docs/phase-4-config.md) |
| 5 | RNode KISS protocol + command set, over USB | **done** — `rnsd` brings it up, [notes](docs/phase-5-rnode.md) |
| 6 | `rnodeconf` provisioning: EEPROM, device hash, signature | **done** — verified on hardware, [notes](docs/phase-6-provisioning.md) |
| 7 | SH1107 OLED status display | **done** — verified on hardware, [notes](docs/phase-7-display.md) |
| 8 | Bluetooth LE transport — the same KISS stream, for Sideband | **done** — iOS Sideband pairs with a passkey on the OLED and drives the radio, [notes](docs/phase-8-bluetooth.md) |
| 9 | An on-device interface, and a simulator to build it with | **done** — the shell is pure code and every screen has a golden image, [notes](docs/phase-9-simulator.md) |
| 10 | The navigation pad driver | **done** — verified on hardware: six switches, repeat, debounce, and the mode switch read in every position it has, [notes](docs/phase-10-pad.md) |
| 11 | The screens, drawn from real modem state | **done** — every screen renders from what the modem knows and says so where it knows nothing; read back from the board, [notes](docs/phase-11-screens.md) |
| 12 | Changing settings from the panel | **done** — every radio parameter editable with no host attached, validated as the host's are, refused not clamped, and stored in TNC mode; the two-controller question decided and written down, [notes](docs/phase-12-settings.md) |
| 13 | The interface as a device-agnostic crate | **done** — `monopanel`, a workspace crate with nothing of oxinode in it: a `Canvas` trait, a layout derived from the canvas, screens supplied by the application, an optional `embedded-graphics` adapter, and golden images at 128 × 64 as well as 128 × 128, [notes](docs/phase-13-interface-crate.md) |
| 14 | The GPS, and the Position screen | **heard on hardware** — the module powered through its load switch, NMEA at 9600 baud with the module's TX on P0.20, parsed in the core against captured sentences, and switchable from the mode switch and the menu; no fix yet, the board having sat indoors, and the current draw unmeasured, [notes](docs/phase-14-gps.md) |

The display comes before Bluetooth on purpose: BLE pairing needs somewhere to
show a six-digit passkey, and the OLED is that somewhere.

Phase 4 no longer goes through `lora-phy`. That crate is at 3.0.1 (July 2024,
marked *minimal maintenance*) and the `lr11xx` driver's `RadioKind` impl leaves
eight of its methods as `todo!()` — including everything a transmission needs.
The same crate's low-level API is complete, and an RNode wants raw LoRa PHY
rather than LoRaWAN, so oxinode builds on that directly. See
[docs/phase-3-radio.md](docs/phase-3-radio.md).

Phase 4 turned every radio parameter into a value a host can set, and cancelled
the 73 ppm reference error phase 3 measured. See
[docs/phase-4-config.md](docs/phase-4-config.md).

Phase 6 gave the board an identity that survives a reflash: `rnodeconf` writes
an EEPROM, signs the device, and stores a configuration it comes up on by
itself. See [docs/phase-6-provisioning.md](docs/phase-6-provisioning.md).

Phase 7 gave it a screen: a 128 × 128 OLED showing what the modem is doing, and
the RNode display protocol so a host can push pictures to it. See
[docs/phase-7-display.md](docs/phase-7-display.md).

Phase 8 gave it Bluetooth: the same KISS stream, over the Nordic UART
Service, into the same protocol core and the same radio. iOS Sideband finds
`RNode 7F23`, pairs with the six digits the panel shows, and brings the
interface online; the bond goes to flash and survives a reset, USB keeps
working alongside, and the panel says which host is on the line. See
[docs/phase-8-bluetooth.md](docs/phase-8-bluetooth.md) — including the bug
that stood between advertising and any of this, which took a captured
program counter to find.

Phase 9 is the on-device interface: screens, a navigation pad, and menus that
do something. Its shell is built as pure code, and so is the thing that makes
it buildable at all: a panel simulator that renders any screen to a PNG,
drives the menus from a script or the arrow keys, and holds every screen and
menu to a committed golden image. See
[Looking at a screen without a board](#looking-at-a-screen-without-a-board)
and [docs/phase-9-simulator.md](docs/phase-9-simulator.md).

Phase 10 is the navigation pad driver: six switches debounced and turned into
the six gestures the interface already understands, with auto-repeat on the
directions and none on OK or back, interrupt-driven and silent when nothing
is pressed. The timing lives in the core and is tested there; the driver
measures how the real switches bounce and logs it, so the debounce can be
settled against the board rather than guessed. It also turned up what
`embassy-nrf` does to `UICR.NFCPINS` when asked for P0.10 — see
[docs/phase-10-pad.md](docs/phase-10-pad.md) before flashing it.

Phase 11 fills the screens. Each one is handed plain values copied out of the
modem loop — which host has the line, the radio's configuration and whether it
took, the Bluetooth link and how many phones are bonded, the identity and the
free RAM — and draws them, reaching into nothing. A field that is not known is
a dash, never a plausible zero, and the `Position` screen — empty until phase
14 — said the GPS was not driven rather than placing the board in the Gulf of
Guinea. The battery
sense on P0.31 is read for the first time. Every screen has a golden image
empty and populated, and the render path is the same code on the board and in
the simulator. See [docs/phase-11-screens.md](docs/phase-11-screens.md).

Phase 12 lets the board be set up with no app attached. The Radio screen's
menu opens an editor for each of the five parameters a host sets — a stepper
for the ones with a fixed set, a digit editor for the frequency — and a
confirmed value goes through the same `ValidConfig` check a host's
configuration does, and is refused with the same reason rather than clamped.
Nothing is applied until it is confirmed and cancel leaves the old value in
place. The phase's real question was what to do when a host is connected,
since an RNode is host-controlled and Reticulum checks the radio exactly once;
the answer, from reading its interface code, is that **a live session on USB
or Bluetooth owns the live radio configuration**, and the panel says so
rather than changing it underneath. In TNC mode an edit goes into the stored
configuration, so it is what the board boots with, and survives a reflash
with the rest of the device record. See
[docs/phase-12-settings.md](docs/phase-12-settings.md).

Phase 13 takes the interface out of oxinode. Everything the panel shows is
drawn by `monopanel`, a workspace crate under `panel/` that depends on
nothing: a `Canvas` trait of three methods, a `Layout` derived from the
canvas size rather than hard-coded for 128 × 128, a navigator built over a
slice of screen descriptors the application supplies, and a `Modal` trait for
the third level. oxinode's own screens, menus, actions and editor stay in
`oxinode-core`, which consumes the crate through its public interface and no
other way; the SH1107 frame implements `Canvas` in twenty lines. The same
pages render on a 128 × 64 panel with four lines between the bars instead of
eleven and long menus windowed, and the simulator holds them to golden images
at both sizes. An optional `embedded-graphics` feature adapts the crate both
ways round, so it draws on any monochrome display driver in that ecosystem and
that ecosystem draws on any canvas of this one. See
[docs/phase-13-interface-crate.md](docs/phase-13-interface-crate.md).

Phase 14 drives the GPS. The Super IO's module sits behind a load switch on
P1.01 and on a UART the two sources named from opposite ends; the firmware
drives the switch, probes each pin order at each likely rate, and settled it
on the board: the module transmits on P0.20, at 9600 baud, within a second of
power. Framing, checksums, `GGA`, `RMC` and `GSV` are parsed in
`oxinode-core` against captured sentences, malformed ones included, and the
`Position` screen draws the receiver's state, satellites, time, date and
last fix with its age — dashes where it knows nothing, and `off`, `no data`,
`searching` or `fix` on its first row. The receiver follows the mode
switch's GPS ON position and the Position menu's `GPS On/Off`, and the UART
is released when it is off. A fix has not yet been seen from this desk, and
the current draw is not yet measured. See
[docs/phase-14-gps.md](docs/phase-14-gps.md).

Phases 1 to 8, 10, 11 and 14 are confirmed on hardware; phase 12's image boots
and serves a host, and its editors are held to golden images, but a pad walk
through them on the board has not been done from this desk. Phase 13 changes
no pixel on the board's panel -- every golden image from before it still
matches -- and the product image builds and links as it did; it has not been
reflashed for it.
What that actually establishes:

* the image links and boots at `0x26000`, so the S140 SoftDevice does forward to
  an application that never enables it;
* `embassy-time` keeps time, so the external 32.768 kHz crystal is running;
* the board enumerates as a CDC-ACM serial port (`oxinode RNode`, serial taken
  from the chip's factory device ID) and macOS binds the ACM driver to it;
* bytes survive a round trip — including the 64-byte packet boundary that needs
  a zero-length packet behind it, and the `0xC0`/`0xDB` bytes that KISS framing
  will later treat as delimiters;
* a 1200-baud open/close reboots the board into its bootloader, so reflashing
  needs no button press;
* defmt logs decode on a second serial port, which is the only diagnostic
  channel this board has — there is no debug probe;
* the SPI peripheral that will drive the LR1121 is configured, and reports back
  from its own `PSEL` registers that it claimed the four pins we meant;
* the LR1121 responds to a reset — pulling NRESET (P1.10) low drives BUSY
  (P1.11) high, and releasing it lets BUSY fall 191 ms later, repeatably to
  within one clock tick. That confirms both pins, since a GPIO that was not
  NRESET would not move a pin that was not BUSY;
* the radio answers `GetVersion` over SPI and identifies itself as an **LR1121,
  hardware `0x22`, transceiver firmware 1.1**, sitting in standby on its RC
  oscillator. That settles which of the two footprint-compatible Elecrow
  modules this board carries, and with it the whole pin map;
* its 32 MHz oscillator runs off the board's 3.0 V TCXO — `GetErrors` goes from
  `hf_xosc_start` to clear, and the die reads 18.5 °C at 3.36 V, which is a
  room rather than a stopped clock.

* **it transmits.** An unmodulated carrier at a commanded 915 MHz was measured
  on an RTL-SDR at −17, 0 and +14 dBm: 31 dB of commanded range produced 29.3 dB
  of measured range, all at one frequency to within 0.6 kHz. That is what proves
  the RF switch masks, which a `TxDone` alone never could.

* **interrupts reach the MCU.** The LR1121 raises `cmd_error` on its DIO9, the
  jumpered line lands on P1.08, an async wait wakes 152 µs later, and the line
  falls again when the interrupt is cleared. With nothing routed to DIO9 the
  same interrupt fires inside the chip and the pin stays down, which is what
  shows the mask is doing the work.

* **it sends LoRa packets.** SF7/125 kHz/CR 4/5, 16-byte payload: `TxDone`
  arrives on the interrupt line 51910 µs after `SetTx` against 51456 µs of
  computed airtime, and the SDR sees a 52.00 ms burst in the channel.

* **it receives.** Pointed at a second Base Duo running stock Meshtastic, it
  demodulates real packets — the broadcast header and the peer's own node
  number, RSSI −45 dBm, SNR 11 dB.

* **it can be configured while it is running.** Frequency, bandwidth, spreading
  factor, coding rate, power, preamble and sync word are a value rather than
  constants, validated before they reach the chip. Pressing one key retunes the
  board to the peer's Meshtastic channel and it demodulates real packets;
  changing the spreading factor by one step, or the sync word, drops it to zero
  — which is what shows the settings are reaching the radio rather than the
  reception being a coincidence.

* **the airtime it predicts is the airtime it takes.** Across four
  configurations spanning a 3.5× range, measured `TxDone` exceeds computed
  airtime by a *constant* 437–461 µs, which is the `SetTx` transaction, PLL lock
  and PA ramp. A wrong formula would scale; this does not.

* **Reticulum brings it online.** Unmodified `rnsd`, pointed at the serial
  port, runs its detect handshake, configures the radio, validates that what
  came back matches what it asked for, and reports `Status: Up` at 3.12 kbps —
  which is its own arithmetic agreeing with the bitrate `oxinode-core` computes.
  Ten starts, ten times up. A `CMD_DATA` frame containing both KISS framing
  bytes goes out as a 19-byte packet in 103363 µs against 102912 µs computed.

* **`rnodeconf` provisions it, and the provisioning sticks.** Unmodified
  `rnodeconf --rom` writes an identity into the board one byte at a time, reads
  the whole image back, recomputes the MD5 over it and verifies a 1024-bit RSA
  signature: *`EEPROM checksum correct` / `Device signature validated`*. The
  dump was parsed independently in Python too — 256 bytes, the checksum over
  bytes 0–10 matching the sixteen stored at `0x0b`, the lock byte at `0x9b`.
  `rnodeconf --sign` gets a device hash that is SHA-256 over the identity block
  and the chip's factory device ID, recomputed byte for byte on the host.

* **it comes up as a TNC on its own.** With a configuration stored by
  `rnodeconf --tnc`, a reset takes 421 ms to reach a configured radio with no
  host attached — the record read back from `0xea000`, validated the same way a
  host's request would be, and commanded at 915,067,069 Hz, which is phase 4's
  reference correction applied to a frequency that came out of flash.

* **it has a screen, and the host can draw on it.** The Super IO's 128 × 128
  SH1107 shows frequency, bandwidth, spreading factor, power, radio state,
  provisioning and packet counters, updated twice a second and never blocking
  the radio for more than 28 ms. A 64 × 64 picture pushed over `CMD_FB_WRITE`
  reads back byte-identical, and `CMD_DISP_READ` returns a screen in which all
  8192 pixels agree with that picture doubled across and folded back — which
  checks the whole display path without anybody looking at it.

* **the 73 ppm is corrected, by the amount it should be.** Turning the
  correction on moves the receive window's edge against the peer board from
  +328 kHz to +254 kHz — a shift of −74 ± 14 kHz against −66.5 kHz predicted —
  and turning it off moves it back. A correction applied at half strength or
  twice would have landed several uncertainties away.

**The 73 ppm error belongs to the module, not to this board.** Sweeping the
receive frequency against the second board gives a reception window symmetric
about zero — so both boards are off by the same amount, and replacing the board
would not fix it.

That is exactly what makes a software correction the right answer rather than a
workaround: the error is a stable property of the part. **Phase 4 corrects it**,
in `oxinode_core::lr1121::reference`, as integer arithmetic in tenths of a ppm.
Whether to apply it is a configuration field and not a constant, because
corrected this board is right in absolute terms and 73 ppm away from every other
nRFLR1121 — including the one on the bench next to it. The default is corrected,
because an RNode's peers are other RNodes.

**The transmitter is 73 ppm low**, and that is measured rather than suspected:
chopping between two carriers inside a single capture separates the
transmitter's clock error from the receiver's, and puts −73.3 ± 0.5 ppm on the
LR1121 and −1.4 ppm on the SDR. At 915 MHz that is 67 kHz — over half a 125 kHz
LoRa channel — and it has to be understood before phase 5.

It is **not** the TCXO supply voltage (swept all eight codes; the oscillator
starts on every one and the frequency does not care) and **not** a crystal being
driven in the wrong mode (without `SetTcxoMode` the oscillator does not start at
all). What is left is the module's own reference, which also drifts ≈0.65 ppm/°C
— roughly twenty times a TCXO's stability, so it probably is not one. That drift
bounds what a static correction can do: 20 °C of temperature swing is 13 ppm, a
fifth of what is being corrected.

There is no Bluetooth code: not stubbed, not half-written, absent.

## Hardware

Board: muzi.works Base Duo + Super IO expansion. Factory bootloader, confirmed
from the board's own `INFO_UF2.TXT`:

```
UF2 Bootloader 0.9.2-37-gf7fad14
Model: muzi Base
Board-ID: muzi-Base-Board
SoftDevice: S140 6.1.1
```

The nRF52840 and the LR1121 are not two chips on the PCB. They are one part:
U1 is an **Elecrow nRFLR1121**, an 80-pin 20 × 20 × 3.5 mm module containing
both, with the SPI link between them routed *inside* the module. This is not
trivia — see [Things to be careful about](#things-to-be-careful-about) — and it
is why the pins below are fixed facts rather than board choices.

Pin assignments come from the board's Meshtastic variant definition
(`variants/nrf52840/muzi_base/variant.h`), cross-checked against the Rev 01
schematic (`Base Duo [MH212A]`) and the module datasheet. Only the LED is wired
up in code so far; the rest is recorded here so the later phases have something
to work from.

| Function | Pin(s) | Notes |
|---|---|---|
| LED green (`PIN_LED1`) | P1.03 | **active low** |
| LED blue | P1.04 | active low |
| LR1121 IRQ | P1.08 | from the module's `LR_DIO9` pin, over a board net named `IRQ_JUMPER` |
| LR1121 NRESET | P1.10 | module-internal |
| LR1121 BUSY | P1.11 | module-internal |
| LR1121 SPI NSS / SCK / MOSI / MISO | P1.12 / P1.13 / P1.14 / P1.15 | module-internal |
| LR1121 TCXO | — | 3.0 V via DIO3, which therefore cannot serve as an IRQ |
| LR1121 RF switch | — | the chip's own DIO5/DIO6, set by an on-chip command, not MCU GPIO |
| OLED I²C SDA / SCL | P0.24 / P0.25 | SH1107 at **0x3c**; 128 × 128, 1.12"; 5.1 kΩ pull-ups on board |
| OLED 12 V boost enable | P0.23 | must be driven **high** or the panel is dark |
| QSPI SCK / CS / IO0-3 | P0.03 / P0.26 / P0.30, P0.29, P0.28, P0.02 | W25Q128, 16 MB |
| LF clock | — | external 32.768 kHz crystal (LFXO) |
| Navigation pad up / down / left / right | P0.21 / P0.17 / P1.05 / P0.16 | active low, internal pull-ups; auto-repeat |
| Navigation pad OK / back | P0.10 / P0.15 | active low; **P0.10 is an NFC pin**, see below |
| Power OFF / Power ON / GPS ON switch | P1.09 / P0.12 | P1.09 high in Power ON, P0.12 high in GPS ON, no pull; Power OFF cuts the board's power |
| GPS load switch (`GPS_EN`) | P1.01 | **active high**; gates the switched 3V3 rail the module lives on, 500 mA |
| GPS UART | P0.20 / P0.19 | the module transmits on **P0.20** (nRF52840 `RXD`) and receives on P0.19; 9600 baud, 8N1; settled on the board, see below |
| Battery sense | P0.31 (`AIN7`) | the cell through 806 kΩ / 1.5 MΩ, ratio 0.65048; read by the SAADC at gain 1/6, 12-bit |
| Charger status | P1.02 | BQ25185 `STAT`, open drain, **low while charging**; internal pull-up |
| SWDIO / SWDCLK | — | test pads TP1 / TP2, no header |

Out of scope for now, recorded so nobody has to re-derive it: second I²C bus
P0.04/P0.06 (IMU, RX8130CE RTC at `0x32`, *and* the Qwiic/STEMMA QT connector,
5.1 kΩ pull-ups on board) and the BQ25185 charger's fault output on P0.27.

Two of those pins are not what their Meshtastic names suggest:

* **P1.01 (`GPS_EN`) and P0.22 (`PIN_BUZZER`) are load-switch enables**, not
  peripheral pins. Each drives an NMOS that gates a high-side PMOS feeding a
  switched 3V3 rail out to the expansion connector, so both are **active high**,
  and P0.22 powers the buzzer rather than sounding it. 500 mA per switch, 600 mA
  total on the 3.3 V rail.
* **The GPS UART is named from opposite ends** in the two sources: the schematic
  labels P0.20 `UART_GPS_TX` and P0.19 `UART_GPS_RX`; the variant declares
  `GPS_RX_PIN` P0.20 and `GPS_TX_PIN` P0.19. Read each from its own end they
  agree, and phase 14 confirmed it on the board: the module's TX arrives on
  P0.20, so that is the nRF52840's `RXD`. The firmware still probes both
  orders rather than trusting either name.

Everything above the Base Duo itself — navigation pad, buzzer, GPS, OLED, mode switch
— lives on the Super IO board and reaches it through 25 castellations (`JC1`–
`JC25`) carrying VBAT+, a solar input, the two switched rails, the second I²C
bus, the OLED bus and eight general-purpose IOs. The Base Duo just brings pins
out.

There is also a **third LED**: a red one wired to the charger's status output
rather than to the MCU. It is not firmware-controllable, so a red glow during
bring-up means charging, not a fault in anything oxinode did.

### Things to be careful about

* **The QSPI flash is 16 MB — probably.** Three sources disagreed: the variant
  declares `W25Q32JVSS` (4 MB), the product page says 8 MB, an owner reported
  16 MB. The Rev 01 schematic settles it in the owner's favour: U5 is a
  **W25Q128JVPIQ**, 128 Mbit. Since the variant is demonstrably wrong and the
  schematic covers one revision, phase 3+ should still read the JEDEC ID at
  runtime — but now it knows what answer to expect.
* **It is a navigation pad, not a trackball, whatever the variant calls it.**
  Meshtastic declares `HAS_TRACKBALL 1` and names the five lines `TB_UP`,
  `TB_DOWN`, `TB_LEFT`, `TB_RIGHT` and `TB_PRESS`, so reading the variant alone
  leaves you expecting a ball. muzi's own specification for the Super IO lists
  "Navigation Pad Buttons + OK + Back", and the board has six discrete
  switches. The pin numbers are the same either way; the *driver* is not. A
  trackball emits a burst of edges as the ball rolls and is read by counting
  them, which is why the variant sets `TB_DIRECTION FALLING`. A pad emits one
  edge and then a level that lasts as long as a finger does, and wants
  debouncing and an auto-repeat instead. Counting edges from a pad gives one
  step per press and no repeat; watching levels on a ball gives a runaway
  cursor. Six buttons, all active low: up P0.21, down P0.17, left P1.05,
  right P0.16, OK P0.10, back P0.15.
* **P0.10 is an NFC pin, and it is the user button.** P0.09 is unconnected;
  P0.10/NFC2 carries `USR_BTN` (SW1, active low, 100 kΩ pull-up) and the
  pad's OK switch. NFC pins only work as GPIO once the `PROTECT` bit of
  `UICR.NFCPINS` is cleared — a non-volatile change, not a runtime register
  write. Meshtastic does it with `CONFIG_NFCT_PINS_AS_GPIOS=1`, so on a board
  that shipped running Meshtastic it is expected to be done already. Since
  phase 10 the firmware builds `embassy-nrf` with `nfc-pins-as-gpio`, which
  is the only way it names P0.10 at all — and which makes `embassy_nrf::init`
  clear that bit itself if it is set: a single masked word write (1→0 only,
  no erase, `REGOUT0` untouched) and one reset. If the bit is already clear
  that is a no-op. The product image reports both words at every boot:

  ```
  board: nav pad usable=true (UICR.NFCPINS), regulator=3.3 V
  ```

  **Read on hardware, 2026-09-07:** `usable=true`, `regulator=3.3 V`. The bit
  was already clear, as expected from a board that shipped running Meshtastic,
  so the first boot of the phase 10 image wrote nothing and `REGOUT0` was
  never at risk. See [docs/phase-10-pad.md](docs/phase-10-pad.md) for the
  whole of it.
* **`USE_SX1262` and `USE_LR1121` are both defined because there are two
  modules, not two wiring options.** Elecrow's **nRFLR1121** (nRF52840 +
  LR1121) and **nRFLR1262** (nRF52840 + SX1262, despite the name) share a
  footprint, so one PCB accepts either. On the SX1262 module the LoRa DIO pins
  and the 2.4 GHz antenna port are simply NC, and its IRQ surfaces on P1.06
  instead of P1.08. Phase 3 should still read the chip ID; oxinode targets the
  LR1121, which is what this board's schematic populates.
* **Most of the LoRa pins do not exist outside the module.** P1.06 and
  P1.10–P1.15 are absent from the nRFLR1121's 80-pin table — SPI, NRESET, BUSY
  and the SX1262 interrupt are all module-internal. Exactly three LR1121 pins
  come out: `LR_DIO9`/IRQ, `LR_DIO8`, `LR_DIO7`. The datasheet's reference
  design requires `LR_DIO9` to be jumpered to an MCU GPIO, and the Base Duo
  jumpers it to P1.08. That is the whole story behind the interrupt pin, and it
  follows from DIO3 being occupied by the 3.0 V TCXO reference.
* **The module is rated below the chip.** 20 dBm max sub-GHz and 11.5 dBm max
  at 2.4 GHz, against the LR1121's headline 22/13 dBm and Meshtastic's 22/13
  clamps. `oxinode_core::lr1121::config` clamps to the module's numbers, and
  refuses rather than clamping quietly: a host that asks for 21 dBm is told no,
  because a silent clamp is a lie it cannot detect.
* **There is no DFU button and never was.** The bootloader's `BUTTON_1` and
  `BUTTON_2` are both P0.05, commented "Unconnected pin", and P0.05 is indeed
  unconnected on the schematic. Double-tap reset and the GPREGRET software path
  are the only two ways in — a button-hold recovery does not exist here.
* **`UICR.REGOUT0` is already 3.3 V.** The bootloader programs it
  (`UICR_REGOUT0_VOUT_3V3`), so an oxinode image inherits 3.3 V rather than the
  1.8 V reset default. Nothing to do; worth not being surprised by.
* **The 40 KB above the application is where provisioning lives.** Phase 6 puts
  the device record at `0xEA000`, in the region the bootloader reserves and
  then refuses to write through on both of its flashing paths. That is what
  makes an identity survive a reflash. Do not move `memory.x`'s FLASH length
  without moving it: the address is derived from the end of the application
  region, so growing the application would relocate the record and lose it.
* **`rnodeconf` cannot tell this board's two serial ports apart.** It ends its
  bootstrap by resetting the device and finding the port again by matching USB
  serial numbers, taking the first match — but a USB serial number belongs to
  the device, not to an interface, so both CDC functions carry the same one and
  the order is not stable. When it picks the log port, `--rom` ends with
  "Could not download EEPROM from device" after having succeeded. `rnodeconf -i`
  shows the truth. There is nothing the firmware can do about it that would not
  be worse — see [docs/phase-6-provisioning.md](docs/phase-6-provisioning.md).
* **`rnodeconf -i` prints this board's band and power from its own table, not
  from the device.** For model `0xff` that is "100.0 MHz - 1100.0 MHz" and
  "Max TX power: 14 dBm". The module is 902–928 MHz at 20 dBm and
  `oxinode_core::lr1121::config` is what enforces it. The model byte was chosen
  so `rnodeconf --update` refuses rather than offering to flash a RAK4631 image
  onto an LR1121; being wrong about the band in a printout is the cheaper of
  the two.
* **The nRF52's TWIM locks up after a NACK, and lies rather than failing.** A
  transaction that ends in an address NACK can leave the peripheral in a state
  it does not come out of, and `embassy-nrf` implements no workaround. It does
  not report an error — it produces *plausible* answers: the same address
  answers a read one moment and not the next, a scan disagrees with a probe run
  half a millisecond earlier. `oxinode::display::reset_peripheral` cycles
  `ENABLE`, which clears it; anything that NACKs by design (a bus scan) must do
  that after every transaction.
* **The 1200-baud touch is state on the *host*.** macOS caches terminal
  settings per device path and re-applies them, so a failed touch leaves the
  port at 1200 — and then anything that opens it, including `cat` reading the
  log, performs another touch and sends the board back to its bootloader.
  `tools/dfu-flash.sh` resets the cached rate after every flash, and firmware
  ignores the condition for the first two seconds after boot.
* **There *are* SWD pads.** This README says repeatedly that the board has no
  debug probe, and no image here assumes one. But SWDIO and SWDCLK come out to
  test pads TP1/TP2, so if the no-probe constraint ever gets expensive enough,
  it is solderable rather than impossible.

## Bluetooth

The target is that [Sideband](https://unsigned.io/sideband/) connects to this
board over Bluetooth the way it connects to any other RNode. What that requires
is now pinned down, in `core/src/ble.rs`, with tests.

**Bluetooth on an RNode is not a protocol of its own.** It is a second pipe
carrying the identical KISS byte stream that the USB serial port carries.
Reticulum's Android `RNodeInterface` connects to a **Nordic UART Service**
peripheral and speaks exactly what it would speak down a wire:

| | |
|---|---|
| Service | `6e400001-b5a3-f393-e0a9-e50e24dcca9e` |
| RX (host writes → us) | `6e400002-b5a3-f393-e0a9-e50e24dcca9e` |
| TX (we notify → host) | `6e400003-b5a3-f393-e0a9-e50e24dcca9e` |
| Device name | must start with `RNode ` — that is the discovery filter |
| MTU | negotiated up to 512, capped by the 512-byte attribute ceiling |

The nRF52840 has no Bluetooth Classic radio, only LE, so this board is a BLE
RNode. In Sideband that means **Hardware → RNode → "Device requires BLE"** has
to be ticked; the classic-Bluetooth/SPP path used by ESP32 RNodes does not
apply.

### Which BLE stack

`trouble-host` + `nrf-sdc`, not the S140 SoftDevice already sitting in flash.

`nrf-softdevice` — the crate that would drive S140 — was last released in
January 2024. `trouble-host`, `nrf-sdc` and `nrf-mpsl` were all released within
the last month. For a stack this load-bearing, that difference decides it.

Using `nrf-sdc` means S140 stays in flash, unused, exactly as it is today: we
keep linking at `0x26000` and keep the bootloader. It is 148 KB we were not
using anyway.

### What this costs, and it is not nothing

* **`nrf-mpsl` claims `RTC0`, `TIMER0`, several PPI channels, and the highest
  interrupt priorities.** `embassy-time` here runs on `RTC1`, so that part is
  already clear.
* **`CLOCK_POWER` is shared.** `POWER` and `CLOCK` have one vector and one
  interrupt-enable word between them, and MPSL's clock handler knows nothing
  about `POWER`. An image serving both USB and BLE binds one handler that does
  both jobs, and clears what the bootloader left enabled before MPSL unmasks
  the vector. Done, and it cost a week: see the phase 8 notes.
* **Pairing is not optional.** The stock firmware requires LE Secure
  Connections with MITM protection, generates a six-digit passkey, and
  explicitly refuses "Just Works". So does this one: both characteristics
  demand an authenticated link, the board is `DisplayOnly`, and the passkey
  goes on the OLED — which is why phase 7 came first.

### Why it is phase 8 and not phase 3

Sideband does not merely open the pipe. It expects an RNode on the other end and
runs a detect/version/configuration exchange before it will use the device.
Until the protocol exists there is nothing to answer with, so a BLE build today
would give you a board that advertises as `RNode XXXX`, accepts a connection,
and then goes nowhere.

Bringing the transport up early was considered and rejected: with no debug probe
on this board, the useful thing is to keep the number of unproven layers small,
and BLE cannot be proven against a real client until there is a real RNode
behind it.

The interoperability constants — the service and characteristic UUIDs, the
`RNode ` name prefix the host scans for, the static random address, and the MTU
arithmetic — are in `core/src/ble.rs` with tests. The stack itself is in
`src/ble.rs` and `src/bin/ble.rs`, and is where phase 8 currently stands: see
[docs/phase-8-bluetooth.md](docs/phase-8-bluetooth.md).

The Bluetooth build is a **separate feature set**, not an extra feature,
because it swaps the `critical-section` implementation for the whole image:

```bash
cargo build --release --no-default-features --features ble --bin ble
```

`cortex-m`'s single-core implementation masks every interrupt, which is right
for phases 1 to 7 and wrong the moment MPSL is running — the link layer keeps
its timing on `RADIO`, `RTC0` and `TIMER0`. `src/lib.rs` refuses a build that
enables both, and `tools/test.sh` lints and builds each separately.

One licensing note: `nrf-sdc-sys` vendors Nordic's SoftDevice Controller as a
binary archive under `LicenseRef-Nordic-5-Clause`, which permits use on Nordic
silicon. It is the one part of oxinode that is neither MIT nor Apache-2.0 and
neither is nor could be built from source.

## Flashing

Flashing goes over the bootloader's **serial DFU** interface, not by dropping a
`.uf2` on its drive.

```bash
rustup target add thumbv7em-none-eabihf
cargo install cargo-binutils && rustup component add llvm-tools
```

Double-tap the board's reset button, then:

```bash
cargo run --release --bin blink
```

The product image carries the Bluetooth stack and so needs that feature set:

```bash
cargo run --release --no-default-features --features ble --bin rnode
```

`cargo run` builds the ELF, checks it against `memory.x`, packages it, and sends
it down `/dev/cu.usbmodem*`. It bootstraps `adafruit-nrfutil` into
`tools/.venv/` on first use if the tool is not already on your PATH.

Once an image with USB serial is on the board (`usb-cdc`, and everything from
phase 5 on), no button press is needed: the runner performs a 1200-baud touch,
which those images answer by rebooting into the bootloader.

Recovery is always double-tap reset. The bootloader lives above the application
and is never overwritten.

### Why not the UF2 drive

The bootloader does expose one, and `tools/uf2-flash.sh` still drives it for
hosts where it works. It does not work on macOS 26.

That drive is not a real FAT volume. It is a synthetic filesystem that exists
only to catch 512-byte UF2 blocks and write them to flash. macOS 26 serves FAT
through FSKit in userspace, and against a shim like this one the writes never
reach the chip — they land in a host-side cache. The copy succeeds, the file
appears in `ls`, the board keeps running its old firmware, and nothing anywhere
reports an error.

There is a second, independent trap in the same path. **A UF2 bootloader
silently discards any block whose family id it does not recognise**, then never
reaches its expected block count and so never reboots — again with no error.

This bootloader accepts two application family ids: `0x239A0081`, formed from
the board's USB VID and PID, and `0xADA52840`, the generic Adafruit nRF52840
id. Neither is the family id microsoft/uf2 actually
[registers](https://github.com/microsoft/uf2/blob/master/utils/uf2families.json)
for the nRF52840, which is `0x1B57745F` and which this board rejects. A third
id, `0xD663823C`, is accepted too and **rewrites the bootloader itself** — never
emit that one.

`uf2-flash.sh` reads the id from the board's own `CURRENT.UF2` rather than
trusting a constant. That yields `0x239A0081`, because the bootloader stamps its
synthetic `CURRENT.UF2` with the board-specific id. Reading it from the board
stays the right approach — it is the only source that cannot drift — but the
generic id would work as a fallback if it ever came to that.

### Checking what is actually on the chip

The bootloader publishes the chip's current flash contents as `CURRENT.UF2`, so
"did that flash take?" is answerable rather than a guess about LED colours.
Double-tap reset, then:

```bash
tools/verify_flash.py target/thumbv7em-none-eabihf/release/blink
```

It prints the vector table found on the board next to the one you built, and
says MATCH or MISMATCH.

### Why the image is linked at `0x26000`

The board ships with a Nordic S140 SoftDevice occupying `0x1000`–`0x26000` and
the Adafruit bootloader at the top of flash. oxinode does not use BLE and never
enables the SoftDevice, but it must still link above it, because the MBR hands
control to the SoftDevice, which forwards to whatever is at `0x26000`. Linking
at `0x0` would work exactly once and cost you the bootloader. See `memory.x`.

Because we boot from behind the SoftDevice, `boot::relocate_vector_table()` sets
`VTOR` to our own vector table before any interrupt is enabled. This is
belt-and-braces: the SoftDevice may already do it, but the cost is one store.

### Why the application region stops at `0xEA000`, not `0xF4000`

The bootloader's own code starts at `0xF4000`, and it would be natural to give
the application everything below that — 824 K. That is wrong, and it was wrong
here until the bootloader source was checked.

The nRF52840 build of this bootloader sets `DFU_APP_DATA_RESERVED` to ten flash
pages — 40 K, commented "to match circuitpython for 840" — and both of its
flashing paths enforce the resulting `USER_FLASH_END` of `0xEA000`:

* serial DFU, which is how oxinode flashes, rejects any image larger than
  `DFU_IMAGE_MAX_SIZE_FULL = (0xF4000 - 0x26000) - 0xA000 = 0xC4000`;
* the UF2 drive's `write_block()` silently drops any block failing
  `in_app_space()`, which is `addr < USER_FLASH_END`.

So the real ceiling is **784 K**. oxinode puts no filesystem in that 40 K, but
the reservation is compiled into the bootloader sitting on the board, so it
binds us anyway. The failure mode is milder than overwriting the bootloader —
an over-large image simply cannot be flashed — but it is still a build that
looks fine and then does not work.

Two nearby numbers are worth not being misled by:

* **824 K** (`0xF4000 - 0x26000`) is where the bootloader's *code* begins. It
  is the right answer to "what must I not overwrite" and the wrong answer to
  "how big can my image be".
* **815 104** (`0xED000 - 0x26000`) is what Meshtastic declares for this board.
  It is not a muzi-specific figure: it comes verbatim from the Adafruit Arduino
  BSP's `nrf52840_s140_v6.ld`, which reserves 28 K where the bootloader
  reserves 40 K. The BSP and the bootloader that has to accept its output
  disagree by 12 K. Trust the bootloader.

## Layout

```
memory.x              flash/RAM layout; the single source of truth for the load address
build.rs              installs memory.x and re-exports its FLASH origin to Rust
core/                 oxinode-core: logic with no hardware dependency, unit tested on the host
core/src/lr1121/config.rs     phase 4: the settable parameters, validated before they reach the chip
core/src/lr1121/reference.rs  phase 4: the module's 73 ppm error, and the arithmetic that cancels it
src/modem.rs          phase 4: the one place that programs a configuration, transmits and receives
core/src/rnode/       phase 5: KISS framing, the command set, the protocol state machine
core/src/rnode/eeprom.rs      phase 6: the EEPROM image, as rnodeconf reads it
core/src/rnode/store.rs       phase 6: the record that survives a power cycle, and its checksum
core/src/hash/        phase 6: MD5 (because the EEPROM checksum is one) and SHA-256
core/src/sh1107.rs    phase 7: the OLED controller's commands and framebuffer -- and, since phase 13, its `Canvas`
src/ble.rs            phase 8: MPSL, the SoftDevice Controller, and what they take away
src/bin/ble.rs        phase 8: the bring-up image, built to be debugged without a probe
core/src/status.rs    phase 7: the status page, rendered from a value
panel/                phase 13: monopanel, the interface as a crate with nothing of oxinode in it -- no_std, no dependencies
panel/src/canvas.rs   the `Canvas` trait: width, height, set a pixel; and a `Bitmap` for tests
panel/src/layout.rs   where the chrome goes, derived from the canvas size
panel/src/nav.rs      the navigation model over application-supplied screens, and the `Modal` trait
panel/src/draw.rs     the title bar, the icon strip, the menu, the scrollbar, and the page
panel/src/font.rs     phase 7's 5x7 font, drawn as art and generated into a table
panel/src/eg.rs       the optional embedded-graphics adapter, both ways round
core/src/ui.rs        phase 9: oxinode's screens, menus and actions, and its editor as the crate's modal
core/src/screens.rs   phase 11: what each screen knows, and the lines it draws from that
core/src/edit.rs      phase 12: editing one radio parameter -- the steppers, the digits, the refusal
core/src/pad.rs       phase 10: the pad as a state machine -- debounce, auto-repeat, the numbers
src/pad.rs            phase 10: the pad driver -- six pins, the PORT interrupt, the channel; the mode switch
sim/                  phase 9: oxinode-sim, the panel simulator -- host only
sim/src/panel.rs      a canvas of any size, for the panels the board does not have
sim/src/image.rs      a canvas as a PNG at 4x with a pixel grid, and the diff between two
sim/src/script.rs     `right right select down select`: an input script
sim/src/scene.rs      a navigator plus what a page borrows, rendered as the board will
sim/src/text.rs       a frame as braille or half blocks, for a terminal
sim/src/tty.rs        the interactive mode: arrow keys against the real menu tree
sim/src/golden.rs     the golden-image set and the comparison
sim/golden/           the committed images: every screen empty and populated, every menu item
sim/golden/128x64/    the same screens on the other common panel
core/src/rnode/display.rs     phase 7: the host's framebuffer and display readback
src/display.rs        phase 7: the I2C bus, the 12 V rail, and the panel transport
src/bin/display.rs    phase 7: the display bring-up image
src/store.rs          phase 6: that record, in the flash a reflash cannot reach
src/bringup.rs        phase 5: the radio bring-up sequence, without the instrumentation
src/bin/rnode.rs      phase 5: the product image -- KISS on the first port, log on the second
src/lib.rs            firmware crate root; panic handler
src/board.rs          board facts: clock config, LED polarity
src/boot.rs           VTOR relocation, reboot-into-bootloader
src/bin/blink.rs      phase 1
src/bin/usb_cdc.rs    phase 2
src/bin/radio.rs      phases 3 and 4: radio bring-up and the configuration console
tools/test.sh         every check that does not need a board
tools/dfu-flash.sh    cargo runner: ELF -> DFU package -> serial (primary)
tools/verify_flash.py compares the chip's actual contents against a built image
tools/package.sh      builds the files that go on a GitHub Release
tools/release_notes.sh  body text for a release
tools/uf2-flash.sh    cargo runner: ELF -> UF2 -> bootloader drive
tools/uf2conv.py      UF2 writer, from the format spec
tools/layout.py       memory.x reader shared by the host tools
tools/check_layout.py validates a built image against memory.x
tools/crate_version.py  reads the version out of Cargo.toml
tools/test_*.py       tests for the above
.github/workflows/    ci on every push; release on every v* tag
```

## Tests

```bash
tools/test.sh
```

That is what CI runs. It covers formatting, lints, unit tests and an image
check; on a clean tree it takes a few seconds.

The firmware crate itself has no unit tests and will not get any. It depends on
`embassy-nrf`, which only builds for `thumbv7em-none-eabihf`, and with no debug
probe there is nowhere to run a test harness. Pretending otherwise would mean
writing tests against mocks of a HAL, which proves the mock works. Instead the
project is arranged so that the parts worth testing are testable:

* **`monopanel`** is the interface itself, with nothing of oxinode in it. It
  is tested on a bitmap at two panel sizes, with and without its one optional
  feature, and the tests are where the layout rules live: the chrome never
  overlaps the content, a menu that does not fit is windowed around its
  highlight, a scrollbar appears exactly when a page does not fit.
* **`oxinode-core`** holds everything decidable without a peripheral, builds for
  the host, and is unit tested there. It is thin today — phases 0–2 are mostly
  register pokes — but phase 5's KISS framing is nearly all pure byte
  manipulation and belongs here.
* **`oxinode-sim`** renders the interface on the host and compares every
  screen and menu against the golden images in `sim/golden/`, at the board's
  128 × 128 and again at 128 × 64. The pixel assertions in `oxinode-core` say
  where nothing is drawn; a golden image says what it looks like, and a
  mismatch fails the run and leaves a diff picture in `target/golden-diff/`.
  If the change was meant, regenerate and commit:

  ```bash
  cargo run -p oxinode-sim --target "$(rustc -vV | sed -n 's/^host: //p')" -- golden --update
  ```

* **The host tooling** (UF2 writer, `memory.x` reader, image checker) is tested
  directly. A malformed UF2 does not crash; it produces a board that quietly
  does not run what you flashed.
* **The built images** are checked against `memory.x` on every build and again
  before every flash: every loadable byte inside FLASH, RAM usage inside RAM,
  the vector table's initial stack pointer pointing into RAM, and the reset
  vector pointing into FLASH with the Thumb bit set. Those last two are the
  words the CPU reads first, and they are exactly what is wrong when a linker
  script is wrong.

`memory.x` is parsed twice — once in Rust for the build, once in Python for the
tools — because the firmware build cannot shell out to Python and the tools
should not have to link Rust. Both suites pin the same expected addresses
against the real file, so a divergence fails one of them.

The checker's own tests mostly feed it deliberately broken images (linked at
`0x0`, overrunning the bootloader, stack pointer in flash, missing Thumb bit)
and assert that it rejects each one. A check that cannot fail is worse than no
check, because it gets believed.

## Looking at a screen without a board

The interface is pure code, so it can be looked at on the host. `oxinode-sim`
is a second host crate, and like `oxinode-core` it needs the host target named,
because the workspace defaults to the Cortex-M:

```bash
alias sim='cargo run -q -p oxinode-sim --target "$(rustc -vV | sed -n "s/^host: //p")" --'
```

**A picture of a screen.** A script is the keys you would press, and the
result is a PNG at 4× with a visible pixel grid, which at 128 × 128 reads
better than the panel does:

```bash
sim render --script "right*4 select down" -o system-reboot.png
```

**A picture per step**, to see a path through the menus as a strip:

```bash
sim steps --script "right select down select" --out /tmp/steps
```

**In the terminal.** Arrow keys move, Enter selects, Esc or Backspace goes
back, `q` quits. The panel is drawn in braille, or in half blocks on a terminal
tall enough for 64 rows of them. The actions a menu item would fire are shown
on the status line rather than performed, exactly as the core hands them to
the firmware:

```bash
sim tty
```

**What the screens draw from.** Since phase 11 the screens draw from a
`State` — the same plain values the board copies out of its modem loop — and
the simulator has three fixtures for it. `--state populated`, the default, is a
board mid-session with every field known and a host on the line; `--state
empty` is one that knows nothing yet, so every screen shows how it says so;
`--state standalone` is a TNC with no host attached, which is the one whose
settings the panel may change. The populated radio screen is longer than the
panel, which is what exercises scrolling and the scrollbar:

```bash
sim render --script "right down*3" -o radio-scrolled.png
sim render --state empty --script "right*4" -o system-empty.png
```

**Editing a setting.** The Radio menu's first five items open an editor.
The scene plays the board's part: with nobody on the line it opens the
editor and a confirmed value lands in the state, so the next picture shows
it; with a host on the line it opens the notice instead. The fourth press
here is `TX Power`, `up*4` takes 17 dBm to 21, and the last `select` is
refused:

```bash
sim render --state standalone --script "right select down*5 select up*4 select" -o refused.png
sim render --script "right select down select" -o locked.png
```

**Another panel.** The interface draws on any size of canvas, and the
simulator can show what the same screens look like on one the board does not
have. `--panel WxH` renders on it; 128 × 64 is the size the golden set is
also held at, with four lines between the bars and the nine-item Radio menu
shown two rows at a time:

```bash
sim render --panel 128x64 --script "right select down*4" -o radio-menu-wide.png
```

**A frame from the board.** A 2048-byte dump of the controller's RAM, as the
RNode display-read command returns it, renders the same way:

```bash
sim raw frame.bin -o frame.png
```

**What the simulator does not tell you** is anything electrical: button
debounce and auto-repeat, I²C timing, the panel's own refresh. Those are the
board's. The pad's timing is [phase 10](docs/phase-10-pad.md), tested in the
core with a clock that is a number and measured on the board by the driver;
the simulator's input script is the gestures *after* that driver.

## Releases

Prebuilt `.uf2` files are attached to every [tagged
release](https://github.com/paulmeier/oxinode/releases). Download one, double-tap
reset, and drop it on the drive that appears — no toolchain needed.

Every push also uploads the same files as a build artifact on the CI run, which
is the easier way to try an unreleased change on a board.

### Cutting a release

Releases are built only from tags matching `v*`. Nothing else can produce one:
there is no manual trigger and no branch build that publishes.

```bash
# 1. Bump the version in Cargo.toml, commit it.
# 2. Tag that commit and push the tag.
git tag v0.1.0
git push origin v0.1.0
```

The tag must match the `[package] version` in `Cargo.toml`. `tools/package.sh`
refuses to build otherwise, because a release whose contents disagree with its
name is a problem nobody notices until they are trying to work out which
firmware is on a board months later.

The workflow runs the full check suite before it publishes anything — a broken
release is worse than a missing one, because it gets flashed. Tags with a suffix
(`v0.2.0-rc1`) are published as pre-releases.

Both scripts run locally, so a release can be reproduced and inspected without
waiting on CI:

```bash
tools/package.sh --version v0.1.0    # writes dist/
tools/release_notes.sh v0.1.0        # the release body
```

Each release carries both `.uf2` images, the `.elf` files they were built from,
and `SHA256SUMS`. The ELFs are kept because with no debug probe, a fault address
reported over serial is the only forensic evidence available, and resolving one
needs the exact binary — not a rebuild.

## Licensing and provenance

Dual licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option, matching the conventions of embedded-hal, lora-phy and Embassy.

Two upstream projects are GPL-licensed and are deliberately *not* being copied:

* **RNode_Firmware_CE** (GPLv3) is the reference implementation of the protocol
  this project speaks. oxinode implements the KISS/RNode wire protocol from its
  observable behaviour and documentation — a clean-room reimplementation. No
  C++ is being ported or transliterated.
* **Meshtastic firmware** (GPLv3) is where the pin mapping was read from. Pin
  numbers and hardware facts are not copyrightable expression; none of their
  driver logic or comments are reproduced here.

The hardware facts in this README were cross-checked against three sources, in
descending order of authority:

1. **The Base Duo Rev 01 schematic** (`Base Duo [MH212A]`, KiCad, from muzi
   works) together with Elecrow's nRFLR1121 and nRFLR1262 module datasheets.
   Where the schematic and the Meshtastic variant disagree, the schematic wins.
2. **muzi works' fork of the Adafruit nRF52 bootloader**, MIT-licensed, whose
   `src/boards/muzi_base/` is the build the board actually reports running
   (`0.9.2-37-gf7fad14`). This is where the UF2 family ids, the absent DFU
   button and the `REGOUT0` setting come from.
3. **The Meshtastic variant**, useful and mostly right, but wrong about the
   flash part and misleading about the load-switch pins.

Contributions are accepted under the same dual license.
