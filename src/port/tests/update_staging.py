#!/usr/bin/env python3
"""Check bounded staging using the maintained A image's ten-second work hold.

Provision A built with --ota --staging-check. Supply its signed B package. This
uses the local kernel receiver to isolate flash admission from RF delivery.
"""

import argparse
import hashlib
from pathlib import Path
import struct
import sys
import time
import serial

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tools"))
from update_peer import Peer, parse_progress, upload


def status_after_reset(port, deadline):
    while time.monotonic() < deadline:
        peer = None
        try:
            peer = Peer(port)
            return peer.control(0)
        except (OSError, serial.SerialException, TimeoutError):
            time.sleep(0.1)
        finally:
            if peer:
                peer.close()
    raise TimeoutError("receiver did not return")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", required=True)
    parser.add_argument("--package", type=Path, required=True)
    args = parser.parse_args()
    image = args.package.read_bytes()
    header = image[:192]
    package = hashlib.sha256(header).digest()[:16]
    peer = Peer(args.port)
    try:
        owner, _, _ = peer.info()
        before = peer.control(0)
        expected = struct.unpack_from("<Q", header, 60)[0]
        if before["release"] >= expected:
            raise RuntimeError("provision A before running this check")
        for offset in range(0, 192, 48):
            p = peer.update(
                owner,
                b"\0" + package + bytes([offset, 48]) + header[offset : offset + 48],
            )
        if p["phase"] != "preparing":
            raise RuntimeError(("expected a fresh candidate", p))
        time.sleep(0.1)  # let the fixture acquire its physical-work hold
        start = time.monotonic()
        deferred = responsive = 0
        frozen = None
        while time.monotonic() - start < 8:
            op, raw = peer.exchange(5, struct.pack("<Q", owner) + b"\x01" + package)
            if op != 0x85:
                raise RuntimeError("unexpected update reply")
            p = parse_progress(raw[8:36])
            if p["status"] != "busy" or p["phase"] != "preparing":
                raise RuntimeError(("staging advanced during critical work", p))
            progress = (p["accepted"], p["committed"], p["prepared"])
            if frozen is None:
                frozen = progress
            if progress != frozen:
                raise RuntimeError("deferred request changed progress")
            deferred += 1
            if peer.control(0)["release"] != before["release"]:
                raise RuntimeError("rebooted while staging was held")
            responsive += 1
            time.sleep(0.1)
        print(
            f"PASS held staging: {deferred} Busy responses, {responsive} ordinary status replies; progress {frozen}",
            flush=True,
        )
        upload(peer, owner, args.package)
        started = time.monotonic()
        print("Selected:", peer.control(1), flush=True)
        print(f"Selection response: {time.monotonic() - started:.3f}s", flush=True)
        print("Reset:", peer.control(3), flush=True)
    finally:
        peer.close()
    time.sleep(1)
    status = status_after_reset(args.port, time.monotonic() + 90)
    if status["release"] != expected:
        raise RuntimeError(status)
    peer = Peer(args.port)
    try:
        print("Trial:", status, flush=True)
        print("Reset without host confirmation:", peer.control(3), flush=True)
    finally:
        peer.close()
    time.sleep(1)
    status = status_after_reset(args.port, time.monotonic() + 90)
    if status["release"] != expected:
        raise RuntimeError(("automatic confirmation did not persist", status))
    print(
        "PASS staged, incrementally verified, and retained release:",
        expected,
        flush=True,
    )


if __name__ == "__main__":
    main()
