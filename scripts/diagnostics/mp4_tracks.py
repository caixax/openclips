"""Prints the timing of every track of an MP4: handler, media duration,
sample count and edit list. A track whose edit list starts with a long
empty edit (media_time -1) plays late or not at all, which a plain probe
(duration, "has audio") does not show.

    python scripts/diagnostics/mp4_tracks.py <file.mp4> [more.mp4 ...]
"""

import struct
import sys

CONTAINERS = ("moov", "trak", "mdia", "minf", "stbl", "edts")


def boxes(data, off, end, path=""):
    while off + 8 <= end:
        size, kind = struct.unpack(">I4s", data[off:off + 8])
        header = 8
        if size == 1:
            size = struct.unpack(">Q", data[off + 8:off + 16])[0]
            header = 16
        if size == 0:
            size = end - off
        if size < header:
            return
        name = kind.decode("latin1")
        yield path + "/" + name, off + header, off + size
        if name in CONTAINERS:
            yield from boxes(data, off + header, off + size, path + "/" + name)
        off += size


def report(path):
    data = open(path, "rb").read()
    print(f"== {path} ({len(data)} bytes)")
    movie_scale = 1
    for name, start, _end in boxes(data, 0, len(data)):
        kind = name.rsplit("/", 1)[-1]
        if kind == "mvhd":
            movie_scale, duration = struct.unpack(">II", data[start + 12:start + 20])
            print(f"  movie: {duration / movie_scale:.3f} s")
        elif kind == "elst":
            count = struct.unpack(">I", data[start + 4:start + 8])[0]
            for i in range(count):
                at = start + 8 + i * 12
                length, media_time = struct.unpack(">Ii", data[at:at + 8])
                what = "empty" if media_time == -1 else f"media from {media_time}"
                print(f"    edit: {length / movie_scale:.3f} s, {what}")
        elif kind == "mdhd":
            scale, duration = struct.unpack(">II", data[start + 12:start + 20])
            print(f"    media: {duration / scale:.3f} s")
        elif kind == "hdlr":
            print(f"  track {data[start + 8:start + 12].decode('latin1')}")
        elif kind == "stsz":
            print(f"    samples: {struct.unpack('>I', data[start + 8:start + 12])[0]}")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    for arg in sys.argv[1:]:
        report(arg)
