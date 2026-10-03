# Distribution

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](distribution.ja.md)

## AppImage

```bash
gui/jammy/setup.sh /path/to/root          # once: the 22.04 root and its FFmpeg
JAMMY=/path/to/root gui/jammy/build.sh    # AppImage + tar.gz
# -> <root>/build/smartcut/gui/src-tauri/target/release/bundle/appimage/
```

The artifact is **176.4 MB and carries 633 shared libraries**. WebKitGTK 4.1 is in
there, and so are `libavcodec`, `libavformat`, `libavutil`, `libavfilter`, `libswscale`
and `libswresample` — so **the machine running it does not need ffmpeg installed**.
linuxdeploy follows `ldd` and picks them up.

| Condition | Value |
|---|---|
| glibc required | **2.35 or newer** (Ubuntu 22.04 / Debian 12 / Fedora 36 and later) |
| FUSE required | Yes, or `--appimage-extract-and-run` |
| ALSA required | `libasound.so.2` (not bundled; see below) |
| Build environment | Ubuntu 22.04, glibc 2.35, FFmpeg 7.1.5 built from source |

glibc is the one thing that cannot be bundled — an inherent AppImage constraint — so the
system the AppImage is built on sets the floor. Up to 0.8.11 that was the Debian 13
development VM, and the floor was 2.39.

### Built on Ubuntu 22.04

The [AppImage catalog](https://appimage.github.io) requires an AppImage to run on the
oldest Ubuntu LTS still supported, and tests it on 22.04. So since 0.8.12 the AppImage
and the tar.gz are built in an Ubuntu 22.04 root. `gui/jammy/setup.sh` makes it without
root privileges — a user namespace (`unshare --map-auto`) lets apt and dpkg run
unmodified — and installs nothing on the host.

22.04 has no FFmpeg 7.1, and its encoders are years behind (SVT-AV1 0.9, x265 3.5), so
FFmpeg 7.1.5 and every external library SmartCut reaches through it are built from
source, **at the versions Debian 13 ships**:

| Library | Version | Why |
|---|---|---|
| x264 | 0.164 (31e19f9) | H.264 re-encodes and proxies |
| x265 | 4.1 | HEVC re-encodes |
| SVT-AV1 / rav1e / libaom | 2.3.0 / 0.7.1 / 3.12.1 | AV1 re-encodes, tried in that order |
| libvpx | 1.15.0 | VP9 re-encodes |
| dav1d | 1.5.1 | AV1 decoding; FFmpeg's own AV1 decoder needs hwaccel |
| libvpl, nv-codec-headers | 2.14.0, 12.2.72.0 | QSV and NVENC proxies |
| libopus, libmp3lame, libvorbis | from 22.04 | audio re-encodes that keep the codec |

Every native codec, container and protocol stays in. SmartCut picks decoders from the
input and muxers from the output's extension, so a trimmed list would turn into files
that no longer open. VAAPI, VDPAU, OpenCL and Vulkan are left out: nothing uses them, and
each would pull in libraries of its own.

Since 0.8.14 the `.deb` is built in the same root and carries the FFmpeg made there
(below). `build-linux.sh` with no argument makes the AppImage, the tar.gz and the deb.

**The output matches the Debian 13 build.** Ten cuts of eight recordings (MPEG-2, H.264
in TS and MP4, HEVC, AV1, VP9, two BDAV clips; two of the cuts change container), run on
the same VM with the 0.8.11 Debian build and the 0.8.12 tar.gz, gave byte-identical
files for six and identical packets for a seventh (MKV writes a random segment UID). HEVC, AV1 and
VP9 differ in 4–5 packets out of about 1,000, but decode to identical pictures (PSNR
infinite between the two): the encoders write their compiler into the stream. Across
machines the re-encoded pictures differ anyway, since x264 and x265 depend on the thread
count.

### AppRun.wrapped was 0770

Up to 0.8.11, `AppRun.wrapped` inside the AppImage had mode 0770. Tauri saves the tools
it downloads that way, and linuxdeploy copied the AppRun it was handed along with its
mode. squashfs stores files owned by root, so a user who mounts the image without the
AppImage runtime — `firejail --appimage`, which is how the catalog tests — cannot run the
app. The runtime's own FUSE mount presents every file as the caller's, which is why it
went unnoticed. `build-linux.sh` now fixes the cached copy and stops if any file in the
AppDir can be run only by its owner. The 0.8.11 AppImage and tar.gz were rebuilt with
the fix and replaced on 2026-09-28; the files are otherwise identical.

### Why `NO_STRIP=1`

Without it, linuxdeploy used to die with `Strip call failed` on every library it bundles
(`failed to run linuxdeploy`). **As of 2026-08-28 that no longer reproduces**, and a
plain build works.

**The artifact is the same size either way.** Building v0.1.1 under both conditions gave
184,515,064 bytes both times; the md5 differs because of squashfs timestamps, but there
are no bytes for strip to remove. Debian's shared libraries ship stripped already. Since
there is no price to pay, `NO_STRIP=1` stays.

### ALSA is not bundled

Adding audio playback (cpal) made the binary require `libasound.so.2` and `libjack.so.0`
**directly through DT_NEEDED** rather than dlopen. Both are on the AppImage exclude list
that linuxdeploy consults, so they fall outside the `ldd`-following bundling and **the
running system's copies are used**.

libasound2 is present on essentially any desktop Linux, and dragging ALSA in tends to
make things *more* fragile across environments, so the exclusion stays. What is bundled
can be checked by extracting:

```bash
./SmartCut_0.8.21_amd64.AppImage --appimage-extract >/dev/null
ldd squashfs-root/usr/bin/smartcut | grep -E 'asound|jack|pulse'
# libasound.so.2 / libjack.so.0 -> /lib/x86_64-linux-gnu/...   (system)
# libpulse.so.0                 -> squashfs-root/usr/bin/../lib/...  (bundled)
```

### What was verified

Behaviour has been checked on the AppImage itself: opening material, scanning (266
thumbnails, 55 scene points), `Ctrl+D` commercial detection, cutting, and the lossless
badge switching all work.

Library resolution can be confirmed on another machine too. Start it with `DISPLAY`
unset and it gets as far as "cannot initialise GTK" before dying. A missing library would
stop it earlier, so getting that far means the dependencies are satisfied.

On a build made after audio playback was added, the AppImage itself was used to open
`mpeg2.ts` (41 lossless points, 720x480, 29.97 fps, audio present), scan 41 thumbnails
and play with `Space`. Playback advanced 117 frames in 4 seconds, with no ALSA-related
errors and no panics.

The 0.8.12 AppImage was started in a bare Ubuntu 22.04 root holding only the packages
the catalog's test installs (Xvfb, icewm, libasound2, libjack0, Mesa, a CJK font; no
bubblewrap, no GTK). It opened a BDAV clip in the editor with thumbnails, scene points
and level meters. The catalog's `check-libc.sh` reports `GLIBC_2.35` and a static
runtime, and `appdir-lint.sh` finds no fatal issues.

## tar.gz and deb

```bash
JAMMY=/path/to/root gui/jammy/build.sh   # AppImage, tar.gz and deb (Ubuntu 22.04 root)
# -> gui/src-tauri/target/release/bundle/linux/SmartCut-0.8.21-linux-x86_64.tar.gz
# -> gui/src-tauri/target/release/bundle/linux/smartcut_0.8.21_amd64.deb
```

One build of SmartCut, packed two ways. Both install **the GUI as `smartcut` and the
command-line version as `smartcut-cli`**. The program is called SmartCut; what you type
is `smartcut`.

The cargo crate is named `gui`, so left alone Tauri installs it straight to
`/usr/bin/gui` — not a name anyone should be occupying. `mainBinaryName` in
`tauri.conf.json` pins it to `smartcut` (since 0.2.0; before that it was set for Windows
only). The bundle *files* Tauri writes are named after `productName` instead —
`SmartCut_0.8.21_amd64.deb` and the like — which is why they and the deb's package name
`smartcut` differ. `build-linux.sh` reads both out of `tauri.conf.json`.

| Artifact | Size | FFmpeg | Requires |
|---|---|---|---|
| `SmartCut-0.8.21-linux-x86_64.tar.gz` | 192.6 MB | Bundled | glibc 2.35 or newer. No FUSE needed |
| `smartcut_0.8.21_amd64.deb` | 29.2 MB | Bundled (`/usr/lib/smartcut`) | Ubuntu 22.04 or later, Debian 13 or later |

**The tar.gz contains the same AppDir as the AppImage, extracted.** The 633 libraries
linuxdeploy gathered by following `ldd` sit in `app/` as they are, `./smartcut` is a
short script that calls AppRun, and `./smartcut-cli` points `LD_LIBRARY_PATH` at
`app/usr/lib` and calls the CLI. It runs anywhere the AppImage runs, and removes the need
to care about FUSE. Being gzip, it is 15 MB larger than the squashfs+zstd AppImage.

### The same library, three times over

linuxdeploy copies every name `ldd` gives it, and `libfoo.so` and `libfoo.so.0` are
symlinks a -dev package keeps to `libfoo.so.0.1.2` — followed on the way in, so all
three arrived as real files. librsvg is 6.2 MB three times. Across seven libraries that
is 19.4 MB, 3.7% of the 524.

The loader opens one of them, by SONAME; the other two are names. So `build-linux.sh`
puts them back as symlinks before the tar is made. **gzip cannot see this for itself:**
its window is 32 KB and the copies are megabytes apart — which is why the AppImage never
had the problem, squashfs sharing identical blocks.

Only `usr/lib`, and only above 64 KB, so that it is libraries this touches. The
copyright files under `usr/share/doc` are duplicates too, but a package's licence is the
last file to replace with a pointer to another package's.

The compression itself is `gzip -9`. It is 0.9 MB below the default 6 and costs a minute.
zstd or xz would take another 40–55 MB off (measured: 163 MB with `zstd -19`, 146 MB with
`xz -9`), at the price of what the other end needs to unpack it, so gzip stays.

**The deb carries FFmpeg and the encoders, and nothing else.** Up to 0.8.13 it linked
the system's FFmpeg 7.1, which only Debian 13 and Ubuntu 25.04 have; Ubuntu 22.04 and
24.04 could not install it.

What it carries is every library under `/opt/ff` that the two binaries actually load,
16 of them (eight libav* and x264, x265, SVT-AV1, rav1e, aom, libvpx, dav1d, libvpl).
They are found by following `ldd`, put in `/usr/lib/smartcut` under their SONAMEs, and
given `$ORIGIN` as their RUNPATH; the two binaries get `/usr/lib/smartcut`. Nothing
else looks there, so another FFmpeg on the system is left alone.

The dependencies are generated by feeding the binaries and the carried libraries to
22.04's `dpkg-shlibdeps`, so the version floors are 22.04's and any later system meets
them. Names that changed in Ubuntu 24.04 and Debian 13 (`libasound2t64` and the like)
still resolve, because the new packages `Provides` the old names.

```
Depends: libaom3 (>= 3.2.0), libasound2 (>= 1.0.29), libbz2-1.0, libc6 (>= 2.35),
 libcairo2 (>= 1.10.0), libdbus-1-3 (>= 1.9.14), libgcc-s1 (>= 4.2),
 libgdk-pixbuf-2.0-0 (>= 2.36.9), libglib2.0-0 (>= 2.65.1), libgtk-3-0 (>= 3.21.5),
 libjavascriptcoregtk-4.1-0, liblzma5 (>= 5.1.1alpha+20120614), libmp3lame0 (>= 3.100),
 libopus0 (>= 1.1), libsoup-3.0-0 (>= 3.0.3), libstdc++6 (>= 11), libvorbis0a (>= 1.1.2),
 libvorbisenc2 (>= 1.1.2), libwebkit2gtk-4.1-0 (>= 2.41.90), zlib1g (>= 1:1.2.0.2)
```

`libaom3` is there because 22.04 has a library of the same name as the carried
`libaom.so.3`. The carried one is what gets loaded; the package is on every system, so it
stays. The carried libraries have no package, which is what `--ignore-missing-info` is
for, and since that would also hide a real gap, `build-linux.sh` then checks every
library the binaries and the carried libraries load directly (`NEEDED`) and stops the
build if one is neither carried nor depended on.

Tauri's own deb stops at two entries, `libwebkit2gtk-4.1-0, libgtk-3-0`: neither
FFmpeg nor the audio codecs appear. That alone is reason enough to rebuild it. Also
added: a `.desktop` file (`Exec=smartcut %f`, `StartupWMClass=smartcut`, MimeTypes for
MPEG-2 TS and MP4), hicolor icons at 32/128/256, `copyright` and `changelog.Debian.gz`.

**The binary is taken from a different place for each bundle.** Tauri stamps the bundle
type into the binary just before packing (`UNKNOWN` → `DEB` / `APPIMAGE`), so
`target/release/smartcut` carries only the stamp of the last bundle built. The deb's
binary comes from Tauri's deb, and the tar.gz's from the AppDir.

### What was verified

- On a bare Ubuntu 22.04 (ubuntu-base 22.04.5), `apt install ./smartcut_0.8.21_amd64.deb`
  installs it: every dependency resolves, neither binary has a library it cannot find,
  and `smartcut-cli` loads `/usr/lib/smartcut/libavcodec.so.61`.
- A 30-second cut of a broadcast recording made there matches the tar.gz's
  `smartcut-cli` to the md5.
- On the Debian 13 development VM, `apt-get -s install` resolves the dependencies too.
- An actual install on Debian 13 was not done (sudo on the VM needs a password), and
  Ubuntu 24.04 was not tried.

## Windows

Cross-built from the Linux development VM to `x86_64-pc-windows-msvc`.

```bash
./gui/build-windows.sh
# -> gui/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/SmartCut_0.8.21_x64-setup.exe
# -> gui/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/portable/smartcut-portable-x64.zip
```

| Artifact | Size | Contents |
|---|---|---|
| NSIS installer | 55.7 MB | 179.0 MB installed (the GUI's exe, the CLI's exe and 8 FFmpeg DLLs) |
| Portable zip | 69.6 MB | The same set. Unzip and run `smartcut.exe` for the GUI, `smartcut-cli.exe` for the command line |

**The command-line tool ships under the name the Linux packages give it.** Its cargo
binary is called `smartcut`, which on Windows is the GUI's name, so the script builds it
first with `cargo xwin`, copies it into `windows-deps/` as `smartcut-cli.exe`, and
`tauri.windows.conf.json` lists it as a resource beside the DLLs. That is what puts it
next to the GUI in the install folder; the portable zip takes it from the same place.
Before 0.8.1 neither package carried it.

**Exactly one piece of code had to be rewritten for the port: audio output.** Everything
else goes through libav, so there is no `Command::new` and no POSIX path. All that was
needed was *the FFmpeg to link against*, plus two additions: `tauri.windows.conf.json`
and the build script. The sound card, however, lives on the far side of libav, and there
the local conventions had to be followed — see "Where it got stuck".

### Where the FFmpeg comes from

`ffmpeg-sys-next` looks at `include/` and `lib/*.lib` under `FFMPEG_DIR`. gyan's shared
build ships both the MSVC-format import libraries and the DLLs, so extracting it and
pointing at it is enough.

**It has to be the 7.1 series.** The VM's system FFmpeg is 7.1.5, and
`ffmpeg-sys-next 7.1.3` selects its bindings by version, so a different series gives a
mismatched API. But upstream stopped distributing Windows builds of 7.1 when 8.1 came
out, and neither BtbN nor gyan's own site carries anything but 8.1 and 9.0. gyan's GitHub
releases (`GyanD/codexffmpeg`) still have 7.1.1, so that is where it comes from.

Eight DLLs are needed. `avfilter` opens `postproc`, so `postproc-58.dll` is needed too,
even though its name never appears in the exe's import table.

### What the running machine needs

| Item | Status |
|---|---|
| FFmpeg | **Not needed** — the DLLs ship alongside the exe |
| VC++ redistributable | **Not needed.** The only C runtime the exe pulls is UCRT (`api-ms-win-crt-*`), which ships with Windows 10 and later |
| WebView2 runtime | Needed. Standard on Windows 11, and present on nearly all Windows 10 machines via Edge. Without it the NSIS installer fetches the bootstrapper by default; the portable zip leaves it to you |
| Architecture | x64 only |

### Verification

Checked under wine 10.0 on the VM.

- **The CLI's output does not differ from the Linux build by a single byte.** `--cut
  5-10` on the same `mpeg2.ts` matches to the md5. The index (41 access points, 39 open
  GOPs) is identical, as is the 0.3% re-encoded for the partial GOPs at the boundaries.
  The CLI checked is the `smartcut-cli.exe` out of the portable zip, run in the folder
  it unpacks to. Right now this is effectively the only route left for confirming that
  the FFmpeg DLLs resolve — see the next point.

- **The GUI no longer starts under wine** (as of 2026-08-27). `tao`'s
  `event_loop.rs:709` hits `assertion failed: subclass_result.as_bool()`, meaning
  `SetWindowSubclass` is failing. That is before WebView2 initialises, so what was
  documented here previously — "a 1180x800 window appears and stops at 'Could not find
  the WebView2 Runtime'" — is no longer reached.

  **This is not a build regression.** The already-published v0.1.0 exe (the portable zip
  in Releases) panics on the same line under the same wine. The cause is on the wine side
  (comctl32 subclassing). So the argument "let the GUI reach WebView2 and thereby confirm
  DLL resolution" — the same argument as starting the AppImage without `DISPLAY` and
  letting it reach GTK initialisation — is unavailable for now, and its stand-in is the
  CLI, which loads the same set of DLLs and matches Linux to the md5.

That is as far as wine goes. **The first bug to appear when it was run on real Windows
was that preview audio was silent** (see below). Wine's WASAPI accepts any format, so it
is not the kind of thing the verification above can hit.

The installer's payload and the standalone exe differ by exactly 3 bytes, because Tauri
stamps the bundle type into the exe: `NSIS` on the installer side, `UNKNOWN` in the
portable zip.

### Where it got stuck

- **Preview audio was silent on Windows only.** The material's sample rate and channel
  count were being requested from the sound card as they were. On Linux that works —
  cpal's default output is ALSA's `default`, which is really a chain of `plug`, and `plug`
  takes on any conversion the card cannot do. **WASAPI's shared mode does not.** Shared
  mode mixes every application into one format, so `IAudioClient` can only be initialised
  with that format, and `IsFormatSupported` answers a different format with `S_FALSE` plus
  "the closest format". cpal treats that as unsupported (`is_format_supported` collapses
  `S_FALSE` to `Ok(false)`), so it ends at `StreamConfigNotSupported` without opening.

  The mixing format is whatever the sound settings say, so **a PC with its output at
  44.1 kHz was completely silent on 48 kHz broadcasts**, and the same happened sending a
  5.1 broadcast to a stereo output. Fixed by asking the device what it mixes in and
  matching that, and adding rate and channel-layout conversion via swresample
  (`playback_audio::candidates`). **The material's own rate and sample format are kept
  as the first candidate**, so Linux plays them unconverted; only the order of the
  channels is changed there, into ALSA's (see [Audio](audio.md)). A candidate with the fixed
  period removed was appended at the end too — ALSA's
  `snd_pcm_hw_params_set_buffer_size` rejects sizes that do not divide evenly, so 882
  frames at 44.1 kHz fails on some cards.

- **`resampling::Context::run`'s output frame only holds as many samples as the input.**
  Going up in rate (48 kHz → 96 kHz, say) that is not enough, and swresample banks the
  overflow internally. Nothing crashes or breaks, but what is banked never comes out
  again, so latency and memory grow for as long as playback continues. The output frame is
  now allocated here as `input.samples() × output rate ÷ input rate`.

- **Static CRT linking (`+crt-static`) did not work.** Invoking `cargo xwin build`
  directly works, but going through `cargo tauri build --runner cargo-xwin`, cargo-xwin
  mishandles the flag and produces a link line mixing static and dynamic CRT libraries
  (`libucrt.lib` drops out and `strlen` goes undefined). Neither the environment variable
  nor `.cargo/config.toml` helps. Dynamic linking produces no VC++ redistributable
  dependency anyway, so this was not pursued further.

- **By default it is `gui.exe`**, because the cargo binary takes the crate name `gui`.
  `mainBinaryName` makes it `smartcut.exe`; it was set in `tauri.windows.conf.json` until
  0.2.0 moved it into `tauri.conf.json`, where it names the Linux binary as well.
