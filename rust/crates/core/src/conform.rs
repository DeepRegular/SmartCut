//! What two recordings must have in common to go into one file, and what is
//! done about each thing they have not.
//!
//! Cutting ranges out of one recording never raises the question: every
//! picture in the output came off the same encoder, so every picture the
//! copy carries is describable by the one stream header the output
//! declares. Joining separate recordings raises it at once. A stream is
//! declared before the first packet is written and cannot be taken back, so
//! the file has exactly one answer for the size of a picture, the rate they
//! come at, and the codec they are written in -- and a clip that answers
//! differently cannot be copied into it.
//!
//! **One clip is the master, and the file is its shape.** Every clip that
//! matches it is smart-rendered as it would be on its own: copied, bar the
//! partial GOPs at the ends of each range. Every clip that does not is
//! decoded and written afresh at the master's shape. That is the whole of
//! the arrangement, and it is the one the reference tool reaches by the same
//! reasoning.
//!
//! What counts as matching is decided here, and only here, so that the
//! answer a list is drawn from and the answer the cut acts on cannot differ.

use crate::{AudioInfo, PictureShape, Source, VideoInfo};
use ffmpeg_next as ff;

/// How close two frame rates may be and still be the same rate.
///
/// Relative, because the absolute gap between two rates that differ by a
/// thousandth is 0.03 at 30 and 0.06 at 60. 29.97 and 30 differ by a part in
/// a thousand and are emphatically not the same rate -- one of them is
/// 30000/1001 and the other is 30, and a timeline built on either shows the
/// other drifting a frame every 33 seconds -- so the tolerance has to be
/// well under that. What it is really for is the last figure of a rate that
/// arrived as a decimal.
const RATE_TOLERANCE: f64 = 1e-4;

/// The same, for a pixel aspect ratio, which is not a rate and is often
/// written as a decimal: 1440x1080 broadcast is 4/3 and arrives as
/// 1.3333333333333333.
const ASPECT_TOLERANCE: f64 = 1e-3;

/// One thing a clip does not have in common with the master.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub what: What,
    /// What the master says and what this clip says, as a reader wants them
    /// said rather than as the fields hold them.
    pub master: String,
    pub theirs: String,
}

impl Mismatch {
    /// One line, for a caller with nowhere to put a table.
    pub fn describe(&self) -> String {
        format!("{}: {} vs {}", self.what.describe(), self.master, self.theirs)
    }
}

/// Which property differs.
///
/// Split by what it costs rather than by where it is read: everything under
/// [`What::is_video`] means the pictures are written afresh, and a
/// [`What::Sound`] means one track is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    Codec,
    Size,
    /// How the samples are laid out -- 4:2:0 against 4:2:2, eight bits
    /// against ten. A decoder hands these over as they were coded and an
    /// encoder must be opened for one of them.
    Pixels,
    Rate,
    /// Interlaced against progressive, or one field order against the other.
    Scan,
    Aspect,
    /// Primaries, transfer or matrix. Pictures that disagree about what
    /// their numbers mean look like pictures from two different cameras,
    /// because that is what they are.
    Colour,
    /// One of the sound tracks, by position: the master's nth track against
    /// this clip's nth.
    Sound(usize, Sound),
    /// The clip has fewer sound tracks than the master, or more.
    SoundTracks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    Codec,
    Rate,
    Channels,
}

impl What {
    /// Whether this is about the pictures rather than about the sound.
    ///
    /// Not the same question as [`Self::costs_pictures`], and the difference
    /// is one property wide. This one sorts the differences into the two
    /// places they are said.
    pub fn is_video(self) -> bool {
        !matches!(self, What::Sound(..) | What::SoundTracks)
    }

    /// ...and whether it is a reason to write the pictures afresh.
    ///
    /// **The scan is not.** It is the one property here that no stream
    /// states: libavformat works it out from the pictures it probed, and a
    /// Japanese broadcast carries progressive-coded frames inside a 1080i
    /// stream constantly -- most animation is coded that way. So whether the
    /// frames a probe happened to see were progressive turns on whether the
    /// recording began in the programme or in the commercial before it, and
    /// the same programme two weeks running comes back progressive once and
    /// interlaced once.
    ///
    /// Measured over 1186 recordings off 32 channels: twelve of those
    /// channels hold recordings that are not all one shape and **differ in
    /// nothing but this**, and 40 recordings are the odd one out on a channel
    /// whose others agree. Every one of them was an hour of encoding to join
    /// two episodes of one series -- for a reading rather than for a
    /// difference. The reference tool does not re-cue for it either.
    ///
    /// Nothing is lost by copying them together. Each picture carries its own
    /// flags -- a frame is coded as a frame or as two fields whatever the
    /// container says about the track -- so a decoder reads them as it reads
    /// the recording's own. What the output declares is the master's, which
    /// is what it declares about every other reel as well.
    ///
    /// It is still *reported*: the run says which two readings met and that
    /// the pictures were copied anyway. See [`crate::cut`].
    pub fn costs_pictures(self) -> bool {
        self.is_video() && self != What::Scan
    }

    /// A name for this property that does not move with the prose.
    ///
    /// [`Self::describe`] is the English of it, and a window that has to say
    /// the same thing in another language cannot look a sentence up. So the
    /// two are separate: this one is a key, and it is allowed to be terse.
    pub fn slug(self) -> &'static str {
        match self {
            What::Codec => "codec",
            What::Size => "size",
            What::Pixels => "pixels",
            What::Rate => "rate",
            What::Scan => "scan",
            What::Aspect => "aspect",
            What::Colour => "colour",
            What::Sound(_, Sound::Codec) => "audioCodec",
            What::Sound(_, Sound::Rate) => "audioRate",
            What::Sound(_, Sound::Channels) => "audioChannels",
            What::SoundTracks => "audioTracks",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            What::Codec => "codec",
            What::Size => "frame size",
            What::Pixels => "pixel format",
            What::Rate => "frame rate",
            What::Scan => "scan",
            What::Aspect => "pixel aspect",
            What::Colour => "colour",
            What::Sound(_, Sound::Codec) => "audio codec",
            What::Sound(_, Sound::Rate) => "sample rate",
            What::Sound(_, Sound::Channels) => "channels",
            What::SoundTracks => "audio tracks",
        }
    }
}

/// What has to be done to a clip for it to be written beside the master.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fit {
    /// The pictures are decoded and written afresh at the master's size,
    /// rate, scan and codec. Where this is false the clip is smart-rendered
    /// exactly as it would be cut on its own.
    pub video: bool,
    /// One per sound track the master declares: whether this clip's own
    /// track has to be written afresh to match it. A track the clip does not
    /// have at all is `false` -- there is nothing to re-encode, and the
    /// output carries a gap there.
    pub audio: Vec<bool>,
}

impl Fit {
    /// Everything carried as it is, which is what a clip cut on its own
    /// gets and what the master itself always gets.
    pub fn as_is(tracks: usize) -> Fit {
        Fit {
            video: false,
            audio: vec![false; tracks],
        }
    }

    /// Whether anything at all is written afresh.
    pub fn anything(&self) -> bool {
        self.video || self.audio.iter().any(|&a| a)
    }
}

/// Everything this clip does not have in common with the master, video
/// first.
///
/// The master compared with itself yields nothing, which is the property the
/// callers lean on: a list where every clip matches is a list that joins by
/// copying.
pub fn compare(master: &Source, clip: &Source) -> Vec<Mismatch> {
    of_parts(&master.video, &master.audios, &clip.video, &clip.audios)
}

/// What [`compare`] found, turned into what the cut will do about it.
pub fn fit(master: &Source, clip: &Source) -> Fit {
    let found = compare(master, clip);
    fit_of(&found, master.audios.len(), clip.audios.len())
}

fn of_parts(
    mv: &VideoInfo,
    ma: &[AudioInfo],
    cv: &VideoInfo,
    ca: &[AudioInfo],
) -> Vec<Mismatch> {
    let mut out = video_mismatches(mv, cv);
    out.extend(sound_mismatches(ma, ca));
    out
}

fn fit_of(found: &[Mismatch], master_tracks: usize, clip_tracks: usize) -> Fit {
    let mut fit = Fit {
        video: found.iter().any(|m| m.what.costs_pictures()),
        audio: vec![false; master_tracks],
    };
    for m in found {
        if let What::Sound(nth, _) = m.what {
            if let Some(slot) = fit.audio.get_mut(nth) {
                *slot = true;
            }
        }
    }
    // A track this clip does not carry is not one that can be written
    // afresh: there is nothing to decode. `SoundTracks` says so on its own.
    for slot in fit.audio.iter_mut().skip(clip_tracks) {
        *slot = false;
    }
    fit
}

fn video_mismatches(master: &VideoInfo, clip: &VideoInfo) -> Vec<Mismatch> {
    let mut out = Vec::new();
    let mut say = |what: What, m: String, t: String| {
        if m != t {
            out.push(Mismatch {
                what,
                master: m,
                theirs: t,
            });
        }
    };
    say(What::Codec, master.codec.clone(), clip.codec.clone());
    say(
        What::Size,
        format!("{}x{}", master.width, master.height),
        format!("{}x{}", clip.width, clip.height),
    );
    if stated(master.frame_rate > 0.0, clip.frame_rate > 0.0)
        && !near(master.frame_rate, clip.frame_rate, RATE_TOLERANCE)
    {
        say(
            What::Rate,
            rate(master.frame_rate),
            rate(clip.frame_rate),
        );
    }
    if let (Some(m), Some(c)) = (scan(master), scan(clip)) {
        say(What::Scan, m, c);
    }
    if stated(master.shape.pix_fmt >= 0, clip.shape.pix_fmt >= 0) {
        say(
            What::Pixels,
            pixels(master.shape.pix_fmt),
            pixels(clip.shape.pix_fmt),
        );
    }
    if !same_colour(&master.shape, &clip.shape) {
        say(What::Colour, colour(&master.shape), colour(&clip.shape));
    }
    if stated(
        master.sample_aspect_ratio > 0.0,
        clip.sample_aspect_ratio > 0.0,
    ) && !near(
        master.sample_aspect_ratio,
        clip.sample_aspect_ratio,
        ASPECT_TOLERANCE,
    ) {
        say(
            What::Aspect,
            aspect(master.sample_aspect_ratio),
            aspect(clip.sample_aspect_ratio),
        );
    }
    out
}

/// **A field one of the two recordings does not state is not a difference.**
///
/// Everything above is read out of the container, and a transport stream is
/// not obliged to describe itself. Broadcast MPEG-2 states its colour in a
/// sequence display extension that a recording begun mid-programme can be
/// missing altogether; libavformat works the field order out from the
/// pictures it probed and reports `unknown` where they did not settle it; a
/// stream that never stated a pixel aspect ratio comes back as none at all.
/// These are gaps in the description, and one recording of a programme can
/// have them where the next week's has not.
///
/// Read as differences, they were an hour of encoding apiece. Two recordings
/// off one channel are the same pictures in the same format, and re-encoding
/// one of them because the other happened to say more about itself is work
/// nobody would ask for if they were asked. So a comparison where either side
/// is silent is passed over: the output states what the master states, which
/// is the only claim there was, and the silent recording contradicts nothing.
///
/// This costs a real difference only where one recording states a thing the
/// other actually is *not* -- and a recording that is not what it says it is
/// cannot be told apart from one that does not say, so there is nothing to
/// lose that was ever there to be had.
fn stated(master: bool, clip: bool) -> bool {
    master && clip
}

/// The colour fields, compared one at a time so that a recording which
/// states two of the three and leaves the last unspecified is still the same
/// colour as one that states all three.
///
/// Reported as one phrase all the same -- see [`colour`] -- because what the
/// reader has to decide is the same whichever of the three it was.
fn same_colour(master: &PictureShape, clip: &PictureShape) -> bool {
    // `AVCOL_PRI_UNSPECIFIED`, `AVCOL_TRC_UNSPECIFIED` and
    // `AVCOL_SPC_UNSPECIFIED` are all 2. Zero is not one of them: it is
    // reserved for the primaries and the transfer, and it is RGB for the
    // matrix, which is a claim like any other.
    const UNSPECIFIED: i32 = 2;
    let same = |m: i32, c: i32| m == c || m == UNSPECIFIED || c == UNSPECIFIED;
    same(master.primaries, clip.primaries)
        && same(master.transfer, clip.transfer)
        && same(master.matrix, clip.matrix)
}

fn sound_mismatches(master: &[AudioInfo], clip: &[AudioInfo]) -> Vec<Mismatch> {
    let mut out = Vec::new();
    if master.len() != clip.len() {
        out.push(Mismatch {
            what: What::SoundTracks,
            master: master.len().to_string(),
            theirs: clip.len().to_string(),
        });
    }
    for (nth, m) in master.iter().enumerate() {
        let Some(t) = clip.get(nth) else { break };
        let mut say = |which: Sound, a: String, b: String| {
            if a != b {
                out.push(Mismatch {
                    what: What::Sound(nth, which),
                    master: a,
                    theirs: b,
                });
            }
        };
        say(Sound::Codec, m.codec.clone(), t.codec.clone());
        say(
            Sound::Rate,
            format!("{} Hz", m.sample_rate),
            format!("{} Hz", t.sample_rate),
        );
        say(
            Sound::Channels,
            m.channels.to_string(),
            t.channels.to_string(),
        );
    }
    out
}

fn near(a: f64, b: f64, tolerance: f64) -> bool {
    let scale = a.abs().max(b.abs());
    if scale <= 0.0 {
        return true;
    }
    (a - b).abs() <= tolerance * scale
}

/// A frame rate as a reader states one: 29.97 rather than 29.970029970029973.
fn rate(fps: f64) -> String {
    let rounded = (fps * 100.0).round() / 100.0;
    if (rounded - rounded.round()).abs() < 1e-9 {
        format!("{:.0}", rounded)
    } else {
        format!("{rounded:.2}")
    }
}

fn aspect(sar: f64) -> String {
    if sar <= 0.0 {
        return "square".into();
    }
    let r = ff::Rational::from(sar).reduce();
    format!("{}:{}", r.numerator(), r.denominator())
}

/// The pixel format's own name -- `yuv420p`, `yuv420p10le` -- as libavutil
/// spells it.
///
/// Its own name and not a description of it, because a description would be
/// a table of them kept here and the only reader is a person deciding
/// whether to re-encode a clip. A format libavutil has no name for is
/// unnamed rather than absent: two clips whose formats are both unnamed are
/// still two clips, and comparing the numbers is what decides the question.
fn pixels(pix_fmt: i32) -> String {
    let name = unsafe {
        let p = ff::ffi::av_get_pix_fmt_name(std::mem::transmute::<i32, ff::ffi::AVPixelFormat>(
            pix_fmt,
        ));
        if p.is_null() {
            None
        } else {
            std::ffi::CStr::from_ptr(p).to_str().ok().map(str::to_owned)
        }
    };
    name.unwrap_or_else(|| format!("format {pix_fmt}"))
}

/// The three colour fields as one phrase.
///
/// One phrase and not three mismatches: primaries, transfer and matrix are
/// read together, mean nothing apart, and a clip that differs in one of them
/// almost always differs in all three. What the reader has to decide is the
/// same either way.
fn colour(shape: &PictureShape) -> String {
    let name = |f: unsafe extern "C" fn(i32) -> *const std::os::raw::c_char, v: i32| -> String {
        let p = unsafe { f(v) };
        if p.is_null() {
            return v.to_string();
        }
        unsafe { std::ffi::CStr::from_ptr(p) }
            .to_str()
            .map(str::to_owned)
            .unwrap_or_else(|_| v.to_string())
    };
    format!(
        "{}/{}/{}",
        name(colour_primaries_name, shape.primaries),
        name(colour_transfer_name, shape.transfer),
        name(colour_space_name, shape.matrix),
    )
}

// libavutil takes these as its own enums; the fields hold the plain
// integers the container stated. Wrapped rather than transmuted at each
// call site, which is three casts written once instead of nine.
unsafe extern "C" fn colour_primaries_name(v: i32) -> *const std::os::raw::c_char {
    ff::ffi::av_color_primaries_name(std::mem::transmute::<i32, ff::ffi::AVColorPrimaries>(v))
}

unsafe extern "C" fn colour_transfer_name(v: i32) -> *const std::os::raw::c_char {
    ff::ffi::av_color_transfer_name(std::mem::transmute::<
        i32,
        ff::ffi::AVColorTransferCharacteristic,
    >(v))
}

unsafe extern "C" fn colour_space_name(v: i32) -> *const std::os::raw::c_char {
    ff::ffi::av_color_space_name(std::mem::transmute::<i32, ff::ffi::AVColorSpace>(v))
}

/// Interlaced or not, and which field leads. `None` where the recording does
/// not say.
///
/// The field order is only asked where the scan is interlaced. A
/// progressive stream may carry any of several numbers there depending on
/// what wrote it, and none of them means anything: two progressive streams
/// that disagree about a field order they do not have are not two shapes of
/// picture.
///
/// `AV_FIELD_UNKNOWN` is not "progressive", which is what it used to be read
/// as. libavformat works the field order out from the pictures it probed and
/// leaves it unknown where they did not settle it, so the same broadcast
/// recorded two weeks running can come back interlaced once and unknown once.
/// Called progressive, the second of those was an hour of encoding to make it
/// match the first. See [`stated`].
fn scan(v: &VideoInfo) -> Option<String> {
    if v.field_order == 0 {
        return None;
    }
    if !v.interlaced() {
        return Some("progressive".into());
    }
    if v.top_field_first() {
        Some("interlaced, top field first".into())
    } else {
        Some("interlaced, bottom field first".into())
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn video() -> VideoInfo {
        VideoInfo {
            stream_index: 0,
            codec: "h264".into(),
            width: 1920,
            height: 1080,
            frame_rate: 30000.0 / 1001.0,
            base_rate: 30000.0 / 1001.0,
            has_b_frames: 2,
            time_base: 1.0 / 90_000.0,
            sample_aspect_ratio: 1.0,
            framing: crate::bitstream::NalFraming::AnnexB,
            shape: PictureShape {
                pix_fmt: ff::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P as i32,
                primaries: ff::ffi::AVColorPrimaries::AVCOL_PRI_BT709 as i32,
                transfer: ff::ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709 as i32,
                matrix: ff::ffi::AVColorSpace::AVCOL_SPC_BT709 as i32,
            },
            pulldown: false,
            variable_rate: false,
            field_order: 0,
            bit_rate: Some(17_000_000.0),
            vc1: None,
            field_shape: None,
        }
    }

    fn sound() -> AudioInfo {
        AudioInfo {
            stream_index: 1,
            pid: 0x111,
            language: None,
            codec: "aac".into(),
            sample_rate: 48_000,
            channels: 2,
            bits: 16,
            time_base: 1.0 / 90_000.0,
            bit_rate: Some(192_000),
            said: None,
        }
    }

    /// A clip compared with itself has nothing to answer for, which is what
    /// lets a list of recordings off one recorder join by copying.
    #[test]
    fn the_same_shape_matches() {
        assert!(video_mismatches(&video(), &video()).is_empty());
        assert!(sound_mismatches(&[sound()], &[sound()]).is_empty());
    }

    /// 29.97 and 30 are not the same rate, and the tolerance that lets a
    /// decimal through must not let this through.
    #[test]
    fn drop_frame_is_not_thirty() {
        let mut theirs = video();
        theirs.frame_rate = 30.0;
        theirs.base_rate = 30.0;
        let found = video_mismatches(&video(), &theirs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].what, What::Rate);
        assert_eq!((found[0].master.as_str(), found[0].theirs.as_str()), ("29.97", "30"));
    }

    /// Two progressive streams that disagree about a field order neither of
    /// them has are the same shape of picture.
    #[test]
    fn a_field_order_without_fields_says_nothing() {
        let mut theirs = video();
        theirs.field_order = 1;
        assert!(video_mismatches(&video(), &theirs).is_empty());
    }

    /// ...and a recording that does not say which way its fields run is not
    /// a recording that disagrees with one that does. libavformat leaves the
    /// field order unknown where the pictures it probed did not settle it,
    /// which is the same broadcast on a week when the recorder began a second
    /// later. See `stated`.
    #[test]
    fn an_unstated_field_order_is_not_a_difference() {
        let mut master = video();
        master.field_order = 2;
        let mut theirs = video();
        theirs.field_order = 0;
        assert!(video_mismatches(&master, &theirs).is_empty());
        assert!(video_mismatches(&theirs, &master).is_empty());
    }

    /// The same for the colour, one field at a time: a recording that states
    /// two of the three and leaves the matrix unspecified is the same colour
    /// as one that states all three.
    #[test]
    fn an_unspecified_colour_is_not_a_difference() {
        let mut theirs = video();
        theirs.shape.matrix = ff::ffi::AVColorSpace::AVCOL_SPC_UNSPECIFIED as i32;
        assert!(video_mismatches(&video(), &theirs).is_empty());
        theirs.shape.primaries = ff::ffi::AVColorPrimaries::AVCOL_PRI_UNSPECIFIED as i32;
        theirs.shape.transfer =
            ff::ffi::AVColorTransferCharacteristic::AVCOL_TRC_UNSPECIFIED as i32;
        assert!(video_mismatches(&video(), &theirs).is_empty());
    }

    /// A colour that is *stated* and differs is still a difference. Two
    /// recordings whose pictures mean different things by the same numbers
    /// look like pictures from two cameras, because that is what they are.
    #[test]
    fn a_stated_colour_that_differs_is_a_difference() {
        let mut theirs = video();
        theirs.shape.matrix = ff::ffi::AVColorSpace::AVCOL_SPC_BT470BG as i32;
        let found = video_mismatches(&video(), &theirs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].what, What::Colour);
    }

    /// And a recording that never stated a pixel aspect ratio is not one
    /// that claims square pixels.
    #[test]
    fn an_unstated_aspect_is_not_a_difference() {
        let mut master = video();
        master.sample_aspect_ratio = 4.0 / 3.0;
        let mut theirs = video();
        theirs.sample_aspect_ratio = 0.0;
        assert!(video_mismatches(&master, &theirs).is_empty());
        assert!(video_mismatches(&theirs, &master).is_empty());
    }

    /// A scan read one way on one recording and the other way on the next is
    /// reported and costs nothing: no stream states it, and the pictures
    /// carry their own flags whatever the container was read as saying. See
    /// `costs_pictures`.
    #[test]
    fn a_scan_is_said_and_not_re_encoded() {
        let mut master = video();
        master.field_order = 2;
        let mut theirs = video();
        theirs.field_order = 1;
        let found = video_mismatches(&master, &theirs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].what, What::Scan);
        assert!(!fit_of(&found, 1, 1).video);
        assert!(!fit_of(&found, 1, 1).anything());
    }

    /// ...and it does not hide a difference beside it. A recording of another
    /// size is written afresh whatever its scan was read as.
    #[test]
    fn a_scan_beside_a_real_difference_still_costs() {
        let mut master = video();
        master.field_order = 2;
        let mut theirs = video();
        theirs.field_order = 1;
        theirs.width = 1440;
        let found = video_mismatches(&master, &theirs);
        assert_eq!(found.len(), 2);
        assert!(fit_of(&found, 1, 1).video);
    }

    /// A track the clip does not have is not a track that can be written
    /// afresh -- but the difference is still reported, because the output
    /// declares a track that carries a gap there.
    #[test]
    fn a_missing_track_is_said_and_not_re_encoded() {
        let found = of_parts(&video(), &[sound(), sound()], &video(), &[sound()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].what, What::SoundTracks);
        assert_eq!(fit_of(&found, 2, 1).audio, vec![false, false]);
    }

    /// A clip whose sound is in another codec has that one track written
    /// afresh, and its pictures left alone.
    #[test]
    fn one_track_in_another_codec_costs_one_track() {
        let mut theirs = sound();
        theirs.codec = "ac3".into();
        let found = of_parts(&video(), &[sound()], &video(), &[theirs]);
        let f = fit_of(&found, 1, 1);
        assert!(!f.video);
        assert_eq!(f.audio, vec![true]);
    }

    /// Ten bits and eight are two shapes of picture, whatever else the two
    /// recordings have in common -- and the difference is named the way
    /// libavutil names it, since that is what a person would look up.
    #[test]
    fn bit_depth_is_a_shape() {
        let mut theirs = video();
        theirs.shape.pix_fmt = ff::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P10LE as i32;
        let found = video_mismatches(&video(), &theirs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].what, What::Pixels);
        assert_eq!(
            (found[0].master.as_str(), found[0].theirs.as_str()),
            ("yuv420p", "yuv420p10le")
        );
    }
}
