"""Container SAR regression, independent of AV1 guards and clock changes.

Requires Python's standard library and FFmpeg with libx264, libx265 and
libaom-av1. Retains generated media, commands and hashes in a new directory.
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cut', type=Path, default=Path('rust/target/release/smartcut' + ('.exe' if os.name == 'nt' else '')))
    parser.add_argument('--ffmpeg', default=os.environ.get('FFMPEG', 'ffmpeg'))
    parser.add_argument('--ffprobe', default=os.environ.get('FFPROBE', 'ffprobe'))
    parser.add_argument('--work-dir', type=Path)
    parser.add_argument('--fixtures', type=Path, help='reuse/generate source clips here for exact paired comparisons')
    parser.add_argument('--outline-probe', type=Path, help='existing conformdiag example; defaults beside the CLI under examples/')
    args = parser.parse_args()
    work = Path(tempfile.mkdtemp(prefix='smartcut-sar-', dir=args.work_dir))
    fixtures = args.fixtures.resolve() if args.fixtures else work
    fixtures.mkdir(parents=True, exist_ok=True)
    cut = args.cut.resolve()
    outline_probe = (args.outline_probe or cut.parent/'examples'/('conformdiag.exe' if os.name == 'nt' else 'conformdiag')).resolve()
    commands, cases, outlines, overrides = [], [], [], []

    def sha(path):
        with path.open('rb') as stream:
            return hashlib.file_digest(stream, 'sha256').hexdigest()

    def run(command):
        command = list(map(str, command))
        p = subprocess.run(command, capture_output=True, timeout=180)
        commands.append({'command': command, 'exit_code': p.returncode})
        assert p.returncode == 0, f'exit {p.returncode}: {command}\n{p.stderr.decode(errors="replace")[-2000:]}'
        return p

    def probe(path):
        data = run([args.ffprobe, '-v', 'error', '-select_streams', 'v:0', '-count_frames',
                    '-show_entries', 'stream=codec_name,width,height,sample_aspect_ratio,nb_read_frames',
                    '-of', 'json', path]).stdout
        return json.loads(data)['streams'][0]

    def sar(stream):
        value = stream.get('sample_aspect_ratio', 'N/A')
        return Fraction(value.replace(':', '/')) if value not in ('N/A', '0:1') else Fraction(1)

    def header_sars(path):
        # Inspect every SPS, including the ones written by boundary encoders;
        # ffprobe's stream SAR alone can hide a change at a seam.
        log = run([args.ffmpeg, '-v', 'info', '-i', path, '-map', '0:v:0',
                   '-c', 'copy', '-bsf:v', 'trace_headers', '-f', 'null', '-']).stderr.decode(errors='replace')
        (work/(path.name+'.headers.log')).write_text(log, encoding='utf-8')
        fields = re.findall(r'\b(aspect_ratio_idc|sar_width|sar_height)\s+[01]+\s*=\s*(\d+)', log)
        ratios = []
        index = 0
        while index < len(fields):
            field, value = fields[index]
            assert field == 'aspect_ratio_idc', (path, fields[index:])
            value = int(value)
            if value == 255:
                assert [v[0] for v in fields[index+1:index+3]] == ['sar_width', 'sar_height'], path
                ratios.append(Fraction(int(fields[index+1][1]), int(fields[index+2][1])))
                index += 3
            else:
                # These fixtures deliberately cover square and non-square SPS SAR.
                assert value in (1, 14), (path, value)
                ratios.append({1: Fraction(1), 14: Fraction(4, 3)}[value])
                index += 1
        assert ratios, ('no SPS SAR found', path)
        assert len(ratios) == len(re.findall(r'\] Sequence Parameter Set\s*$', log, re.M)), ('SPS without SAR', path)
        return ratios

    encoders = {
        'h264': ['libx264', '-preset', 'medium', '-crf', '14'],
        'hevc': ['libx265', '-preset', 'medium', '-crf', '18', '-x265-params', 'log-level=error:pools=none'],
        'av1': ['libaom-av1', '-cpu-used', '8', '-crf', '22', '-b:v', '0'],
    }
    cuts = {'copy': ('0-4.8', 120), 'mixed': ('0.52-4.28', 94), 'encoded': ('0.52-0.76', 6)}
    for codec, encoder in encoders.items():
        for ratio in ('4/3', '1/1'):
            mp4 = fixtures/f'{codec}-sar{ratio.replace("/", "_")}.mp4'
            if not mp4.exists():
                run([args.ffmpeg, '-v', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=size=192x112:rate=25',
                     '-frames:v', '120', '-an', '-vf', f'setsar={ratio}', '-c:v', *encoder,
                     '-g', '24', '-pix_fmt', 'yuv420p', mp4])
            for container in ('.mp4', '.mkv'):
                source = mp4.with_suffix(container)
                if container == '.mkv' and not source.exists():
                    run([args.ffmpeg, '-v', 'error', '-y', '-i', mp4, '-map', '0:v:0', '-c', 'copy', source])
                original = probe(source)
                assert sar(original) == Fraction(ratio), ('fixture SAR', source, original)
                shape = run([outline_probe, '--shape', source]).stdout.decode(errors='replace')
                found = re.search(r'\bsar=([\d.]+)', shape)
                correct = found is not None and abs(float(found[1])-float(Fraction(ratio))) < 0.0001
                outlines.append({'input': source.name, 'input_sha256': sha(source),
                                 'expected_sar': ratio, 'passed': correct, 'shape': shape})
                print('ok' if correct else 'FAIL', 'outline', codec, ratio, container, flush=True)
                for mode, (bounds, frames) in cuts.items():
                    out = work/f'{source.stem}-{mode}{container}'
                    row = {'codec': codec, 'container': container, 'sar': ratio, 'mode': mode,
                           'input': source.name, 'input_sha256': sha(source), 'output': out.name}
                    try:
                        log = run([cut, source, '--no-chapters', '--keep', bounds, '-o', out])
                        out.with_suffix(container+'.log').write_bytes(log.stdout+log.stderr)
                        made = probe(out)
                        assert sar(made) == Fraction(ratio), ('output SAR', made)
                        assert (made['width'], made['height']) == (192, 112), made
                        assert made['codec_name'] == codec, made
                        assert int(made['nb_read_frames']) == frames, made
                        run([args.ffmpeg, '-v', 'error', '-xerror', '-err_detect', 'explode', '-i', out,
                             '-map', '0:v:0', '-f', 'null', '-'])
                        row.update(passed=True, output_sha256=sha(out))
                    except (AssertionError, subprocess.TimeoutExpired) as error:
                        row.update(passed=False, error=str(error))
                        if out.exists():
                            row['output_sha256'] = sha(out)
                    cases.append(row)
                    print('ok' if row['passed'] else 'FAIL', codec, ratio, container, mode,
                          row.get('error', ''), flush=True)

    # A display-aspect override must not leak into the boundary SPS. The first
    # fixture uses the reviewer's exact 30 s / 640x360 H.264 generation command.
    # Also cover HEVC, a non-square coded SAR, both output containers, and a
    # later TS remux where the container's display override no longer applies.
    for codec in ('h264', 'hevc'):
        for coded_sar in ('1/1', '4/3'):
            stem = f'override-{codec}-{coded_sar.replace("/", "_")}'
            original = fixtures/(stem+'.mp4')
            source = fixtures/(stem+'.mkv')
            if not original.exists():
                options = [] if coded_sar == '1/1' else ['-vf', f'setsar={coded_sar}']
                encoder = ['libx264'] if codec == 'h264' else ['libx265', '-x265-params', 'log-level=error:pools=none']
                run([args.ffmpeg, '-v', 'error', '-y', '-f', 'lavfi', '-i',
                     'testsrc2=rate=25:size=640x360', '-t', '30', *options,
                     '-c:v', *encoder, '-g', '50', original])
            if not source.exists():
                run([args.ffmpeg, '-v', 'error', '-y', '-i', original,
                     '-c', 'copy', '-aspect', '4:3', source])
            expected = Fraction(coded_sar)
            source_sars = header_sars(source)
            assert all(s == expected for s in source_sars), ('fixture SPS SAR', source, source_sars)
            assert sar(probe(source)) == Fraction(3, 4), ('fixture container SAR', source)
            shape = run([outline_probe, '--shape', source]).stdout.decode(errors='replace')
            found = re.search(r'\bsar=([\d.]+)', shape)
            correct = found is not None and abs(float(found[1])-0.75) < 0.0001
            outlines.append({'input': source.name, 'input_sha256': sha(source),
                             'expected_sar': '3/4', 'passed': correct, 'shape': shape})
            print('ok' if correct else 'FAIL', 'outline override', codec, coded_sar, flush=True)
            for container in ('.mp4', '.mkv'):
                out = work/(stem+'-cut'+container)
                remux = work/(out.name+'.ts')
                row = {'codec': codec, 'container': container, 'coded_sar': coded_sar,
                       'display_sar': '3/4', 'input': source.name, 'input_sha256': sha(source),
                       'source_sps_sars': [str(s) for s in source_sars], 'output': out.name}
                try:
                    log = run([cut, source, '--keep', '3.3-12.1', '--keep', '15-22.7', '-o', out])
                    text = (log.stdout+log.stderr).decode(errors='replace')
                    (work/(out.name+'.log')).write_text(text, encoding='utf-8')
                    assert re.search(r'^\s*copy\s+', text, re.M), 'fixture must exercise copied pictures'
                    assert re.search(r'^\s*reencode\s+', text, re.M), 'fixture must exercise boundary encoders'
                    made = probe(out)
                    assert sar(made) == Fraction(3, 4), ('output container SAR', made)
                    assert (made['width'], made['height'], made['codec_name']) == (640, 360, codec), made
                    run([args.ffmpeg, '-v', 'error', '-xerror', '-err_detect', 'explode', '-i', out,
                         '-map', '0:v:0', '-f', 'null', '-'])
                    values = header_sars(out)
                    row['output_sps_sars'] = [str(s) for s in values]
                    # Inspect the TS even when the original output has a bad SPS,
                    # so the retained evidence shows the downstream effect too.
                    run([args.ffmpeg, '-v', 'error', '-y', '-i', out, '-map', '0:v:0', '-c', 'copy', remux])
                    remux_sars = header_sars(remux)
                    row['ts_sps_sars'] = [str(s) for s in remux_sars]
                    row['ts_sha256'] = sha(remux)
                    # The FFmpeg 7.1 HEVC TS remux also has a strict-decode
                    # failure with unmodified v0.8.39. Check its SAR headers
                    # here without making this suite a TS decoding fix.
                    if codec == 'h264':
                        run([args.ffmpeg, '-v', 'error', '-xerror', '-err_detect', 'explode', '-i', remux,
                             '-map', '0:v:0', '-f', 'null', '-'])
                    assert all(s == expected for s in values), ('output SPS SAR changes', row['output_sps_sars'])
                    assert all(s == expected for s in remux_sars), ('remux SPS SAR changes', row['ts_sps_sars'])
                    row.update(passed=True, frames=int(made['nb_read_frames']))
                except (AssertionError, subprocess.TimeoutExpired) as error:
                    row.update(passed=False, error=str(error))
                if out.exists():
                    row['output_sha256'] = sha(out)
                overrides.append(row)
                print('ok' if row['passed'] else 'FAIL', 'override', codec, coded_sar, container,
                      row.get('error', ''), flush=True)
    record = {'cut': str(cut), 'cut_sha256': sha(cut), 'generator_sha256': sha(Path(__file__)),
              'outline_probe_sha256': sha(outline_probe), 'fixture_directory': str(fixtures), 'commands': commands,
              'cases': cases, 'outline_checks': outlines, 'override_cases': overrides}
    (work/'results.json').write_text(json.dumps(record, indent=2)+'\n', encoding='utf-8')
    checks = cases + outlines + overrides
    failures = sum(not c['passed'] for c in checks)
    print(f'{len(checks)-failures} passed, {failures} failed ({len(cases)+len(overrides)} exports, {len(outlines)} outlines); evidence: {work}', flush=True)
    raise SystemExit(1 if failures else 0)


if __name__ == '__main__':
    main()
