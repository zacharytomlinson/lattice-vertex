#!/usr/bin/env python3
"""Create a release key or sign a fixed-format Lattice firmware package.

Requires Python cryptography. Signing never writes to a device. Private keys must
remain outside source control; the receiver is provisioned with only the public key.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import struct
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import (
    Encoding,
    PrivateFormat,
    PublicFormat,
    NoEncryption,
)

HEADER_BYTES = 192
COST_FIELDS = (
    "erase_page_us",
    "write_per_byte",
    "verify_per_byte",
    "activation_us",
    "swap_per_byte",
    "longest_pause_us",
)


def keygen(args):
    key = Ed25519PrivateKey.generate()
    raw = key.private_bytes(Encoding.Raw, PrivateFormat.Raw, NoEncryption())
    fd = os.open(args.key, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as output:
        output.write(raw)
    public = key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
    public_path = Path(str(args.key) + ".pub")
    with public_path.open("xb") as output:
        output.write(public)
    print(f"Public key: {public.hex()}")
    print(f"Private key saved to {args.key}; public key saved to {public_path}")


def pack(args):
    image = args.image.read_bytes()
    surface = args.surface.read_bytes()
    candidate_surface = args.candidate_surface.read_bytes()
    if surface != candidate_surface:
        raise ValueError(
            "candidate changes exported surface/handles; rebind is not supported yet"
        )
    if not image or len(image) > 0xFFFFFFFF:
        raise ValueError("invalid image length")
    appliance = bytes.fromhex(args.appliance)
    if len(appliance) != 16:
        raise ValueError("appliance family must be 16 bytes in hex")
    if args.costs:
        costs = json.loads(args.costs.read_text())
        values = [costs[name] for name in COST_FIELDS]
        if any(type(v) is not int or not 0 <= v < 0xFFFFFFFF for v in values):
            raise ValueError("dry-run costs must contain six known u32 coefficients")
    else:
        values = [
            0xFFFFFFFF
        ] * 6  # explicitly unmeasured fixtures; no automatic scheduling
    header = (
        b"LVUP"
        + struct.pack("<HHI", 1, args.platform, args.layout)
        + appliance
        + hashlib.sha256(surface).digest()
        + struct.pack("<QI", args.release, len(image))
        + hashlib.sha256(image).digest()
        + struct.pack("<6I", *values)
    )
    assert len(header) == 128
    key = Ed25519PrivateKey.from_private_bytes(args.key.read_bytes())
    signed = header + key.sign(header)
    assert len(signed) == HEADER_BYTES
    args.output.write_bytes(signed + image)
    print(
        f"Package {hashlib.sha256(signed).digest()[:16].hex()}: {len(image)} image bytes, release {args.release}"
    )
    if not args.costs:
        print(
            "Local timing costs are explicitly UNKNOWN; this is not an automatically schedulable release."
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    k = sub.add_parser("keygen")
    k.add_argument("--key", type=Path, required=True)
    p = sub.add_parser("pack")
    for name in ("image", "surface", "candidate-surface", "key", "output"):
        p.add_argument("--" + name, type=Path, required=True)
    p.add_argument("--platform", type=int, choices=[1, 2], required=True)
    p.add_argument("--layout", type=int, required=True)
    p.add_argument("--appliance", required=True)
    p.add_argument("--release", type=int, required=True)
    c = p.add_mutually_exclusive_group(required=True)
    c.add_argument("--costs", type=Path)
    c.add_argument("--unmeasured", action="store_true")
    args = parser.parse_args()
    keygen(args) if args.command == "keygen" else pack(args)


if __name__ == "__main__":
    main()
