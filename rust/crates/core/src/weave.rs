//! Pictures back into the frames a screen shows, for a recording that repeats
//! fields.
//!
//! 24 fps film goes out in a 29.97 stream by `repeat_first_field`: every other
//! picture is shown for three fields rather than two, so four pictures fill
//! five frames. A television, a DVD player and the reference tool all show
//! those five frames, and two of each five are woven from two different
//! pictures -- the last field of one and the first of the next -- which is the
//! comb anybody checking the recording expects to see.
//!
//! The decoder hands over the four pictures and a count of the fields each
//! stands for, nothing more. Shown as they are, the film looks cleaner than
//! the recording is, and it does not fill the frame grid the editor steps
//! along: one frame in five has no picture of its own. The film strip drew
//! that frame black and a step went straight past it.
//!
//! So where the recording carries pulldown, the pictures are laid out field by
//! field on the recording's own clock and read off again two fields at a time.
//! **The frames are paired from the first access point**, which is where the
//! editor's timeline begins and what its frame count is measured from, so the
//! frames made here land exactly on the frames it counts.
//!
//! What this does not touch is the cut. A copied picture is copied with its
//! flags, so the output repeats exactly the fields the recording did, and a
//! re-encoded one is given the same count back; see `cut::place`. This is
//! only about what is put on the screen.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;
use std::rc::Rc;

use crate::Source;

/// Whether the pictures of this recording are to be woven into frames.
///
/// Only where the walk found a picture shown for other than two fields. Every
/// other recording already has a picture per frame and is left exactly as it
/// was: nothing here is so much as constructed for it.
pub fn woven(src: &Source) -> bool {
    src.video.pulldown
}

/// Where the frame holding the instant `t` begins.
///
/// On a woven recording a picture can begin half way through a frame -- a
/// picture that follows a three-field one starts on the field after a frame
/// boundary -- and so can an access point or a scene change, which are
/// pictures. The editor steps from frame to frame, so anything it is to land
/// on has to be named by the frame it falls in; handed the picture's own
/// instant, a step forward from that frame found the same point still ahead
/// of it and never moved.
///
/// A picture that begins half way through a frame belongs to that frame, not
/// the next one: the frame is where its first field is on screen, and a cut
/// placed there copies from that picture on. Identity everywhere else.
pub fn on_frame(src: &Source, t: f64) -> f64 {
    if !woven(src) {
        return t;
    }
    let fd = src.video.frame_duration();
    let head = anchor(src).unwrap_or(0.0);
    // A quarter of a frame of slack either way of a boundary: the instants
    // come off a 90 kHz clock, and a field is 1501.5 ticks of it.
    head + ((t - head) / fd + 0.25).floor() * fd
}

/// The instant frames are paired from: the first access point, which is where
/// the editor's timeline begins.
fn anchor(src: &Source) -> Option<f64> {
    src.points.first().map(|p| p.time)
}

/// One field of the recording: the picture it is taken from, and which of
/// the picture's two fields it is.
struct Field {
    /// Counted from the anchor, in fields.
    at: i64,
    picture: Rc<ff::frame::Video>,
    top: bool,
}

/// Pictures in, frames out. See the module notes.
pub struct Weave {
    /// Half a frame, in seconds.
    field: f64,
    head: Option<f64>,
    /// The first field of a frame, waiting for the second.
    open: Option<Field>,
    /// The last frame handed out, in fields, so that a picture whose clock
    /// jumped back cannot hand the same frame out twice.
    last: Option<i64>,
    /// Whether a picture has been seen at all, which is what tells the first
    /// picture of a walk from one arriving in the middle of it.
    started: bool,
}

impl Weave {
    pub fn new(src: &Source) -> Self {
        Self::with(src.video.frame_duration(), anchor(src))
    }

    /// Frames `frame` seconds long, paired from `head` -- or from the first
    /// picture, where there is no access point to pair from.
    fn with(frame: f64, head: Option<f64>) -> Self {
        Weave {
            field: frame / 2.0,
            head,
            open: None,
            last: None,
            started: false,
        }
    }

    /// Take the next picture, in presentation order, at its own instant, and
    /// answer with the frames it completes, each at its own instant.
    pub fn push(
        &mut self,
        t: f64,
        picture: ff::frame::Video,
    ) -> Result<Vec<(f64, Rc<ff::frame::Video>)>> {
        let head = *self.head.get_or_insert(t);
        // Two fields normally; three where the first is repeated. A
        // progressive sequence repeats whole frames instead and says so as
        // two or four -- the fields laid out below are then all the same
        // picture's, and come back as that picture again on each frame.
        // VC-1 repeats a progressive frame up to three times, which is six.
        let fields = 2 + i64::from(unsafe { (*picture.as_ptr()).repeat_pict }.clamp(0, 6));
        let first_top = unsafe {
            (*picture.as_ptr()).flags & ff::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST != 0
        };
        let from = ((t - head) / self.field).round() as i64;
        let picture = Rc::new(picture);
        let first = !std::mem::replace(&mut self.started, true);

        let mut out = Vec::new();
        for i in 0..fields {
            let field = Field {
                at: from + i,
                picture: picture.clone(),
                top: first_top ^ (i % 2 == 1),
            };
            if field.at.rem_euclid(2) == 0 {
                // A frame begins here. One still open never got its second
                // field -- the fields stopped alternating -- and is shown as
                // the picture it came from. Unless it is open on this very
                // field: the picture before claimed a field that this one's
                // timestamp says is its own, and the frame is this one's.
                // Shown as the one before, this picture's second field found
                // that frame already out and it was never shown at all.
                if let Some(open) = self.open.take() {
                    if open.at != field.at {
                        self.emit_whole(open, &mut out);
                    }
                }
                self.open = Some(field);
                continue;
            }
            match self.open.take() {
                Some(open) if open.at == field.at - 1 => self.emit_pair(open, field, &mut out)?,
                Some(open) => {
                    self.emit_whole(open, &mut out);
                }
                // The second half of a frame whose first half is not here.
                // At the start of a walk that is the picture before the seek
                // landed, which nobody decoded; the frame is shown as this
                // picture rather than not at all, because it is the frame an
                // access point falls in and that is exactly where a walk
                // starts. Anywhere else it is a field with nothing to pair
                // with, and it goes.
                None if first && i == 0 => {
                    let at = field.at - 1;
                    self.emit(at, field.picture, &mut out);
                }
                None => {}
            }
        }
        Ok(out)
    }

    /// Whatever is still open, once no more pictures are coming.
    pub fn finish(&mut self) -> Vec<(f64, Rc<ff::frame::Video>)> {
        let mut out = Vec::new();
        if let Some(open) = self.open.take() {
            self.emit_whole(open, &mut out);
        }
        out
    }

    fn emit_whole(&mut self, field: Field, out: &mut Vec<(f64, Rc<ff::frame::Video>)>) {
        self.emit(field.at, field.picture, out);
    }

    fn emit_pair(
        &mut self,
        a: Field,
        b: Field,
        out: &mut Vec<(f64, Rc<ff::frame::Video>)>,
    ) -> Result<()> {
        // Both halves out of one picture, or two fields of the same parity --
        // a stream whose fields stopped alternating, which a weave would only
        // turn into a picture with half its lines missing.
        if Rc::ptr_eq(&a.picture, &b.picture) || a.top == b.top {
            self.emit(a.at, a.picture, out);
            return Ok(());
        }
        let (top, bottom) = if a.top {
            (&a.picture, &b.picture)
        } else {
            (&b.picture, &a.picture)
        };
        // Everything but the lines is the second picture's. It is the one
        // that begins in this frame -- the first began a frame or more
        // earlier -- so its kind is the frame's: the frame an I picture's
        // first field is in is the frame a cut copies from.
        //
        // Two shapes of picture either side of a change of programme have no
        // frame made of both, and the first of them is shown instead.
        match interleave(top, bottom, &b.picture) {
            Ok(woven) => self.emit(a.at, Rc::new(woven), out),
            Err(_) => self.emit(a.at, a.picture, out),
        }
        Ok(())
    }

    fn emit(
        &mut self,
        at: i64,
        picture: Rc<ff::frame::Video>,
        out: &mut Vec<(f64, Rc<ff::frame::Video>)>,
    ) {
        if self.last.is_some_and(|l| at <= l) {
            return;
        }
        self.last = Some(at);
        let head = self.head.unwrap_or(0.0);
        out.push((head + at as f64 * self.field, picture));
    }
}

/// A frame made of the even lines of `top` and the odd lines of `bottom`, with
/// everything else about it taken from `props`.
///
/// Line by line in every plane, chroma included: interlaced 4:2:0 carries its
/// chroma lines by field as well, the even ones with the top field.
fn interleave(
    top: &ff::frame::Video,
    bottom: &ff::frame::Video,
    props: &ff::frame::Video,
) -> Result<ff::frame::Video> {
    if top.format() != bottom.format()
        || top.width() != bottom.width()
        || top.height() != bottom.height()
    {
        return Err(anyhow!("fields of different shapes"));
    }
    let mut out = ff::frame::Video::new(top.format(), top.width(), top.height());
    unsafe {
        ff::ffi::av_frame_copy_props(out.as_mut_ptr(), props.as_ptr());
    }
    for plane in 0..top.planes() {
        let rows = top.plane_height(plane) as usize;
        let (ts, bs, os) = (top.stride(plane), bottom.stride(plane), out.stride(plane));
        let width = ts.min(bs).min(os);
        let (td, bd) = (top.data(plane), bottom.data(plane));
        let od = out.data_mut(plane);
        for r in 0..rows {
            let (from, stride) = if r % 2 == 0 { (td, ts) } else { (bd, bs) };
            od[r * os..r * os + width].copy_from_slice(&from[r * stride..r * stride + width]);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FD: f64 = 1001.0 / 30000.0;

    /// A 4x4 grey picture of one level, shown for `fields` fields, first field
    /// top or not.
    fn picture(level: u8, fields: i32, top_first: bool) -> ff::frame::Video {
        crate::init().unwrap();
        let mut f = ff::frame::Video::new(ff::format::Pixel::YUV420P, 4, 4);
        for plane in 0..f.planes() {
            f.data_mut(plane).fill(level);
        }
        unsafe {
            let p = f.as_mut_ptr();
            (*p).repeat_pict = fields - 2;
            if top_first {
                (*p).flags |= ff::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST;
            } else {
                (*p).flags &= !ff::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST;
            }
        }
        f
    }

    /// The level of each line of the luma, top to bottom.
    fn lines(f: &ff::frame::Video) -> Vec<u8> {
        (0..4).map(|r| f.data(0)[r * f.stride(0)]).collect()
    }

    #[test]
    fn four_film_pictures_fill_five_frames() {
        // TBT, BT, BTB, TB: the 2:3 cadence as broadcast film carries it.
        let mut w = Weave::with(FD, Some(0.0));
        let pics = [(10, 3, true, 0), (20, 2, false, 3), (30, 3, false, 5), (40, 2, true, 8)];
        let mut out = Vec::new();
        for (level, fields, top, at) in pics {
            let t = at as f64 * FD / 2.0;
            out.extend(w.push(t, picture(level, fields, top)).unwrap());
        }
        out.extend(w.finish());
        let times: Vec<f64> = out.iter().map(|(t, _)| (t / FD * 100.0).round() / 100.0).collect();
        assert_eq!(times, vec![0.0, 1.0, 2.0, 3.0, 4.0]);
        let shown: Vec<Vec<u8>> = out.iter().map(|(_, f)| lines(f)).collect();
        assert_eq!(shown[0], vec![10, 10, 10, 10]);
        // The repeated top field of the first picture over the bottom field
        // of the second: the comb.
        assert_eq!(shown[1], vec![10, 20, 10, 20]);
        // The second's top field over the third's bottom.
        assert_eq!(shown[2], vec![20, 30, 20, 30]);
        assert_eq!(shown[3], vec![30, 30, 30, 30]);
        assert_eq!(shown[4], vec![40, 40, 40, 40]);
    }

    #[test]
    fn pictures_of_two_fields_come_back_as_they_went_in() {
        let mut w = Weave::with(FD, Some(0.0));
        let mut out = Vec::new();
        for i in 0..6 {
            let pic = picture(i as u8, 2, true);
            let ptr = unsafe { pic.as_ptr() };
            let got = w.push(i as f64 * FD, pic).unwrap();
            // The very picture, not a copy of it.
            assert!(got.iter().all(|(_, f)| unsafe { f.as_ptr() } == ptr));
            out.extend(got);
        }
        out.extend(w.finish());
        assert_eq!(out.len(), 6);
        for (i, (t, _)) in out.iter().enumerate() {
            assert!((t - i as f64 * FD).abs() < 1e-9);
        }
    }

    #[test]
    fn a_picture_that_overlaps_a_repeated_field_is_still_shown() {
        // The first picture says three fields and the second's timestamp
        // says it begins on the third: the frame there is the second's.
        let mut w = Weave::with(FD, Some(0.0));
        let mut out = w.push(0.0, picture(10, 3, true)).unwrap();
        out.extend(w.push(FD, picture(20, 2, true)).unwrap());
        out.extend(w.finish());
        assert_eq!(out.len(), 2);
        assert_eq!(lines(&out[1].1), vec![20, 20, 20, 20]);
    }

    #[test]
    fn a_frame_repeated_three_times_fills_four_frames() {
        // VC-1's RPTFRM of 3 on a progressive stream.
        let mut w = Weave::with(FD, Some(0.0));
        let mut out = w.push(0.0, picture(10, 8, true)).unwrap();
        out.extend(w.push(4.0 * FD, picture(20, 2, true)).unwrap());
        out.extend(w.finish());
        let times: Vec<f64> = out.iter().map(|(t, _)| (t / FD * 100.0).round() / 100.0).collect();
        assert_eq!(times, vec![0.0, 1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn a_walk_that_lands_on_the_second_field_of_a_frame_still_shows_it() {
        // The seek landed on a picture beginning half way through a frame --
        // an access point after a three-field picture -- with the picture
        // before it never decoded.
        let mut w = Weave::with(FD, Some(0.0));
        let got = w.push(1.5 * FD, picture(50, 2, false)).unwrap();
        assert_eq!(got.len(), 1);
        assert!((got[0].0 - FD).abs() < 1e-9);
        assert_eq!(lines(&got[0].1), vec![50, 50, 50, 50]);
    }
}
