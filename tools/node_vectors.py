#!/usr/bin/env python3.11
"""Writes the Reticulum packets `oxinode_core::node` is tested against.

Every byte comes from stock Python Reticulum, not from a hand-built packet, so
the tests hold the board's stack to what a real peer puts on the air:

* `announce.bin`: an `lxmf.delivery` announce from a fixed identity, with a
  display name in its app data the way Sideband and MeshChat send one;
* `path_request.bin`: a path request for that destination, as
  `Transport.request_path` builds it on a node that is not a transport node;
* `vectors.txt`: what the tests expect to read out of them.

The identity is fixed so the hashes are the same on every run. The announce
is not: it carries a random hash and a timestamp, so regenerating it changes
the file and nothing the tests check.

    python3.11 tools/node_vectors.py core/src/node/vectors

Needs Reticulum (`pip install rns`), which is why it is a script run by hand
and its output is committed, rather than something `test.sh` runs.
"""

import argparse
import os
import sys
import tempfile

# A fixed X25519 + Ed25519 private key, so the destination hash is stable.
PRIVATE_KEY = bytes(range(64))
DISPLAY_NAME = "Bench peer"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", help="directory to write the vectors into")
    args = parser.parse_args(argv)

    import RNS
    import RNS.vendor.umsgpack as msgpack

    # A Reticulum with no interfaces, in a configuration directory of its
    # own, so nothing is sent and nothing on this machine is touched.
    configdir = tempfile.mkdtemp(prefix="oxinode-vectors-")
    with open(os.path.join(configdir, "config"), "w") as f:
        f.write("[reticulum]\n  share_instance = No\n  enable_transport = No\n[interfaces]\n")
    RNS.Reticulum(configdir=configdir, loglevel=RNS.LOG_ERROR)

    identity = RNS.Identity.from_bytes(PRIVATE_KEY)
    delivery = RNS.Destination(identity, RNS.Destination.IN, RNS.Destination.SINGLE, "lxmf", "delivery")
    # LXMF's announce app data: the display name and a stamp cost, packed.
    app_data = msgpack.packb([DISPLAY_NAME.encode("utf-8"), None])
    announce = delivery.announce(app_data=app_data, send=False)
    announce.pack()

    path_request_destination = RNS.Destination(
        None, RNS.Destination.OUT, RNS.Destination.PLAIN, RNS.Transport.APP_NAME, "path", "request"
    )
    tag = RNS.Identity.get_random_hash()
    path_request = RNS.Packet(
        path_request_destination,
        delivery.hash + tag,
        packet_type=RNS.Packet.DATA,
        transport_type=RNS.Transport.BROADCAST,
        header_type=RNS.Packet.HEADER_1,
    )
    path_request.pack()

    os.makedirs(args.out, exist_ok=True)
    with open(os.path.join(args.out, "announce.bin"), "wb") as f:
        f.write(announce.raw)
    with open(os.path.join(args.out, "path_request.bin"), "wb") as f:
        f.write(path_request.raw)
    with open(os.path.join(args.out, "vectors.txt"), "w") as f:
        f.write(f"# Written by tools/node_vectors.py with RNS {RNS.__version__}.\n")
        f.write(f"destination {delivery.hash.hex()}\n")
        f.write(f"identity {identity.hash.hex()}\n")
        f.write(f"public_key {identity.get_public_key().hex()}\n")
        f.write(f"name_hash {delivery.name_hash.hex()}\n")
        f.write(f"app_data {app_data.hex()}\n")
    print(f"wrote {args.out}: announce {len(announce.raw)} B, path request {len(path_request.raw)} B")
    return 0


if __name__ == "__main__":
    sys.exit(main())
