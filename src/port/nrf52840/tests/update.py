#!/usr/bin/env python3
"""Build/package the Nordic boot check, or drive its local USB staging commands.

Preparing artifacts never flashes a board. Initial provisioning uses the factory
DFU twice: combined loader + A, then loader only, so its CRC covers immutable code.
"""
import argparse
import sys
import hashlib
import re
import shutil
import struct
import subprocess
import time
from pathlib import Path

from package_xiao import read_hex, validate

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[1] / "tests"))
from update_console import Console, CHUNK_SIZE
BOOT = HERE.parent / "boot"
TARGET = "thumbv7em-none-eabihf"


def layout():
    # This literal is also compiled by both Rust linker-script generators. Fail
    # on a changed representation instead of silently using old flash addresses.
    source = (BOOT / "src/layout.rs").read_text()
    body = re.search(r"pub const XIAO: Layout = Layout \{([^}]+)\}", source).group(1)
    fields = {key: int(value, 16) for key, value in
              re.findall(r"(\w+):\s*(0x[0-9a-fA-F]+)\s*,", body)}
    if set(fields) != {"loader_start", "state_start", "active_start", "dfu_start", "journal_start", "end"}:
        raise ValueError("unsupported XIAO layout definition")
    return fields


def check_image(memory, start, end):
    if not memory or min(memory) != start or max(memory) >= end:
        raise ValueError(f"image does not fit {start:#x}..{end:#x}")
    vectors = bytes(memory[start + i] for i in range(8))
    sp, reset = struct.unpack("<II", vectors)
    if not 0x20000000 < sp <= 0x20040000 or sp % 8:
        raise ValueError("invalid image stack pointer")
    if not reset & 1 or not start + 8 <= (reset & ~1) < end or reset & ~1 not in memory:
        raise ValueError("invalid image reset vector")


def write_hex(path, memory):
    records = []

    def record(address, kind, data):
        body = bytes([len(data)]) + struct.pack(">HB", address, kind) + data
        records.append(":" + (body + bytes([-sum(body) & 255])).hex().upper())

    addresses = sorted(memory)
    upper = None
    i = 0
    while i < len(addresses):
        address = addresses[i]
        if address >> 16 != upper:
            upper = address >> 16
            record(0, 4, struct.pack(">H", upper))
        data = bytearray([memory[address]])
        i += 1
        while (i < len(addresses) and len(data) < 16
               and addresses[i] == address + len(data)
               and addresses[i] >> 16 == upper):
            data.append(memory[addresses[i]])
            i += 1
        record(address & 65535, 0, data)
    record(0, 1, b"")
    path.write_text("\n".join(records) + "\n")


def prepare(args):
    if args.staging_check and not args.ota:
        raise ValueError("--staging-check requires --ota")
    if args.watchdog_check and not args.ota:
        raise ValueError("--watchdog-check requires --ota")
    if args.activation_check and not args.ota:
        raise ValueError("--activation-check requires --ota")
    out = HERE / ("target/ota" if args.ota else "target/update")
    out.mkdir(parents=True, exist_ok=True)
    regions = layout()
    common = ["cargo", "+nightly", "build", "-Z", "build-std=core", "--release", "--locked"]
    subprocess.run(common + ["--target", TARGET, "--features", "loader", "--bin", "lattice-nrf52840-loader"], cwd=BOOT, check=True)
    loader_elf = BOOT / "target" / TARGET / "release/lattice-nrf52840-loader"
    subprocess.run([args.objcopy, "-I", "elf32-little", "-O", "ihex", str(loader_elf), str(out / "loader.hex")], check=True)
    loader = read_hex(out / "loader.hex")
    check_image(loader, regions["loader_start"], regions["state_start"])
    images = {}
    builds = [("a", "ota-test"), ("b", "ota-image-b")] if args.ota else [("a", "update-test"), ("b", "update-image-b")]
    if args.watchdog_check:
        builds += [("stall-entry", "ota-stall-before-init"), ("stall-radio", "ota-stall-before-radio")]
    for name, features in builds:
        if args.staging_check and name == "a": features += ",ota-staging-check"
        if args.activation_check and name == "a": features += ",ota-activation-check"
        subprocess.run(common + ["--bin", "ota" if args.ota else "update", "--features", features], cwd=HERE, check=True)
        elf = HERE / "target" / TARGET / ("release/ota" if args.ota else "release/update")
        shutil.copyfile(elf, out / f"{name}.elf")
        subprocess.run([args.objcopy, "-I", "elf32-little", "-O", "ihex", str(elf), str(out / f"{name}.hex")], check=True)
        image = read_hex(out / f"{name}.hex")
        check_image(image, regions["active_start"], regions["dfu_start"])
        data = bytes(image.get(address, 255) for address in range(min(image), max(image) + 1))
        (out / f"{name}.bin").write_bytes(data)
        images[name] = image
        print(f"Image {name.upper()}: {len(data)} bytes, SHA256 {hashlib.sha256(data).hexdigest()}")
    # The receiver authenticates any retained journal and initializes it before
    # accepting image bytes. Do not pad factory DFU across the inactive slot.
    write_hex(out / "initial.hex", loader | images["a"])
    for name in ["initial", "loader"]:
        validate(out / f"{name}.hex")
        subprocess.run([args.nrfutil, "dfu", "genpkg", "--dev-type", "0x0052", "--sd-req", "0x0123",
                        "--application", str(out / f"{name}.hex"), str(out / f"{name}.zip")], check=True)
    print(f"Prepared {out}; no device accessed.")



def drive(args):
    console = Console(args.port)
    try:
        if args.action == "stage":
            data = args.image.read_bytes()
            regions = layout()
            check_image(dict(enumerate(data, regions["active_start"])), regions["active_start"], regions["dfu_start"])
            print(console.command(1, struct.pack("<I", len(data))))
            while "progress=1 " not in console.command(2):
                pass
            for offset in range(0, len(data), CHUNK_SIZE):
                console.command(3, struct.pack("<I", offset) + data[offset:offset + CHUNK_SIZE])
            print(console.command(4, hashlib.sha256(data).digest()))
            print("Verified and selected for trial. Use reset to boot it; confirm only after checking startup.")
        else:
            print(console.command({"status": 0, "reset": 5, "confirm": 6, "dfu": 7}[args.action]))
            if args.action in ("reset", "dfu"):
                time.sleep(0.25)  # let the completion leave USB before reset
    finally:
        console.serial.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    build = sub.add_parser("prepare")
    build.add_argument("--nrfutil", default="adafruit-nrfutil")
    build.add_argument("--objcopy", default="objcopy")
    build.add_argument("--activation-check", action="store_true", help="hold automatic activation for ten seconds after verification in image A")
    build.add_argument("--staging-check", action="store_true", help="hold staging for ten seconds after accepting an offer in image A")
    build.add_argument("--watchdog-check", action="store_true", help="also build stalled trial images for hardware rollback checks")
    build.add_argument("--ota", action="store_true", help="build the signed receiver and binary host interface")
    for action in ["status", "stage", "reset", "confirm", "dfu"]:
        command = sub.add_parser(action)
        command.add_argument("--port", required=True)
        if action == "stage":
            command.add_argument("image", type=Path)
    args = parser.parse_args()
    prepare(args) if args.action == "prepare" else drive(args)


if __name__ == "__main__":
    main()
