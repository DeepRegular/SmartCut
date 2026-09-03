//! The tables a broadcast carries about itself, and how to keep them.
//!
//! A recording is not only pictures and sound. Around them the broadcaster
//! sends a description of what is being sent: which service this is and what
//! it is called, what each PID contains and how to read it, which programme
//! is on now and which follows, and what time it is. Those are the tables --
//! PAT, PMT, SDT, EIT, TOT -- and they are what a recorder's library view,
//! a player's channel display and every downstream tool actually read.
//!
//! None of it survives a mux. libavformat writes its own PAT, PMT and SDT
//! from what it knows, which is the streams and nothing else: the caption
//! keeps its ARIB descriptors because the muxer knows that codec, and
//! everything else -- the audio's component tag, the copy-control
//! descriptor, the superimpose stream's identity -- is simply not written.
//! EIT and TOT are worse than lost: asked to copy PID 0x12 the muxer accepts
//! it as an anonymous private stream and puts it on a PID of its own
//! choosing, where nothing will ever look for it.
//!
//! So the tables are put back afterwards, by one pass over the finished file:
//!
//!   * the PMT is rebuilt with the recording's own descriptors,
//!   * the SDT is replaced with the recording's own section, which is how
//!     the service name arrives in ARIB's own character encoding without
//!     this program having to understand a byte of it,
//!   * EIT present/following and TOT are injected on the PIDs they belong
//!     on, taken from the recording at the point each kept range starts,
//!   * and every stream is put back on the PID it arrived on.
//!
//! The pass is byte-level on purpose. Sections are not timed -- they carry
//! no PTS and are meant to be repeated -- so nothing here has to be spliced,
//! only placed and given a continuity counter that follows on.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};

/// One transport packet. The whole format is built on this being fixed.
pub const PACKET: usize = 188;

const PID_PAT: u16 = 0x0000;
const PID_SDT: u16 = 0x0011;
const PID_EIT: u16 = 0x0012;
const PID_TDT: u16 = 0x0014;

const TABLE_PAT: u8 = 0x00;
const TABLE_PMT: u8 = 0x02;
/// Service description for services in *this* transport stream.
const TABLE_SDT_ACTUAL: u8 = 0x42;
/// Event information, present and following, for this transport stream.
/// The schedule tables (0x50..0x6F) describe the days around the recording
/// and are deliberately left behind: they are large, they are about
/// programmes that are not in this file, and nothing reads them off a
/// recording.
const TABLE_EIT_PF_ACTUAL: u8 = 0x4E;
const TABLE_TDT: u8 = 0x70;
const TABLE_TOT: u8 = 0x73;

/// Descriptor tags that must not be carried across.
///
/// 0x09 is the conditional access descriptor -- it says where the entitlement
/// messages are and which system scrambles the service. The output is not
/// scrambled and carries no ECM stream, so restating it would describe a file
/// that does not exist and invites a player to wait for a key that never
/// comes.
const DROP_DESCRIPTORS: [u8; 1] = [0x09];

/// Whether a byte offset holds a packet, checked the way a demuxer checks.
///
/// One sync byte proves nothing -- 0x47 is a perfectly ordinary payload byte.
/// A run of them at exactly the packet spacing does.
fn sync_at(buf: &[u8], at: usize) -> bool {
    (0..5).all(|k| buf.get(at + k * PACKET) == Some(&0x47))
}

/// Find the first packet boundary in `buf`.
fn find_sync(buf: &[u8]) -> Option<usize> {
    (0..buf.len().min(PACKET * 8)).find(|&i| sync_at(buf, i))
}

fn pid_of(p: &[u8]) -> u16 {
    (((p[1] & 0x1F) as u16) << 8) | p[2] as u16
}

/// Where a packet's payload starts, or `None` when it carries none.
fn payload_start(p: &[u8]) -> Option<usize> {
    let afc = (p[3] >> 4) & 0x03;
    let start = match afc {
        1 => 4,
        3 => 5 + p[4] as usize,
        _ => return None, // 0 is reserved, 2 is adaptation field only
    };
    (start < PACKET).then_some(start)
}

/// The program clock reference this packet carries, in 90 kHz ticks.
///
/// The clock is the only timeline the finished file has that can be read
/// without decoding anything, which is what decides where an injected
/// section goes.
fn pcr_of(p: &[u8]) -> Option<i64> {
    let afc = (p[3] >> 4) & 0x03;
    if afc < 2 || p[4] < 7 {
        return None;
    }
    if p[5] & 0x10 == 0 {
        return None;
    }
    let base = ((p[6] as i64) << 25)
        | ((p[7] as i64) << 17)
        | ((p[8] as i64) << 9)
        | ((p[9] as i64) << 1)
        | ((p[10] as i64) >> 7);
    Some(base)
}

/// CRC-32/MPEG-2, which every section ends with and no section is accepted
/// without.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

fn section_len(sec: &[u8]) -> Option<usize> {
    (sec.len() >= 3).then(|| 3 + ((((sec[1] & 0x0F) as usize) << 8) | sec[2] as usize))
}

/// Reassemble sections arriving on one PID.
///
/// Sections are not packets: one packet can end a section and begin the next,
/// and a long section spans several packets. The pointer field in the first
/// payload byte of a unit-start packet says how far in the new section
/// begins, which is the whole of the trick.
#[derive(Default)]
struct SectionReader {
    buf: Vec<u8>,
    filling: bool,
}

impl SectionReader {
    fn feed(&mut self, packet: &[u8], mut visit: impl FnMut(&[u8])) {
        let Some(start) = payload_start(packet) else { return };
        let unit_start = packet[1] & 0x40 != 0;
        let mut data = &packet[start..];
        if unit_start {
            let Some(&pointer) = data.first() else { return };
            let pointer = pointer as usize;
            if self.filling && pointer > 0 {
                if let Some(tail) = data.get(1..1 + pointer) {
                    self.buf.extend_from_slice(tail);
                    self.drain(&mut visit);
                }
            }
            self.buf.clear();
            self.filling = true;
            let Some(rest) = data.get(1 + pointer..) else { return };
            data = rest;
        } else if !self.filling {
            return;
        }
        self.buf.extend_from_slice(data);
        self.drain(&mut visit);
    }

    /// Hand over every whole section the buffer now holds.
    fn drain(&mut self, visit: &mut impl FnMut(&[u8])) {
        loop {
            // 0xFF is stuffing: the rest of the payload is padding.
            if self.buf.first().is_none_or(|&b| b == 0xFF) {
                self.filling = false;
                self.buf.clear();
                return;
            }
            let Some(len) = section_len(&self.buf) else { return };
            if self.buf.len() < len {
                return;
            }
            let sec: Vec<u8> = self.buf.drain(..len).collect();
            // The syntax indicator says whether the section ends in a CRC.
            // Almost everything here does; time and date is the exception,
            // being short enough that the standard did not think it worth
            // one, and checking it for a CRC it never had would throw away
            // the only table that says when the recording was made.
            let checked = if sec[1] & 0x80 == 0 { sec.len() >= 3 } else { sec.len() > 4 && crc32(&sec) == 0 };
            if checked {
                visit(&sec);
            }
        }
    }
}

/// One elementary stream, as the recording's own PMT describes it.
#[derive(Debug, Clone)]
pub struct ElementaryStream {
    pub pid: u16,
    pub stream_type: u8,
    /// The descriptor loop, minus anything in [`DROP_DESCRIPTORS`].
    pub descriptors: Vec<u8>,
}

impl ElementaryStream {
    /// The component tag, which is how ARIB names a stream: 0x00 the main
    /// video, 0x10 the main audio and 0x11 the second, 0x30..0x37 the
    /// captions and 0x38..0x3F the superimposed crawls.
    pub fn component_tag(&self) -> Option<u8> {
        descriptor(&self.descriptors, 0x52).and_then(|d| d.first().copied())
    }
}

/// What the recording says about itself.
#[derive(Debug, Clone)]
pub struct Service {
    pub transport_stream_id: u16,
    pub original_network_id: u16,
    pub service_id: u16,
    pub service_type: u8,
    pub pmt_pid: u16,
    pub pcr_pid: u16,
    /// Programme-level descriptors from the PMT, minus the dropped ones.
    pub program_info: Vec<u8>,
    pub streams: Vec<ElementaryStream>,
    /// The SDT section for this service alone, ready to be written back.
    ///
    /// Kept as the bytes that arrived rather than as a decoded name. A
    /// service name is ARIB STD-B24 text, whose character set is neither
    /// UTF-8 nor anything a Rust `String` can hold without a table this
    /// program has no other use for -- and the muxer would only have to
    /// encode it again. Carrying the section across is exact and costs
    /// nothing.
    pub sdt: Option<Vec<u8>>,
}

impl Service {
    pub fn stream(&self, pid: u16) -> Option<&ElementaryStream> {
        self.streams.iter().find(|s| s.pid == pid)
    }
}

/// Find one descriptor in a loop, by tag.
pub fn descriptor(loop_bytes: &[u8], tag: u8) -> Option<&[u8]> {
    let mut i = 0;
    while i + 2 <= loop_bytes.len() {
        let t = loop_bytes[i];
        let len = loop_bytes[i + 1] as usize;
        let body = loop_bytes.get(i + 2..i + 2 + len)?;
        if t == tag {
            return Some(body);
        }
        i += 2 + len;
    }
    None
}

/// Copy a descriptor loop, leaving out the tags that must not travel.
fn keep_descriptors(loop_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(loop_bytes.len());
    let mut i = 0;
    while i + 2 <= loop_bytes.len() {
        let tag = loop_bytes[i];
        let len = loop_bytes[i + 1] as usize;
        let Some(whole) = loop_bytes.get(i..i + 2 + len) else { break };
        if !DROP_DESCRIPTORS.contains(&tag) {
            out.extend_from_slice(whole);
        }
        i += 2 + len;
    }
    out
}

/// Read the recording's own description of itself.
///
/// Only the head of the file is read. PAT, PMT and SDT repeat every few
/// hundred milliseconds, so a few megabytes is many copies of each; a
/// recording that does not carry them in its first stretch does not carry
/// them at all.
pub fn read_service(path: &str) -> Result<Service> {
    const WINDOW: usize = 8 << 20;
    let mut f = std::fs::File::open(path).with_context(|| format!("cannot open {path}"))?;
    let mut buf = vec![0u8; WINDOW];
    let n = read_fully(&mut f, &mut buf)?;
    buf.truncate(n);
    let base = find_sync(&buf).ok_or_else(|| anyhow!("{path} is not a transport stream"))?;

    let mut pat = SectionReader::default();
    let mut pmt = SectionReader::default();
    let mut sdt = SectionReader::default();

    let mut transport_stream_id = 0u16;
    let mut pmt_pid = 0u16;
    let mut service_id = 0u16;
    let mut found: Option<Service> = None;
    let mut sdt_section: Option<Vec<u8>> = None;
    let mut original_network_id = 0u16;
    let mut service_type = 0u8;

    let mut at = base;
    while at + PACKET <= buf.len() {
        let p = &buf[at..at + PACKET];
        at += PACKET;
        if p[0] != 0x47 {
            // A recording can be cut short mid-packet or carry a bad read;
            // re-find the grid rather than giving up on the file.
            match find_sync(&buf[at..]) {
                Some(off) => at += off,
                None => break,
            }
            continue;
        }
        match pid_of(p) {
            PID_PAT if pmt_pid == 0 => pat.feed(p, |sec| {
                if sec[0] != TABLE_PAT || sec.len() < 12 {
                    return;
                }
                transport_stream_id = ((sec[3] as u16) << 8) | sec[4] as u16;
                let mut i = 8;
                while i + 4 <= sec.len() - 4 {
                    let number = ((sec[i] as u16) << 8) | sec[i + 1] as u16;
                    let pid = (((sec[i + 2] & 0x1F) as u16) << 8) | sec[i + 3] as u16;
                    // Programme 0 is the network information table, not a
                    // service. The first real entry is the recording's own:
                    // a recorder writing one service writes one entry.
                    if number != 0 {
                        service_id = number;
                        pmt_pid = pid;
                        break;
                    }
                    i += 4;
                }
            }),
            pid if pid != 0 && pid == pmt_pid && found.is_none() => pmt.feed(p, |sec| {
                if sec[0] != TABLE_PMT || sec.len() < 16 {
                    return;
                }
                let number = ((sec[3] as u16) << 8) | sec[4] as u16;
                if number != service_id {
                    return;
                }
                let pcr_pid = (((sec[8] & 0x1F) as u16) << 8) | sec[9] as u16;
                let info_len = (((sec[10] & 0x0F) as usize) << 8) | sec[11] as usize;
                let Some(info) = sec.get(12..12 + info_len) else { return };
                let program_info = keep_descriptors(info);
                let mut streams = Vec::new();
                let mut i = 12 + info_len;
                let end = sec.len() - 4;
                while i + 5 <= end {
                    let stream_type = sec[i];
                    let pid = (((sec[i + 1] & 0x1F) as u16) << 8) | sec[i + 2] as u16;
                    let len = (((sec[i + 3] & 0x0F) as usize) << 8) | sec[i + 4] as usize;
                    let Some(desc) = sec.get(i + 5..i + 5 + len) else { break };
                    streams.push(ElementaryStream {
                        pid,
                        stream_type,
                        descriptors: keep_descriptors(desc),
                    });
                    i += 5 + len;
                }
                found = Some(Service {
                    transport_stream_id,
                    original_network_id: 0,
                    service_id,
                    service_type: 0,
                    pmt_pid,
                    pcr_pid,
                    program_info,
                    streams,
                    sdt: None,
                });
            }),
            PID_SDT if sdt_section.is_none() => sdt.feed(p, |sec| {
                if sec[0] != TABLE_SDT_ACTUAL || sec.len() < 15 || service_id == 0 {
                    return;
                }
                let onid = ((sec[8] as u16) << 8) | sec[9] as u16;
                // Keep the one service this recording is of, and drop the
                // rest of the multiplex: the others are not in this file.
                let mut i = 11;
                let end = sec.len() - 4;
                while i + 5 <= end {
                    let sid = ((sec[i] as u16) << 8) | sec[i + 1] as u16;
                    let len = (((sec[i + 3] & 0x0F) as usize) << 8) | sec[i + 4] as usize;
                    let Some(body) = sec.get(i..i + 5 + len) else { break };
                    if sid == service_id {
                        original_network_id = onid;
                        service_type = descriptor(&body[5..], 0x48)
                            .and_then(|d| d.first().copied())
                            .unwrap_or(0);
                        sdt_section = Some(one_service_sdt(sec, body));
                        break;
                    }
                    i += 5 + len;
                }
            }),
            _ => {}
        }
        if found.is_some() && sdt_section.is_some() {
            break;
        }
    }

    let mut service = found.ok_or_else(|| {
        anyhow!("{path} carries no program map table; there are no broadcast tables to keep")
    })?;
    service.original_network_id = original_network_id;
    service.service_type = service_type;
    service.sdt = sdt_section;
    Ok(service)
}

/// Rewrite an SDT section so it describes one service instead of a multiplex.
fn one_service_sdt(sec: &[u8], service: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(11 + service.len() + 4);
    out.extend_from_slice(&sec[..11]);
    out.extend_from_slice(service);
    finish_section(&mut out);
    out
}

/// Set a section's length field and append its CRC.
fn finish_section(sec: &mut Vec<u8>) {
    let len = sec.len() - 3 + 4;
    sec[1] = (sec[1] & 0xF0) | ((len >> 8) as u8 & 0x0F);
    sec[2] = len as u8;
    let crc = crc32(sec);
    sec.extend_from_slice(&crc.to_be_bytes());
}

/// The tables that describe what is on at one moment of the recording.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Event information, present and following, for this service.
    pub eit: Vec<Vec<u8>>,
    /// Time and date, with the offset descriptor when the broadcaster sends
    /// one. Held as the section it arrived as; the clock inside it is moved
    /// on when it is written back, so a player is not told the same second
    /// for the length of the file.
    pub tot: Option<Vec<u8>>,
    /// Whether a bare time and date table was seen instead of the offset one.
    pub tdt: Option<Vec<u8>>,
}

impl Snapshot {
    pub fn is_empty(&self) -> bool {
        self.eit.is_empty() && self.tot.is_none() && self.tdt.is_none()
    }
}

fn read_fully(f: &mut std::fs::File, buf: &mut [u8]) -> Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

/// Read the programme description in force at one point in the recording.
///
/// `pos` is a byte offset -- the one the access-point index recorded for the
/// picture a kept range opens on. Present-and-following changes only when
/// the programme does, so reading it beside each range's own first picture
/// is what makes a cut spanning two programmes describe both.
pub fn snapshot_at(path: &str, pos: i64, service_id: u16) -> Result<Snapshot> {
    // Long enough to hold several repetitions. Event information goes out
    // about every two seconds and time every five, which at broadcast rates
    // is a few megabytes.
    const WINDOW: usize = 24 << 20;
    let mut f = std::fs::File::open(path)?;
    if pos > 0 {
        f.seek(SeekFrom::Start(pos as u64))?;
    }
    let mut buf = vec![0u8; WINDOW];
    let n = read_fully(&mut f, &mut buf)?;
    buf.truncate(n);
    let Some(base) = find_sync(&buf) else { return Ok(Snapshot::default()) };

    let mut eit = SectionReader::default();
    let mut tdt = SectionReader::default();
    let mut out = Snapshot::default();
    // Present and following is two sections, numbered 0 and 1. Both are
    // wanted and either may be absent.
    let mut seen: HashSet<u8> = HashSet::new();

    let mut at = base;
    while at + PACKET <= buf.len() {
        let p = &buf[at..at + PACKET];
        at += PACKET;
        if p[0] != 0x47 {
            match find_sync(&buf[at..]) {
                Some(off) => at += off,
                None => break,
            }
            continue;
        }
        match pid_of(p) {
            PID_EIT => eit.feed(p, |sec| {
                if sec[0] != TABLE_EIT_PF_ACTUAL || sec.len() < 18 {
                    return;
                }
                if (((sec[3] as u16) << 8) | sec[4] as u16) != service_id {
                    return;
                }
                let number = sec[6];
                if seen.insert(number) {
                    out.eit.push(sec.to_vec());
                }
            }),
            PID_TDT => tdt.feed(p, |sec| match sec[0] {
                TABLE_TOT if out.tot.is_none() => out.tot = Some(sec.to_vec()),
                TABLE_TDT if out.tdt.is_none() => out.tdt = Some(sec.to_vec()),
                _ => {}
            }),
            _ => {}
        }
        if out.eit.len() >= 2 && out.tot.is_some() {
            break;
        }
    }
    Ok(out)
}

/// One elementary stream as it is to be described in the rebuilt map.
#[derive(Debug, Clone)]
pub struct GraftStream {
    /// The PID it was written on, which is the PID it arrived on -- the
    /// muxer takes an explicit stream id and honours it.
    pub pid: u16,
    /// Whether the stream in the output is still the stream that was
    /// described. A track that was downmixed is not: its audio component
    /// descriptor names a channel arrangement the file no longer contains,
    /// so only the identity of the stream survives and the description of
    /// its contents is dropped.
    pub faithful: bool,
}

/// A stretch of the output, and what was being broadcast where it came from.
#[derive(Debug, Clone)]
pub struct GraftRange {
    /// Where this range begins on the output timeline, in seconds.
    pub start: f64,
    /// The tables that were in force at the source point it opens on.
    pub snapshot: Snapshot,
}

/// Everything the pass needs to put the recording's own tables back.
pub struct Graft<'a> {
    pub service: &'a Service,
    pub streams: Vec<GraftStream>,
    /// The PID the clock is expected on, which is the video's. Only used
    /// when the finished file's own map does not say -- it normally does.
    pub pcr_pid: u16,
    pub ranges: Vec<GraftRange>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Program map sections written in place of the muxer's.
    pub pmt: usize,
    /// Service description sections written in place of the muxer's.
    pub sdt: usize,
    /// Event information sections injected.
    pub eit: usize,
    /// Time and date sections injected.
    pub tot: usize,
}

/// How often each table is repeated through the output.
///
/// Broadcast practice, near enough: event information about every two
/// seconds, time about every five. A player that joins the file part way
/// through -- which is what seeking is -- has to wait one interval before it
/// can say what it is playing, so these are short on purpose.
const EIT_PERIOD: f64 = 2.0;
const TOT_PERIOD: f64 = 5.0;

/// Wrap a section into transport packets on one PID.
fn packetize(pid: u16, section: &[u8], cc: &mut u8, out: &mut Vec<u8>) {
    let mut first = true;
    let mut rest = section;
    while first || !rest.is_empty() {
        let mut p = Vec::with_capacity(PACKET);
        p.push(0x47);
        let hi = ((pid >> 8) as u8) & 0x1F | if first { 0x40 } else { 0x00 };
        p.push(hi);
        p.push(pid as u8);
        p.push(0x10 | (*cc & 0x0F));
        *cc = cc.wrapping_add(1) & 0x0F;
        if first {
            p.push(0x00); // pointer field: the section starts here
        }
        let room = PACKET - p.len();
        let take = rest.len().min(room);
        p.extend_from_slice(&rest[..take]);
        rest = &rest[take..];
        p.resize(PACKET, 0xFF);
        out.extend_from_slice(&p);
        first = false;
    }
}

/// Build the program map the recording had, for the streams that were kept.
fn build_pmt(g: &Graft, pcr_pid: u16) -> Vec<u8> {
    let s = g.service;
    let mut sec = vec![
        TABLE_PMT,
        0xB0, // syntax indicator, then the length, filled in below
        0x00,
        (s.service_id >> 8) as u8,
        s.service_id as u8,
        0xC1, // version 0, current
        0x00, // section number
        0x00, // last section number
        0xE0 | ((pcr_pid >> 8) as u8 & 0x1F),
        pcr_pid as u8,
        0xF0 | ((s.program_info.len() >> 8) as u8 & 0x0F),
        s.program_info.len() as u8,
    ];
    sec.extend_from_slice(&s.program_info);
    for gs in &g.streams {
        // A stream the source's own map did not describe is written the way
        // the muxer would have: the type it was given, and nothing said
        // about it. Nothing here can invent a descriptor.
        let (stream_type, desc) = match s.stream(gs.pid) {
            Some(es) if gs.faithful => (es.stream_type, es.descriptors.clone()),
            // Only the stream's identity survives; see `GraftStream`.
            Some(es) => (
                es.stream_type,
                match descriptor(&es.descriptors, 0x52) {
                    Some(body) => vec![0x52, body.len() as u8, body[0]],
                    None => Vec::new(),
                },
            ),
            None => continue,
        };
        sec.push(stream_type);
        sec.push(0xE0 | ((gs.pid >> 8) as u8 & 0x1F));
        sec.push(gs.pid as u8);
        sec.push(0xF0 | ((desc.len() >> 8) as u8 & 0x0F));
        sec.push(desc.len() as u8);
        sec.extend_from_slice(&desc);
    }
    finish_section(&mut sec);
    sec
}

/// Move a time and date table's clock on by `seconds`.
///
/// The table says what time it is, and the copy taken at a range's opening
/// would otherwise say so for the length of the range. Broadcast time is
/// carried as a modified Julian day and three bytes of binary-coded decimal,
/// so moving it is arithmetic on the day and the clock separately.
fn advance_time(section: &[u8], seconds: f64) -> Option<Vec<u8>> {
    let time = section.get(3..8)?;
    let mjd = ((time[0] as i64) << 8) | time[1] as i64;
    let bcd = |b: u8| (b >> 4) as i64 * 10 + (b & 0x0F) as i64;
    let of_day = bcd(time[2]) * 3600 + bcd(time[3]) * 60 + bcd(time[4]);
    let mut total = of_day + seconds.round() as i64;
    let mut mjd = mjd;
    while total >= 86_400 {
        total -= 86_400;
        mjd += 1;
    }
    let to_bcd = |v: i64| ((v / 10) << 4) as u8 | (v % 10) as u8;
    let mut out = section.to_vec();
    out[3] = (mjd >> 8) as u8;
    out[4] = mjd as u8;
    out[5] = to_bcd(total / 3600);
    out[6] = to_bcd(total / 60 % 60);
    out[7] = to_bcd(total % 60);
    // Time and date carries no CRC; the offset table does, despite saying it
    // has no syntax to check, and it has to be recomputed over what changed.
    if section[0] == TABLE_TOT && out.len() >= 4 {
        let body = out.len() - 4;
        let crc = crc32(&out[..body]);
        out[body..].copy_from_slice(&crc.to_be_bytes());
    }
    Some(out)
}

/// Where the muxer actually put the map, and which PID carries the clock.
///
/// Asked of the finished file rather than assumed. The muxer is *told* both
/// -- the PMT PID the recording used, and a stream id per stream -- but it
/// declines a PMT PID that would collide with the run it numbers streams
/// from, and picks its own. Writing the rebuilt map onto the PID it was
/// asked to use would then leave the muxer's own map in place beside it, on
/// a PID this never wrote to, and the output would carry two.
fn output_layout(path: &str) -> Result<(u16, u16)> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 4 << 20];
    let n = read_fully(&mut f, &mut buf)?;
    buf.truncate(n);
    let base = find_sync(&buf).ok_or_else(|| anyhow!("{path} is not a transport stream"))?;

    let mut pat = SectionReader::default();
    let mut pmt = SectionReader::default();
    let mut pmt_pid = 0u16;
    let mut service_id = 0u16;
    let mut pcr_pid = 0u16;
    let mut at = base;
    while at + PACKET <= buf.len() {
        let p = &buf[at..at + PACKET];
        at += PACKET;
        if p[0] != 0x47 {
            break;
        }
        match pid_of(p) {
            PID_PAT if pmt_pid == 0 => pat.feed(p, |sec| {
                if sec[0] != TABLE_PAT || sec.len() < 12 {
                    return;
                }
                let mut i = 8;
                while i + 4 <= sec.len() - 4 {
                    let number = ((sec[i] as u16) << 8) | sec[i + 1] as u16;
                    if number != 0 {
                        service_id = number;
                        pmt_pid = (((sec[i + 2] & 0x1F) as u16) << 8) | sec[i + 3] as u16;
                        break;
                    }
                    i += 4;
                }
            }),
            pid if pid != 0 && pid == pmt_pid && pcr_pid == 0 => pmt.feed(p, |sec| {
                if sec[0] == TABLE_PMT && sec.len() >= 16 {
                    pcr_pid = (((sec[8] & 0x1F) as u16) << 8) | sec[9] as u16;
                }
            }),
            _ => {}
        }
        if pmt_pid != 0 && pcr_pid != 0 {
            break;
        }
    }
    if pmt_pid == 0 {
        bail!("{path} carries no program association table");
    }
    Ok((pmt_pid, pcr_pid))
}

/// Put the recording's own tables back into a finished cut.
///
/// One pass, packet by packet. The map and the service description are
/// written over the muxer's, at the same cadence the muxer chose; event
/// information and the clock are inserted, since there is nothing in the
/// output to write over. Everything else is passed through untouched --
/// which is the point: the pictures and the sound this walks past are the
/// ones that were copied bit for bit, and they stay that way.
pub fn graft(output: &str, g: &Graft) -> Result<Stats> {
    if g.ranges.is_empty() {
        bail!("nothing to graft onto: no ranges");
    }
    let (out_pmt_pid, out_pcr_pid) = output_layout(output)?;
    let pcr_pid = if out_pcr_pid > 0 { out_pcr_pid } else { g.pcr_pid };
    // Beside the output rather than in a temporary directory: the two are
    // renamed into each other at the end, and a rename across filesystems is
    // a copy of the whole file.
    let temp = format!("{output}.si");
    let mut stats = Stats::default();
    let outcome = (|| -> Result<()> {
        let mut src = BufReader::with_capacity(1 << 20, std::fs::File::open(output)?);
        let mut dst = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&temp)?);

        let pmt_section = build_pmt(g, pcr_pid);
        let sdt_section = g.service.sdt.clone();
        let mut cc: HashMap<u16, u8> = HashMap::new();
        let mut scratch: Vec<u8> = Vec::with_capacity(PACKET * 4);

        // The output's own clock, which is what an injected section is
        // placed against. It starts wherever the muxer started it.
        let mut first_pcr: Option<i64> = None;
        let mut now = 0.0f64;
        let mut next_eit = 0.0f64;
        let mut next_tot = 0.0f64;
        // Which range the clock is inside, so a cut spanning two programmes
        // describes each of them over its own stretch.
        let mut range = 0usize;

        let mut packet = [0u8; PACKET];
        loop {
            match src.read_exact(&mut packet) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e).context("reading the cut back"),
            }
            if packet[0] != 0x47 {
                bail!("{output} is not packet aligned; the cut was not written as a transport stream");
            }
            let pid = pid_of(&packet);
            if pid == pcr_pid {
                if let Some(pcr) = pcr_of(&packet) {
                    let base = *first_pcr.get_or_insert(pcr);
                    // The clock is 33 bits and wraps every 26.5 hours. A
                    // recording that long is not the case worth handling
                    // wrongly, so a step backwards is read as the wrap.
                    let mut ticks = pcr - base;
                    if ticks < 0 {
                        ticks += 1 << 33;
                    }
                    now = ticks as f64 / 90_000.0;
                }
            }
            while range + 1 < g.ranges.len() && now >= g.ranges[range + 1].start {
                range += 1;
                // Each range opens with what was on where it came from,
                // rather than waiting out the interval.
                next_eit = now;
                next_tot = now;
            }

            match pid {
                p if p == out_pmt_pid => {
                    let c = cc.entry(p).or_default();
                    scratch.clear();
                    packetize(p, &pmt_section, c, &mut scratch);
                    dst.write_all(&scratch)?;
                    stats.pmt += 1;
                    continue;
                }
                PID_SDT if sdt_section.is_some() => {
                    let c = cc.entry(PID_SDT).or_default();
                    scratch.clear();
                    packetize(PID_SDT, sdt_section.as_ref().unwrap(), c, &mut scratch);
                    dst.write_all(&scratch)?;
                    stats.sdt += 1;
                    continue;
                }
                _ => {}
            }
            dst.write_all(&packet)?;

            let snap = &g.ranges[range].snapshot;
            if now >= next_eit && !snap.eit.is_empty() {
                next_eit = now + EIT_PERIOD;
                let c = cc.entry(PID_EIT).or_default();
                scratch.clear();
                for sec in &snap.eit {
                    packetize(PID_EIT, sec, c, &mut scratch);
                }
                dst.write_all(&scratch)?;
                stats.eit += 1;
            }
            if now >= next_tot {
                if let Some(sec) = snap.tot.as_ref().or(snap.tdt.as_ref()) {
                    next_tot = now + TOT_PERIOD;
                    let elapsed = now - g.ranges[range].start;
                    if let Some(moved) = advance_time(sec, elapsed) {
                        let c = cc.entry(PID_TDT).or_default();
                        scratch.clear();
                        packetize(PID_TDT, &moved, c, &mut scratch);
                        dst.write_all(&scratch)?;
                        stats.tot += 1;
                    }
                }
            }
        }
        dst.flush()?;
        Ok(())
    })();
    // A half-written copy is worse than none: it is the size of the cut and
    // it looks like one. The cut itself is untouched either way -- this pass
    // only ever reads it.
    if let Err(e) = outcome {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&temp, output) {
        let _ = std::fs::remove_file(&temp);
        return Err(anyhow::Error::new(e).context(format!("replacing {output}")));
    }
    Ok(stats)
}
