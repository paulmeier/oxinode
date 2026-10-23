#!/usr/bin/env python3
"""Read the RNode image's defmt log from its vendor USB interface.

The RNode image has one serial port, the KISS port. Its log is a vendor-specific
bulk interface beside it, so that `rnodeconf` -- which reopens a reset board on
the first port with a matching USB serial number -- has nothing to confuse it
with (issue #22). No host binds a driver to that interface, so this claims it
through libusb and writes the raw bytes to stdout for `defmt-print`:

    tools/log_reader.py | defmt-print -e target/thumbv7em-none-eabihf/release/rnode

With two boards attached, say which:

    tools/log_reader.py --serial 65C11224153B1DFD | defmt-print -e ...
    tools/log_reader.py --list

It follows the board across resets and reflashes; `--once` exits instead. The
bring-up images keep their log on a CDC port and are read as before.

Standard library only: libusb is called through ctypes (`brew install libusb`;
on Linux, libusb-1.0 and a udev rule for 1209:0003).
"""

import argparse
import ctypes
import ctypes.util
import os
import signal
import struct
import sys
import time
from typing import List, NamedTuple, Optional

VID = 0x1209
PID = 0x0003  # the RNode image; see `USB_PID` in src/bin/rnode.rs

# The contract with the firmware, defined in core/src/usb.rs and checked
# against it by test_log_reader.py.
LOG_INTERFACE_CLASS = 0xFF
LOG_INTERFACE_SUBCLASS = 0x6F
LOG_INTERFACE_PROTOCOL = 0x01
LOG_REQUEST_ATTACH = 0x01

DESC_CONFIGURATION = 0x02
DESC_INTERFACE = 0x04
DESC_ENDPOINT = 0x05
ENDPOINT_IN = 0x80
TRANSFER_BULK = 0x02

LIBUSB_ERROR_TIMEOUT = -7
LIBUSB_ERROR_NO_DEVICE = -4
LIBUSB_ERROR_NOT_FOUND = -5
LIBUSB_ERROR_ACCESS = -3
LIBUSB_ERROR_BUSY = -6
LIBUSB_ERROR_PIPE = -9
LIBUSB_ERROR_IO = -1


def board_went_away(rc: int) -> Optional[bool]:
    """What a bulk read's return code says: False to keep reading, True if the
    board reset or went away, None for an error that is neither.

    macOS reports a board that resets mid-read as any of these, NOT_FOUND
    included; that one used to end the reader on the first 1200-baud touch.
    """
    if rc in (0, LIBUSB_ERROR_TIMEOUT):
        return False
    if rc in (LIBUSB_ERROR_NO_DEVICE, LIBUSB_ERROR_NOT_FOUND, LIBUSB_ERROR_IO, LIBUSB_ERROR_PIPE):
        return True
    return None


class LogInterface(NamedTuple):
    number: int
    endpoint: int


def find_log_interface(config: bytes) -> Optional[LogInterface]:
    """The log interface and its bulk IN endpoint, from a raw configuration
    descriptor, or None if the configuration has none.

    Walks the descriptors by their own lengths and stops at the first that is
    malformed, so a short read gives None rather than an exception.
    """
    current = None  # interface number while inside the log interface
    i = 0
    while i + 2 <= len(config):
        length, kind = config[i], config[i + 1]
        if length < 2 or i + length > len(config):
            return None
        body = config[i : i + length]
        if kind == DESC_INTERFACE and length >= 9:
            number, _alt, _eps, cls, sub, proto = body[2:8]
            matched = (cls, sub, proto) == (
                LOG_INTERFACE_CLASS,
                LOG_INTERFACE_SUBCLASS,
                LOG_INTERFACE_PROTOCOL,
            )
            current = number if matched else None
        elif kind == DESC_ENDPOINT and length >= 7 and current is not None:
            address, attributes = body[2], body[3]
            if address & ENDPOINT_IN and attributes & 0x03 == TRANSFER_BULK:
                return LogInterface(current, address)
        i += length
    return None


class UsbError(Exception):
    pass


class Libusb:
    """The few libusb-1.0 calls this needs, through ctypes."""

    def __init__(self) -> None:
        # find_library does not search Homebrew's prefix on Apple Silicon.
        candidates = [ctypes.util.find_library("usb-1.0"), "/opt/homebrew/lib/libusb-1.0.dylib",
                      "/usr/local/lib/libusb-1.0.dylib"]
        for candidate in filter(None, candidates):
            try:
                self.lib = ctypes.CDLL(candidate)
                break
            except OSError:
                continue
        else:
            raise UsbError("libusb-1.0 not found (macOS: brew install libusb)")
        lib = self.lib
        p = ctypes.c_void_p
        lib.libusb_init.argtypes = [ctypes.POINTER(p)]
        lib.libusb_exit.argtypes = [p]
        lib.libusb_get_device_list.argtypes = [p, ctypes.POINTER(ctypes.POINTER(p))]
        lib.libusb_get_device_list.restype = ctypes.c_ssize_t
        lib.libusb_free_device_list.argtypes = [ctypes.POINTER(p), ctypes.c_int]
        lib.libusb_get_device_descriptor.argtypes = [p, ctypes.c_char_p]
        lib.libusb_open.argtypes = [p, ctypes.POINTER(p)]
        lib.libusb_close.argtypes = [p]
        lib.libusb_get_string_descriptor_ascii.argtypes = [p, ctypes.c_uint8, ctypes.c_char_p, ctypes.c_int]
        lib.libusb_control_transfer.argtypes = [
            p, ctypes.c_uint8, ctypes.c_uint8, ctypes.c_uint16, ctypes.c_uint16,
            ctypes.c_char_p, ctypes.c_uint16, ctypes.c_uint,
        ]
        lib.libusb_claim_interface.argtypes = [p, ctypes.c_int]
        lib.libusb_release_interface.argtypes = [p, ctypes.c_int]
        lib.libusb_bulk_transfer.argtypes = [
            p, ctypes.c_uint8, ctypes.c_char_p, ctypes.c_int, ctypes.POINTER(ctypes.c_int), ctypes.c_uint,
        ]
        self.ctx = p()
        if lib.libusb_init(ctypes.byref(self.ctx)) != 0:
            raise UsbError("libusb_init failed")

    def boards(self) -> List[str]:
        """USB serial numbers of the attached RNode-image boards."""
        return [serial for serial, _ in self._scan(None, keep=False)]

    def open(self, serial: Optional[str]):
        found = self._scan(serial, keep=True)
        if not found:
            return None, None
        if len(found) > 1:
            for _, handle in found:
                self.lib.libusb_close(handle)
            raise UsbError(
                "more than one board attached ("
                + ", ".join(s for s, _ in found)
                + "); say which with --serial"
            )
        return found[0]

    def _scan(self, want: Optional[str], keep: bool):
        devices = ctypes.POINTER(ctypes.c_void_p)()
        count = self.lib.libusb_get_device_list(self.ctx, ctypes.byref(devices))
        found = []
        try:
            for n in range(max(count, 0)):
                dev = devices[n]
                desc = ctypes.create_string_buffer(18)
                if self.lib.libusb_get_device_descriptor(dev, desc) != 0:
                    continue
                (_, _, _, _, _, _, _, vid, pid, _, _, _, i_serial, _) = struct.unpack("<BBHBBBBHHHBBBB", desc.raw)
                if (vid, pid) != (VID, PID):
                    continue
                handle = ctypes.c_void_p()
                if self.lib.libusb_open(dev, ctypes.byref(handle)) != 0:
                    continue
                buf = ctypes.create_string_buffer(64)
                got = self.lib.libusb_get_string_descriptor_ascii(handle, i_serial, buf, 64)
                serial = buf.raw[:got].decode("ascii", "replace") if got > 0 else "?"
                if keep and (want is None or serial == want):
                    found.append((serial, handle))
                else:
                    if not keep:
                        found.append((serial, None))
                    self.lib.libusb_close(handle)
        finally:
            if count > 0:
                self.lib.libusb_free_device_list(devices, 1)
        return found

    def config_descriptor(self, handle) -> bytes:
        buf = ctypes.create_string_buffer(512)
        # GET_DESCRIPTOR(CONFIGURATION 0), device-to-host, standard, device.
        n = self.lib.libusb_control_transfer(handle, 0x80, 0x06, DESC_CONFIGURATION << 8, 0, buf, 512, 1000)
        if n < 0:
            raise UsbError(f"could not read the configuration descriptor ({n})")
        return buf.raw[:n]

    def attach(self, handle, interface: int, listening: bool) -> int:
        # Host-to-device, vendor, interface.
        return self.lib.libusb_control_transfer(
            handle, 0x41, LOG_REQUEST_ATTACH, 1 if listening else 0, interface, None, 0, 1000
        )


def read(usb: Libusb, serial: Optional[str], once: bool, out) -> int:
    waiting = False
    while True:
        serial_found, handle = usb.open(serial)
        if handle is None:
            if once:
                print("log_reader: no board with the RNode image attached", file=sys.stderr)
                return 1
            if not waiting:
                print("log_reader: waiting for the board...", file=sys.stderr)
                waiting = True
            time.sleep(0.5)
            continue
        waiting = False
        try:
            log = find_log_interface(usb.config_descriptor(handle))
            if log is None:
                print(
                    "log_reader: this board's image has no log interface "
                    "(an older image logs on its second serial port)",
                    file=sys.stderr,
                )
                return 1
            rc = usb.lib.libusb_claim_interface(handle, log.number)
            if rc in (LIBUSB_ERROR_ACCESS, LIBUSB_ERROR_BUSY):
                raise UsbError(f"{serial_found}'s log is already being read (another log_reader?)")
            if rc != 0:
                raise UsbError(f"could not claim interface {log.number} ({rc})")
            try:
                if usb.attach(handle, log.number, True) < 0:
                    raise UsbError("the board refused the attach request")
                print(f"log_reader: reading {serial_found}", file=sys.stderr)
                buf = ctypes.create_string_buffer(4096)
                got = ctypes.c_int()
                while True:
                    rc = usb.lib.libusb_bulk_transfer(handle, log.endpoint, buf, 4096, ctypes.byref(got), 500)
                    if got.value:
                        out.write(buf.raw[: got.value])
                        out.flush()
                    gone = board_went_away(rc)
                    if gone is None:
                        raise UsbError(f"bulk read failed ({rc})")
                    if gone:
                        break
            finally:
                usb.attach(handle, log.number, False)
                usb.lib.libusb_release_interface(handle, log.number)
        finally:
            usb.lib.libusb_close(handle)
        if once:
            return 0
        print("log_reader: board went away; waiting for it to come back", file=sys.stderr)
        # Pin to the board that went, so a reset does not hand us the other one.
        serial = serial_found


def _stop(_signum, _frame):
    raise KeyboardInterrupt


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--serial", help="USB serial number of the board to read")
    parser.add_argument("--list", action="store_true", help="list attached boards and exit")
    parser.add_argument("--once", action="store_true", help="exit when the board goes away")
    args = parser.parse_args(argv)

    # A reader piped into defmt-print that loses its reader should go quietly.
    signal.signal(signal.SIGPIPE, signal.SIG_DFL)
    # Stopping always goes through the `finally` that tells the board, however
    # it is asked for. Set explicitly because a background job started by a
    # non-interactive shell inherits SIGINT ignored, and Python then installs
    # no handler of its own -- that reader outlived its `kill -INT`, measured.
    signal.signal(signal.SIGINT, _stop)
    signal.signal(signal.SIGTERM, _stop)
    try:
        usb = Libusb()
        if args.list:
            for serial in usb.boards():
                print(serial)
            return 0
        return read(usb, args.serial, args.once, sys.stdout.buffer)
    except UsbError as e:
        print(f"log_reader: {e}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main())
