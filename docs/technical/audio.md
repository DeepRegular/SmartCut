# Audio

[← Documentation](../README.md) ・ [← SmartCut](../../README.md) ・ [日本語](audio.ja.md)

Audio has no GOP structure, so it cannot be cut the way video is. This page covers
how SmartCut cuts it instead: the three modes, what each one costs, how MPEG-2 AAC
framing is preserved, how a 5.1 track is folded to stereo, and how a bilingual
broadcast's two tracks are handled.

## The three modes

| Mode | Boundary error | Left over at a seam | Frames re-encoded |
|---|---|---|---|
| `copy` | ±half a frame (10.7 ms for AAC at 48 kHz), no accumulation | up to 21.3 ms of the audio that was cut away | none |
| `smart` (default) | as above | nothing (it is silence) | at most 2 per boundary, none when the seam is in silence |
| `reencode` | **−0.02 ms** — one sample is 0.021 ms, so effectively zero | nothing | all of them |

**The default is `smart`.** On an ordinary cut — a commercial break taken out in the
silence around it — its output is byte-identical to `copy`, so there is nothing to
lose by making it the default. Where they differ, the cut is in the middle of sound
and the frame is worth touching. `copy` remains for work that must be exact to the
byte. The ±10.7 ms boundary error is the same in both, and far below the perceptual
threshold for lip sync.

Run `tests/run_audio_tests.sh` with `SMARTCUT_AUDIO=reencode` and all 5 cases land at
−0.02 ms. Video stays lossless in every mode (1798/1798, 99.1% on real material).

## Cutting per interval, and the drift that nearly happened

Audio is handled **per kept interval** rather than per video segment. Each interval's
audio is anchored to the output time where that interval's video starts, so drift
across intervals cannot happen by construction — or so it seemed.

There was a trap. **An MP4 audio track expresses time through sample durations
(`stts`); it does not keep a timestamp per packet.** All it keeps is the track's start
offset, and the samples are laid out back to back after that. So dropping a single
audio frame at an interval boundary **shifts every following interval permanently, and
the error accumulates interval by interval**. Measured on material with a click every
0.5 s, the second interval came out a uniform 11 ms early.

The fix is to **track the actual end position of the audio written so far and carry
that error into the choice of the next interval's first frame**. Since the interval
opens on the frame nearest the boundary, the error is bounded by half a frame, about
10.7 ms for AAC at 48 kHz, and does not accumulate. A test with three intervals and
every boundary deliberately off the frame grid showed a worst case of +7.67 ms.

Two routes to trimming in the container were tried, and both are dead ends:

- `AV_PKT_DATA_SKIP_SAMPLES`, the side data used for gapless playback — **the MP4
  muxer ignores it.** The samples marked for skipping stayed in and the audio ran late
  by exactly the amount requested.
- `initial_padding` on the output stream — **the muxer does not write it to the
  file.** Probe the output and `initial_padding=0`. Trying it made the single-interval
  error worse, from 0.00 ms to +9.33 ms.

That leaves re-encoding, which is what `--audio-mode reencode` does: each interval's
audio is cut sample-exactly and fed continuously into a single encoder.

A trap hit while implementing smart mode: audio frames straddling a segment boundary
were submitted twice, once from each of the two adjacent segments, and the error
accumulated one AAC frame (21.3 ms) at a time. Fixed by adopting the same exclusive
ownership rule as the copy path.

`tests/run_audio_tests.sh` measures A/V sync sample by sample on material with a 2 ms
impulse every 0.5 s. Steady tones cannot reveal a sync error.

## Smart rendering applied to audio

The boundary error is not the only thing copying leaves behind. **The frame a boundary
falls inside is copied whole**, and what is inside it includes the audio from the side
that was cut away. That is the last syllable of a commercial arriving over the opening
of the programme: up to 21.3 ms of it, once per seam, every seam.

Smart mode re-encodes **only the frames the boundaries fall inside**. Each is built
from the recording's own samples, with everything outside the kept range faded to
silence over a millisecond. No other frame is touched. On real material — Nippon TV,
two intervals, four boundaries — **5602 of 5606 frames are the recording's own bytes;
4 were rewritten**.

**The boundary itself does not move.** Two frames cannot occupy one instant: MP4 lays
its samples end to end, and MPEG-TS rejects a timestamp that goes backwards. So an
interval is always a whole number of frames long. Opening an interval on the frame the
boundary falls inside would lose nothing in itself, but that frame reaches back into
the interval before it, where the previous interval's own overrunning last frame
already is. Every mode therefore keeps whole frames and centres the error. What smart
mode changes is **what fills the rest of the straddling frame**.

**One neighbour is re-encoded with it, as a guard.** AAC frames overlap by half a
window, so the decoder rebuilds each frame from that frame and its neighbour, and a
re-encoded frame sitting directly against a copied one is the one place the two halves
can disagree. Silencing part of a frame is a transient, and a transient is what makes
an encoder switch to short windows — which is exactly that disagreement. The guard
carries the recording's own samples, unmasked, and keeps the switch one frame away
from the copied material. When the straddling frame is not the one the interval opens
on (the nearest-frame rule may pick the next), the guard has nothing to guard and is
not re-encoded either.

**When the far side is already silent, nothing is re-encoded.** A commercial break is
cut in the silence around it, so the far half of the straddling frame is usually silent
already, and replacing it would remove nothing. The test is whether the samples outside
the range peak above −60 dBFS. On real material, a cut that takes out one commercial
block — all four boundaries inside the silence — re-encodes **no frames at all, and the
output is byte-identical to `copy`**. The re-encode only engages where a cut lands in
the middle of sound.

### Where the design came from

This design came from measuring TMPGEnc MPEG Smart Renderer 6.1's output against its
source. It re-encodes **not one audio frame**: of the 67499 frames in a 24-minute cut,
67492 (99.99%) carry the recording's own coded audio, and the seven that do not sit
nowhere near a join. All three of its joins are inside digital silence at −91.0 dBFS,
where cutting on a frame boundary loses nothing. (It does re-pad every frame to a
strict 256 kbps CBR, so only 24.7% of its frames are byte-identical — the audio data
inside them is untouched.)

### Which codecs it reaches

Whether a frame can be replaced at all comes down to what the encoder for that codec
will take and what it hands back.

**It has to take what these buffers hold.** Everything here is planar float, which is
what the AAC and AC-3 encoders want. Blu-ray LPCM wants 16 or 32 bit integers, MP2 wants
16, and an encoder handed a format it does not list will not open at all. So the encoder
is asked what it takes — and asked for the recording's own sample width first, which is
not only about keeping the samples. The LPCM encoder frames 16 bit sound 240 samples at
a time and 24 bit sound 360, so a 16 bit recording asked for as 24 bit comes back framed
differently from itself.

**Its delay has to be a whole number of frames.** The replacement has to cover the same
samples the recording's frame did, and a fractional delay puts every packet the encoder
makes off the recording's frame grid. AAC's delay is 1024, exactly one frame, and lines
up. AC-3's is 256 and MP2's is 481, and neither does.

An encoder is opened once before the run to check both, and when they do not line up
SmartCut says so and copies instead. Two codecs are not asked at all: DTS and TrueHD are
lossless and libavformat's encoders for them are not, so their frames are carried
through untouched — see
[Reading a Blu-ray](../developers/disc.md#the-sound-a-disc-carries).

So smart rendering reaches **AAC** and **Blu-ray LPCM**. AC-3, E-AC-3 and MP2 are copied
because they cannot be lined up; DTS and TrueHD are copied on purpose.

## Writing MPEG-2 AAC (`--aac`)

A Japanese broadcast carries **MPEG-2 AAC**: the ADTS `ID` bit is 1, profile LC,
48 kHz, with a CRC. FFmpeg's AAC encoder produces MPEG-4 AAC, and every muxer that
frames raw AAC for us writes `ID = 0`. The ADTS muxer has a `write_mpeg2` option, but
**MPEG-TS has no way to pass it down to the ADTS muxer it uses internally**. Left
alone, a cut comes out as a stream that is MPEG-2 nearly everywhere and MPEG-4 for one
frame per seam, which the tools downstream of a recording read as malformed rather than
as the recording they were handed.

So the ADTS headers are written here instead (`adts.rs`). The profile, sampling
frequency, channel configuration and `ID` are read off the recording's own frames and
put in front of the encoder's output. **Every muxer involved leaves a packet that
already begins with a sync word alone**, which is what makes this work: MPEG-TS passes
it through untouched, and MP4 runs it through `aac_adtstoasc` exactly as it does the
source's own frames.

- The default follows the recording (`--aac auto`), so broadcast material stays
  MPEG-2.
- Only **the frames SmartCut writes** are framed here; copied frames keep their own
  headers. So a version that disagrees with the recording cannot be delivered while
  anything is being copied — it would make a stream that is two kinds of AAC at once —
  and the request is refused with a note instead. Under `--audio-mode reencode` nothing
  is copied and it is honoured. The tests check both.
- The payload is kept inside MPEG-2 AAC LC as well: perceptual noise substitution, an
  MPEG-4 tool, is switched off.
- The frames written here carry no CRC (`protection_absent = 1`). It is a per-frame
  field, so such a frame sits legally among frames that have one, and writing a CRC
  that was subtly wrong would be worse: a decoder that checks it would throw the frame
  away.
- `--audio-mode reencode` uses the same framing, so **a whole track re-encoded into a
  transport stream also comes out MPEG-2 AAC** — a combination FFmpeg on its own cannot
  be asked for.

`tests/run_aac_tests.sh` walks the output's ADTS frame by frame, checks they are all
MPEG-2 LC, and counts how many are the recording's own bytes. Neither question can be
put to a decoder; both are about the bytes.

There is more on why this matters downstream in
[broadcast workflow compatibility](broadcast-ts.md#audio-can-be-written-out-on-its-own-as-aac).

## Downmixing (`--audio-channels`)

Some recordings carry 5.1, and a good many of the places they end up do not want it: a
television that folds it badly, a player that puts the dialogue in the centre channel
and never plays it, a phone.

Asking for stereo is asking for the track to be rebuilt, and this is the one audio
setting that **decides the mode rather than living under it**. No frame of a 5.1
recording can be spliced into a stereo track, so `smart` and `copy` have nothing to
offer here: a channel count that is not the recording's makes the cut a whole-track
re-encode, and SmartCut says so on the way past.

The fold itself is swresample's, which means the rematrixing coefficients are libav's
own — centre at −3 dB into both, the surrounds into their own side, the LFE dropped.
What comes out is what a player downmixing the recording would have produced, which is
the point: the recording is being fixed, not reinterpreted.

- **In and out at the same rate**, which is what keeps it a per-frame operation.
  swresample hands a frame's samples back one for one and holds nothing over, so the
  sample window each keep-range is trimmed against still means what it meant on the
  source's own clock, and the boundaries stay sample-accurate.
- **The ADTS header has to be rewritten too.** A transport stream says how many
  channels a frame has in the frame's own header, and the header these frames inherit is
  the recording's. Left alone it announces 5.1 over a stereo payload, and a decoder that
  believes the header ahead of the payload gets neither. `AdtsFormat::with_channels` puts
  the `channel_config` right. A count with no configuration of its own — 7 channels, say
  — is left alone, because such a stream describes itself in a program config element
  inside the frame instead.
- **The output stream's parameters come from the encoder** once the channels have
  changed, framed output or not: the recording's parameters describe a track the file no
  longer contains. This costs the framing nothing, since a packet that already begins
  with a sync word is passed through whatever the extradata says.
- **A derived bitrate comes down with the channel count.** 384 kbit/s is what the 5.1
  cost, not what the stereo it was folded into is worth, so the rate taken from the
  recording is scaled by the fold and floored at 128 kbit/s. `--audio-bitrate` is taken
  as given.

`tests/run_downmix_tests.sh` uses a 5.1 fixture with a different tone in every channel,
so the fold is read out of the spectrum rather than off the metadata: the centre has to
arrive in both output channels, the left surround in the left only, and the LFE
nowhere. It checks the ADTS `channel_config` as well, and finishes by measuring A/V
sync through the fold on a 5.1 impulse fixture — 0.83 ms at worst, against the 1 ms bar
a whole-track re-encode is held to.

## The rate and the width (`--audio-samplerate`, `--audio-bits`)

A sample has three things about it, and the channel count is only the first. The other
two are how often the sample is taken and how wide it is written, and both behave the
same way a downmix does: **each decides the mode rather than living under it**. Samples
on a 44.1 kHz grid cannot be spliced in among a 48 kHz recording's frames, and a 16 bit
sample is not a 24 bit one, so either setting makes the cut a whole-track re-encode and
SmartCut says so on the way past.

### The rate

The resampler is swresample's again, but where the downmix's is per-frame this one is
**one context for the whole track**, and that difference is the whole design.

A resampler is a filter. The output grid does not land on the input grid, so part of a
sample is always held back between calls — and a context rebuilt per frame would drop
that remainder every time, which is a click at every frame boundary rather than a track.
So the two stages are kept apart: the channels are put right frame by frame on the way
in, where in and out are at the same rate and the sample window a keep-range is trimmed
against still counts in the recording's own samples; then the kept samples, already
spliced end to end, run through one resampler on their way to the encoder. What that
filter is still holding at the end of the track is pushed out with a flush, or the last
few milliseconds are simply missing.

Three places have to hear about the new rate, and missing any one of them is audible:

- **The samples themselves**, which is the resampler's job.
- **The output stream's declaration.** Its time base is the rate, and a re-encoded
  packet's timestamp is its sample count divided by the rate — the output's, not the
  recording's, or an MPEG-TS packet lands at the wrong second.
- **The ADTS header on every frame**, for AAC in a transport stream. A track resampled
  to 44.1 kHz whose frames still announce 48 plays 9% fast to any decoder that believes
  the header ahead of the payload. `AdtsFormat::with_rate` puts the sampling frequency
  index right, the same way `with_channels` puts the channel configuration right.

**Not every codec speaks every rate.** AC-3 has three — 32, 44.1 and 48 kHz — MP2 has
those and their halves, and Blu-ray LPCM has 48, 96 and 192. An encoder handed a rate it
does not list refuses to open at all, which is the same failure a sample format or a
channel layout it does not list produces, and it is answered the same way: by asking the
encoder. `audio::writable_rate` takes the rate asked for to the nearest the codec has,
preferring the higher of two equally near, and the cut says which rate it settled on.
It is asked before anything is opened, so the stream is declared at the rate its packets
will actually arrive at.

### The width

A width only means something where samples are what is being written. Every lossy
encoder here takes a float and spends a bitrate; how many bits the sound had before it
is not a number one of them has anywhere to put, so a width asked of AAC is declined out
loud rather than quietly ignored.

Where it does mean something it means a great deal. It picks the codec outright in an
MP4 — `pcm_s16be` or `pcm_s24be`, since there is no one box for PCM of any width — and
in a transport stream it picks the sample format the Blu-ray LPCM encoder is opened
with, which is 16 bit or 32 bit carrying 24. And it decides the file's size on its own:
channels times width times the rate, with nothing else in it.

It also decides the *frame length*. The Blu-ray LPCM encoder frames 16 bit sound 240
samples at a time and 24 bit sound 360, which is why the width was already being settled
before any of this was a setting — a 16 bit recording asked for as 24 comes back framed
differently from itself, and a frame that does not line up with the recording's own is a
frame that cannot stand in for one.

`tests/run_audio_format_tests.sh` reads the rate out of all three places, checks that a
rate a codec does not have comes back as the nearest it does, reads each width off the
size of the file it produced, and finishes by measuring A/V sync through a resample on
the impulse fixture — 0.02 ms at worst, against the 1 ms bar a whole-track re-encode is
held to.

## Choosing the codec (`--audio-codec`)

A cut of a broadcast is AAC because the broadcast was, and for most of what this program
is for that is the end of it. It is not the end of it when the file has somewhere to go
afterwards: a disc player and an AV receiver want AC-3 or DTS, and an editor that will
encode again wants the samples themselves rather than a second generation of something
lossy.

Naming a codec is the second setting that **decides the mode rather than living under
it**, for exactly the reasoning that makes a downmix one. The only frame that can be
copied is a frame already in the codec being written, so a codec that is not the
recording's leaves nothing to splice: `smart` and `copy` have nothing to offer, the cut
becomes a whole-track re-encode, and SmartCut says so on the way past. Asking for the
codec the recording already carries is not a conversion and changes nothing — the mode
stands.

| Asked for | Into a transport stream | Into MP4, MKV, MOV |
|---|---|---|
| `aac` | AAC, ADTS framed | AAC |
| `ac3` | AC-3 | AC-3 |
| `dts` | DTS | DTS |
| `lpcm` | Blu-ray LPCM (`pcm_bluray`) | `pcm_s16be`, or `pcm_s24be` from a deep source |

Four things had to be settled to make that table true.

- **The ADTS framing is AAC's alone.** Everything this program encodes into a transport
  stream is framed, because a re-encoded frame has to be the same shape as the copied
  frames beside it — but ADTS is AAC's framing and nothing else's, and six bytes of it in
  front of an AC-3 frame is six bytes a decoder will try to read as a frame. The framing
  is now conditional on the codec written, not on the codec that arrived.
- **The programme map has to describe what is actually in the file.** A downmixed track is
  still the codec it was, so the recording's own map entry stays true of everything but the
  channels; a track re-encoded as AC-3 is not, and an entry that says MPEG-2 AAC will have
  a player hand AC-3 frames to an AAC decoder. Where the codec changed, the entry is
  written from what the cut produced: 0x0F for AAC, 0x81 and an `AC-3` registration
  descriptor, 0x82 for DTS, 0x80 for LPCM.
- **LPCM in a transport stream needs the programme registered as HDMV.** There is no
  standard way to carry raw PCM in an MPEG-2 transport stream; there is only Blu-ray's,
  which is a private stream that means LPCM because the programme registers itself as HDMV
  and not otherwise. libavformat's muxer writes that registration when it is asked for
  Blu-ray's own framing and not when it is asked for a plain `.ts`, where it falls back to
  declaring private data of no stated kind — which reads back as `bin_data` and decodes to
  nothing. The bytes are identical either way, so the cut writes the registration and the
  0x80 into the map it was already rebuilding. Into an MP4 or an MKV there is no such
  problem: those have a box for plain PCM, and the samples go in big-endian at the
  recording's own width.
- **The encoder's own idea of a channel layout.** Six channels are not one arrangement:
  libav's default for six puts the surrounds at the back, FFmpeg's DTS and Blu-ray LPCM
  encoders list only the arrangement with them at the sides, and `avcodec_open2` refuses
  to open on a layout the encoder does not list — the same failure as an unlisted sample
  format, and answered the same way, by asking the encoder and resampling into what it
  says. The frames the recording's own smart-rendered patches are built from are exempt:
  those go back among the recording's own frames, so the layout asked for there is the
  layout that arrived and the only conversion is one of format.

**Bitrates are the codec's, not the recording's.** Following the recording's rate is right
while the codec is the recording's and meaningless once it is not: 384 kbit/s is what a
5.1 broadcast's AAC cost, it is not what the same programme is worth as AC-3, and as DTS
it is not a rate the format has. So where the codec changed, the derived rate is what that
codec is ordinarily carried at for that many channels — AC-3 at 96 / 192 / 448 kbit/s for
mono, stereo and surround, DTS at 768 and 1536 — and `--audio-bitrate` is taken as given
whatever the codec.

LPCM has no rate to choose at all. Its size is arithmetic — channels times bit depth
times the sample rate — and the encoder throws away any figure handed to it, so the
window greys the control out and puts the arithmetic in it: what an uncompressed track
costs is the one thing anyone wants the control to say, and it can be said exactly.

All three numbers are the output's rather than the recording's. The channels are what is
being written, so a fold halves the figure; the rate is what `--audio-samplerate` asked
for, or the recording's; and the depth is what `--audio-bits` asked for, or what
[`audio::pcm_bits`] settles — 24 only where the recording has more than 16 bits in it,
which off a broadcast it never does. And there is a third term in a transport stream:
Blu-ray's LPCM writes its channels in pairs and pads an odd count with a silent one, so a
mono track in a `.ts` costs two channels' worth of bytes and the window says 1536 kbit/s
rather than 768.

**DTS is behind libavcodec's experimental flag.** Its encoder refuses to open unless the
caller says that is understood, which the cut does, once, for that codec. "Experimental"
is libav's account of how much attention the encoder has had; what comes out is a DTS
stream a receiver decodes.

`tests/run_audio_codec_tests.sh` asks for each of the four into each of four containers,
and checks the track is the codec asked for, that every channel of a 5.1 fixture still
carries the tone it went in with, and that a transport stream's map declares the codec
that is in it.

## Multi-audio broadcasts

A bilingual broadcast sends its sound on **two separate PIDs**. Only one used to be
picked up, by `best(Type::Audio)`, so the second language was never even read.

`Source.audios` now lists every sound track the recording carries. `Source.audio`
stays, as the *main* one: commercial detection, preview playback and the `.aac` sidecar
each read a single track, and which one that is remains a real question.

On the way out, **each track is cut on its own**. That is the whole point, and the
reason the writer's state was split from one set into one set per track. Two tracks
have their frames at different instants, so:

- a boundary falls inside a different frame on each, meaning a different number of
  frames get re-encoded;
- each accumulates a different drift from range to range;
- and they need not even be framed the same way — one MPEG-2 AAC, the other MPEG-4.

Use one track's answer for the other and the other slips a frame per range. So an
`AudioTrack` holds everything true of one track alone — its output stream index, how far
it has been written, the previous frame's `pts`, its table of re-encoded boundary
frames, its re-encoder — and the `Writer` holds a list of them.

Options like `--audio-channels` apply to every track, but the *decision* is per track:
only a track that actually has to be folded becomes a whole-track re-encode, and a
track that was already stereo is still copied.

A track that is not wanted is dropped with `--drop-stream <stream index>`, or in the
GUI with the cut editor's track menu. The default is to **write them all**: a track
nobody asked about is a track that was in the recording, and dropping it silently would
be this program deciding what the recording is for.

## The encoder knows its own delay

The re-encoding path was a frame out, and had been all along. It counted the encoder's
delay itself, from `initial_padding`, dropped that many opening packets, *and*
subtracted the same amount from their timestamps — taking it off twice.

A libav encoder declares its delay by stamping its output: the first packet comes out
at `pts = -1024`, covering the window that reaches back before anything was fed, and the
next at `pts = 0`, covering the first frame that was. The packet says where it belongs,
so the fix is to believe it: drop what lands before zero and take the timestamp as the
position. The A/V length skew in `run_audio_tests.sh` under `reencode`, which peaked at
21.3 ms — exactly one frame — now peaks at 16.0 ms and averages 6.4 ms.

## Verifying real material's audio

Impulse material can only answer "how does it behave on material we made". "When a real
broadcast recording is cut, is the sound that comes out really the sound from that
place?" is a different question.

So `tests/run_audio_content_tests.sh` decodes 6 seconds of the output and finds, by
cross-correlation, where in the recording it matches. Dropped, muted or misplaced audio
all fail this.

What matters is **not the absolute offset but whether the offset changes from interval
to interval**. Decoder priming and the container start time apply equally everywhere, so
a constant offset belongs to the measurement. If it changes, that is a bad seam.

Measured on two intervals of real material (Nihonkai TV, 30 minutes):

| Audio | Container | Correlation | Offset spread across seams |
|---|---|---|---|
| copy | MP4 | **1.000** | 31.0 ms |
| copy | TS | **1.000** | 31.0 ms |
| sample-accurate | MP4 | **1.000** | **0.0 ms** |
| sample-accurate | TS | **1.000** | **0.0 ms** |

**The sound is unquestionably there** — correlation 1.000 across every interval, with
RMS within 1 dB of the source. On top of that, leaving audio on copy shifts the sound by
up to about 30 ms per seam, and it stacks up with the number of seams. AAC can only be
cut on a 21.3 ms frame grid, so the boundary rounding shows through directly. The same
31.0 ms appears in MP4 and in TS, so this is a property of the cutting side, not of the
muxing.

Re-rendering sample-accurately brings it to 0.0 ms (`--audio-mode reencode`). It is
nonetheless kept out of the GUI, for two reasons, both from measurement:

- **On actual commercial cuts, copy showed no drift.** The 31.0 ms above comes from two
  intervals deliberately placed on boundaries that are not lossless points. Commercial
  detection snaps its boundaries to access points, and across a five-interval export of
  real material all six seams were a constant +5.7 ms, unchanged from interval to
  interval. The offset is per-seam rounding, not something that stacks, and placing the
  boundaries well removes almost all of it.
- **Re-rendering changes the shape of the ADTS and breaks downstream tools.** Japanese
  broadcast AAC is `ff f8` (MPEG-2, with CRC) while ffmpeg can only write `ff f1`
  (MPEG-4, no CRC). Indexing software written for ARIB fails to read it.

The judgement is that what is lost — downstream tools cannot read the file — outweighs
what is gained, an improvement of roughly 0 ms. It is still in the engine and the CLI,
so `--audio-mode reencode` is there if it is needed.

## The output settings screen

Six controls, one above five. **Audio** picks the mode — smart rendering, copy through,
re-encode everything — and under it **Audio codec**, **Audio channels**, **Sample rate**,
**Bit depth** and **Audio bitrate** say what that encode should be. The cases they exist
for are a 5.1 recording that has to play somewhere that will not have it, and a cut that
has somewhere to go after this program is done with it.

**Audio codec** offers the recording's own, AAC, AC-3, DTS and linear PCM. See
[Choosing the codec](#choosing-the-codec---audio-codec) for what each is for and what had
to be settled to write them.

**Audio channels** offers the recording's own count, 1ch, 2ch or 5.1ch, and names them by
the count rather than by the direction. Which way it goes is the recording's to say, not
the setting's, and one list can hold a 5.1 recording and a stereo one, where 2ch folds the
first and spreads the second. The readouts say which happened. (The engine takes any count
from 1 to 8, and `--audio-channels` will do 7.1; the window offers the three that get
asked for.)

**Sample rate** offers the recording's own, 48, 44.1 and 32 kHz — a broadcast, a CD, and
what a small file gets away with. Like a downmix it is a whole-track re-encode or it is
nothing; unlike a downmix the codec may not have the rate at all, in which case the cut
writes the nearest it does have and says which. Of the three offered, only one case falls
outside a codec's list — Blu-ray LPCM has 48, 96 and 192 kHz and nothing between, so 44.1
asked of a `.ts` comes back as 48 — and the window does that arithmetic too (`writableRate`),
because the figure it shows in place of an uncompressed track's bitrate has to be the size
of the file that will actually be written. See
[The rate and the width](#the-rate-and-the-width---audio-samplerate---audio-bits).

**Bit depth** offers the recording's own, 16 and 24 bit, and is the one control here that
does not answer to the mode alone: **it is greyed out unless linear PCM is what is being
written**. Every other codec writes a description of the sound rather than the sound, and
has nowhere to put a width. That makes it the mirror image of the bitrate below it —
exactly one of the two is grey at any time, because an uncompressed track's size is
arithmetic nobody chooses and a lossy track's width is a number nobody can name.

**All of them are greyed out unless the mode is "Re-encode everything"**, because they describe
an encode and the other two modes do not run one over the whole track. `copy` runs none at
all, and `smart` runs one on two frames per boundary, where the whole point is that they
come out the same shape as the frames they are spliced between. They are greyed out rather
than cleared — what they hold is still readable, and still there when the mode comes back
round to wanting it — and what they hold does not reach the cut while they are greyed: the
window sends the two settings only when it is re-encoding, so the screen and the file
agree.

The engine takes the harder line, and has to: `audio_channels`, `audio_sample_rate` and
`audio_bits` there each force a whole-track re-encode whatever the mode says, because a
stereo frame cannot be spliced among a recording's 5.1 ones — nor a 44.1 kHz one among
48 kHz ones — and there is no honest way to half-do it. The window never puts it in
that position; the CLI can, and gets a note saying so.

**Audio bitrate** on "Leave it to the engine" means the engine derives one from the
recording, bringing it down with the channel count, since what 5.1 cost is not what the
stereo it was folded into is worth.

**The rungs are the codec's own, and where the ladder stops moves with the channel
count:**

| Codec | Rungs | 1ch | 2ch | 5.1 |
|---|---|---|---|---|
| AAC | 64 … 640 kbit/s | 192 | 384 | 640 |
| AC-3 | the format's own table, 64 … 640 | 192 | 384 | 640 |
| DTS | the format's own table, 384 … 1536 | 768 | 1536 | 1536 |
| linear PCM | — | — | — | — |

AAC's are where a broadcast puts them, with room over the top; AC-3's are where a disc
does, and 640 is the format's own limit; DTS has two rates anyone uses, 768 and 1536, and
the ladder is the way between them. AC-3 and DTS both carry the rate as a number out of a
table the format defines, so only the rates in that table are offered — a figure between
two of them is one the encoder rounds wherever it pleases. "Same as the input" is not in
the table at all: what a clip carries is not known until it is read, and for a broadcast
it is AAC, so AAC's ladder is the one shown.

Linear PCM's row is empty because there is nothing to choose in it — its size is channels
times width times the rate, and **Bit depth** and **Sample rate** are where those two are
chosen. The control goes grey and the setting is cleared with it — a figure left standing behind a greyed-out control
would come back as an answer nobody gave the moment the codec changed — and in its place
the control shows what the track will actually cost, worked out as above.

The setting is one answer for a whole list, and the clips in it need not cost the same: a
stereo recording beside a 5.1 one, both read as "same as the input", are two figures. Then
the control names the range rather than picking one of them, which is the same honesty as
the ceiling above taking the widest track in the list. The format panel's own audio line
has only one clip to describe, so there the figure is always exact.

The top of the stereo ladder is headroom rather than a promise, because the encoder has a
ceiling of its own and does not announce it: asked for more than it can spend it simply
writes less, and asked for a good deal more it writes *less than it would have*, because
the rate control overshoots and backs off. Driven with noise at 48 kHz, so that the
encoder and not the material is what runs out, mono walls near 218 kbit/s and stereo near
250. Ask for 384 of stereo and what comes back is whatever the encoder found worth
spending.

"Same as the input" has no count of its own, and the setting is one answer for a whole
list, so the widest track in the list decides — the widest the ceiling could have to
cover. A count with no ceiling of its own takes the next one up, a 4-channel recording
being nearer 5.1 than stereo. A rate the list stops offering, because the channel count
came down under it or a project was written when the rungs were different, is taken to the
nearest rung at or below it rather than thrown away.

**The panel's drop-downs open upward.** File settings are the bottom panel of the window,
so a native popup there has nowhere to go: the bitrate ladder, sixteen rungs of it, ran off
the bottom of the screen with most of itself out of reach. Where a native popup opens is
the platform's decision and not ours, so the popup is ours instead (`opensUpward`) — but
only the popup. The `<select>` stays where it was and goes on holding the answer, so
reading a setting off a control, putting one back when a project opens, and translating the
options all keep working untouched. What is drawn carries two marks rather than one: the
answer the control holds, and the cursor, which the mouse and the arrow keys move together.
The one thing to remember is that suppressing the native popup costs the click its focus,
so the focus is handed back by hand — a control that cannot be reached by the keyboard
after being clicked would be worse than a popup in the wrong place.

Two places say what will happen, because the screens are two:

- The format panel names it as part of the audio line — `Audio: re-encoded (5.1ch → 2ch,
  48 kHz → 44.1 kHz, 192 kbps)`, and `Audio: yes (5.1ch)` in quick properties and in the
  editor's info bar,
  so a 5.1 clip can be picked out of a list of stereo ones **before** the output has been
  written rather than after.
- The output screen is about pictures, and its best line is "nothing re-encoded, the whole
  clip is copied losslessly". That stops being true of the file as soon as the audio is
  re-encoded end to end, so the audio's own fate is appended to it: `(the audio is
  downmixed 5.1ch → 2ch and re-encoded)`.

Every one of these settings travels in the project file, which needs no code: the file
carries whatever keys the settings object has.
