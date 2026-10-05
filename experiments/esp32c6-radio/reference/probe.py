"""Drive the vendor IEEE 802.15.4 CLI; verify peer payloads and hardware ACKs."""
import argparse
import json
import re
import time
from contextlib import ExitStack
from pathlib import Path
import serial

p = argparse.ArgumentParser()
p.add_argument('port_a')
p.add_argument('port_b')
p.add_argument('--output', required=True)
p.add_argument('--count', type=int, default=20)
p.add_argument('--channel', type=int, default=15)
p.add_argument('--power', type=int, default=0)
a = p.parse_args()
ansi = re.compile(r'\x1b\[[0-?]*[ -/]*[@-~]')
records = []
with ExitStack() as stack:
    log = stack.enter_context(open(a.output + '.log', 'w'))
    ports = []
    for path in (a.port_a, a.port_b):
        s = serial.Serial(baudrate=115200, timeout=0, exclusive=True)
        s.dtr = False
        s.rts = False
        s.port = path
        s.open()
        stack.callback(s.close)
        ports.append(s)

    def drain(seconds):
        texts = ['', '']
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            for i, s in enumerate(ports):
                data = s.read(s.in_waiting)
                if data:
                    text = ansi.sub('', data.decode(errors='replace'))
                    texts[i] += text
                    log.write(f'[{"AB"[i]}] ' + text)
                    log.flush()
            time.sleep(0.005)
        return texts

    def command(i, cmd, expect):
        print(f'{"AB"[i]} > {cmd}', flush=True)
        log.write(f'\n[HOST->{"AB"[i]}] {cmd}\n')
        ports[i].write((cmd + '\r').encode())
        texts = drain(0.35)
        if expect not in texts[i]:
            raise RuntimeError(f'{cmd!r} did not report {expect!r}: {texts[i]!r}')

    drain(2)
    for i in range(2):
        ports[i].write(b'\r')
    drain(1)
    for i in range(2):
        for cmd, expect in [
            ('rx -r 0', 'radio exit receive mode'),
            ('promisc -d', 'promiscuous mode is disabled'),
            (f'channel -s {a.channel}', f'set channel: {a.channel}'),
            (f'txpower -s {a.power}', f'set txpower: {a.power}'),
            ('panid 0x4c56', 'set panid: 0x4c56'),
            (f'shortaddr {i+1}', 'set shortaddr:'),
            ('rx -r 1', 'RX Start'),
            ('channel -g', f'current channel: {a.channel}'),
            ('txpower -g', f'current txpower: {a.power}'),
        ]:
            command(i, cmd, expect)

    for sequence in range(a.count):
        mode = sequence % 2
        for i in range(2):
            source = i + 1
            destination = 0xffff if mode else 2 - i
            fcf = 0x8841 if mode else 0x8861
            payload = b'LVP\x01' + bytes([0, mode]) + sequence.to_bytes(2, 'little') + b'IDFprobe'
            frame = fcf.to_bytes(2, 'little') + bytes([sequence]) + b'\x56\x4c' + destination.to_bytes(2, 'little') + source.to_bytes(2, 'little') + payload + b'\0\0'
            cmd = 'tx -C ' + ' '.join(f'0x{b:02x}' for b in frame)
            log.write(f'\n[HOST->{"AB"[i]}] {cmd}\n')
            ports[i].write((cmd + '\r').encode())
            texts = drain(0.4)
            rx = re.search(r'Rx Done (\d+) bytes rssi=(-?\d+) lqi=(\d+)(.*)', texts[1-i], re.S)
            received = False
            if rx:
                chunks = re.findall(r'i154cmd: ((?:[0-9a-fA-F]{2} ){7}[0-9a-fA-F]{2})', rx[4])
                decoded = bytes.fromhex(' '.join(chunks))
                received = int(rx[1]) == len(frame) and decoded[:len(frame)-2] == frame[:-2]
            record = dict(node=source, mode='broadcast' if mode else 'unicast', sequence=sequence,
                          tx_done='Tx Done' in texts[i], ack='Rx ack' in texts[i], received=received,
                          rssi=int(rx[2]) if rx else None, lqi=int(rx[3]) if rx else None,
                          failure=re.findall(r'Failure reason: (\d+)',texts[i]))
            records.append(record)
            print(json.dumps(record), flush=True)
    Path(a.output + '.json').write_text(json.dumps(dict(channel=a.channel,power=a.power,records=records),indent=2)+'\n')
