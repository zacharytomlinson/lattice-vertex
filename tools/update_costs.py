#!/usr/bin/env python3
"""Dry-run an image through a maintained local port fixture and record update costs.

Uses device-measured flash operation times, and host-observed reset-to-peer times.
Stages, selects, boots, and confirms the candidate. Requires pyserial. The matching
fixture must already be installed; this tool never provisions a boot layout.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import struct
import sys
import time
import serial

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src/port/tests"))
from update_console import Console, CHUNK_SIZE


def elapsed(line):
    match = re.search(r" elapsed_us=(\d+)", line)
    if not match:
        raise ValueError("fixture does not provide device timing; rebuild it")
    return int(match[1])


def reboot(console, port):
    console.command(5)
    started = time.monotonic_ns()
    console.serial.close()
    time.sleep(0.5)
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        candidate = None
        try:
            candidate = Console(port)
            line = candidate.command(0, timeout=1)
            if "peer_ready=1" not in line:
                candidate.serial.close()
                time.sleep(0.1)
                continue
            return candidate, (time.monotonic_ns() - started + 999) // 1000, line
        except (serial.SerialException, OSError, TimeoutError):
            if candidate:
                candidate.serial.close()
            time.sleep(0.1)
    raise TimeoutError("candidate did not return; inspect recovery before proceeding")


def measure(args):
    data = args.image.read_bytes()
    digest = hashlib.sha256(data).digest()
    console = Console(args.port)
    try:
        before = console.command(0)
        if not any(f"boot={state} " in before for state in ("Boot", "Valid", "Revert")):
            raise ValueError(
                "confirm the running trial or restore its prior image before measuring"
            )
        if "ready=1 " not in before:
            raise ValueError("fixture storage is not provisioned for updates")
        console, baseline, _ = reboot(console, args.port)
        print(
            f"Baseline reset-to-peer: {baseline} us; staging {len(data)} bytes",
            flush=True,
        )
        console.command(1, struct.pack("<I", len(data)))
        erase_max = write_max = pause_max = 0
        pages = 0
        while True:
            line = console.command(2)
            erase_max = max(erase_max, elapsed(line))
            pages += 1
            if "progress=1 " in line:
                break
        for offset in range(0, len(data), CHUNK_SIZE):
            chunk = data[offset : offset + CHUNK_SIZE]
            duration = elapsed(console.command(3, struct.pack("<I", offset) + chunk))
            write_max = max(write_max, (duration * 256 + len(chunk) - 1) // len(chunk))
            pause_max = max(pause_max, duration)
        print(
            f"Measured {pages} erase pages and {len(data)} programmed bytes; verifying",
            flush=True,
        )
        verify = elapsed(console.command(8, digest))
        select = elapsed(console.command(4, digest))
        print("Booting trial for outage measurement", flush=True)
        console, trial, status = reboot(console, args.port)
        old_image = re.search(r" image=(\S+)", before)[1]
        if re.search(r" image=(\S+)", status)[1] == old_image:
            raise RuntimeError(
                "trial did not change fixture identity; refusing confirmation"
            )
        console.command(6)

        def margin(value):
            return (value * args.margin_percent + 99) // 100

        swap = max(0, trial - baseline) if args.swap_bytes else 0
        result = {
            "erase_page_us": margin(erase_max),
            "write_per_byte": margin(write_max),
            "verify_per_byte": margin((verify * 256 + len(data) - 1) // len(data)),
            "activation_us": margin(
                (baseline if args.swap_bytes else max(baseline, trial)) + select
            ),
            "swap_per_byte": margin(
                (swap * 256 + args.swap_bytes - 1) // args.swap_bytes
            )
            if args.swap_bytes
            else 0,
            "longest_pause_us": margin(max(erase_max, pause_max, verify)),
            "measurement": {
                "image_sha256": digest.hex(),
                "image_bytes": len(data),
                "erased_pages": pages,
                "reset_baseline_us": baseline,
                "trial_reset_us": trial,
                "verify_us": verify,
                "select_us": select,
                "swap_bytes": args.swap_bytes,
                "margin_percent": args.margin_percent,
                "note": "Flash timing is on-device; reset timing includes local peer formation/join, fixture USB return and host waits.",
            },
        }
        args.output.write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2), flush=True)
    finally:
        console.serial.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", required=True)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--swap-bytes",
        type=int,
        default=0,
        help="actual bootloader swap extent; Nordic XIAO revision 2 is 380928",
    )
    parser.add_argument("--margin-percent", type=int, default=125)
    args = parser.parse_args()
    if args.swap_bytes < 0 or args.margin_percent < 100:
        parser.error(
            "swap extent must be nonnegative and timing margin at least 100 percent"
        )
    measure(args)


if __name__ == "__main__":
    main()
