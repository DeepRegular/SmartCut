"""Report what a transport stream says about the broadcast it came from.

`ts_layout.py` asks how the elementary streams are wrapped. This asks the
other question: what the recording says about *itself*, and whether a cut of
it still says the same thing. The service and its name, the descriptors on
each stream, the programme on now and the one after, and the clock.

None of it survives a mux -- libavformat writes its own tables from the
streams and nothing else -- so all of it is put back afterwards, and this is
what says whether it was.

Prints `key=value` lines for a caller to compare, plus one `eit.<n>=` line
per event section, hashed, so two files can be told apart without either
this or the caller having to understand ARIB text.

The conditional access descriptor (0x09) is left out of every descriptor
loop printed here. It says where the entitlement messages are and which
system scrambles the service, and a cut carries neither -- so a cut that
restated it would be describing a file that does not exist. Printing it
would make every comparison fail for the one difference that is correct.
"""
import hashlib
import sys

SZ = 188
PID_SDT, PID_EIT, PID_TDT = 0x0011, 0x0012, 0x0014

path = sys.argv[1]
data = open(path, "rb").read()
start = data.find(b"\x47")


def no_ca(loop):
    """A descriptor loop with the conditional access descriptor taken out."""
    out, i = bytearray(), 0
    while i + 2 <= len(loop):
        ln = loop[i + 1]
        if loop[i] != 0x09:
            out += loop[i:i + 2 + ln]
        i += 2 + ln
    return bytes(out)


def crc32(b):
    crc = 0xFFFFFFFF
    for x in b:
        crc ^= x << 24
        for _ in range(8):
            crc = ((crc << 1) ^ 0x04C11DB7) & 0xFFFFFFFF if crc & 0x80000000 else (crc << 1) & 0xFFFFFFFF
    return crc


def sections(want_pids):
    """Reassemble the sections arriving on the PIDs asked for."""
    held = {}
    for off in range(start, len(data) - SZ, SZ):
        p = data[off:off + SZ]
        if p[0] != 0x47:
            continue
        pid = ((p[1] & 0x1F) << 8) | p[2]
        if pid not in want_pids:
            continue
        afc = (p[3] >> 4) & 3
        i = 4
        if afc & 2:
            i += 1 + p[4]
        if not afc & 1 or i >= SZ:
            continue
        body = p[i:]
        if p[1] & 0x40:
            ptr = body[0]
            if pid in held and ptr:
                held[pid] += body[1:1 + ptr]
            held[pid] = body[1 + ptr:]
        elif pid in held:
            held[pid] += body
        else:
            continue
        buf = held[pid]
        while len(buf) >= 3 and buf[0] != 0xFF:
            ln = 3 + (((buf[1] & 0x0F) << 8) | buf[2])
            if len(buf) < ln:
                break
            sec, buf = buf[:ln], buf[ln:]
            # Time and date carries no CRC; everything else here does.
            if sec[1] & 0x80 == 0 or crc32(sec) == 0:
                yield pid, sec
        held[pid] = buf


# --- the service, from the PAT and the PMT it points at --------------------
pat = None
for pid, sec in sections({0x0000}):
    if sec[0] == 0x00:
        pat = sec
        break
if pat is None:
    print("error=no PAT")
    sys.exit(0)

tsid = (pat[3] << 8) | pat[4]
service_id = pmt_pid = 0
i = 8
while i + 4 <= len(pat) - 4:
    number = (pat[i] << 8) | pat[i + 1]
    if number:
        service_id = number
        pmt_pid = ((pat[i + 2] & 0x1F) << 8) | pat[i + 3]
        break
    i += 4
print(f"transport_stream_id={tsid}")
print(f"service_id={service_id}")
print(f"pmt_pid={pmt_pid}")

for pid, sec in sections({pmt_pid}):
    if sec[0] != 0x02 or ((sec[3] << 8) | sec[4]) != service_id:
        continue
    pil = ((sec[10] & 0x0F) << 8) | sec[11]
    print(f"program_info={no_ca(sec[12:12 + pil]).hex()}")
    i, end = 12 + pil, len(sec) - 4
    while i + 5 <= end:
        stype = sec[i]
        spid = ((sec[i + 1] & 0x1F) << 8) | sec[i + 2]
        el = ((sec[i + 3] & 0x0F) << 8) | sec[i + 4]
        print(f"stream.{spid:04x}={stype:02x}:{no_ca(sec[i + 5:i + 5 + el]).hex()}")
        i += 5 + el
    break

# --- the service description, the events and the clock ---------------------
for pid, sec in sections({PID_SDT}):
    if sec[0] != 0x42:
        continue
    onid = (sec[8] << 8) | sec[9]
    i, end = 11, len(sec) - 4
    while i + 5 <= end:
        sid = (sec[i] << 8) | sec[i + 1]
        dl = ((sec[i + 3] & 0x0F) << 8) | sec[i + 4]
        if sid == service_id:
            print(f"original_network_id={onid}")
            # The name is ARIB text; its bytes are the comparison.
            print(f"sdt.service={sec[i:i + 5 + dl].hex()}")
        i += 5 + dl
    break

events = {}
for pid, sec in sections({PID_EIT}):
    # Present and following, for this service: the two sections that say
    # what this recording is of.
    if sec[0] != 0x4E or ((sec[3] << 8) | sec[4]) != service_id:
        continue
    events.setdefault(sec[6], sec)
    if len(events) >= 2:
        break
for n, sec in sorted(events.items()):
    print(f"eit.{n}={hashlib.sha1(sec).hexdigest()[:16]}")

for pid, sec in sections({PID_TDT}):
    if sec[0] in (0x70, 0x73):
        print(f"clock={(sec[3] << 8) | sec[4]}:{sec[5]:02x}{sec[6]:02x}{sec[7]:02x}")
        break
