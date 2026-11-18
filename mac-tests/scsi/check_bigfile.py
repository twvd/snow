#!/usr/bin/python3
# Checks SCSIExer.big, from an hcopy -r of its data fork or a raw copy, on the host
import sys
d = open(sys.argv[1], 'rb').read()
chunks = [512, 3072, 65536, 7168, 131072, 1024, 32768]
bufsz = int(sys.argv[2]) if len(sys.argv) > 2 else 524288
def fill(n, seed):
    x = (seed * 2654435761 + 0x9E3779B9) & 0xFFFFFFFF
    o = bytearray(n)
    for i in range(n):
        x ^= (x << 13) & 0xFFFFFFFF; x ^= x >> 17; x ^= (x << 5) & 0xFFFFFFFF
        o[i] = x & 0xFF
    return bytes(o)
pos = c = bad = 0
while pos < len(d):
    n = min(chunks[c], bufsz, len(d) - pos)
    if d[pos:pos + n] != fill(n, pos + 77):
        bad += 1; print('BAD chunk at', pos, n)
    pos += n; c = (c + 1) % 7
print('big file', len(d), 'bytes, bad chunks:', bad)
