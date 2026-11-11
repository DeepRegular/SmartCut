//! Which containers can hold which codecs.
//!
//! A cut copies the recording's own frames, so what comes out is written in
//! the codec that went in, and not every container has a box for every
//! codec. Three things can happen where one does not, and only two of them
//! are any use:
//!
//! - **The muxer refuses.** An `.mp4` asked for VP8, a `.mov` asked for
//!   TrueHD: the header is never written and the cut stops with an error.
//! - **The muxer writes it as private data.** This is MPEG-TS, and it is why
//!   this module exists. A transport stream declares every stream by a type
//!   in its programme map, and libavformat has one for each codec the format
//!   has a type for. For everything else it writes `0x06` -- private data of
//!   no stated kind -- and says nothing at all. The cut finishes, the file is
//!   the right size, and every player reads the pictures as `bin_data`:
//!   carried, declared, and unplayable. Measured with VP9, AV1 and VP8 into a
//!   `.ts`, and with FLAC and Vorbis; VP8 came out declared as MPEG-4 video,
//!   which is worse again, being a description that is not true.
//! - **It fits and is written.** Everything else.
//!
//! So the transport stream is answered from a list of what it *has* types
//! for, and the others from a list of what they are known to refuse. The
//! asymmetry is the point: a format whose stream types are enumerated by its
//! own standard can be asked what it holds, and a container that says no out
//! loud needs no list to be safe -- the list is only there so the window can
//! grey the answer out before somebody waits through a cut for it.
//!
//! The window asks the same questions through
//! [`crate::cut::holds`](holds), so that what is greyed on the output
//! settings screen and what the engine will write are one answer. See
//! `lockContainer` in `gui/src/app.js`.

use ffmpeg_next as ff;

/// The picture codecs a transport stream has a stream type for.
///
/// MPEG-2 and H.264 are what a broadcast is; HEVC is what a 4K one is; VC-1
/// and MPEG-4 are what a disc can be. Each of those five was cut into a
/// `.ts` and read back. VVC is here on the format's own account rather than
/// on a measurement -- it has a type, and nothing this program does would
/// reach it any other way.
const TS_VIDEO: &[&str] = &[
    "mpeg1video",
    "mpeg2video",
    "mpeg4",
    "h264",
    "hevc",
    "vvc",
    "vc1",
];

/// And the sound.
///
/// `pcm_bluray` is Blu-ray LPCM, which is the one linear PCM a transport
/// stream can declare -- a DVD's becomes it and so does anything this
/// program writes as LPCM into a `.ts`; see `carriage` in [`crate::cut`].
/// Opus is carried with a registration descriptor saying so, which is what
/// makes it readable back; FLAC and Vorbis have no type and are the ones
/// this list is keeping out.
///
/// `pcm_dvd` is a DVD's LPCM as it is read, and is here for the same reason
/// `pcm_bluray` is: a cut copying it into a `.ts` writes it as Blu-ray LPCM
/// (`carriage` again), so the transport stream holds it. Left out, the
/// window greyed `.ts` and `.m2ts` for every DVD with LPCM sound while the
/// cut into either was fine. Nothing that writes a stream declares it as
/// `pcm_dvd` -- the sound-only writer names what it declares, which is never
/// this -- so no stream reaches a transport stream under this name.
const TS_AUDIO: &[&str] = &[
    "aac",
    "aac_latm",
    "ac3",
    "eac3",
    "dts",
    "truehd",
    "mlp",
    "mp1",
    "mp2",
    "mp3",
    "opus",
    "pcm_bluray",
    "pcm_dvd",
];

/// What an MP4 refuses. VP9 and AV1 are written; TrueHD is written on this
/// program's own insistence, which it says out loud (see `outside` in
/// [`crate::cut`]).
///
/// A 4K broadcast's LATM AAC is the one a recording of this program's own
/// kind runs into: the muxer has no tag for it and the cut stopped with
/// "Invalid argument" after planning. Bare MLP and 8-bit PCM are turned away
/// the same way, "Could not find tag", whatever the compliance level -- the
/// unsigned kind a WAV holds and the signed kind (`twos`) an old QuickTime
/// file does alike.
///
/// And the intermediate and lossless picture codecs a camera, an editor or a
/// capture card writes into a `.mov` or a `.mkv`: ProRes, DNxHD, CineForm,
/// DV, HAP, FFV1, Ut Video, HuffYUV, v210, QuickTime Animation and
/// uncompressed pictures, and H.263, Sorenson Spark, the old Microsoft
/// MPEG-4s and Theora, which an MP4 has no tag for either. Each was cut out
/// of a `.mov` or `.mkv` into a `.mp4` with libavformat 61 and turned away
/// at the header, "Could not find tag for codec": the cut planned and then
/// stopped with a bare "Invalid argument". WavPack is the sound's one more.
const MP4_REFUSES: &[&str] = &[
    "vp8", "aac_latm", "mlp", "pcm_u8", "pcm_s8", "prores", "hap", "dnxhd", "cfhd", "dvvideo",
    "v210", "ffv1", "utvideo", "huffyuv", "qtrle", "rawvideo", "h263", "flv1", "msmpeg4v2",
    "msmpeg4v3", "wmv1", "wmv2", "theora", "wavpack",
];

/// The sound a program stream holds, a `.mpg` or a `.vob`: MPEG audio, AC-3,
/// DTS and 16 bit big-endian linear PCM, the packs a DVD's own sound comes
/// in. The muxer turns anything else away at the header with "Invalid
/// argument" -- AAC, E-AC-3, Opus, Vorbis, FLAC and 24 bit PCM among them --
/// and a cut planned and stopped with that and nothing else. Its pictures
/// are not listed: the muxer writes whatever it is given as a video stream.
const PS_AUDIO: &[&str] = &["mp1", "mp2", "mp3", "ac3", "dts", "pcm_s16be"];

/// What a Matroska file refuses, which is one thing: 8-bit PCM is stored the
/// way a WAV stores it, unsigned, and the muxer has no tag for the signed
/// kind an old QuickTime file carries. Asked for as 24 bit it is written
/// wide, which every container here holds.
const MKV_REFUSES: &[&str] = &["pcm_s8"];

/// What a QuickTime file refuses. It takes the MP4 family's pictures except
/// the three WebM carries, and its sound stops at the lossless ones: "vp9
/// only supported in MP4", "VP8 muxing is currently not supported", and a
/// plain refusal of the header for TrueHD, FLAC and Opus.
const MOV_REFUSES: &[&str] = &["vp8", "vp9", "av1", "truehd", "mlp", "flac", "opus", "wavpack"];

/// What a WebM holds, which is the shortest list of the lot and the reason
/// the window has had one container greyed out since before this module.
const WEBM: &[&str] = &["vp8", "vp9", "av1", "opus", "vorbis"];

/// Which container a cut is going into, by the name on the end of it.
///
/// The families rather than the extensions: a `.m2ts` is a transport stream
/// written in Blu-ray's framing, and what it can hold is what a `.ts` can.
/// An empty answer is a name this program does not write, which is nothing
/// to refuse on -- the muxer will have its own opinion and it is welcome to
/// it.
pub fn family(output: &str) -> &'static str {
    match extension(output).as_str() {
        "ts" | "m2ts" | "mts" | "m2t" => "ts",
        // An MP4 by another name, written by the MP4 muxer: see where the
        // cut opens its output.
        "mp4" | "m4v" => "mp4",
        "mov" => "mov",
        "mkv" => "mkv",
        "webm" => "webm",
        "mpg" | "mpeg" | "vob" => "ps",
        _ => "",
    }
}

/// The name's ending, lowercased: what follows the last dot of the file's
/// own name, or nothing where there is no dot.
///
/// The way libavformat picks a muxer by the name (`av_match_ext`), rather
/// than the way [`std::path::Path::extension`] reads one: a file named just
/// `.ts` has no extension to `Path`, which takes it for a hidden file's
/// stem, while libavformat writes it as a transport stream.
pub fn extension(output: &str) -> String {
    std::path::Path::new(output)
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Whether a container of this family can carry a stream in this codec.
///
/// The codec is libav's own name for it, lowercased, which is how every
/// stream this program has read names itself -- see `VideoInfo::codec` and
/// [`name_of`] for the one going the other way.
pub fn holds(family: &str, codec: &str) -> bool {
    match family {
        "ts" => TS_VIDEO.contains(&codec) || TS_AUDIO.contains(&codec),
        "mp4" => !MP4_REFUSES.contains(&codec),
        // The pictures as they are, the sound from the list.
        "ps" => !is_sound(codec) || PS_AUDIO.contains(&codec),
        "mov" => !MOV_REFUSES.contains(&codec),
        "webm" => WEBM.contains(&codec),
        "mkv" => !MKV_REFUSES.contains(&codec),
        // An unknown name is not this module's to refuse.
        _ => true,
    }
}

/// Whether libav knows this codec as one of sound.
fn is_sound(codec: &str) -> bool {
    let Ok(name) = std::ffi::CString::new(codec) else { return false };
    // SAFETY: a lookup in libavcodec's static table by a NUL-terminated
    // name; the answer is a pointer into that table or null.
    unsafe {
        let d = ff::ffi::avcodec_descriptor_get_by_name(name.as_ptr());
        !d.is_null() && (*d).type_ == ff::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO
    }
}

/// The codecs a disc of recordings can describe.
///
/// A BDAV clip index names each stream by a coding type, and there are five
/// of them for pictures. What is not one of the five used to be written as
/// MPEG-2 -- the fallback in [`crate::bdav::video_coding`] -- which is a
/// disc whose index says one thing and whose stream is another.
pub fn disc_holds_video(codec: &str) -> bool {
    matches!(codec, "mpeg1video" | "mpeg2video" | "h264" | "hevc" | "vc1")
}

/// And the sound, where the fallback was AAC for the same reason.
pub fn disc_holds_audio(codec: &str) -> bool {
    matches!(
        codec,
        "aac" | "aac_latm" | "ac3" | "eac3" | "dts" | "truehd" | "mlp" | "pcm_bluray" | "mp1"
            | "mp2" | "mp3"
    )
}

/// The codec a kept sound track goes onto a disc as, for asking
/// [`disc_holds_audio`] before the cut rather than after it: the one asked
/// for (`""` or `"source"` for the recording's own), linear PCM being
/// Blu-ray's, and a DVD's LPCM copied being rewritten as Blu-ray's -- the
/// same rule as `carriage` in [`crate::cut`].
///
/// A transport stream holds Opus and a disc's index has no coding type for
/// it, so a cut onto a disc was refused only once it had been written --
/// and in the window, with every recording of the run after it.
pub fn on_a_disc_as<'a>(codec: &'a str, asked: &'a str) -> &'a str {
    match asked {
        "" | "source" if codec == "pcm_dvd" => "pcm_bluray",
        "" | "source" => codec,
        "lpcm" | "pcm" => "pcm_bluray",
        named => named,
    }
}

/// libav's name for a codec, spelled the way every stream this program has
/// read is spelled: the identifier, lowercased.
pub fn name_of(id: ff::codec::Id) -> String {
    format!("{id:?}").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transport_stream_holds_a_broadcast_and_not_a_webm() {
        // A DVD's LPCM as it is read: the cut writes it as Blu-ray's.
        for codec in ["mpeg2video", "h264", "hevc", "vc1", "aac", "ac3", "mp2", "pcm_dvd"] {
            assert!(holds("ts", codec), "{codec}");
        }
        for codec in ["vp8", "vp9", "av1", "flac", "vorbis"] {
            assert!(!holds("ts", codec), "{codec}");
        }
    }

    #[test]
    fn a_disc_stream_is_a_transport_stream() {
        assert_eq!(family("BDAV/STREAM/00001.m2ts"), "ts");
        assert_eq!(family("/tmp/CUT.TS"), "ts");
        assert_eq!(family("cut.mkv"), "mkv");
        assert_eq!(family("cut.wav"), "");
    }

    #[test]
    fn what_each_of_the_others_turns_away() {
        assert!(!holds("mp4", "vp8"));
        assert!(holds("mp4", "vp9"));
        assert!(holds("mp4", "truehd"));
        assert!(!holds("mp4", "aac_latm") && holds("mov", "aac_latm"));
        assert!(!holds("mov", "vp9"));
        assert!(!holds("mov", "opus"));
        assert!(holds("mov", "ac3"));
        // Matroska, and a container this program does not write.
        assert!(holds("mkv", "vp8") && holds("mkv", "truehd"));
        // Signed 8-bit PCM has a tag only in QuickTime; unsigned is a WAV's
        // and Matroska's.
        assert!(!holds("mp4", "pcm_s8") && !holds("mkv", "pcm_s8") && holds("mov", "pcm_s8"));
        assert!(!holds("mp4", "pcm_u8") && holds("mkv", "pcm_u8"));
        assert!(holds("", "vp8"));
    }

    #[test]
    fn a_name_is_read_the_way_libav_reads_it() {
        assert_eq!(family("/out/.ts"), "ts");
        assert_eq!(family("/out/.M2TS"), "ts");
        assert_eq!(family("/out/ts"), "");
        assert_eq!(family("/out.d/cut"), "");
        assert_eq!(family("a.b.mkv"), "mkv");
        assert_eq!(extension("x/CUT.Mp4"), "mp4");
        assert_eq!(extension("x/cut"), "");
    }

    #[test]
    fn the_intermediate_codecs_and_the_program_stream() {
        for codec in ["prores", "dnxhd", "ffv1", "hap", "dvvideo", "cfhd", "wavpack"] {
            assert!(!holds("mp4", codec) && holds("mkv", codec), "{codec}");
        }
        assert!(holds("mp4", "mjpeg") && holds("mp4", "mpeg2video") && holds("mp4", "hevc"));
        assert!(holds("mov", "prores") && !holds("mov", "wavpack"));
        // A .m4v is written by the MP4 muxer.
        assert_eq!(family("x.m4v"), "mp4");
        // A program stream: its pictures as they come, its sound by list.
        assert_eq!(family("x.VOB"), "ps");
        assert_eq!(family("x.mpg"), "ps");
        for codec in ["mpeg2video", "mpeg1video", "h264", "ac3", "mp2", "dts", "pcm_s16be"] {
            assert!(holds("ps", codec), "{codec}");
        }
        for codec in ["aac", "eac3", "opus", "vorbis", "flac", "pcm_s24be", "pcm_s16le", "truehd"] {
            assert!(!holds("ps", codec), "{codec}");
        }
    }

    #[test]
    fn a_disc_is_asked_about_the_sound_as_it_will_be_written() {
        // A DVD's LPCM copied, and anything asked for as linear PCM, is
        // Blu-ray's; Opus copied is not anything a disc can describe.
        assert!(disc_holds_audio(on_a_disc_as("pcm_dvd", "source")));
        assert!(disc_holds_audio(on_a_disc_as("opus", "lpcm")));
        assert!(disc_holds_audio(on_a_disc_as("opus", "aac")));
        assert!(!disc_holds_audio(on_a_disc_as("opus", "")));
        assert!(holds("ts", "opus"));
        assert!(disc_holds_audio(on_a_disc_as("aac_latm", "source")));
    }

    #[test]
    fn a_codec_is_named_the_way_a_read_stream_names_itself() {
        assert_eq!(name_of(ff::codec::Id::H264), "h264");
        assert_eq!(name_of(ff::codec::Id::PCM_BLURAY), "pcm_bluray");
        assert_eq!(name_of(ff::codec::Id::TRUEHD), "truehd");
    }
}
