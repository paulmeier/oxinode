# Phase 3 — LR1121 bring-up

Getting the radio to identify itself, configure itself, and emit RF that
something else can see. No protocol, no `lora-phy`, no framing.

## Decisions taken

| | |
|---|---|
| Band | **US915** sub-GHz, plus the LR1121's 2.4 GHz path (sub-GHz first) |
| RF verification | **SDR** — a carrier is visible without any packet framing |
| PHY layer | **Skip `lora-phy`; build on `lr11xx`'s low-level API** |

### The board is one module, not two chips

Worth having in mind before any of the wiring steps. The Base Duo (schematic
Rev 01, MH212A) does not carry a discrete nRF52840 and a discrete LR1121. It
carries a single **Elecrow nRFLR1121** system-on-module — 80 pins,
20 × 20 × 3.5 mm — with both dies inside and **the SPI between them routed
internally**.

The practical consequences run through the whole phase:

* The nRF52840 GPIO numbers in the Meshtastic variant are still exactly right,
  because they are still that MCU's own pins. They simply terminate inside the
  module rather than on board copper.
* So there is nothing to probe. No oscilloscope, no logic analyser, no continuity
  check. Every failure in steps 1–3 has to be diagnosed from software.
* Equally, there is no wiring mistake available to us. If `GetVersion` fails it
  is our SPI configuration, our reset timing, or a dead part — not a trace.
* Three separate antenna ports leave the module: `ANT_BLE` (pin 54),
  `ANT_LORA_2.4G` (pin 65) and `ANT_LoRa` (pin 72). BLE has its own path and
  never contends with the LoRa switch, which matters again at phase 8.

### Why not `lora-phy`

`lr11xx` 0.1.0 does implement `lora_phy::mod_traits::RadioKind`, but eight of its
methods are `todo!()` — `create_modulation_params`, `create_packet_params`,
`set_modulation_params`, `set_packet_params`, `set_tx_rx_buffer_base_address`,
`set_irq_params`, `await_irq`, `process_irq_event`. Those are exactly the calls a
transmission needs, and they panic. `lora-phy` itself is at 3.0.1 (July 2024) and
marked *minimal maintenance* upstream.

The same crate's **low-level API is complete** — zero `todo!()` in `radio.rs` and
`system.rs` — and an RNode needs raw LoRa PHY, not LoRaWAN. `lora-phy`'s value is
portability across radios we do not have. So oxinode talks to `lr11xx` directly
and keeps its own thin abstraction, which phase 4 turns into runtime
configuration.

Note that `lora-phy` and `defmt` are **mandatory** dependencies of `lr11xx` with
no features to switch them off, so `lora-phy` is compiled either way. LTO drops
the unused half.

## Steps

### 0. defmt over USB — done

`lr11xx` calls `defmt::debug!` and `defmt::trace!` internally. A binary using it
**will not link** without a `#[defmt::global_logger]`, and with no debug probe
`defmt-rtt` is useless. The log has to leave over USB.

Add it as a **second CDC-ACM interface**. Phase 5's KISS stream owns the first
one, and retrofitting a second interface later renumbers them and changes the
device paths on the host.

*Done when:* the board enumerates two serial ports; defmt output decodes on the
second while the first still echoes. **Verified on hardware.** Three things had
to be true at once, each silent on its own: `DEFMT_LOG` set (it filters at
*compile* time, defaulting to `error`), `-Tdefmt.x` linked (or the `.defmt`
section holding the interned format strings is absent and `defmt-print` refuses
the ELF), and `defmt::timestamp!` defined (or the link fails on
`_defmt_timestamp`).

### 1. SPI — done

SPIM on SCK P1.13, MOSI P1.14, MISO P1.15, with CS P1.12 driven as a GPIO.
`lr11xx` wants an **async** `embedded-hal` `SpiDevice`, so the bus is wrapped in
`embedded-hal-bus`'s `ExclusiveDevice`. Mode 0, MSB first, 1 MHz.

**SPIM2, not SPIM3.** SPIM3 is the fast instance, and also the one carrying an
nRF52840 EasyDMA anomaly around CPU writes to the RAM holding an in-flight TX
buffer, which `embassy-nrf` does not work around. At 1 MHz that speed buys
nothing. SPIM2 also keeps both TWI/SPI-shared instances free for the phase-7
display.

`config.orc = 0x00` is not decoration: `lr11xx` requires MOSI held low during a
read, and the over-read character is the byte SPIM sends once the TX buffer runs
short — which is exactly what a `Read` operation is.

Worth knowing before step 3: EasyDMA cannot read from flash, so a `&[u8]` that
the compiler promoted to a static would fail. `embassy-nrf` catches this and
copies through a 512-byte RAM buffer, transparently — but `copy_from_slice`
panics above 512 bytes. Nothing `lr11xx` sends comes close.

These four signals never reach board copper — they are the module-internal link
described above. That removes a whole class of problem (nothing is miswired) and
removes the only tool that would normally diagnose the rest.

*Done when:* ~~it builds and runs~~ — **this was too pessimistic.** The original
note said there was "genuinely no intermediate test to write". There is one, and
it is worth having: the SPIM's own `PSEL` registers can be read back and compared
against the pins we asked for. That catches a transposition — and `Spim::new`
takes `miso` *before* `mosi`, so a transposition is one keystroke away — without
needing the LR1121 to answer anything. The encoding lives in
`oxinode_core::gpio` with host tests; `radio::check_pin_selection` does the
readback.

**Verified on hardware:**

```
INFO  spi SCK: P1.13
INFO  spi MISO: P1.15
INFO  spi MOSI: P1.14
INFO  spi NSS: P1.12 (GPIO)
INFO  spi: SPIM2 up, mode 0, MSB first, 1 MHz, ORC 0x00
```

This proves the peripheral claimed the right MCU pins. It proves nothing about
the LR1121 on the other end; that starts at step 3.

#### The bring-up image gets a single CDC port

Step 0 left one problem open: `wait_connection()` resolves at
`SET_CONFIGURATION`, so a startup log is written into a port with no reader and
discarded, and DTR — which would fix it — never goes true on the *second* CDC
function of a composite device.

A bring-up image is exactly the case that cannot tolerate that, because its
entire output is a one-shot startup sequence. So `radio` exposes a single CDC
function, which *does* see DTR. It uses it twice: the log drain waits for DTR,
and so does the bring-up sequence itself. Both confirmed on hardware — the log
above appeared 26 seconds after boot, when the terminal attached, with nothing
lost. The 1200-baud touch works from the same port, so reflashing needs no
reset button.

`usb-cdc` keeps its two ports and its continuous heartbeat; the drain loop is
now shared between both images (`src/usb_log.rs`), with the DTR gate passed in.

### 2. Reset and BUSY — done

`Lr11xx::new(spi, busy)` takes **no reset pin**, so NRESET (P1.10) is ours: hold
low ≥100 µs, release, then wait for BUSY (P1.11) to fall. BUSY needs
`InputPin + Wait`, which `embassy-nrf`'s `Input` provides through GPIOTE.

Like the SPI bus, both of these run inside the module. `LR_DIO9` is the only LoRa
signal that reaches board copper at all (step 6), so BUSY is not observable
except by reading the pin from firmware.

That makes the timeout the important part of this step rather than a nicety. If
BUSY never falls there is no external way to tell a dead part from a stuck reset
from a misconfigured pin, so the code should say which of those it *cannot*
distinguish rather than silently waiting.

*Done when:* BUSY is observed high then low within a bounded time, and a timeout
logs a clear error instead of hanging forever.

#### The startup takes 191 ms, not "milliseconds"

**This step's original timing assumption was wrong, and the board said so on the
first run.** The plan above says LR1121 startup is milliseconds — the figure the
SX126x family trains you to expect — so the first timeout was 100 ms. It fired:

```
ERROR busy: still high after 100000 us
ERROR reset: BUSY never fell
```

Widening the timeout and repeating the reset eight times gave 191101, 191162,
191131, 191162, 191101, 191131, 191162 and 191131 µs. That is a spread of 61 µs,
or two ticks of the 32.768 kHz crystal doing the measuring — so **191.1 ms,
deterministic to the limit of what oxinode can observe**, and nearly two hundred
times longer than assumed.

Why it takes that long is *not* established. The LR11x0 family carries its own
on-chip transceiver firmware, so a boot that verifies or loads an image is the
obvious guess — but it is a guess, and the datasheet was not on hand to check
it. The number is not a guess. It lives in `oxinode_core::lr1121` as
`STARTUP_MEASURED_US`, with the timeout set to 5× it and a compile-time
assertion that stops anyone quietly reverting it towards "milliseconds".

The consequence for later steps: step 4's "allow ~10 ms after power-on" is
dwarfed by this. Anything that resets the radio pays a fifth of a second, which
matters for a phase-5 `rnodeconf` interaction that expects a prompt reply.

#### What the trace can and cannot rule out

`ResetVerdict` reads a `BusyTrace` rather than returning a bare bool, because
the interesting distinctions are the ones a bool destroys:

* **BUSY never fell** — a dead part, a reset that never released, a BUSY line
  that is not the pin we think it is, and a chip in its own bootloader all
  produce this one symptom, and all four are inside the module.
* **BUSY was low throughout and never rose** — either the chip finished before
  the first sample, or the pin does not follow BUSY at all. This one is a trap
  worth naming: `wait_for_low()` returns *immediately* on a pin stuck low, so
  the obvious implementation reports success loudest exactly when it is most
  wrong.
* **BUSY rose and fell** — says the pin behaved, and nothing at all about
  whether the chip on the other end is an LR1121.

Sampling BUSY *before* touching NRESET turned out to be worth more than
expected. The run reports `before reset false, during reset true`: BUSY was low,
driving P1.10 low drove P1.11 high, and releasing P1.10 let it fall. That causal
link confirms **both** pins at once — a GPIO that was not NRESET would not move
a pin that was not BUSY. The plan expected step 3 to be the first real evidence;
this arrives a step earlier.

The level during reset is recorded and logged but deliberately never judged: the
datasheet on hand does not say what BUSY must do while the chip is held in
reset, and inventing a requirement would turn a guess into a failing check.

### 3. GetVersion

`Lr11xx::new` already issues `GetVersion` and logs the result. Assert
`use_case == UseCase::Lr1121` (`0x03`).

The check is cheap insurance rather than a live ambiguity. The Meshtastic
variant defines **both** `USE_SX1262` and `USE_LR1121` because Elecrow ships two
footprint-compatible modules — nRFLR1121 (LR1121) and nRFLR1262 (an SX1262,
despite the name) — and one PCB accepts either. The Rev 01 schematic populates
the LR1121 part, and on an SX1262 board the interrupt would arrive on P1.06
instead. Assert rather than probe, but assert.

Failure signatures worth recognising: `UseCase::Bootloader` (`0xDF`) means the
chip is sitting in its own bootloader; all-`0x00` or all-`0xFF` means SPI is not
working at all rather than the chip being wrong.

**Log the firmware version, do not just check the use case.** The LR11x0 family
carries its own on-chip transceiver firmware, and behaviour genuinely differs
between images — RadioLib knows `0x0307`, `0x0401` and `0x0402`, and Meshtastic
carries a whole opt-in update path for parts running old ones (~240 kB of flash
per baked-in image, so oxinode wants no part of it). When an LR11x0 misbehaves
in a way the datasheet does not explain, the transceiver firmware version is the
first thing to look at, and it costs nothing to have it already in the log.

*Done when:* the log says `Lr1121`, with hardware and firmware versions.

### 4. TCXO

`set_tcxo_mode` with tune = 3.0 V. The delay field counts 30.52 µs steps and has
to cover TCXO startup; too short gives `HF_XOSC_START_ERR`. This must happen
before any RF operation.

Allow ~10 ms after power-on before touching the radio at all; Meshtastic waits
that long for the TCXO to settle, on top of the `set_tcxo_mode` delay field.

Note the corollary the schematic makes concrete: DIO3 is driving the TCXO
reference, so **DIO3 is not available as an interrupt line**. That is why the
board jumpers DIO9 out to the MCU (step 6).

`GetTemp` is a good smoke test — the crate's own docs note it runs off XOSC, so
it exercises the TCXO path.

*Done when:* no error status, and the temperature reads like a room.

### 5. RF switch

`set_dio_as_rf_switch` is the equivalent of RadioLib's
`LR11X0_DIO_AS_RF_SWITCH`. It tells the chip which of **its own** DIOs (DIO5,
DIO6, DIO7, DIO8, DIO10) drive the antenna switch, and what state each should
take in standby / RX / TX / TX-high-power / GNSS / WiFi.

The mask values are board-specific, and this board's are known. Only **DIO5 and
DIO6** are used:

| Mode | DIO5 | DIO6 |
|---|---|---|
| standby | low | low |
| RX | **high** | low |
| TX | low | **high** |
| TX high power | low | **high** |
| TX high frequency (2.4 GHz) | low | low |
| GNSS | low | low |
| WiFi | low | low |

The 2.4 GHz row is not a mistake and not an oversight: TX-HF is the same
low/low as standby because the 2.4 GHz path does not go through this switch at
all. The schematic gives the sub-GHz output its own SMA connector and the
2.4 GHz output a separate u.FL, so the switch only ever arbitrates the sub-GHz
RX/TX pair.

**This step still cannot be validated on its own.** A wrong switch
configuration produces a clean `TxDone` while nothing reaches the antenna. That
is the entire reason step 7 requires an SDR — knowing the table is not the same
as having sent the command correctly.

*Done when:* the command is accepted. Real validation is deferred to step 7.

### 6. Interrupts

`set_dio_irq(irq1 /* DIO9 */, irq2 /* DIO11 */)`, with nRF P1.08 as an interrupt
input. **DIO9 is confirmed**, no longer assumed: the module brings `LR_DIO9` out
on pin 11, its datasheet requires that pin be jumpered to an MCU GPIO, and the
Base Duo schematic does so on a net named `IRQ_JUMPER` landing on P1.08. The
module's other two exposed LoRa pins, `LR_DIO7` and `LR_DIO8`, are unconnected.

*Done when:* `TxDone` raises P1.08 and the async wait wakes.

### 7. First transmission

Cheapest test first:

**7a — continuous wave.** `set_tx_cw` needs no packet parameters at all: set the
frequency, configure the PA, transmit. On the SDR it is a carrier at the target
frequency. This is the first moment the RF switch config from step 5 is proved.

**7b — a real LoRa packet.** `set_packet_type(LoRa)`, `set_lora_modulation`
(SF/BW/CR), `set_lora_packet`, `set_lora_sync_word`, `set_pa_config`,
`set_tx_params`, write the buffer, `set_tx`, await `TxDone`.

*Done when:* the SDR sees the carrier at 7a, and `TxDone` arrives within the
expected airtime at 7b.

> **Before transmitting.** A LoRa antenna must be attached to the sub-GHz port —
> the SMA connector, not the 2.4 GHz u.FL. Transmitting into an open port can
> damage the PA, and an antenna on a different connector does not count. Start
> at the lowest usable output power, and cap it at what the *module* is rated
> for — **20 dBm sub-GHz, 11.5 dBm at 2.4 GHz** — rather than at the LR1121's
> headline 22/13 dBm, which is what Meshtastic clamps to.
> Keep continuous-wave bursts short: a bare carrier is a bench diagnostic, not a
> mode that satisfies FCC Part 15.247, which expects digital modulation or
> frequency hopping in 902–928 MHz.

### 8. Lock it in

* **Select the DC-DC regulator** rather than leaving the chip in LDO mode. Every
  LR1110-family board does; it is one command and it matters for TX current.
* A separate `radio` binary. Keep it apart from `usb-cdc` until phase 5 merges
  the transports — with no probe, a bisectable failure is worth a lot.
* Push everything decidable without hardware into `oxinode-core`, with tests:
  `RfSwitchConfig` construction, the TCXO delay ↔ microseconds conversion,
  `Version` decoding, and LoRa airtime calculation (needed for step 7's timeout,
  and again in phase 5).

## Open questions

Two of the three original unknowns were closed by the Rev 01 schematic and the
Elecrow module datasheet, both of which are now on hand:

* ~~Which LR1121 DIO reaches nRF P1.08.~~ **DIO9**, over the `IRQ_JUMPER` net.
* ~~The RF switch masks for this board, including the 2.4 GHz path.~~ DIO5/DIO6
  only, table in step 5; the 2.4 GHz path bypasses the switch entirely.

What remains:

* Whether the attached GPS antenna feeds the LR1121's own GNSS input or the
  separate UART GPS module on P0.19/P0.20. The board does hang a UART GPS off
  the expansion connector behind a load switch, and the switch's GNSS state is
  low/low regardless, so this is now a question about what the antenna is
  *for* rather than about what to configure.
