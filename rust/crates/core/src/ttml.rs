//! The subtitles a 4K recording carries, which are XML rather than ARIB text.
//!
//! A high definition broadcast writes its captions as ARIB STD-B24 -- bytes
//! that shift between character sets and carry their own layout -- and
//! [`crate::caption`] reads those. **A 4K broadcast writes TTML instead**: a
//! small XML document per caption, with the words in it, the region they go
//! in stated in pixels of a 3840x2160 screen, and the times it is shown at
//! stated as text.
//!
//! Two things about the stream matter more than the format does.
//!
//! **The times are inside the document, not on the packet.** A recorder
//! stamps these packets with a counter -- 1, 2, 3 -- where a presentation
//! time should be, and the document says `begin="00:02:21.667"` on the
//! recording's own clock. So a cut cannot carry these packets the way it
//! carries every other stream, by moving the timestamp: the times have to be
//! moved inside the document, and a caption that falls outside every kept
//! range has to be left behind. See [`retimed`].
//!
//! **One document is one caption.** Every one of the 195 documents in the
//! clip this was written against holds exactly one `<div>` with one pair of
//! times, so a document is a unit a cut can keep or drop whole.
//!
//! What is read here is what a reader needs and no more: the times, the
//! regions, the styles that say how big the characters are and what colour,
//! and the words. There is no XML parser in this program and this does not
//! add one -- these documents are written by a machine, to one shape, and
//! are read by walking their tags.

use crate::caption::{Page, Run, Written};

/// The screen a 4K recording's subtitles are placed on, where the document
/// does not say. ARIB's own profile for these fixes it at the picture's own
/// size, and every document read here leaves it out.
const SCREEN: (u16, u16) = (3840, 2160);

/// Where the document begins inside a packet's payload.
///
/// The recorder puts twelve bytes of its own in front of it, the last two of
/// which count the document. Rather than trust that, the document is found
/// by looking for its opening -- which also answers whether the packet holds
/// one at all, and so whether a `bin_data` stream the map called subtitles
/// really is this kind.
pub fn document(payload: &[u8]) -> Option<&str> {
    let at = find(payload, b"<?xml")?;
    std::str::from_utf8(&payload[at..]).ok()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// When the caption in this packet goes up and comes down, in seconds on the
/// recording's own clock.
pub fn cue(payload: &[u8]) -> Option<(f64, f64)> {
    let doc = document(payload)?;
    let div = tag_at(doc, "<div")?;
    let begin = clock(attribute(div, "begin")?)?;
    let end = clock(attribute(div, "end")?)?;
    (end > begin).then_some((begin, end))
}

/// The same document with its times moved by `shift` and clipped to
/// `window`, or `None` where the caption does not belong in that window at
/// all.
///
/// **The document's length does not change.** `HH:MM:SS.mmm` is a fixed
/// width, so a time written back over another takes the same room -- which
/// matters because the twelve bytes the recorder writes in front of the
/// document count it, and a document whose length no longer matched them
/// would be one nothing downstream could believe.
///
/// A caption that was already on screen when the range opened is kept, with
/// its beginning moved up to the range's own: what a viewer should see at
/// the first frame of a cut is what they would have seen at that frame.
pub fn retimed(payload: &[u8], shift: f64, window: (f64, f64)) -> Option<Vec<u8>> {
    let (begin, end) = cue(payload)?;
    let (from, to) = window;
    if end <= from || begin >= to {
        return None;
    }
    let begin = begin.max(from) + shift;
    let end = end.min(to) + shift;
    let doc_at = find(payload, b"<?xml")?;
    let doc = std::str::from_utf8(&payload[doc_at..]).ok()?;
    let div_at = doc.find("<div")?;
    let div_end = doc[div_at..].find('>')? + div_at;
    let mut out = payload.to_vec();
    for (name, value) in [("begin", begin), ("end", end)] {
        let hay = &doc[div_at..div_end];
        let at = hay.find(&format!("{name}=\""))? + name.len() + 2;
        let text = written_clock(value);
        let start = doc_at + div_at + at;
        if out.len() < start + text.len() {
            return None;
        }
        out[start..start + text.len()].copy_from_slice(text.as_bytes());
    }
    Some(out)
}

/// `HH:MM:SS.mmm` as seconds.
fn clock(text: &str) -> Option<f64> {
    let mut parts = text.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some(h * 3600.0 + m * 60.0 + s)
}

/// And back again, in the width the document already spends on one.
fn written_clock(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    let whole = seconds as u64;
    let ms = ((seconds - whole as f64) * 1000.0).round() as u64;
    let (whole, ms) = if ms >= 1000 { (whole + 1, 0) } else { (whole, ms) };
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        whole / 3600,
        (whole / 60) % 60,
        whole % 60,
        ms
    )
}

/// The opening tag whose name is `open`, as it stands in the document.
fn tag_at<'a>(doc: &'a str, open: &str) -> Option<&'a str> {
    let at = doc.find(open)?;
    let end = doc[at..].find('>')? + at;
    Some(&doc[at..end])
}

/// One attribute out of an opening tag.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(at) = rest.find(&format!("{name}=\"")) {
        // A name is an attribute's own only where what comes before it is a
        // space: `begin` must not be found inside `xml:begin` or in another
        // attribute's value.
        let before = rest[..at].chars().next_back();
        let after = &rest[at + name.len() + 2..];
        if before.is_none_or(|c| c.is_whitespace()) {
            return after.split('"').next();
        }
        rest = after;
    }
    None
}

/// What the caption in this packet puts on screen, laid out the way
/// [`crate::caption`] lays an ARIB statement out, so that a window drawing
/// one can draw the other without knowing which it has.
///
/// The times come back as they stand in the document -- seconds on the
/// recording's own clock -- rather than as an offset from the packet, which
/// is the one place this differs from a statement. The packet has no clock
/// worth the name; see the module's own note.
pub fn written(payload: &[u8]) -> Option<Written> {
    let doc = document(payload)?;
    let (begin, end) = cue(payload)?;
    let styles = styles(doc);
    let regions = regions(doc);
    let plane = tag_at(doc, "<tt")
        .and_then(|t| attribute(t, "tts:extent"))
        .and_then(pixels)
        .unwrap_or(SCREEN);
    let mut runs = Vec::new();
    for p in elements(doc, "<p") {
        let Some(&(origin, extent)) = p
            .0
            .and_then(|tag| attribute(tag, "region"))
            .and_then(|id| regions.get(id))
        else {
            continue;
        };
        let mut x = origin.0;
        let mut line = 0u16;
        for (tag, text) in spans(p.1) {
            if tag.is_none() {
                // `<br/>`: down one line, back to the region's left edge.
                x = origin.0;
                line += 1;
                continue;
            }
            let style = tag
                .and_then(|t| attribute(t, "style"))
                .map(|names| style_of(names, &styles))
                .unwrap_or_default();
            let text = unescape(text);
            let count = text.chars().count() as u16;
            if count == 0 {
                continue;
            }
            let advance = style.size.0 + style.spacing;
            let leading = style.line_height.saturating_sub(style.size.1) / 2;
            runs.push(Run {
                x,
                y: origin.1 + line * style.line_height + leading,
                width: advance * count,
                height: style.size.1,
                advance,
                text,
                glyph: None,
                colour: style.colour,
            });
            x += advance * count;
        }
        // A region that says how tall it is and a document that fills it
        // disagree only where this has misread the sizes; nothing is drawn
        // outside it either way.
        let _ = extent;
    }
    Some(Written {
        plane,
        pages: vec![Page {
            at: begin as f32,
            until: Some(end as f32),
            runs,
        }],
    })
}

/// What a `<style>` says, with the document's own defaults where it says
/// nothing.
#[derive(Debug, Clone, Copy)]
struct Style {
    /// Character width and height. The two differ: a 4K broadcast writes
    /// half-width characters as `72px 144px`, which is how a bracket or a
    /// kana takes half the room of a kanji beside it.
    size: (u16, u16),
    line_height: u16,
    spacing: u16,
    colour: u32,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            size: (144, 144),
            line_height: 240,
            spacing: 0,
            colour: 0xFF_FF_FF,
        }
    }
}

/// Every `<style>` in the document, by the id a span names it with.
fn styles(doc: &str) -> Vec<(&str, Style)> {
    let mut out = Vec::new();
    for (tag, _) in elements(doc, "<style") {
        let Some(tag) = tag else { continue };
        let Some(id) = attribute(tag, "xml:id") else {
            continue;
        };
        let mut style = Style::default();
        if let Some(size) = attribute(tag, "tts:fontSize").and_then(pixels) {
            style.size = size;
        }
        if let Some(h) = attribute(tag, "tts:lineHeight").and_then(px) {
            style.line_height = h;
        }
        if let Some(s) = attribute(tag, "arib-tt:letter-spacing").and_then(px) {
            style.spacing = s;
        }
        if let Some(c) = attribute(tag, "tts:color").and_then(colour) {
            style.colour = c;
        }
        out.push((id, style));
    }
    out
}

/// A span names several styles at once -- the writing mode from one, the
/// size from another, the colour from a third -- and each says only its own
/// part. So they are laid over one another in the order named, and a style
/// that says nothing about a thing leaves what the one before it said.
fn style_of(names: &str, styles: &[(&str, Style)]) -> Style {
    let mut out = Style::default();
    for name in names.split_whitespace() {
        let Some((_, style)) = styles.iter().find(|(id, _)| *id == name) else {
            continue;
        };
        // Only the parts that differ from the default: a style that exists
        // to say `tts:color` should not take the size back to 144.
        let default = Style::default();
        if style.size != default.size {
            out.size = style.size;
        }
        if style.line_height != default.line_height {
            out.line_height = style.line_height;
        }
        if style.spacing != default.spacing {
            out.spacing = style.spacing;
        }
        if style.colour != default.colour {
            out.colour = style.colour;
        }
    }
    out
}

/// Where a region sits and how big it is, in the screen's own pixels.
type Placed = ((u16, u16), (u16, u16));

/// Every `<region>`, by id: where it sits and how big it is.
fn regions(doc: &str) -> Vec<(&str, Placed)> {
    let mut out = Vec::new();
    for (tag, _) in elements(doc, "<region") {
        let Some(tag) = tag else { continue };
        let (Some(id), Some(origin)) = (
            attribute(tag, "xml:id"),
            attribute(tag, "tts:origin").and_then(pixels),
        ) else {
            continue;
        };
        let extent = attribute(tag, "tts:extent")
            .and_then(pixels)
            .unwrap_or((0, 0));
        out.push((id, (origin, extent)));
    }
    out
}

trait Lookup<'a, T> {
    fn get(&self, id: &str) -> Option<&T>;
}

impl<'a, T> Lookup<'a, T> for Vec<(&'a str, T)> {
    fn get(&self, id: &str) -> Option<&T> {
        self.iter().find(|(name, _)| *name == id).map(|(_, v)| v)
    }
}

/// `"1040px 240px"` as a pair, and `"144px"` as one number twice.
fn pixels(text: &str) -> Option<(u16, u16)> {
    let mut parts = text.split_whitespace();
    let first = px(parts.next()?)?;
    Some(match parts.next() {
        Some(second) => (first, px(second)?),
        None => (first, first),
    })
}

fn px(text: &str) -> Option<u16> {
    text.trim().trim_end_matches("px").trim().parse().ok()
}

/// `#RRGGBBAA` or `#RRGGBB` as `0xRRGGBB`. The alpha is read and dropped:
/// what draws these puts them over a picture with its own opacity.
fn colour(text: &str) -> Option<u32> {
    let text = text.trim().strip_prefix('#')?;
    (text.len() >= 6)
        .then(|| u32::from_str_radix(&text[..6], 16).ok())
        .flatten()
}

/// The elements whose opening tag begins with `open`, as (tag, body).
///
/// The body is what stands between the opening tag and the next `<`, which
/// for a `<p>` is the whole run of spans inside it: enough for [`spans`] to
/// walk, and nothing that needs a parser to find.
fn elements<'a>(doc: &'a str, open: &str) -> Vec<(Option<&'a str>, &'a str)> {
    let mut out = Vec::new();
    let mut rest = doc;
    while let Some(at) = rest.find(open) {
        let after = &rest[at..];
        // `<p` must not match `<pre`: what follows the name is a space or
        // the end of the tag.
        let next = after[open.len()..].chars().next();
        if !next.is_some_and(|c| c.is_whitespace() || c == '>' || c == '/') {
            rest = &after[open.len()..];
            continue;
        }
        let Some(end) = after.find('>') else { break };
        let tag = &after[..end];
        let body = &after[end + 1..];
        let body_end = close_of(body, open);
        out.push((Some(tag), &body[..body_end]));
        rest = &after[end + 1..];
    }
    out
}

/// Where an element's body ends: at its own closing tag, or at the end of
/// what is left.
fn close_of(body: &str, open: &str) -> usize {
    let close = format!("</{}>", &open[1..]);
    body.find(&close).unwrap_or(body.len())
}

/// The runs inside a `<p>`: each `<span>` with its text, and `(None, "")`
/// for every `<br/>` between them.
fn spans(body: &str) -> Vec<(Option<&str>, &str)> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find('<') {
        let after = &rest[at..];
        if let Some(past) = after.strip_prefix("<br") {
            out.push((None, ""));
            rest = past;
            continue;
        }
        if !after.starts_with("<span") {
            rest = &after[1..];
            continue;
        }
        let Some(end) = after.find('>') else { break };
        let text_end = after[end + 1..].find('<').unwrap_or(0) + end + 1;
        out.push((Some(&after[..end]), &after[end + 1..text_end]));
        rest = &after[text_end..];
    }
    out
}

/// The five entities XML always has. A broadcast's subtitles reach for the
/// first three regularly -- `＜` and `＞` open and close a narrator's lines
/// -- and never for a numeric one in anything read here.
fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One document as a 4K recorder sends it, trimmed to one span and its
    /// styles. The twelve bytes in front of it are the recorder's own.
    fn packet(begin: &str, end: &str, text: &str) -> Vec<u8> {
        let doc = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><tt xmlns=\"http://www.w3.org/ns/ttml\" \
             xml:lang=\"ja\"><head><styling>\
             <style xml:id=\"normalSize1\" tts:fontSize=\"144px 144px\" tts:lineHeight=\"240px\" \
             arib-tt:letter-spacing=\"16px\" />\
             <style xml:id=\"middleSize1\" tts:fontSize=\"72px 144px\" tts:lineHeight=\"240px\" \
             arib-tt:letter-spacing=\"8px\" />\
             <style xml:id=\"foreColor1\" tts:color=\"#FFFF00FF\" /></styling><layout>\
             <region xml:id=\"r10\" tts:extent=\"1040px 240px\" tts:origin=\"552px 1796px\" />\
             </layout></head><body><div begin=\"{begin}\" end=\"{end}\">\
             <p xml:id=\"P10-1\" region=\"r10\">\
             <span xml:id=\"S10-1\" style=\"normalSize1 foreColor1\">{text}</span>\
             </p></div></body></tt>"
        );
        let mut out = vec![0u8; 12];
        out.extend_from_slice(doc.as_bytes());
        out
    }

    #[test]
    fn reads_the_times_out_of_the_document() {
        let p = packet("00:02:21.667", "00:02:26.667", "テスト");
        assert_eq!(cue(&p), Some((141.667, 146.667)));
        // A packet with no document in it is not one of these at all.
        assert_eq!(cue(b"\x80\xff\xf0"), None);
    }

    #[test]
    fn moves_the_times_without_changing_the_length() {
        let p = packet("00:02:21.667", "00:02:26.667", "テスト");
        // The range begins at 2:00 and the cut puts it at 0:10.
        let moved = retimed(&p, 10.0 - 120.0, (120.0, 200.0)).unwrap();
        assert_eq!(moved.len(), p.len());
        assert_eq!(cue(&moved), Some((31.667, 36.667)));
        // A caption already on screen when the range opens is kept, from
        // the range's own beginning.
        let moved = retimed(&p, -145.0, (145.0, 200.0)).unwrap();
        assert_eq!(cue(&moved), Some((0.0, 1.667)));
        // And one that falls outside every kept range is not carried.
        assert!(retimed(&p, 0.0, (0.0, 100.0)).is_none());
        assert!(retimed(&p, 0.0, (200.0, 300.0)).is_none());
    }

    #[test]
    fn lays_the_words_out_where_the_region_puts_them() {
        let p = packet("00:00:05.000", "00:00:43.000", "あいう");
        let written = written(&p).unwrap();
        assert_eq!(written.plane, (3840, 2160));
        let page = &written.pages[0];
        assert_eq!((page.at, page.until), (5.0, Some(43.0)));
        let run = &page.runs[0];
        assert_eq!(run.text, "あいう");
        // The region's own corner, and the character pitch is the size plus
        // the spacing the document asks for.
        assert_eq!(run.x, 552);
        assert_eq!(run.advance, 160);
        assert_eq!(run.width, 480);
        assert_eq!(run.height, 144);
        // Centred in the line the document leaves for it.
        assert_eq!(run.y, 1796 + (240 - 144) / 2);
        assert_eq!(run.colour, 0xFFFF00);
    }

    #[test]
    fn a_half_width_span_advances_by_half() {
        let doc = String::from_utf8(packet("00:00:01.000", "00:00:02.000", "x")[12..].to_vec())
            .unwrap()
            .replace("normalSize1 foreColor1", "middleSize1 foreColor1");
        let mut p = vec![0u8; 12];
        p.extend_from_slice(doc.as_bytes());
        let run = &written(&p).unwrap().pages[0].runs[0];
        assert_eq!((run.advance, run.width, run.height), (80, 80, 144));
    }
}
