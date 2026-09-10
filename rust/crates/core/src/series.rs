//! What a disc of recordings is called, read out of the programmes on it.
//!
//! A recorder shows one line over the list of what a disc holds, and until
//! now this program filled that line in with the channel the first recording
//! came off. That is right for an evening scraped off one channel and wrong
//! for the disc most people make, which is a run of one series: six weeks of
//! the same programme, named after the transponder it arrived on.
//!
//! The name to use is in the recordings, but not by itself. What a broadcast
//! calls a programme is the series, the episode, the episode's own title and
//! a row of marks about the broadcast, run together in one field:
//!
//! ```text
//! [字]アニメA・星降る夜の郵便局　＃03「初めての配達」
//! ```
//!
//! A disc named that is a disc named after one episode of it. What belongs
//! over the list is `星降る夜の郵便局`, which is the part every recording of
//! the series shares -- and where they do not all share one, [`shared`] says
//! so rather than guessing, and the caller names the disc after the moment
//! it was made. So this module takes a name apart: the marks in
//! brackets go, the block a channel puts its animation in goes, and the name
//! is cut where the episode begins -- because everything from the episode
//! number rightwards is about the episode and not about the series.
//!
//! None of it is a standard. ARIB says how the text is coded (see
//! [`crate::arib`]) and says nothing whatever about how a broadcaster is to
//! write an episode number into it, so what is here is the shapes Japanese
//! broadcasts actually use, and a name that fits none of them comes through
//! unchanged. Getting it wrong costs a disc a good title, which somebody can
//! type over -- the field is filled in, not locked -- so this leans towards
//! cutting only where the evidence is plain.

/// The longest text in square brackets that is taken for a mark rather than
/// for part of a name. The marks a listing carries are one or two
/// characters -- `[字]`, `[再]`, `[解]` -- and the longest anyone writes is a
/// classification like `[PG12相当]`.
const MARK_MAX: usize = 8;

/// The counters that make `第三十二X` an episode rather than a season.
///
/// `第2期` and `第2シリーズ` are the series, not a part of it, and a disc of
/// the second season is called after the second season; they are absent
/// from here deliberately. The rest are what programmes number their
/// episodes with, including the ones that number them in their own voice --
/// a monster of the week is a `怪`, a concert programme a `番`.
const EPISODE: [char; 10] = ['話', '回', '幕', '夜', '章', '篇', '編', '部', '怪', '番'];

/// The name the series goes by, out of one recording's name.
///
/// What comes back is never empty for a name that was not: a name this can
/// find nothing to cut is its own answer.
pub fn of(name: &str) -> String {
    let text = without_marks(name);
    let text = without_block(&text);
    let text = before_episode(&text);
    let text = without_subtitle(&text);
    let text = tidy(&text);
    if text.is_empty() {
        tidy(name)
    } else {
        text
    }
}

/// What to call a disc holding these recordings, or `None` where they do not
/// all say the same thing.
///
/// Every recording of one series answers [`of`] with the same text, and that
/// text is the disc's. Anything else has no name of its own to find: four
/// unrelated programmes plainly, and two seasons of one programme just as
/// much -- the season is part of the series' name, and a disc holding both
/// is not a disc of either. The caller names those after the moment the
/// disc was made instead, which is a true thing to say about a disc when
/// there is nothing else true to say.
///
/// This did once cut a mixture back to what the names had in common, and it
/// is the sort of guess that goes wrong quietly: a disc of six programmes
/// labelled with the first of them reads, in a recorder's list, exactly like
/// a disc of six episodes of that one.
pub fn shared<S: AsRef<str>>(names: &[S]) -> Option<String> {
    let mut titles = names
        .iter()
        .map(|n| of(n.as_ref()))
        .filter(|t| !t.is_empty());
    let first = titles.next()?;
    titles.all(|t| t == first).then_some(first)
}

/// The name with the marks a listing hangs off it taken out.
///
/// `[字]` and its fellows sit in front of a name as often as behind it, and
/// `【…】` carries either a block's name or an episode's -- neither of which
/// is the series -- so both go wherever they are found.
fn without_marks(name: &str) -> String {
    let raw: Vec<char> = name.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < raw.len() {
        let close = match fold(raw[i]) {
            '[' => Some((']', MARK_MAX)),
            '【' => Some(('】', usize::MAX)),
            _ => None,
        };
        if let Some((close, most)) = close {
            if let Some(end) = raw[i + 1..]
                .iter()
                .take(most.saturating_add(1))
                .position(|c| fold(*c) == close)
            {
                i += end + 2;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

/// The name with the slot it went out in taken off the front.
///
/// Channels put their animation in a named block and write the block in
/// front of the programme: `＜アニメギルド＞`, `アニメA・`, or plain `アニメ`
/// and a space. Only the front of the name, and only these shapes: `・` is a
/// character titles use themselves, and a rule that cut at the first one
/// would take the title off `シャングリラ・フロンティア`.
fn without_block(name: &str) -> String {
    let mut raw: Vec<char> = name.chars().collect();
    loop {
        while raw.first().is_some_and(|c| fold(*c) == ' ') {
            raw.remove(0);
        }
        // A block in angle brackets. Long enough to hold a season of a
        // programme's name, and no longer: what this is looking for is a
        // label, and a title that opens with a bracket keeps it.
        if raw.first().is_some_and(|c| fold(*c) == '<') {
            if let Some(end) = raw.iter().take(26).position(|c| fold(*c) == '>') {
                raw.drain(..=end);
                continue;
            }
        }
        // `アニメ`, with the letter that tells one channel's two blocks
        // apart, and then the space or the `・` that ends a label.
        if raw.starts_with(&['ア', 'ニ', 'メ']) {
            let after = if raw.get(3).is_some_and(|c| fold(*c).is_ascii_alphabetic()) {
                4
            } else {
                3
            };
            if raw
                .get(after)
                .is_some_and(|c| matches!(fold(*c), ' ' | '・'))
            {
                raw.drain(..=after);
                continue;
            }
        }
        break;
    }
    raw.into_iter().collect()
}

/// The name up to where the episode begins.
///
/// Everything from the episode number rightwards belongs to the episode: the
/// number, what that episode is called, and often a mark or two. So the
/// earliest of these ends the name.
///
/// * `#3`, `＃03`, `#11-14`, `Season1#16` -- a hash and a digit.
/// * `第7幕`, `第十九章`, `第08話` -- see [`EPISODE`] for what is a season.
/// * `42話`, `最終話` -- the same without the `第`.
/// * `(1)`, `（３２）` -- a number in brackets and nothing else in them, so
///   that `(2nd season)` stays.
/// * `ep．６`, `EPISODE　24`.
fn before_episode(name: &str) -> String {
    let raw: Vec<char> = name.chars().collect();
    let folded: Vec<char> = raw.iter().map(|c| fold(*c)).collect();
    for i in 0..raw.len() {
        let cut = folded[i] == '#' && digit(&folded, i + 1)
            || raw[i] == '第' && counted(&raw, i + 1)
            || (digit(&folded, i) || raw[i] == '最')
                && numbered(&raw, i).is_some_and(|n| raw.get(n) == Some(&'話'))
            || folded[i] == '(' && bracketed_number(&folded, i)
            || word(&folded, i, "ep")
            || word(&folded, i, "episode");
        if cut {
            return raw[..i].iter().collect();
        }
    }
    name.to_string()
}

/// The name with the episode's own title taken off the end.
///
/// A programme that numbers its episodes has had them cut off already; this
/// is for the ones that do not, and write what tonight's is about in corner
/// brackets instead. It is only ever the last of them and only at the end,
/// which is where an episode's title goes -- a title with brackets of its
/// own keeps them, because something follows them.
fn without_subtitle(name: &str) -> String {
    let raw: Vec<char> = name.chars().collect();
    let end = raw.iter().rposition(|c| !fold(*c).is_whitespace());
    if end.is_none_or(|end| raw[end] != '」') {
        return name.to_string();
    }
    match raw.iter().rposition(|c| *c == '「') {
        Some(0) | None => name.to_string(),
        Some(open) => raw[..open].iter().collect(),
    }
}

/// Whether the run of digits or kanji numerals at `i` is followed by one of
/// the counters an episode is numbered with.
fn counted(raw: &[char], i: usize) -> bool {
    numbered(raw, i).is_some_and(|n| raw.get(n).is_some_and(|c| EPISODE.contains(c)))
}

/// Where the number starting at `i` ends, or `None` where there is not one.
///
/// Both ways of writing one: `08` and `十九`, in either width. `最終` counts
/// as a number, because `最終話` is the last episode and is numbered exactly
/// as plainly as `第12話`.
fn numbered(raw: &[char], i: usize) -> Option<usize> {
    if raw.get(i) == Some(&'最') && raw.get(i + 1) == Some(&'終') {
        return Some(i + 2);
    }
    let end = i + raw[i..]
        .iter()
        .take_while(|c| {
            fold(**c).is_ascii_digit() || "一二三四五六七八九十百零壱参參".contains(**c)
        })
        .count();
    (end > i).then_some(end)
}

fn digit(folded: &[char], i: usize) -> bool {
    folded.get(i).is_some_and(|c| c.is_ascii_digit())
}

/// Whether `(` at `i` opens a bracket holding a number and nothing else.
fn bracketed_number(folded: &[char], i: usize) -> bool {
    let end = match folded[i + 1..].iter().position(|c| *c == ')') {
        Some(end) => i + 1 + end,
        None => return false,
    };
    end > i + 1 && folded[i + 1..end].iter().all(|c| c.is_ascii_digit())
}

/// Whether `word` stands at `i` as a word of its own, with the episode's
/// number after it.
fn word(folded: &[char], i: usize, word: &str) -> bool {
    if i > 0 && folded[i - 1].is_ascii_alphanumeric() {
        return false;
    }
    if !folded[i..].starts_with(&word.chars().collect::<Vec<_>>()) {
        return false;
    }
    let mut j = i + word.chars().count();
    if folded.get(j) == Some(&'.') {
        j += 1;
    }
    while folded.get(j).is_some_and(|c| c.is_whitespace()) {
        j += 1;
    }
    digit(folded, j)
}

/// The spaces and the hanging separator off both ends.
///
/// Whitespace of either width, and a `・` that had a label after it. A dash
/// is left where it is: the ones that turn up at the end of a cut name are
/// the closing half of a pair around a subtitle, and half a pair reads worse
/// than the whole of one.
fn tidy(name: &str) -> String {
    name.trim_matches(|c: char| fold(c).is_whitespace() || c == '・')
        .to_string()
}

/// The character as it would be written in ASCII, for the shapes above to be
/// looked for once rather than once a width. One character for one, so that
/// what is found in the folded text is at the same place in the name.
fn fold(c: char) -> char {
    match c {
        '　' => ' ',
        '０'..='９' | 'Ａ'..='Ｚ' | 'ａ'..='ｚ' => char::from_u32(c as u32 - 0xFEE0)
            .unwrap_or(c)
            .to_ascii_lowercase(),
        '．' => '.',
        '＃' => '#',
        '（' => '(',
        '）' => ')',
        '［' => '[',
        '］' => ']',
        '＜' => '<',
        '＞' => '>',
        _ => c.to_ascii_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four ways a broadcast writes an episode number, and the marks and
    /// the block name around them.
    #[test]
    fn takes_the_episode_off() {
        assert_eq!(
            of("[字]アニメA・星降る夜の郵便局　＃03「初めての配達」"),
            "星降る夜の郵便局"
        );
        assert_eq!(of("＜アニメギルド＞灯台守の休日　#4"), "灯台守の休日");
        assert_eq!(of("アニメ　鉄塔のある街　第１０話"), "鉄塔のある街");
        assert_eq!(
            of("霧の中の図書館（３２）迷子の栞[解][字]"),
            "霧の中の図書館"
        );
        assert_eq!(of("風待ちの丘 ep．６「約束」"), "風待ちの丘");
        assert_eq!(
            of("四畳半ものがたり　42話「引っ越し」[字]"),
            "四畳半ものがたり"
        );
        assert_eq!(of("砂の音　最終話「さよなら」[終]"), "砂の音");
        assert_eq!(of("硝子の塔　ＥＰＩＳＯＤＥ　２４"), "硝子の塔");
    }

    /// The season is the series' name and stays; the episode inside the
    /// season still goes.
    #[test]
    fn keeps_the_season() {
        assert_eq!(
            of("砂時計の向こう側 第2期　第27話「最後の一粒」"),
            "砂時計の向こう側 第2期"
        );
        assert_eq!(of("渡り鳥食堂 Season1#16-17"), "渡り鳥食堂 Season1");
        assert_eq!(
            of("やり直しの街(2nd season)#3-4"),
            "やり直しの街(2nd season)"
        );
    }

    /// A name with nothing in it to cut comes through as it is -- including
    /// the punctuation a rule that went looking for labels would eat.
    #[test]
    fn leaves_a_plain_name_alone() {
        assert_eq!(of("名もなき午後"), "名もなき午後");
        assert_eq!(of("硝子の塔-終幕-　第7幕"), "硝子の塔-終幕-");
        assert_eq!(of("アニメーションの作り方"), "アニメーションの作り方");
        assert_eq!(of("うたたねラジオ「ゲストは誰？」"), "うたたねラジオ");
        assert_eq!(of("「約束」"), "「約束」");
        assert_eq!(of(""), "");
    }

    /// What a disc of them is called.
    #[test]
    fn shares_a_name() {
        let run = ["星降る夜の郵便局 #1 [字]", "星降る夜の郵便局 #2 [字]"];
        assert_eq!(shared(&run).as_deref(), Some("星降る夜の郵便局"));
        // One recording is a run of one.
        assert_eq!(
            shared(&["灯台守の休日　#4"]).as_deref(),
            Some("灯台守の休日")
        );
    }

    /// And when they do not agree, nothing -- so that the caller can say the
    /// one true thing about a mixture, which is when it was made. A season
    /// boundary is a disagreement like any other: the season is part of the
    /// name.
    #[test]
    fn says_nothing_for_a_mixture() {
        assert_eq!(shared(&["硝子の塔 #1", "名もなき午後"]), None);
        assert_eq!(
            shared(&["渡り鳥食堂 Season1#16", "渡り鳥食堂 Season2#3"]),
            None
        );
        assert_eq!(shared(&["風待ちの丘Ⅱ #1", "風待ちの丘Ⅲ #2"]), None);
        assert_eq!(shared::<&str>(&[]), None);
        assert_eq!(shared(&[""]), None);
    }
}
