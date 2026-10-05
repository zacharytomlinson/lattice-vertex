#!/usr/bin/env python3
"""Capture both UART logs without forwarding any data between the boards."""

import argparse
from contextlib import ExitStack
import time

import serial


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("port_a")
    parser.add_argument("port_b")
    parser.add_argument("--seconds", type=float, default=30)
    parser.add_argument("--output", help="Also save the tagged serial log to this file")
    parser.add_argument("--reset", action="store_true", help="Reset both into their flashed firmware")
    args = parser.parse_args()
    with ExitStack() as stack:
        log = stack.enter_context(open(args.output, "w")) if args.output else None
        ports = []
        for path in (args.port_a, args.port_b):
            port = serial.Serial(baudrate=115200, timeout=0, exclusive=True)
            port.dtr = False
            port.rts = False
            port.port = path
            port.open()
            stack.callback(port.close)
            ports.append(port)
        if args.reset:
            for port in ports:
                port.rts = True
            time.sleep(0.1)
            for port in ports:
                port.rts = False
        buffers = [bytearray(), bytearray()]
        end = time.monotonic() + args.seconds
        while time.monotonic() < end:
            for index, port in enumerate(ports):
                buffers[index].extend(port.read(port.in_waiting))
                while b"\n" in buffers[index]:
                    line, _, rest = buffers[index].partition(b"\n")
                    buffers[index] = bytearray(rest)
                    tagged = f"[{'AB'[index]}] {line.decode(errors='replace').rstrip()}"
                    print(tagged, flush=True)
                    if log:
                        print(tagged, file=log, flush=True)
            time.sleep(0.005)


if __name__ == "__main__":
    main()
