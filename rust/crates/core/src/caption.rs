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

use std::collections::HashMap;

use anyhow::Result;
use ffmpeg_next as ff;

use crate::Source;

/// Two resets closer together than this are one junction marked twice --
/// broadcasters re-clear a plane that is already clear. Keep the first.
const MIN_GAP: f64 = 2.0;

/// How many marks a recording needs before its caption stream counts as
/// marking junctions at all.
///
/// A channel that marks them marks every one, so the count follows from how
/// long the recording is rather than from luck: the two measured channels
/// that do this sent 13 and 35 across half an hour. A channel that does not
/// sends none -- except that the caption service itself starts and stops, and
/// a stop leaves a reset behind with no junction under it.
///
/// One such stray is what this exists for. A BS 日テレ recording carried
/// captions for its first twenty seconds and nothing afterwards, and the
/// single reset that ended them read as "this broadcaster marks its
/// junctions" -- which is read *exclusively*, since neither the silences nor
/// the logo are looked at once marks are found. Half an hour of commercials
/// went undetected on the strength of one mark with no junction under it.
///
/// Three is near neither side of the measured gap.
const MIN_MARKS: usize = 3;

/// No caption stream in this recording marks its junctions.
///
/// Several broadcasters never send a bare reset -- their caption encoder
/// clears the plane only as part of writing the next line. Saying so lets
/// the caller fall back to the silences and the logo, which is the same
/// shape [`crate::logo::NoLogo`] has and for the same reason.
///
/// Too few marks says the same thing: see [`MIN_MARKS`].
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
pub fn data_groups(payload: &[u8], mut visit: impl FnMut(u8, &[u8])) {
    if payload.len() < 3 || payload[0] != 0x80 {
        return;
    }
    let mut i = 3 + (payload[2] & 0x0F) as usize;
    while i + 5 <= payload.len() {
        let id = payload[i] >> 2;
        let size = ((payload[i + 3] as usize) << 8) | payload[i + 4] as usize;
        let Some(body) = payload.get(i + 5..i + 5 + size) else {
            return;
        };
        visit(id, body);
        i += 5 + size + 2; // + CRC16
    }
}

/// Whether a data group id is caption statement data rather than management
/// data. Two language groups exist, A and B, eight statements each.
pub fn is_statement(id: u8) -> bool {
    (1..=8).contains(&id) || (0x21..=0x28).contains(&id)
}

/// Walk a statement's data units, handing each to `visit` with the
/// parameter that says what it is.
///
/// A statement is not only its text. The words are one data unit and the
/// pictures of the characters no code stands for are others, side by side
/// in the same loop -- which is why this is a walk rather than a reader for
/// the text alone. See [`text_units`] and [`Layout::glyphs`].
fn data_units(body: &[u8], mut visit: impl FnMut(u8, &[u8])) {
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
        let size =
            ((body[i + 2] as usize) << 16) | ((body[i + 3] as usize) << 8) | body[i + 4] as usize;
        let Some(data) = body.get(i + 5..i + 5 + size) else {
            return;
        };
        visit(param, data);
        i += 5 + size;
    }
}

/// Concatenate a statement's text data units (`data_unit_parameter` 0x20).
pub fn text_units(body: &[u8], out: &mut Vec<u8>) {
    out.clear();
    data_units(body, |param, data| {
        if param == 0x20 {
            out.extend_from_slice(data);
        }
    });
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
    let mut ictx = crate::input::demux(&src.input.url)?;
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
        let (Some(data), Some(pts)) = (packet.data(), packet.pts()) else {
            continue;
        };
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
    if out.len() < MIN_MARKS {
        return Err(NoResets.into());
    }
    Ok(out)
}

/// A character a broadcaster sent the picture of.
///
/// **This is what a caption line ending in `〓` was.** ARIB calls them DRCS,
/// and what they hold is not exotic: an arrow saying the sentence carries on
/// into the next line, and the double brackets a speaker's name sits in.
/// There is no code for them in any of the sets, so the broadcaster sends
/// the dots and then writes the cell it put them in -- and a reader with no
/// dots has nothing to show but the geta mark, which is what this program
/// showed. The dots arrive in the same statement as the words, a data unit
/// along; see [`Layout::glyphs`].
///
/// One bit a dot, which is not what the standard sends. A glyph may be sent
/// in up to four shades, and the shades are a colour map away from a colour
/// -- but what every glyph measured here uses them for is the anti-aliasing
/// around a solid shape, drawn at 36 dots where it is shown at a fraction of
/// that. So a dot is ink or it is not, and the shades go the way the
/// ornaments and the colour maps went in [`Layout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glyph {
    /// The dots across and down, which is the character cell the format
    /// declared: 36 by 36 on the broadcasts measured here.
    pub width: u8,
    pub height: u8,
    /// One bit a dot, rows in order from the top, each row starting on a
    /// byte and the high bit of a byte the leftmost dot in it.
    pub ink: Vec<u8>,
}

impl Glyph {
    /// How many bytes a row takes.
    pub fn stride(&self) -> usize {
        (self.width as usize).div_ceil(8)
    }

    /// Whether the dot at `x`, `y` is ink.
    pub fn at(&self, x: u8, y: u8) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let bit = y as usize * self.stride() * 8 + x as usize;
        self.ink
            .get(bit / 8)
            .is_some_and(|b| b >> (7 - bit % 8) & 1 == 1)
    }
}

/// Read one DRCS data unit into `into`, under the cells it names.
///
/// `wide` says which of the two downloaded sets this unit is for, because
/// the same code can be defined in both; it is the data unit's own
/// parameter, 0x30 for the one-byte sets and 0x31 for the two-byte one.
/// `cell` is the character cell the format declared, which is how a font is
/// chosen where a broadcaster sends the same glyph at more than one size.
///
/// A unit carries any number of cells, each with any number of fonts, and
/// only the fonts that are dots are read: a glyph may also be sent as
/// geometry, which nothing here draws, and a unit that reaches one stops
/// rather than guessing where the next cell begins.
fn read_glyphs(
    data: &[u8],
    wide: bool,
    cell: (u16, u16),
    into: &mut HashMap<crate::arib::Drcs, Glyph>,
) {
    let Some(&codes) = data.first() else { return };
    let mut i = 1;
    for _ in 0..codes {
        let Some(head) = data.get(i..i + 3) else {
            return;
        };
        let code = u16::from(head[0]) << 8 | u16::from(head[1]);
        let fonts = head[2];
        i += 3;
        let mut best: Option<(bool, Glyph)> = None;
        for _ in 0..fonts {
            let Some(&mode) = data.get(i) else { return };
            i += 1;
            // The two modes that are dots. The others are geometry and a
            // compression this has never seen sent.
            if mode & 0x0F > 1 {
                return;
            }
            let Some(shape) = data.get(i..i + 3) else {
                return;
            };
            let (depth, width, height) = (shape[0], shape[1], shape[2]);
            i += 3;
            // The shades are `depth` + 2, and a dot is as many bits as it
            // takes to count them.
            let bits = (u32::BITS - (u32::from(depth) + 1).leading_zeros()).max(1) as usize;
            let dots = width as usize * height as usize;
            let Some(pattern) = data.get(i..i + (dots * bits).div_ceil(8)) else {
                return;
            };
            i += pattern.len();
            // A cell may be sent at several sizes. The one that is kept is
            // the one the size of the character cell -- the rest are the
            // same glyph for the sizes a caption asks for with a control
            // code, and those are drawn by scaling this one -- and the
            // first of them where none of them is.
            let matches = u16::from(width) == cell.0 && u16::from(height) == cell.1;
            if best.as_ref().is_none_or(|(was, _)| !was && matches) {
                best = Some((matches, ink(pattern, bits, width, height)));
            }
        }
        if let Some((_, glyph)) = best {
            // What a *later statement* sends replaces this, which is how
            // the material is actually sent: one cell, redefined line by
            // line -- an arrow in this one, a bracket in the next.
            into.insert(
                crate::arib::Drcs {
                    // A cell is named in the graphic-left range wherever it
                    // was written from; see [`crate::arib::Drcs`].
                    code: code & 0x7F7F,
                    wide,
                },
                glyph,
            );
        }
    }
}

/// Turn a glyph's shades into ink: any shade but the first is a dot that is
/// drawn, and the first is the one the picture shows through.
fn ink(pattern: &[u8], bits: usize, width: u8, height: u8) -> Glyph {
    let stride = (width as usize).div_ceil(8);
    let mut out = vec![0u8; stride * height as usize];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let at = (y * width as usize + x) * bits;
            let shade = (0..bits).any(|k| {
                let bit = at + k;
                pattern
                    .get(bit / 8)
                    .is_some_and(|b| b >> (7 - bit % 8) & 1 == 1)
            });
            if shade {
                out[y * stride + x / 8] |= 0x80 >> (x % 8);
            }
        }
    }
    Glyph {
        width,
        height,
        ink: out,
    }
}

/// One run of characters on the caption plane: what it says, where it sits
/// and what it is drawn in.
///
/// Everything is in the plane's own dots -- see [`Layout::plane`] -- so that
/// a window drawing this over a picture can scale the lot by one number.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// The top left of the first character's box.
    pub x: u16,
    pub y: u16,
    /// The whole run's box: `advance` times however many characters, by the
    /// height of one of them.
    pub width: u16,
    pub height: u16,
    /// How far apart the characters are. A run is broken wherever this
    /// changes, so one number covers all of it -- which is what lets a
    /// renderer place every character without knowing a font's metrics.
    pub advance: u16,
    pub text: String,
    /// The picture of one character, where the broadcaster sent one rather
    /// than a code. A run carrying this is that character and nothing else,
    /// and its `text` is empty: see [`Glyph`].
    pub glyph: Option<Glyph>,
    /// What the broadcaster asked for it to be drawn in, `0xRRGGBB`.
    pub colour: u32,
}

/// One page a statement puts up, and how long it stands.
///
/// Both times are counted from the statement's own moment, because that is
/// the only clock a statement carries: it writes, waits out the time `TIME`
/// names, and clears. A page that is never cleared leaves `until` empty and
/// stands until the next statement replaces it, which is how most of a
/// broadcast's captions come down.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    /// When it goes up, in seconds after the statement.
    pub at: f32,
    /// And when it comes down, where the statement says so.
    pub until: Option<f32>,
    /// What is on the plane while it is up.
    pub runs: Vec<Run>,
}

/// What one caption statement puts on the plane.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Written {
    /// The plane the pages are placed on, in dots. 960 x 540 for the high
    /// definition captions every broadcaster measured here sends.
    pub plane: (u16, u16),
    /// Empty for the statement that only clears the plane, which is what a
    /// broadcaster sends when the caption comes down and at every junction.
    /// See [`resets`].
    ///
    /// More than one where a statement paces itself: a page, a wait, a
    /// clear, and another page. That is how a line that is timed to the
    /// speech rather than to the next statement is sent.
    pub pages: Vec<Page>,
}

/// The eight colours a caption names directly, in ARIB's own order.
///
/// The standard's full table is a hundred and twenty-eight colours across
/// eight maps, and a caption reaches most of them through `COL`. These are
/// the eight the code itself names -- `RDF`, `YLF` and the rest -- and the
/// first eight of every map, which is what a broadcast's own text is
/// written in.
const COLOURS: [u32; 8] = [
    0x000000, 0xFF0000, 0x00FF00, 0xFFFF00, 0x0000FF, 0xFF00FF, 0x00FFFF, 0xFFFFFF,
];

/// How wide and how tall the plane is, for each screen format a caption can
/// declare.
///
/// The one every recording here carries is 7, and the plane it names can be
/// read off the broadcast itself: a display area of 620 x 480 placed at
/// (170, 30) is centred exactly in 960 x 540, top and bottom and both
/// sides. The others are the standard's, in the same shape.
fn plane_of(swf: u16) -> Option<(u16, u16)> {
    match swf {
        5 => Some((1920, 1080)),
        7 => Some((960, 540)),
        9 => Some((720, 480)),
        11 => Some((1280, 720)),
        _ => None,
    }
}

/// How big a character is drawn, which is a size the text asks for rather
/// than one the format declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Size {
    Normal,
    /// Half the width, the full height. What a caption with more than
    /// fifteen characters on a line is written in.
    Middle,
    /// Half of both.
    Small,
}

impl Size {
    /// How much of a character field this size takes, across and down.
    fn scale(self) -> (f32, f32) {
        match self {
            Size::Normal => (1.0, 1.0),
            Size::Middle => (0.5, 1.0),
            Size::Small => (0.5, 0.5),
        }
    }
}

/// Reads a caption service into what is on screen, statement by statement.
///
/// The format is the reason this is a thing to be kept rather than a
/// function. A broadcaster declares the plane, the display area, the size of
/// a character and the space around it *ahead of* the text -- and having
/// declared it, may not say it again for the rest of the programme. Every
/// recording measured here re-declares all six at the head of every
/// statement, which is what makes captions readable from the middle of a
/// file at all; the ones that do not are read correctly only by a reader
/// that remembers.
///
/// What it does not do is draw. What comes back is the characters and where
/// they go, which is what a window can put over a picture with the fonts it
/// already has -- see [`Written`]. The parts of the format that are about
/// *how* rather than *where* are read past: the ornament around a glyph, the
/// colour maps beyond the first, flashing, and the downloaded glyphs of
/// DRCS, which have no character to stand for them. So is the *size* of the
/// writing area, which only a renderer that clipped a line would need -- a
/// broadcaster's own line fits the area it declared, and what is wanted here
/// is where the line goes.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The whole caption plane, in dots.
    plane: (u16, u16),
    /// Where in the plane a caption begins: the top left of the area it may
    /// be written in, which is where the pen goes home to.
    origin: (u16, u16),
    /// One character, and the space left around it. A field is the two
    /// added together, and that is the grid a row and a column count.
    cell: (u16, u16),
    gap: (u16, u16),
    /// The pictures of the characters no code stands for, under the cells
    /// they were sent in. Kept for the same reason the format is: a
    /// broadcaster may define a glyph in one statement and write it in a
    /// later one. See [`Layout::glyphs`].
    glyphs: HashMap<crate::arib::Drcs, Glyph>,
}

impl Default for Layout {
    /// What a high definition caption is written against, which is what
    /// every recording measured here declares for itself anyway. It stands
    /// in only for a statement that begins mid-format -- a recording joined
    /// in the middle, which is what scrubbing a timeline does.
    fn default() -> Self {
        Layout {
            plane: (960, 540),
            origin: (170, 30),
            cell: (36, 36),
            gap: (4, 24),
            glyphs: HashMap::new(),
        }
    }
}

impl Layout {
    /// A character field: the character and the space around it.
    fn field(&self) -> (u16, u16) {
        (self.cell.0 + self.gap.0, self.cell.1 + self.gap.1)
    }

    /// Take in the glyphs a statement carries the pictures of.
    ///
    /// Handed the whole data group rather than the text, because that is
    /// where the pictures are: the words are one data unit of it and the
    /// glyphs are others. Call it before [`Layout::statement`] reads the
    /// words -- the cell a line writes is defined in the same statement that
    /// writes it, every time, on all the material measured here.
    pub fn glyphs(&mut self, body: &[u8]) {
        data_units(body, |param, data| match param {
            // The one-byte downloaded sets, and the two-byte one.
            0x30 => read_glyphs(data, false, self.cell, &mut self.glyphs),
            0x31 => read_glyphs(data, true, self.cell, &mut self.glyphs),
            _ => {}
        });
    }

    /// Read one statement's text units, and say what they leave on screen.
    ///
    /// The format the statement declares is kept for the ones after it.
    pub fn statement(&mut self, units: &[u8]) -> Written {
        let mut pen = Pen::new(self);
        // Against what a caption starts with, which is not what a name
        // starts with: the macro set is in G3 rather than the katakana.
        // See [`crate::arib::Start`].
        crate::arib::walk_from(crate::arib::Start::Caption, units, &mut |step| {
            pen.step(self, step)
        });
        pen.turn(None);
        Written {
            plane: self.plane,
            pages: pen.pages,
        }
    }
}

/// Where the next character goes, and what it will look like when it gets
/// there. One statement's worth.
struct Pen {
    x: f32,
    /// The top of the character field the pen is in.
    y: f32,
    /// Where `ACPS` has just put the pen, as the *bottom* of the field --
    /// which is what that sequence names, and which cannot be turned into a
    /// top until the size says how tall the field is. Broadcasters send the
    /// size after the position as often as before it, so this is kept until
    /// a character is actually drawn. See the `0x61` arm.
    bottom: Option<f32>,
    size: Size,
    colour: u32,
    runs: Vec<Run>,
    /// The run being gathered: it ends when the pen jumps, when the colour
    /// changes, or when the characters stop being the same width.
    open: Option<Run>,
    /// The statement's own clock, in seconds: `TIME` is the only thing that
    /// moves it, and a clear at a moment past zero ends a page rather than
    /// throwing it away. See [`Page`].
    clock: f32,
    /// When the page being written went up.
    page_at: f32,
    pages: Vec<Page>,
}

impl Pen {
    fn new(layout: &Layout) -> Pen {
        Pen {
            x: layout.origin.0 as f32,
            y: layout.origin.1 as f32,
            bottom: None,
            size: Size::Normal,
            colour: COLOURS[7],
            runs: Vec::new(),
            open: None,
            clock: 0.0,
            page_at: 0.0,
            pages: Vec::new(),
        }
    }

    /// End the run being gathered, wherever the pen is about to go.
    fn finish(&mut self) {
        if let Some(run) = self.open.take() {
            self.runs.push(run);
        }
    }

    /// Close the page on the plane, if anything is written on it.
    ///
    /// `until` is the moment it comes down: the clock where a clear takes it
    /// down, and nothing at the end of the statement, where it stands until
    /// the next one. A clear with nothing written closes no page -- it is
    /// the head of the statement, which every broadcaster sends.
    fn turn(&mut self, until: Option<f32>) {
        self.finish();
        let stood = until.is_none_or(|u| u > self.page_at);
        if !self.runs.is_empty() && stood {
            self.pages.push(Page {
                at: self.page_at,
                until,
                runs: std::mem::take(&mut self.runs),
            });
        }
        self.runs.clear();
        self.page_at = self.clock;
    }

    /// Turn the position `ACPS` left into the top of a field, now that the
    /// size is known.
    fn settle(&mut self, field_h: f32) {
        if let Some(bottom) = self.bottom.take() {
            self.y = bottom - field_h;
        }
    }

    /// Put one character where the pen is, and move the pen on.
    ///
    /// `text` is how it is spelled and `glyph` the picture of it, where the
    /// broadcaster sent one instead; a character is one or the other.
    fn place(&mut self, layout: &Layout, text: &str, full_width: bool, glyph: Option<Glyph>) {
        let (field_w, field_h) = layout.field();
        let (sx, sy) = self.size.scale();
        // Where `ACPS` put the pen, now that the size says how tall the
        // field under it is.
        self.settle(field_h as f32 * sy);
        // What this character takes: a field, or half of one where the set
        // is a half-width set or the size is a half-width size.
        let advance = field_w as f32 * sx * if full_width { 1.0 } else { 0.5 };
        let height = layout.cell.1 as f32 * sy;
        // The glyph sits in the middle of its field: the space a format
        // leaves around a character is left around it rather than under it,
        // which is what keeps two lines the same distance apart however
        // tall the characters are.
        let top = self.y + (field_h as f32 * sy - height) / 2.0;
        let advance_u = advance.round() as u16;
        // A picture is a run of its own, and so is the character after it:
        // a run is a string drawn in one call, and this one is not a string.
        let fits = glyph.is_none()
            && self.open.as_ref().is_some_and(|r| {
                r.glyph.is_none()
                    && r.advance == advance_u
                    && r.colour == self.colour
                    && r.height == height.round() as u16
                    && (r.y as f32 - top).abs() < 0.5
                    && ((r.x + r.width) as f32 - self.x).abs() < 0.5
            });
        if !fits {
            self.finish();
        }
        let run = self.open.get_or_insert_with(|| Run {
            x: self.x.max(0.0).round() as u16,
            y: top.max(0.0).round() as u16,
            width: 0,
            height: height.round() as u16,
            advance: advance_u,
            text: String::new(),
            glyph,
            colour: self.colour,
        });
        run.text.push_str(text);
        run.width += advance_u;
        self.x += advance;
        if run.glyph.is_some() {
            self.finish();
        }
    }

    fn step(&mut self, layout: &mut Layout, step: crate::arib::Step<'_>) {
        let (field_w, field_h) = layout.field();
        let (sx, sy) = self.size.scale();
        match step {
            // A character the broadcaster sent the picture of goes where a
            // character goes; it is only what is drawn there that differs.
            // Where the picture never arrived -- a statement read from the
            // middle of a recording, which is what scrubbing a timeline
            // does -- what is left is what a receiver without it shows.
            crate::arib::Step::Glyph(drcs) => {
                let glyph = layout.glyphs.get(&drcs).cloned();
                let text = match glyph {
                    Some(_) => String::new(),
                    None => crate::arib::UNKNOWN.to_string(),
                };
                // A whole field, because the sets these come from are drawn
                // full width -- except where the glyph is half a cell wide
                // and nothing has asked for a half-width size, which is a
                // character that is half a character. Both happen, and the
                // difference is the size code: a broadcaster writing a line
                // in `MSZ` sends the glyph at 18 dots *for that*, and the
                // pen has already halved the field, so halving it again
                // would put the glyph in a quarter of one. A `ü` in a
                // German line is the other case -- 18 dots in a line
                // written at full size, beside half-width letters.
                let half = glyph
                    .as_ref()
                    .is_some_and(|g| u16::from(g.width) * 2 <= layout.cell.0)
                    && self.size == Size::Normal;
                self.place(layout, &text, !half, glyph);
            }
            crate::arib::Step::Text(text, full_width) => {
                self.place(layout, text, full_width, None);
            }
            crate::arib::Step::Control(code, params) => {
                let p = |k: usize| params.get(k).map_or(0.0, |b| f32::from(b & 0x3F));
                match code {
                    // Clear the screen: the plane goes and the pen goes
                    // home. A statement that carries this and nothing else
                    // is how a caption comes down.
                    0x0C => {
                        // What was written before it is a page rather than
                        // nothing, and the clear is when that page comes
                        // down. `TIME` is how a caption times itself to the
                        // speech instead of to the next statement, and
                        // throwing away the text in front of the clear was
                        // showing nothing at all on the channels that write
                        // captions that way. See [`Page`].
                        self.turn(Some(self.clock));
                        self.open = None;
                        self.runs.clear();
                        self.bottom = None;
                        self.x = layout.origin.0 as f32;
                        self.y = layout.origin.1 as f32;
                    }
                    // The cursor moves. Each is a field, in the direction
                    // its name says; APR is the start of the next line.
                    0x08 => {
                        self.finish();
                        self.x -= field_w as f32 * sx;
                    }
                    0x09 => {
                        self.finish();
                        self.x += field_w as f32 * sx;
                    }
                    0x0A => {
                        self.finish();
                        self.settle(field_h as f32 * sy);
                        self.y += field_h as f32 * sy;
                    }
                    0x0B => {
                        self.finish();
                        self.settle(field_h as f32 * sy);
                        self.y -= field_h as f32 * sy;
                    }
                    0x0D => {
                        self.finish();
                        self.settle(field_h as f32 * sy);
                        self.x = layout.origin.0 as f32;
                        self.y += field_h as f32 * sy;
                    }
                    // Forward by a count of fields.
                    0x16 => {
                        self.finish();
                        self.x += field_w as f32 * sx * p(0);
                    }
                    // Straight to a row and a column, counted from the top
                    // left of the display area in whole fields. The row
                    // comes first.
                    0x1C => {
                        self.finish();
                        self.bottom = None;
                        self.y = layout.origin.1 as f32 + p(0) * field_h as f32;
                        self.x = layout.origin.0 as f32 + p(1) * field_w as f32;
                    }
                    // TIME: the statement waiting on its own account.
                    // `0x20` and a count of tenths of a second is the wait a
                    // caption times a line with; the other form names a
                    // clock, which nothing here is running.
                    0x9D => {
                        if params.first() == Some(&0x20) {
                            self.clock += p(1) / 10.0;
                        }
                    }
                    // The eight colours a code names outright.
                    0x80..=0x87 => self.colour = COLOURS[(code & 0x07) as usize],
                    0x88 => self.size = Size::Small,
                    0x89 => self.size = Size::Middle,
                    0x8A => self.size = Size::Normal,
                    // Colour by number. `0x20` chooses a colour map, which
                    // is read past -- the eight below are the first eight of
                    // every map -- and 0x40 to 0x4F is the foreground, which
                    // is the one this draws with. The background and the
                    // half-tone colours are not followed: what stands behind
                    // a caption here is the box the window draws, not the
                    // one the broadcaster chose.
                    0x90 => {
                        if params.first() != Some(&0x20) {
                            if let Some(&c) = params.first() {
                                if (0x40..=0x4F).contains(&c) {
                                    self.colour = COLOURS[(c & 0x07) as usize];
                                }
                            }
                        }
                    }
                    // A control sequence: the format, mostly. Its last byte
                    // says which one, and the digits before it are its
                    // parameters.
                    0x9B => {
                        let (&last, rest) = match params.split_last() {
                            Some(x) => x,
                            None => return,
                        };
                        let args = csi_args(rest);
                        let arg = |k: usize| args.get(k).copied().unwrap_or(0);
                        match last {
                            // SWF: which screen this is written for.
                            0x53 => {
                                if let Some(plane) = plane_of(arg(0)) {
                                    layout.plane = plane;
                                }
                            }
                            // SDF: how big the writing area is. Read past:
                            // nothing here clips a line to it, because a
                            // broadcaster's own line fits the area it just
                            // declared. Where a line goes is `SDP` and
                            // `ACPS`, below.
                            0x56 => {}
                            // SSM: how big one character is.
                            0x57 => layout.cell = (arg(0), arg(1)),
                            // SHS and SVS: the space left around it.
                            0x58 => layout.gap.0 = arg(0),
                            0x59 => layout.gap.1 = arg(0),
                            // ACPS: straight to a place in the plane's own
                            // dots. What it names is the *bottom* left of
                            // the character field -- 509 on a 540 dot plane
                            // is the last row, not a row past the bottom --
                            // and it may name it before the size that says
                            // how tall that field is, so the bottom is kept
                            // and turned into a top when a character is
                            // drawn. Every channel that puts a caption
                            // anywhere but the head of the display area
                            // places it with this, and reading past it left
                            // their captions in the top left corner.
                            0x61 => {
                                self.finish();
                                self.x = f32::from(arg(0));
                                self.bottom = Some(f32::from(arg(1)) + 1.0);
                            }
                            // SDP: where the writing area begins. The pen
                            // has not moved, but home has.
                            0x5F => {
                                let was = layout.origin;
                                layout.origin = (arg(0), arg(1));
                                if (self.x - was.0 as f32).abs() < 0.5 {
                                    self.x = layout.origin.0 as f32;
                                }
                                if (self.y - was.1 as f32).abs() < 0.5 {
                                    self.y = layout.origin.1 as f32;
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// The numbers in a control sequence: digits, separated by semicolons, with
/// an intermediate byte before the one that says which sequence it is.
fn csi_args(bytes: &[u8]) -> Vec<u16> {
    let mut out = vec![0u16];
    for &b in bytes {
        match b {
            b'0'..=b'9' => {
                let last = out.last_mut().expect("one to start with");
                *last = last.saturating_mul(10) + u16::from(b - b'0');
            }
            b';' => out.push(0),
            // The intermediate byte, and anything else that is not a number.
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A statement in the shape every broadcast measured here sends: clear
    /// the plane, declare the format, then place a line and write it.
    ///
    /// The format is the one read off four channels' recordings -- a
    /// 960 x 540 plane, a 620 x 480 area at (170, 30), 36 x 36 characters
    /// with 4 and 24 dots of space around them -- so a field is 40 x 60 and
    /// there are eight rows to put a line on.
    fn statement(row: u8, column: u8, text: &[u8]) -> Vec<u8> {
        let mut out = vec![0x0C];
        for csi in [
            &b"7 S"[..],
            &b"620;480 V"[..],
            &b"170;30 _"[..],
            &b"4 X"[..],
            &b"24 Y"[..],
            &b"36;36 W"[..],
        ] {
            out.push(0x9B);
            out.extend_from_slice(csi);
        }
        out.extend_from_slice(&[0x1C, 0x40 + row, 0x40 + column]);
        out.extend_from_slice(text);
        out
    }

    /// Two kanji, in the set a caption starts in.
    const KANJI: [u8; 4] = [0x30, 0x21, 0x30, 0x22];

    /// A data group body: what a statement arrives as, which is the text
    /// and whatever else the broadcaster sent with it.
    ///
    /// [`Layout::statement`] is handed the text alone -- that is what
    /// [`text_units`] pulls out of this -- and [`Layout::glyphs`] is handed
    /// the whole of it, because the pictures are the other units.
    fn body(units: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![0x00, 0x00, 0x00, 0x00];
        for (param, data) in units {
            out.extend_from_slice(&[0x1F, *param]);
            let n = data.len();
            out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
            out.extend_from_slice(data);
        }
        out
    }

    /// One cell of the one-byte downloaded set, as a broadcaster defines
    /// it: two shades, one bit a dot, and whichever dots `ink` says.
    fn drcs_unit(code: u16, width: u8, height: u8, ink: impl Fn(u8, u8) -> bool) -> (u8, Vec<u8>) {
        let mut out = vec![1, (code >> 8) as u8, code as u8, 1, 0x00, 0, width, height];
        let mut byte = 0u8;
        let mut bits = 0;
        for y in 0..height {
            for x in 0..width {
                byte = byte << 1 | u8::from(ink(x, y));
                bits += 1;
                if bits == 8 {
                    out.push(byte);
                    (byte, bits) = (0, 0);
                }
            }
        }
        if bits > 0 {
            out.push(byte << (8 - bits));
        }
        (0x30, out)
    }

    /// The bytes that designate the one-byte downloaded set into G0 and
    /// write the first cell of it.
    const DRCS_CELL: [u8; 6] = [0x1B, 0x28, 0x20, 0x41, 0x0F, 0x21];

    /// The runs of a statement that writes one page, which is what a
    /// statement with no waiting in it writes.
    fn only(written: &Written) -> &[Run] {
        match written.pages.as_slice() {
            [] => &[],
            [page] => &page.runs,
            pages => panic!("{} pages, not one", pages.len()),
        }
    }

    #[test]
    fn reads_the_format_a_broadcast_declares() {
        let mut layout = Layout::default();
        let written = layout.statement(&statement(5, 0, &KANJI));
        assert_eq!(written.plane, (960, 540));
        let run = &only(&written)[0];
        // Row five of eight, counted from the top of the display area: the
        // area begins 30 dots down and a field is 60 tall, so the field is
        // at 330 and the 36 dot character is centred in it.
        assert_eq!(run.y, 342);
        assert_eq!(run.x, 170);
        assert_eq!(run.height, 36);
        assert_eq!(run.advance, 40);
        assert_eq!(run.width, 80);
        assert_eq!(run.text.chars().count(), 2);
        // White until a colour is asked for.
        assert_eq!(run.colour, 0xFFFFFF);
    }

    #[test]
    fn a_statement_that_only_clears_the_plane_writes_nothing() {
        let mut layout = Layout::default();
        assert!(layout.statement(&statement(5, 0, b"")).pages.is_empty());
        // And it is not the same as an empty payload: the format was still
        // declared, and the next statement is laid out against it.
        assert_eq!(layout.field(), (40, 60));
    }

    #[test]
    fn the_format_stands_until_it_is_declared_again() {
        let mut layout = Layout::default();
        layout.statement(&statement(0, 0, &KANJI));
        // A statement with no format in it at all -- position and text --
        // is laid out against the one before it.
        let mut bare = vec![0x0C, 0x1C, 0x40 + 7, 0x40 + 2];
        bare.extend_from_slice(&KANJI);
        let written = layout.statement(&bare);
        let run = &only(&written)[0];
        assert_eq!(run.y, 30 + 7 * 60 + 12);
        assert_eq!(run.x, 170 + 2 * 40);
    }

    #[test]
    fn the_colours_a_caption_names() {
        let mut layout = Layout::default();
        // Yellow, which is what a caption naming a speaker is written in.
        let mut units = statement(5, 0, &[]);
        units.push(0x83);
        units.extend_from_slice(&KANJI);
        let written = layout.statement(&units);
        assert_eq!(only(&written)[0].colour, 0xFFFF00);
        // And by number, which is the same eight colours.
        let mut units = statement(5, 0, &[]);
        units.extend_from_slice(&[0x90, 0x46]);
        units.extend_from_slice(&KANJI);
        let written = layout.statement(&units);
        assert_eq!(only(&written)[0].colour, 0x00FFFF);
    }

    #[test]
    fn half_width_characters_take_half_a_field() {
        let mut layout = Layout::default();
        // The middle size: a full-width character in half a field, which is
        // what a line of more than fifteen characters is written in.
        let mut units = statement(5, 0, &[0x89]);
        units.extend_from_slice(&KANJI);
        let written = layout.statement(&units);
        let run = &only(&written)[0];
        assert_eq!(run.advance, 20);
        assert_eq!(run.width, 40);
        assert_eq!(run.height, 36);

        // And the alphanumeric set, which is half width by nature: one byte
        // a character, drawn in half a field at the normal size.
        let mut units = statement(5, 0, &[0x1B, 0x28, 0x4A]);
        units.extend_from_slice(b"AB");
        let written = layout.statement(&units);
        let run = &only(&written)[0];
        assert_eq!(run.advance, 20);
        assert_eq!(run.text, "AB");
    }

    #[test]
    fn a_line_break_puts_the_next_run_a_field_lower() {
        let mut layout = Layout::default();
        let mut units = statement(5, 0, &KANJI);
        units.push(0x0D);
        units.extend_from_slice(&KANJI);
        let written = layout.statement(&units);
        let runs = only(&written);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[1].y, runs[0].y + 60);
        assert_eq!(runs[1].x, 170);
    }

    /// `ACPS`, which is how every channel but the ones that write straight
    /// down the display area places a line: a place in the plane's own dots.
    fn at_dots(x: u16, y: u16) -> Vec<u8> {
        let mut out = vec![0x9B];
        out.extend_from_slice(format!("{x};{y} a").as_bytes());
        out
    }

    #[test]
    fn a_place_in_dots_names_the_bottom_of_the_field() {
        let mut layout = Layout::default();
        // 509 is the last dot of the plane's last row, not a row past the
        // bottom of it: the same line `APS(7,0)` names.
        let mut units = statement(0, 0, &[]);
        units.extend_from_slice(&at_dots(310, 509));
        units.extend_from_slice(&KANJI);
        let written = layout.statement(&units);
        let run = &only(&written)[0];
        assert_eq!(run.y, 30 + 7 * 60 + 12);
        assert_eq!(run.x, 310);
    }

    #[test]
    fn a_size_after_the_place_still_sits_on_it() {
        let mut layout = Layout::default();
        // Which is the order a broadcast sends them in as often as not, and
        // the field is half as tall once the size has been read: the line
        // stands on the dot named either way.
        let mut units = statement(0, 0, &[]);
        units.extend_from_slice(&at_dots(310, 509));
        units.push(0x88);
        units.extend_from_slice(&KANJI);
        let written = layout.statement(&units);
        let run = &only(&written)[0];
        assert_eq!(run.height, 18);
        // Half a field tall, ending where a full one would.
        assert_eq!(run.y + run.height, 510 - 6);
    }

    #[test]
    fn a_wait_and_a_clear_leave_the_line_up_for_the_wait() {
        let mut layout = Layout::default();
        let mut units = statement(7, 0, &KANJI);
        // Two seconds, then the plane is cleared. Which is the statement
        // saying how long its own line stands -- not saying there is none.
        units.extend_from_slice(&[0x9D, 0x20, 0x54, 0x0C]);
        let written = layout.statement(&units);
        assert_eq!(written.pages.len(), 1);
        assert_eq!(written.pages[0].at, 0.0);
        assert_eq!(written.pages[0].until, Some(2.0));
        assert_eq!(written.pages[0].runs.len(), 1);
    }

    #[test]
    fn a_statement_paces_as_many_pages_as_it_carries() {
        let mut layout = Layout::default();
        let mut units = statement(7, 0, &KANJI);
        units.extend_from_slice(&[0x9D, 0x20, 0x4A, 0x0C]);
        units.extend_from_slice(&[0x1C, 0x40 + 7, 0x40]);
        units.extend_from_slice(&KANJI);
        let written = layout.statement(&units);
        assert_eq!(written.pages.len(), 2);
        assert_eq!(written.pages[0].until, Some(1.0));
        // The second goes up where the first came down, and the statement
        // says nothing about when it comes down again.
        assert_eq!(written.pages[1].at, 1.0);
        assert_eq!(written.pages[1].until, None);
    }

    #[test]
    fn the_numbers_in_a_control_sequence() {
        assert_eq!(csi_args(b"620;480 "), vec![620, 480]);
        assert_eq!(csi_args(b"7 "), vec![7]);
        assert_eq!(csi_args(b""), vec![0]);
    }

    #[test]
    fn every_screen_format_names_a_plane() {
        assert_eq!(plane_of(7), Some((960, 540)));
        assert_eq!(plane_of(5), Some((1920, 1080)));
        // Not one the standard gives a size for: the plane in hand stands.
        assert_eq!(plane_of(3), None);
    }

    /// A caption line ending in a character the broadcaster sent the
    /// picture of, which is what every such line used to end in a geta mark
    /// for.
    ///
    /// The arrow that carries a sentence into the next line and the double
    /// brackets around a speaker's name are sent this way on the material
    /// measured here -- one cell, redefined statement by statement.
    #[test]
    fn draws_the_character_a_broadcaster_sent_the_picture_of() {
        let mut layout = Layout::default();
        // A triangle, in a glyph the size of the character cell.
        let mut text = statement(5, 0, &KANJI);
        text.extend_from_slice(&DRCS_CELL);
        let group = body(&[
            drcs_unit(0x4121, 36, 36, |x, y| x >= 18 - y / 2 && x <= 18 + y / 2),
            (0x20, text.clone()),
        ]);
        layout.glyphs(&group);
        let written = layout.statement(&text);
        let runs = only(&written);
        // The two kanji, and the picture beside them as a run of its own.
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "亜唖");
        assert!(runs[0].glyph.is_none());
        let cell = &runs[1];
        assert_eq!(cell.text, "");
        assert_eq!(cell.x, 170 + 80);
        assert_eq!(cell.advance, 40);
        let glyph = cell.glyph.as_ref().expect("the picture that was sent");
        assert_eq!((glyph.width, glyph.height), (36, 36));
        // The dots that were sent, read back: the point of the triangle at
        // the top and its base at the bottom.
        let row = |y: u8| -> String {
            (0..36)
                .map(|x| if glyph.at(x, y) { '#' } else { '.' })
                .collect()
        };
        assert_eq!(row(0), "..................#.................");
        assert!(row(35).starts_with(".#####"));
    }

    /// The same cell, redefined.
    ///
    /// Which is not a corner case: a broadcaster that sends glyphs at all
    /// sends one cell and redefines it in every statement that uses it.
    #[test]
    fn the_last_picture_sent_for_a_cell_is_the_one_drawn() {
        let mut layout = Layout::default();
        let mut text = statement(5, 0, &[]);
        text.extend_from_slice(&DRCS_CELL);
        let first = body(&[drcs_unit(0x4121, 36, 36, |_, _| true), (0x20, text.clone())]);
        layout.glyphs(&first);
        let written = layout.statement(&text);
        assert!(only(&written)[0].glyph.as_ref().unwrap().at(0, 0));
        let then = body(&[
            drcs_unit(0x4121, 36, 36, |_, _| false),
            (0x20, text.clone()),
        ]);
        layout.glyphs(&then);
        let written = layout.statement(&text);
        assert!(!only(&written)[0].glyph.as_ref().unwrap().at(0, 0));
    }

    /// A glyph sent at half the dots of a character cell.
    ///
    /// Both of the ways that happens, because they want opposite things: a
    /// line written in `MSZ` is already in half a field and the glyph sent
    /// for it fills that, while a `ü` in a line of half-width letters at
    /// full size is a half-width character in its own right.
    #[test]
    fn a_glyph_half_a_cell_wide_is_a_half_width_character() {
        let mut layout = Layout::default();
        // Eighteen dots across of a thirty-six dot cell, which is the
        // shape a broadcaster sends for a letter no set has.
        let narrow = drcs_unit(0x4121, 18, 36, |_, _| true);
        let mut text = statement(5, 0, &[]);
        text.extend_from_slice(&DRCS_CELL);
        let group = body(&[narrow.clone(), (0x20, text.clone())]);
        layout.glyphs(&group);
        let written = layout.statement(&text);
        assert_eq!(only(&written)[0].advance, 20);
        // The same glyph where the line asked for the half-width size. The
        // field is already half of one, and halving it again would draw the
        // character in a quarter of a field.
        let mut text = statement(5, 0, &[0x89]);
        text.extend_from_slice(&DRCS_CELL);
        let group = body(&[narrow, (0x20, text.clone())]);
        layout.glyphs(&group);
        let written = layout.statement(&text);
        assert_eq!(only(&written)[0].advance, 20);
    }

    /// A cell written by a statement whose definition was never read.
    ///
    /// Which is what scrubbing a timeline does: the window reads from the
    /// middle of a recording, and the statement that defined the cell is
    /// behind it. What a receiver shows without the picture is what this
    /// shows, in the place the character would have taken.
    #[test]
    fn a_cell_with_no_picture_is_what_a_receiver_shows() {
        let mut layout = Layout::default();
        let mut text = statement(5, 0, &[]);
        text.extend_from_slice(&DRCS_CELL);
        let written = layout.statement(&text);
        let run = &only(&written)[0];
        assert_eq!(run.text, "〓");
        assert!(run.glyph.is_none());
        assert_eq!(run.advance, 40);
    }
}
