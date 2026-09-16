"""Compare the data broadcast in a cut against the one in the recording.

What is behind the blue button is a *carousel*: a set of modules sent round
and round in DSM-CC sections, so that a receiver switching on at any moment
has the whole of it within a few seconds. A cut cannot mux one -- libav
delivers no packets at all for a stream of sections -- so SmartCut carries
the packets across itself, byte for byte, in the pass that puts the
broadcast's own tables back.

This is what says whether that worked. Three questions, and a cut has to
answer all three:

  * **is every module whole?** A receiver reassembles a module out of its
    blocks and cannot draw a page whose blocks are not all there. The
    carousel goes round every few seconds, so a kept range of any length
    holds a whole turn of it -- and a cut that dealt the packets out wrongly
    would show up here as a module short of blocks.
  * **is every module the recording's own?** Compared by content, block by
    block. Nothing here is re-encoded or rewritten, so anything but an exact
    match is a fault.
  * **does the count still run?** The continuity counter has to be
    renumbered, because what fell between two kept ranges was not written; a
    gap in the count is packets a receiver believes it missed.

Prints `key=value` lines for a caller to compare.
"""
import hashlib
import sys

SZ = 188


def framing(data):
    """Where the packets start. 188 bytes throughout; a cut is never m2ts."""
    for at in range(0, SZ * 3):
        if data[at] == 0x47 and data[at + SZ] == 0x47:
            return at
    return None


def packets(path, limit=None):
    """Every packet in the file, in order.

    `limit` stops the reading after that many bytes. The recording behind a
    cut can be gigabytes of which the cut used the first few minutes, and a
    test that read all of it would spend its time on packets no range
    covered.
    """
    with open(path, "rb") as f:
        head = f.read(SZ * 4)
        at = framing(head)
        if at is None:
            return
        f.seek(at)
        read = 0
        while limit is None or read < limit:
            p = f.read(SZ)
            read += SZ
            if len(p) < SZ or p[0] != 0x47:
                return
            yield p


def sections(stream):
    """Reassemble the sections on one PID out of its packets."""
    held, want = None, 0
    for p in stream:
        afc = (p[3] >> 4) & 3
        at = 4
        if afc & 2:
            at += 1 + p[4]
        if at >= SZ:
            continue
        if p[1] & 0x40:
            at += 1 + p[at]
            held = bytearray(p[at:])
            want = ((held[1] & 0x0F) << 8 | held[2]) + 3 if len(held) >= 3 else 0
        elif held is not None:
            held += p[at:]
        if held is not None and want and len(held) >= want:
            yield bytes(held[:want])
            held, want = None, 0


def data_pids(path):
    """The PIDs the map calls a data carousel: DSM-CC sections, type 0x0D."""
    pmt_pids = []
    for sec in sections(p for p in packets(path) if (p[1] << 8 | p[2]) & 0x1FFF == 0):
        if sec[0] != 0x00:
            continue
        end = ((sec[1] & 0x0F) << 8 | sec[2]) + 3 - 4
        for i in range(8, end, 4):
            if sec[i] << 8 | sec[i + 1]:
                pmt_pids.append((sec[i + 2] & 0x1F) << 8 | sec[i + 3])
        break
    out = []
    for pmt in pmt_pids:
        for sec in sections(p for p in packets(path) if (p[1] << 8 | p[2]) & 0x1FFF == pmt):
            if sec[0] != 0x02:
                continue
            end = ((sec[1] & 0x0F) << 8 | sec[2]) + 3 - 4
            i = 12 + ((sec[10] & 0x0F) << 8 | sec[11])
            while i + 5 <= end:
                kind = sec[i]
                pid = (sec[i + 1] & 0x1F) << 8 | sec[i + 2]
                length = (sec[i + 3] & 0x0F) << 8 | sec[i + 4]
                if kind == 0x0D:
                    out.append(pid)
                i += 5 + length
            break
    return sorted(set(out))


def modules(path, pids, limit=None):
    """Every module the file carries, by PID, module id and version.

    A download data block names the module it belongs to and which block of
    it this is; a module is whole when every block from nought to the last
    one is there. The blocks are kept as bytes so that two files can be
    compared by content rather than by count.

    One pass over the file for every PID at once: a broadcast sends its
    carousel on up to seven of them, and reading the file once per PID would
    be reading it seven times.
    """
    held = {}
    parts = {pid: [None, 0] for pid in pids}
    for p in packets(path, limit):
        pid = (p[1] << 8 | p[2]) & 0x1FFF
        if pid not in parts:
            continue
        state = parts[pid]
        afc = (p[3] >> 4) & 3
        at = 4
        if afc & 2:
            at += 1 + p[4]
        if at >= SZ:
            continue
        if p[1] & 0x40:
            at += 1 + p[at]
            state[0] = bytearray(p[at:])
            state[1] = ((state[0][1] & 0x0F) << 8 | state[0][2]) + 3 if len(state[0]) >= 3 else 0
        elif state[0] is not None:
            state[0] += p[at:]
        if state[0] is None or not state[1] or len(state[0]) < state[1]:
            continue
        sec = bytes(state[0][: state[1]])
        state[0], state[1] = None, 0
        # 0x3C is the download data block; the message header in front of it
        # can carry an adaptation field of its own.
        if sec[0] != 0x3C or len(sec) < 26:
            continue
        adapt = sec[17]
        module = sec[20 + adapt] << 8 | sec[21 + adapt]
        version = sec[22 + adapt]
        block = sec[24 + adapt] << 8 | sec[25 + adapt]
        held.setdefault((pid, module, version), {}).setdefault(block, sec[26 + adapt : -4])
    return held


def whole(blocks):
    return len(blocks) == max(blocks) + 1


def digest(blocks):
    h = hashlib.sha1()
    for i in sorted(blocks):
        h.update(blocks[i])
    return h.hexdigest()[:12]


def counts(path, pids):
    """Packets per PID, and how often the continuity counter skipped."""
    seen, errors, last = {}, {}, {}
    for p in packets(path):
        pid = (p[1] << 8 | p[2]) & 0x1FFF
        if pid not in pids:
            continue
        seen[pid] = seen.get(pid, 0) + 1
        count = p[3] & 0x0F
        if pid in last:
            # A packet with no payload repeats the number before it; one with
            # a payload moves the count on by one.
            want = (last[pid] + 1) & 0x0F if p[3] & 0x10 else last[pid]
            if count != want:
                errors[pid] = errors.get(pid, 0) + 1
        last[pid] = count
    return seen, errors


recording, cut = sys.argv[1], sys.argv[2]
was = data_pids(recording)
now = data_pids(cut)
print(f"source_pids={' '.join(f'{p:04x}' for p in was)}")
print(f"cut_pids={' '.join(f'{p:04x}' for p in now)}")

why = []
if not was:
    print("source=0")
    print("ok=0" if now else "ok=1")
    sys.exit(0)
print(f"source={len(was)}")
if sorted(now) != sorted(was):
    why.append(f"mapの並びが違う({was}→{now})")

seen, errors = counts(cut, now)
print(f"packets={sum(seen.values())}")
print(f"cc_errors={sum(errors.values())}")
if sum(errors.values()):
    why.append(f"連続性カウンタの飛び {sum(errors.values())}")
if not sum(seen.values()):
    why.append("データ放送のパケットが1つもない")

# The whole recording, because what a cut holds has to be looked for wherever
# it came from: a range an hour in is an hour in. A third argument bounds the
# reading in megabytes, for a caller that knows its ranges are near the start
# and would rather not read gigabytes to prove it.
bound = int(sys.argv[3]) << 20 if len(sys.argv) > 3 else None
theirs = modules(recording, was, limit=bound)
ours = modules(cut, now)
print(f"modules={len(ours)}")
broken = [k for k, v in ours.items() if not whole(v)]
print(f"whole={len(ours) - len(broken)}")
for pid, module, version in broken:
    why.append(f"module {module} v{version} (pid {pid:04x}) のブロックが欠けている")
matched = 0
for key, blocks in ours.items():
    if not whole(blocks):
        continue
    if key not in theirs or not whole(theirs[key]):
        where = "" if bound is None else f"（録画の先頭 {bound >> 20} MB しか読んでいません）"
        why.append(f"module {key[1]} v{key[2]} が録画側に見つからない{where}")
    elif digest(blocks) != digest(theirs[key]):
        why.append(f"module {key[1]} v{key[2]} の中身が違う")
    else:
        matched += 1
print(f"matched={matched}")
print(f"ok={0 if why else 1}")
if why:
    print("why=" + ", ".join(why))
