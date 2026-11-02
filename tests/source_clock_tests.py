"""Source clocks and range-end coverage, using real files and the built CLI.

Python standard library only. All inputs, commands, hashes and results are
kept in a fresh directory; --fixtures reuses exactly the same input bytes.
The 720p reproducer deliberately keeps the reviewer's command unchanged.
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
import time


def sha(path):
    with Path(path).open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--cut', type=Path, default=Path('rust/target/release/smartcut' + ('.exe' if os.name == 'nt' else '')))
    ap.add_argument('--ffmpeg', default='ffmpeg')
    ap.add_argument('--ffprobe', default='ffprobe')
    ap.add_argument('--work-dir', type=Path, default=Path(tempfile.gettempdir()))
    ap.add_argument('--fixtures', type=Path)
    ap.add_argument('--review-source', type=Path)
    args = ap.parse_args()
    args.work_dir.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix='smartcut-source-clock-', dir=args.work_dir)).resolve()
    fx = args.fixtures.resolve() if args.fixtures else root / 'fixtures'
    fx.mkdir(parents=True, exist_ok=True)
    result = {'test_sha256': sha(__file__), 'cut': str(args.cut.resolve()),
              'cut_sha256': sha(args.cut), 'commands': [], 'fixtures': {}, 'cases': [],
              'source_pixel_oracle': 'Generated MP4s: ignore_editlist=1 decodes all generated packets; outputs use normal demux/decoding.'}

    def run(cmd, key, check=True):
        cmd = list(map(str, cmd))
        start = time.monotonic()
        p = subprocess.run(cmd, capture_output=True)
        result['commands'].append({'command': cmd, 'exit_code': p.returncode,
                                   'seconds': time.monotonic() - start})
        (root / (key + '.log')).write_bytes(p.stdout + b'\nSTDERR\n' + p.stderr)
        if check and p.returncode:
            raise RuntimeError(f'{key}: {p.stderr.decode(errors="replace")[-1800:]}')
        return p

    def inspect(path, key):
        p = run([args.ffprobe, '-v', 'error', '-select_streams', 'v:0',
                 '-show_streams', '-show_format', '-show_packets', '-show_data_hash', 'sha256',
                 '-of', 'json', path], key)
        data = json.loads(p.stdout)
        data['sorted'] = sorted(data['packets'], key=lambda x: int(x['pts']))
        tb = Fraction(data['streams'][0]['time_base'])
        data['times'] = [float(int(p['pts']) * tb) for p in data['sorted']]
        codec = data['streams'][0]['codec_name']
        if codec in ('h264', 'hevc'):
            # SmartCut may repeat parameter sets at an entry point. Compare
            # the compressed pictures (VCL NALs) separately from those headers.
            types = '1-5' if codec == 'h264' else '0-31'
            hashes = run([args.ffmpeg, '-v', 'error', '-i', path, '-map', '0:v:0',
                          '-c:v', 'copy', '-bsf:v', 'filter_units=pass_types=' + types,
                          '-f', 'framehash', '-hash', 'sha256', '-'], key + '-vcl')
            hashes = [line.rsplit(',', 1)[1].strip() for line in hashes.stdout.decode().splitlines()
                      if line and not line.startswith('#')]
            assert len(hashes) == len(data['packets'])
            data['picture_hashes'] = [h for _, h in sorted(zip(data['packets'], hashes),
                                                         key=lambda item: int(item[0]['pts']))]
        else:
            data['picture_hashes'] = [p['data_hash'] for p in data['sorted']]
        return data

    def pixels(path, key, source=False):
        # FFmpeg 7.1's muxer can end the edit list at the final PTS of these
        # reordered VFR generators, hiding their last encoded picture from
        # normal decoding. All 240 generated packets are the reference here.
        # Never bypass an output's edit list: it must decode normally.
        options = ['-ignore_editlist', '1'] if source and path.suffix == '.mp4' else []
        p = run([args.ffmpeg, '-v', 'error', *options, '-i', path, '-map', '0:v:0',
                 '-fps_mode', 'passthrough', '-f', 'framemd5', '-'], key)
        return [line.rsplit(',', 1)[1].strip() for line in p.stdout.decode().splitlines()
                if line and not line.startswith('#')]

    def generate(name, cmd):
        path = fx / name
        if not args.fixtures:
            run([args.ffmpeg, '-hide_banner', '-v', 'error', '-y', *cmd, path],
                'generate-' + name)
        result['fixtures'][name] = {'path': str(path), 'sha256': sha(path)}
        return path

    base = ['-f', 'lavfi', '-i', 'testsrc2=size=192x112:rate=25', '-frames:v', '240']
    codec_options = {
        'h264': ['-c:v', 'libx264', '-preset', 'veryfast', '-crf', '20',
                 '-x264-params', 'keyint=24:min-keyint=24:scenecut=0:bframes=3'],
        'hevc': ['-c:v', 'libx265', '-preset', 'ultrafast', '-crf', '24',
                 '-x265-params', 'keyint=24:min-keyint=24:scenecut=0:bframes=3:log-level=error'],
        'av1': ['-c:v', 'libaom-av1', '-cpu-used', '8', '-crf', '32', '-b:v', '0', '-g', '24'],
    }
    specs = []
    for codec, options in codec_options.items():
        mp4 = generate(f'{codec}-vfr.mp4', base + ['-vf',
            'setpts=N+floor(N/3)', '-fps_mode', 'passthrough',
            *options, '-video_track_timescale', '12800'])
        mkv = generate(f'{codec}-vfr.mkv', ['-i', mp4, '-map', '0:v:0', '-c', 'copy'])
        specs.extend([(mp4, 'vfr'), (mkv, 'vfr')])
    for codec in ('h264', 'hevc'):
        for label, rate, timescale in [('25', '25', '12800'), ('ntsc', '24000/1001', '24000')]:
            mp4 = generate(f'{codec}-{label}.mp4', ['-f', 'lavfi', '-i',
                f'testsrc2=size=192x112:rate={rate}', '-frames:v', '240',
                *codec_options[codec], '-video_track_timescale', timescale])
            mkv = generate(f'{codec}-{label}.mkv', ['-i', mp4, '-map', '0:v:0', '-c', 'copy'])
            specs.extend([(mp4, 'cfr'), (mkv, 'coarse' if label == 'ntsc' else 'cfr')])
    hold = generate('holds.mp4', ['-f', 'lavfi', '-i', 'testsrc2=size=192x112:rate=30',
        '-frames:v', '180', '-vf', r'setpts=N+gte(N\,60)*60+gte(N\,120)*60',
        '-fps_mode', 'passthrough', *codec_options['h264'],
        '-video_track_timescale', '30000'])
    hold_mkv = generate('holds.mkv', ['-i', hold, '-map', '0:v:0', '-c', 'copy'])
    if args.fixtures and not (fx / 'review.mp4').exists():
        saved = json.loads((fx / 'fixtures.json').read_text(encoding='utf-8'))
        review = Path(saved['review.mp4']['path'])
        result['fixtures']['review.mp4'] = {'path': str(review), 'sha256': sha(review)}
    elif args.review_source and not args.fixtures:
        review = args.review_source.resolve()
        result['fixtures']['review.mp4'] = {'path': str(review), 'sha256': sha(review)}
    else:
        review = generate('review.mp4', ['-f', 'lavfi', '-i',
            'testsrc2=rate=24000/1001:size=1280x720', '-t', '90', '-vf',
            r'setpts=(N*1001/24000-if(eq(mod(N\,300)\,299)\,0.025\,0))/TB',
            '-fps_mode', 'passthrough', '-c:v', 'libx264', '-g', '120',
            '-video_track_timescale', '1000'])
    # A reference to an existing exact reproducer also survives --fixtures.
    manifest = fx / 'fixtures.json'
    if args.fixtures:
        saved = json.loads(manifest.read_text(encoding='utf-8'))
        if not (fx / 'review.mp4').exists():
            review = Path(saved['review.mp4']['path'])
            result['fixtures']['review.mp4'] = {'path': str(review), 'sha256': sha(review)}
        assert saved == result['fixtures'], 'fixture bytes changed'
    else:
        manifest.write_text(json.dumps(result['fixtures'], indent=2), encoding='utf-8')

    sources = {}
    for path in [p for p, _ in specs] + [hold, hold_mkv, review]:
        sources[path] = inspect(path, 'source-' + path.name)
        sources[path]['pixels'] = pixels(path, 'pixels-' + path.name, source=True)
        assert len(sources[path]['pixels']) == len(sources[path]['times']), path

    def save():
        result['passed'] = sum(c['passed'] for c in result['cases'])
        result['total'] = len(result['cases'])
        (root / 'results.json').write_text(json.dumps(result, indent=2), encoding='utf-8')

    def case(path, name, ranges, expected, end, suffix=None, kind='', extra=()):
        src = sources[path]
        out = root / (name + (suffix or path.suffix))
        command = [args.cut.resolve(), path, '-o', out]
        for a, b in ranges:
            command += ['--keep', f'{a:.12f}-{b:.12f}']
        command += list(extra)
        row = {'name': name, 'kind': kind, 'input': str(path), 'input_sha256': sha(path),
               'ranges': ranges, 'output': str(out), 'issues': []}
        try:
            proc = run(command, name, check=False)
            row['exit_code'] = proc.returncode
            if proc.returncode:
                raise RuntimeError('export failed')
            got = inspect(out, name + '-probe')
            pts = got['times']
            count = len(expected)
            row['frames'] = len(pts)
            row['expected_frames'] = count
            if len(pts) != count:
                row['issues'].append('frame_count')
            tolerance = max(1e-6, float(Fraction(got['streams'][0]['time_base']))) + 1e-7
            error = max((abs(t - at) for t, (_, at) in zip(pts, expected)), default=0.0)
            row['max_display_error_seconds'] = error
            if error > tolerance:
                row['issues'].append('display_pts')
            packets = got['packets']
            dts = [int(p['dts']) for p in packets if 'dts' in p]
            # Matroska stores PTS, not DTS. On the reviewer's duplicate-PTS
            # source its millisecond clock can give equal inferred DTS.
            row['equal_dts'] = sum(b == a for a, b in zip(dts, dts[1:]))
            row['dts_monotonic'] = all((b >= a if out.suffix == '.mkv' else b > a)
                                       for a, b in zip(dts, dts[1:]))
            if not row['dts_monotonic']:
                row['issues'].append('dts')
            strict = run([args.ffmpeg, '-v', 'error', '-xerror', '-err_detect', 'explode',
                          '-i', out, '-map', '0:v:0', '-f', 'null', '-'], name + '-strict', check=False)
            row['strict_decode'] = strict.returncode == 0
            if not row['strict_decode']:
                row['issues'].append('strict_decode')
            # These outputs contain video only. Check the movie duration as
            # well as every display PTS: decoding alone cannot detect a
            # final hold shortened by the muxer's sample-table durations.
            duration = float(got['format']['duration'])
            row['presentation_duration'] = duration
            row['stream_duration'] = got['streams'][0].get('duration')
            row['expected_duration'] = end
            if abs(duration - end) > max(tolerance, .001001):
                row['issues'].append('last_picture_duration')
            if out.suffix == '.mp4' and abs(float(row['stream_duration']) - end) > max(tolerance, .001001):
                row['issues'].append('video_track_duration')
            log = proc.stdout + proc.stderr
            intervals = [(float(a), float(b)) for a, b in
                         re.findall(rb'^\s*copy\s+([\d.]+)\s+->\s*([\d.]+)', log, re.M)]
            decoded = pixels(out, name + '-pixels')
            row['decoded_frames'] = len(decoded)
            if len(decoded) != count:
                row['issues'].append('decoded_count')
            copied = [i for i, (source_index, _) in enumerate(expected)
                      if any(a + .001 < src['times'][source_index] < b - .001 for a, b in intervals)]
            row['copy_intervals'] = intervals
            row['copy_interior_frames'] = len(copied)
            row['copied_packet_changes'] = [i for i in copied if i >= len(got['sorted']) or
                got['sorted'][i]['data_hash'] != src['sorted'][expected[i][0]]['data_hash']]
            row['copied_picture_errors'] = [i for i in copied if i >= len(got['picture_hashes']) or
                got['picture_hashes'][i] != src['picture_hashes'][expected[i][0]]]
            row['copied_pixel_errors'] = [i for i in copied if i >= len(decoded) or
                decoded[i] != src['pixels'][expected[i][0]]]
            if row['copied_picture_errors'] or row['copied_pixel_errors']:
                row['issues'].append('copied_picture')
            row['output_sha256'] = sha(out)
            row['time_base'] = got['streams'][0]['time_base']
            if kind == 'review' and len(ranges) == 2:
                seam = sum(20 <= t < 38 for t in src['times'])
                row['seam_gap_seconds'] = pts[seam] - pts[seam-1]
                if not .040 <= row['seam_gap_seconds'] <= .043:
                    row['issues'].append('four_ms_regression')
        except Exception as exc:
            row['issues'].append('exception')
            row['error'] = str(exc)
        row['passed'] = not row['issues']
        result['cases'].append(row)
        save()
        print(('PASS' if row['passed'] else 'FAIL'), name, row['issues'], flush=True)

    cuts = {'partial': [(13, 227)], 'multi': [(13, 81), (100, 227)],
            'aligned': [(24, 216)], 'one': [(23, 24)], 'full': [(0, 240)]}
    for path, kind in specs:
        src = sources[path]
        stream = src['streams'][0]
        # The generating cadence gives the final picture 40 ms. A B-frame
        # MP4's stream.duration is a decode duration, not its displayed end.
        final = ((240 * 1001 / 24000 if 'ntsc' in path.name else 9.6) if kind != 'vfr'
                 else 12.76)
        times = src['times'] + [final]
        for label, indices in cuts.items():
            expected, ranges, offset = [], [], 0.0
            for a, b in indices:
                ranges.append((times[a], times[b]))
                expected.extend((i, offset + times[i] - times[a]) for i in range(a, b))
                offset += times[b] - times[a]
            case(path, path.stem + '-' + path.suffix[1:] + '-' + label,
                 ranges, expected, offset, kind=kind)

    for path in (hold, hold_mkv):
        times = sources[path]['times']
        for label, ranges in [('ends-in-hold', [(0.6, 2.7)]),
                              ('inside-hold', [(2.2, 2.7)]),
                              ('joins-held-ranges', [(0.6, 2.7), (4.4, 6.8)])]:
            expected, offset = [], 0.0
            for a, b in ranges:
                indices = [i for i, t in enumerate(times) if a - 1e-8 <= t < b - 1e-8]
                if not indices:
                    indices = [max(i for i, t in enumerate(times) if t < a)]
                expected.extend((i, offset + max(times[i], a) - a) for i in indices)
                offset += b - a
            case(path, label + '-' + path.suffix[1:], ranges, expected, offset, kind='hold')

    for label, ranges in [('multi', [(20, 38), (60, 75)]), ('single', [(20, 38)])]:
        times = sources[review]['times']
        expected, offset = [], 0.0
        for a, b in ranges:
            indices = [i for i, t in enumerate(times) if a <= t < b]
            first = times[indices[0]]
            expected.extend((i, offset + times[i] - first) for i in indices)
            # This normal 23.976 picture keeps its coded floor, including
            # the reviewer's picture at 37.996 s before a bound of 38 s.
            offset += max(b - first, times[indices[-1]] - first + 1001/24000)
        for suffix in ('.mp4', '.mkv'):
            case(review, 'review-' + label + suffix, ranges, expected, offset,
                 suffix=suffix, kind='review')
    save()
    print(f"{result['passed']}/{result['total']} passed; {root}", flush=True)
    return 0 if result['passed'] == result['total'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
