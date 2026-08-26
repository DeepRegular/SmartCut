//! Where the caption service was reset, which is where a break begins or ends.
//!
//! A Japanese broadcast carries its subtitles as an ARIB STD-B24 stream, and
//! that stream says more than the words. Every time the service starts over
//! -- which is what happens at a junction, because the commercials are not
//! the programme and carry their own captions or none -- the encoder sends a
//! statement that clears the plane and re-declares the display format:
//!
//!   CS(0x0C)  then  CSI…SWF  CSI…SDP  CSI…SDF  CSI…SSM  CSI…SHS  CSI…SVS
//!
//! and writes nothing. A caption *line* looks the same up to that point and
//! then goes on to position the cursor and put characters down, so the two
//! are told apart by what follows the format: nothing, or something.
//!
//! What that buys is a junction time that is not an estimate. The silences
//! give a stretch the cut is somewhere inside, and the logo gives an extent
//! blurred by however long a window was averaged; a reset is one packet with
//! one timestamp, and on real material it lands within 0.16 s of the picture
//! the cut is on -- consistently just before it, since the plane is cleared
//! for the cut rather than by it.
//!
//! Not every broadcaster does this. Of four recordings measured, two mark
//! every junction this way and two never do it at all, so this reads like the
//! logo does: when the marks are there they are worth more than either other
//! signal, and when they are not, [`NoResets`] says so and the caller falls
//! back rather than being handed noise.

use anyhow::Result;
use ffmpeg_next as ff;

use crate::Source;

/// Two resets closer together than this are one junction marked twice --
/// broadcasters re-clear a plane that is already clear. Keep the first.
const MIN_GAP: f64 = 2.0;

/// No caption stream in this recording marks its junctions.
///
/// Several broadcasters never send a bare reset -- their caption encoder
/// clears the plane only as part of writing the next line. Saying so lets
/// the caller fall back to the silences and the logo, which is the same
/// shape [`crate::logo::NoLogo`] has and for the same reason.
#[derive(Debug)]
pub struct NoResets;

impl std::fmt::Display for NoResets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no caption resets in this recording")
    }
}

impl std::error::Error for NoResets {}

/// Walk one PES payload's data groups, handing each to `visit`.
///
/// The payload libav hands over starts at the data identifier, so the PES
/// header is already off. Superimpose (0x81) is skipped: it carries emergency
/// crawls and station bugs, which come and go for their own reasons.
fn data_groups(payload: &[u8], mut visit: impl FnMut(u8, &[u8])) {
    if payload.len() < 3 || payload[0] != 0x80 {
        return;
    }
    let mut i = 3 + (payload[2] & 0x0F) as usize;
    while i + 5 <= payload.len() {
        let id = payload[i] >> 2;
        let size = ((payload[i + 3] as usize) << 8) | payload[i + 4] as usize;
        let Some(body) = payload.get(i + 5..i + 5 + size) else { return };
        visit(id, body);
        i += 5 + size + 2; // + CRC16
    }
}

/// Whether a data group id is caption statement data rather than management
/// data. Two language groups exist, A and B, eight statements each.
fn is_statement(id: u8) -> bool {
    (1..=8).contains(&id) || (0x21..=0x28).contains(&id)
}

/// Concatenate a statement's text data units (`data_unit_parameter` 0x20).
fn text_units(body: &[u8], out: &mut Vec<u8>) {
    out.clear();
    let Some(&first) = body.first() else { return };
    // A time-controlled statement carries a five-byte origin before the loop.
    let tmd = first >> 6;
    let mut i = 1 + if tmd == 1 || tmd == 2 { 5 } else { 0 };
    i += 3; // data_unit_loop_length
    while i + 5 <= body.len() {
        if body[i] != 0x1F {
            return;
        }
        let param = body[i + 1];
        let size = ((body[i + 2] as usize) << 16)
            | ((body[i + 3] as usize) << 8)
            | body[i + 4] as usize;
        let Some(data) = body.get(i + 5..i + 5 + size) else { return };
        if param == 0x20 {
            out.extend_from_slice(data);
        }
        i += 5 + size;
    }
}

/// Whether the bytes are a non-empty run of CSI sequences and nothing else.
///
/// CSI is 0x9B, then digits and semicolons, then a space, then a letter. The
/// format declarations a reset sends are all of this shape; anything a
/// caption writes -- cursor moves, colours, characters -- is not.
fn only_csi(b: &[u8]) -> bool {
    if b.is_empty() {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        if b[i] != 0x9B {
            return false;
        }
        i += 1;
        while i < b.len() && (0x30..=0x3B).contains(&b[i]) {
            i += 1;
        }
        if b.get(i) != Some(&0x20) {
            return false;
        }
        i += 1;
        match b.get(i) {
            Some(&c) if (0x40..=0x7E).contains(&c) => i += 1,
            _ => return false,
        }
    }
    true
}

/// Whether this payload clears the caption plane and writes nothing after.
fn is_reset(payload: &[u8], scratch: &mut Vec<u8>) -> bool {
    let mut found = false;
    data_groups(payload, |id, body| {
        if found || !is_statement(id) {
            return;
        }
        text_units(body, scratch);
        if scratch.first() == Some(&0x0C) && only_csi(&scratch[1..]) {
            found = true;
        }
    });
    found
}

/// Times, in seconds from the start of the recording, at which the caption
/// service was reset.
///
/// Costs one pass over the caption stream's packets and no decoding at all --
/// three seconds on a 3.7 GB recording, against thirty for the logo.
pub fn resets(src: &Source) -> Result<Vec<f64>> {
    resets_with(src, None)
}

/// As [`resets`], reporting how far through the recording it has read.
pub fn resets_with(
    src: &Source,
    mut progress: Option<Box<dyn FnMut(f64) + Send>>,
) -> Result<Vec<f64>> {
    crate::init()?;
    let mut ictx = ff::format::input(&src.path)?;
    let streams: Vec<(usize, f64)> = ictx
        .streams()
        .filter(|s| s.parameters().medium() == ff::media::Type::Subtitle)
        .map(|s| (s.index(), f64::from(s.time_base())))
        .collect();
    if streams.is_empty() {
        return Err(NoResets.into());
    }

    let mut out: Vec<f64> = Vec::new();
    let mut scratch: Vec<u8> = Vec::new();
    let mut told = -1.0;
    for (stream, packet) in ictx.packets() {
        let Some(&(_, tb)) = streams.iter().find(|(i, _)| *i == stream.index()) else {
            continue;
        };
        let (Some(data), Some(pts)) = (packet.data(), packet.pts()) else { continue };
        let t = pts as f64 * tb - src.start_time;
        if is_reset(data, &mut scratch) {
            out.push(t);
        }
        if let Some(f) = progress.as_mut() {
            let done = (t / src.duration.max(1e-9)).clamp(0.0, 1.0);
            if done - told >= 0.02 {
                told = done;
                f(done);
            }
        }
    }
    if let Some(f) = progress.as_mut() {
        f(1.0);
    }

    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    out.dedup_by(|later, earlier| *later - *earlier < MIN_GAP);
    if out.is_empty() {
        return Err(NoResets.into());
    }
    Ok(out)
}
