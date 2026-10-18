#!/usr/bin/env python3
"""Serve a signed package through an attached Lattice peer. No privileged node role.

Requires pyserial. Upload uses ordinary kernel host/radio forwarding. Activation
and confirmation are explicit local controls on the receiver's attached port.
"""

import argparse
import hashlib
import secrets
import struct
import time
from pathlib import Path
import serial

VERSION = 4
CHUNK = 48
PHASES = (
    "idle",
    "header",
    "preparing",
    "receiving",
    "ready",
    "fault",
    "selected",
    "verifying",
)
STATUSES = (
    "ok",
    "unsupported",
    "invalid",
    "signature",
    "target",
    "version",
    "busy",
    "order",
    "storage",
    "verification",
)


class HostFailure(RuntimeError):
    def __init__(self, code):
        self.code = code
        super().__init__(f"host failure {code}")


def cobs_encode(body):
    out = bytearray([0])
    start = 0
    code = 1
    for b in body:
        if b == 0:
            out[start] = code
            start = len(out)
            out.append(0)
            code = 1
        else:
            out.append(b)
            code += 1
    out[start] = code
    return b"\0" + out + b"\0"


def cobs_decode(encoded):
    out = bytearray()
    i = 0
    while i < len(encoded):
        code = encoded[i]
        i += 1
        if code == 0 or i + code - 1 > len(encoded):
            raise ValueError("invalid COBS")
        out.extend(encoded[i : i + code - 1])
        i += code - 1
        if i < len(encoded):
            out.append(0)
    return bytes(out)


class Peer:
    def __init__(self, port):
        self.serial = serial.Serial(baudrate=115200, timeout=0.1, write_timeout=2)
        self.serial.rts = False
        self.serial.dtr = True
        self.serial.port = port
        self.serial.open()
        self.token = secrets.randbits(32)

    def close(self):
        self.serial.close()

    def exchange(self, opcode, payload=b"", timeout=12):
        self.token = (self.token + 1) & 0xFFFFFFFF
        self.serial.write(
            cobs_encode(struct.pack("<BBI", VERSION, opcode, self.token) + payload)
        )
        end = time.monotonic() + timeout
        discard = False
        while time.monotonic() < end:
            encoded = self.serial.read_until(b"\0", 129)
            if not encoded:
                continue
            if encoded[-1] != 0:
                discard = True
                continue
            if discard:
                discard = False
                continue
            if len(encoded) == 1:
                continue
            try:
                body = cobs_decode(encoded[:-1])
            except ValueError:
                continue
            if (
                len(body) < 6
                or body[0] != VERSION
                or struct.unpack_from("<I", body, 2)[0] != self.token
            ):
                continue
            if body[1] == 255:
                raise HostFailure(body[6])
            return body[1], body[6:]
        raise TimeoutError("peer response timed out")

    def info(self):
        op, data = self.exchange(0)
        if op != 0x80:
            raise ValueError("unexpected identity reply")
        address, founder, channel = struct.unpack("<QQB", data)
        return address, founder, channel

    def control(self, action):
        op, data = self.exchange(6, bytes([action]))
        if op != 0x86:
            raise ValueError("unexpected local control reply")
        progress = parse_progress(data[:28])
        progress["release"] = struct.unpack_from("<Q", data, 28)[0]
        if progress["status"] != "ok":
            raise RuntimeError(progress)
        return progress

    def update(self, owner, request):
        # Only immutable header/range transfers and bounded preparation are retried.
        # Native calls and local activation controls never enter this retry path.
        failures = 0
        busy_deadline = time.monotonic() + 30
        while True:
            try:
                op, data = self.exchange(5, struct.pack("<Q", owner) + request)
            except (TimeoutError, HostFailure) as error:
                if isinstance(error, HostFailure) and error.code not in (1, 3, 4):
                    raise
                failures += 1
                if failures == 4 or time.monotonic() >= busy_deadline:
                    raise
                time.sleep(0.1)
                continue
            if op != 0x85 or struct.unpack_from("<Q", data)[0] != owner:
                raise ValueError("unexpected update reply")
            progress = parse_progress(data[8:36])
            progress["path_cost_raw"] = struct.unpack_from("<I", data, 36)[0]
            if progress["package"] != request[1:17].hex():
                raise ValueError("package identity mismatch")
            if progress["status"] == "busy":
                if time.monotonic() >= busy_deadline:
                    raise TimeoutError(
                        f"receiver deferred staging for 30 seconds: {progress}"
                    )
                time.sleep(0.01)
                continue
            if progress["status"] != "ok":
                raise RuntimeError(progress)
            return progress


def parse_progress(data):
    if len(data) != 28:
        raise ValueError("invalid progress length")
    package, phase, status, accepted, committed, prepared = struct.unpack(
        "<16sBBIIH", data
    )
    return dict(
        package=package.hex(),
        phase=PHASES[phase],
        status=STATUSES[status],
        accepted=accepted,
        committed=committed,
        prepared=prepared,
    )


def upload(peer, owner, path, stop_after=None):
    content = path.read_bytes()
    header = content[:192]
    image = content[192:]
    if len(header) != 192 or header[:6] != b"LVUP\x01\0":
        raise ValueError("invalid package header")
    if (
        struct.unpack_from("<I", header, 68)[0] != len(image)
        or header[72:104] != hashlib.sha256(image).digest()
    ):
        raise ValueError("invalid package image digest/length")
    package = hashlib.sha256(header).digest()[:16]
    for offset in range(0, 192, CHUNK):
        data = header[offset : offset + CHUNK]
        p = peer.update(owner, b"\0" + package + bytes([offset, len(data)]) + data)
    while p["phase"] == "preparing":
        p = peer.update(owner, b"\x01" + package)
    if p["phase"] == "ready":
        print(p)
        return p
    if p["phase"] not in ("receiving", "verifying"):
        raise RuntimeError(p)
    offset = p["accepted"]
    print(
        f"Resuming at {offset} of {len(image)} bytes; durable prefix {p['committed']}",
        flush=True,
    )
    started = time.monotonic()
    last = offset
    while offset < len(image):
        data = image[offset : offset + CHUNK]
        p = peer.update(
            owner,
            b"\x02" + package + struct.pack("<I", offset) + bytes([len(data)]) + data,
        )
        if p["accepted"] < offset + len(data):
            raise RuntimeError("receiver did not accept the range")
        offset = p["accepted"]
        if offset - last >= 16384 or offset == len(image):
            print(f"{offset}/{len(image)} bytes; durable {p['committed']}", flush=True)
            last = offset
        if stop_after is not None and offset >= stop_after:
            print("Stopped without finishing or activating:", p, flush=True)
            return p
    verification_steps = 0
    while True:
        p = peer.update(owner, b"\x03" + package)
        verification_steps += 1
        if p["phase"] == "ready":
            break
        if p["phase"] != "verifying":
            raise RuntimeError(p)
    print(f"Verification steps: {verification_steps}", flush=True)
    print(
        f"Verified in {time.monotonic() - started:.2f}s; activation is decided by the receiver.",
        flush=True,
    )
    print(p, flush=True)
    return p


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", required=True)
    sub = parser.add_subparsers(dest="action", required=True)
    sub.add_parser("info")
    sub.add_parser("activate")
    sub.add_parser("confirm")
    sub.add_parser("reset")
    p = sub.add_parser("upload")
    p.add_argument("--owner", type=lambda s: int(s, 16), required=True)
    p.add_argument("package", type=Path)
    p.add_argument(
        "--stop-after",
        type=int,
        help="stop at an accepted prefix, leaving the image unselected",
    )
    args = parser.parse_args()
    peer = Peer(args.port)
    try:
        if args.action == "info":
            print("Identity:", tuple(hex(x) for x in peer.info()))
            print("Update:", peer.control(0))
        elif args.action == "upload":
            upload(peer, args.owner, args.package, args.stop_after)
        elif args.action == "activate":
            print(peer.control(1))
            print(peer.control(3))
            time.sleep(0.25)
        else:
            print(peer.control(2 if args.action == "confirm" else 3))
            time.sleep(0.25)
    finally:
        peer.close()


if __name__ == "__main__":
    main()
