# Serial DFU reliability

On the evening of 2026-10-24, serial DFU failed repeatedly on both bench
boards: `adafruit-nrfutil` timed out early in a transfer, and the bootloader
then answered nothing until a double-tap on reset. This page records what was
measured, what the bootloader and the tool do, and why `tools/dfu-flash.sh`
was not changed in response. The short version: three remedies were
suggested (wait for the bootloader's drive to mount before flashing, settle
longer, retry from the start packet), and the measurements support none of
them. A double-tap is still the recovery.

## What was measured

No board was flashed for this. The evidence is the host's own USB log, which
records every enumeration, disconnect and pipe abort with millisecond
timestamps, read back with:

```bash
/usr/bin/log show --start "2026-10-23 00:00:00" --style compact \
    --predicate 'senderImagePath CONTAINS "IOUSBHostFamily" AND
                 (eventMessage CONTAINS "enumerated 0x" OR
                  eventMessage CONTAINS "terminateDevice" OR
                  eventMessage CONTAINS "aborting")'
```

(`/usr/bin/log`, because zsh has a `log` builtin.) Three kinds of line matter:

- `enumerated 0x239a/0081`: a board came up in its bootloader;
  `terminateDevice ... hardware connection lost`: it reset out of it.
- `endpoint 0x82: aborting`: the CDC data-IN pipe was torn down, which is
  what a host-side **close of the serial port** looks like. Each attempt by
  `adafruit-nrfutil` ends in one.
- `endpoint 0x83: ... (timeout)`: a mass-storage (UF2 drive) request the
  board did not answer in time.

The `.bin` beside each ELF is rewritten by every run of `dfu-flash.sh`, so its
modification time says which image the last attempt sent.

### A successful flash is a fixed-length session

From 16:01 on 2026-10-23 to 21:03 on 2026-10-24 there were 58 bootloader
sessions. Every one ended with the board resetting into a new application,
each had exactly one port close (at the end), and their lengths fell into a
few tight clusters, one per image size: 21.1-21.5 s for the small images,
32.0-33.4 s, then 34.4-35.7 s and 37.5-37.8 s for the node image as it grew.
Eight consecutive flashes of the 618 KB node image, alternating boards, ran
37.5-37.8 s each.

That regularity has a cause: the transfer is clocked by the tool's own sleeps,
not by the link. After the start packet it sleeps a fixed 89.7 ms for every
4 KB page the bootloader has to erase (13.6 s for the node image), and it
sleeps 102 ms after every eighth 512-byte packet (about 15.5 s more). A session
that departs from its cluster is one where something went wrong.

### The failures

All of the failures came between 21:26 and 21:47 on 2026-10-24. Times are
local.

| Board | Bootloader came up | Entered by | What the log shows |
|---|---|---|---|
| 7F23 | 21:26:00.12 | touch, from the RNode image | port closed at +3.7 s, +8.5 s, then four closes 3.4 s apart at 21:27:09-19. No reset for 12.5 min. |
| 354B | 21:32:22.53 | touch, from the qspi bring-up | closes at +3.7 s, +10.0 s, +38.5 s, +46.3 s. No reset for 6 min. |
| 354B | 21:38:23 | double-tap (two resets 0.2 s apart) | flashed the display bring-up (40 KB) at the first attempt. |
| 7F23 | 21:38:30 | double-tap | idle about 3 min, then flashed the qspi bring-up (34 KB) at the first attempt. |
| 354B | 21:40:10.91 | touch, from the display bring-up | mass-storage timeout at +11.1 s; first port close at +16.5 s; then eleven more attempts over 5 min, most in bursts 3.4 s apart, every one failing. No reset until 21:47:06. |

For contrast, at 21:30:39 354B was touched out of the qspi bring-up and
flashed in a 3.8 s session, at the first attempt.

A burst of four closes 3.4 s apart is `dfu-flash.sh` itself: the first
attempt plus three retries, 2 s apart, each taking about 1.4 s. That is how
long the tool takes to open the port, send one packet and give up after its
1 s acknowledgement timeout. So every retry in those bursts failed on its
**first** packet. From 21:44 each burst is followed 0.26 s later by one more
close, the cached-baud reset reopening the port. Before that (after the 21:03
flashes, and after the 21:27 burst) there is none, which is the silent
pyserial failure that `clear_cached_baud` now reports.

No other process opened either board through the USB user client between
21:20 and 21:48, so nothing else was talking to the bootloader.

### What the timings say about the first attempt

In the 21:40 session the first attempt's port stayed open for about 15.6 s.
The tool sleeps through the erase only **after** the start packet has been
acknowledged, and the node image's erase sleep is 13.6 s, so the start packet
was received and acknowledged, and the bootloader went into its erase. The
attempt then failed on the init packet or the first data packets. While the
board was erasing, macOS sent a request to the mounted UF2 drive, and it timed
out (+11.1 s). That is the only mass-storage timeout in all 67 bootloader
sessions in the log.

The two short failures (21:26, 21:32) fit the same pattern, but the timings
cannot prove it. The first close came 2.35-2.5 s after the start packet would
have gone out. That fits an acknowledged start, a 0.8-0.9 s erase sleep for a
small image and a 1 s timeout on the next packet. A timeout on the start
packet itself, which takes 1-2 s with the tool's read loop, fits almost as
well.

## What the code does

The tool is `adafruit-nrfutil` 0.5.3.post16. The bootloader is the muzi fork
at `f7fad14`, whose `lib/` and `src/` are identical to that fork's current
`HEAD`. The relevant behaviour:

- **The bootloader acknowledges a packet as soon as it has been received**,
  before acting on it (`hci_transport.c`, `rx_vendor_specific_pkt_type_handle`).
  A packet with an unexpected sequence number is dropped and acknowledged with
  the number it does expect.
- **The tool never retransmits, and accepts any acknowledgement.** Its
  `send_packet` breaks out of the retry loop after the first read of an
  acknowledgement, whatever its number. The "three failed tx attempts" check
  underneath can never run. A missing acknowledgement raises after 1 s, which
  the command line turns into exit status 0 (hence the `Device programmed.`
  check in `dfu-flash.sh`).
- **Serial DFU erases the whole image in one blocking loop** when the start
  packet is handled (`dfu_single_bank.c`, `dfu_prepare_func_app_erase`), one
  `nrfx_nvmc_page_erase` per 4 KB page. USB is serviced from the same main
  loop (`tud_task` in `wait_for_events`), so for the node image the board does
  not answer USB requests for about 13 s. Its CDC port and its UF2 drive both
  go quiet, which is when the 21:40 drive request timed out.
- **The SLIP receiver can get stuck receiving nothing.** On a receive overflow
  the HCI layer points the SLIP decoder at its 5-byte acknowledgement buffer
  (`hci_transport.c`, `HCI_SLIP_RX_OVERFLOW`). It goes back to a full-size
  buffer only once a frame of five bytes or fewer arrives. The tool never
  sends one, so every later packet overflows again and is never acknowledged,
  while the drive stays mounted. That matches "serial DFU stops answering
  until a reset". Nothing measured here shows that this is what happened.
- **A retry against a bootloader that is mid-transfer is not a fresh start.**
  A new tool process numbers its packets from 1, and the bootloader still
  expects the next number of the old transfer. Packets are dropped (and
  acknowledged, so the tool carries on) until the numbers happen to line up.
  The packet that gets through first is then a start or init packet the DFU
  state machine rejects, which ends in `APP_ERROR_CHECK` and a reset, or a
  data packet written at the old transfer's offset. A retry from the start
  packet can therefore report `Device programmed.` without having programmed
  anything. The per-board check `dfu-flash.sh` makes would then see a board
  still in its bootloader, but only on the next flash.
- **Entered by a touch, the bootloader gives up after 3 s unless USB has
  enumerated** (`main.c`, `bootloader_dfu_start(_ota_dfu, 3000, true)`).
  Every touch-entered session above enumerated within 10 ms of the reset, so
  this timer did not end any of them.

## The three suggested remedies

**Waiting for the bootloader's drive to mount.** Serial DFU is open from boot
(`dfu_transport_serial_update_start` runs before USB comes up), and the start
packet was acknowledged in at least one failure, without any wait. The drive
mounting is not what the transport waits for. Waiting would change when the
erase overlaps macOS's first look at the new volume, and nothing measured says
which overlap is the bad one.

**A longer settle before the first packet.** Same problem: the start packet
got through. Whatever broke came after it.

**Retrying from the start packet.** The script already does this, and it is
measured: in the three failing sessions, 19 further attempts all failed on
their first packet, and none of them recovered the session. Only a reset did,
and the first flash after each reset worked. From the source, a retry against
a bootloader that is mid-transfer can also do harm (above). A longer gap
between retries is not supported either: in the 21:32 session, attempts 6 s,
29 s and 37 s after the first all failed.

So nothing in `dfu-flash.sh` changes here. Its retry still handles the failure
it was written for, a touch that loses the race to the bootloader's
enumeration, where nothing has reached the bootloader yet.

## What would settle it

These need a board on the bench and Paul there to double-tap:

1. Run the tool with `-v -v` (`adafruit-nrfutil -v -v dfu serial ...`). It
   then logs every packet in each direction, which shows which packet lost its
   acknowledgement and what sequence number the bootloader was expecting.
2. Flash the node image (long erase) and a bring-up image (short erase) by
   touch, several times each, recording the session lengths. If failures
   follow the long erase, unmounting the UF2 drive first
   (`diskutil unmount /Volumes/muzi-Base`) is a cheap thing to try and to
   measure: the UF2 drive is the only other USB client that talks to the board
   during the erase.
3. With a board stuck, send it one SLIP frame of five bytes or fewer, which
   would clear an overflow trap, and see whether the next attempt is
   acknowledged. If it is, that confirms the mechanism, and gives
   `dfu-flash.sh` a software recovery in place of the double-tap.

One data point for the third, on 2026-10-24 at 22:46, flashing the 30 KB
`buzzer` image onto 7F23 by touch. The first attempt raised `No data
received`; the retry printed `Device programmed.`, and the board came up in
its bootloader. Read back through `CURRENT.UF2`, the first 50 of the image's
119 256-byte blocks matched and the rest did not: the partial write
described above, on a short erase, not a long one. The next attempt could
not reach the bootloader. Four SLIP frames of four zero bytes each, opened
at 115200 baud, got no reply, and the attempt after them still could not
reach it. That does not rule the overflow trap out (nothing showed the
frames were read), but it is not the software recovery hoped for.

A second, on 2026-10-25 at 09:10, the same board and a 31 KB `buzzer`
image, touched out of the `buzzer` image itself. This time the **first**
attempt printed `Device programmed.` with no error and no retry, and the
board came up in its bootloader. `CURRENT.UF2` matched the first 38 of 124
blocks. The next attempt could not reach the bootloader, as before. So a
partial write does not need a retry against a bootloader mid-transfer to
happen, and `Device programmed.` alone is not proof of a flash.
