#!/usr/bin/env python3
"""Screenshot through the shipped MCP tool and sample its pixels.

Usage: pixel-probe.py <daiorama> <scope> <session_id> <out.png>
Prints: DIM=<w>x<h> then SAMPLES at fixed proportions, then SAMPLE_OK/FAIL.
The PNG is decoded with stdlib zlib only (the L1 image has no Pillow).
"""
import base64
import json
import os
import struct
import subprocess
import sys
import zlib


def call(binary, scope, sid):
    env = dict(os.environ)
    env["DAIORAMA_SCOPE"] = scope
    proc = subprocess.Popen(
        [binary, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True, env=env,
    )
    def send(frame):
        proc.stdin.write(json.dumps(frame) + "\n")
        proc.stdin.flush()
    def read():
        line = proc.stdout.readline()
        if not line:
            raise RuntimeError(proc.stderr.read())
        return json.loads(line)
    try:
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
              "params": {"protocolVersion": "2025-06-18"}})
        read()
        send({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
              "params": {"name": "screenshot", "arguments": {"session_id": sid}}})
        reply = read()
        if reply.get("error") or (reply.get("result") or {}).get("isError"):
            raise RuntimeError(json.dumps(reply)[:800])
        result = reply["result"]
        image = result["content"][0]
        return base64.b64decode(image["data"])
    finally:
        try:
            proc.stdin.close()
        except Exception:
            pass
        proc.terminate()


def decode_png(data):
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    pos = 8
    width = height = color = depth = None
    chunks = []
    while pos < len(data):
        length = struct.unpack(">I", data[pos:pos + 4])[0]
        kind = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, depth, color = struct.unpack(">IIBB", body[:10])
        elif kind == b"IDAT":
            chunks.append(body)
        elif kind == b"IEND":
            break
    if depth != 8 or color not in (2, 6):
        raise ValueError(f"unsupported PNG depth={depth} color={color}")
    channels = 4 if color == 6 else 3
    raw = zlib.decompress(b"".join(chunks))
    stride = width * channels
    rows = []
    prior = bytearray(stride)
    off = 0
    for _ in range(height):
        filter_type = raw[off]
        off += 1
        line = bytearray(raw[off:off + stride])
        off += stride
        if filter_type == 1:
            for i in range(channels, stride):
                line[i] = (line[i] + line[i - channels]) & 0xFF
        elif filter_type == 2:
            for i in range(stride):
                line[i] = (line[i] + prior[i]) & 0xFF
        elif filter_type == 3:
            for i in range(stride):
                left = line[i - channels] if i >= channels else 0
                line[i] = (line[i] + ((left + prior[i]) >> 1)) & 0xFF
        elif filter_type == 4:
            for i in range(stride):
                a = line[i - channels] if i >= channels else 0
                b = prior[i]
                c = prior[i - channels] if i >= channels else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 0xFF
        elif filter_type != 0:
            raise ValueError(f"bad filter {filter_type}")
        rows.append(line)
        prior = line
    return width, height, channels, rows


def main():
    binary, scope, sid, out = sys.argv[1:5]
    data = call(binary, scope, sid)
    open(out, "wb").write(data)
    width, height, channels, rows = decode_png(data)
    print(f"DIM={width}x{height}")
    # Corners and lower-centre: the foot background fills the screen; text sits at the
    # top-left, so these points are background unless the client is not fullscreen.
    points = [
        (width // 10, height // 10),
        (width - width // 10, height // 10),
        (width // 2, height - height // 10),
        (width - width // 10, height - height // 10),
    ]
    ok = True
    for x, y in points:
        rgba = rows[y][x * channels:x * channels + channels]
        r, g, b = rgba[:3]
        print(f"SAMPLE x={x} y={y} rgb=#{r:02x}{g:02x}{b:02x}")
        if not (r > 200 and g < 40 and b < 40):
            ok = False
    print("SAMPLE_OK" if ok else "SAMPLE_FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
