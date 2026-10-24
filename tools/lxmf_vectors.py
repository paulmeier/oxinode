#!/usr/bin/env python3.11
"""Writes the LXMF messages `oxinode_core::node::lxmf` is tested against.

Every byte comes from stock LXMF and Reticulum, not from a hand-built packet:
each message is an `LXMF.LXMessage` packed by LXMF for opportunistic delivery
and turned into a packet the way `LXMessage.__as_packet` does it, which is
the packet a sender puts on the air. So the destination hash is stripped from
the front of what is encrypted, exactly as on the air.

The board is a fixed identity (`bytes(range(64, 128))`) and the sender is the
one `node_vectors.py` announces (`bytes(range(64))`, "Bench peer"), so the
board's tests can hear that announce first and then the message.

* `message.bin`: a message with the punctuation people type on phones --
  curly quotes, an en dash, an ellipsis -- and no title or fields;
* `message_retry.bin`: the same message sent again, as LXMF sends it when a
  proof is lost: a new packet, encrypted afresh, with the same message hash;
* `message_fields.bin`: a title, and fields: an image, which is an attachment,
  and a renderer, which is not;
* `message_stamped.bin`: a message carrying a stamp, the fifth element LXMF
  leaves out of the message hash;
* `stranger_message.bin` and `stranger_announce.bin`: a message from an
  identity the board has not heard, and that identity's announce;
* `proof.bin`: the delivery proof Reticulum makes for `message.bin` when the
  board's identity receives it: what the board must send back, to the byte;
* `lxmf.txt`: what the tests expect to read out of them.

Ed25519 is deterministic, so the signatures and the proof are the same on
every run; the encryption is not, so regenerating changes the packets and
nothing the tests check.

    python3.11 tools/lxmf_vectors.py core/src/node/vectors

Needs Reticulum and LXMF (`pip install lxmf`), which is why it is run by hand
and its output is committed.
"""

import argparse
import os
import sys
import tempfile

BOARD_KEY = bytes(range(64, 128))
SENDER_KEY = bytes(range(64))
STRANGER_KEY = bytes(range(128, 192))
# 2026-10-24 00:00:00.25 UTC: a float, as LXMF stamps every message.
TIMESTAMP = 1_792_800_000.25
CONTENT = "Hello from the bench – it’s “Python”…"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", help="directory to write the vectors into")
    args = parser.parse_args(argv)

    import LXMF
    import RNS
    import RNS.vendor.umsgpack as msgpack

    # A Reticulum with no interfaces, in a configuration directory of its
    # own, so nothing is sent and nothing on this machine is touched.
    configdir = tempfile.mkdtemp(prefix="oxinode-lxmf-vectors-")
    with open(os.path.join(configdir, "config"), "w") as f:
        f.write("[reticulum]\n  share_instance = No\n  enable_transport = No\n[interfaces]\n")
    RNS.Reticulum(configdir=configdir, loglevel=RNS.LOG_ERROR)

    board = RNS.Identity.from_bytes(BOARD_KEY)
    board_in = RNS.Destination(board, RNS.Destination.IN, RNS.Destination.SINGLE, "lxmf", "delivery")
    board_out = RNS.Destination(
        RNS.Identity.from_bytes(BOARD_KEY), RNS.Destination.OUT, RNS.Destination.SINGLE, "lxmf", "delivery"
    )
    sender = RNS.Identity.from_bytes(SENDER_KEY)
    sender_in = RNS.Destination(sender, RNS.Destination.IN, RNS.Destination.SINGLE, "lxmf", "delivery")
    stranger = RNS.Identity.from_bytes(STRANGER_KEY)
    stranger_in = RNS.Destination(stranger, RNS.Destination.IN, RNS.Destination.SINGLE, "lxmf", "delivery")

    def message(source, content, title="", fields=None, ticket=None):
        lxm = LXMF.LXMessage(
            board_out, source, content, title, fields=fields, desired_method=LXMF.LXMessage.OPPORTUNISTIC
        )
        lxm.timestamp = TIMESTAMP
        if ticket is not None:
            # As `LXMRouter.handle_outbound` does for a peer that gave us a
            # ticket: the stamp is made at once, from the ticket.
            lxm.outbound_ticket = ticket
            lxm.defer_stamp = False
        lxm.pack()
        assert lxm.method == LXMF.LXMessage.OPPORTUNISTIC, lxm.method
        return lxm

    def on_air(lxm):
        # `LXMessage.__as_packet` for an opportunistic message: the packed
        # message with the destination hash taken off, encrypted to it.
        packet = RNS.Packet(board_out, lxm.packed[LXMF.LXMessage.DESTINATION_LENGTH :])
        packet.pack()
        return packet.raw

    def received(raw):
        # What the board's side of Reticulum makes of it: the packet hash it
        # proves, and the plaintext LXMF reads.
        packet = RNS.Packet(None, raw)
        packet.unpack()
        packet.destination = board_in
        return packet, board.decrypt(packet.data)

    plain = message(sender_in, CONTENT)
    fields = message(
        sender_in,
        "see attached",
        title="Photo",
        fields={LXMF.FIELD_IMAGE: ["webp", bytes(8)], LXMF.FIELD_RENDERER: LXMF.RENDERER_PLAIN},
    )
    stamped = message(sender_in, "stamped", ticket=bytes(range(16)))
    assert stamped.stamp is not None
    from_stranger = message(stranger_in, "who is this")

    packets = {
        "message.bin": on_air(plain),
        "message_retry.bin": on_air(plain),
        "message_fields.bin": on_air(fields),
        "message_stamped.bin": on_air(stamped),
        "stranger_message.bin": on_air(from_stranger),
    }
    assert packets["message.bin"] != packets["message_retry.bin"], "a retry is a new packet"

    # Each must be what LXMF itself reads back, signature and all, once the
    # sender is known -- and the destination hash goes back on the front
    # first, as `LXMRouter.delivery_packet` puts it.
    for name, lxm in [
        ("message.bin", plain),
        ("message_fields.bin", fields),
        ("message_stamped.bin", stamped),
        ("stranger_message.bin", from_stranger),
    ]:
        _, plaintext = received(packets[name])
        back = LXMF.LXMessage.unpack_from_bytes(board_in.hash + plaintext)
        assert back.hash == lxm.hash, name
        assert back.signature_validated, name

    # The proof, as `Identity.prove` makes it for the receiving destination.
    packet, _ = received(packets["message.bin"])
    signature = board.sign(packet.packet_hash)
    assert RNS.Reticulum.should_use_implicit_proof()
    proof = RNS.Packet(packet.generate_proof_destination(), signature, RNS.Packet.PROOF)
    proof.pack()

    stranger_announce = stranger_in.announce(app_data=msgpack.packb([b"Stranger", None]), send=False)
    stranger_announce.pack()

    os.makedirs(args.out, exist_ok=True)
    for name, raw in packets.items():
        with open(os.path.join(args.out, name), "wb") as f:
            f.write(raw)
    with open(os.path.join(args.out, "proof.bin"), "wb") as f:
        f.write(proof.raw)
    with open(os.path.join(args.out, "stranger_announce.bin"), "wb") as f:
        f.write(stranger_announce.raw)
    with open(os.path.join(args.out, "lxmf.txt"), "w") as f:
        f.write(f"# Written by tools/lxmf_vectors.py with LXMF {LXMF.__version__} and RNS {RNS.__version__}.\n")
        f.write(f"board {board_in.hash.hex()}\n")
        f.write(f"sender {sender_in.hash.hex()}\n")
        f.write(f"stranger {stranger_in.hash.hex()}\n")
        f.write(f"message_hash {plain.hash.hex()}\n")
        f.write(f"fields_hash {fields.hash.hex()}\n")
        f.write(f"stamped_hash {stamped.hash.hex()}\n")
        f.write(f"stamp {stamped.stamp.hex()}\n")
        f.write(f"stranger_hash {from_stranger.hash.hex()}\n")
        f.write(f"packet_hash {packet.packet_hash.hex()}\n")
        f.write(f"content {CONTENT.encode('utf-8').hex()}\n")
    print(f"wrote {args.out}: message {len(packets['message.bin'])} B, proof {len(proof.raw)} B")
    return 0


if __name__ == "__main__":
    sys.exit(main())
