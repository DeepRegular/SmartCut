//! Where a source's access-point index comes from.
//!
//! Walking every packet is exact but reads the whole file -- around a second
//! per gigabyte from cache, far worse from a disc. Some containers already
//! know where their entry points are: MP4 keeps a sync-sample table, and a
//! Blu-ray carries an EP map in CLIPINF. This is the seam that lets such an
//! index be dropped in instead of the walk.
//!
//! An external index only knows *where* the entry points are. It cannot say
//! whether a GOP is open, or whether the leading pictures hanging off it are
//! safe to discard -- that needs the bitstream. So a source that cannot
//! answer says so, and [`refine_leading`] fills the gaps by inspecting only
//! the handful of access points a cut actually uses.

use anyhow::{anyhow, bail, Result};
use ffmpeg_next as ff;

use crate::{bitstream, AccessPoint, Source, VideoInfo};

pub struct Index {
    pub points: Vec<AccessPoint>,
    /// Whether the leading-picture fields were measured or merely assumed.
    pub leading_known: bool,
    /// Whether the stream uses 2:3 pulldown, when the source could tell.
    pub pulldown: Option<bool>,
    /// Whether the pictures come at a rate the frame arithmetic can
    /// predict, when the source could tell. See [`varies`].
    pub variable: Option<bool>,
    /// Bits per second the pictures take, when the source read them --
    /// all of them, in the walk's case, or enough of them to divide one
    /// into the other, in the disc index's. See [`crate::VideoInfo::bit_rate`],
    /// which is where it ends up.
    pub bit_rate: Option<f64>,
    /// Where the last picture is, in rebased seconds, when the source read
    /// far enough to know.
    ///
    /// A container usually says how long it is and is usually right. A
    /// program stream is the exception: nothing in it records the length, so
    /// libavformat works one out from the timestamps at either end of the
    /// file, and on a DVD -- four gigabytes written in one run with the
    /// timestamps counting straight through -- it has been seen to come back
    /// with eight seconds for an hour. A pass that read every packet has the
    /// better answer and this is where it says so.
    pub end: Option<f64>,
}

/// How far through a source is, as a fraction, told to whoever is waiting.
pub type OnProgress<'a> = &'a (dyn Fn(f64) + Sync);

/// What an index source is given to work with.
pub struct IndexInput<'a> {
    pub path: &'a str,
    pub video: &'a VideoInfo,
    /// Container start time; access point times are rebased against it.
    pub start_time: f64,
    /// A demuxer already open on the source.
    pub ictx: crate::input::Demux,
    /// Where to say how far through the read is, if anyone is listening.
    ///
    /// Only the walk has anything to report: the sources that read a table
    /// the container already holds are done before a bar could be drawn.
    pub on: Option<OnProgress<'a>>,
}

pub trait IndexSource {
    fn name(&self) -> &'static str;
    fn build(&self, input: IndexInput) -> Result<Index>;
}

/// Read every packet and work the index out from first principles.
///
/// Decode order is what exposes leading pictures: a picture that follows an I
/// picture in decode order but presents *before* it references the previous
/// GOP. Each packet's reference flag is read in the same pass, which is what
/// makes `droppable` exact.
pub struct PacketScan;

impl IndexSource for PacketScan {
    fn name(&self) -> &'static str {
        "packet scan"
    }

    fn build(&self, input: IndexInput) -> Result<Index> {
        let IndexInput {
            video,
            start_time,
            ictx,
            on,
            ..
        } = input;
        walk(video, start_time, ictx, on, |_| Ok(()), None)
    }
}

/// The walk itself, with each entry picture offered to `entry` as it goes by.
///
/// One read where there would otherwise be two. The index needs every packet
/// and decodes none of them; the thumbnail track needs only the key ones and
/// decodes all of those -- so run apart they are two full sequential reads of
/// the same recording. The second one is nearly free where the machine still
/// has the file in its page cache and is a second transfer where it does not,
/// which a recording on a share always is. Handing the key packets out from
/// here lets a caller that wants both pay for one read; see
/// [`crate::scan_with_pictures`].
///
/// `entry` is offered the packet, not the picture: what to do with it --
/// decode it, count it, ignore it -- is the caller's business, and this file
/// has no decoder in it. It is offered *both* packets of a field-coded entry
/// picture, the marked half and the unmarked one behind it, because half of a
/// picture is not one and a decoder handed only the marked half gives nothing
/// back at all. See [`crate::EntryPictures`].
pub fn walk(
    video: &VideoInfo,
    start_time: f64,
    mut ictx: crate::input::Demux,
    on: Option<OnProgress>,
    mut entry: impl FnMut(&ff::Packet) -> Result<()>,
    stop: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<Index> {
    let stream_index = video.stream_index;
    let time_base = video.time_base;
    let codec = video.codec.clone();
    let framing = video.framing;
    let vc1 = video.vc1.as_ref();
    // Only the pictures are counted here, and on some recordings a stream
    // left switched on costs the pictures. Nothing seeks in this pass, so
    // there is no seek for this to get in front of; see
    // [`crate::input::keep_only`].
    crate::input::keep_only(&mut ictx, &[stream_index]);

    // How far there is to read. Asked of the byte stream rather than of
    // the file, because what is open is not always a file: a DVD title is
    // a range of sectors inside a VOB, or several VOBs joined, and only
    // the demuxer's own reader knows how long that adds up to. `None`
    // where it cannot say -- a pipe, a stream -- and then nothing is
    // reported rather than a fraction of an unknown.
    let total = on.and_then(|_| unsafe {
        let pb = (*ictx.as_ptr()).pb;
        if pb.is_null() {
            return None;
        }
        match ff::ffi::avio_size(pb) {
            n if n > 0 => Some(n as f64),
            _ => None,
        }
    });

    let mut packets: Vec<PacketView> = Vec::new();
    let mut entries = crate::EntryPictures::new(video);
    let mut pulldown = false;
    // What the pictures weigh. Free here -- the walk is holding every packet
    // already -- and there is nowhere else to learn it: a transport stream
    // declares no bit rate per stream, and the container's overall figure
    // counts the sound and the tables in with the pictures.
    let mut video_bytes: u64 = 0;
    // How far through is worked out from the byte the packet came from and
    // not from a count of packets. The count was one in a couple of
    // thousand, which is a dozen a second on a broadcast recording and was
    // chosen for that -- but a packet is one picture where the stream is a
    // Blu-ray's, and on a 617 MB clip that same count came to **five**
    // reports for the whole pass. A bar that stands still and then jumps.
    // What is offered is throttled where it is decided, in [`crate::Told`].
    let mut told = crate::Told::new();
    let mut seen: u64 = 0;
    for (s, p) in ictx.packets() {
        seen += 1;
        if seen.is_multiple_of(256) {
            // Asked on the same count as the progress, but not behind it: a
            // pass with nobody watching still has to be stoppable.
            if let Some(f) = stop {
                if f() {
                    bail!("abandoned");
                }
            }
            if let Some(total) = total {
                let pos = p.position();
                if pos > 0 {
                    told.at(on, (pos as f64 / total).clamp(0.0, 1.0));
                }
            }
        }
        if s.index() != stream_index {
            continue;
        }
        video_bytes += p.size() as u64;
        // Before the timestamp is asked for, not after: the second field of a
        // pair often carries none, and a packet with no timestamp is nothing
        // to the index but is still half of a picture to a decoder.
        if entries.step(&p) != crate::Step::Skip {
            entry(&p)?;
        }
        let Some(pts) = p.pts() else { continue };
        let reference = p
            .data()
            .map(|d| bitstream::is_reference(d, &codec, framing, vc1))
            .unwrap_or(true);
        if !pulldown {
            // A picture shown for anything other than two fields is a
            // stream that is not constant frame rate, whatever its
            // container says. Asked of every picture until one says so,
            // because the pattern does not start at the first one.
            pulldown = p
                .data()
                .map(|d| bitstream::display_fields(d, &codec, vc1) != 2)
                .unwrap_or(false);
        }
        packets.push(PacketView {
            pts: pts as f64 * time_base - start_time,
            dts: p.dts().unwrap_or(pts) as f64 * time_base - start_time,
            key: p.is_key(),
            reference,
            pos: p.position() as i64,
        });
    }

    // The last picture to be shown, which is not the last to arrive.
    let end = packets
        .iter()
        .map(|p| p.pts)
        .fold(f64::NEG_INFINITY, f64::max)
        .into_finite()
        .map(|t| t + video.frame_duration());
    // Over the pictures' own span, not the container's: a recording whose
    // sound runs past its last picture would otherwise read as thinner than
    // it is. A span of nothing gives no rate at all rather than infinity.
    let bit_rate = end
        .filter(|span| *span > 0.0)
        .map(|span| video_bytes as f64 * 8.0 / span)
        .filter(|r| r.is_finite() && *r > 0.0);
    // In the order they are shown, which for this question is the only
    // order there is: what is being asked is how long each picture is on
    // screen, and a picture is on screen until the next one shown replaces
    // it.
    let mut shown: Vec<f64> = packets.iter().map(|p| p.pts).collect();
    shown.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(Index {
        points: points_from(&packets, &codec),
        leading_known: true,
        pulldown: Some(pulldown),
        variable: Some(varies(&shown, video.frame_duration())),
        bit_rate,
        end,
    })
}

/// How few gaps have to be the wrong size before a stream counts as one
/// whose pictures do not come at a predictable rate, as a share of them all.
///
/// A recording is not disqualified by its faults. Dropouts leave gaps that
/// look exactly like holds -- 28 of them in a 668 MB broadcast recording
/// measured here -- and so does a packet libavcodec would not parse. What
/// tells a variable-rate recording apart from a damaged constant-rate one is
/// that the first does it constantly.
const ODD_GAPS: usize = 1;

/// How many gaps shorter than half a frame it takes to say the pictures come
/// closer together than the recording's own rate. See [`varies`].
const DENSE_GAPS: usize = 4;

/// Too few pictures to be asked. A handful of gaps says nothing either way,
/// and a recording this short is walked again in no time if it matters.
const ENOUGH: usize = 32;

/// Do the pictures come at a rate the frame arithmetic can predict?
///
/// **Not the same question as whether the container declares one.** Every
/// Matroska this was measured on declares a rate; what the pictures do is
/// another matter, and a WebM that has been through `mpdecimate` -- which is
/// what a screen capture and a good deal of what is downloaded amount to --
/// holds a picture for as long as nothing changed, which here was up to 2.4
/// seconds against a declared 29.97.
///
/// A gap is wrong when it is far enough from one frame that no rounding
/// explains it. **2:3 pulldown is deliberately left out of this**: its long
/// gap is exactly one and a half frames, the timeline already counts in
/// fields so that it can hold one, and a recording of film is not a
/// variable-rate recording in the sense this asks about. The bar is set
/// above it.
///
/// The short side is asked as well, for the stream whose declared rate is
/// slower than its own fastest stretch. Nothing measured here has been one,
/// but a container that gets to state one number for a stream that does not
/// have one can state any of them.
fn varies(shown: &[f64], fd: f64) -> bool {
    if fd <= 0.0 || shown.len() < ENOUGH {
        return false;
    }
    let (mut held, mut dense, mut counted) = (0usize, 0usize, 0usize);
    for w in shown.windows(2) {
        let gap = w[1] - w[0];
        if gap <= 0.0 {
            continue;
        }
        counted += 1;
        if gap > fd * 1.75 {
            held += 1;
        } else if gap < fd * 0.5 {
            dense += 1;
        }
    }
    if counted < ENOUGH {
        return false;
    }
    // **The two sides are not asked the same question.** A gap too long is
    // what a hold looks like and also what a dropout looks like, so it takes
    // a share of them to tell a recording that varies from one that is
    // damaged. A gap too *short* has no such twin: nothing goes wrong with a
    // recording in a way that puts two of its pictures closer together than
    // its own rate allows, and every one of them is a picture that the
    // output timeline has nowhere to put unless it is divided more finely.
    // So a handful settles it -- enough that one malformed timestamp cannot,
    // and few enough that the ten fast pictures in a twenty-five-minute
    // programme can. Measured across seven broadcast and disc recordings,
    // 47,971 pictures between them: not one gap shorter than half a frame.
    dense >= DENSE_GAPS || held * 100 > counted * ODD_GAPS
}

/// Take the entry points from the index a Blu-ray keeps beside the stream.
///
/// **The pass over the packets, already made and written down.** A disc
/// records where every picture a player may start at is, in `CLIPINF`, and
/// reading that is a hundred kilobytes against the tens of gigabytes the walk
/// reads to arrive at the same list. On the UHD disc measured here, opening a
/// two-and-a-half-hour title went from 8 minutes 45 seconds to under a
/// second.
///
/// Like every index that did not read the pictures, it cannot say whether a
/// GOP is open or whether the leading pictures hanging off one may be thrown
/// away, so `leading_known` is false and [`refine_leading`] measures the
/// handful of points a cut actually uses. What the pictures weigh it cannot
/// say either, and that one is worth a little reading rather than a guess:
/// see [`sample_bit_rate`].
///
/// Falls back to nothing rather than to a guess: a source that is not a clip
/// on a disc, or a disc whose map does not read, returns an error and the
/// caller walks the packets as before.
pub struct DiscIndex;

impl IndexSource for DiscIndex {
    fn name(&self) -> &'static str {
        "disc index"
    }

    fn build(&self, input: IndexInput) -> Result<Index> {
        let IndexInput {
            path,
            video,
            start_time,
            mut ictx,
            ..
        } = input;
        let held = crate::disc::clip_entry_points(path)
            .ok_or_else(|| anyhow!("no entry-point map beside this recording"))?;
        // The map counts on the stream's own clock, the same one the demuxer
        // reports, so a point is rebased exactly as a packet's timestamp is.
        let points: Vec<AccessPoint> = held
            .into_iter()
            .map(|(t, pos)| {
                let t = t - start_time;
                AccessPoint {
                    time: t,
                    lead_start: t,
                    lead_indices: Vec::new(),
                    droppable: true,
                    pos: pos as i64,
                    measured: false,
                }
            })
            .filter(|p| p.time >= -1.0)
            .collect();
        if points.is_empty() {
            return Err(anyhow!(
                "the disc's entry-point map is empty for this recording"
            ));
        }
        // The last picture is not in the map -- the map holds the ones a
        // player may *start* at -- so the container's own length stands
        // wherever it has one, and `end: None` is how that is said.
        //
        // Where it has none, nothing else does either, and a recording of
        // unknown length is one no range can be cut out of: every `--keep`
        // is refused for beginning after the recording ends at zero. The
        // clips on the BD-REs a recorder writes here are that case -- each
        // holds several arrival-time sequences, and libavformat declines to
        // measure a stream whose clock restarts part-way through. The last
        // point in the map, plus the picture it names, is then the best
        // floor there is. It is a floor and not the length: the last picture
        // is still not in the map, so a recording read this way ends up to
        // one interval of entry points short of where it really ends.
        let told = unsafe { (*ictx.as_ptr()).duration != ff::ffi::AV_NOPTS_VALUE };
        let end = (!told)
            .then(|| {
                points
                    .iter()
                    .map(|p| p.time)
                    .fold(f64::NEG_INFINITY, f64::max)
                    .into_finite()
                    .map(|t| t + video.frame_duration())
            })
            .flatten();
        // What the map cannot say: how much the pictures weigh. Read at a
        // few dozen places rather than guessed at. See [`sample_bit_rate`].
        let bit_rate = sample_bit_rate(&mut ictx, video, &points);
        Ok(Index {
            points,
            leading_known: false,
            pulldown: None,
            variable: None,
            bit_rate,
            end,
        })
    }
}

/// Take the entry points from the container's own seek table.
///
/// MP4 and Matroska both carry one, so this skips the read entirely. It says
/// nothing about leading pictures, hence `leading_known: false`.
///
/// **A container that keeps no table still hands one over.** libavformat
/// indexes what it reads, so a program stream or a transport stream answers
/// with whatever the probe happened to pass on its way to the first few
/// frames -- on a DVD title measured here, eight entries covering the first
/// four seconds of twelve and a half minutes. Nothing in the shape of the
/// answer says which kind it is, and taken at face value that one is worse
/// than no index at all: a cut anywhere past the fourth second finds no
/// entry point to copy from and re-encodes the lot, and `--scenes` pulls
/// every boundary in the recording onto the same picture. So the table is
/// asked the one question that needs nothing read -- does it cover the
/// recording? -- and a table that only covers where the probe went is
/// declined, which sends the caller on to the walk. See [`covers`].
pub struct ContainerIndex;

impl IndexSource for ContainerIndex {
    fn name(&self) -> &'static str {
        "container seek table"
    }

    fn build(&self, input: IndexInput) -> Result<Index> {
        let IndexInput {
            video,
            start_time,
            mut ictx,
            ..
        } = input;
        // How long the recording is, by the container's own reckoning. Only
        // used to ask whether the table below covers it, so a container that
        // will not say how long it is simply asks nothing.
        let duration = unsafe {
            let d = (*ictx.as_ptr()).duration;
            (d != ff::ffi::AV_NOPTS_VALUE)
                .then(|| d as f64 / ff::ffi::AV_TIME_BASE as f64)
                .filter(|d| *d > 0.0)
        };
        // The entries as the table holds them: a timestamp on the stream's
        // own clock and the byte its picture lies at. Kept in that form as
        // well as in seconds because [`table_shift`] asks the stream about
        // them, and what it has to ask with is the number the table gave.
        let mut entries: Vec<(i64, i64)> = Vec::new();
        {
            let stream = ictx
                .stream(video.stream_index)
                .ok_or_else(|| anyhow!("stream {} vanished", video.stream_index))?;
            unsafe {
                let st = stream.as_ptr() as *mut ff::ffi::AVStream;
                let n = ff::ffi::avformat_index_get_entries_count(st);
                for i in 0..n {
                    let e = ff::ffi::avformat_index_get_entry(st, i);
                    if e.is_null() || (*e).flags() & ff::ffi::AVINDEX_KEYFRAME == 0 {
                        continue;
                    }
                    entries.push(((*e).timestamp, (*e).pos));
                }
            }
        }
        if entries.is_empty() {
            return Err(anyhow!("the container has no seek table for this stream"));
        }
        entries.sort_by_key(|&(ts, _)| ts);
        let mut points: Vec<AccessPoint> = entries
            .iter()
            .map(|&(ts, pos)| {
                let t = ts as f64 * video.time_base - start_time;
                AccessPoint {
                    time: t,
                    lead_start: t,
                    lead_indices: Vec::new(),
                    droppable: true,
                    pos,
                    measured: false,
                }
            })
            .collect();
        covers(&points.iter().map(|p| p.time).collect::<Vec<_>>(), duration)?;
        // The table counts on its own clock, and it is not always the one a
        // point is read on. See [`table_shift`], which measures the distance
        // between them at the table's own entries.
        let stamps: Vec<i64> = entries.iter().map(|&(ts, _)| ts).collect();
        let (head, rest) = table_shift(&mut ictx, video, &stamps)?;
        for (i, slot) in points.iter_mut().enumerate() {
            slot.time += if i == 0 { head } else { rest };
            slot.lead_start = slot.time;
        }
        Ok(Index {
            points,
            leading_known: false,
            pulldown: None,
            variable: None,
            bit_rate: None,
            end: None,
        })
    }
}

/// How many of a seek table's entries are read back to measure the distance
/// between its clock and the pictures'.
const SHIFT_SAMPLES: usize = 8;

/// How far a seek table's clock stands from the clock a point is read on,
/// measured at the table's own entries. Comes back as the shift the first
/// entry wants and the shift the rest want.
///
/// **A table is indexed in decode order and a point is read as the instant
/// its picture is shown.** MP4 keeps its entries in the sample table, whose
/// timestamps are decode times, and libavformat hands one over as it stands;
/// Matroska counts its cues in presentation time and wants nothing done to
/// them. Nothing in the shape of the answer says which kind it is, so it is
/// asked: seek to an entry, and read what the picture it lands on says its
/// own time is.
///
/// Where the stream reorders nothing the two clocks are the same and this
/// comes back nought. Where it carries B pictures they stand the reorder
/// delay apart -- two frames, 0.083 s, on the 23.976 fps H.264 MP4 measured
/// here -- and every entry point came out that much early. What that costs is
/// not a frame's worth of anything: an instant two frames before an entry
/// point is not an entry point, so the film strip's cells stopped standing on
/// held pictures and were decoded out of the recording from the GOP before,
/// one GOP of decoding apiece. On material whose GOPs run four seconds and
/// longer -- which is what a file off the web is -- that is what a refresh
/// cost, and the strip stood still while the playhead went on.
///
/// **The first entry is measured on its own**, because the first picture is
/// not like the others: nothing is decoded before it, so it waits only as
/// long as the codec's own delay where the rest wait for their leading
/// pictures too. On the open-GOP fixture the head stands two frames from its
/// entry and every other picture five, and a single shift put one or the
/// other five frames out.
///
/// Measured rather than worked out from the reorder depth the container
/// declares, and measured at several places rather than at one: the question
/// is what *this* table's timestamps mean, and a table that gives no one
/// answer to it -- a delay that changes part way through, entries that land
/// on pictures they do not name -- is one nothing here can mend. It is
/// declined, and the caller walks the packets as it does for any other table
/// that cannot be shown to be describing the recording.
///
/// What is left over is material whose pictures do not come at one rate. What
/// a picture waits is a whole number of pictures and what this measures is
/// seconds, so where the durations vary the two part company by a fraction of
/// a frame here and there. On a variable-rate recording of 1467 entry points
/// measured here, four of them end up more than half a frame from their
/// picture -- against all 1467 of them before. Those four cost the film strip
/// a decode apiece, and a cut near one is put back onto its picture by
/// [`refine_leading`] like any other.
pub fn table_shift(
    ictx: &mut ff::format::context::Input,
    video: &VideoInfo,
    stamps: &[i64],
) -> Result<(f64, f64)> {
    let half = video.frame_duration() / 2.0;
    let at = |ts: i64| ts as f64 * video.time_base;
    // Which entry stands nearest an instant, which is how a picture is
    // matched back to the entry that names it.
    let nearest = |t: f64| {
        let i = stamps.partition_point(|&ts| at(ts) < t);
        [i.wrapping_sub(1), i]
            .iter()
            .filter_map(|&j| stamps.get(j).map(|&ts| (j, at(ts))))
            .min_by(|a, b| (a.1 - t).abs().total_cmp(&(b.1 - t).abs()))
    };
    let take = SHIFT_SAMPLES.min(stamps.len());
    let mut head: Option<f64> = None;
    let mut body: Vec<f64> = Vec::new();
    for k in 0..take {
        let stamp = stamps[k * stamps.len() / take];
        unsafe {
            let sought = ff::ffi::av_seek_frame(
                ictx.as_mut_ptr(),
                video.stream_index as i32,
                stamp,
                ff::ffi::AVSEEK_FLAG_BACKWARD,
            );
            if sought < 0 {
                continue;
            }
        }
        // Whichever key picture the seek landed on, which is not always the
        // one asked for: libavformat's search through the table stops at the
        // first entry it finds below the one wanted, so a seek to an entry
        // can land an entry or two early. It does not matter which -- what is
        // being measured is the distance between the clocks, and any entry
        // and its own picture measure it. Which entry it was is read back
        // below, because the first one is measured apart from the rest.
        //
        // A ceiling on the read, for a stream that hands back no key picture
        // at all after the seek: the landing is one, so this is reached only
        // where something is wrong.
        let mut landed = None;
        for (s, p) in ictx.packets().take(4096) {
            if s.index() != video.stream_index || !p.is_key() {
                continue;
            }
            landed = p.pts().map(|t| (at(t), p.dts().map_or(at(t), &at)));
            break;
        }
        let Some((shown, decoded)) = landed else {
            bail!("the container's seek table lands where the stream has no key picture");
        };
        // Which entry names this picture, and on which clock. A table counted
        // in decode order has an entry standing on the picture's decode time
        // and the shift is what the picture waits; one counted in
        // presentation order has an entry standing on the picture's own time
        // and there is nothing to shift. An entry near neither is not
        // describing this stream's pictures at all -- measured here on an
        // MPEG-2 stream in an MP4, three of whose five entries stand two
        // frames from any picture in the file -- and a shift taken off it
        // would be the distance to the entry before.
        let found = nearest(decoded)
            .filter(|(_, e)| (e - decoded).abs() <= half)
            .map(|(i, e)| (i, shown - e))
            .or_else(|| {
                nearest(shown)
                    .filter(|(_, e)| (e - shown).abs() <= half)
                    .map(|(i, _)| (i, 0.0))
            });
        let Some((i, shift)) = found else {
            bail!(
                "the container's seek table has no entry on the picture at {shown:.3}s -- it is \
                 not a table of this stream's entry points"
            );
        };
        if i == 0 {
            head = Some(shift);
        } else {
            body.push(shift);
        }
    }
    if body.is_empty() {
        bail!("the container's seek table was read back at no entry but its first");
    }
    let lo = body.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = body.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    // Half a frame, which is the width of the question being asked: the two
    // clocks are a whole number of pictures apart or they are not one answer.
    if hi - lo > half {
        bail!(
            "the container's seek table stands between {lo:.3}s and {hi:.3}s from the pictures \
             it names -- it is not counting on one clock"
        );
    }
    let rest = crate::thumbs::median_gap(&body).unwrap_or(0.0);
    Ok((head.unwrap_or(rest), rest))
}

/// Does a seek table cover the recording, or only the part a probe read?
///
/// There are two ways to fall short of covering it, and a program stream
/// shows both. The plain one is stopping early: the table runs out where the
/// probe stopped and the rest of the recording has no entry point in it. The
/// other is a hole, and it is the one that gets through a test written only
/// against the end -- libavformat measures a program stream's length by
/// seeking to the far end and reading what is there, and indexes that packet
/// like any other. The table it leaves behind on a DVD title measured here is
/// ten entries over the first five seconds and one entry at 5110.6s of
/// 5110.9s. It reaches the end. It holds nothing in between, and the film
/// strip drawn from it has two cells in an hour and a half.
///
/// So both are asked, each against the figure that can answer it. The end is
/// asked against the table's own spacing, taken as the median gap, because a
/// real table's last entry is an entry or two short of the end whatever it is
/// spaced at. The hole is asked against the recording's length instead: gaps
/// in a real table vary by more than a factor of three -- an encoder puts an
/// entry wherever the picture changes -- and what is being caught here is not
/// an uneven table but an empty one, so the bar is set where no table a
/// container kept can reach it and a probe's cannot help but.
///
/// The floors are for the short recording, whose entry points sit in the
/// first seconds because that is where the recording is.
fn covers(times: &[f64], duration: Option<f64>) -> Result<()> {
    let (Some(&first), Some(&last)) = (times.first(), times.last()) else {
        return Ok(());
    };
    let gaps: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let widest = gaps.iter().copied().fold(0.0, f64::max);
    let allowed = (duration.unwrap_or(last - first) / 10.0).max(30.0);
    if widest > allowed {
        bail!(
            "the container's seek table leaves {widest:.1}s with no entry point in it -- it is \
             what the probe read, not a table the container kept"
        );
    }
    // **A table of one entry is not a table.** It has no spacing to be
    // measured by, so the widest gap above is nought and passes, and the
    // end's question below is asked against a floor of ten seconds -- so
    // every recording shorter than that, and every short one whose probe
    // stopped after the first picture, was taken at its word. Measured on an
    // eight-second WebM whose pictures carry eight entry points: the table
    // offered one, at nought, and the cut re-encoded the whole range rather
    // than copying five seconds of it. Nothing is lost by declining, because
    // what a decline asks for is the walk, and a recording with genuinely
    // one entry point is one GOP long and walked in no time at all.
    if times.len() < 2 {
        bail!(
            "the container's seek table has one entry and cannot be shown to cover the \
             recording -- it is what the probe read, not a table the container kept"
        );
    }
    if let Some(duration) = duration {
        let spacing = crate::thumbs::median_gap(&gaps).unwrap_or(0.0);
        if duration - last > (spacing * 3.0).max(10.0) {
            bail!(
                "the container's seek table stops at {last:.1}s of {duration:.1}s -- it is \
                 what the probe read, not a table the container kept"
            );
        }
    }
    Ok(())
}

/// `f64::max` over an empty run gives negative infinity, which is not an
/// answer about where a recording ends.
trait Finite {
    fn into_finite(self) -> Option<f64>;
}

impl Finite for f64 {
    fn into_finite(self) -> Option<f64> {
        self.is_finite().then_some(self)
    }
}

/// Is an environment switch turned off?
fn off(key: &str) -> bool {
    matches!(
        std::env::var(key).as_deref(),
        Ok("0") | Ok("off") | Ok("no")
    )
}

/// The access point decoding has to begin at to produce the picture at `time`.
pub fn entry_before(points: &[AccessPoint], time: f64, slack: f64) -> Option<&AccessPoint> {
    points.iter().rev().find(|p| p.time <= time + slack)
}

/// Put the demuxer on the access point that can decode `time`, by byte
/// offset. Answers the access point's presentation time, or nothing if the
/// caller has to fall back to a timestamp seek.
///
/// This is the whole point of holding an index rather than asking the
/// container. A transport stream carries no seek table, so libavformat
/// answers a timestamp by bisecting the file, and the landing is only
/// approximate: it can arrive past the entry point wanted, or inside a GOP
/// whose sequence header has already gone by. The only fix available without
/// an index is to aim `seek_margin` seconds early and read forward, which
/// costs a few GOPs of decoding on every move of the pointer -- and still
/// fails, sometimes, when one margin is not enough.
///
/// The index has the byte the access point's packet begins at, so there is
/// nothing to approximate: seek there and the next packet out of the demuxer
/// is the one wanted.
pub fn seek_to_entry(
    ictx: &mut ff::format::context::Input,
    src: &Source,
    time: f64,
) -> Option<f64> {
    if !src.byte_seekable || off("SMARTCUT_BYTE_SEEK") {
        return None;
    }
    let entry = entry_before(&src.points, time, 1e-6)?;
    if entry.pos < 0 {
        return None;
    }
    // Stream index -1 with AVSEEK_FLAG_BYTE means "the timestamp is a byte
    // offset": libavformat repositions the file and flushes what it had
    // buffered, and the demuxer picks the stream up again from there.
    let placed = unsafe {
        ff::ffi::av_seek_frame(ictx.as_mut_ptr(), -1, entry.pos, ff::ffi::AVSEEK_FLAG_BYTE) >= 0
    };
    placed.then_some(entry.time)
}

// --- shared machinery ---------------------------------------------------

struct PacketView {
    pts: f64,
    /// Decode timestamp. A container's seek table is indexed by this, not by
    /// presentation time, and on a B-pyramid the gap between the two is not
    /// even constant -- so an entry has to be matched on DTS and read back as
    /// PTS.
    dts: f64,
    key: bool,
    reference: bool,
    /// Byte offset the packet starts at, or -1 where the demuxer does not say.
    pos: i64,
}

/// The access point rooted at `i`, from the packets that follow it.
///
/// `always_droppable` is the codec answering for its leading pictures where
/// it can, rather than each picture answering for itself; see
/// [`bitstream::leading_always_droppable`].
fn point_at(packets: &[PacketView], i: usize, always_droppable: bool) -> AccessPoint {
    let pkt = &packets[i];
    let mut lead_start = pkt.pts;
    let mut lead_indices = Vec::new();
    let mut droppable = true;
    for (j, next) in packets[i + 1..].iter().enumerate() {
        if next.key {
            break;
        }
        if next.pts < pkt.pts {
            lead_start = lead_start.min(next.pts);
            lead_indices.push(j + 1);
            droppable &= always_droppable || !next.reference;
        }
    }
    AccessPoint {
        time: pkt.pts,
        lead_start,
        lead_indices,
        droppable,
        pos: pkt.pos,
        measured: true,
    }
}

/// Derive access points from a run of packets in decode order.
fn points_from(packets: &[PacketView], codec: &str) -> Vec<AccessPoint> {
    let always_droppable = bitstream::leading_always_droppable(codec);
    let mut points: Vec<AccessPoint> = (0..packets.len())
        .filter(|&i| packets[i].key)
        .map(|i| point_at(packets, i, always_droppable))
        .collect();
    points.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    points
}

/// Measure the leading pictures of the access points a cut will actually use.
///
/// **Only the ones a boundary can land on.** A cut copies from one entry
/// point to another and re-encodes the fringes; every point in between is
/// carried through untouched, and nothing is ever asked about it. So what
/// needs measuring is the run of points around each end of each range -- the
/// planner walks forward from the start until it finds one it can open a copy
/// on, and back from the end likewise, and `SKIRT` entry points is far more
/// than that walk has ever needed.
///
/// `url` is what a demuxer is given rather than what the recording is called:
/// a clip inside an image is a byte range of the image, and the name it goes
/// by opens nothing. See [`crate::input::Input::url`].
///
/// It used to be every point inside the range, which cost a seek and a short
/// read apiece. That was invisible while an index came from a walk over the
/// packets, since a walk answers for itself and this never ran. Handed the
/// sixteen thousand entry points a Blu-ray records for a two-and-a-half-hour
/// title, it took ten minutes to do what the walk it replaced took nine to
/// do -- which is to say the disc's own index bought nothing at all.
///
/// **Each point once.** A point read here is marked [`AccessPoint::measured`]
/// and passed over from then on, because the plan is made again on every edit
/// and the same boundaries come round again and again.
///
/// **By byte where the index knows it** (`byte_seek`, which is
/// [`crate::Source::byte_seekable`]). A timestamp seek on a transport stream
/// bisects the file, and on an 81 GB UHD title that is most of a second and a
/// half a point -- a minute for the forty-six points a plan over the whole
/// title reads. The entry-point map says which byte each picture begins at.
pub fn refine_leading(
    url: &str,
    video: &VideoInfo,
    start_time: f64,
    byte_seek: bool,
    points: &mut [AccessPoint],
    ranges: &[(f64, f64)],
) -> Result<()> {
    /// How many entry points either side of a boundary are measured.
    ///
    /// Counted in points rather than turned into seconds, because what the
    /// planner walks is points. A disc puts an entry point at every scene
    /// change, and on the UHD title measured here there are runs of them a
    /// frame apart: a stretch of seconds says nothing about how many that is.
    const SKIRT: usize = 24;
    let always_droppable = bitstream::leading_always_droppable(&video.codec);
    let mut wanted = vec![false; points.len()];
    for (t_in, t_out) in ranges {
        for t in [*t_in, *t_out] {
            let i = points.partition_point(|p| p.time < t);
            let lo = i.saturating_sub(SKIRT);
            let hi = (i + SKIRT).min(points.len());
            for w in &mut wanted[lo..hi] {
                *w = true;
            }
        }
    }
    if !points.iter().zip(&wanted).any(|(p, &w)| w && !p.measured) {
        return Ok(());
    }
    let mut ictx = crate::input::demux(url)?;
    for (slot, &w) in points.iter_mut().zip(&wanted) {
        if !w || slot.measured {
            continue;
        }
        let at = (byte_seek && slot.pos >= 0).then_some(slot.pos);
        let window = window_at(&mut ictx, video, start_time, slot.time, at)?;
        let half = video.frame_duration() / 2.0;
        // Match on either clock: a walk reports presentation time, a
        // container's seek table reports decode time.
        let hit = window.iter().position(|p| {
            p.key && ((p.pts - slot.time).abs() < half || (p.dts - slot.time).abs() < half)
        });
        if let Some(i) = hit {
            let found = point_at(&window, i, always_droppable);
            slot.time = found.time;
            slot.lead_start = found.lead_start;
            slot.lead_indices = found.lead_indices;
            slot.droppable = found.droppable;
            // A container's seek table already gave a position; keep it
            // if this read could not better it.
            if found.pos >= 0 {
                slot.pos = found.pos;
            }
        }
        // Whether or not the read found it: a second read of the same
        // bytes would find the same thing.
        slot.measured = true;
    }
    Ok(())
}

/// How far a stretch's first entry point may be from the first picture at
/// that stretch's first byte before the map is judged not to be describing
/// the stretch at all.
///
/// Wide, deliberately. What this catches is a map that is seconds out, and a
/// map that is a frame or two out is a map doing its job: the point a
/// recorder writes is the picture a player starts at, and where it names one
/// picture either side of the one the demuxer hands back, nothing downstream
/// notices.
const STRETCH_SLACK: f64 = 1.0;

/// The first key picture at or after `byte`, on the clock the points are on.
///
/// Placed by byte rather than by time on purpose: the question being asked is
/// what the stream holds at a position, and the times are the very thing
/// under suspicion.
fn key_at_byte(
    ictx: &mut ff::format::context::Input,
    video: &VideoInfo,
    start_time: f64,
    byte: u64,
) -> Option<f64> {
    unsafe {
        if ff::ffi::av_seek_frame(ictx.as_mut_ptr(), -1, byte as i64, ff::ffi::AVSEEK_FLAG_BYTE) < 0
        {
            return None;
        }
    }
    for (s, p) in ictx.packets().take(8192) {
        if s.index() != video.stream_index || !p.is_key() {
            continue;
        }
        if p.position() >= 0 && (p.position() as u64) < byte {
            continue;
        }
        if let Some(pts) = p.pts() {
            return Some(pts as f64 * video.time_base - start_time);
        }
    }
    None
}

/// The entry points one stretch of a clip really has, read off the stream.
///
/// Shaped like the ones [`DiscIndex`] hands over rather than like the walk's:
/// where the leading pictures go is not asked, because the caller is mending
/// an index that never said and [`refine_leading`] measures the handful that
/// matter. What this supplies is the pair the map got wrong — a time and the
/// byte it belongs to.
/// Comes back with where the stretch's pictures end as well, which is the
/// other thing the map got wrong: a sequence whose entries are six seconds
/// out also has a span six seconds too long, and a range planned to that span
/// asks the cutter to re-encode a window with nothing in it.
fn stretch_points(
    ictx: &mut ff::format::context::Input,
    video: &VideoInfo,
    start_time: f64,
    lo: u64,
    hi: u64,
) -> (Vec<AccessPoint>, Option<f64>) {
    unsafe {
        if ff::ffi::av_seek_frame(ictx.as_mut_ptr(), -1, lo as i64, ff::ffi::AVSEEK_FLAG_BYTE) < 0 {
            return (Vec::new(), None);
        }
    }
    let mut out = Vec::new();
    let mut last = f64::NEG_INFINITY;
    let mut read_at = lo;
    for (s, p) in ictx.packets() {
        if p.position() >= 0 {
            read_at = p.position() as u64;
        }
        if read_at >= hi {
            break;
        }
        if s.index() != video.stream_index || read_at < lo {
            continue;
        }
        let Some(pts) = p.pts() else { continue };
        let t = pts as f64 * video.time_base - start_time;
        last = last.max(t);
        if p.is_key() {
            out.push(AccessPoint {
                time: t,
                lead_start: t,
                lead_indices: Vec::new(),
                droppable: true,
                pos: read_at as i64,
                measured: false,
            });
        }
    }
    // The last picture's own instant, not the end of the time it occupies.
    // A range that ends at the latter asks for one more picture than the
    // stretch has, and a re-encode of one picture that is not there is the
    // same failure in miniature as the one this whole function exists to
    // stop. Ending on it instead leaves that picture out of the cut, which
    // at a seam that already gives up a third of a second is nothing.
    let end = last.is_finite().then_some(last);
    (out, end)
}

/// Replace the entry points of any stretch whose map does not describe it.
///
/// **A recorder can write a map that is not about the stream it sits beside.**
/// The clips a BD recorder writes hold several arrival-time sequences (see
/// [`crate::restamp`]), and on one of the twenty measured here the fourth
/// sequence's entries carry times six seconds ahead of the pictures at the
/// positions they name — and the first two of them a further 11.65 seconds,
/// which is one turn of the eleven bits a fine entry keeps. Nothing in the
/// map says so. What the cut saw was an entry point that was "passed without
/// being met": it seeked to a time the map gave, read forward, and the
/// picture it was promised was eight seconds further on. That stopped the
/// whole recording, and it was the one title of twenty that could not be
/// written.
///
/// So each stretch is asked one question that costs a seek and a short read:
/// is the first picture at your first byte the one your first entry point
/// names? Where it is not, the stretch's entries are thrown away and the
/// stream is read for them instead — that stretch only, which is a few
/// hundred megabytes rather than the whole clip, and only on a clip that
/// carries seams at all.
///
/// **The seam after a mended stretch is pulled back to where its pictures
/// end.** The span the table gives a bad sequence is as wrong as its entries:
/// on the clip measured here it claims 316 seconds for 289 seconds of
/// pictures, and a range planned to that span ends 8.8 seconds past the last
/// picture there is — which is a re-encode with nothing in it, and the cut
/// stops on that instead. Only ever pulled back, never pushed out: a seam is
/// where the recorder stopped, and the sequence after it begins where the
/// table says.
///
/// Returns where it mended, for the caller to say so.
pub fn mend_stretches(
    url: &str,
    video: &VideoInfo,
    start_time: f64,
    joins: &mut [crate::restamp::Seam],
    points: &mut Vec<AccessPoint>,
) -> Result<Vec<f64>> {
    if joins.is_empty() {
        return Ok(Vec::new());
    }
    let mut ictx = crate::input::demux(url)?;
    let mut mended = Vec::new();
    for i in 0..joins.len() {
        let (at, time) = (joins[i].at, joins[i].time);
        let hi = joins.get(i + 1).map_or(u64::MAX, |j| j.at);
        let Some(first) = key_at_byte(&mut ictx, video, start_time, at) else {
            continue;
        };
        let claimed = points
            .iter()
            .find(|p| p.pos >= 0 && (p.pos as u64) >= at && (p.pos as u64) < hi)
            .map(|p| p.time);
        if claimed.is_some_and(|t| (t - first).abs() <= STRETCH_SLACK) {
            continue;
        }
        let (fresh, end) = stretch_points(&mut ictx, video, start_time, at, hi);
        if fresh.is_empty() {
            continue;
        }
        points.retain(|p| p.pos < 0 || (p.pos as u64) < at || (p.pos as u64) >= hi);
        points.extend(fresh);
        points.sort_by(|a, b| {
            a.time
                .partial_cmp(&b.time)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if let (Some(end), Some(next)) = (end, joins.get_mut(i + 1)) {
            next.ends = next.ends.min(end);
        }
        mended.push(time);
    }
    Ok(mended)
}

/// Packets around one access point, in decode order.
fn window_at(
    ictx: &mut ff::format::context::Input,
    video: &VideoInfo,
    start_time: f64,
    at: f64,
    byte: Option<i64>,
) -> Result<Vec<PacketView>> {
    // Straight to the picture's own packet where the index says where it is,
    // and a timestamp seek a second short of it otherwise. Should the read
    // from the byte not find it there, the timestamp is tried instead.
    if let Some(pos) = byte {
        let placed = unsafe {
            ff::ffi::av_seek_frame(ictx.as_mut_ptr(), -1, pos, ff::ffi::AVSEEK_FLAG_BYTE) >= 0
        };
        if placed {
            let out = window_from(ictx, video, start_time, at, Some(pos))?;
            if out.iter().any(|p| p.key && p.pos == pos) {
                return Ok(out);
            }
        }
    }
    let target = ((at - 1.0).max(0.0) + start_time) * ff::ffi::AV_TIME_BASE as f64;
    let _ = ictx.seek(target as i64, ..target as i64);
    window_from(ictx, video, start_time, at, None)
}

/// The packets from wherever `ictx` stands up to the access point after `at`.
///
/// `landed` is the byte a read was placed on, where it was placed by byte.
/// Then the picture wanted is the one that begins *there*, and nothing else
/// will do: in a run of entry points a frame apart, the next picture's decode
/// time is this one's presentation time, and matching on either clock took
/// the neighbour for it. Where the first key picture out is some other one,
/// the demuxer dropped the packet on the seek -- it does, when the read
/// before stopped on that very packet -- and this gives up at once rather
/// than read on to the cap, which on 4K is a hundred megabytes for nothing.
fn window_from(
    ictx: &mut ff::format::context::Input,
    video: &VideoInfo,
    start_time: f64,
    at: f64,
    landed: Option<i64>,
) -> Result<Vec<PacketView>> {
    let mut out = Vec::new();
    let mut target: Option<usize> = None;
    for (s, p) in ictx.packets() {
        if s.index() != video.stream_index {
            continue;
        }
        let Some(pts) = p.pts() else { continue };
        let t = pts as f64 * video.time_base - start_time;
        let d = p.dts().unwrap_or(pts) as f64 * video.time_base - start_time;
        let key = p.is_key();
        let half = video.frame_duration() / 2.0;
        let here = match landed {
            Some(byte) => p.position() as i64 == byte,
            None => (t - at).abs() < half || (d - at).abs() < half,
        };
        if target.is_none() && key && here {
            target = Some(out.len());
        }
        if landed.is_some() && target.is_none() && (key || d > at + 1.0) {
            break;
        }
        let reference = p
            .data()
            .map(|d| bitstream::is_reference(d, &video.codec, video.framing, video.vc1.as_ref()))
            .unwrap_or(true);
        out.push(PacketView {
            pts: t,
            dts: d,
            key,
            reference,
            pos: p.position() as i64,
        });
        // Stop at the *next* access point: everything between it and the
        // target is what the target's leading pictures could be. Matching on
        // DTS means the target's own PTS is already past `at`, so the test
        // has to be against the target itself, not against `at`.
        if let Some(ti) = target {
            let last = out.len() - 1;
            if last > ti && out[last].key && out[last].pts > out[ti].pts + half {
                break;
            }
        }
        if out.len() > 4096 {
            break;
        }
    }
    Ok(out)
}

/// How many places to read the pictures at when the index did not count them.
///
/// The figure being measured is a ratio that holds steady across a recording,
/// not the rate itself, so this does not have to be many. See
/// [`sample_bit_rate`].
pub const SAMPLE_POINTS: usize = 24;

/// And how much of the recording to read at each of them.
///
/// Measured between two entry points rather than over a stopwatch, so this is
/// a floor: a sample runs to the first entry point at least this far on.
pub const SAMPLE_SECONDS: f64 = 2.0;

/// What the pictures weigh, measured here and there rather than counted.
///
/// A disc's entry-point map says where every picture a player may start at is
/// and nothing whatever about how big any of them are, so a recording read out
/// of one arrives with no rate on it. That figure is what `--fit` sizes a disc
/// from and what a re-encoded stretch is written at, and what stood in for it
/// -- the whole file's rate, less a tenth for the sound -- is a guess.
/// Measured against six broadcast recordings written to a BDAV disc the guess
/// was 2.1% low: the pictures are 92.0% of what the file weighs there and the
/// guess says 90%. Two percent of a disc is a quarter of a gigabyte, and a
/// disc a quarter of a gigabyte too large is four hours of writing and a
/// coaster.
///
/// **What is read is the share, not the rate.** A broadcast recording's rate
/// over any two seconds of it has nothing to do with its rate over the hour:
/// the twenty-four windows sampled on the recording measured here run from 5
/// to 14 Mbit/s against an average of 11, and their mean lands within about
/// five percent of it -- which is five percent of a disc, no better than the
/// guess it replaces. The *share* the pictures take of those bytes is the
/// steady thing: 0.875 to 0.937 across the same windows, and byte-weighted
/// 0.920 against a counted 0.9195.
///
/// So the share is what the reading is for, and the rate it is applied to
/// comes from the map itself -- the bytes between its first entry point and
/// its last, over the time between them -- which is exact and costs nothing.
/// Tens of megabytes read, and on that recording the answer lands 0.3% from
/// the counted one.
///
/// **Between two points of the map, not over a span of timestamps.** A window
/// ended on the timestamps as they come out would stop mid-reorder and leave
/// two or three pictures of its own span unweighed. The map's positions bound
/// the bytes at both ends and the same positions bound the window they are
/// divided by, so the two agree by construction.
pub fn sample_bit_rate(
    ictx: &mut ff::format::context::Input,
    video: &VideoInfo,
    points: &[AccessPoint],
) -> Option<f64> {
    // Only the points the stream can be opened at, in the order they sit in
    // the file.
    let placed: Vec<&AccessPoint> = points.iter().filter(|p| p.pos >= 0).collect();
    if placed.len() < 2 {
        return None;
    }
    // What the recording as a whole runs at, off the map. The last entry
    // point is not the last picture and the first is not always the first
    // byte, so this is the rate across what the map covers rather than
    // across the file -- which is the same rate, and is the one the samples
    // below are a share of.
    let (first, last) = (placed[0], placed[placed.len() - 1]);
    if last.pos <= first.pos || last.time <= first.time {
        return None;
    }
    let stream_rate = (last.pos - first.pos) as f64 * 8.0 / (last.time - first.time);

    // Each sample runs from one point to the first that is SAMPLE_SECONDS
    // further on, so the starts come from everything with room for one
    // behind it.
    let starts: Vec<usize> = (0..placed.len())
        .filter(|&i| last.time - placed[i].time >= SAMPLE_SECONDS)
        .collect();
    if starts.is_empty() {
        return None;
    }
    let want = SAMPLE_POINTS.min(starts.len());
    let (mut pictures, mut window) = (0u64, 0u64);
    for k in 0..want {
        let i = starts[k * starts.len() / want];
        let from = placed[i];
        let Some(to) = placed[i + 1..]
            .iter()
            .find(|p| p.time - from.time >= SAMPLE_SECONDS)
        else {
            continue;
        };
        // A map whose positions do not climb is one this cannot measure
        // against: the sample is skipped rather than counted backwards.
        if to.pos <= from.pos {
            continue;
        }
        unsafe {
            if ff::ffi::av_seek_frame(ictx.as_mut_ptr(), -1, from.pos, ff::ffi::AVSEEK_FLAG_BYTE)
                < 0
            {
                continue;
            }
        }
        let (mut took, mut read_at) = (0u64, from.pos);
        for (s, p) in ictx.packets() {
            if p.position() >= 0 {
                read_at = p.position() as i64;
            }
            if read_at >= to.pos {
                break;
            }
            if s.index() == video.stream_index && read_at >= from.pos {
                took += p.size() as u64;
            }
        }
        if took == 0 {
            continue;
        }
        pictures += took;
        window += (to.pos - from.pos) as u64;
    }
    if window == 0 {
        return None;
    }
    // Byte-weighted on purpose: a busy window is more of the recording than a
    // quiet one, and it is the recording's own share that is wanted.
    let share = pictures as f64 / window as f64;
    (share * stream_rate)
        .into_finite()
        .filter(|r| *r > 0.0 && share < 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Entry points every `every` seconds from `from` to `to`.
    fn every(from: f64, to: f64, every: f64) -> Vec<f64> {
        let mut out = Vec::new();
        let mut t = from;
        while t < to {
            out.push(t);
            t += every;
        }
        out
    }

    /// What libavformat hands over for the main title of a DVD measured
    /// here: the probe's own half second of entry points, and the packet the
    /// duration estimate found by seeking to the far end. It reaches the end
    /// of the recording and holds nothing in between.
    #[test]
    fn the_probes_table_with_the_end_on_it_is_not_a_table() {
        let mut times = every(0.467, 5.0, 0.5005);
        times.push(5110.572);
        assert_eq!(times.len(), 11);
        let e = covers(&times, Some(5110.944)).unwrap_err().to_string();
        assert!(e.contains("no entry point in it"), "{e}");
    }

    /// The same probe on a container that would not say how long it is: the
    /// hole is measured against the table's own reach instead, and is just
    /// as wide.
    #[test]
    fn the_hole_is_a_hole_whether_or_not_the_length_is_known() {
        let mut times = every(0.467, 5.0, 0.5005);
        times.push(5110.572);
        assert!(covers(&times, None).is_err());
    }

    /// And without it: the table simply stops where the probe stopped.
    #[test]
    fn the_probes_table_on_its_own_stops_where_the_probe_did() {
        let times = every(0.467, 5.0, 0.5005);
        let e = covers(&times, Some(5110.944)).unwrap_err().to_string();
        assert!(e.contains("stops at"), "{e}");
    }

    /// A table a container kept, with the unevenness a real one has: an
    /// entry every five seconds, a stretch of nothing where the picture held
    /// still, and a last entry a spacing short of the end.
    #[test]
    fn a_table_the_container_kept_is_taken() {
        let mut times = every(0.0, 600.0, 5.0);
        times.retain(|&t| !(300.0..340.0).contains(&t));
        assert!(covers(&times, Some(603.0)).is_ok());
    }

    /// A recording shorter than the floors: every entry point it has sits in
    /// the first seconds because that is where the recording is.
    #[test]
    fn a_short_recording_is_not_measured_against_a_long_one() {
        assert!(covers(&every(0.0, 4.0, 0.5), Some(4.2)).is_ok());
    }

    /// One entry and nothing else is the probe's table however short the
    /// recording is. It used to be taken, because a single entry leaves no
    /// gap to measure and the end's floor is ten seconds -- so every
    /// recording shorter than that passed on the strength of its first
    /// picture. An eight-second WebM measured here has eight entry points
    /// and offered one.
    #[test]
    fn one_entry_is_not_a_table_however_short_the_recording() {
        let e = covers(&[0.0], Some(4.2)).unwrap_err().to_string();
        assert!(e.contains("one entry"), "{e}");
        assert!(covers(&[0.0], None).is_err());
    }
}
