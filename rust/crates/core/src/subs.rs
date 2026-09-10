//! The subtitles a preview draws over the picture.
//!
//! Nothing here is part of a cut. A cut carries subtitles from one file to
//! another without ever asking what they say -- see [`crate::pgs`] and
//! [`crate::vobsub`] -- and that is the right way to move them. This is the
//! other question, and the window asks it: **what is on screen at this
//! instant**, so that the frame under the playhead can be shown the way a
//! player would show it. Which is what a cut is judged by: a break that
//! lands in the middle of a line is a break in the wrong place, and the only
//! way to see that is to see the line.
//!
//! Three kinds arrive here and two answers come out.
//!
//! A broadcast carries **ARIB captions**, which are characters and where to
//! put them. Nothing decodes those -- libavcodec has no decoder for them in
//! any build this program is shipped against -- so they are read here, by
//! [`crate::caption::Layout`], and handed over as text with positions. The
//! window draws them with the fonts it already has, which is also why they
//! come out sharp at any size.
//!
//! A disc carries **pictures**: a Blu-ray's display sets and a DVD's units.
//! libavcodec decodes both, and what comes back is one byte per pixel and a
//! table of colours. That is turned into a PNG here -- cropped to the
//! subtitle's own rectangle, which is a fraction of the screen -- because a
//! PNG is what a webview can put over an image without being told anything
//! about palettes.
//!
//! ## Reading a window rather than the file
//!
//! A timeline is scrubbed, and a preview has to answer at whatever instant
//! the pointer lands on. Indexing the whole recording first would be minutes
//! of reading for a disc, most of it never asked about; decoding from
//! scratch at every frame would be a seek and a decode per preview.
//!
//! So a stretch is read at a time -- [`LOOKBACK`] before the instant and
//! [`AHEAD`] after it -- and kept. Scrubbing inside that stretch is a lookup
//! and playback runs off the end of it once a minute. The lookback is what
//! makes the answer right rather than merely quick: a subtitle that went up
//! twenty seconds ago is still on screen, and a reader that started at the
//! playhead would say there was none.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;

use crate::caption;
use crate::vobsub::{self, Drawn};
use crate::Source;

/// How far before the instant a window begins.
///
/// A subtitle is on screen because something before it put it there, and
/// nothing says how long before. Thirty seconds covers every subtitle
/// measured here twice over -- a broadcast caption stands for a few seconds
/// and a disc's for less -- and covers the one that is left up over a long
/// silent shot, which is the case a shorter lookback loses.
pub const LOOKBACK: f64 = 30.0;

/// And how far past it the window is read on.
///
/// Long enough that playback crosses a window edge once a minute rather than
/// once a second, and short enough that a jump costs a fraction of a second.
pub const AHEAD: f64 = 30.0;

/// Which kind of subtitle a track carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A broadcast's ARIB captions: characters, read here.
    Caption,
    /// A Blu-ray's graphics: pictures, decoded by libavcodec.
    Graphics,
    /// A DVD's subtitles: pictures too, and a palette that is not in the
    /// stream. See [`crate::vobsub::Palette`].
    Subpicture,
}

/// One subtitle track, as something to choose from a list.
#[derive(Debug, Clone)]
pub struct Track {
    /// What the recording calls it: a PID in a transport stream, a
    /// substream id on a DVD. The same number the track chooser uses.
    pub id: i32,
    pub kind: Kind,
    pub language: Option<String>,
}

/// Every subtitle track a recording carries, in the order the recording
/// names them.
///
/// Takes the three lists rather than the recording, because the window asks
/// this of a file it has only glanced at: an [`crate::Outline`] carries the
/// same three and no index. See `audio_tracks_of` in the window, which is
/// the same arrangement for the same reason.
pub fn tracks(
    captions: &[crate::CaptionInfo],
    graphics: &[crate::GraphicsInfo],
    subpictures: &[crate::SubpictureInfo],
) -> Vec<Track> {
    let captions = captions.iter().map(|c| Track {
        id: c.pid,
        kind: Kind::Caption,
        language: c.language.clone(),
    });
    let graphics = graphics.iter().map(|g| Track {
        id: g.pid,
        kind: Kind::Graphics,
        language: g.language.clone(),
    });
    let subpictures = subpictures.iter().map(|s| Track {
        id: s.id,
        kind: Kind::Subpicture,
        language: s.language.clone(),
    });
    captions.chain(graphics).chain(subpictures).collect()
}

/// What is on screen: characters to draw, or a picture to put up.
#[derive(Debug, Clone)]
pub enum Shown {
    /// Characters and where they go, on the plane they were laid out for.
    Text {
        plane: (u16, u16),
        runs: Vec<caption::Run>,
    },
    /// A picture, cropped to itself, and where on the screen it sits.
    Picture {
        /// The screen the position is measured against.
        screen: (u16, u16),
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        png: Vec<u8>,
    },
}

/// One thing the stream said, at the moment it said it.
struct Event {
    at: f64,
    /// When it takes itself down, where it says so. A caption says so by
    /// sending the next statement; a disc's subtitle often carries its own
    /// end.
    until: Option<f64>,
    /// `None` for the statement or the display set that only clears the
    /// screen, which is how a subtitle comes down.
    shown: Option<Shown>,
}

/// One subtitle track, read a window at a time.
///
/// Held open between questions: the file stays open, the decoder stays
/// built, and a question inside the window already read is answered without
/// touching the disc.
pub struct Reader {
    url: String,
    start_time: f64,
    /// Which track, by the number the recording gives it.
    id: i32,
    kind: Kind,
    /// The picture the subtitles are drawn over, which is what a disc's
    /// positions are measured against.
    screen: (u16, u16),
    /// A DVD's colours, which are on the disc rather than in the stream.
    palette: Option<vobsub::Palette>,
    /// Which stream the pictures are on. See [`Reader::fill`]: they are the
    /// clock a window is read against.
    video: usize,
    ictx: Option<ff::format::context::Input>,
    /// The stretch [`Reader::events`] covers. Empty until the first read.
    window: Option<(f64, f64)>,
    events: Vec<Event>,
}

impl Reader {
    /// Open a reader on one of the recording's subtitle tracks.
    ///
    /// The palette is read here rather than per window: a DVD keeps it in
    /// the index beside the cells, and reading that is one small read of a
    /// file that is not the stream.
    pub fn open(src: &Source, id: i32) -> Result<Reader> {
        let track = tracks(&src.captions, &src.graphics, &src.subpictures)
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("no subtitle track {id} in this recording"))?;
        let palette = (track.kind == Kind::Subpicture)
            .then(|| {
                crate::dvd::subtitles_of(&src.path)
                    .map(|s| s.palette)
                    .unwrap_or_else(vobsub::Palette::grey)
            });
        Ok(Reader {
            url: src.input.url.clone(),
            start_time: src.start_time,
            id,
            kind: track.kind,
            screen: (src.video.width as u16, src.video.height as u16),
            palette,
            video: src.video.stream_index,
            ictx: None,
            window: None,
            events: Vec::new(),
        })
    }

    /// What is on screen at `t`, reading the recording if it has to.
    pub fn at(&mut self, t: f64) -> Result<Option<&Shown>> {
        let inside = self
            .window
            .is_some_and(|(from, to)| t >= from && t < to);
        if !inside {
            self.fill(t)?;
        }
        // The last thing said before this instant, and only if it is still
        // standing: a subtitle that named its own end has gone by then, and
        // one the next statement replaced is not in the way of it either.
        let i = self.events.partition_point(|e| e.at <= t);
        let event = self.events[..i].iter().rev().find(|e| e.shown.is_some());
        let Some(event) = event else { return Ok(None) };
        if event.until.is_some_and(|u| t >= u) {
            return Ok(None);
        }
        // Anything after it that clears the screen takes it down.
        if self.events[..i].iter().any(|e| e.at > event.at) {
            return Ok(None);
        }
        Ok(event.shown.as_ref())
    }

    /// Read the stretch around `t` into [`Reader::events`].
    fn fill(&mut self, t: f64) -> Result<()> {
        let from = (t - LOOKBACK).max(0.0);
        let to = t + AHEAD;
        let (id, kind, screen, start_time) = (self.id, self.kind, self.screen, self.start_time);
        let video = self.video;
        let ictx = match self.ictx.as_mut() {
            Some(c) => c,
            None => {
                let c = crate::input::demux(&self.url)?;
                // The pictures and the sound are the whole file; nothing
                // here reads a byte of either, so the demuxer is told not
                // to hand them over. Without this a window costs the file.
                //
                // The pictures are the exception, and only their entry
                // points: a window ends at a time, and a track that says
                // nothing for a minute -- a subtitle stream on a scene with
                // no dialogue, or a second language that subtitles almost
                // nothing -- would otherwise be read to the end of the
                // recording looking for a packet to end on. An entry point
                // every half second is a clock to stop against, and costs
                // the demuxer nothing to hand over.
                let keep = id;
                let pictures = video;
                for stream in c.streams() {
                    let mine = stream.parameters().medium() == ff::media::Type::Subtitle
                        && stream.id() == keep;
                    let discard = match () {
                        _ if mine => continue,
                        _ if stream.index() == pictures => ff::Discard::NonKey,
                        _ => ff::Discard::All,
                    };
                    unsafe {
                        (*(stream.as_ptr() as *mut ff::ffi::AVStream)).discard = discard.into();
                    }
                }
                self.ictx.insert(c)
            }
        };
        let target = ((from + self.start_time) * ff::ffi::AV_TIME_BASE as f64) as i64;
        let _ = ictx.seek(target, ..target);

        let mut events: Vec<Event> = Vec::new();
        let mut layout = caption::Layout::default();
        let mut decoder = match kind {
            Kind::Caption => None,
            Kind::Graphics => Some(pictures_decoder(
                ff::codec::Id::HDMV_PGS_SUBTITLE,
                screen,
                None,
            )?),
            Kind::Subpicture => Some(pictures_decoder(
                ff::codec::Id::DVD_SUBTITLE,
                screen,
                self.palette.as_ref(),
            )?),
        };
        let mut units: Vec<u8> = Vec::new();
        // What each stream is, so a packet can be placed without asking the
        // context for its stream while the context is being read from.
        let streams: Vec<(i32, f64, ff::media::Type)> = ictx
            .streams()
            .map(|s| {
                (
                    s.id(),
                    f64::from(s.time_base()),
                    s.parameters().medium(),
                )
            })
            .collect();
        loop {
            // Read by hand rather than through the packet iterator, which
            // treats every error but the end of the file as something to
            // try again: a recording that stops in the middle of a packet
            // -- a capture that was cut off, which is half of what this
            // program is pointed at -- makes that iterator spin.
            let mut packet = ff::Packet::empty();
            if packet.read(ictx).is_err() {
                break;
            }
            let Some(&(stream_id, tb, medium)) = streams.get(packet.stream()) else {
                continue;
            };
            let mine = stream_id == id && medium == ff::media::Type::Subtitle;
            if !mine {
                // A picture's entry point, which is here to be a clock.
                let past = packet
                    .pts()
                    .is_some_and(|pts| pts as f64 * tb - start_time > to);
                if packet.stream() == video && past {
                    break;
                }
                continue;
            }
            let (Some(data), Some(pts)) = (packet.data(), packet.pts()) else {
                continue;
            };
            let at = pts as f64 * tb - start_time;
            if at > to {
                break;
            }
            match decoder.as_mut() {
                None => {
                    // A caption's PES payload can carry several data groups
                    // and only the statements are of any interest. See
                    // [`crate::caption`].
                    caption::data_groups(data, |id, body| {
                        if !caption::is_statement(id) {
                            return;
                        }
                        caption::text_units(body, &mut units);
                        if units.is_empty() {
                            return;
                        }
                        let written = layout.statement(&units);
                        events.push(Event {
                            at,
                            until: None,
                            shown: (!written.runs.is_empty()).then(|| Shown::Text {
                                plane: written.plane,
                                runs: written.runs,
                            }),
                        });
                    });
                }
                Some(decoder) => {
                    let mut sub = ff::codec::subtitle::Subtitle::new();
                    if !decoder.decode(&packet, &mut sub).unwrap_or(false) {
                        continue;
                    }
                    let drawn = vobsub::drawn_from(&sub);
                    events.push(Event {
                        at,
                        until: drawn.as_ref().and_then(|d| d.until).map(|d| at + d),
                        shown: drawn.map(|d| picture(screen, &d)).transpose()?,
                    });
                }
            }
        }
        self.events = events;
        self.window = Some((from, to));
        Ok(())
    }

}

/// One decoded subtitle, as a picture a window can put up.
fn picture(screen: (u16, u16), drawn: &Drawn) -> Result<Shown> {
    Ok(Shown::Picture {
        screen,
        x: drawn.x,
        y: drawn.y,
        width: drawn.width,
        height: drawn.height,
        png: png(drawn)?,
    })
}

/// A decoder for the pictures one of the disc formats carries.
///
/// A DVD's needs the palette handed to it in the text an `.idx` is written
/// in, which is the one thing about a DVD's subtitles that is not in the
/// stream at all; a Blu-ray's carries its own colours and needs only the
/// screen.
fn pictures_decoder(
    id: ff::codec::Id,
    screen: (u16, u16),
    palette: Option<&vobsub::Palette>,
) -> Result<ff::codec::decoder::Subtitle> {
    let mut ctx = ff::codec::context::Context::new();
    let told = palette.map(|p| {
        let colours: Vec<String> = p.0.iter().map(|c| format!("{c:06x}")).collect();
        format!(
            "size: {}x{}\npalette: {}\n",
            screen.0,
            screen.1,
            colours.join(", ")
        )
    });
    unsafe {
        let p = ctx.as_mut_ptr();
        (*p).codec_type = ff::ffi::AVMediaType::AVMEDIA_TYPE_SUBTITLE;
        (*p).codec_id = id.into();
        (*p).width = screen.0 as i32;
        (*p).height = screen.1 as i32;
        if let Some(told) = told {
            let n = told.len();
            let room = n + ff::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
            let buf = ff::ffi::av_mallocz(room) as *mut u8;
            if buf.is_null() {
                return Err(anyhow!("no room for the palette"));
            }
            std::ptr::copy_nonoverlapping(told.as_ptr(), buf, n);
            (*p).extradata = buf;
            (*p).extradata_size = n as i32;
        }
    }
    Ok(ctx.decoder().subtitle()?)
}

/// A decoded subtitle written as a PNG.
///
/// Cropped to the subtitle's own rectangle, which is where it was already:
/// what libavcodec hands back is the picture and where on the screen it
/// goes, and a page can place a small image far more cheaply than it can
/// place a screen-sized one that is transparent nearly everywhere.
fn png(drawn: &Drawn) -> Result<Vec<u8>> {
    let (w, h) = (drawn.width as u32, drawn.height as u32);
    if w == 0 || h == 0 {
        return Err(anyhow!("a subtitle with no picture in it"));
    }
    let codec = ff::encoder::find(ff::codec::Id::PNG).ok_or_else(|| anyhow!("no PNG encoder"))?;
    let mut enc = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()?;
    enc.set_width(w);
    enc.set_height(h);
    enc.set_format(ff::format::Pixel::RGBA);
    enc.set_time_base(ff::Rational::new(1, 25));
    let mut enc = enc.open_as(codec)?;

    let mut frame = ff::frame::Video::new(ff::format::Pixel::RGBA, w, h);
    let stride = frame.stride(0);
    let data = frame.data_mut(0);
    for row in 0..h as usize {
        for col in 0..w as usize {
            let index = drawn.indices[row * w as usize + col] as usize;
            let (r, g, b, a) = drawn.palette.get(index).copied().unwrap_or((0, 0, 0, 0));
            let at = row * stride + col * 4;
            data[at] = r;
            data[at + 1] = g;
            data[at + 2] = b;
            data[at + 3] = a;
        }
    }
    frame.set_pts(Some(0));
    enc.send_frame(&frame)?;
    enc.send_eof()?;
    let mut out = Vec::new();
    let mut packet = ff::Packet::empty();
    while enc.receive_packet(&mut packet).is_ok() {
        out.extend_from_slice(packet.data().unwrap_or(&[]));
        packet = ff::Packet::empty();
    }
    if out.is_empty() {
        return Err(anyhow!("the PNG encoder produced nothing"));
    }
    Ok(out)
}
