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
    /// Bits per second the pictures take, when the source read them all.
    /// See [`crate::VideoInfo::bit_rate`], which is where it ends up.
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
    pub ictx: ff::format::context::Input,
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
        let IndexInput { video, start_time, ictx, on, .. } = input;
        walk(video, start_time, ictx, on, |_| Ok(()), None)
    }
}

/// The walk itself, with each key packet offered to `key` as it goes by.
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
/// `key` is offered the packet, not the picture: what to do with it -- decode
/// it, count it, ignore it -- is the caller's business, and this file has no
/// decoder in it.
pub fn walk(
    video: &VideoInfo,
    start_time: f64,
    mut ictx: ff::format::context::Input,
    on: Option<OnProgress>,
    mut key: impl FnMut(&ff::Packet) -> Result<()>,
    stop: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<Index> {
    let stream_index = video.stream_index;
    let time_base = video.time_base;
    let codec = video.codec.clone();
    let framing = video.framing;
    let vc1 = video.vc1.as_ref();

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
    let mut pulldown = false;
    // What the pictures weigh. Free here -- the walk is holding every packet
    // already -- and there is nowhere else to learn it: a transport stream
    // declares no bit rate per stream, and the container's overall figure
    // counts the sound and the tables in with the pictures.
    let mut video_bytes: u64 = 0;
    // Said on a count rather than on every packet: a gigabyte is on the
    // order of a hundred thousand of them and the bar has 100 steps, so
    // the rest of the calls would draw nothing. One in a couple of
    // thousand works out at a dozen or so a second on a broadcast
    // recording, which is a bar that moves without being a bar that is
    // redrawn for nothing.
    let mut seen: u64 = 0;
    for (s, p) in ictx.packets() {
        seen += 1;
        if seen.is_multiple_of(2048) {
            // Asked on the same count as the progress, but not behind it: a
            // pass with nobody watching still has to be stoppable.
            if let Some(f) = stop {
                if f() {
                    bail!("abandoned");
                }
            }
            if let (Some(on), Some(total)) = (on, total) {
                let pos = p.position();
                if pos > 0 {
                    on((pos as f64 / total).clamp(0.0, 1.0));
                }
            }
        }
        if s.index() != stream_index {
            continue;
        }
        video_bytes += p.size() as u64;
        let Some(pts) = p.pts() else { continue };
        let reference =
            p.data().map(|d| bitstream::is_reference(d, &codec, framing, vc1)).unwrap_or(true);
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
        if p.is_key() {
            key(&p)?;
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
    Ok(Index {
        points: points_from(&packets),
        leading_known: true,
        pulldown: Some(pulldown),
        bit_rate,
        end,
    })
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
/// handful of points a cut actually uses. It cannot say what the pictures
/// weigh either; a re-encode falls back on the frame size. See
/// [`crate::VideoInfo::bit_rate`].
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
        let IndexInput { path, video, start_time, .. } = input;
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
                }
            })
            .filter(|p| p.time >= -1.0)
            .collect();
        if points.is_empty() {
            return Err(anyhow!("the disc's entry-point map is empty for this recording"));
        }
        // The last picture is not in the map -- the map holds the ones a
        // player may *start* at -- so the container's own length stands, and
        // `end: None` is how that is said.
        let _ = video;
        Ok(Index { points, leading_known: false, pulldown: None, bit_rate: None, end: None })
    }
}

/// Take the entry points from the container's own seek table.
///
/// MP4 and Matroska both carry one, so this skips the read entirely. It says
/// nothing about leading pictures, hence `leading_known: false`.
pub struct ContainerIndex;

impl IndexSource for ContainerIndex {
    fn name(&self) -> &'static str {
        "container seek table"
    }

    fn build(&self, input: IndexInput) -> Result<Index> {
        let IndexInput { video, start_time, ictx, .. } = input;
        let stream = ictx
            .stream(video.stream_index)
            .ok_or_else(|| anyhow!("stream {} vanished", video.stream_index))?;
        let mut points = Vec::new();
        unsafe {
            let st = stream.as_ptr() as *mut ff::ffi::AVStream;
            let n = ff::ffi::avformat_index_get_entries_count(st);
            for i in 0..n {
                let e = ff::ffi::avformat_index_get_entry(st, i);
                if e.is_null() || (*e).flags() & ff::ffi::AVINDEX_KEYFRAME == 0 {
                    continue;
                }
                let t = (*e).timestamp as f64 * video.time_base - start_time;
                points.push(AccessPoint {
                    time: t,
                    lead_start: t,
                    lead_indices: Vec::new(),
                    droppable: true,
                    pos: (*e).pos,
                });
            }
        }
        if points.is_empty() {
            return Err(anyhow!("the container has no seek table for this stream"));
        }
        points.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
        Ok(Index { points, leading_known: false, pulldown: None, bit_rate: None, end: None })
    }
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
    matches!(std::env::var(key).as_deref(), Ok("0") | Ok("off") | Ok("no"))
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
        ff::ffi::av_seek_frame(
            ictx.as_mut_ptr(),
            -1,
            entry.pos,
            ff::ffi::AVSEEK_FLAG_BYTE,
        ) >= 0
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
fn point_at(packets: &[PacketView], i: usize) -> AccessPoint {
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
            droppable &= !next.reference;
        }
    }
    AccessPoint { time: pkt.pts, lead_start, lead_indices, droppable, pos: pkt.pos }
}

/// Derive access points from a run of packets in decode order.
fn points_from(packets: &[PacketView]) -> Vec<AccessPoint> {
    let mut points: Vec<AccessPoint> = (0..packets.len())
        .filter(|&i| packets[i].key)
        .map(|i| point_at(packets, i))
        .collect();
    points.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
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
pub fn refine_leading(
    url: &str,
    video: &VideoInfo,
    start_time: f64,
    points: &mut [AccessPoint],
    ranges: &[(f64, f64)],
) -> Result<()> {
    /// How many entry points either side of a boundary are measured.
    const SKIRT: usize = 24;
    let mut ictx = crate::input::demux(url)?;
    // How far `SKIRT` points reaches in seconds, so the test below can stay a
    // comparison of times: the points are sorted by time and a boundary can
    // fall between two of them.
    let gaps: Vec<f64> = points.windows(2).map(|w| w[1].time - w[0].time).collect();
    let mean_gop =
        if gaps.is_empty() { 1.0 } else { gaps.iter().sum::<f64>() / gaps.len() as f64 };
    let skirt = (mean_gop * SKIRT as f64).clamp(2.0, 60.0);
    for (t_in, t_out) in ranges {
        for slot in points.iter_mut() {
            let near_start = (slot.time - *t_in).abs() <= skirt;
            let near_end = (slot.time - *t_out).abs() <= skirt;
            if !near_start && !near_end {
                continue;
            }
            let window = window_at(&mut ictx, video, start_time, slot.time)?;
            let half = video.frame_duration() / 2.0;
            // Match on either clock: a walk reports presentation time, a
            // container's seek table reports decode time.
            let hit = window.iter().position(|p| {
                p.key && ((p.pts - slot.time).abs() < half || (p.dts - slot.time).abs() < half)
            });
            if let Some(i) = hit {
                let found = point_at(&window, i);
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
        }
    }
    Ok(())
}

/// Packets around one access point, in decode order.
fn window_at(
    ictx: &mut ff::format::context::Input,
    video: &VideoInfo,
    start_time: f64,
    at: f64,
) -> Result<Vec<PacketView>> {
    let target = ((at - 1.0).max(0.0) + start_time) * ff::ffi::AV_TIME_BASE as f64;
    let _ = ictx.seek(target as i64, ..target as i64);
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
        if target.is_none() && key && ((t - at).abs() < half || (d - at).abs() < half) {
            target = Some(out.len());
        }
        let reference = p
            .data()
            .map(|d| bitstream::is_reference(d, &video.codec, video.framing, video.vc1.as_ref()))
            .unwrap_or(true);
        out.push(PacketView { pts: t, dts: d, key, reference, pos: p.position() as i64 });
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
