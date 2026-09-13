//! Reading the text a table says a recording's name in, whichever way the
//! thing that wrote the table wrote it.
//!
//! A Japanese broadcast writes its service and event names in ARIB STD-B24's
//! eight-unit code, and [`crate::arib`] reads that. Nothing else does. A file
//! that has been through a general multiplexer carries a service description
//! that muxer wrote itself, in the character tables of DVB EN 300 468 -- the
//! ones the rest of the world's broadcasts use -- and reading those bytes
//! against ARIB's code table turns a channel name into mojibake. `ffmpeg`
//! writes `Service01` as plain ASCII and a name with a Japanese character in
//! it as UTF-8 behind the byte that says so; read as ARIB, the first becomes
//! kanji and the second becomes nothing anybody typed.
//!
//! So the bytes are read against whichever of the two they were written in,
//! decided in this order:
//!
//! 1. **The selector.** DVB puts a byte below 0x20 in front of text that is
//!    not in its default table, and that byte says which table. The ones that
//!    matter here -- 0x10 to 0x15, which is where UTF-8 is -- are codes ARIB
//!    assigns nothing to, so a field that begins with one settles the
//!    question on its own, whatever the recording is. See [`selected`].
//! 2. **The network.** A section says which network it came off, and a
//!    Japanese one is written to ARIB's standards by everything that touches
//!    it. Anything else is read as DVB text; see [`Written`].
//! 3. **The bytes.** A Japanese recording whose tables a muxer rewrote keeps
//!    its network number and loses its encoding, so one more check rescues
//!    that: text that is well-formed UTF-8 *and* carries a byte ARIB would
//!    have to read as a control code is UTF-8. See [`rewritten`].
//!
//! What is decided here is only how the name is *read*. The tables themselves
//! are copied into the output as the bytes they arrived as -- see
//! [`crate::si`] -- so a player decodes them exactly as it would have decoded
//! the recording, and a disc this program writes gets the name back in ARIB's
//! code because that is what a recorder draws; see [`crate::arib::encode`].

/// Which standard's conventions a recording's tables are written under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Written {
    /// A Japanese broadcast, or a file that says nothing about where it came
    /// from. The default, so that a recording nothing is known about is read
    /// the way every recording was read before this module existed.
    #[default]
    Arib,
    /// Anything else: the character tables of DVB EN 300 468, which is also
    /// what a muxer writing a service description of its own writes.
    Dvb,
}

impl Written {
    /// What the network a section names says about the text on it.
    ///
    /// Terrestrial Japan is a range of numbers of its own; the satellites
    /// are single numbers. Anything outside those is somebody else's
    /// network -- or a muxer's invention, which `ffmpeg` puts at 0xFF01 --
    /// and its tables are read as DVB text. Zero is not a network at all: a
    /// table that never filled the field in has said nothing, and the
    /// default stands.
    ///
    /// The 124 and 128 degree satellites are deliberately not here. Their
    /// numbers are 1 and 3, which are two of the most used numbers in
    /// European DVB, and a rule that claimed them would read half the
    /// world's broadcasts as ARIB. A recording off one of those still reads
    /// correctly anyway, because ARIB is what is read when nothing says
    /// otherwise.
    pub fn of_network(onid: u16) -> Written {
        match matches!(onid, 0 | 0x0004 | 0x0006 | 0x0007 | 0x7880..=0x7FE8) {
            true => Written::Arib,
            false => Written::Dvb,
        }
    }
}

/// Decode the text of a table's descriptor.
///
/// Never fails, for [`crate::arib::decode`]'s reason: these are somebody
/// else's bytes, and half a name is better than an error where a name should
/// be.
pub fn decode(bytes: &[u8], written: Written) -> String {
    if let Some(text) = selected(bytes, written) {
        return text;
    }
    match written {
        Written::Dvb => default_table(bytes),
        Written::Arib if rewritten(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Written::Arib => crate::arib::decode(bytes),
    }
}

/// The text behind a DVB character-table selector, when there is one.
///
/// EN 300 468 annex A: text that is not in the default table begins with a
/// byte below 0x20 naming the table it is in, and the rest of the field is
/// that table's bytes.
///
/// Half of those bytes are also ARIB control codes, and `written` is what
/// keeps the two apart. ARIB assigns nothing to 0x10 through 0x15, so the
/// selectors in that run -- UTF-16, the two Chinese tables, Korean, and
/// UTF-8, which is the one a muxer writes -- are read as selectors wherever
/// they turn up. The rest overlap: 0x07 is a bell and 0x09 to 0x0B move a
/// caption's cursor, and a name that began with one and was read as Baltic
/// Latin would be a name this had broken rather than fixed. So those are
/// read as selectors only where the recording is a DVB one to begin with.
///
/// `None` where there is no selector, or where the bytes behind one are not
/// the table it named: a field that begins 0x15 and is not UTF-8 is not a
/// field somebody wrote in UTF-8.
fn selected(bytes: &[u8], written: Written) -> Option<String> {
    let label = |name: &str| encoding_rs::Encoding::for_label(name.as_bytes());
    let dvb = written == Written::Dvb;
    let (encoding, from) = match *bytes.first()? {
        // ISO 8859-5 through -15, in the order the annex lists them, with
        // the reserved 0x08 and 0x0C to 0x0F left out.
        n @ (0x01..=0x07 | 0x09..=0x0B) if dvb => (label(&format!("ISO-8859-{}", n + 4))?, 1),
        // The same, said the long way round: the part number in two bytes.
        0x10 => {
            let part = (u16::from(*bytes.get(1)?) << 8) | u16::from(*bytes.get(2)?);
            (label(&format!("ISO-8859-{part}"))?, 3)
        }
        // The basic multilingual plane, two bytes a character, most
        // significant first. An odd number of bytes is not that.
        0x11 if bytes.len() % 2 == 1 => (encoding_rs::UTF_16BE, 1),
        0x12 => (label("EUC-KR")?, 1),
        0x13 => (label("GBK")?, 1),
        0x14 => (label("Big5")?, 1),
        0x15 => (encoding_rs::UTF_8, 1),
        // A table named by a registration this program has no list of. The
        // two bytes it takes are stepped over and the rest is read the way
        // anything unlabelled is read, which is no worse than mojibake and
        // right where the registration was a Latin one.
        0x1F if dvb => return Some(default_table(bytes.get(2..)?)),
        _ => return None,
    };
    // Without the byte-order mark sniffing the other reader does: the
    // selector has already said what the table is, and a name that happens
    // to begin with the two bytes of some other table's mark is a name, not
    // a table. (`\xFF\xFE` behind the selector for UTF-8 is not UTF-16.)
    let (text, broken) = encoding.decode_without_bom_handling(bytes.get(from..)?);
    // A selector that named a table the bytes are not in is a selector this
    // is not reading: a field that begins with the one byte of a longer
    // code, or a recording that never had a selector at all.
    (!broken).then(|| text.trim_start_matches('\u{FEFF}').to_string())
}

/// Text in the table DVB uses when nothing says otherwise.
///
/// Which is ISO 6937: ASCII in the low half, and in the high half the
/// symbols and the combining diacritics that write a European language --
/// `0xC2 0x65` is `é`, the accent first. See [`composed`].
///
/// UTF-8 first, though. A muxer that writes a name of its own writes it in
/// whatever the operating system handed it, which today is UTF-8, and the
/// selector that would have said so is written by fewer of them than write
/// the text: `ffmpeg` puts one on a name with a Japanese character in it and
/// nothing at all on `Service01`. Well-formed UTF-8 carrying a character
/// outside ASCII is not a coincidence, and ASCII is the same text in both.
fn default_table(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    let mut out = String::new();
    let mut at = 0;
    while let Some(&b) = bytes.get(at) {
        at += 1;
        match b {
            // The low half is ASCII, and the one place ISO 6937 differs from
            // it -- 0x24, which the table draws as a currency sign rather
            // than a dollar -- is left as the dollar every muxer that writes
            // this text means by it.
            0x00..=0x7F => out.push(b as char),
            // Nothing is assigned down here: it is where the control codes
            // of an eight-bit table go, and this table has none.
            0x80..=0x9F => {}
            0xC1..=0xCF => {
                let base = bytes.get(at).copied().unwrap_or(b' ');
                at += 1;
                out.push_str(&composed(b, base as char));
            }
            _ => out.push(HIGH[b as usize - 0xA0]),
        }
    }
    out
}

/// The high half of ISO 6937, from 0xA0, less the diacritics at 0xC1 to
/// 0xCF, which are not characters and are handled where they are read.
///
/// A cell the table assigns nothing to is the geta mark, which is what is
/// shown everywhere else here for a character there is no drawing.
const HIGH: [char; 96] = [
    ' ', '¡', '¢', '£', '$', '¥', '〓', '§', '¤', '‘', '“', '«', '←', '↑', '→', '↓', //
    '°', '±', '²', '³', '×', 'µ', '¶', '·', '÷', '’', '”', '»', '¼', '½', '¾', '¿', //
    '〓', '〓', '〓', '〓', '〓', '〓', '〓', '〓', '〓', '〓', '〓', '〓', '〓', '〓', '〓',
    '〓', //
    '―', '¹', '®', '©', '™', '♪', '¬', '¦', '〓', '〓', '〓', '〓', '⅛', '⅜', '⅝', '⅞', //
    'Ω', 'Æ', 'Đ', 'ª', 'Ħ', '〓', 'Ĳ', 'Ŀ', 'Ł', 'Ø', 'Œ', 'º', 'Þ', 'Ŧ', 'Ŋ', 'ŉ', //
    'ĸ', 'æ', 'đ', 'ð', 'ħ', 'ı', 'ĳ', 'ŀ', 'ł', 'ø', 'œ', 'ß', 'þ', 'ŧ', 'ŋ', '\u{AD}',
];

/// A letter with the diacritic ISO 6937 wrote in front of it.
///
/// The letters that have one character of their own get it: a name is
/// compared, sorted and written into a file name, and `é` does all three
/// differently from an `e` with a mark hung on it afterwards. The rest --
/// a diacritic on a letter no alphabet puts it on -- comes out as the letter
/// and the combining mark, which draws the same and says as much as the
/// bytes did.
fn composed(mark: u8, base: char) -> String {
    // Each row is the diacritic's own combining character, the letters it is
    // written on, and those letters carrying it, in the same order.
    const TABLE: [(u8, char, &str, &str); 15] = [
        (0xC1, '\u{300}', "aeiouAEIOU", "àèìòùÀÈÌÒÙ"),
        (
            0xC2,
            '\u{301}',
            "aceilnorsuyzACEILNORSUYZ",
            "áćéíĺńóŕśúýźÁĆÉÍĹŃÓŔŚÚÝŹ",
        ),
        (
            0xC3,
            '\u{302}',
            "aceghijosuwyACEGHIJOSUWY",
            "âĉêĝĥîĵôŝûŵŷÂĈÊĜĤÎĴÔŜÛŴŶ",
        ),
        (0xC4, '\u{303}', "ainouAINOU", "ãĩñõũÃĨÑÕŨ"),
        (0xC5, '\u{304}', "aeiouAEIOU", "āēīōūĀĒĪŌŪ"),
        (0xC6, '\u{306}', "aguAGU", "ăğŭĂĞŬ"),
        (0xC7, '\u{307}', "cegzICEGZ", "ċėġżİĊĖĠŻ"),
        (0xC8, '\u{308}', "aeiouyAEIOUY", "äëïöüÿÄËÏÖÜŸ"),
        (0xC9, '\u{309}', "", ""),
        (0xCA, '\u{30A}', "auAU", "åůÅŮ"),
        (0xCB, '\u{327}', "cgklnrstCGKLNRST", "çģķļņŗşţÇĢĶĻŅŖŞŢ"),
        (0xCC, '\u{332}', "", ""),
        (0xCD, '\u{30B}', "ouOU", "őűŐŰ"),
        (0xCE, '\u{328}', "aeiuAEIU", "ąęįųĄĘĮŲ"),
        (0xCF, '\u{30C}', "cdelnrstzCDELNRSTZ", "čďěľňřšťžČĎĚĽŇŘŠŤŽ"),
    ];
    let Some(&(_, combining, plain, carrying)) = TABLE.iter().find(|row| row.0 == mark) else {
        return base.to_string();
    };
    match plain.chars().position(|c| c == base) {
        Some(i) => carrying.chars().nth(i).into_iter().collect(),
        None => [base, combining].iter().collect(),
    }
}

/// Whether text off a Japanese network was written by something that did not
/// know that, and is UTF-8.
///
/// Two things have to hold, because either on its own happens by accident.
/// The field has to be well-formed UTF-8 carrying a character outside ASCII,
/// which a run of ARIB's graphic bytes almost never is; and it has to carry
/// a byte from 0x80 to 0x9F, which in ARIB is a control code with parameters
/// after it and in a name is nothing anybody writes. Both together is a
/// muxer's UTF-8.
fn rewritten(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|t| !t.is_ascii())
        && bytes.iter().any(|&b| (0x80..=0x9F).contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ffmpeg -metadata service_name=...` on a name it can write in ASCII:
    /// the bytes go in as they are, with no selector in front of them.
    #[test]
    fn a_muxers_plain_name_is_not_read_as_kanji() {
        let name = b"Service01";
        assert_eq!(decode(name, Written::Dvb), "Service01");
        // And the same bytes off a Japanese network are two-byte kanji,
        // because that is what they are there.
        assert_ne!(decode(name, Written::Arib), "Service01");
    }

    /// The same, on a name it cannot: the selector for UTF-8 and then the
    /// name.
    #[test]
    fn a_muxers_utf8_name_is_read_as_utf8() {
        let mut field = vec![0x15];
        field.extend_from_slice("ＢＳフジ・１８１".as_bytes());
        assert_eq!(decode(&field, Written::Dvb), "ＢＳフジ・１８１");
        assert_eq!(decode(&field, Written::Arib), "ＢＳフジ・１８１");
    }

    /// A muxer that wrote the name and left the selector off, in a file that
    /// still says it came off a Japanese network.
    #[test]
    fn utf8_off_a_japanese_network_is_still_utf8() {
        let name = "ＢＳフジ・１８１".as_bytes();
        assert_eq!(decode(name, Written::Arib), "ＢＳフジ・１８１");
    }

    /// And the rescue does not fire on the thing it would spoil: a real
    /// broadcast's own name, which is ARIB text and not UTF-8.
    #[test]
    fn a_broadcasts_own_name_is_read_as_arib() {
        let name = crate::arib::encode("ＢＳフジ・１８１");
        assert!(!rewritten(&name));
        assert_eq!(decode(&name, Written::Arib), "ＢＳフジ・１８１");
    }

    #[test]
    fn a_network_says_which_standard_its_tables_are() {
        // What `ffmpeg` puts there, and what a broadcast does.
        assert_eq!(Written::of_network(0xFF01), Written::Dvb);
        assert_eq!(Written::of_network(0x7FE0), Written::Arib);
        assert_eq!(Written::of_network(0x0004), Written::Arib);
        assert_eq!(Written::of_network(0x2028), Written::Dvb);
        // A field nobody filled in says nothing.
        assert_eq!(Written::of_network(0), Written::Arib);
    }

    /// The default table, which is where a European broadcast's own name is:
    /// the accent is written before the letter it goes on.
    #[test]
    fn the_default_table_puts_the_accent_first() {
        assert_eq!(decode(b"Cin\xc2\x65ma", Written::Dvb), "Cinéma");
        assert_eq!(decode(b"\xc8u", Written::Dvb), "ü");
        // A diacritic on a letter that has no character of its own keeps the
        // letter and hangs the mark on it.
        assert_eq!(decode(b"\xc1w", Written::Dvb), "w\u{300}");
    }

    #[test]
    fn the_high_half_of_the_default_table_is_symbols() {
        assert_eq!(decode(b"\xd3 2026", Written::Dvb), "© 2026");
        assert_eq!(decode(b"\xa2", Written::Dvb), "¢");
    }

    /// The tables named by a selector, each read as the selector said.
    #[test]
    fn a_selector_names_the_table() {
        // ISO 8859-7, Greek: 0x01 is -5, so 0x03 is -7.
        assert_eq!(decode(b"\x03\xc1\xc2\xc3", Written::Dvb), "ΑΒΓ");
        // The basic multilingual plane, two bytes a character.
        assert_eq!(decode(b"\x11\x00A\x00B", Written::Dvb), "AB");
        // And the long-form ISO 8859 selector: part 2, Central European.
        assert_eq!(decode(b"\x10\x00\x02\xe8", Written::Dvb), "č");
    }

    /// A selector byte that is not one, in front of bytes that are not the
    /// table it would have named.
    #[test]
    fn a_selector_the_bytes_belie_is_not_a_selector() {
        // 0x15 says UTF-8 and what follows is not UTF-8, so it is not a
        // selector and the field is read the way the recording is.
        let field = b"\x15\xff\xfe";
        assert_eq!(selected(field, Written::Dvb), None);
    }

    #[test]
    fn nothing_is_nothing() {
        assert_eq!(decode(b"", Written::Dvb), "");
        assert_eq!(decode(b"", Written::Arib), "");
    }
}
