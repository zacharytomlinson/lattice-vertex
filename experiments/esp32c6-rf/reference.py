#!/usr/bin/env python3
"""Compare the retained vendor 802.15.4 CLI at fixed channel 15 / 20 dBm.

--flash temporarily loads the vendor image and restores the current Lattice
image afterward. Without --flash, both boards must already run the vendor CLI.
--dry-run verifies artifacts and prints commands without opening either board.
"""

import argparse
from contextlib import ExitStack
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import shlex
import statistics
import subprocess
import time

import serial

ROOT = Path(__file__).resolve().parents[2]
REFERENCE = ROOT / "target/esp32c6-radio/reference"
KERNEL = ROOT / "src/port/esp32c6/tests/target/riscv32imac-unknown-none-elf/release/radio"
CHANNEL = 15
POWER_DBM = 20
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
RX = re.compile(r"Rx Done (\d+) bytes rssi=(-?\d+) lqi=(\d+)")
HEX = re.compile(r"i154cmd: ((?:[0-9a-fA-F]{2} ){7}[0-9a-fA-F]{2})")


def received_frame(text, expected):
    """Match the entire expected MAC payload, ignoring its replaced FCS bytes."""
    matches = list(RX.finditer(text))
    for index, match in enumerate(matches):
        end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
        decoded = bytes.fromhex(" ".join(HEX.findall(text[match.end():end])))
        if (int(match[1]) == len(expected) and len(decoded) >= len(expected)
                and decoded[:len(expected) - 2] == expected[:-2]):
            return {"rssi_dbm": int(match[2]), "lqi": int(match[3])}
    return None


def frame(source, token):
    # Same broadcast MAC layout/size as the Lattice probe: extended source,
    # broadcast PAN/destination, 2006 data frame, no requested MAC ACK.
    payload = b"LVD\x01\x00" + token.to_bytes(4, "little")
    return (b"\x41\xd8" + bytes([token & 255]) + b"\xff\xff\xff\xff"
            + source.to_bytes(8, "little") + payload + b"\0\0")


def stable_port(name):
    resolved = Path(name).resolve()
    for candidate in Path("/dev/serial/by-id").glob("*"):
        if candidate.resolve() == resolved:
            return str(candidate)
    return name


def flash_command(port, image):
    return ["espflash", "flash", "--skip-update-check", "--non-interactive",
            "--port", port, "--flash-size", "8mb", str(image)]


def verify_reference():
    for line in (REFERENCE / "SHA256SUMS").read_text().splitlines():
        expected, name = line.split()
        actual = hashlib.sha256((REFERENCE / name).read_bytes()).hexdigest()
        if actual != expected:
            raise RuntimeError(f"reference checksum mismatch: {name}")


def run_radio_test(ports, output, count):
    records = []
    with ExitStack() as stack:
        logs = [stack.enter_context((output / f"board-{name}.log").open("w")) for name in "AB"]
        devices = []
        for path in ports:
            deadline = time.monotonic() + 10
            while not Path(path).exists() and time.monotonic() < deadline:
                time.sleep(0.1)
            device = serial.Serial(baudrate=115200, timeout=0, write_timeout=2, exclusive=True)
            device.dtr = False
            device.rts = False
            device.port = path
            device.open()
            stack.callback(device.close)
            devices.append(device)

        def drain(seconds):
            texts = ["", ""]
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                for i, device in enumerate(devices):
                    data = device.read(device.in_waiting)
                    if data:
                        text = ANSI.sub("", data.decode(errors="replace"))
                        texts[i] += text
                        logs[i].write(text)
                        logs[i].flush()
                time.sleep(0.005)
            return texts

        def command(i, command, expected):
            logs[i].write(f"\n[HOST] {command}\n")
            devices[i].write((command + "\r").encode())
            text = drain(0.5)[i]
            if not re.search(expected, text):
                raise RuntimeError(f"board {'AB'[i]} command {command!r} failed: {text!r}")

        drain(2)
        for device in devices:
            device.write(b"\r")
        drain(0.5)
        for i in range(2):
            for cmd, expect in [
                ("rx -r 0", "radio exit receive mode"),
                ("promisc -d", "promiscuous mode is disabled"),
                (f"channel -s {CHANNEL}", rf"set channel: {CHANNEL}\b"),
                (f"txpower -s {POWER_DBM}", rf"set txpower: {POWER_DBM}\b"),
                ("panid 0xffff", "set panid: 0xffff"),
                ("rx -r 1", "RX Start"),
                ("channel -g", rf"current channel: {CHANNEL}\b"),
                ("txpower -g", rf"current txpower: {POWER_DBM}\b"),
            ]:
                command(i, cmd, expect)
        print("Both radios confirmed: channel 15, 20 dBm. No MAC ACKs or retries.", flush=True)

        for token in range(1, count + 1):
            for i in range(2):
                # Distinct locally administered test addresses; the receiver is
                # broadcast, so no short address or role assignment is needed.
                packet = frame(0x0200000000000001 + i, token)
                cmd = "tx -C " + " ".join(f"0x{b:02x}" for b in packet)
                logs[i].write(f"\n[HOST] {cmd}\n")
                devices[i].write((cmd + "\r").encode())
                texts = drain(0.5)
                reception = received_frame(texts[1 - i], packet)
                record = {"sender": "AB"[i], "receiver": "AB"[1 - i], "token": token,
                          "tx_done": "Tx Done" in texts[i], "received": reception is not None,
                          "failure": re.findall(r"Failure reason: (\d+)", texts[i]),
                          **(reception or {})}
                records.append(record)
                print(json.dumps(record), flush=True)

    directions = []
    for sender in "AB":
        selected = [r for r in records if r["sender"] == sender]
        rssi = [r["rssi_dbm"] for r in selected if r["received"]]
        directions.append({"sender": sender, "attempted": len(selected),
                           "tx_completed": sum(r["tx_done"] for r in selected),
                           "peer_received": len(rssi),
                           "rssi_dbm": {"min": min(rssi), "median": statistics.median(rssi),
                                        "max": max(rssi)} if rssi else None})
    summary = {"channel": CHANNEL, "tx_power_dbm": POWER_DBM, "mac_ack_requested": False,
               "ports": ports, "directions": directions}
    (output / "results.json").write_text(json.dumps({**summary, "records": records}, indent=2) + "\n")
    print(json.dumps(summary, indent=2), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("port_a")
    parser.add_argument("port_b")
    parser.add_argument("--flash", action="store_true", help="flash vendor, test, then restore Lattice")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--count", type=int, default=20)
    args = parser.parse_args()
    if not 1 <= args.count <= 255:
        parser.error("--count must be between 1 and 255")
    ports = [stable_port(args.port_a), stable_port(args.port_b)]
    if Path(ports[0]).resolve() == Path(ports[1]).resolve():
        parser.error("select two distinct serial ports")
    verify_reference()
    if args.flash and not KERNEL.is_file():
        parser.error(f"build the Lattice radio image first: {KERNEL}")

    output = ROOT / "target/radio-reference" / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    output.mkdir(parents=True)
    # Preserve the SDK images verbatim. Regenerating through espflash flash
    # rewrites this bootloader's frequency header (0x30 to 0x3f with 80mhz),
    # which prevents it from booting on these C6s. Offsets are from the retained
    # reference partition table; writing separately preserves NVS/calibration.
    commands = []
    for port in ports:
        commands.append([
            ["espflash", "write-bin", "--skip-update-check", "--non-interactive",
             "--port", port, "--after", reset, offset, str(REFERENCE / name)]
            for offset, name, reset in [
                ("0x0", "bootloader.bin", "no-reset"),
                ("0x8000", "partition-table.bin", "no-reset"),
                ("0x10000", "ieee802154_cli.bin", "hard-reset"),
            ]
        ])
    restore = [flash_command(p, KERNEL) for p in ports]
    plan = {"reference": json.loads((REFERENCE / "provenance.json").read_text()),
            "test_power_dbm": POWER_DBM, "test_channel": CHANNEL, "ports": ports,
            "kernel_sha256": hashlib.sha256(KERNEL.read_bytes()).hexdigest() if KERNEL.is_file() else None}
    (output / "run.json").write_text(json.dumps(plan, indent=2) + "\n")
    print(f"Output: {output}", flush=True)
    if args.dry_run:
        for cmd in ([cmd for board in commands for cmd in board] + restore) if args.flash else []:
            print(shlex.join(cmd))
        print(f"Would send {args.count} frames per direction on channel 15 at 20 dBm.")
        return

    touched = []
    restore_failed = False
    try:
        if args.flash:
            for i, board in enumerate(commands):
                touched.append(i)
                for cmd in board:
                    subprocess.run(cmd, check=True)
        run_radio_test(ports, output, args.count)
    finally:
        # Serial handles close before reflashing. Attempt restoration of each
        # board even if the reference run or the other restoration failed.
        for i in touched:
            print(f"Restoring Lattice on {ports[i]}", flush=True)
            try:
                result = subprocess.run(restore[i], check=False)
                failed = result.returncode != 0
            except OSError as error:
                print(error, flush=True)
                failed = True
            if failed:
                restore_failed = True
                print(f"RESTORE FAILED: {shlex.join(restore[i])}", flush=True)
    return 1 if restore_failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
