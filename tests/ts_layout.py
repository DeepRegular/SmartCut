"""Report the scaffolding a transport stream puts around its elementary ones.

The PIDs, the PMT they are listed in, the descriptors on them and the PES
stream_id they are wrapped in. None of it is visible from the elementary
stream, and all of it is what a demuxer built around broadcast recordings
looks at first -- so when such a tool reads the source but not the cut, this
is where the difference will be.

Prints `key=value` lines for a caller to compare.
"""
import sys
from collections import Counter, OrderedDict

path = sys.argv[1]
data = open(path, "rb").read()
start = data.find(b"\x47")
SZ = 188

pmt_pids, pcr_pid = set(), None
streams = OrderedDict()          # pid -> stream_type
descriptors = {}                 # pid -> list of descriptor tags
pes_sid = {}                     # pid -> Counter of stream_id
pes_flags = {}                   # pid -> Counter of "PTS/DTS" presence
pid_count = Counter()

def parse_pmt(body):
    global pcr_pid
    if len(body) < 12:
        return
    slen = ((body[1] & 0x0F) << 8) | body[2]
    pcr_pid = ((body[8] & 0x1F) << 8) | body[9]
    pil = ((body[10] & 0x0F) << 8) | body[11]
    i = 12 + pil
    end = 3 + slen - 4
    while i + 5 <= end and i + 5 <= len(body):
        stype = body[i]
        pid = ((body[i + 1] & 0x1F) << 8) | body[i + 2]
        el = ((body[i + 3] & 0x0F) << 8) | body[i + 4]
        streams[pid] = stype
        tags, j = [], i + 5
        while j + 2 <= i + 5 + el and j + 2 <= len(body):
            tags.append(body[j])
            j += 2 + body[j + 1]
        descriptors[pid] = tags
        i += 5 + el

for off in range(start, len(data) - SZ, SZ):
    p = data[off:off + SZ]
    if p[0] != 0x47:
        continue
    pid = ((p[1] & 0x1F) << 8) | p[2]
    pid_count[pid] += 1
    payload_start = bool(p[1] & 0x40)
    afc = (p[3] >> 4) & 3
    i = 4
    if afc & 2:
        i += 1 + p[4]
    if not afc & 1 or i >= SZ:
        continue
    body = p[i:]
    if pid == 0 and payload_start:
        b = body[1 + body[0]:]
        slen = ((b[1] & 0x0F) << 8) | b[2]
        j = 8
        while j + 4 <= 3 + slen - 4:
            if ((b[j] << 8) | b[j + 1]) != 0:
                pmt_pids.add(((b[j + 2] & 0x1F) << 8) | b[j + 3])
            j += 4
    elif pid in pmt_pids and payload_start:
        parse_pmt(body[1 + body[0]:])
    elif payload_start and body[:3] == b"\x00\x00\x01":
        sid = body[3]
        pes_sid.setdefault(pid, Counter())[sid] += 1
        if len(body) > 7:
            f = (body[7] >> 6) & 3
            pes_flags.setdefault(pid, Counter())[
                {0: "なし", 2: "PTS", 3: "PTS+DTS"}.get(f, str(f))] += 1

def first_of(kind):
    """The PID and stream_type of the first stream of a kind we care about."""
    video = {0x01, 0x02, 0x1B, 0x24}
    audio = {0x03, 0x04, 0x0F, 0x11}
    want = video if kind == "video" else audio
    for pid, st in streams.items():
        if st in want:
            return pid, st
    return None, None

print(f"pmt_pid={sorted(pmt_pids)[0] if pmt_pids else 0}")
vpid, vst = first_of("video")
apid, ast = first_of("audio")
for name, pid, st in (("video", vpid, vst), ("audio", apid, ast)):
    if pid is None:
        print(f"{name}_pid=0")
        continue
    sid = pes_sid.get(pid, Counter()).most_common(1)
    print(f"{name}_pid={pid}")
    print(f"{name}_stream_type={st}")
    print(f"{name}_stream_id={sid[0][0] if sid else 0}")
    print(f"{name}_descriptors={','.join('%d' % t for t in descriptors.get(pid, []))}")
