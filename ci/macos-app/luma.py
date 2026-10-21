#!/usr/bin/env python3
"""How light a picture of a window is, at its top and below it.

  luma.py <window.png>

ci/test-app.sh looks measures each picture it takes with this, to say how a
window looks in light and in dark appearance; it gates nothing. Prints the
mean luminance, 0 (black) to 255 (white), of the picture's top band, where
the toolbar is (its top 6%), and of a band as tall just below it, where the
page or the window's content begins. Reads only 8-bit, non-interlaced PNG,
which is what screencapture writes; anything else is said, not guessed.
"""
import struct
import sys
import zlib

BAND = 0.06


def rows(path):
    with open(path, "rb") as f:
        data = f.read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    pos, idat, header = 8, [], None
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat.append(body)
        pos += 12 + length
    width, height, depth, colour, _, _, interlace = header
    channels = {0: 1, 2: 3, 4: 2, 6: 4}.get(colour)
    if depth != 8 or interlace or channels is None:
        raise ValueError(f"a PNG of depth {depth}, colour type {colour}, interlace {interlace}")
    raw = zlib.decompress(b"".join(idat))
    stride = width * channels
    wanted = min(height, 2 * max(1, int(height * BAND)))
    previous = bytearray(stride)
    out = []
    for y in range(wanted):
        start = y * (stride + 1)
        kind, line = raw[start], bytearray(raw[start + 1:start + 1 + stride])
        for i in range(stride):
            left = line[i - channels] if i >= channels else 0
            up = previous[i]
            if kind == 1:
                line[i] = (line[i] + left) & 0xFF
            elif kind == 2:
                line[i] = (line[i] + up) & 0xFF
            elif kind == 3:
                line[i] = (line[i] + ((left + up) >> 1)) & 0xFF
            elif kind == 4:
                corner = previous[i - channels] if i >= channels else 0
                p = left + up - corner
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - corner)
                line[i] = (line[i] + (left if pa <= pb and pa <= pc else up if pb <= pc else corner)) & 0xFF
        out.append(line)
        previous = line
    return width, height, channels, out


def mean(lines, width, channels):
    total = count = 0
    for line in lines:
        for x in range(0, width, 4):
            px = line[x * channels:(x + 1) * channels]
            if channels >= 3:
                total += 0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2]
            else:
                total += px[0]
            count += 1
    return round(total / count) if count else -1


def main(argv):
    if len(argv) != 1:
        sys.exit("usage: luma.py <window.png>")
    try:
        width, height, channels, lines = rows(argv[0])
    except (OSError, ValueError, zlib.error, struct.error, TypeError) as err:
        print(f"unreadable: {err}")
        return 1
    half = len(lines) // 2
    print(f"{width}x{height}: top {mean(lines[:half], width, channels)}, "
          f"below it {mean(lines[half:], width, channels)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
