#!/usr/bin/env python3
"""Package a linked XIAO application for Seeed's serial DFU bootloader."""

import argparse
from pathlib import Path
import struct
import subprocess


def validate(hex_path):
    """Check every emitted address before handing an image to the DFU tool."""
    memory = {}
    base = 0
    for line in hex_path.read_text().splitlines():
        if not line.startswith(":"):
            raise ValueError("invalid Intel HEX record")
        record = bytes.fromhex(line[1:])
        count = record[0]
        if len(record) != count + 5 or sum(record) & 255:
            raise ValueError("invalid Intel HEX length/checksum")
        kind = record[3]
        data = record[4:-1]
        if kind in (2, 4):
            base = int.from_bytes(data, "big") << (4 if kind == 2 else 16)
        elif kind == 0:
            address = base + int.from_bytes(record[1:3], "big")
            if not 0x27000 <= address < address + count <= 0xED000:
                raise ValueError(f"record outside XIAO application region: {address:#x}")
            for offset, byte in enumerate(data):
                if address + offset in memory:
                    raise ValueError("overlapping Intel HEX data")
                memory[address + offset] = byte
        elif kind not in (1, 3, 5):
            raise ValueError(f"unsupported Intel HEX record: {kind}")
    if not memory or min(memory) != 0x27000:
        raise ValueError("missing XIAO application vector table at 0x27000")
    sp, reset = struct.unpack("<II", bytes(memory[0x27000 + i] for i in range(8)))
    if not 0x20000000 < sp <= 0x20040000 or sp % 8:
        raise ValueError("invalid initial stack pointer")
    if not reset & 1 or (reset & ~1) not in memory:
        raise ValueError("invalid Thumb reset vector")
    print(f"Checked application: {len(memory)} bytes, "
          f"0x27000..{max(memory) + 1:#x}, MSP={sp:#x}, reset={reset:#x}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("elf", type=Path)
    parser.add_argument("zip", type=Path)
    parser.add_argument("--objcopy", default="objcopy")
    parser.add_argument("--nrfutil", default="adafruit-nrfutil")
    args = parser.parse_args()
    hex_path = args.zip.with_suffix(".hex")
    # Generic little-endian ELF input also works with the host's GNU objcopy.
    subprocess.run([args.objcopy, "-I", "elf32-little", "-O", "ihex",
                    str(args.elf), str(hex_path)], check=True)
    validate(hex_path)
    # Application only. The installed S140 v7.3.0 and bootloader are preserved;
    # Lattice does not enable or execute the SoftDevice radio stack.
    subprocess.run([args.nrfutil, "dfu", "genpkg", "--dev-type", "0x0052",
                    "--sd-req", "0x0123", "--application", str(hex_path),
                    str(args.zip)], check=True)


if __name__ == "__main__":
    main()
