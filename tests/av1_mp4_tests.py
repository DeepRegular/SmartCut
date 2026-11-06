"""AV1 MP4 sample-description regressions; Matroska keeps its current policy.

Needs Python 3.11+, FFmpeg with libaom-av1/librav1e/libsvtav1 and ffprobe.
Media, commands, hashes and failures are retained in a fresh work directory.
"""
import argparse
from fractions import Fraction
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def unhex(text):
    return b''.join(bytes.fromhex(line.split(':', 1)[1].split('  ')[0].replace(' ', ''))
                    for line in text.splitlines() if ':' in line)


def boxes(data):
    pos = 0
    while pos + 8 <= len(data):
        size = int.from_bytes(data[pos:pos+4], 'big')
        kind = data[pos+4:pos+8]
        head = 8
        if size == 1:
            size = int.from_bytes(data[pos+8:pos+16], 'big')
            head = 16
        if size == 0:
            size = len(data) - pos
        assert size >= head and pos+size <= len(data), 'malformed MP4 box'
        yield kind, data[pos+head:pos+size]
        pos += size
    assert pos == len(data), 'trailing partial MP4 box'


def find(data, wanted):
    for kind, body in boxes(data):
        if kind == wanted:
            yield body
        elif kind in (b'moov', b'trak', b'mdia', b'minf', b'stbl'):
            yield from find(body, wanted)


def config(path):
    videos = [body for body in find(path.read_bytes(), b'stsd') if body[12:16] == b'av01']
    assert len(videos) == 1, 'expected one AV1 track'
    assert int.from_bytes(videos[0][4:8], 'big') == 1, 'test targets the single-description writer'
    entries = list(boxes(videos[0][8:]))
    assert len(entries) == 1
    records = [b for k, b in boxes(entries[0][1][78:]) if k == b'av1C']
    assert len(records) == 1 and len(records[0]) >= 4
    extra = records[0]
    assert extra[0] == 0x81 and extra[3] == 0
    return extra


def sequence_payloads(d):
    result = set()
    for packet in d['packets']:
        data = unhex(packet.get('data', ''))
        while data:
            head, data = data[0], data[1:]
            if head & 4:
                data = data[1:]
            size = len(data)
            if head & 2:
                size = 0
                for n in range(8):
                    byte, data = data[0], data[1:]
                    size |= (byte & 127) << (7*n)
                    if not byte & 128:
                        break
            body, data = data[:size], data[size:]
            if (head >> 3) & 15 == 1:
                result.add(body)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--cut', type=Path, default=Path('rust/target/release/smartcut' + ('.exe' if os.name == 'nt' else '')))
    parser.add_argument('--ffmpeg', default=os.environ.get('FFMPEG', 'ffmpeg'))
    parser.add_argument('--ffprobe', default=os.environ.get('FFPROBE', 'ffprobe'))
    parser.add_argument('--fixtures', type=Path, help='reuse these exact input files in a paired run')
    parser.add_argument('--work-dir', type=Path)
    args = parser.parse_args()
    if args.work_dir:
        args.work_dir.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix='smartcut-av1-mp4-', dir=args.work_dir))
    fixtures = args.fixtures.resolve() if args.fixtures else work / 'fixtures'
    fixtures.mkdir(parents=True, exist_ok=True)
    cut = args.cut.resolve()
    report = {'cut': str(cut), 'cut_sha256': sha(cut), 'test_sha256': sha(__file__),
              'commands': [], 'fixtures': [], 'cases': []}

    def save():
        (work/'results.json').write_text(json.dumps(report, indent=2)+'\n', encoding='utf-8')

    def run(command):
        command = list(map(str, command))
        p = subprocess.run(command, capture_output=True, timeout=240)
        report['commands'].append({'command': command, 'exit_code': p.returncode})
        assert p.returncode == 0, f'exit {p.returncode}: {command}\n{p.stderr.decode(errors="replace")[-2500:]}'
        return p

    def probe(path):
        return json.loads(run([args.ffprobe, '-v', 'error', '-select_streams', 'v:0', '-show_streams',
                               '-show_packets', '-show_data', '-show_data_hash', 'sha256', '-of', 'json', path]).stdout)

    def pixels(path):
        text = run([args.ffmpeg, '-v', 'error', '-i', path, '-map', '0:v:0',
                    '-fps_mode', 'passthrough', '-f', 'framemd5', '-']).stdout.decode()
        return [line.rsplit(',', 1)[1].strip() for line in text.splitlines() if line and not line.startswith('#')]

    def check_config(path, row):
        extra = config(path)
        trace = run([args.ffmpeg, '-hide_banner', '-loglevel', 'info', '-i', path, '-map', '0:v:0',
                     '-c', 'copy', '-bsf:v', 'trace_headers', '-f', 'null', '-']).stderr.decode(errors='replace')
        headers = []
        for line in trace.splitlines():
            match = re.search(r'\b(seq_profile|seq_level_idx\[0\]|seq_tier\[0\]|high_bitdepth|twelve_bit|mono_chrome|chroma_sample_position)\s+\S+\s+=\s*(\d+)', line)
            if not match:
                continue
            key, value = match[1], int(match[2])
            if key == 'seq_profile':
                headers.append({})
            headers[-1][key] = value
        assert headers, 'independent CBS parser found no Sequence Header'
        # All generated sources and boundary encoders use profile 0, 4:2:0.
        # AV1 infers subsampling_x/y=1 for this profile; they are not coded bits.
        fields = []
        for header in headers:
            assert header['seq_profile'] == 0
            fields.append(bytes([0x81, header['seq_level_idx[0]'],
                                 header.get('seq_tier[0]', 0) << 7
                                 | header['high_bitdepth'] << 6
                                 | header.get('twelve_bit', 0) << 5
                                 | header.get('mono_chrome', 0) << 4
                                 | 12 | header.get('chroma_sample_position', 0)]))
        row['av1C_prefix'] = extra[:4].hex()
        row['sequence_field_prefixes'] = sorted(set(x.hex() for x in fields))
        assert all(value == extra[:3] for value in fields), 'av1C disagrees with a Sequence Header'

    try:
        report['ffmpeg_version'] = run([args.ffmpeg, '-version']).stdout.decode().splitlines()[0]
        report['ffprobe_version'] = run([args.ffprobe, '-version']).stdout.decode().splitlines()[0]
        sources = []
        for name, encoder, pix_fmt in [('aom8', 'libaom-av1', 'yuv420p'),
                                       ('aom10', 'libaom-av1', 'yuv420p10le'),
                                       ('rav1e', 'librav1e', 'yuv420p')]:
            path = fixtures/(name+'.mp4')
            if not path.exists():
                options = (['-cpu-used', '8', '-crf', '22', '-b:v', '0']
                           if encoder == 'libaom-av1' else ['-speed', '10', '-qp', '70'])
                run([args.ffmpeg, '-v', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=size=192x112:rate=25',
                     '-frames:v', '240', '-an', '-c:v', encoder, *options, '-g', '24', '-pix_fmt', pix_fmt, path])
            sources.append((name, path))
        mkv = fixtures/'rav1e.mkv'
        if not mkv.exists():
            run([args.ffmpeg, '-v', 'error', '-y', '-i', fixtures/'rav1e.mp4', '-map', '0:v:0', '-c', 'copy', mkv])
        sources.append(('rav1e-mkv', mkv))
        finite = fixtures/'svt-level40.mp4'
        if not finite.exists():
            run([args.ffmpeg, '-v', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=size=192x112:rate=25',
                 '-frames:v', '240', '-an', '-c:v', 'libsvtav1', '-preset', '12', '-crf', '28',
                 '-g', '24', '-pix_fmt', 'yuv420p', '-svtav1-params', 'level=40:lp=2', finite])
        chroma = fixtures/'chroma.mp4'
        if not chroma.exists():
            run([args.ffmpeg, '-v', 'error', '-y', '-i', fixtures/'aom8.mp4', '-map', '0:v:0',
                 '-c', 'copy', '-bsf:v', 'av1_metadata=chroma_sample_position=colocated', chroma])
        assert config(finite)[:3] == bytes.fromhex('81080c'), 'expected Level 4.0 fixture'
        assert config(chroma)[:3] == bytes.fromhex('81000e'), 'expected colocated chroma fixture'
        sources.extend([('finite4', finite), ('chroma', chroma)])
        cuts = [('partial', [(13, 227)]), ('multi', [(13, 81), (100, 227)]),
                ('aligned', [(24, 216)]), ('one', [(23, 24)]), ('full', [(0, 240)]),
                ('copy-first', [(0, 227)])]
        for name, source in sources:
            src = probe(source)
            source_pixels = pixels(source)
            packets = sorted(src['packets'], key=lambda p: int(p['pts']))
            source_tb = Fraction(src['streams'][0]['time_base'])
            times = [int(p['pts'])*source_tb for p in packets]
            times.append(times[-1]+int(packets[-1]['duration'])*source_tb)
            assert len(source_pixels) == len(packets) == 240
            report['fixtures'].append({'name': name, 'path': str(source), 'sha256': sha(source)})
            for label, ranges in cuts:
                if name in ('finite4', 'chroma') and label != 'partial':
                    continue
                if label == 'copy-first' and name != 'rav1e':
                    continue
                if source.suffix == '.mkv' and label not in ('partial', 'multi', 'full'):
                    continue
                out = work/(name+'-'+label+source.suffix)
                row = {'name': name+'-'+label, 'input': str(source), 'input_sha256': sha(source),
                       'ranges': ranges, 'output': str(out), 'passed': False}
                report['cases'].append(row)
                try:
                    command = [cut, source, '--no-chapters', '-o', out]
                    for a, b in ranges:
                        command += ['--keep', f'{float(times[a]-times[0]):.12f}-{float(times[b]-times[0]):.12f}']
                    proc = run(command)
                    log = proc.stdout+proc.stderr
                    out.with_suffix(out.suffix+'.log').write_bytes(log)
                    row['full_video_fallback'] = b'The entire selected video is re-encoded' in log
                    if row['full_video_fallback']:
                        assert b'discarded plan' in log and b'copies no video (0%)' in log
                    if source.suffix == '.mkv':
                        assert not row['full_video_fallback'], 'MKV must retain the existing policy'
                    d = probe(out)
                    row['output_sha256'] = sha(out)
                    row['sequence_payload_count'] = len(sequence_payloads(d))
                    tb = Fraction(d['streams'][0]['time_base'])
                    observed = sorted(d['packets'], key=lambda p: int(p['pts']))
                    origin = int(observed[0]['pts'])*tb
                    expected = []
                    duration = Fraction(0)
                    indices = []
                    for a, b in ranges:
                        expected.extend(duration+t-times[a] for t in times[a:b])
                        duration += times[b]-times[a]
                        indices.extend(range(a, b))
                    assert len(observed) == len(expected), 'wrong frame count'
                    assert all(abs(int(p['pts'])*tb-origin-t) <= tb for p, t in zip(observed, expected)), 'display PTS changed'
                    dts = [int(p['dts']) for p in d['packets']]
                    assert all(a < b for a, b in zip(dts, dts[1:])), 'non-monotonic DTS'
                    if out.suffix == '.mp4':
                        assert abs(Fraction(d['streams'][0]['duration'])-duration) <= tb, 'video duration changed'
                    run([args.ffmpeg, '-v', 'error', '-xerror', '-err_detect', 'explode', '-i', out,
                         '-map', '0:v:0', '-f', 'null', '-'])
                    made_pixels = pixels(out)
                    assert len(made_pixels) == len(expected), 'decoded frame count changed'
                    intervals = [(float(a), float(b)) for a, b in
                                 re.findall(rb'^\s*copy\s+([\d.]+)\s+->\s*([\d.]+)', log, re.M)]
                    copied = [] if row['full_video_fallback'] else [
                        (oi, si) for oi, si in enumerate(indices)
                        if any(a+0.001 < float(times[si]-times[0]) < b-0.001 for a, b in intervals)]
                    for oi, si in copied:
                        assert observed[oi]['data_hash'] == packets[si]['data_hash'], 'copy payload changed'
                        assert made_pixels[oi] == source_pixels[si], 'copy pixels changed'
                    row['copied_interior_frames_checked'] = len(copied)
                    row['identical_source_packets'] = sum(p['data_hash'] == packets[si]['data_hash']
                                                          for p, si in zip(observed, indices))
                    if label == 'full':
                        assert [p['data_hash'] for p in observed] == [p['data_hash'] for p in packets], 'pure copy changed packets'
                        assert made_pixels == source_pixels
                    elif label != 'one' and not row['full_video_fallback']:
                        assert copied, 'test did not exercise a copied middle'
                    if out.suffix == '.mp4':
                        check_config(out, row)
                        if name in ('finite4', 'rav1e') and label in ('partial', 'multi', 'copy-first'):
                            assert not row['full_video_fallback'], 'level-only seam discarded the copied middle'
                            assert config(source)[:3] == config(out)[:3], 'level must match the copied source'
                            # These CFR fixtures have exact millisecond cut points. Include the
                            # very first copied key packet, where the source Sequence Header lives.
                            for oi, si in enumerate(indices):
                                if any(a <= float(times[si]-times[0]) < b for a, b in intervals):
                                    assert observed[oi]['data_hash'] == packets[si]['data_hash'], 'copied header changed'
                                    assert made_pixels[oi] == source_pixels[si], 'copied picture changed'
                        if name == 'chroma':
                            assert row['full_video_fallback'], 'incompatible chroma must retry'
                            assert log.count(b'The entire selected video is re-encoded') == 1
                    row['frames'] = len(observed)
                    row['passed'] = True
                    print('ok', row['name'], flush=True)
                except (AssertionError, KeyError, ValueError, subprocess.TimeoutExpired) as error:
                    row['error'] = str(error)
                    print('FAIL', row['name'], str(error)[:200], flush=True)
                save()
    finally:
        report['passed'] = sum(row['passed'] for row in report['cases'])
        report['total'] = len(report['cases'])
        save()
        print(f'{report["passed"]}/{report["total"]} passed; evidence: {work}', flush=True)
    return 0 if report['total'] == 21 and report['passed'] == 21 else 1


if __name__ == '__main__':
    raise SystemExit(main())
