#!/usr/bin/env python3
"""Build the C6 rollback bootloader and A/B fixtures, or drive local USB staging.

Preparation never flashes hardware. Use an activated ESP-IDF 5.5.4 environment
with the C6 toolchain. Firmware runs Lattice; IDF builds only the second-stage loader.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[1] / "tests"))
from update_console import Console


def check_image(data):
    if not 24 <= len(data) <= 0x100000 or data[0] != 0xe9 or not 1 <= data[1] <= 16:
        raise ValueError("image does not fit the C6 OTA slot")
    if struct.unpack_from('<H', data, 12)[0] != 13:
        raise ValueError("not an ESP32-C6 image")


def prepare(args):
    idf = Path(args.idf or os.environ['IDF_PATH']).resolve()
    version = subprocess.check_output(['git', '-C', str(idf), 'describe', '--tags', '--exact-match'], text=True).strip()
    if version != 'v5.5.4':
        raise ValueError(f"expected ESP-IDF v5.5.4, found {version}")
    build = HERE / 'target/idf-boot'
    subprocess.run([args.idf_python, str(idf/'tools/idf.py'), '-C', str(HERE/'bootloader'), '-B', str(build),
                    '-D', f'SDKCONFIG={build}/sdkconfig', '-D', 'IDF_TARGET=esp32c6',
                    'bootloader', 'partition-table'], check=True)
    config = (build/'sdkconfig').read_text().splitlines()
    if 'CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y' not in config:
        raise ValueError('bootloader rollback is disabled')
    for option in ['CONFIG_SECURE_BOOT=y', 'CONFIG_SECURE_FLASH_ENC_ENABLED=y', 'CONFIG_BOOTLOADER_APP_ANTI_ROLLBACK=y']:
        if option in config:
            raise ValueError(f'unsupported provisioning option: {option}')
    out = HERE/'target/update'
    out.mkdir(parents=True, exist_ok=True)
    loader = (build/'bootloader/bootloader.bin').read_bytes()
    if len(loader) > 0x8000 or loader[0] != 0xe9:
        raise ValueError('bootloader overlaps the partition table')
    (out/'bootloader.bin').write_bytes(loader)
    shutil.copyfile(build/'partition_table/partition-table.bin', out/'partition-table.bin')
    (out/'otadata-empty.bin').write_bytes(b'\xff' * 0x2000)
    for image, features in [('a', 'update-test'), ('b', 'update-image-b')]:
        subprocess.run(['cargo', '+nightly', 'build', '-Z', 'build-std=core', '--release', '--locked',
                        '--bin', 'update', '--features', features], cwd=HERE, check=True)
        elf = HERE/'target/riscv32imac-unknown-none-elf/release/update'
        shutil.copyfile(elf, out/f'{image}.elf')
        subprocess.run(['espflash', 'save-image', '--chip', 'esp32c6', '--flash-size', '8mb',
                        '--flash-mode', 'dio', '--flash-freq', '40mhz', '--skip-update-check',
                        str(elf), str(out/f'{image}.bin')], check=True)
        check_image((out/f'{image}.bin').read_bytes())
    files = {name: hashlib.sha256((out/name).read_bytes()).hexdigest()
             for name in ['bootloader.bin', 'partition-table.bin', 'otadata-empty.bin', 'a.bin', 'b.bin']}
    (out/'provenance.json').write_text(json.dumps({
        'idf_version': version,
        'idf_commit': subprocess.check_output(['git', '-C', str(idf), 'rev-parse', 'HEAD'], text=True).strip(),
        'sdkconfig_sha256': hashlib.sha256((build/'sdkconfig').read_bytes()).hexdigest(),
        'files': files,
    }, indent=2) + '\n')
    print(f'Prepared {out}; no device accessed.')


def drive(args):
    console = Console(args.port)
    try:
        if args.action == 'stage':
            data = args.image.read_bytes()
            check_image(data)
            print(console.command(1, struct.pack('<I', len(data))))
            while 'progress=1 ' not in console.command(2):
                pass
            for offset in range(0, len(data), 52):
                console.command(3, struct.pack('<I', offset) + data[offset:offset+52])
            print(console.command(4, hashlib.sha256(data).digest()))
            print('Verified and selected for trial; reset separately, then confirm only after checking startup.')
        else:
            print(console.command({'status': 0, 'reset': 5, 'confirm': 6}[args.action]))
            if args.action == 'reset': time.sleep(0.25)
    finally:
        console.serial.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    build = sub.add_parser('prepare')
    build.add_argument('--idf')
    build.add_argument('--idf-python', default=sys.executable)
    for action in ['status', 'stage', 'reset', 'confirm']:
        command = sub.add_parser(action)
        command.add_argument('--port', required=True)
        if action == 'stage': command.add_argument('image', type=Path)
    args = parser.parse_args()
    prepare(args) if args.action == 'prepare' else drive(args)


if __name__ == '__main__':
    main()
