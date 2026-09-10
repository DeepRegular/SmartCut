"""Put a synthetic ARIB caption stream into a transport stream.

The demo clip the screenshots are taken from is built out of ffmpeg's own
sources and carries no captions, because ffmpeg cannot write one: there is no
ARIB caption encoder anywhere, which is the same reason SmartCut reads them
itself (see `caption.rs`). So the statements are written here, byte by byte,
in the shape every broadcast measured against this program sends:

    CS  SWF(7)  SDF(620;480)  SDP(170;30)  SHS(4)  SVS(24)  SSM(36;36)
    APS(row, 0)  <text>

No recorded material is involved, and nothing here is a decoder: this writes
what a caption encoder would have written, so that the reader has something
of its own to read.

    add_captions.py IN.ts OUT.ts

The lines are below, in `CAPTIONS`: a time in seconds and the lines to put on
screen, with an empty list for the moment the caption comes down.
"""
import sys

PACKET = 188
CAPTION_PID = 0x0102
COMPONENT_TAG = 0x30

# When each caption goes up, and what it says. Timed against the demo clip's
# own structure -- programme, break, programme -- so that a screenshot of a
# cut near a break has a line on screen either side of it.
CAPTIONS = [
    (8.0, ["これはテスト放送の字幕です"]),
    (14.0, ["この録画は合成した映像だけで", "できています"]),
    (22.0, []),
    (30.0, ["パートＡ・第３場面"]),
    (38.0, []),
    (46.0, ["まもなくＣＭに入ります"]),
    (56.0, []),
    (125.0, ["パートＢ・ここから本編です"]),
    (133.0, []),
    (140.0, ["字幕は放送局が指定した", "位置と色のまま出ます"]),
    (150.0, []),
    (195.0, ["パートＣ・第１場面"]),
    (203.0, []),
    (210.0, ["継ぎ目が台詞の途中に", "来ていないか確かめられます"]),
    (222.0, []),
]


def crc32(data):
    """The CRC every MPEG section ends with."""
    crc = 0xFFFFFFFF
    for byte in data:
        crc ^= byte << 24
        for _ in range(8):
            crc = ((crc << 1) ^ 0x04C11DB7) & 0xFFFFFFFF if crc & 0x80000000 else (crc << 1) & 0xFFFFFFFF
    return crc


def crc16(data):
    """The CRC a caption's data group ends with (CCITT, zero seeded)."""
    crc = 0
    for byte in data:
        crc ^= byte << 8
        for _ in range(8):
            crc = ((crc << 1) ^ 0x1021) & 0xFFFF if crc & 0x8000 else (crc << 1) & 0xFFFF
    return crc


def arib(text):
    """One line as ARIB's eight-unit code.

    The kanji set is what a statement starts with in G0, and JIS X 0208 is
    what EUC-JP carries with the top bit set -- so the same bytes, taken
    down into the graphic left range, are the characters the caption means.

    Which is why the Latin here is widened first. A caption written in the
    kanji set is two bytes a character, and an ASCII byte left among them is
    read as half of the next character: `CM` came back as a kanji nobody
    asked for. A broadcaster writes those wide for the same reason.
    """
    wide = "".join(
        "\u3000" if c == " " else chr(ord(c) + 0xFEE0) if "!" <= c <= "~" else c
        for c in text
    )
    return bytes(b & 0x7F for b in wide.encode("euc_jp"))


def statement(lines):
    """The text units of one caption statement.

    The format is declared ahead of the text every time, which is what every
    broadcaster does and what lets a caption be read from the middle of a
    recording.
    """
    out = bytearray([0x0C])  # clear the screen
    for csi in (b"7 S", b"620;480 V", b"170;30 _", b"4 X", b"24 Y", b"36;36 W"):
        out += b"\x9b" + csi
    # The lines sit at the bottom of the eight rows the display area holds.
    row = 8 - len(lines)
    for line in lines:
        out += bytes([0x1C, 0x40 + row, 0x40])
        out += arib(line)
        row += 1
    return bytes(out)


def data_group(lines):
    """One statement, wrapped as the data group a PES packet carries."""
    text = statement(lines)
    unit = bytes([0x1F, 0x20, 0, (len(text) >> 8) & 0xFF, len(text) & 0xFF]) + text
    body = bytes([0x00])  # TMD: free
    body += bytes([0, (len(unit) >> 8) & 0xFF, len(unit) & 0xFF]) + unit
    # data_group_id 1: statement data, language 1, group A.
    head = bytes([1 << 2, 0, 0, (len(body) >> 8) & 0xFF, len(body) & 0xFF])
    group = head + body
    return group + crc16(group).to_bytes(2, "big")


def pes(lines, pts):
    """The data group in a PES packet of its own, timed."""
    payload = bytes([0x80, 0xFF, 0xF0]) + data_group(lines)
    stamp = bytes([
        0x21 | ((pts >> 29) & 0x0E),
        (pts >> 22) & 0xFF,
        0x01 | ((pts >> 14) & 0xFE),
        (pts >> 7) & 0xFF,
        0x01 | ((pts << 1) & 0xFE),
    ])
    header = bytes([0x80, 0x80, len(stamp)]) + stamp
    body = header + payload
    return bytes([0, 0, 1, 0xBD, (len(body) >> 8) & 0xFF, len(body) & 0xFF]) + body


def ts_packets(pid, payload, counter):
    """One PES payload as transport packets, stuffed to the packet size."""
    out = []
    first = True
    while payload:
        room = 184
        head = bytes([
            0x47,
            (0x40 if first else 0x00) | ((pid >> 8) & 0x1F),
            pid & 0xFF,
            0x10 | (counter & 0x0F),
        ])
        counter += 1
        take = payload[:room]
        payload = payload[room:]
        if len(take) < room:
            # An adaptation field of nothing but stuffing, which is how a
            # short payload is padded out to a whole packet.
            pad = room - len(take)
            head = head[:3] + bytes([0x30 | (head[3] & 0x0F)])
            af = bytes([pad - 1]) + (bytes([0]) if pad >= 2 else b"") + b"\xff" * max(0, pad - 2)
            out.append(head + af + take)
        else:
            out.append(head + take)
        first = False
    return out, counter


def parse_pat(packet):
    """The programme map's PID, from a packet of the programme table."""
    at = 4
    if packet[3] & 0x20:
        at += 1 + packet[at]
    at += 1 + packet[at]  # pointer field
    length = ((packet[at + 1] & 0x0F) << 8) | packet[at + 2]
    end = at + 3 + length - 4
    i = at + 8
    while i + 4 <= end:
        program = (packet[i] << 8) | packet[i + 1]
        pid = ((packet[i + 2] & 0x1F) << 8) | packet[i + 3]
        if program:
            return pid
        i += 4
    return None


def with_caption_stream(packet):
    """The programme map, with a caption stream added to it.

    Two descriptors, because ffmpeg reads both: the stream identifier that
    gives the stream its component tag, and the data component descriptor
    whose id 0x0008 is what says "these are captions" rather than some other
    private data on stream type 0x06.
    """
    at = 4
    if packet[3] & 0x20:
        at += 1 + packet[at]
    at += 1 + packet[at]
    length = ((packet[at + 1] & 0x0F) << 8) | packet[at + 2]
    section = bytearray(packet[at:at + 3 + length])
    descriptors = bytes([0x52, 0x01, COMPONENT_TAG]) + bytes([0xFD, 0x03, 0x00, 0x08, 0x3D])
    entry = bytes([
        0x06,
        0xE0 | ((CAPTION_PID >> 8) & 0x1F),
        CAPTION_PID & 0xFF,
        0xF0 | ((len(descriptors) >> 8) & 0x0F),
        len(descriptors) & 0xFF,
    ]) + descriptors
    body = section[:-4] + entry
    length = len(body) - 3 + 4
    body[1] = (body[1] & 0xF0) | ((length >> 8) & 0x0F)
    body[2] = length & 0xFF
    section = bytes(body) + crc32(bytes(body)).to_bytes(4, "big")
    room = PACKET - at
    if len(section) > room:
        sys.exit("the programme map has no room for another stream")
    return packet[:at] + section + b"\xff" * (room - len(section))


def pcr_of(packet):
    """The clock a packet carries, in 90 kHz ticks, or None."""
    if not packet[3] & 0x20 or packet[4] == 0:
        return None
    if not packet[5] & 0x10:
        return None
    base = (packet[6] << 25) | (packet[7] << 17) | (packet[8] << 9) | (packet[9] << 1) | (packet[10] >> 7)
    return base


def main():
    src, dst = sys.argv[1], sys.argv[2]
    data = open(src, "rb").read()
    packets = [data[i:i + PACKET] for i in range(0, len(data) - PACKET + 1, PACKET)]
    pmt_pid = None
    for p in packets:
        pid = ((p[1] & 0x1F) << 8) | p[2]
        if pid == 0 and p[1] & 0x40:
            pmt_pid = parse_pat(p)
            break
    if pmt_pid is None:
        sys.exit("no programme map in " + src)

    # Where the clock starts, so the captions can be timed from the top of
    # the recording rather than from the container's own zero.
    first_pcr = next((pcr_of(p) for p in packets if pcr_of(p) is not None), None)
    if first_pcr is None:
        sys.exit("no clock in " + src)

    todo = list(CAPTIONS)
    counter = 0
    out = bytearray()
    clock = first_pcr
    for p in packets:
        pcr = pcr_of(p)
        if pcr is not None:
            clock = pcr
        while todo and (clock - first_pcr) / 90000.0 >= todo[0][0]:
            at, lines = todo.pop(0)
            pts = first_pcr + int(at * 90000)
            built, counter = ts_packets(CAPTION_PID, pes(lines, pts), counter)
            for one in built:
                out += one
        pid = ((p[1] & 0x1F) << 8) | p[2]
        out += with_caption_stream(p) if pid == pmt_pid and p[1] & 0x40 else p
    open(dst, "wb").write(bytes(out))
    print(f"{len(CAPTIONS)} caption statement(s) on PID 0x{CAPTION_PID:04x} -> {dst}")


if __name__ == "__main__":
    main()
