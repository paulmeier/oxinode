# Debugging without a probe

This board has no debug probe attached and no image assumes one. The only
diagnostic channel is a USB serial port, and the discipline that follows from
that is most of what makes the firmware debuggable at all.

## The log port

Every image with USB carries a `defmt` log on a CDC port (the second port on
`rnode` and `usb-cdc`; the only port on `radio` and `display`, so that the
bring-up images can wait for DTR before they start a one-shot sequence).
`defmt` interns its format strings into a `.defmt` section that never ships to
the device; the host decoder reads them out of the ELF, which is why the
stream is compact and why the exact ELF is needed to read it:

```bash
cargo install defmt-print
stty -f /dev/cu.usbmodemXXX3 115200
defmt-print -e target/thumbv7em-none-eabihf/release/rnode < /dev/cu.usbmodemXXX3
```

Three things have to be true at once for any of that to work, and each is
silent on its own: `DEFMT_LOG` set (it filters at compile time, defaulting to
`error`), `-Tdefmt.x` linked (or the section is absent and `defmt-print`
refuses the ELF), and `defmt::timestamp!` defined. `.cargo/config.toml` takes
care of all three.

**Always open the port at an explicit baud rate.** macOS caches the last line
setting per device path, including a flasher's 1200-baud touch, and `cat` on
the port would replay it and reboot the board into its bootloader.

### Getting the boot log

The product image's log pump waits for DTR on the log port and holds 4 KB of
ring buffer, the whole of boot. But DTR on the *second* CDC function is seen
by the firmware only around enumeration, so the port has to be opened within
the first second or so of the device appearing. The recipe that works: reset
the board over the KISS port, then open the log port the instant it comes
back.

```python
# Decode afterwards with:
#   defmt-print -e target/thumbv7em-none-eabihf/release/rnode < boot.bin
import os, serial, time
LOG, KISS = "/dev/cu.usbmodemXXX3", "/dev/cu.usbmodemXXX1"
k = serial.Serial(KISS, 115200); k.write(b"\xc0\x55\xf8\xc0"); k.close()   # CMD_RESET
while os.path.exists(LOG): time.sleep(0.02)
while True:
    try: s = serial.Serial(LOG, 115200, timeout=0.2); break
    except Exception: time.sleep(0.02)
end = time.time() + 8
with open("boot.bin", "wb") as f:
    while time.time() < end: f.write(s.read(4096))
```

The first lines of a boot say what the hardware is: the `UICR.NFCPINS` and
`REGOUT0` words, the mode switch's position, the device record's state, and
the radio's bring-up.

## Make a crash look different from a hang

From the host, a stalled image, a crashed one and a stopped clock all look
the same: the board goes quiet. The firmware is arranged so they do not:

- **A panic reboots into the bootloader.** A halted image is a USB device
  that never enumerates, and the only way back is a physical double-tap. The
  panic handler in `src/lib.rs` reboots into DFU instead, so a board that
  panicked is enumerated and reflashable from the keyboard. The cost is that
  the panic message goes with it.
- **Faults and unhandled interrupts** in the `ble` image record their cause
  in `GPREGRET2` and reboot into the bootloader. Silence then means a stall
  and nothing else.
- **A keystroke gate.** The Bluetooth bring-up waits for a byte on the log
  port, so a board that comes up is always a board that enumerates.
- **A channel that does not need the executor.** The blue LED is lit for
  exactly as long as a bring-up runs. When everything else has stopped, one
  look says whether the call returned.
- **A breadcrumb** in `GPREGRET2`, written before a bring-up and cleared
  after, so the next boot can say the last one did not come back.

## Capturing the program counter

The `ble` image arms `TIMER1` around the controller bring-up. If it fires,
its handler (two instructions of assembly, because the exception frame sits
at the stack pointer *at entry* and a compiled prologue would move it) reads
the interrupted `PC`, `LR` and `xPSR` out of the frame, snapshots the shared
`CLOCK_POWER` enable word and every `POWER`/`CLOCK` event, writes them to RAM
the linker does not zero, and resets into the application, which reports them
on the next keystroke. See `oxinode::ble::stall`. That is how an interrupt
storm on a shared vector was found; it puts a `PC` inside a handler with
`xPSR` saying which exception is active, which no amount of reasoning about
thread-mode code would have.

## Reading a binary blob

The SoftDevice Controller ships as an archive of obfuscated symbols, and it
is still just ARM. `rust-objdump -d` on the linked image, plus a script that
walks the call graph from an entry point and reports tight polling loops,
answers questions the headers cannot: whether an initialiser returns on all
settings, whether an assert path resets or halts, whether a wait helper uses
a bare `WFE`. It is worth doing early rather than late.

## The 1200-baud touch, three ways round

The touch is state on the host, and it outlives the operation that set it.

- **The window is 100 ms.** `adafruit-nrfutil` holds the port at 1200 baud
  for 100 ms and then allows 1.5 s for the board to reboot. An image that
  samples DTR too slowly misses it; the firmware polls every 20 ms.
- **A cached 1200 baud re-applies on the next open.** So a freshly flashed
  board can be opened at 1200 by nobody in particular and sent straight back
  to its bootloader on every boot. The firmware ignores the touch for the
  first two seconds after boot, which sits between the 1.5 s the flasher
  allows and anything a person would notice.
- **Reading the log is a touch** after a failed one. `tools/dfu-flash.sh`
  resets the cached rate to 115200 after every flash, successful or not.

## When a command "succeeds"

The LR1121 returns the status of the previous command on every call, so a
driver that reports `Ok` has told you about the one before. Several of its
commands report success and do nothing when issued in the wrong mode, and
several persist across experiments until the chip is rebooted, so once one
attempt succeeds every later one succeeds for the wrong reason. Two habits:
read `stat2.chip_mode` after a sequence (the chip either reaches `Tx` or it
does not), and reboot the radio between trials of anything you are bisecting.
The `radio` image's `r` key exists for that.

## Two things that are not the firmware

**Contended measurements.** Two series of the same host test started while
the first was still running, both opening the same serial port, look exactly
like a device dropping commands. Check what else is touching the thing you
are measuring before you believe the measurement.

**A wedged bootloader.** A 1200-baud touch on the product image taken while
a phone is connected over Bluetooth has been seen to leave the board in its
bootloader with serial DFU not answering; only a double-tap recovers it.
Disconnect the phone or reset the board first. See
[Known limitations](../reference/limitations.md).
