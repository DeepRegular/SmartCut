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
PID_SDT, PID_EIT, PID_TDT, PID_SIT = 0x0011, 0x0012, 0x0014, 0x001F

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


# ARIB points at a stream by a one-byte component tag, and three descriptors
# do so: component (0x50), audio component (0xC4) and data content (0xC7),
# each with the tag third in the body. A cut carries fewer streams than the
# recording it came from, so these are the descriptors that legitimately
# differ between the two -- separated out here rather than compared.
COMPONENT_DESCRIPTORS = {0x50, 0xC4, 0xC7}


def each_event(sec):
    """Walk an event information section, yielding (header, descriptors).

    The header is the twelve bytes that say which event this is, when it
    started and how long it ran.
    """
    at, end = 14, len(sec) - 4
    while at + 12 <= end:
        ln = ((sec[at + 10] & 0x0F) << 8) | sec[at + 11]
        yield sec[at:at + 12], sec[at + 12:at + 12 + ln]
        at += 12 + ln


def component_refs(sec):
    """The component tags an event information section names."""
    out = set()
    for _, loop in each_event(sec):
        i = 0
        while i + 2 <= len(loop):
            ln = loop[i + 1]
            if loop[i] in COMPONENT_DESCRIPTORS and ln >= 3:
                out.add(loop[i + 4])
            i += 2 + ln
    return out


def no_components(sec):
    """An event information section with every component-naming descriptor out.

    Not a valid section, and not meant to be -- it is only ever hashed. Every
    length is blanked along with the descriptors, because taking a descriptor
    out shortens both the loop it was in and the section around it, and those
    are the bytes that would otherwise report a difference that is the point
    of the exercise rather than a fault in it.
    """
    out = bytearray(sec[:14])
    out[1] &= 0xF0
    out[2] = 0
    for head, loop in each_event(sec):
        out += head[:10] + bytes([head[10] & 0xF0, 0])
        i = 0
        while i + 2 <= len(loop):
            ln = loop[i + 1]
            if loop[i] not in COMPONENT_DESCRIPTORS:
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


# --- what the file says about the programme, wherever it says it ----------
#
# A broadcast says it in a service description and an event information
# table; a partial transport stream says the same things in one selection
# information table. Both are reported here under the same names, so a cut
# can be compared against the recording it came from whichever shape it was
# written in.
service_descriptor = None
event_descriptors = None
event_time = None


def take_service_and_event(loop, from_sit):
    """Sort a descriptor loop into the service, the times and the programme."""
    global service_descriptor, event_time
    rest, i = bytearray(), 0
    while i + 2 <= len(loop):
        ln = loop[i + 1]
        whole = loop[i:i + 2 + ln]
        if whole[0] == 0x48:
            service_descriptor = whole
        elif whole[0] == 0xC3 and from_sit:
            # partial transport stream time: the event version, then when it
            # started and how long it ran.
            event_time = whole[3:11]
        else:
            rest += whole
        i += 2 + ln
    return bytes(rest)


def no_component_descriptors(loop):
    out, i = bytearray(), 0
    while i + 2 <= len(loop):
        ln = loop[i + 1]
        if loop[i] not in COMPONENT_DESCRIPTORS:
            out += loop[i:i + 2 + ln]
        i += 2 + ln
    return bytes(out)


def loop_refs(loop):
    out, i = set(), 0
    while i + 2 <= len(loop):
        ln = loop[i + 1]
        if loop[i] in COMPONENT_DESCRIPTORS and ln >= 3:
            out.add(loop[i + 4])
        i += 2 + ln
    return out


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
    carried = set()
    while i + 5 <= end:
        stype = sec[i]
        spid = ((sec[i + 1] & 0x1F) << 8) | sec[i + 2]
        el = ((sec[i + 3] & 0x0F) << 8) | sec[i + 4]
        loop = sec[i + 5:i + 5 + el]
        print(f"stream.{spid:04x}={stype:02x}:{no_ca(loop).hex()}")
        # The stream identifier descriptor (0x52) is how a stream says which
        # component tag it answers to, and so which of the programme's
        # descriptors are allowed to be about it.
        j = 0
        while j + 2 <= len(loop):
            if loop[j] == 0x52 and loop[j + 1] >= 1:
                carried.add(loop[j + 2])
            j += 2 + loop[j + 1]
        i += 5 + el
    print(f"components={''.join(f'{t:02x}' for t in sorted(carried))}")
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
            take_service_and_event(sec[i + 5:i + 5 + dl], False)
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
    if n == 0:
        at = 14
        ln = ((sec[at + 10] & 0x0F) << 8) | sec[at + 11]
        event_descriptors = sec[at + 12:at + 12 + ln]
        event_time = sec[at + 2:at + 10]
    # Hashed without the descriptors that name a component, because those are
    # the ones a cut is entitled to differ on: it carries fewer streams than
    # the recording, and a description of a stream it does not carry is taken
    # out on the way. What is left -- which programme, when, its name, its
    # genre, the long text -- has to be the recording's own, byte for byte.
    print(f"eit.{n}={hashlib.sha1(no_components(sec)).hexdigest()[:16]}")
    print(f"eit.{n}.refs={''.join(f'{t:02x}' for t in sorted(component_refs(sec)))}")

for pid, sec in sections({PID_TDT}):
    if sec[0] in (0x70, 0x73):
        print(f"clock={(sec[3] << 8) | sec[4]}:{sec[5]:02x}{sec[6]:02x}{sec[7]:02x}")
        break

# --- the one table a partial transport stream carries instead --------------
for pid, sec in sections({PID_SIT}):
    if sec[0] != 0x7F:
        continue
    til = ((sec[8] & 0x0F) << 8) | sec[9]
    transmission = sec[10:10 + til]
    print(f"sit=1")
    print(f"sit.version={(sec[5] >> 1) & 0x1F}")
    print(f"sit.transmission={transmission.hex()}")
    j = 0
    while j + 2 <= len(transmission):
        ln = transmission[j + 1]
        if transmission[j] == 0x63 and ln == 8:
            body = transmission[j + 2:j + 2 + ln]
            peak = ((body[0] & 0x3F) << 16) | (body[1] << 8) | body[2]
            print(f"sit.peak_rate={peak * 400}")
        j += 2 + ln
    body = sec[10 + til:len(sec) - 4]
    sid = (body[0] << 8) | body[1]
    sll = ((body[2] & 0x0F) << 8) | body[3]
    print(f"sit.service_id={sid}")
    print(f"sit.running_status={(body[2] >> 4) & 7}")
    # The section has to add up, or nothing below it can be believed.
    print(f"sit.well_formed={int(10 + til + 4 + sll + 4 == len(sec))}")
    event_descriptors = take_service_and_event(body[4:4 + sll], True)
    break
else:
    print("sit=0")

# --- the same facts under the same names, whichever table carried them -----
if service_descriptor is not None:
    print(f"service_descriptor={bytes(service_descriptor).hex()}")
if event_descriptors is not None:
    loop = bytes(event_descriptors)
    print(f"event_descriptors={hashlib.sha1(no_component_descriptors(loop)).hexdigest()[:16]}")
    print(f"event_refs={''.join(f'{t:02x}' for t in sorted(loop_refs(loop)))}")
if event_time is not None:
    print(f"event_time={bytes(event_time).hex()}")
