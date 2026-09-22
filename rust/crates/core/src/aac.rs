//! Which of AAC's two framings a recording uses, and writing frames in it.
//!
//! The codec is the same either way; what differs is the wrapper around each
//! frame. An HD broadcast writes ADTS, a 4K broadcast writes LOAS/LATM, and a
//! frame this tool encodes has to arrive framed the way the frames it is
//! spliced between are. See [`crate::adts`] and [`crate::latm`] for the two.
//!
//! An encoder hands back a raw frame with no wrapper at all, and every muxer
//! involved leaves a packet alone once it holds one -- so the wrapper is put
//! on here, in front of the payload, rather than left to the muxer.

use crate::adts::{AacVersion, AdtsFormat};
use crate::latm::LatmFormat;

/// How the frames of a track are framed, where they are framed at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    Adts(AdtsFormat),
    Latm(LatmFormat),
}

impl Framing {
    /// A raw AAC frame with this framing around it.
    pub fn wrap(&self, payload: &[u8]) -> Vec<u8> {
        match self {
            Framing::Adts(f) => f.wrap(payload),
            Framing::Latm(f) => f.wrap(payload),
        }
    }

    /// Which AAC the frames announce themselves as. Only ADTS says: the bit
    /// that tells MPEG-2 AAC from MPEG-4 AAC is in its header, and LATM
    /// states the object type instead, which both kinds share.
    pub fn as_version(self, want: AacVersion) -> Self {
        match self {
            Framing::Adts(f) => Framing::Adts(f.as_version(want)),
            Framing::Latm(f) => Framing::Latm(f),
        }
    }

    pub fn with_channels(self, channels: u16) -> Self {
        match self {
            Framing::Adts(f) => Framing::Adts(f.with_channels(channels)),
            Framing::Latm(f) => Framing::Latm(f.with_channels(channels)),
        }
    }

    pub fn with_rate(self, rate: u32) -> Self {
        match self {
            Framing::Adts(f) => Framing::Adts(f.with_rate(rate)),
            Framing::Latm(f) => Framing::Latm(f.with_rate(rate)),
        }
    }

    /// What to call it where a reader is told what the recording carries.
    pub fn as_str(&self) -> &'static str {
        match self {
            Framing::Adts(f) if f.mpeg2 => "MPEG-2 ADTS",
            Framing::Adts(_) => "MPEG-4 ADTS",
            Framing::Latm(_) => "LATM",
        }
    }
}

/// Read the framing off the recording's own audio frames.
///
/// Reads forward from wherever the context is, which at the point this is
/// called is the beginning: a transport stream interleaves its audio finely
/// enough that the frames turn up within a few hundred kilobytes.
///
/// Not the *first* audio packet, though -- a recording starts wherever the
/// tuner was told to start, and its opening audio packet is regularly the
/// tail of a frame whose header went out before the recording began. So this
/// keeps looking until a packet parses.
///
/// `None` means the frames carry no wrapper this can write: raw AAC in an
/// MP4, some other codec entirely, or a LATM stream in a shape
/// [`LatmFormat::parse`] declines. Nothing then frames anything, and the
/// frames at a cut's boundaries are copied rather than re-encoded.
pub fn framing(
    ictx: &mut ffmpeg_next::format::context::Input,
    audio_index: usize,
) -> Option<Framing> {
    let mut seen = 0;
    for (stream, packet) in ictx.packets().take(8192) {
        if stream.index() != audio_index {
            continue;
        }
        let Some(data) = packet.data() else { continue };
        if let Some(f) = AdtsFormat::parse(data) {
            return Some(Framing::Adts(f));
        }
        if let Some(f) = LatmFormat::parse(data) {
            return Some(Framing::Latm(f));
        }
        seen += 1;
        if seen > 64 {
            return None;
        }
    }
    None
}

/// As [`framing`], for a recording that is not open.
pub fn of_source(src: &crate::Source) -> Option<Framing> {
    let audio = src.audio.as_ref()?;
    let mut ictx = crate::input::demux(&src.input.url).ok()?;
    // Only this track. See [`crate::input::keep_only`].
    crate::input::keep_only(&mut ictx, &[audio.stream_index]);
    framing(&mut ictx, audio.stream_index)
}
