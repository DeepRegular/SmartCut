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
//! entry has one size and one colour anyway. ARIB's own additional symbols
//! are named where the standard names them -- see [`ADDITIONAL`] -- except
//! for the markers a listing puts in front of a programme name, which are
//! spelled out rather than drawn; see [`symbol`]. A character this cannot
//! name -- a cell nothing is assigned to, the downloaded glyphs of DRCS --
//! comes out as `〓`, which is what a receiver that cannot draw it shows.

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
    /// ARIB's own additional symbols, designated as a set of their own.
    /// They also appear in the rows [`Set::Kanji`] leaves to them; see
    /// [`FIRST_ARIB_ROW`].
    Symbols,
    /// Downloaded glyphs, or anything else there is no naming.
    Unknown {
        wide: bool,
    },
}

impl Set {
    fn wide(&self) -> bool {
        matches!(
            self,
            Set::Kanji | Set::Symbols | Set::Unknown { wide: true }
        )
    }

    /// Whether a character of this set takes a whole character field.
    ///
    /// Not the same question as [`Set::wide`], which is about the bytes. The
    /// kana sets are one byte a character and drawn full width; the
    /// alphanumerics and the half-width katakana are one byte and drawn at
    /// half the width. A caption is laid out on a grid of fields, so this is
    /// what decides where the next character goes.
    fn full_width(&self) -> bool {
        !matches!(self, Set::Alnum | Set::HalfKatakana)
    }

    /// The graphic set a designation escape names.
    fn from_final(f: u8, wide: bool) -> Set {
        match (f, wide) {
            (0x42, true) => Set::Kanji,
            // The JIS compatibility planes, which are the same characters in
            // the same places for everything a programme name uses.
            (0x39 | 0x3A, true) => Set::Kanji,
            // The additional symbols, in a slot of their own.
            (0x3B, true) => Set::Symbols,
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
/// symbols there -- the kanji JIS left out, the units, the weather, and the
/// bracketed markers a listing carries. They are not JIS characters and must
/// not be decoded as though they were: the mapping tables of the encodings
/// that *do* fill those rows, which is where a general purpose decoder would
/// send them, hold entirely different characters. What ARIB puts there is
/// [`ADDITIONAL`].
const FIRST_ARIB_ROW: u8 = 0x75;

/// The row EUC-JP fills with characters JIS X 0208 never put there.
///
/// The same trap as [`FIRST_ARIB_ROW`] and at the other end of the table.
/// Row 13 is unassigned in JIS X 0208; the encodings a UTF-8 world reaches
/// for fill it with a vendor's additions -- the Roman numerals, the circled
/// digits, the units -- and ARIB does not. So a name carrying one of those
/// must not be *written* there, whatever `encoding_rs` says: it would be
/// written where a receiver has nothing to draw. ARIB's own cells for them
/// are in [`ADDITIONAL`], which is where they go instead. See [`wide`].
const VENDOR_ROW: u8 = 0x2D;

/// What ARIB puts in the rows JIS X 0208 leaves empty, row by row.
///
/// **This is why a series in its third season was listed as `〓`.** The
/// Roman numeral a broadcaster ends such a name with is not a JIS character
/// and not an ASCII `III`: it is row 94 cell 3 of a set of ARIB's own, and a
/// decoder that answers the geta mark for the whole of these rows throws it
/// away. So does the writer downstream of it, which then puts the geta mark
/// on the disc, where it stays.
///
/// Each row is its cells in order, from cell 1, as the characters ARIB
/// STD-B62 names them by. A `〓` in the table is a cell nothing is assigned
/// to, or one whose glyph Unicode never encoded -- the instrument marks a
/// score uses, which take up most of the second half of row 92 -- and a row
/// stops at its last assigned cell. Rows 87 to 89 have nothing in them at
/// all and are not here.
///
/// Rows 85 and 86 are kanji rather than symbols: the ones JIS X 0208 left
/// out and a Japanese name still needs, `髙`, `﨑`, `辻`. They belong here
/// for the same reason as the rest -- a name that carries one carries it.
const ADDITIONAL: [(u8, &str); 7] = [
    (0x75, ROW_85),
    (0x76, ROW_86),
    (0x7A, ROW_90),
    (0x7B, ROW_91),
    (0x7C, ROW_92),
    (0x7D, ROW_93),
    (0x7E, ROW_94),
];

const ROW_85: &str = "\
    㐂𠅘份仿侚俉傜儞冼㔟匇卡卬詹𠮷呍咖咜咩唎啊噲囤圳\
    圴塚墀姤娣婕寬﨑㟢庬弴彅德怗恵愰昤曈曙曺曻桒鿄椑\
    椻橅檑櫛𣏌𣏾𣗄毱泠洮海涿淊淸渚潞濹灤𤋮𤋮煇燁爀玟\
    玨珉珖琛琡琢琦琪琬琹瑋㻚畵疁睲䂓磈磠祇禮鿆䄃\
    ";

const ROW_86: &str = "\
    鿅秚稞筿簱䉤綋羡脘脺舘芮葛蓜蓬蕙藎蝕蟬蠋裵角諶跎\
    辻迶郝鄧鄭醲鈳銈錡鍈閒雞餃饀髙鯖鷗麴麵\
    ";

const ROW_90: &str = "\
    ⛌⛍❗⛏⛐⛑〓⛒⛕⛓⛔〓〓〓〓🅿🆊〓〓⛖⛗⛘⛙⛚\
    ⛛⛜⛝⛞⛟⛠⛡⭕㉈㉉㉊㉋㉌㉍㉎㉏〓〓〓〓⒑⒒⒓🅊\
    🅌🄿🅆🅋🈐🈑🈒🈓🅂🈔🈕🈖🅍🄱🄽⬛⬤🈗🈘🈙🈚🈛⚿🈜\
    🈝🈞🈟🈠🈡🈢🈣🈤🈥🅎㊙🈀\
    ";

const ROW_91: &str = "\
    ⛣⭖⭗⭘⭙☓㊋〒⛨㉆㉅⛩࿖⛪⛫⛬♨⛭⛮⛯⚓✈⛰⛱\
    ⛲⛳⛴⛵🅗ⒹⓈ⛶🅟🆋🆍🆌🅹⛷⛸⛹⛺🅻☎⛻⛼⛽⛾🅼\
    ⛿\
    ";

const ROW_92: &str = "\
    ➡⬅⬆⬇⬯⬮年月日円㎡㎥㎝㎠㎤🄀⒈⒉⒊⒋⒌⒍⒎⒏\
    ⒐氏副元故前新🄁🄂🄃🄄🄅🄆🄇🄈🄉🄊㈳㈶㈲㈱㈹㉄▶\
    ◀〖〗⟐²³🄭〓〓〓〓〓〓〓〓〓〓〓〓〓〓〓〓〓\
    〓〓〓〓〓〓〓〓〓〓〓〓〓🄬🄫㉇🆐🈦℻\
    ";

const ROW_93: &str = "\
    ㈪㈫㈬㈭㈮㈯㈰㈷㍾㍽㍼㍻№℡〶⚾🉀🉁🉂🉃🉄🉅🉆🉇\
    🉈🄪🈧🈨🈩🈔🈪🈫🈬🈭🈮🈯🈰🈱ℓ㎏㎐㏊㎞㎢㍱〓〓½\
    ↉⅓⅔¼¾⅕⅖⅗⅘⅙⅚⅐⅛⅑⅒☀☁☂⛄☖☗⛉⛊♦\
    ♥♣♠⛋⨀‼⁉⛅☔⛆☃⛇⚡⛈〓⚞⚟♬☎\
    ";

const ROW_94: &str = "\
    ⅠⅡⅢⅣⅤⅥⅦⅧⅨⅩⅪⅫ⑰⑱⑲⑳⑴⑵⑶⑷⑸⑹⑺⑻\
    ⑼⑽⑾⑿㉑㉒㉓㉔🄐🄑🄒🄓🄔🄕🄖🄗🄘🄙🄚🄛🄜🄝🄞🄟\
    🄠🄡🄢🄣🄤🄥🄦🄧🄨🄩㉕㉖㉗㉘㉙㉚①②③④⑤⑥⑦⑧\
    ⑨⑩⑪⑫⑬⑭⑮⑯❶❷❸❹❺❻❼❽❾❿⓫⓬㉛\
    ";

/// The additional symbol in one cell of one row, when there is one.
///
/// The cell number is the byte less the 0x20 every JIS-shaped code table
/// begins at, and cell 1 is the first character of the row.
fn additional(hi: u8, lo: u8) -> Option<char> {
    let row = ADDITIONAL.iter().find(|(at, _)| *at == hi)?.1;
    let cell = lo.checked_sub(0x21)? as usize;
    row.chars().nth(cell).filter(|c| *c != UNKNOWN)
}

/// The cell a marker's spelling goes back into, and how much of `text` it
/// took, when `text` begins with one.
///
/// A marker read out of a playlist is `[新]`, three characters where the
/// recording had one cell, and writing those three back would be writing the
/// name a listing prints rather than the name the broadcast sent -- ten bytes
/// where the cell costs two, and a television drawing three characters where
/// it would have drawn the boxed glyph. So the spelling is matched on the way
/// out and the cell is written.
///
/// Only the ones spelled as a word in brackets. Three of the thirty-seven are
/// spelled as ordinary text -- `■`, `●` and `ほか` -- and a programme name
/// carries those on their own account: a title ending `…遠出」ほか` would
/// otherwise have its last two kana swallowed into a symbol. They are written
/// as the characters they are, which a receiver draws the same way.
fn marker_cell(text: &str) -> Option<([u8; 2], usize)> {
    let (i, m) = MARKERS
        .iter()
        .enumerate()
        .find(|(_, m)| m.starts_with(['[', '(']) && text.starts_with(**m))?;
    Some(([0x7A, MARKERS_AT + i as u8], m.len()))
}

/// Which cell of which row a character is ARIB's, when it is one of them.
///
/// The markers of row 90 are passed over. They are read as the word inside
/// the box -- `[二]` rather than the glyph -- so a name never arrives here
/// carrying one, and two of them are also symbols a caption reaches through
/// a row of its own. Left in, the row they are in comes first and a caption's
/// `🈔` would be written back as the marker.
fn additional_cell(c: char) -> Option<[u8; 2]> {
    if c == UNKNOWN {
        return None;
    }
    ADDITIONAL.iter().find_map(|(hi, row)| {
        row.chars()
            .enumerate()
            .map(|(cell, in_row)| (0x21 + cell as u8, in_row))
            .find(|(lo, in_row)| *in_row == c && symbol(Set::Kanji, *hi, *lo).is_none())
            .map(|(lo, _)| [*hi, lo])
    })
}

/// One step of an eight-unit string, for a reader that wants more than the
/// characters.
///
/// [`decode`] wants only the characters -- a programme name has one size and
/// one colour whatever the broadcaster wrote it in. A **caption** is the
/// other case: where its lines go and what colour they are is half of what
/// it says, and that is in the control codes. So the walk is one thing and
/// what is made of it is another.
pub enum Step<'a> {
    /// A character, spelled the way [`decode`] spells it. The flag is
    /// whether it takes a whole character field or half of one, which is
    /// what a caption's layout is counted in.
    Text(&'a str, bool),
    /// A control code, with the parameters that belong to it. A control
    /// sequence (`0x9B`) carries everything up to and including the byte
    /// that ends it, which is the byte that says which sequence it was.
    Control(u8, &'a [u8]),
}

/// Decode an ARIB eight-unit string.
///
/// Never fails: a broadcast's own text is not always well formed, and half a
/// programme name is better than an error where a name should be.
pub fn decode(bytes: &[u8]) -> String {
    let mut out = String::new();
    walk(bytes, &mut |step| match step {
        Step::Text(text, _) => out.push_str(text),
        // A line break. Both are written: a recorder's own playlists
        // separate the lines of a programme description with 0x0A, and a
        // broadcast's text uses 0x0D.
        Step::Control(0x0A | 0x0D, _) => out.push('\n'),
        Step::Control(..) => {}
    });
    out
}

/// Walk an ARIB eight-unit string, handing every character and every control
/// code to `visit` in the order they arrive.
///
/// Never fails, for [`decode`]'s reason: this is a broadcaster's own bytes,
/// and half of a caption is better than none of one.
pub fn walk(bytes: &[u8], visit: &mut impl FnMut(Step<'_>)) {
    // What a receiver starts with, and what a playlist or a service
    // description is written against.
    let mut sets = [Set::Kanji, Set::Alnum, Set::Hiragana, Set::Katakana];
    let mut gl = 0usize;
    let mut gr = 2usize;
    let mut at = 0usize;
    // Where a character is spelled before it is handed over. One buffer for
    // the whole walk: a symbol is several characters and a character is
    // several bytes, so there has to be something to spell them into.
    let mut scratch = String::new();

    while at < bytes.len() {
        let b = bytes[at];
        at += 1;
        match b {
            // C0. The shifts and the escapes are acted on here because they
            // decide how the bytes after them are read; the rest is handed
            // over with the parameters it carries -- the cursor moves have
            // them, and a reader that stepped over the code and not its
            // parameters would read those as text.
            0x00..=0x1F => match b {
                0x0F => gl = 0,                                       // LS0
                0x0E => gl = 1,                                       // LS1
                0x19 => at = one(&mut scratch, bytes, at, sets[2], visit), // SS2
                0x1D => at = one(&mut scratch, bytes, at, sets[3], visit), // SS3
                0x1B => at = escape(bytes, at, &mut sets, &mut gl, &mut gr),
                _ => {
                    let n = match b {
                        0x16 => 1, // PAPF
                        0x1C => 2, // APS
                        _ => 0,
                    };
                    let end = (at + n).min(bytes.len());
                    visit(Step::Control(b, &bytes[at..end]));
                    at = end;
                }
            },
            0x20 => visit(Step::Text(" ", false)),
            0x21..=0x7E => at = at_char(&mut scratch, bytes, at - 1, sets[gl], visit),
            0x7F => {}
            // C1. Colours, sizes and positioning, each with its own count of
            // parameters.
            0x80..=0x9F => {
                let end = control(bytes, at, b);
                visit(Step::Control(b, &bytes[at..end]));
                at = end;
            }
            0xA0 => visit(Step::Text("\u{3000}", true)),
            0xA1..=0xFE => at = at_char(&mut scratch, bytes, at - 1, sets[gr], visit),
            0xFF => {}
        }
    }
}

/// Read one character of `set` at `at`, and say where the next one starts.
fn at_char(
    scratch: &mut String,
    bytes: &[u8],
    at: usize,
    set: Set,
    visit: &mut impl FnMut(Step<'_>),
) -> usize {
    let width = if set.wide() { 2 } else { 1 };
    let Some(raw) = bytes.get(at..at + width) else {
        return bytes.len();
    };
    // Both halves of the code are read in the graphic left range, whichever
    // side of the code table the bytes arrived on.
    let hi = raw[0] & 0x7F;
    let lo = if width == 2 { raw[1] & 0x7F } else { 0 };
    // A symbol stands for a word rather than a letter, so it is the one
    // thing here that is not one character wide.
    scratch.clear();
    match symbol(set, hi, lo) {
        Some(text) => scratch.push_str(text),
        None => scratch.push(character(set, hi, lo)),
    }
    visit(Step::Text(scratch, set.full_width()));
    at + width
}

/// One of ARIB's own additional symbols, spelled the way a listing spells it.
///
/// **These are not JIS characters and there is no Unicode character for most
/// of them.** They are what a broadcaster marks a programme with -- the first
/// episode of a run, a repeat, a subtitled showing -- drawn on a television as
/// one boxed glyph, and written down everywhere else as the bracketed word
/// inside the box. That is what this returns, because a name is a name: a
/// listing that says `[新]` says what the broadcast said, and one that says
/// `🅍` has said it in a glyph half the fonts on a machine cannot draw.
///
/// Row 90 is the row of them, and cells 48 to 84 of it are the markers. The
/// rest of ARIB's additional symbols are single characters and come back as
/// themselves; see [`ADDITIONAL`].
fn symbol(set: Set, hi: u8, lo: u8) -> Option<&'static str> {
    if !matches!(set, Set::Kanji | Set::Symbols) || hi != 0x7A {
        return None;
    }
    MARKERS.get(lo.checked_sub(MARKERS_AT)? as usize).copied()
}

/// The `lo` byte of the first marker: cell 48, which is 0x50 -- the cell
/// number plus the 0x20 every JIS-shaped code table begins at.
const MARKERS_AT: u8 = 0x50;

/// Cells 48 to 84 of row 90, as a listing spells them; see [`symbol`].
const MARKERS: [&str; 37] = [
    "[HV]",
    "[SD]",
    "[P]",
    "[W]",
    "[MV]",
    "[手]",
    "[字]",
    "[双]",
    "[デ]",
    "[S]",
    "[二]",
    "[多]",
    "[解]",
    "[SS]",
    "[B]",
    "[N]",
    "■",
    "●",
    "[天]",
    "[交]",
    "[映]",
    "[無]",
    "[料]",
    "[年齢制限]",
    "[前]",
    "[後]",
    "[再]",
    "[新]",
    "[初]",
    "[終]",
    "[生]",
    "[販]",
    "[声]",
    "[吹]",
    "[PPV]",
    "(秘)",
    "ほか",
];

/// Read one character of `set`, for the single shifts, which name the set
/// themselves.
fn one(
    scratch: &mut String,
    bytes: &[u8],
    at: usize,
    set: Set,
    visit: &mut impl FnMut(Step<'_>),
) -> usize {
    if at >= bytes.len() {
        return at;
    }
    at_char(scratch, bytes, at, set, visit)
}

fn character(set: Set, hi: u8, lo: u8) -> char {
    match set {
        // The additional symbols are reachable two ways -- designated as a
        // set of their own, and in the rows the kanji set leaves to them --
        // and a broadcast uses both.
        Set::Kanji => {
            if hi >= FIRST_ARIB_ROW {
                additional(hi, lo).unwrap_or(UNKNOWN)
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
        Set::Symbols => additional(hi, lo).unwrap_or(UNKNOWN),
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
///
/// Never past the end of what was handed over. A broadcaster's own bytes
/// stop where the recording stopped, and a statement cut off after the code
/// that opens it -- which is what the tail of a capture that was interrupted
/// looks like -- names parameters that are not there. What comes back is
/// where the next byte is, and there is no byte after the last one.
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
    (at + params).min(bytes.len())
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

// --- writing it back -----------------------------------------------------
//
// One place needs the other direction. A disc this program *writes* names
// its recordings and itself where a recorder would have, and that field is
// this code rather than UTF-8: a name written there as UTF-8 is a name a
// recorder draws as mojibake. See [`crate::bdav`].

/// What a receiver starts with, which is also what this writes against: the
/// kanji set invoked over the graphic-left range, the alphanumerics one
/// locking shift away.
///
/// Two sets carry nearly all of a name. The kanji set holds the kana as
/// well, so the only thing the other is for is ASCII, and both are already
/// designated when the text begins.
///
/// The third is not, and is the only reason anything is escaped into a slot
/// here: ARIB's additional symbols have to be designated over the kanji set
/// before they can be invoked, and the kanji set designated back afterwards.
/// A name that carries none of them -- most names -- comes out with no
/// escape sequence in it at all, exactly as before there was a third.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Writing {
    Kanji,
    Alnum,
    Symbols,
}

/// The half-width katakana, in the order [`Set::HalfKatakana`] reads them,
/// as the characters the kanji set has.
///
/// JIS X 0201's katakana are not in JIS X 0208, so a name carrying them
/// cannot be written in the two sets above. They are widened rather than
/// refused: a receiver draws the wide form of `ｱ` as `ア`, which is what the
/// name said, and the alternative is a name with holes in it. The voicing
/// marks stay separate characters, which is where they already were.
const WIDENED: &str = "。「」、・ヲァィゥェォャュョッーアイウエオカキクケコサシスセソタチツテトナニヌネノハヒフヘホマミムメモヤユヨラリルレロワン゛゜";

/// Write text as an ARIB eight-unit string.
///
/// The reverse of [`decode`], and deliberately a smaller thing: a decoder
/// has to read whatever a broadcaster sent, while a writer only has to be
/// read correctly. So this uses the two sets a receiver already has invoked
/// and the two locking shifts between them, and reaches for a designation
/// only where a name carries one of ARIB's own symbols; see [`Writing`].
///
/// The sizes go in as a recorder writes them -- half width over the
/// alphanumerics, normal over the kanji -- because that is what makes a name
/// read on a television the way it read on air. A decoder that drops them,
/// this one included, sees the same characters either way.
pub fn encode(text: &str) -> Vec<u8> {
    encode_within(text, usize::MAX)
}

/// As [`encode`], stopping before the output would pass `limit` bytes.
///
/// The field a name goes in is a length byte and 255 bytes at most, and a
/// Japanese title reaches that at 85 characters. What is cut is cut at a
/// character, never inside one: half a JIS pair is a different character
/// rather than a shorter name.
pub fn encode_within(text: &str, limit: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut mode: Option<Writing> = None;
    // Which two-byte set is designated into G0. It begins as the kanji set,
    // which is what a receiver begins with, and only a symbol moves it.
    let mut g0 = Writing::Kanji;
    let mut at = 0usize;
    while at < text.len() {
        let rest = &text[at..];
        let c = rest.chars().next().expect("inside the string");
        // Which set is written against, the bytes written, and how much of
        // the text they account for -- one character, except for a marker,
        // which is a word in brackets standing for a single cell. A line
        // break and a space are written against no set at all: both mean the
        // same thing whichever one is invoked, so they are written where they
        // are and leave the state alone.
        let (against, body, took): (Option<Writing>, [u8; 2], usize) = match c {
            '\n' => (None, [0x0D, 0], 1),
            ' ' => (None, [0x20, 0], 1),
            _ => match marker_cell(rest) {
                Some((pair, len)) => (Some(Writing::Symbols), pair, len),
                None => {
                    let n = c.len_utf8();
                    match narrow(c) {
                        Some(b) => (Some(Writing::Alnum), [b, 0], n),
                        None => match wide(c) {
                            Some(pair) => (Some(Writing::Kanji), pair, n),
                            None => match additional_cell(c) {
                                Some(pair) => (Some(Writing::Symbols), pair, n),
                                // No set has it. What a receiver shows for a
                                // character it cannot draw is written
                                // instead, so the name keeps its shape and
                                // says plainly where it could not be carried.
                                None => (Some(Writing::Kanji), [0x22, 0x2E], n),
                            },
                        },
                    }
                }
            },
        };
        let wide_char = matches!(against, Some(Writing::Kanji | Writing::Symbols));
        // Built to one side and only then accepted, so what is measured
        // against the limit is what the character actually costs -- the
        // shift and the size in front of it included.
        let mut piece: Vec<u8> = Vec::new();
        if let Some(to) = against {
            // A two-byte set has to be in G0 before it can be invoked, and
            // only one of them can be there at a time.
            if wide_char && to != g0 {
                piece.extend_from_slice(match to {
                    Writing::Symbols => &[0x1B, 0x24, 0x3B],
                    _ => &[0x1B, 0x24, 0x42],
                });
            }
            if mode != Some(to) {
                piece.extend_from_slice(match to {
                    Writing::Alnum => &[0x0E, 0x89], // LS1, and the half width
                    _ => &[0x0F, 0x8A],              // LS0, and the normal size
                });
            }
        }
        piece.push(body[0]);
        if wide_char {
            piece.push(body[1]);
        }
        if out.len() + piece.len() > limit {
            break;
        }
        out.extend_from_slice(&piece);
        at += took;
        if let Some(to) = against {
            mode = Some(to);
            if wide_char {
                g0 = to;
            }
        }
    }
    out
}

/// The alphanumeric set's byte for a character, when it has one.
fn narrow(c: char) -> Option<u8> {
    match c {
        // The two cells JIS X 0201 spends differently from ASCII.
        '¥' => Some(0x5C),
        '‾' => Some(0x7E),
        // And the two ASCII spends there instead, which the set does not
        // have at all. They go through the kanji set as their wide forms
        // rather than as the money and the overline they would otherwise
        // be read as.
        '\\' | '~' => None,
        c if c.is_ascii_graphic() => Some(c as u8),
        _ => None,
    }
}

/// The kanji set's two bytes for a character, when it has them.
///
/// By way of EUC-JP, which is JIS X 0208 with the high bit set on both
/// bytes -- the same table [`jis`] reads, run the other way, and the same
/// single dependency.
fn wide(c: char) -> Option<[u8; 2]> {
    let c = widen(c);
    let mut buf = [0u8; 8];
    let text = c.encode_utf8(&mut buf);
    let (euc, _, bad) = encoding_rs::EUC_JP.encode(text);
    if bad || euc.len() != 2 {
        return None;
    }
    let (hi, lo) = (euc[0] & 0x7F, euc[1] & 0x7F);
    // The rows JIS X 0208 leaves empty are not reached this way, at either
    // end of the table: EUC-JP fills them with characters of its own, and
    // writing one of those cells would be writing a different character.
    // ARIB has its own cells for what belongs in them, and
    // [`additional_cell`] is asked next. See [`FIRST_ARIB_ROW`] and
    // [`VENDOR_ROW`].
    (hi != VENDOR_ROW
        && hi < FIRST_ARIB_ROW
        && (0x21..=0x7E).contains(&hi)
        && (0x21..=0x7E).contains(&lo))
    .then_some([hi, lo])
}

/// The wide form of a half-width katakana. Anything else is itself.
fn widen(c: char) -> char {
    // The two ASCII cells JIS X 0201 spends on something else; see
    // [`narrow`].
    match c {
        '\\' => return '＼',
        '~' => return '～',
        _ => {}
    }
    match c as u32 {
        n @ 0xFF61..=0xFF9F => WIDENED.chars().nth((n - 0xFF61) as usize).unwrap_or(c),
        _ => c,
    }
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

    /// A statement that stops in the middle of a control code's parameters.
    ///
    /// Which is what the tail of a recording that was interrupted looks
    /// like, and half of what this program is pointed at. Reading past the
    /// end of the bytes handed over used to bring the whole run down --
    /// the programme name in a CLI cut, and the subtitle preview in the
    /// window, where the panic also left the reader's own lock poisoned and
    /// every later question about the picture answered with an error.
    #[test]
    fn a_control_code_cut_off_mid_parameter_is_not_read_past() {
        // TIME wants two parameters and COL one, and here there are none.
        assert_eq!(decode(&[0x9D]), "");
        assert_eq!(decode(&[0x90]), "");
        // One of TIME's two, and the second missing.
        assert_eq!(decode(&[0x9D, 0x20]), "");
        // And what came before it still comes back.
        assert_eq!(decode(&[0x0F, 0x47, 0x2F, 0x9D]), "年");
    }

    #[test]
    fn steps_over_the_sizes_and_the_colours() {
        // MSZ, NSZ and a colour by index, none of which is text.
        let raw = [0x89, 0x8A, 0x90, 0x41, 0x0F, 0x47, 0x2F];
        assert_eq!(decode(&raw), "年");
    }

    #[test]
    fn shows_what_it_cannot_name() {
        // Row 90 of the kanji set is ARIB's own, not JIS's. Cell 1 of it is
        // a symbol ARIB names and this names with it; cell 7 is a cell
        // nothing is assigned to, and cell 85 is past the end of the row.
        assert_eq!(decode(&[0x0F, 0x7A, 0x21]), "⛌");
        assert_eq!(decode(&[0x0F, 0x7A, 0x27]), "〓");
        assert_eq!(decode(&[0x0F, 0x7A, 0x75]), "〓");
        // And a downloaded glyph, which is a set with no characters in it at
        // all: ESC 0x24 0x28 0x20 designates DRCS into G0.
        assert_eq!(
            decode(&[0x1B, 0x24, 0x28, 0x20, 0x41, 0x0F, 0x21, 0x21]),
            "〓"
        );
    }

    /// The shape a broadcast writes the season number of a returning series
    /// in, taken off the air: the marker, the name, and a Roman numeral that
    /// used to come back as the geta mark. Row 94 cell 3 of the additional
    /// symbols, designated into G0 and designated out again -- which is also
    /// the shape [`encode`] writes.
    #[test]
    fn names_the_roman_numeral_in_a_programmes_name() {
        let raw = [
            0x1B, 0x24, 0x3B, 0x0F, 0x7A, 0x6B, // [新]
            0x1B, 0x24, 0x39, 0x0F, 0x25, 0x46, 0x25, 0x39, 0x25, 0x48, // テスト
            0x1B, 0x24, 0x3B, 0x0F, 0x7E, 0x23, // Ⅲ
            0x89, 0x20, 0x8A, // a half-width space between the two halves
            0x1B, 0x24, 0x39, 0x0F, 0x21, 0x41, // ～
        ];
        assert_eq!(decode(&raw), "[新]テストⅢ ～");
    }

    #[test]
    fn names_the_marker_in_front_of_a_programme() {
        // What a recorder wrote for the first episode of a run: LS1, the
        // additional symbols designated into G1, then row 90 cell 75.
        let raw = [0x0E, 0x1B, 0x24, 0x29, 0x3B, 0x7A, 0x6B];
        assert_eq!(decode(&raw), "[新]");
        // And the same cell reached through the kanji set's own rows, which
        // is the other way a playlist writes it.
        assert_eq!(decode(&[0x0F, 0x7A, 0x6B]), "[新]");
        // The ends of the block, and a cell past it that is still a geta.
        assert_eq!(decode(&[0x0F, 0x7A, 0x50]), "[HV]");
        assert_eq!(decode(&[0x0F, 0x7A, 0x74]), "ほか");
        assert_eq!(decode(&[0x0F, 0x7A, 0x75]), "〓");
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
    fn writes_the_name_a_disc_carries() {
        // The volume name of the same real `info.bdav` the reader is tested
        // against, written from the text it decoded to. The sizes are where
        // the disc has them: half width over the alphanumerics, normal over
        // the kanji.
        assert_eq!(
            encode("アニメ テスト"),
            vec![
                0x0F, 0x8A, 0x25, 0x22, 0x25, 0x4B, 0x25, 0x61, 0x20, 0x25, 0x46, 0x25, 0x39, 0x25,
                0x48,
            ]
        );
    }

    #[test]
    fn comes_back_as_what_went_in() {
        // A programme name of the shape a listing actually carries: the date
        // in digits, the channel in ASCII, the title in kanji and kana, and
        // the brackets that are a kana set's last cells.
        for text in [
            "2026年08月17日01時00分-衛星第一-サンプル番組",
            "てすとばんぐみ!にっ!! #07「架空の休息日の過ごし方。」",
            "アニメ テスト",
            "Anime Test",
            // A season number, a circled episode number and a kanji JIS X
            // 0208 left out: all three are ARIB's own cells, and all three
            // sit next to characters that are not.
            "テストⅢ ②「髙い」",
        ] {
            assert_eq!(decode(&encode(text)), text);
        }
    }

    /// Every cell the table names can be written back as the cell it was
    /// read from, or as a cell that means the same character.
    ///
    /// Not a tautology. Some of what ARIB keeps in these rows is also
    /// somewhere EUC-JP can reach -- `年` and `新` are ordinary kanji as well
    /// as squares of their own -- and [`wide`] is asked first, so those go
    /// out through JIS. What this checks is that whichever way a character
    /// leaves, it comes back as itself.
    #[test]
    fn every_symbol_named_can_be_written_again() {
        for (hi, row) in ADDITIONAL {
            for (cell, c) in row.chars().enumerate().filter(|(_, c)| *c != UNKNOWN) {
                let lo = 0x21 + cell as u8;
                // A marker's text is the word a listing spells it with, not
                // the character; see [`symbol`].
                let one = symbol(Set::Kanji, hi, lo)
                    .map(str::to_string)
                    .unwrap_or_else(|| c.to_string());
                assert_eq!(decode(&encode(&one)), one, "U+{:04X}", c as u32);
            }
        }
    }

    /// A marker goes back into the cell it was read from, not out as the
    /// three characters a listing spells it with.
    #[test]
    fn writes_a_marker_back_as_the_cell_it_came_from() {
        // The same bytes a recorder wrote, which is what the reader was
        // tested against: row 90 cell 75, the additional symbols designated
        // over the kanji set.
        assert_eq!(
            encode("[新]"),
            vec![0x1B, 0x24, 0x3B, 0x0F, 0x8A, 0x7A, 0x6B]
        );
        // Ten bytes as the word, two as the cell, once the set is invoked.
        assert_eq!(encode("[新][再]").len(), 9);
        // But the three that are spelled as ordinary text stay ordinary
        // text: a name ends in `ほか` because the programme has others after
        // it, and those are two kana of the name.
        assert_eq!(decode(&encode("そのほか")), "そのほか");
        assert!(!encode("そのほか").contains(&0x7A));
    }

    #[test]
    fn writes_arib_symbols_where_arib_keeps_them() {
        // Row 94 cell 3, designated over the kanji set and designated back
        // -- which is what the broadcast this was found in does. The two
        // characters around it are ordinary kanji and are written with no
        // escape at all.
        assert_eq!(
            encode("生Ⅲ生"),
            vec![
                0x0F, 0x8A, 0x40, 0x38, // 生
                0x1B, 0x24, 0x3B, 0x0F, 0x8A, 0x7E, 0x23, // Ⅲ
                0x1B, 0x24, 0x42, 0x0F, 0x8A, 0x40, 0x38, // 生
            ]
        );
        // And nothing is written into the row EUC-JP would have put the
        // numeral in, which is a row a receiver has nothing to draw for.
        assert!(!encode("Ⅲ").contains(&VENDOR_ROW));
    }

    #[test]
    fn widens_what_the_two_sets_do_not_hold() {
        // Half-width katakana are JIS X 0201's, which JIS X 0208 does not
        // have; they are written as the wide characters they are read as.
        assert_eq!(decode(&encode("ｱｲｳ")), "アイウ");
        // And a character neither set holds comes back as what a receiver
        // shows for one it cannot draw.
        assert_eq!(decode(&encode("A🙂")), "A〓");
    }

    #[test]
    fn stops_at_a_character_and_not_inside_one() {
        // Eleven bytes is the shift, the size, and four kanji; the fifth
        // would be two more and does not go in.
        let raw = encode_within("時時時時時", 11);
        assert_eq!(raw.len(), 10);
        assert_eq!(decode(&raw), "時時時時");
    }

    #[test]
    fn flattens_a_name_for_a_list() {
        assert_eq!(one_line("  アニメ\n　テスト "), "アニメ テスト");
    }
}
