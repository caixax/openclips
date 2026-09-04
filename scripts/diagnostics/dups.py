# Scores a recording for repeated frames: decodes it to 320x180 grey with
# GStreamer, then counts frames identical to the previous one that sit inside
# motion (a repeat in a static scene is not a lost frame). Needs
# gst-launch-1.0 on PATH.
#
#   python scripts/diagnostics/dups.py "D:/Clips/some clip.mp4"
import collections
import os
import subprocess
import sys

f = sys.argv[1].replace("\\", "/")
raw = os.path.join(os.environ.get("TEMP", "."), os.path.basename(f) + ".gray")
subprocess.run(
    [
        "gst-launch-1.0.exe", "-q", "filesrc", "location=" + f, "!", "qtdemux", "!",
        "h264parse", "!", "avdec_h264", "!", "videoscale", "!",
        "video/x-raw,width=320,height=180", "!", "videoconvert", "!",
        "video/x-raw,format=GRAY8", "!", "filesink", "location=" + raw.replace("\\", "/"),
    ],
    check=False,
)
b = open(raw, "rb").read()
n = 320 * 180
frames = len(b) // n
changed = [True]
prev = b[0:n]
for i in range(1, frames):
    fr = b[i * n:(i + 1) * n]
    changed.append(sum(1 for x, y in zip(fr[::2], prev[::2]) if abs(x - y) > 12) >= 2)
    prev = fr
missed = [
    i for i in range(1, frames)
    if not changed[i]
    and any(changed[j] for j in range(max(1, i - 3), i))
    and any(changed[j] for j in range(i + 1, min(frames, i + 4)))
]
motion = sum(1 for c in changed[1:] if c)
buckets = collections.Counter(i // 600 for i in missed)
print(
    os.path.basename(f), "frames", frames,
    "motion", f"{motion / max(frames - 1, 1) * 100:.0f}%",
    "missed", len(missed), f"({len(missed) / max(motion, 1) * 100:.1f}%)",
    "per 10 s:", [buckets.get(k, 0) for k in range((frames + 599) // 600)],
)
os.remove(raw)
