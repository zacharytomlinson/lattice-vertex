#!/usr/bin/env python3
"""Flash the C6 Wi-Fi comparison, capture USB serial, then restore Lattice.

This runner never accesses or configures the host's wireless adapter.
"""
import argparse
from contextlib import ExitStack
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import statistics
import subprocess
import sys
import time

import serial

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from reference import KERNEL, ROOT, flash_command, stable_port

BUILD = Path(__file__).resolve().parent / 'target'
IMAGES = [BUILD / 'wifi-receiver.elf', BUILD / 'wifi-ap.elf']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('port_a')
    parser.add_argument('port_b')
    parser.add_argument('--seconds', type=int, default=40)
    parser.add_argument('--reverse', action='store_true', help='A transmits and B receives')
    args = parser.parse_args()
    if not 5 <= args.seconds <= 120:
        parser.error('--seconds must be between 5 and 120')
    ports = [stable_port(args.port_a), stable_port(args.port_b)]
    images = list(reversed(IMAGES)) if args.reverse else IMAGES
    if Path(ports[0]).resolve() == Path(ports[1]).resolve():
        parser.error('select two distinct serial ports')
    for path in [*images, KERNEL, *map(Path, ports)]:
        if not path.exists():
            parser.error(f'missing: {path}')
    output = ROOT / 'target/wifi-comparison' / datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S.%fZ')
    output.mkdir(parents=True)
    provenance = {'ports': ports, 'seconds': args.seconds, 'host_wireless_used': False,
                  'board_images': list(map(str, images)),
                  'images': {str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                             for path in [*images, KERNEL]}}
    (output / 'run.json').write_text(json.dumps(provenance, indent=2) + '\n')
    print(f'Output: {output}', flush=True)
    touched = []
    restore_failed = False
    try:
        for i, port in enumerate(ports):
            touched.append(i)
            subprocess.run(flash_command(port, images[i]), check=True)
        texts = ['', '']
        with ExitStack() as stack:
            logs = [stack.enter_context((output / f'board-{name}.log').open('w')) for name in 'AB']
            devices = []
            for port in ports:
                device = serial.Serial(baudrate=115200, timeout=0, write_timeout=2, exclusive=True)
                device.dtr = False
                device.rts = False
                device.port = port
                device.open()
                stack.callback(device.close)
                devices.append(device)
            deadline = time.monotonic() + args.seconds
            while time.monotonic() < deadline:
                for i, device in enumerate(devices):
                    text = device.read(device.in_waiting).decode(errors='replace')
                    if text:
                        texts[i] += text
                        logs[i].write(text)
                        logs[i].flush()
                time.sleep(.01)
        boards = []
        for name, text in zip('AB', texts):
            samples = [int(x) for x in re.findall(r'WIFI_RX .*rssi_dbm=(-?\d+)', text)]
            boards.append({'board': name,
                'ready': next((line for line in text.splitlines() if line.startswith('WIFI_READY')), None),
                'survey': next((line for line in text.splitlines() if line.startswith('WIFI_SURVEY')), None),
                'peer_scan_samples': len(samples),
                'failures': [line for line in text.splitlines() if 'FAIL ' in line],
                'rssi_dbm': {'min': min(samples), 'median': statistics.median(samples),
                             'max': max(samples)} if samples else None})
        result = {'ports': ports, 'boards': boards}
        (output / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result, indent=2), flush=True)
    finally:
        for i in touched:
            print(f'Restoring Lattice: {ports[i]}', flush=True)
            try:
                result = subprocess.run(flash_command(ports[i], KERNEL), check=False)
                failed = result.returncode != 0
            except OSError as error:
                print(error, flush=True)
                failed = True
            if failed:
                restore_failed = True
                print(f'RESTORE FAILED: {ports[i]}', flush=True)
    return 1 if restore_failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
