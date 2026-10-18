# SPDX-License-Identifier: MIT
"""Host-side framing for maintained MCU image-staging fixtures, not OTA."""
import struct
import time
from binascii import crc_hqx

CHUNK_SIZE = 48

class Console:
    def __init__(self, port):
        import serial
        self.serial = serial.Serial(baudrate=115200, timeout=0.2, write_timeout=2)
        self.serial.rts = False  # Do not assert the ESP reset/download control line.
        self.serial.dtr = True
        self.serial.port = port
        self.serial.open()
        self.sequence = time.monotonic_ns() & 0xffffffff

    def command(self, opcode, payload=b"", timeout=10):
        self.sequence = (self.sequence + 1) & 0xffffffff
        body = struct.pack("<IB", self.sequence, opcode) + payload
        if not 5 <= len(body) <= 59:
            raise ValueError("oversized fixture command")
        checked = bytes([len(body)]) + body
        self.serial.write(b'\xa5\x5a' + checked + struct.pack('<H', crc_hqx(checked, 0xffff)))
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            line = self.serial.readline().decode("ascii", errors="replace").strip()
            if f" seq={self.sequence} op={opcode} " in line:
                if "result=Ok(())" not in line:
                    raise RuntimeError(line)
                return line
        raise TimeoutError(f"no result for command {opcode}, sequence {self.sequence}")

