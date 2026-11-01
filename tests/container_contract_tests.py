"""Synthetic regressions for container SAR, source clocks, AV1 config and ATS.

Requires only Python's standard library and FFmpeg/ffprobe. Generated media
stay in a new work directory, printed at the end for failure investigation.
"""
import argparse
from fractions import Fraction
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--cut', type=Path, default=Path('rust/target/release/smartcut' + ('.exe' if os.name == 'nt' else '')))
    parser.add_argument('--ffmpeg', default=os.environ.get('FFMPEG', 'ffmpeg'))
    parser.add_argument('--ffprobe', default=os.environ.get('FFPROBE', 'ffprobe'))
    parser.add_argument('--work-dir', type=Path)
    args = parser.parse_args()
    work = Path(tempfile.mkdtemp(prefix='smartcut-container-', dir=args.work_dir))
    cut = args.cut.resolve()
    results = []

    def run(command):
        p = subprocess.run(list(map(str, command)), capture_output=True, timeout=240)
        if p.returncode:
            raise AssertionError(f'exit {p.returncode}: {command}\n{p.stderr.decode(errors="replace")[-2500:]}')
        return p

    def probe(path):
        return json.loads(run([args.ffprobe, '-v', 'error', '-select_streams', 'v:0', '-show_streams',
                               '-show_packets', '-show_data', '-show_format', '-of', 'json', path]).stdout)

    def unhex(text):
        return b''.join(bytes.fromhex(line.split(':', 1)[1].split('  ')[0].replace(' ', ''))
                        for line in text.splitlines() if ':' in line)

    def sequences(d):
        headers = set()
        for p in d['packets']:
            data = unhex(p.get('data', ''))
            while data:
                h, data = data[0], data[1:]
                if h & 4:
                    data = data[1:]
                size = len(data)
                if h & 2:
                    size = 0
                    for i in range(8):
                        v, data = data[0], data[1:]
                        size |= (v & 127) << (7*i)
                        if not v & 128:
                            break
                body, data = data[:size], data[size:]
                if (h >> 3) & 15 == 1:
                    headers.add(body)
        return headers

    def av1_contract(path, d):
        # FFmpeg's independent CBS reader reports every actual sequence level.
        trace = run([args.ffmpeg, '-hide_banner', '-loglevel', 'info', '-i', path, '-map', '0:v:0',
                     '-c', 'copy', '-bsf:v', 'trace_headers', '-f', 'null', '-']).stderr.decode(errors='replace')
        levels = [int(x) for x in re.findall(r'\bseq_level_idx\[0\]\s+\S+\s+=\s*(\d+)', trace)]
        assert levels, 'CBS did not report a sequence level'
        if path.suffix in ('.mkv', '.webm'):
            assert len(sequences(d)) == 1, 'one Matroska AV1 track contains different Sequence Headers'
            extra = unhex(d['streams'][0]['extradata'])
        else:
            def boxes(data):
                pos = 0
                while pos + 8 <= len(data):
                    n = int.from_bytes(data[pos:pos+4], 'big'); kind = data[pos+4:pos+8]; head = 8
                    if n == 1:
                        n = int.from_bytes(data[pos+8:pos+16], 'big'); head = 16
                    if n == 0:
                        n = len(data) - pos
                    assert n >= head and pos+n <= len(data)
                    yield kind, data[pos+head:pos+n]
                    pos += n
            def find(data, wanted):
                for kind, body in boxes(data):
                    if kind == wanted:
                        yield body
                    elif kind in (b'moov', b'trak', b'mdia', b'minf', b'stbl'):
                        yield from find(body, wanted)
            descriptions = list(find(path.read_bytes(), b'stsd'))
            video = [body for body in descriptions if body[12:16] == b'av01']
            assert len(video) == 1
            entries = list(boxes(video[0][8:]))
            assert len(entries) == 1, 'this regression targets the FFmpeg 7.1 single-description route'
            configs = [b for k, b in boxes(entries[0][1][78:]) if k == b'av1C']
            assert len(configs) == 1
            extra = configs[0]
        assert extra[0] == 0x81
        assert all(level == extra[1] & 31 for level in levels), (levels, extra[:4].hex())

    def check(path, src, ranges):
        d = probe(path)
        s = src['streams'][0]; st = Fraction(s['time_base'])
        packets = sorted(src['packets'], key=lambda p: int(p['pts']))
        times = [int(p['pts'])*st for p in packets]
        times.append(times[-1]+int(packets[-1]['duration'])*st)
        expected = []; duration = Fraction(0)
        for a, b in ranges:
            expected += [duration+t-times[a] for t in times[a:b]]
            duration += times[b]-times[a]
        tb = Fraction(d['streams'][0]['time_base'])
        observed = sorted(int(p['pts'])*tb for p in d['packets'])
        origin = observed[0]
        assert len(observed) == len(expected), (len(observed), len(expected))
        assert all(abs(a-origin-b) <= tb for a, b in zip(observed, expected)), 'source display PTS changed'
        # MP4 stts durations advance the decode clock. With reordered B
        # pictures, pts + that packet's stts duration is not its display end.
        # Use the video track duration; these fixtures have no other tracks.
        end = (Fraction(d['format']['duration'])-origin if path.suffix == '.mkv'
               else Fraction(d['streams'][0]['duration']))
        assert abs(end-duration) <= max(tb, Fraction(1, 1000)), (float(end), float(duration))
        assert d['streams'][0].get('sample_aspect_ratio') == s.get('sample_aspect_ratio'), 'SAR changed'
        run([args.ffmpeg, '-v', 'error', '-xerror', '-err_detect', 'explode', '-i', path,
             '-map', '0:v:0', '-f', 'null', '-'])
        if s['codec_name'] == 'av1':
            av1_contract(path, d)
        return d

    def export(name, source, ranges, suffix=None, extra=()):
        src = probe(source); tb = Fraction(src['streams'][0]['time_base'])
        ps = sorted(src['packets'], key=lambda p: int(p['pts']))
        times = [int(p['pts'])*tb for p in ps]
        times.append(times[-1]+int(ps[-1]['duration'])*tb)
        out = work/(name+(suffix or source.suffix))
        cmd = [cut, source, '--no-chapters', '-o', out, *extra]
        for a, b in ranges:
            cmd += ['--keep', f'{float(times[a]-times[0]):.12f}-{float(times[b]-times[0]):.12f}']
        log = run(cmd); out.with_suffix(out.suffix+'.log').write_bytes(log.stdout+log.stderr)
        d = check(out, src, ranges)
        results.append({'name': name, 'frames': len(d['packets']),
                        'full_video_fallback': b'The entire selected video is re-encoded' in log.stdout+log.stderr})
        print('ok', name, flush=True)
        return out

    def generate(name, encoder, rate='25', vfr=False, sar=False):
        out = work/(name+'.mp4')
        filters = []
        if vfr:
            filters.append('setpts=(N+floor(N/3))/(25*TB)')
        if sar:
            filters.append('setsar=4/3')
        enc = {'libx264':['-preset','medium','-crf','14','-bf','3'],
               'libx265':['-preset','medium','-crf','18','-x265-params','log-level=error:pools=none'],
               'libaom-av1':['-cpu-used','8','-crf','22','-b:v','0'],
               'librav1e':['-speed','10','-qp','70']}[encoder]
        cmd = [args.ffmpeg, '-v', 'error', '-y', '-f', 'lavfi', '-i', f'testsrc2=size=192x112:rate={rate}',
               '-frames:v', '240', '-an', '-c:v', encoder, *enc, '-g', '24', '-pix_fmt', 'yuv420p']
        if filters:
            cmd += ['-vf', ','.join(filters)]
        cmd += ['-fps_mode', 'passthrough', out]
        run(cmd)
        return out

    for codec in ('libx264', 'libx265', 'libaom-av1'):
        source = generate(codec+'-vfr', codec, vfr=True)
        for suffix in ('.mp4', '.mkv'):
            current = source
            if suffix == '.mkv':
                current = source.with_suffix(suffix)
                run([args.ffmpeg, '-v', 'error', '-y', '-i', source, '-c', 'copy', current])
            for label, ranges in [('partial', [(13,227)]), ('multi', [(13,81),(100,227)]), ('one', [(23,24)])]:
                export(codec+'-vfr-'+suffix[1:]+'-'+label, current, ranges)
    ntsc = generate('ntsc', 'libx264', rate='30000/1001')
    run([args.ffmpeg, '-v', 'error', '-y', '-i', ntsc, '-c', 'copy', ntsc.with_suffix('.mkv')])
    export('rounded-ntsc', ntsc.with_suffix('.mkv'), [(13,81),(100,227)])
    sar = generate('sar', 'libaom-av1', sar=True)
    export('sar-copy', sar, [(0,240)])
    export('sar-reencode', sar, [(13,227)])
    changed = generate('level31', 'librav1e')
    export('av1-description', changed, [(13,227)])

    source = generate('transport', 'libx264')
    ts = source.with_suffix('.ts')
    run([args.ffmpeg, '-v', 'error', '-y', '-i', source, '-c', 'copy', ts])
    # Transport clocks have a lead-in, so check frame count/decoding here;
    # the native-file research matrix checks their relative PTS separately.
    for mode in ('muxer', 'partial', 'broadcast'):
        for label, bounds in [('partial','0.52-9.08'),('one','0.92-0.96')]:
            out = work/f'arrivals-{mode}-{label}.m2ts'
            run([cut, ts, '--keep', bounds, '--tables', mode, '-o', out])
            data = out.read_bytes(); assert len(data)%192 == 0
            stamps = []
            for pos in range(0,len(data),192):
                assert data[pos+4] == 0x47
                stamps.append(int.from_bytes(data[pos:pos+4],'big') & 0x3fffffff)
            assert all(((b-a) & 0x3fffffff) < 1 << 29 for a,b in zip(stamps,stamps[1:])), 'arrival clock runs backwards'
            d = probe(out); assert len(d['packets']) == (214 if label == 'partial' else 1)
            run([args.ffmpeg, '-v', 'error', '-xerror', '-err_detect', 'explode', '-i', out, '-map', '0:v', '-f', 'null', '-'])
            results.append({'name':out.stem,'frames':len(d['packets'])}); print('ok',out.stem,flush=True)
    (work/'results.json').write_text(json.dumps(results, indent=2)+'\n', encoding='utf-8')
    print(f'{len(results)} passed; evidence: {work}',flush=True)


if __name__ == '__main__':
    main()
