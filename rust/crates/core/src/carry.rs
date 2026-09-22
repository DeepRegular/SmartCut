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
];

/// What an MP4 refuses. VP9 and AV1 are written; TrueHD is written on this
/// program's own insistence, which it says out loud (see `outside` in
/// [`crate::cut`]).
const MP4_REFUSES: &[&str] = &["vp8"];

/// What a QuickTime file refuses. It takes the MP4 family's pictures except
/// the three WebM carries, and its sound stops at the lossless ones: "vp9
/// only supported in MP4", "VP8 muxing is currently not supported", and a
/// plain refusal of the header for TrueHD, FLAC and Opus.
const MOV_REFUSES: &[&str] = &["vp8", "vp9", "av1", "truehd", "mlp", "flac", "opus"];

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
    let ext = std::path::Path::new(output)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "ts" | "m2ts" | "mts" | "m2t" => "ts",
        "mp4" | "m4v" => "mp4",
        "mov" => "mov",
        "mkv" => "mkv",
        "webm" => "webm",
        _ => "",
    }
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
        "mov" => !MOV_REFUSES.contains(&codec),
        "webm" => WEBM.contains(&codec),
        // Matroska holds everything that reaches it, and an unknown name is
        // not this module's to refuse.
        _ => true,
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
        for codec in ["mpeg2video", "h264", "hevc", "vc1", "aac", "ac3", "mp2"] {
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
        assert!(!holds("mov", "vp9"));
        assert!(!holds("mov", "opus"));
        assert!(holds("mov", "ac3"));
        // Matroska, and a container this program does not write.
        assert!(holds("mkv", "vp8") && holds("mkv", "truehd"));
        assert!(holds("", "vp8"));
    }

    #[test]
    fn a_codec_is_named_the_way_a_read_stream_names_itself() {
        assert_eq!(name_of(ff::codec::Id::H264), "h264");
        assert_eq!(name_of(ff::codec::Id::PCM_BLURAY), "pcm_bluray");
        assert_eq!(name_of(ff::codec::Id::TRUEHD), "truehd");
    }
}
