//! Reading a CUE sheet for the places it divides a recording at.
//!
//! A cue sheet is a list of tracks, each with the instant it starts at, laid
//! over one file: what a CD ripper writes beside the image of a disc, and
//! what LosslessCut and mp3splt read and write as a list of split points.
//! What is taken from it here is those instants and nothing else -- they
//! become marks, the same as a keyframe list's, and what is kept and what is
//! cut is still decided on the timeline.
//!
//! A track starts at its `INDEX 01`. `INDEX 00` is the pregap before it,
//! which on a disc is the silence a player skips; it is the start only of a
//! track that has no `01`. Times are `mm:ss:ff`, where a frame is a
//! seventy-fifth of a second -- a CD's sector -- and the minutes are free to
//! run past 99.
//!
//! **Only the first `FILE`.** The times start again at nought under each
//! one, and a sheet that names several files is describing several
//! recordings; the tracks of the second are not places in the first.
//!
//! Read off bytes rather than text: a sheet written on a Japanese machine is
//! Shift_JIS more often than not, and the titles that would make it unreadable
//! as UTF-8 are not anything this needs. The keywords and the numbers are
//! ASCII in every encoding a sheet is written in.

/// The instants the tracks of a cue sheet start at, in seconds from the
/// start of its file: in order, one to an instant.
pub fn track_starts(sheet: &[u8]) -> Vec<f64> {
    // A UTF-8 byte order mark, which would make the first line's keyword
    // another word: a sheet that opens on its `FILE` would count none, and
    // read the tracks of its second file as places in the first.
    let sheet = sheet.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(sheet);
    let mut starts = Vec::new();
    let mut files = 0;
    // The current track's `00` and `01`, settled when the next one opens.
    let mut track: Option<(Option<f64>, Option<f64>)> = None;
    fn settle(t: Option<(Option<f64>, Option<f64>)>, starts: &mut Vec<f64>) {
        if let Some(at) = t.and_then(|(pregap, start)| start.or(pregap)) {
            starts.push(at);
        }
    }
    for line in sheet.split(|&b| b == b'\n' || b == b'\r') {
        let line = String::from_utf8_lossy(line);
        let mut words = line.split_whitespace();
        let Some(word) = words.next() else { continue };
        match word.to_ascii_uppercase().as_str() {
            "FILE" => {
                files += 1;
                if files > 1 {
                    break;
                }
            }
            "TRACK" => {
                settle(track.take(), &mut starts);
                track = Some((None, None));
            }
            "INDEX" => {
                let (Some(t), Some(n), Some(at)) = (track.as_mut(), words.next(), words.next()) else {
                    continue;
                };
                let Some(at) = msf(at) else { continue };
                match n.parse::<u32>() {
                    Ok(0) => t.0 = Some(at),
                    Ok(1) => t.1 = Some(at),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    settle(track.take(), &mut starts);
    starts.sort_by(f64::total_cmp);
    starts.dedup_by(|b, a| *b - *a < 1.0 / 75.0 / 2.0);
    starts
}

/// `mm:ss:ff`, in seconds.
fn msf(at: &str) -> Option<f64> {
    let mut parts = at.split(':').map(|p| p.parse::<u32>().ok());
    let (Some(Some(m)), Some(Some(s)), Some(Some(f)), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if s >= 60 || f >= 75 {
        return None;
    }
    Some(f64::from(m) * 60.0 + f64::from(s) + f64::from(f) / 75.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape a ripper writes, pregaps and all: each track starts at its
    /// `01`, and a frame is a seventy-fifth of a second.
    #[test]
    fn tracks_start_at_index_one() {
        let sheet = b"REM GENRE Anime\r\nPERFORMER \"x\"\r\nFILE \"a.wav\" WAVE\r\n  TRACK 01 AUDIO\r\n    INDEX 01 00:00:00\r\n  TRACK 02 AUDIO\r\n    INDEX 00 03:58:20\r\n    INDEX 01 04:00:00\r\n  TRACK 03 AUDIO\r\n    INDEX 01 104:30:74\r\n";
        let got = track_starts(sheet);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], 0.0);
        assert_eq!(got[1], 240.0);
        assert!((got[2] - (104.0 * 60.0 + 30.0 + 74.0 / 75.0)).abs() < 1e-9);
    }

    /// A track with only a pregap starts there; a second `FILE` is another
    /// recording's tracks, and a title in Shift_JIS is not a reason to read
    /// nothing.
    #[test]
    fn pregap_only_second_file_and_foreign_titles() {
        let mut sheet = b"FILE \"a.flac\" WAVE\nTRACK 1 AUDIO\nTITLE \"".to_vec();
        sheet.extend_from_slice(&[0x83, 0x65, 0x83, 0x58, 0x83, 0x67]);
        sheet.extend_from_slice(b"\"\nINDEX 01 00:00:00\ntrack 2 audio\nindex 00 01:00:00\nFILE \"b.flac\" WAVE\nTRACK 3 AUDIO\nINDEX 01 00:30:00\n");
        assert_eq!(track_starts(&sheet), vec![0.0, 60.0]);
    }

    /// A byte order mark in front of the first `FILE` still makes it the
    /// first.
    #[test]
    fn a_byte_order_mark_hides_no_file() {
        let sheet = b"\xEF\xBB\xBFFILE \"a.wav\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nINDEX 01 02:00:00\nFILE \"b.wav\" WAVE\nTRACK 03 AUDIO\nINDEX 01 01:00:00\n";
        assert_eq!(track_starts(sheet), vec![0.0, 120.0]);
    }

    /// Not a sheet at all, or a time that cannot be one.
    #[test]
    fn nothing_from_what_is_not_a_sheet() {
        assert!(track_starts(b"").is_empty());
        assert!(track_starts(b"Trim(0,100)\n").is_empty());
        assert!(track_starts(b"TRACK 01 AUDIO\nINDEX 01 00:61:00\n").is_empty());
        assert!(track_starts(b"TRACK 01 AUDIO\nINDEX 01 00:00:75\n").is_empty());
    }
}
