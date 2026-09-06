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

### 3. GetVersion — done

**Verified on hardware.** The chip is an LR1121, hardware `0x22`, transceiver
firmware **1.1**:

```
INFO  version: reply 0x22 0x03 0x01 0x01
INFO  version: stat1 0x07 -- previous command ok, data follows
INFO  chip: standby (RC), executing from flash true, last reset power-on or brown-out
INFO  version: hw 0x22, use case 0x03, fw 1.1
INFO  version: LR1121, as expected
INFO  lr11xx: driver attached
INFO  lr11xx: errors ErrorStat { hf_xosc_start }
```

`use_case == 0x03` settles the module ambiguity: this is the nRFLR1121, not the
footprint-compatible nRFLR1262, so the pin map — including DIO9 on P1.08 — is
the right one.

#### The probe is raw on purpose

`lr11xx` has a `version()` that does this, and it is the one used from here on.
It is the wrong tool for the *first* transaction, because it validates the reply
and returns `Error::Fail` — discarding the bytes at exactly the moment the bytes
are the only evidence there is. An all-zeros reply and a healthy reply arrive by
identical means; telling them apart is the entire job and cannot be done from an
error enum. So `radio::probe_version` issues the command by hand, keeps every
byte, and `oxinode_core::lr1121::version` reads them with host tests behind it.

Two things that would otherwise have to be learned the hard way:

* **The command and its reply are separate NSS cycles**, with BUSY high in
  between. Done in one transaction you get the status of the *previous* command
  and no reply at all — a failure that looks like broken hardware and is not.
* **`lr11xx` waits for BUSY with no timeout of its own.** Handing the crate a
  BUSY that never falls hangs the driver, which is precisely the silent board
  step 2 exists to prevent. The bounded probe therefore goes first, and the
  crate is only given the pins once BUSY has proved itself.

#### The firmware-version check was wrong and has been replaced

This step originally said to compare against versions "RadioLib knows" —
`0x0307`, `0x0401`, `0x0402`. Implemented as written, it announced that the
first real board was running firmware *"no reference implementation
documents"*. The list is family-wide rather than per-part, it could not be
checked against RadioLib from here, and its first act was to call healthy
hardware suspect.

It is now `OBSERVED_FIRMWARE`, a single measured value — the version on the
bench board, 1.1 — and a mismatch means "this is not the board the behaviour in
this repository was observed on", which is worth one log line and nothing more.
The underlying advice stands: when an LR11x0 misbehaves in a way the datasheet
does not explain, the transceiver firmware version is the first thing to look
at.

#### Open: the chip says the last reset was analog, not external

`reset_status` reads `Analog` — power-on or brown-out — after a reset we drove
on NRESET, where `External` is the value that means "the NRESET pin". Both the
raw probe and `lr11xx` agree on the byte, so it is not a decoding error.

Two readings, and the schematic does not settle it: either NRESET on this module
drives the analog reset domain and is reported as such, or the field is
reporting the board's power-on and our pulse did not register as a distinct
reset. The second would be worrying, except that BUSY demonstrably answers the
pulse (step 2) and the 191 ms startup is a full boot. Recorded rather than
resolved.

### 4. TCXO — done

`set_tcxo_mode` with tune = 3.0 V and a delay of **164 steps (5005 µs)**. The
delay field counts 30.52 µs steps; the conversion lives in
`oxinode_core::lr1121::tcxo` with host tests, because every value in the
datasheet, in this repository and in a log is in different units from at least
one of the others — and because rounding it *down* is the one mistake that
produces `HF_XOSC_START_ERR`.

Note the corollary the schematic makes concrete: DIO3 is driving the TCXO
reference, so **DIO3 is not available as an interrupt line**. That is why the
board jumpers DIO9 out to the MCU (step 6).

The order matters and is the crate's own: `ClearErrors`, `SetTcxoMode`,
`Calibrate(ALL)`. The calibrations that ran at boot did so without a working
32 MHz oscillator, so they have to be redone — which is exactly what `lr11xx`'s
note on the `hf_xosc_start` flag says to do. Clearing *first* means the errors
read at the end are fresh evidence rather than the flag already latched from
before.

*Done when:* `hf_xosc_start` is clear, and the temperature reads like a room.

**Verified on hardware, first try:**

```
INFO  lr11xx: errors ErrorStat { hf_xosc_start }      <- before
INFO  tcxo: 3.0 V, delay 164 steps (5005 us at 30.52 us per step)
INFO  tcxo: errors clear -- the 32 MHz oscillator started
INFO  tcxo: die 18.458565 C, vbat 3.361765 V
INFO  tcxo: SetTcxoMode took 213 us for a 5005 us delay; calibration took 43579 us
```

18.5 °C and 3.36 V. `GetTemp` is a real test rather than a formality: it runs
off the 32 MHz oscillator, so before `SetTcxoMode` it does not fail — it returns
a *number*, computed from an ADC reading of a clock that is not running.
"Is this a plausible temperature" is therefore a direct test of whether the TCXO
came up, and `temperature_is_plausible` is written as an ordered range check
specifically so that NaN is rejected.

#### The delay is a wait, not a timeout — measured

The datasheet wording ("**maximum** duration for the 32 MHz oscillator to start
and stabilize") reads like a timeout that ends early once the oscillator is
detected. It does not behave like one.

| programmed delay | `SetTcxoMode` | `Calibrate(ALL)` |
|---|---|---|
| 5005 µs | 213 µs | 43579 µs |
| 20021 µs | 213 µs | 60058 µs |

+15.0 ms of programmed delay bought +16.5 ms of calibration time, near enough
1:1, while `SetTcxoMode` itself was unchanged. So the wait is not paid by that
command — it is paid by the first operation that actually needs the oscillator.

The practical consequence: **generosity here is not free.** A larger delay is a
recurring cost on XOSC startup rather than an unused safety margin, which is why
the constant stays at the 5 ms every reference implementation uses and the
margin goes on the evidence instead.

### 5. RF switch — configured, unproven

`set_dio_as_rf_switch` is the equivalent of RadioLib's
`LR11X0_DIO_AS_RF_SWITCH`. It tells the chip which of **its own** DIOs (DIO5,
DIO6, DIO7, DIO8, DIO10) drive the antenna switch, and what state each should
take in standby / RX / TX / TX-high-power / TX-high-frequency / GNSS / WiFi.

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

That table packs to `0x0300_0102_0200_0000`, and the packing lives in
`oxinode_core::lr1121::rf_switch` with tests that pin every byte to its own
field. The masks are a property of the copper, not of the chip; there is
nothing to discover them from and nothing to check them against, which is
exactly why they are written down once and asserted rather than assembled at
the call site.

*Done when:* the command is accepted, and — by way of step 7a — a receiver
confirms something reaches the antenna. **Both, on hardware:**

```
INFO  rfsw: enable 0x03, standby 0x00, rx 0x01, tx 0x02, tx_hp 0x02, tx_hf 0x00
      (word 0x0300010202000000)
INFO  rfsw: command accepted -- which is NOT proof that anything reaches the antenna
```

**This step cannot be validated on its own.** A wrong switch configuration
produces a clean `TxDone` while nothing reaches the antenna: the chip does
exactly what it was told, and what it was told is a fact about copper it cannot
see. So step 7a was brought forward, out of order — CW needs no interrupts, so
step 6 is not a prerequisite — and the switch was proved with a receiver.

#### Validated on an SDR — the switch is right

A NooElec R820T2 (RTL-SDR), `rtl_sdr` capturing raw IQ at 2.048 MS/s centred at
914.4 MHz so the carrier sits well away from the receiver's own DC spike.
Spectra are compared against a capture with the transmitter off, because
902–928 MHz is busy with frequency-hopping traffic and the strongest tone in any
single capture is usually somebody else.

| commanded | measured rise | at |
|---|---|---|
| −17 dBm | +51.9 dB | 914.9336 MHz |
| 0 dBm | +65.8 dB | 914.9342 MHz |
| +14 dBm | +81.2 dB | 914.9339 MHz |

**31 dB of commanded range produced 29.3 dB of measured range, at one frequency
to within 0.6 kHz.** A single burst would only show that *something* appeared;
a monotonic ramp shows the transmitter is under our control, which is what
proves the switch mask rather than merely the existence of RF.

The board's sub-GHz SMA had an antenna fitted throughout. The firmware refuses
powers outside the low-power PA's range rather than clamping them, and stops any
carrier by itself after 10 s — a bare carrier is a bench diagnostic, not
something FCC Part 15.247 contemplates, and a crashed host must not be able to
leave one up.

#### The carrier is 66 kHz low, and it is not clear whose fault that is

Every measurement puts the carrier at 914.934 MHz against a commanded
915.000 MHz: **−66.1 kHz, or −72 ppm**, repeatable across power levels to under
a kilohertz.

−72 ppm is ordinary for an uncalibrated RTL-SDR crystal and implausible for the
LR1121, which is running off a TCXO. That makes the receiver the likely culprit,
but "likely" is the honest word: there is no calibrated reference on this bench,
and the test that would settle it — commanding several frequencies and checking
whether the error stays constant in ppm or in hertz — has not been run. It
matters before phase 5, because a transmitter 72 ppm off would eat a large part
of a LoRa link's tolerance.

#### `lr11xx` cannot express the high-frequency TX state

`RfSwitchConfig` maps bits 56..=63 (enable), 48..=55 (standby), 40..=47 (rx),
32..=39 (tx), 24..=31 (tx_hp), 8..=15 (gnss) and 0..=7 (wifi). **Bits 16..=23 —
the high-frequency TX state — have no field.** A configuration assembled
through the crate's builder leaves that byte zero with no way to say otherwise.

On this board that is harmless, and harmless by coincidence rather than by
design: the 2.4 GHz output bypasses the switch, so its correct state *is*
all-low. A board that routed 2.4 GHz through the same switch could not be
configured with this crate at all. oxinode therefore packs the word itself and
passes it via `RfSwitchConfig::new_with_raw_value`, which keeps the coincidence
visible instead of load-bearing — and matters directly here, since this board
is meant to use its 2.4 GHz path.

### 6. Interrupts

`set_dio_irq(irq1 /* DIO9 */, irq2 /* DIO11 */)`, with nRF P1.08 as an interrupt
input. **DIO9 is confirmed**, no longer assumed: the module brings `LR_DIO9` out
on pin 11, its datasheet requires that pin be jumpered to an MCU GPIO, and the
Base Duo schematic does so on a net named `IRQ_JUMPER` landing on P1.08. The
module's other two exposed LoRa pins, `LR_DIO7` and `LR_DIO8`, are unconnected.

*Done when:* `TxDone` raises P1.08 and the async wait wakes.

### 7. First transmission

Cheapest test first:

**7a — continuous wave. Done, ahead of step 6** (CW needs no interrupts).

`set_tx_cw` does **not** need "no packet parameters at all", which is what its
own documentation implies and what this plan said. It requires
**`SetPacketType`**, and refuses without it.

That was expensive to learn, so the failure signature is worth recording. With
frequency and PA configured and no packet type set, `SetTxCw` returns
`command_status: Fail` and latches `cmd_error`, while `GetErrors` stays
completely clean and the chip remains in whatever mode it was in. Nothing in the
error flags points at the cause. Worse, `lr11xx` returns the status of the
*previous* command from every call, so the last command in a chain fails
silently: the driver reported success and the firmware logged a carrier that did
not exist.

Two things made it findable. Reading `stat2.chip_mode` after the sequence — the
chip either reaches `Tx` or it does not, and that is not a matter of
interpretation. And a `Reboot`-based radio reset, because several LR1121
commands persist until reset: once one experiment succeeded, every later one
succeeded too, for the wrong reason. Without a way back to a virgin chip the
bisect measured nothing, and the first pass through it produced four confident
and entirely worthless results.

Bisected against a freshly rebooted radio, three trials each way: with a packet
type it keys every time, without one it never does. `SetStandby(XOSC)`,
`SetFs`, `CalibImage`, `SetRegMode`, `ClearIrq` and `ClearErrors` were each
tested alone and none of them helps.

**7b — a real LoRa packet.** `set_packet_type(LoRa)`, `set_lora_modulation`
(SF/BW/CR), `set_lora_packet`, `set_lora_sync_word`, `set_pa_config`,
`set_tx_params`, write the buffer, `set_tx`, await `TxDone`.

*Done when:* ~~the SDR sees the carrier at 7a~~ (done — see step 5), and
`TxDone` arrives within the expected airtime at 7b.

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
