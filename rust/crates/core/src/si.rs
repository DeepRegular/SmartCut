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
//! What is put back is trimmed to what the file turned out to hold. The
//! recording describes a data broadcast, a superimposed crawl and every
//! sound track it was sent with; a cut carries the pictures, the sound and
//! the subtitles. A descriptor that names one of the streams left behind
//! would have the output announce something a player can then go looking
//! for and never find, so those come out -- of the map and of the programme
//! description alike.
//!
//! The pass is byte-level on purpose. Sections are not timed -- they carry
//! no PTS and are meant to be repeated -- so nothing here has to be spliced,
//! only placed and given a continuity counter that follows on.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};

/// One transport packet. The whole format is built on this being fixed.
pub const PACKET: usize = 188;

/// What one packet occupies on a Blu-ray. BDAV writes the same 188 bytes
/// behind four more that hold the time the packet arrived, so the packets are
/// where they always were and the spacing between them is not. Only the
/// reading side ever meets this: what is written out is a transport stream.
pub const M2TS_PACKET: usize = 192;

const PID_PAT: u16 = 0x0000;
const PID_SDT: u16 = 0x0011;
const PID_EIT: u16 = 0x0012;
const PID_TDT: u16 = 0x0014;
/// Where a partial transport stream says what it is. Reserved for exactly
/// this and nothing else, which is why a recording never carries it.
const PID_SIT: u16 = 0x001F;

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
/// Selection information: everything a partial transport stream says about
/// itself, in the one table that replaces the rest.
const TABLE_SIT: u8 = 0x7F;
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

/// Which account of itself a finished cut carries.
///
/// A recording is a slice out of a multiplex, and there are two established
/// ways to write one down. A partial transport stream -- what a recorder
/// writes, what DVB describes in EN 300 468 Annex C and ARIB in TR-B15 --
/// says everything in one table, the SIT, and carries none of the tables
/// that describe a live multiplex. Keeping the broadcast's own SDT, EIT and
/// TOT instead says the same things in the shape they arrived in, which is
/// what the tools built around Japanese recordings read.
///
/// The first is the standard answer to "what is a recording", so it is the
/// default. The second is kept because it is what most software downstream
/// of this one actually looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tables {
    /// The muxer's own, which describe the streams and nothing else.
    Muxer,
    /// The recording's own SDT, EIT and TOT, put back where they belong.
    Broadcast,
    /// One selection information table, the way a recording is written down.
    #[default]
    Partial,
}

impl Tables {
    pub fn as_str(self) -> &'static str {
        match self {
            Tables::Muxer => "muxer",
            Tables::Broadcast => "broadcast",
            Tables::Partial => "partial",
        }
    }
}

/// Whether a byte offset holds a packet, checked the way a demuxer checks.
///
/// One sync byte proves nothing -- 0x47 is a perfectly ordinary payload byte.
/// A run of them at exactly the packet spacing does.
fn sync_at(buf: &[u8], at: usize, stride: usize) -> bool {
    (0..5).all(|k| buf.get(at + k * stride) == Some(&0x47))
}

/// Find the first packet boundary in `buf`, for a stream already known to be
/// spaced `stride` apart.
fn find_sync(buf: &[u8], stride: usize) -> Option<usize> {
    (0..buf.len().min(stride * 8)).find(|&i| sync_at(buf, i, stride))
}

/// Where the packets start and how far apart they are.
///
/// Five in a row at the same spacing is what settles it. A recording is
/// either a transport stream, packet after packet, or the same packets with
/// four bytes of arrival time in front of each -- which is what a Blu-ray
/// recording is, and what a `.m2ts` taken off one still is. Nothing else
/// gets this far: libavformat has already agreed the file is MPEG-TS.
fn framing(buf: &[u8]) -> Option<(usize, usize)> {
    [PACKET, M2TS_PACKET]
        .into_iter()
        .find_map(|stride| find_sync(buf, stride).map(|at| (at, stride)))
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

/// The component a descriptor is about, when it is about one.
///
/// ARIB names streams by a one-byte tag rather than by PID, and three of the
/// descriptors a programme carries point at a stream that way: the component
/// and audio component descriptors say which stream they describe, and the
/// data content descriptor says which stream a viewer would enter the data
/// broadcast on. All three put the tag in the same place, third in the body.
///
/// Nothing else here refers to a stream. The event group descriptor points
/// at other *events*, and the series descriptor at other broadcasts; those
/// travel whole.
fn component_reference(tag: u8, body: &[u8]) -> Option<u8> {
    match tag {
        0x50 | 0xC4 | 0xC7 => body.get(2).copied(),
        _ => None,
    }
}

/// Which component tags the recording described, and which the cut carries.
///
/// The two are compared rather than the second used alone, because a
/// recording that names no components at all -- one that has been through a
/// muxer already, where the stream identifier descriptors did not survive --
/// would otherwise look like a cut that carries nothing, and every
/// descriptor naming a component would be thrown away on no evidence.
struct Components {
    described: HashSet<u8>,
    carried: HashSet<u8>,
}

impl Components {
    fn of(g: &Graft) -> Self {
        let described = g.service.streams.iter().filter_map(|s| s.component_tag()).collect();
        let carried = g
            .streams
            .iter()
            // A downmixed track is not the track that was described; see
            // `GraftStream`. Its audio component descriptor names a channel
            // arrangement the output no longer has, so the tag counts as
            // uncarried and the descriptor goes with it.
            .filter(|gs| gs.faithful)
            .filter_map(|gs| g.service.stream(gs.pid).and_then(|es| es.component_tag()))
            .collect();
        Components { described, carried }
    }

    /// Whether a descriptor about this component still describes the output.
    ///
    /// Only a component the recording itself described can be judged
    /// missing. One that appears nowhere in the recording's map is a
    /// disagreement between the broadcaster's own tables, and not something
    /// to settle here.
    fn still_true(&self, tag: u8) -> bool {
        self.carried.contains(&tag) || !self.described.contains(&tag)
    }
}

/// Copy a descriptor loop, leaving out anything that names a component the
/// cut does not carry.
///
/// A cut is a smaller file than the recording in more than length: the data
/// broadcast is gone, the superimposed crawl may be, a track the editor
/// switched off certainly is. The programme description that arrives with
/// the recording still speaks of all of them, and copied across whole it
/// would have the output announce an entry point into a data broadcast that
/// is not in the file. This is the same reasoning as [`DROP_DESCRIPTORS`],
/// applied to what the cut turned out to contain rather than to a fixed tag.
fn keep_carried(loop_bytes: &[u8], components: &Components) -> Vec<u8> {
    let mut out = Vec::with_capacity(loop_bytes.len());
    let mut i = 0;
    while i + 2 <= loop_bytes.len() {
        let tag = loop_bytes[i];
        let len = loop_bytes[i + 1] as usize;
        let Some(whole) = loop_bytes.get(i..i + 2 + len) else { break };
        let keep = match component_reference(tag, &whole[2..]) {
            Some(component) => components.still_true(component),
            None => true,
        };
        if keep {
            out.extend_from_slice(whole);
        }
        i += 2 + len;
    }
    out
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
pub fn read_service(input: &crate::input::Input) -> Result<Service> {
    const WINDOW: usize = 8 << 20;
    let mut f = input.open()?;
    let mut buf = vec![0u8; WINDOW];
    let n = read_fully(&mut f, &mut buf)?;
    buf.truncate(n);
    let path = &input.spec;
    let (base, stride) =
        framing(&buf).ok_or_else(|| anyhow!("{path} is not a transport stream"))?;

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
        at += stride;
        if p[0] != 0x47 {
            // A recording can be cut short mid-packet or carry a bad read;
            // re-find the grid rather than giving up on the file.
            match find_sync(&buf[at..], stride) {
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

fn read_fully<R: Read>(f: &mut R, buf: &mut [u8]) -> Result<usize> {
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
pub fn snapshot_at(input: &crate::input::Input, pos: i64, service_id: u16) -> Result<Snapshot> {
    // Long enough to hold several repetitions. Event information goes out
    // about every two seconds and time every five, which at broadcast rates
    // is a few megabytes.
    const WINDOW: usize = 24 << 20;
    let mut f = input.open()?;
    if pos > 0 {
        f.seek(SeekFrom::Start(pos as u64))?;
    }
    let mut buf = vec![0u8; WINDOW];
    let n = read_fully(&mut f, &mut buf)?;
    buf.truncate(n);
    let Some((base, stride)) = framing(&buf) else { return Ok(Snapshot::default()) };

    let mut eit = SectionReader::default();
    let mut tdt = SectionReader::default();
    let mut out = Snapshot::default();
    // Present and following is two sections, numbered 0 and 1. Both are
    // wanted and either may be absent.
    let mut seen: HashSet<u8> = HashSet::new();

    let mut at = base;
    while at + PACKET <= buf.len() {
        let p = &buf[at..at + PACKET];
        at += stride;
        if p[0] != 0x47 {
            match find_sync(&buf[at..], stride) {
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
    /// Which account of itself the output is to carry.
    pub tables: Tables,
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
    /// Selection information sections written, in a partial transport stream.
    pub sit: usize,
}

/// How often each table is repeated through the output.
///
/// Broadcast practice, near enough: event information about every two
/// seconds, time about every five. A player that joins the file part way
/// through -- which is what seeking is -- has to wait one interval before it
/// can say what it is playing, so these are short on purpose.
const EIT_PERIOD: f64 = 2.0;
const TOT_PERIOD: f64 = 5.0;
/// How often the muxer writes a service description, and so how often a
/// partial stream's own table goes out in its place. libavformat's default,
/// which this does not change.
const SDT_PERIOD: f64 = 0.5;

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
fn build_pmt(g: &Graft, pcr_pid: u16, components: &Components) -> Vec<u8> {
    let s = g.service;
    // The programme-level loop can name components too -- a data content
    // descriptor sits there as readily as in an event. The per-stream loops
    // below are not filtered this way: a stream's own descriptors are about
    // that stream, and it is in the output or it is not written at all.
    let program_info = keep_carried(&s.program_info, components);
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
        0xF0 | ((program_info.len() >> 8) as u8 & 0x0F),
        program_info.len() as u8,
    ];
    sec.extend_from_slice(&program_info);
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

/// Rewrite an event information section for the streams the cut carries.
///
/// The section arrives from the recording and is written back as it came,
/// except for the descriptors that name a stream the cut left behind. Those
/// are taken out and the section is closed again -- a new length, a new CRC.
///
/// Only the present event is trimmed this way. Present and following arrive
/// as two sections, numbered 0 and 1, and only the first is about this file:
/// the second is a note about what came next on the air, a programme whose
/// streams were never going to be in here. Judging its description against
/// what this file carries would answer a question nobody asked -- and would
/// throw away the one true thing it says, which is what was coming.
///
/// Returns `None` when nothing had to change, which is the usual case and
/// the one where the recording's own bytes should travel untouched, and also
/// when the section does not parse as event information, where saying
/// nothing is better than writing a guess.
fn prune_events(section: &[u8], components: &Components) -> Option<Vec<u8>> {
    if *section.get(6)? != 0 {
        return None;
    }
    // Everything but the CRC, which is recomputed over whatever is left.
    let body = section.get(..section.len().checked_sub(4)?)?;
    // Table id, length, service, version, section numbers, transport stream,
    // network, and the two that say where this section sits in the schedule.
    let mut out = body.get(..14)?.to_vec();
    let mut at = 14;
    let mut changed = false;
    while at + 12 <= body.len() {
        let len = (((body[at + 10] & 0x0F) as usize) << 8) | body[at + 11] as usize;
        let descriptors = body.get(at + 12..at + 12 + len)?;
        let kept = keep_carried(descriptors, components);
        changed |= kept.len() != descriptors.len();
        // Event id, start time, duration and running status are untouched:
        // which programme this is and when it went out is a fact about the
        // broadcast, not about what was kept of it.
        out.extend_from_slice(&body[at..at + 10]);
        out.push((body[at + 10] & 0xF0) | ((kept.len() >> 8) as u8 & 0x0F));
        out.push(kept.len() as u8);
        out.extend_from_slice(&kept);
        at += 12 + len;
    }
    // A trailing byte means the walk and the section disagree about where
    // the events end, so the walk was wrong about all of it.
    if at != body.len() || !changed {
        return None;
    }
    finish_section(&mut out);
    Some(out)
}

/// The largest a section may be, counting the three bytes before the length
/// field and the four of CRC after the body.
const SECTION_MAX: usize = 4096;

/// What the programme on now says about itself, ready to go into a SIT.
struct Present {
    /// The version of the section it was read from, which is what a partial
    /// transport stream calls the event version.
    version: u8,
    /// Modified Julian day and three bytes of binary-coded decimal.
    start: [u8; 5],
    /// Binary-coded decimal, hours to seconds.
    duration: [u8; 3],
    /// The event's own descriptor loop, trimmed to the streams that are here.
    descriptors: Vec<u8>,
}

/// Read the present event out of a snapshot.
///
/// Present and following arrive as two sections and only the first describes
/// this file. The following one names a programme that is not here, and a
/// partial transport stream has nowhere to put it: the SIT describes what
/// the file *is*.
fn present_event(snapshot: &Snapshot, components: &Components) -> Option<Present> {
    let section = snapshot.eit.iter().find(|s| s.len() > 26 && s[6] == 0)?;
    let body = &section[..section.len() - 4];
    let event = body.get(14..)?;
    let len = (((event[10] & 0x0F) as usize) << 8) | event[11] as usize;
    let descriptors = keep_carried(event.get(12..12 + len)?, components);
    Some(Present {
        version: (section[5] >> 1) & 0x1F,
        start: event[2..7].try_into().ok()?,
        duration: event[7..10].try_into().ok()?,
        descriptors,
    })
}

/// The service descriptor, dug out of the recording's own service description.
///
/// The name arrives as ARIB text and leaves as ARIB text; this only has to
/// find where it starts and how far it runs.
fn service_descriptor(sdt: &[u8]) -> Option<&[u8]> {
    // Section header, then the one service: its id, the event flags, and the
    // running status and length that open its descriptor loop.
    let service = sdt.get(11..sdt.len().checked_sub(4)?)?;
    let len = (((service[3] & 0x0F) as usize) << 8) | service[4] as usize;
    let loop_bytes = service.get(5..5 + len)?;
    let mut i = 0;
    while i + 2 <= loop_bytes.len() {
        let whole = loop_bytes.get(i..i + 2 + loop_bytes[i + 1] as usize)?;
        if whole[0] == 0x48 {
            return Some(whole);
        }
        i += whole.len();
    }
    None
}

/// How fast the partial transport stream runs, in the 400 bit/s the
/// descriptor counts in.
fn partial_stream_descriptor(peak_rate: u32) -> Vec<u8> {
    let peak = peak_rate.min(0x3F_FFFF);
    vec![
        0x63,
        0x08,
        0xC0 | ((peak >> 16) as u8 & 0x3F),
        (peak >> 8) as u8,
        peak as u8,
        // The smoothing rate and buffer are what a device would need to feed
        // this back into a decoder at a steady rate. Nothing here measures
        // either, and all ones is how the descriptor says so.
        0xFF,
        0xFF,
        0xFF,
        0xFF,
        0xFF,
    ]
}

/// Which network the recording came off, when the network id says plainly.
///
/// The original network id is allocated per country and per medium, so the
/// three that matter in Japan can be read straight off it. An id outside
/// them is left undescribed rather than guessed at.
fn network_descriptor(original_network_id: u16) -> Option<Vec<u8>> {
    let medium = match original_network_id {
        0x0004 => b"BS",
        0x0006 | 0x0007 => b"CS",
        0x7880..=0x7FEF => b"TB",
        _ => return None,
    };
    let mut d = vec![0xC2, 0x07, b'J', b'P', b'N'];
    d.extend_from_slice(medium);
    d.extend_from_slice(&original_network_id.to_be_bytes());
    Some(d)
}

/// When the programme went out and how long it ran.
///
/// This is where a partial transport stream keeps what an event information
/// table would have said, and it is the reason the times in it are the
/// broadcast's own rather than the file's: the descriptor is about the event,
/// not about how much of it was kept.
fn partial_time_descriptor(present: &Present) -> Vec<u8> {
    let mut d = vec![0xC3, 0x0D, present.version];
    d.extend_from_slice(&present.start);
    d.extend_from_slice(&present.duration);
    // No offset from the time given, so the offset itself is zero and the
    // flag that would say to read it is clear.
    d.extend_from_slice(&[0x00, 0x00, 0x00]);
    // Reserved bits set, then three flags: whether to read the offset above,
    // whether the event's other descriptors follow in this loop, and whether
    // the time is already the broadcaster's local one. The offset is zero so
    // the first is clear; the descriptors do follow, so the second is set.
    d.push(0b1111_1000 | 0b010);
    d
}

/// Build the one table a partial transport stream carries.
///
/// Everything a player would otherwise read from four tables is here: which
/// service this is and what it is called, which programme, when it went out
/// and for how long, and what the broadcaster said about it. The bytes are
/// the recording's own wherever there were any -- only the frame around them
/// is written here.
fn build_sit(
    service: &Service,
    present: Option<&Present>,
    version: u8,
    peak_rate: u32,
) -> Vec<u8> {
    let mut transmission = partial_stream_descriptor(peak_rate);
    if let Some(d) = network_descriptor(service.original_network_id) {
        transmission.extend_from_slice(&d);
    }

    let mut described = Vec::new();
    if let Some(p) = present {
        described.extend_from_slice(&partial_time_descriptor(p));
    }
    if let Some(d) = service.sdt.as_deref().and_then(service_descriptor) {
        described.extend_from_slice(d);
    }
    if let Some(p) = present {
        described.extend_from_slice(&p.descriptors);
    }

    let mut sec = vec![
        TABLE_SIT,
        0xF0, // syntax indicator and the reserved bits, then the length
        0x00,
        0xFF, // reserved
        0xFF,
        0xC0 | ((version & 0x1F) << 1) | 0x01, // current
        0x00,                                  // section number
        0x00,                                  // last section number
        0xF0 | ((transmission.len() >> 8) as u8 & 0x0F),
        transmission.len() as u8,
    ];
    sec.extend_from_slice(&transmission);

    // A section has a ceiling and a long programme description can reach it:
    // the extended event descriptors alone ran to a kilobyte on the recording
    // this was written against. What does not fit is dropped whole
    // descriptors at a time, from the end, rather than truncated -- the
    // service and the times come first for that reason.
    let room = SECTION_MAX - sec.len() - 4 - 4;
    let mut kept = 0usize;
    let mut i = 0;
    while i + 2 <= described.len() {
        let len = 2 + described[i + 1] as usize;
        if kept + len > room {
            break;
        }
        kept += len;
        i += len;
    }
    sec.extend_from_slice(&service.service_id.to_be_bytes());
    // Running status undefined: a recording is not on the air.
    sec.push(0x80 | ((kept >> 8) as u8 & 0x0F));
    sec.push(kept as u8);
    sec.extend_from_slice(&described[..kept]);
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
    let base = find_sync(&buf, PACKET).ok_or_else(|| anyhow!("{path} is not a transport stream"))?;

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

/// How fast the finished cut runs at its fastest, in the 400 bit/s a partial
/// transport stream descriptor counts in.
///
/// Measured rather than assumed. The rate a mux comes out at is neither the
/// recording's nor a constant, and this is the one number in the table that
/// cannot be copied from anywhere -- the recording never described itself as
/// a partial stream, because it was not one.
///
/// The window is a second because that is the shortest span a rate is
/// meaningful over: measured tighter, one large picture reads as a burst the
/// file never sustains, and the descriptor would name a rate no device needs
/// to provide. `added` is what the tables written here will themselves take
/// up, since they are not in the file being measured yet.
///
/// This is a whole extra pass over the output, which is why it is only asked
/// for when a partial stream is being written.
fn peak_rate(path: &str, pcr_pid: u16, added: f64) -> Result<u32> {
    let mut src = BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
    let mut packet = [0u8; PACKET];
    let mut at: u64 = 0;
    let mut base: Option<i64> = None;
    let mut window: std::collections::VecDeque<(u64, f64)> = std::collections::VecDeque::new();
    let mut peak = 0f64;
    let mut last = 0f64;
    loop {
        match src.read_exact(&mut packet) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e).context("measuring the rate of the cut"),
        }
        at += PACKET as u64;
        if pid_of(&packet) != pcr_pid {
            continue;
        }
        let Some(pcr) = pcr_of(&packet) else { continue };
        let start = *base.get_or_insert(pcr);
        let mut ticks = pcr - start;
        if ticks < 0 {
            ticks += 1 << 33;
        }
        let now = ticks as f64 / 90_000.0;
        last = now;
        window.push_back((at, now));
        while window.front().is_some_and(|&(_, t)| now - t > 1.0) {
            window.pop_front();
        }
        if let Some(&(from, t)) = window.front() {
            // Under half a second is not a window, it is two clocks close
            // together; the rate between them says nothing.
            if now - t >= 0.5 {
                peak = peak.max((at - from) as f64 * 8.0 / (now - t));
            }
        }
    }
    // A file too short to hold a window still has an average, and an average
    // is a truer floor for the peak than zero.
    if last > 0.0 {
        peak = peak.max(at as f64 * 8.0 / last);
    }
    Ok((((peak + added) / 400.0).ceil() as u64).min(0x3F_FFFF) as u32)
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

        let components = Components::of(g);
        let pmt_section = build_pmt(g, pcr_pid, &components);
        let sdt_section = g.service.sdt.clone();
        // One selection information table per range, so a cut spanning two
        // programmes names each over its own stretch -- the same reason the
        // event information is read per range. A range whose table comes out
        // identical to the one before keeps its version number: a version
        // that changes when nothing has says a player should re-read a table
        // it already has.
        let sit = if g.tables == Tables::Partial {
            // Measured before the pass, against the file as the muxer left
            // it, plus what these tables will add to it.
            // The largest a section can be, since which range holds the
            // largest table is not known before the rate they all carry is.
            let biggest = SECTION_MAX.div_ceil(PACKET - 5) as f64;
            let rate = peak_rate(output, pcr_pid, biggest * PACKET as f64 * 8.0 / SDT_PERIOD)?;
            let mut sections: Vec<Vec<u8>> = Vec::with_capacity(g.ranges.len());
            let mut version = 0u8;
            for r in &g.ranges {
                let present = present_event(&r.snapshot, &components);
                let probe = build_sit(g.service, present.as_ref(), version, rate);
                if sections.last().is_some_and(|last| *last != probe) {
                    version = version.wrapping_add(1) & 0x1F;
                    sections.push(build_sit(g.service, present.as_ref(), version, rate));
                } else {
                    sections.push(probe);
                }
            }
            sections
        } else {
            Vec::new()
        };
        // Done once per range rather than at every injection: the same two
        // sections go out every couple of seconds for the length of the
        // range, and what has to come out of them does not change within it.
        let eit: Vec<Vec<Vec<u8>>> = g
            .ranges
            .iter()
            .map(|r| {
                r.snapshot
                    .eit
                    .iter()
                    .map(|sec| prune_events(sec, &components).unwrap_or_else(|| sec.clone()))
                    .collect()
            })
            .collect();
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
                // The muxer's own service description is where a partial
                // stream's table goes too. It arrives at the cadence a
                // service description is written at, which is the cadence
                // either table wants, and taking its place is what leaves
                // PID 0x11 out of the output altogether.
                PID_SDT if g.tables == Tables::Partial => {
                    let c = cc.entry(PID_SIT).or_default();
                    scratch.clear();
                    packetize(PID_SIT, &sit[range], c, &mut scratch);
                    dst.write_all(&scratch)?;
                    stats.sit += 1;
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

            // A partial stream has said everything it has to say in the one
            // table; the tables below are the ones it exists instead of.
            if g.tables == Tables::Partial {
                continue;
            }
            let snap = &g.ranges[range].snapshot;
            if now >= next_eit && !eit[range].is_empty() {
                next_eit = now + EIT_PERIOD;
                let c = cc.entry(PID_EIT).or_default();
                scratch.clear();
                for sec in &eit[range] {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A component descriptor, which is about one stream and says so third.
    fn component(tag: u8) -> Vec<u8> {
        vec![0x50, 0x06, 0x01, 0xB3, tag, 0x6A, 0x70, 0x6E]
    }

    /// A data content descriptor, whose third byte is the component a viewer
    /// would enter the data broadcast on.
    fn data_content(entry: u8) -> Vec<u8> {
        vec![0xC7, 0x07, 0x00, 0x07, entry, 0x00, 0x6A, 0x70, 0x6E]
    }

    /// A short event descriptor, which is about the programme and not about
    /// any one stream, so it travels whatever happens around it.
    fn short_event() -> Vec<u8> {
        vec![0x4D, 0x06, 0x6A, 0x70, 0x6E, 0x02, 0x41, 0x42]
    }

    /// One present-and-following section carrying one event.
    fn eit(section_number: u8, descriptors: &[Vec<u8>]) -> Vec<u8> {
        let loop_bytes: Vec<u8> = descriptors.concat();
        let mut sec = vec![
            TABLE_EIT_PF_ACTUAL,
            0xF0,
            0x00, // length, filled in by finish_section
            0x00,
            0xB5, // service
            0xC1, // version 0, current
            section_number,
            0x01, // last section number
            0x40,
            0xD1, // transport stream
            0x00,
            0x04, // original network
            0x01, // segment last section number
            TABLE_EIT_PF_ACTUAL,
        ];
        sec.extend_from_slice(&[0x0B, 0x29]); // event id
        sec.extend_from_slice(&[0xEF, 0x58, 0x00, 0x00, 0x00]); // start time
        sec.extend_from_slice(&[0x00, 0x30, 0x00]); // duration
        // Running status 0 and not scrambled, then the loop length.
        sec.push((loop_bytes.len() >> 8) as u8 & 0x0F);
        sec.push(loop_bytes.len() as u8);
        sec.extend_from_slice(&loop_bytes);
        finish_section(&mut sec);
        sec
    }

    /// A recording that sent five components, of which a cut kept three.
    fn broadcast() -> Components {
        Components {
            described: [0x00, 0x10, 0x30, 0x38, 0x40].into_iter().collect(),
            carried: [0x00, 0x10, 0x30].into_iter().collect(),
        }
    }

    /// The descriptor tags a section's one event carries, in order.
    fn tags(sec: &[u8]) -> Vec<u8> {
        let loop_bytes = &sec[26..sec.len() - 4];
        let mut out = Vec::new();
        let mut i = 0;
        while i + 2 <= loop_bytes.len() {
            out.push(loop_bytes[i]);
            i += 2 + loop_bytes[i + 1] as usize;
        }
        out
    }

    #[test]
    fn a_section_about_streams_that_are_all_there_is_left_alone() {
        let sec = eit(0, &[component(0x00), short_event(), data_content(0x30)]);
        assert!(prune_events(&sec, &broadcast()).is_none());
    }

    #[test]
    fn the_description_of_a_dropped_stream_does_not_travel() {
        let sec = eit(0, &[component(0x00), short_event(), data_content(0x40)]);
        let out = prune_events(&sec, &broadcast()).expect("the data broadcast is not in the cut");
        assert_eq!(tags(&out), vec![0x50, 0x4D]);
        // What is left is the recording's own bytes, in the order they came.
        // Up to byte 24 only: which event, when it started and how long it
        // ran. The nibble after that is the loop length, and it is the one
        // thing that has to have changed.
        assert_eq!(&out[14..24], &sec[14..24], "the event itself must not move");
        assert_eq!(out[24] & 0xF0, sec[24] & 0xF0, "nor its running status");
        assert_eq!(&out[26..out.len() - 4], [component(0x00), short_event()].concat());
    }

    #[test]
    fn what_is_left_is_still_a_section() {
        let sec = eit(0, &[component(0x00), short_event(), data_content(0x40)]);
        let out = prune_events(&sec, &broadcast()).unwrap();
        let length = (((out[1] & 0x0F) as usize) << 8) | out[2] as usize;
        assert_eq!(length + 3, out.len(), "the length has to describe what is there");
        let event_loop = (((out[24] & 0x0F) as usize) << 8) | out[25] as usize;
        assert_eq!(event_loop, out.len() - 4 - 26, "and so does the event's own");
        assert_eq!(crc32(&out), 0, "a section is not accepted without its CRC");
    }

    #[test]
    fn the_programme_that_followed_is_not_judged_against_this_file() {
        // Section 1 is the next programme, whose streams were never going to
        // be in here. Naming a component this file lacks is correct of it.
        let sec = eit(1, &[component(0x00), data_content(0x40)]);
        assert!(prune_events(&sec, &broadcast()).is_none());
    }

    #[test]
    fn a_component_the_recording_never_described_is_left_where_it_is() {
        // The broadcaster's own tables disagreeing is not something to settle
        // here: 0x11 is in no map this program read, so nothing is known
        // about whether the cut should have carried it.
        let sec = eit(0, &[component(0x11)]);
        assert!(prune_events(&sec, &broadcast()).is_none());
    }

    #[test]
    fn a_recording_that_named_no_components_loses_nothing() {
        // A source that has been through a muxer already carries no stream
        // identifier descriptors, so nothing can be judged missing.
        let muxed = Components { described: HashSet::new(), carried: HashSet::new() };
        let sec = eit(0, &[component(0x00), data_content(0x40)]);
        assert!(prune_events(&sec, &muxed).is_none());
    }

    #[test]
    fn a_downmixed_track_is_not_the_track_that_was_described() {
        // Carried, but not faithfully: the tag is in the recording's map and
        // out of the cut's, which is the same case as a dropped stream.
        let folded = Components {
            described: [0x00, 0x10].into_iter().collect(),
            carried: [0x00].into_iter().collect(),
        };
        let sec = eit(0, &[component(0x00), vec![0xC4, 0x03, 0x01, 0x03, 0x10]]);
        let out = prune_events(&sec, &folded).expect("the audio is no longer as described");
        assert_eq!(tags(&out), vec![0x50]);
    }

    #[test]
    fn a_section_that_does_not_parse_is_not_rewritten() {
        let mut sec = eit(0, &[component(0x00), data_content(0x40)]);
        // An event loop that runs past the end of the section.
        sec[25] = 0xFF;
        assert!(prune_events(&sec, &broadcast()).is_none());
        assert!(prune_events(&[], &broadcast()).is_none());
        assert!(prune_events(&[TABLE_EIT_PF_ACTUAL, 0xF0, 0x00], &broadcast()).is_none());
    }

    /// A service description carrying one service and the descriptors given.
    fn sdt(descriptors: &[u8]) -> Vec<u8> {
        let mut sec = vec![
            TABLE_SDT_ACTUAL,
            0xF0,
            0x00,
            0x40,
            0xD1, // transport stream
            0xC1,
            0x00,
            0x00,
            0x00,
            0x04, // original network
            0xFF,
        ];
        sec.extend_from_slice(&[0x00, 0xB5]); // service
        sec.push(0xFC); // reserved, and both event information flags
        sec.push(0x80 | ((descriptors.len() >> 8) as u8 & 0x0F));
        sec.push(descriptors.len() as u8);
        sec.extend_from_slice(descriptors);
        finish_section(&mut sec);
        sec
    }

    /// The service descriptor as a broadcast sends it: the type, then the
    /// provider and the name as ARIB text.
    fn service_name() -> Vec<u8> {
        vec![0x48, 0x06, 0x01, 0x02, b'A', b'B', 0x01, b'C']
    }

    fn recording(sdt_section: Option<Vec<u8>>) -> Service {
        Service {
            transport_stream_id: 0x40D1,
            original_network_id: 0x0004,
            service_id: 0x00B5,
            service_type: 0x01,
            pmt_pid: 0x0101,
            pcr_pid: 0x0100,
            program_info: Vec::new(),
            streams: Vec::new(),
            sdt: sdt_section,
        }
    }

    fn snapshot_of(section: Vec<u8>) -> Snapshot {
        Snapshot { eit: vec![section], tot: None, tdt: None }
    }

    /// The descriptor tags a built SIT carries, service loop only.
    fn sit_tags(sec: &[u8]) -> Vec<u8> {
        let til = (((sec[8] & 0x0F) as usize) << 8) | sec[9] as usize;
        let body = &sec[10 + til..sec.len() - 4];
        let len = (((body[2] & 0x0F) as usize) << 8) | body[3] as usize;
        let loop_bytes = &body[4..4 + len];
        let mut out = Vec::new();
        let mut i = 0;
        while i + 2 <= loop_bytes.len() {
            out.push(loop_bytes[i]);
            i += 2 + loop_bytes[i + 1] as usize;
        }
        out
    }

    #[test]
    fn one_table_says_what_four_used_to() {
        let service = recording(Some(sdt(&service_name())));
        let snapshot = snapshot_of(eit(0, &[short_event(), component(0x00), data_content(0x40)]));
        let present = present_event(&snapshot, &broadcast()).expect("a programme is on");
        let sec = build_sit(&service, Some(&present), 0, 46235);

        assert_eq!(sec[0], TABLE_SIT);
        assert_eq!(crc32(&sec), 0, "a section is not accepted without its CRC");
        let length = (((sec[1] & 0x0F) as usize) << 8) | sec[2] as usize;
        assert_eq!(length + 3, sec.len(), "the length has to describe what is there");
        // The times, the name, then the programme's own descriptors -- and
        // not the one naming the data broadcast, which is not in the cut.
        assert_eq!(sit_tags(&sec), vec![0xC3, 0x48, 0x4D, 0x50]);
    }

    #[test]
    fn the_transmission_loop_describes_the_stream_and_the_network() {
        let sec = build_sit(&recording(None), None, 0, 46235);
        let til = (((sec[8] & 0x0F) as usize) << 8) | sec[9] as usize;
        let loop_bytes = &sec[10..10 + til];
        assert_eq!(loop_bytes[0], 0x63, "a partial stream says how fast it runs");
        let peak = (((loop_bytes[2] & 0x3F) as u32) << 16)
            | ((loop_bytes[3] as u32) << 8)
            | loop_bytes[4] as u32;
        assert_eq!(peak, 46235);
        // The partial stream descriptor is ten bytes, then the network's:
        // read the medium and the original network id off the end of it.
        assert_eq!(loop_bytes[10], 0xC2);
        assert_eq!(&loop_bytes[12..19], &[b'J', b'P', b'N', b'B', b'S', 0x00, 0x04]);
    }

    #[test]
    fn a_network_nobody_here_knows_is_left_undescribed() {
        assert!(network_descriptor(0x2000).is_none());
        assert_eq!(&network_descriptor(0x7FE0).unwrap()[2..7], b"JPNTB");
        assert_eq!(&network_descriptor(0x0006).unwrap()[2..7], b"JPNCS");
        assert_eq!(network_descriptor(0x0004).unwrap()[7..9], [0x00, 0x04]);
    }

    #[test]
    fn the_times_are_the_broadcasts_and_not_the_files() {
        let snapshot = snapshot_of(eit(0, &[short_event()]));
        let present = present_event(&snapshot, &broadcast()).unwrap();
        let d = partial_time_descriptor(&present);
        assert_eq!(d[0], 0xC3);
        assert_eq!(d[1] as usize, d.len() - 2);
        // The event started when it was broadcast and ran for half an hour,
        // whatever was kept of it.
        assert_eq!(&d[3..8], &[0xEF, 0x58, 0x00, 0x00, 0x00]);
        assert_eq!(&d[8..11], &[0x00, 0x30, 0x00]);
        assert_eq!(&d[11..14], &[0x00, 0x00, 0x00], "no offset from that time");
        assert_eq!(d[14] & 0b010, 0b010, "the event's own descriptors follow");
    }

    #[test]
    fn a_programme_too_long_to_fit_is_cut_at_a_descriptor() {
        // Extended event descriptors run to a kilobyte on a real recording;
        // enough of them will not fit in a section however large it is.
        let long: Vec<Vec<u8>> = (0..40)
            .map(|_| {
                let mut d = vec![0x4E, 0xFF];
                d.extend(std::iter::repeat_n(0x41, 0xFF));
                d
            })
            .collect();
        let snapshot = snapshot_of(eit(0, &long));
        let present = present_event(&snapshot, &broadcast()).unwrap();
        let sec = build_sit(&recording(Some(sdt(&service_name()))), Some(&present), 0, 1);
        assert!(sec.len() <= SECTION_MAX, "a section has a ceiling: {}", sec.len());
        assert_eq!(crc32(&sec), 0);
        // Whole descriptors were dropped, not the tail of one, and the two
        // that have to survive are still at the front.
        let tags = sit_tags(&sec);
        assert_eq!(&tags[..2], &[0xC3, 0x48]);
        assert!(tags[2..].iter().all(|&t| t == 0x4E));
        assert!(tags.len() < 42, "something had to go");
    }

    #[test]
    fn a_recording_with_no_service_description_still_gets_a_table() {
        let snapshot = snapshot_of(eit(0, &[short_event()]));
        let present = present_event(&snapshot, &broadcast()).unwrap();
        let sec = build_sit(&recording(None), Some(&present), 3, 1);
        assert_eq!((sec[5] >> 1) & 0x1F, 3, "the version it was asked for");
        assert_eq!(sit_tags(&sec), vec![0xC3, 0x4D]);
    }

    #[test]
    fn the_following_programme_is_not_what_the_file_is() {
        // Only section 0 describes this file, so a snapshot holding just the
        // following event describes nothing here.
        let snapshot = snapshot_of(eit(1, &[short_event()]));
        assert!(present_event(&snapshot, &broadcast()).is_none());
    }

    #[test]
    fn the_service_name_is_found_where_the_broadcast_put_it() {
        let section = sdt(&[service_name(), vec![0xC1, 0x01, 0x84]].concat());
        assert_eq!(service_descriptor(&section), Some(&service_name()[..]));
        assert!(service_descriptor(&sdt(&[])).is_none());
    }
}
