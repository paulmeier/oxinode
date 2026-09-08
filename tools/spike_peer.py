#!/usr/bin/env python3
"""The other end of the phase 15 spike: a real Reticulum on the host.

Runs Python Reticulum with one `SerialInterface` pointed at the spike image's
first USB port, and talks to the board the way any Reticulum peer would:

  * it announces `oxinode.spike` and an LXMF delivery destination every
    twenty seconds, so the board learns a public key to encrypt to;
  * when the board's `oxinode.spike` announce arrives it sends an encrypted
    packet there and prints what comes back;
  * when the board's `lxmf.delivery` announce arrives it sends an
    opportunistic LXMF message there through the standard LXMF router, and
    prints the LXMF message the board answers with -- signature checked by
    LXMF, not by this script.

Its own configuration directory, so it never touches ~/.reticulum or a
running rnsd. Everything it prints is timestamped so it can be laid against
the board's defmt log.

    tools/spike_peer.py --serial /dev/cu.usbmodem101 --seconds 120

With a second oxinode running the `rnode` image, the same conversation can
be had over the air instead: `--rnode` points Reticulum's own
`RNodeInterface` at it, on the radio parameters the spike image uses when
the board has no stored configuration. Either or both may be given.

    tools/spike_peer.py --rnode /dev/cu.usbmodem3101 --seconds 120
"""

import argparse
import os
import sys
import threading
import time

import RNS
import LXMF

APP_NAME = "oxinode"
ASPECT = "spike"

CONFIG = """\
[reticulum]
  enable_transport = False
  share_instance = No
[logging]
  loglevel = {loglevel}
[interfaces]
"""

SERIAL = """\
  [[spike serial]]
    type = SerialInterface
    enabled = yes
    port = {port}
    speed = 115200
    databits = 8
    parity = none
    stopbits = 1
"""

# `oxinode_core::lr1121::config::DEFAULT`, as the spike image applies it
# when the board has no stored configuration.
RNODE = """\
  [[spike rnode]]
    type = RNodeInterface
    enabled = yes
    port = {port}
    frequency = 915000000
    bandwidth = 125000
    txpower = 14
    spreadingfactor = 8
    codingrate = 5
"""

t0 = time.time()
lock = threading.Lock()


def say(msg):
    with lock:
        print(f"[{time.time() - t0:8.3f}] {msg}", flush=True)


class SpikeHandler:
    """Answers the board's `oxinode.spike` announce with an encrypted packet."""

    aspect_filter = f"{APP_NAME}.{ASPECT}"

    def __init__(self):
        self.count = 0
        self.board = None

    def received_announce(self, destination_hash, announced_identity, app_data, *_, **__):
        say(f"announce oxinode.spike from {RNS.prettyhexrep(destination_hash)}")
        self.board = RNS.Destination(
            announced_identity,
            RNS.Destination.OUT,
            RNS.Destination.SINGLE,
            APP_NAME,
            ASPECT,
        )
        self.count += 1
        text = f"hello from the host #{self.count}".encode()
        packet = RNS.Packet(self.board, text)
        receipt = packet.send()
        say(f"sent {len(packet.raw)} bytes to oxinode.spike: {text!r} (receipt {receipt is not None})")


class LxmfHandler:
    """Answers the board's `lxmf.delivery` announce with an LXMF message."""

    aspect_filter = "lxmf.delivery"

    def __init__(self, router, source, own_hash):
        self.router = router
        self.source = source
        self.own_hash = own_hash
        self.count = 0

    def received_announce(self, destination_hash, announced_identity, app_data, *_, **__):
        if destination_hash == self.own_hash:
            return
        name = LXMF.display_name_from_app_data(app_data)
        say(f"announce lxmf.delivery from {RNS.prettyhexrep(destination_hash)} name={name!r}")
        dest = RNS.Destination(
            announced_identity,
            RNS.Destination.OUT,
            RNS.Destination.SINGLE,
            "lxmf",
            "delivery",
        )
        self.count += 1
        message = LXMF.LXMessage(
            dest,
            self.source,
            f"hello from the host, message {self.count}",
            title="spike",
            desired_method=LXMF.LXMessage.OPPORTUNISTIC,
        )
        self.router.handle_outbound(message)
        say(f"sent LXMF {RNS.prettyhexrep(message.hash)} to {RNS.prettyhexrep(destination_hash)}, "
            f"{message.packed_size} bytes packed, method {message.desired_method}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--serial", help="the spike image's first USB port, as a SerialInterface")
    ap.add_argument("--rnode", help="another oxinode running `rnode`, as an RNodeInterface")
    ap.add_argument("--seconds", type=float, default=120.0)
    ap.add_argument("--config", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), ".spike-peer"))
    ap.add_argument("--loglevel", type=int, default=4)
    ap.add_argument("--every", type=float, default=20.0, help="announce period in seconds")
    args = ap.parse_args()

    if not (args.serial or args.rnode):
        ap.error("give --serial and/or --rnode")
    config = CONFIG.format(loglevel=args.loglevel)
    if args.serial:
        config += SERIAL.format(port=args.serial)
    if args.rnode:
        config += RNODE.format(port=args.rnode)
    os.makedirs(args.config, exist_ok=True)
    with open(os.path.join(args.config, "config"), "w") as f:
        f.write(config)

    RNS.Reticulum(configdir=args.config)

    identity_path = os.path.join(args.config, "identity")
    if os.path.exists(identity_path):
        identity = RNS.Identity.from_file(identity_path)
    else:
        identity = RNS.Identity()
        identity.to_file(identity_path)
    say(f"host identity {RNS.prettyhexrep(identity.hash)}")

    spike_in = RNS.Destination(
        identity, RNS.Destination.IN, RNS.Destination.SINGLE, APP_NAME, ASPECT
    )
    say(f"host oxinode.spike {RNS.prettyhexrep(spike_in.hash)}")

    def on_spike_packet(data, packet):
        rssi = getattr(packet, "rssi", None)
        snr = getattr(packet, "snr", None)
        via = getattr(packet, "receiving_interface", None)
        say(f"packet at oxinode.spike: {data!r} ({len(packet.raw)} bytes on the wire, "
            f"via {via}, rssi {rssi}, snr {snr})")

    spike_in.set_packet_callback(on_spike_packet)

    router = LXMF.LXMRouter(identity=identity, storagepath=os.path.join(args.config, "lxmf"))
    delivery = router.register_delivery_identity(identity, display_name="spike host")
    say(f"host lxmf.delivery {RNS.prettyhexrep(delivery.hash)}")

    def on_lxmf(message):
        say(
            f"LXMF from {RNS.prettyhexrep(message.source_hash)}: title={message.title_as_string()!r} "
            f"content={message.content_as_string()!r} timestamp={message.timestamp:.3f} "
            f"signature_validated={message.signature_validated} method={message.method} "
            f"rssi={getattr(message, 'rssi', None)} snr={getattr(message, 'snr', None)}"
        )

    router.register_delivery_callback(on_lxmf)

    RNS.Transport.register_announce_handler(SpikeHandler())
    RNS.Transport.register_announce_handler(LxmfHandler(router, delivery, delivery.hash))

    deadline = time.time() + args.seconds
    next_announce = time.time() + 2.0
    try:
        while time.time() < deadline:
            if time.time() >= next_announce:
                spike_in.announce()
                router.announce(delivery.hash)
                say("announced oxinode.spike and lxmf.delivery")
                next_announce = time.time() + args.every
            time.sleep(0.2)
    except KeyboardInterrupt:
        pass
    say("done")
    router.exit_handler()
    os._exit(0)


if __name__ == "__main__":
    main()
