//! Reading the text a Japanese broadcast writes about itself.
//!
//! A programme name does not arrive as UTF-8. ARIB STD-B24 carries text in an
//! eight-unit code descended from ISO 2022: four graphic sets are held at
//! once -- kanji, alphanumerics, hiragana, katakana -- and the bytes in the
//! stream mean whichever of them is currently invoked, with escape sequences
//! and shift codes moving between them. Sizes, colours and rubies are in the
//! same byte stream as control codes.
//!
//! Everywhere else in this program that text is left alone. The service
//! description and the event information are copied into the output as the
//! bytes they arrived as, so that a player decodes them exactly as it would
//! have decoded the recording -- see [`crate::si`]. This module exists for
//! the one place that is not enough: a disc's own playlists name the
//! programme they hold, and a list of recordings that says `00001.rpls` is
//! not a list of recordings.
//!
//! So this is a reader, not a renderer. What comes out is the text: the
//! characters in order, with the sizes and colours dropped, because a list
//! entry has one size and one colour anyway. A character this cannot name --
//! ARIB's own additional symbols, which fill the rows JIS X 0208 leaves
//! empty, and the downloaded glyphs of DRCS -- comes out as `〓`, which is
//! what a receiver that cannot draw it shows.

/// Which graphic set a byte is to be read against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Set {
    /// JIS X 0208, two bytes a character, and the additional symbols ARIB
    /// puts in its empty rows.
    Kanji,
    /// One byte a character, ASCII in all but name.
    Alnum,
    Hiragana,
    Katakana,
    /// JIS X 0201's half-width katakana.
    HalfKatakana,
    /// Downloaded glyphs, or anything else there is no naming.
    Unknown { wide: bool },
}

impl Set {
    fn wide(&self) -> bool {
        matches!(self, Set::Kanji | Set::Unknown { wide: true })
    }

    /// The graphic set a designation escape names.
    fn from_final(f: u8, wide: bool) -> Set {
        match (f, wide) {
            (0x42, true) => Set::Kanji,
            // The JIS compatibility planes, which are the same characters in
            // the same places for everything a programme name uses.
            (0x39 | 0x3A, true) => Set::Kanji,
            (0x4A, false) => Set::Alnum,
            (0x30 | 0x36, false) => Set::Hiragana,
            (0x31 | 0x37, false) => Set::Katakana,
            (0x38, false) => Set::Katakana,
            (0x49, false) => Set::HalfKatakana,
            _ => Set::Unknown { wide },
        }
    }
}

/// What a receiver shows for a character it has no glyph for.
const UNKNOWN: char = '〓';

/// The rows of JIS X 0208 that ARIB fills with symbols of its own.
///
/// Rows 85 and up are unassigned in JIS, and ARIB puts its additional
/// symbols there -- the bracketed markers a listing carries, `[新]`, `[字]`,
/// `[終]`. They are not JIS characters and must not be decoded as though
/// they were: the mapping tables of the encodings that *do* fill those rows,
/// which is where a general purpose decoder would send them, hold entirely
/// different characters.
const FIRST_ARIB_ROW: u8 = 0x75;

/// Decode an ARIB eight-unit string.
///
/// Never fails: a broadcast's own text is not always well formed, and half a
/// programme name is better than an error where a name should be.
pub fn decode(bytes: &[u8]) -> String {
    // What a receiver starts with, and what a playlist or a service
    // description is written against.
    let mut sets = [Set::Kanji, Set::Alnum, Set::Hiragana, Set::Katakana];
    let mut gl = 0usize;
    let mut gr = 2usize;
    let mut out = String::new();
    let mut at = 0usize;

    while at < bytes.len() {
        let b = bytes[at];
        at += 1;
        match b {
            // C0. Only the shifts and the spacing mean anything here.
            0x00..=0x1F => match b {
                0x0F => gl = 0,               // LS0
                0x0E => gl = 1,               // LS1
                0x19 => at = one(&mut out, bytes, at, sets[2]), // SS2
                0x1D => at = one(&mut out, bytes, at, sets[3]), // SS3
                0x0D => out.push('\n'),       // APR, a line break
                0x1B => at = escape(bytes, at, &mut sets, &mut gl, &mut gr),
                _ => {}
            },
            0x20 => out.push(' '),
            0x21..=0x7E => at = at_char(&mut out, bytes, at - 1, sets[gl]),
            0x7F => {}
            // C1. Colours, sizes and positioning, each with its own count of
            // parameters to step over.
            0x80..=0x9F => at = control(bytes, at, b),
            0xA0 => out.push('\u{3000}'),
            0xA1..=0xFE => at = at_char(&mut out, bytes, at - 1, sets[gr]),
            0xFF => {}
        }
    }
    out
}

/// Read one character of `set` at `at`, and say where the next one starts.
fn at_char(out: &mut String, bytes: &[u8], at: usize, set: Set) -> usize {
    let width = if set.wide() { 2 } else { 1 };
    let Some(raw) = bytes.get(at..at + width) else { return bytes.len() };
    // Both halves of the code are read in the graphic left range, whichever
    // side of the code table the bytes arrived on.
    let hi = raw[0] & 0x7F;
    let lo = if width == 2 { raw[1] & 0x7F } else { 0 };
    out.push(character(set, hi, lo));
    at + width
}

/// Read one character of `set`, for the single shifts, which name the set
/// themselves.
fn one(out: &mut String, bytes: &[u8], at: usize, set: Set) -> usize {
    if at >= bytes.len() {
        return at;
    }
    at_char(out, bytes, at, set)
}

fn character(set: Set, hi: u8, lo: u8) -> char {
    match set {
        Set::Kanji => {
            if hi >= FIRST_ARIB_ROW {
                UNKNOWN
            } else {
                jis(hi, lo).unwrap_or(UNKNOWN)
            }
        }
        // The kana sets are one row of JIS each, indexed by the byte itself:
        // row 4 is hiragana and row 5 katakana, which is the same order the
        // sets are in. Neither row is full, and ARIB spends the cells JIS
        // left empty on the punctuation a caption needs to hand -- which is
        // why a programme name in kana ends in `」` and not in nothing.
        Set::Hiragana => tail(hi, false).or_else(|| jis(0x24, hi)).unwrap_or(UNKNOWN),
        Set::Katakana => tail(hi, true).or_else(|| jis(0x25, hi)).unwrap_or(UNKNOWN),
        Set::HalfKatakana => char::from_u32(0xFF61 + (hi.saturating_sub(0x21)) as u32)
            .filter(|c| *c <= '\u{FF9F}')
            .unwrap_or(UNKNOWN),
        // ASCII, with the two places JIS X 0201 differs from it: the money
        // and the overline.
        Set::Alnum => match hi {
            0x5C => '¥',
            0x7E => '‾',
            0x21..=0x7D => hi as char,
            _ => UNKNOWN,
        },
        Set::Unknown { .. } => UNKNOWN,
    }
}

/// The last eight cells of a kana set, which are punctuation rather than
/// kana: the iteration mark, the long vowel, and the four marks a Japanese
/// sentence is built with.
fn tail(cell: u8, katakana: bool) -> Option<char> {
    const MARKS: [char; 6] = ['ー', '。', '「', '」', '、', '・'];
    match cell {
        0x77 => Some(if katakana { 'ヽ' } else { 'ゝ' }),
        0x78 => Some(if katakana { 'ヾ' } else { 'ゞ' }),
        0x79..=0x7E => Some(MARKS[(cell - 0x79) as usize]),
        _ => None,
    }
}

/// One JIS X 0208 character, by way of EUC-JP.
///
/// EUC-JP is JIS X 0208 with the high bit set on both bytes, so the mapping
/// table a UTF-8 world already has is the mapping table this needs. Rows the
/// broadcaster fills with its own symbols are turned away before they get
/// here; see [`FIRST_ARIB_ROW`].
fn jis(hi: u8, lo: u8) -> Option<char> {
    if !(0x21..=0x7E).contains(&hi) || !(0x21..=0x7E).contains(&lo) {
        return None;
    }
    let euc = [hi | 0x80, lo | 0x80];
    let (text, _, bad) = encoding_rs::EUC_JP.decode(&euc);
    (!bad).then(|| text.chars().next()).flatten()
}

/// Step over an escape sequence, applying the designations and locking
/// shifts it carries.
fn escape(bytes: &[u8], at: usize, sets: &mut [Set; 4], gl: &mut usize, gr: &mut usize) -> usize {
    let Some(&b) = bytes.get(at) else { return at };
    match b {
        // Two byte set into one of the four.
        0x24 => match bytes.get(at + 1) {
            Some(0x28..=0x2B) => {
                let to = (bytes[at + 1] - 0x28) as usize;
                // A DRCS designation puts 0x20 before the final byte.
                let (f, end) = match bytes.get(at + 2) {
                    Some(0x20) => (bytes.get(at + 3).copied().unwrap_or(0), at + 4),
                    other => (other.copied().unwrap_or(0), at + 3),
                };
                sets[to] = Set::from_final(f, true);
                end
            }
            Some(&f) => {
                sets[0] = Set::from_final(f, true);
                at + 2
            }
            None => at + 1,
        },
        // One byte set into one of the four.
        0x28..=0x2B => {
            let to = (b - 0x28) as usize;
            match bytes.get(at + 1) {
                Some(0x20) => {
                    sets[to] = Set::Unknown { wide: false };
                    at + 3
                }
                Some(&f) => {
                    sets[to] = Set::from_final(f, false);
                    at + 2
                }
                None => at + 1,
            }
        }
        0x6E => {
            *gl = 2;
            at + 1
        } // LS2
        0x6F => {
            *gl = 3;
            at + 1
        } // LS3
        0x7C => {
            *gr = 3;
            at + 1
        } // LS3R
        0x7D => {
            *gr = 2;
            at + 1
        } // LS2R
        0x7E => {
            *gr = 1;
            at + 1
        } // LS1R
        _ => at + 1,
    }
}

/// Step over a C1 control and its parameters.
///
/// The counts are from ARIB STD-B24 table 7-14. Getting one wrong reads a
/// parameter as text, which is how a decoder ends up printing stray digits
/// in the middle of a programme name.
fn control(bytes: &[u8], at: usize, code: u8) -> usize {
    let params = match code {
        // Colour by index, flashing, conceal, pattern polarity, writing mode,
        // highlight, repeat: one parameter each, and a leading 0x20 means a
        // second follows.
        0x90 | 0x92 => match bytes.get(at) {
            Some(0x20) => 2,
            _ => 1,
        },
        0x8B | 0x91 | 0x93 | 0x94 | 0x95 | 0x97 | 0x98 => 1,
        // Time, which carries a mode and a value.
        0x9D => 2,
        // Control sequence introducer: parameters until an intermediate byte
        // ends it.
        0x9B => {
            let mut i = at;
            while let Some(&b) = bytes.get(i) {
                i += 1;
                if (0x40..=0x7E).contains(&b) {
                    break;
                }
            }
            return i;
        }
        _ => 0,
    };
    at + params
}

/// A programme name as a list can show it: one line, no runs of spaces.
///
/// A broadcaster writes a name to be laid out in a box on a television, and
/// uses line breaks and padding to do it. In a list of recordings that is
/// noise.
pub fn one_line(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() || c == '\u{3000}' {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_name_a_disc_carries() {
        // The volume name out of a real `info.bdav`: katakana through the
        // kanji set, with a size control in the middle of it.
        let raw = [
            0x0F, 0x25, 0x22, 0x25, 0x4B, 0x25, 0x61, 0x89, 0x20, 0x8A, 0x25, 0x46, 0x25, 0x39,
            0x25, 0x48,
        ];
        assert_eq!(decode(&raw), "アニメ テスト");
    }

    #[test]
    fn reads_kanji_and_the_alphanumeric_set() {
        // "2026年" -- digits from the alphanumeric set, 年 from the kanji set.
        let raw = [0x0E, 0x32, 0x30, 0x32, 0x36, 0x0F, 0x47, 0x2F];
        assert_eq!(decode(&raw), "2026年");
    }

    #[test]
    fn steps_over_the_sizes_and_the_colours() {
        // MSZ, NSZ and a colour by index, none of which is text.
        let raw = [0x89, 0x8A, 0x90, 0x41, 0x0F, 0x47, 0x2F];
        assert_eq!(decode(&raw), "年");
    }

    #[test]
    fn shows_what_it_cannot_name() {
        // Row 90 of the kanji set is ARIB's own, not JIS's.
        assert_eq!(decode(&[0x0F, 0x7A, 0x21]), "〓");
    }

    #[test]
    fn half_width_katakana_arrives_through_its_own_set() {
        // ESC 0x28 0x49 designates JIS X 0201 katakana into G0.
        let raw = [0x1B, 0x28, 0x49, 0x0F, 0x31, 0x32];
        assert_eq!(decode(&raw), "ｱｲ");
    }

    #[test]
    fn the_punctuation_at_the_end_of_a_kana_set_is_punctuation() {
        // GR is hiragana to begin with, and 0xFB is the cell ARIB spends on
        // an opening bracket -- the character a programme name puts before
        // its subtitle.
        assert_eq!(decode(&[0xFB, 0x0F, 0x3F, 0x40]), "「神");
        assert_eq!(decode(&[0xFC]), "」");
        // The kana themselves are still kana.
        assert_eq!(decode(&[0xCB, 0xC3]), "にっ");
    }

    #[test]
    fn flattens_a_name_for_a_list() {
        assert_eq!(one_line("  アニメ\n　テスト "), "アニメ テスト");
    }
}
