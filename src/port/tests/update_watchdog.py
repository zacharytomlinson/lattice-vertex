#!/usr/bin/env python3
"""Exercise signed stalled trials and automatic confirmation on one attached MCU.

Requires the matching A image and watchdog-capable loader already provisioned.
The image directory must contain signed stall-entry-watchdog.lvup,
stall-radio-watchdog.lvup, and b-watchdog.lvup packages (release 2).
Transfers use the local kernel receiver; this checks flash/boot behavior, not RF.
No confirmation command or reset is sent while waiting for a failed trial.
"""

import argparse
from pathlib import Path
import sys
import time
import serial

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tools"))
from update_peer import Peer, upload


def status(port, deadline):
    last = None
    while time.monotonic() < deadline:
        peer = None
        try:
            peer = Peer(port)
            return peer.control(0)
        except (OSError, serial.SerialException, TimeoutError) as error:
            last = error
        finally:
            if peer:
                peer.close()
        time.sleep(0.2)
    raise TimeoutError(f"no running image before deadline: {last}")


def activate(peer):
    print("Selected:", peer.control(1), flush=True)
    print("Reset accepted:", peer.control(3), flush=True)
    return time.monotonic()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", required=True)
    parser.add_argument("--images", type=Path, required=True)
    parser.add_argument(
        "--minimum-recovery-seconds",
        type=float,
        default=25,
        help="25 on C6; 65 on Nordic to include both full-slot swaps",
    )
    parser.add_argument("--deadline-seconds", type=float, default=120)
    args = parser.parse_args()
    initial = status(args.port, time.monotonic() + 30)
    if initial["release"] != 1:
        raise RuntimeError("start from provisioned A, release 1")
    for name in ["stall-entry", "stall-radio", "b"]:
        peer = Peer(args.port)
        try:
            owner, _, _ = peer.info()
            print(f"CASE {name}", flush=True)
            upload(peer, owner, args.images / f"{name}-watchdog.lvup")
            started = activate(peer)
        finally:
            peer.close()
        time.sleep(1)  # the reset reply precedes the physical reset
        result = status(args.port, started + args.deadline_seconds)
        elapsed = time.monotonic() - started
        expected = 2 if name == "b" else 1
        if result["release"] != expected or result["phase"] != "idle":
            raise RuntimeError((name, result))
        if name != "b" and elapsed < args.minimum_recovery_seconds:
            raise RuntimeError(
                f"{name} returned too early ({elapsed:.2f}s) to establish watchdog recovery"
            )
        print(
            f"PASS {name}: release {expected} returned in {elapsed:.2f}s; no host confirmation",
            flush=True,
        )
    peer = Peer(args.port)
    try:
        print(
            "Reset healthy trial without host confirmation:",
            peer.control(3),
            flush=True,
        )
    finally:
        peer.close()
    time.sleep(1)
    result = status(args.port, time.monotonic() + args.deadline_seconds)
    if result["release"] != 2:
        raise RuntimeError(("automatic confirmation did not persist", result))
    print("PASS automatically confirmed B retained after reset:", result, flush=True)


if __name__ == "__main__":
    main()
