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

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;

use crate::{bitstream, AccessPoint, Source, VideoInfo};

pub struct Index {
    pub points: Vec<AccessPoint>,
    /// Whether the leading-picture fields were measured or merely assumed.
    pub leading_known: bool,
    /// Whether the stream uses 2:3 pulldown, when the source could tell.
    pub pulldown: Option<bool>,
}

/// What an index source is given to work with.
pub struct IndexInput<'a> {
    pub path: &'a str,
    pub video: &'a VideoInfo,
    /// Container start time; access point times are rebased against it.
    pub start_time: f64,
    /// A demuxer already open on the source.
    pub ictx: ff::format::context::Input,
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
        let IndexInput { video, start_time, mut ictx, .. } = input;
        let stream_index = video.stream_index;
        let time_base = video.time_base;
        let codec = video.codec.clone();
        let framing = video.framing;

        let mut packets: Vec<PacketView> = Vec::new();
        let mut pulldown = false;
        for (s, p) in ictx.packets() {
            if s.index() != stream_index {
                continue;
            }
            let Some(pts) = p.pts() else { continue };
            let reference =
                p.data().map(|d| bitstream::is_reference(d, &codec, framing)).unwrap_or(true);
            if codec == "mpeg2video" && !pulldown {
                pulldown = p.data().map(bitstream::mpeg2_repeats_field).unwrap_or(false);
            }
            packets.push(PacketView {
                pts: pts as f64 * time_base - start_time,
                dts: p.dts().unwrap_or(pts) as f64 * time_base - start_time,
                key: p.is_key(),
                reference,
                pos: p.position() as i64,
            });
        }

        Ok(Index {
            points: points_from(&packets),
            leading_known: true,
            pulldown: Some(pulldown),
        })
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
        Ok(Index { points, leading_known: false, pulldown: None })
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
/// Only the entry points that fall inside the requested ranges matter, so an
/// index that could not answer for itself costs a short read per range rather
/// than a pass over the file.
pub fn refine_leading(
    path: &str,
    video: &VideoInfo,
    start_time: f64,
    points: &mut [AccessPoint],
    ranges: &[(f64, f64)],
) -> Result<()> {
    let mut ictx = ff::format::input(&path)?;
    for (t_in, t_out) in ranges {
        for slot in points.iter_mut() {
            if slot.time < *t_in - 1.0 || slot.time > *t_out + 1.0 {
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
        let reference =
            p.data().map(|d| bitstream::is_reference(d, &video.codec, video.framing)).unwrap_or(true);
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
