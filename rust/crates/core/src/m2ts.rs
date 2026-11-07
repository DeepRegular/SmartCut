//! The arrival times of a `.m2ts` written on its own.
//!
//! A `.m2ts` is a transport stream with four bytes in front of every packet,
//! and the low thirty bits of those four are when the packet arrived, on the
//! same 27 MHz clock as the stream's clock references. Asked for one,
//! libavformat writes that field from a rate it was never told, and what
//! comes out steps *backwards* on most packets: cut from a broadcast, 776511
//! of 779470 packets, by up to 14 seconds. A player or an editor that
//! meters the stream by its arrival times is fed it out of order.
//!
//! A clip in a disc's folder never kept those times: [`crate::bdav::stamp`]
//! writes every one again at the rate a disc is recorded at, and moves the
//! clock references onto it. So this is for the other `.m2ts`, the one
//! written on its own, and it does less: the clock references and every
//! 188-byte packet stay as they were, and only the four bytes in front
//! change. A packet carrying a clock reference arrives at that reference;
//! the packets between two references are spread evenly across the time
//! between them; the packets before the first and after the last carry on
//! at the rate of the nearest pair. The two copy-permission bits above the
//! time are kept.
//!
//! A stream with no clock reference, or whose references step backwards,
//! has nothing to stamp from. It is left as it was, with a note, rather than
//! failing a cut that is otherwise written.

use anyhow::{Context, Result};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

const SOURCE_PACKET: usize = 192;
const ATS_MASK: u32 = 0x3FFF_FFFF;
/// The clock reference's whole range in 27 MHz ticks: 33 bits of 90 kHz.
const PCR_WRAP: u64 = (1u64 << 33) * 300;

/// Whether `path` is a `.m2ts` written on its own, and not a clip in a
/// disc's folder (`BDAV/STREAM/00001.m2ts`), whose times
/// [`crate::bdav::stamp`] writes.
pub fn standalone(path: &str) -> bool {
    let p = Path::new(path);
    let upper = |q: Option<&Path>| {
        q.and_then(|q| q.file_name())
            .and_then(|n| n.to_str())
            .map(str::to_ascii_uppercase)
    };
    let m2ts = p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("m2ts"));
    let on_a_disc = upper(p.parent()).as_deref() == Some("STREAM")
        && upper(p.parent().and_then(|q| q.parent())).as_deref() == Some("BDAV");
    m2ts && !on_a_disc
}

/// Write the arrival times of a finished standalone `.m2ts` again, from its
/// own clock references. See the module's own account.
///
/// Where the stream has nothing to stamp from, says so and leaves it alone.
/// An error is a file that could not be read or written.
pub fn restamp(path: &str) -> Result<()> {
    match plan(path)? {
        Ok(anchors) => write(path, &anchors),
        Err(why) => {
            crate::say!("note: the arrival times in {path} were left as written: {why}");
            Ok(())
        }
    }
}

/// Every clock reference on the stream's clock PID, as (packet, 27 MHz
/// time) with the wrap taken out, or why there is nothing to stamp from.
fn plan(path: &str) -> Result<std::result::Result<Vec<(u64, u64)>, String>> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("reading {path}"))?
        .len();
    if size % SOURCE_PACKET as u64 != 0 {
        return Ok(Err("it is not a whole number of 192-byte packets".into()));
    }
    let mut src = BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
    let mut frame = [0u8; SOURCE_PACKET];
    let mut anchors: Vec<(u64, u64)> = Vec::new();
    let mut clock_pid: Option<u16> = None;
    let (mut base, mut was) = (0u64, None::<u64>);
    for i in 0..size / SOURCE_PACKET as u64 {
        src.read_exact(&mut frame)
            .with_context(|| format!("reading {path}"))?;
        let p = &frame[4..];
        if p[0] != 0x47 {
            return Ok(Err(format!("packet {i} is out of sync")));
        }
        let Some(pcr) = pcr_of(p) else { continue };
        let pid = (u16::from(p[1] & 0x1F) << 8) | u16::from(p[2]);
        // One programme, one clock: the first PID that carries a reference
        // is the one the map names, and a reference on any other belongs to
        // nothing a player is timing by.
        if *clock_pid.get_or_insert(pid) != pid {
            continue;
        }
        if let Some(before) = was {
            // Forward across the wrap is the clock going on; any other step
            // back is the clock going back, and no arrival time can follow it.
            if pcr < before {
                if before - pcr > PCR_WRAP / 2 {
                    base += PCR_WRAP;
                } else {
                    return Ok(Err(format!(
                        "its clock steps back {:.3} s at packet {i}",
                        (before - pcr) as f64 / 27e6
                    )));
                }
            }
        }
        was = Some(pcr);
        anchors.push((i, base + pcr));
    }
    if anchors.is_empty() {
        return Ok(Err("it carries no clock reference".into()));
    }
    Ok(Ok(anchors))
}

/// The arrival time of packet `i`, given the anchor at or before it (`at`,
/// or the first where there is none).
fn arrival(anchors: &[(u64, u64)], at: usize, i: u64) -> u64 {
    // The pair whose rate applies: the one `i` sits between, or the nearest
    // at either end.
    let pair = match anchors.len() {
        1 => None,
        n => Some(at.min(n - 2)),
    };
    let Some(k) = pair else {
        return anchors[0].1;
    };
    let (p0, t0) = anchors[k];
    let (p1, t1) = anchors[k + 1];
    let span = p1 - p0;
    if span == 0 {
        return t0;
    }
    let ticks = i128::from(t1 as i64 - t0 as i64);
    let off = i128::from(i as i64 - p0 as i64) * ticks / i128::from(span);
    // Before the first reference this can come out below zero; the field is
    // the clock modulo 2^30, so wrapping it is the right answer there too.
    (i128::from(t0) + off).rem_euclid(1i128 << 62) as u64
}

/// Write each packet's four leading bytes again, in place.
#[allow(clippy::chunks_exact_to_as_chunks)] // `as_chunks_mut` needs Rust 1.88.
fn write(path: &str, anchors: &[(u64, u64)]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {path} to stamp its arrival times"))?;
    let size = file.metadata()?.len();
    let mut block = vec![0u8; SOURCE_PACKET * 4096];
    let (mut offset, mut next) = (0u64, 0usize);
    while offset < size {
        let take = (size - offset).min(block.len() as u64) as usize;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut block[..take])
            .with_context(|| format!("reading {path}"))?;
        let first = offset / SOURCE_PACKET as u64;
        for (n, frame) in block[..take].chunks_exact_mut(SOURCE_PACKET).enumerate() {
            let i = first + n as u64;
            while anchors.get(next + 1).is_some_and(|&(p, _)| p <= i) {
                next += 1;
            }
            let at = arrival(anchors, next, i) as u32 & ATS_MASK;
            let held = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) & !ATS_MASK;
            frame[..4].copy_from_slice(&(held | at).to_be_bytes());
        }
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&block[..take])
            .with_context(|| format!("writing the arrival times of {path}"))?;
        offset += take as u64;
    }
    file.flush()?;
    Ok(())
}

/// The clock reference a transport stream packet carries, in 27 MHz ticks.
fn pcr_of(p: &[u8]) -> Option<u64> {
    if (p[3] >> 4) & 0x02 == 0 || p[4] < 7 || p[5] & 0x10 == 0 {
        return None;
    }
    let base = (u64::from(p[6]) << 25)
        | (u64::from(p[7]) << 17)
        | (u64::from(p[8]) << 9)
        | (u64::from(p[9]) << 1)
        | (u64::from(p[10]) >> 7);
    let ext = (u64::from(p[10] & 0x01) << 8) | u64::from(p[11]);
    // An extension out of its range is not a clock reference.
    (ext < 300).then_some(base * 300 + ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(pid: u16, pcr: Option<u64>, ats: u32) -> [u8; SOURCE_PACKET] {
        let mut f = [0xFFu8; SOURCE_PACKET];
        f[..4].copy_from_slice(&ats.to_be_bytes());
        f[4..8].copy_from_slice(&[0x47, (pid >> 8) as u8 & 0x1F, pid as u8, 0x30]);
        f[8] = 183;
        f[9] = if pcr.is_some() { 0x10 } else { 0x00 };
        if let Some(t) = pcr {
            let (b, e) = (t / 300, t % 300);
            f[10..16].copy_from_slice(&[
                (b >> 25) as u8,
                (b >> 17) as u8,
                (b >> 9) as u8,
                (b >> 1) as u8,
                ((b & 1) << 7) as u8 | 0x7E | (e >> 8) as u8,
                e as u8,
            ]);
        }
        f
    }

    fn run(name: &str, frames: &[[u8; SOURCE_PACKET]]) -> Vec<u8> {
        let dir = std::env::temp_dir().join(format!("smartcut-m2ts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, frames.concat()).unwrap();
        restamp(path.to_str().unwrap()).unwrap();
        let out = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        out
    }

    fn times(out: &[u8]) -> Vec<u32> {
        out.chunks_exact(SOURCE_PACKET)
            .map(|f| u32::from_be_bytes([f[0], f[1], f[2], f[3]]) & ATS_MASK)
            .collect()
    }

    #[test]
    fn spread_between_references_and_carried_on_at_the_ends() {
        // libavformat's backward counter, with copy-permission bits set.
        let frames: Vec<_> = [None, Some(27_000), None, None, Some(30_000), None]
            .iter()
            .enumerate()
            .map(|(k, &pcr)| packet(0x100, pcr, 0xC000_0000 | (1_000_000 - k as u32 * 7)))
            .collect();
        let out = run("a.m2ts", &frames);
        assert_eq!(
            times(&out),
            vec![26_000, 27_000, 28_000, 29_000, 30_000, 31_000]
        );
        for (a, b) in frames.concat().chunks_exact(192).zip(out.chunks_exact(192)) {
            assert_eq!(a[4..], b[4..], "a transport packet changed");
            assert_eq!(a[0] & 0xC0, b[0] & 0xC0, "the copy bits changed");
        }
    }

    #[test]
    fn across_the_wraps() {
        // The clock reference wraps at 33 bits and the field at 30.
        let near = PCR_WRAP - 600;
        let frames = [
            packet(0x100, Some(near), 0),
            packet(0x100, None, 0),
            packet(0x100, Some(600), 0),
        ];
        let t = times(&run("b.m2ts", &frames));
        assert_eq!(t[0], (near as u32) & ATS_MASK);
        assert_eq!(t[1], ((near + 600) as u32) & ATS_MASK);
        assert_eq!(t[2], 600);
    }

    #[test]
    fn one_reference_is_a_burst_and_another_pid_is_ignored() {
        let frames = [
            packet(0x100, None, 5),
            packet(0x100, Some(90_000), 5),
            packet(0x101, Some(1), 5),
            packet(0x100, None, 5),
        ];
        assert_eq!(times(&run("c.m2ts", &frames)), vec![90_000; 4]);
    }

    #[test]
    fn nothing_to_stamp_from_is_left_alone() {
        let none = [packet(0x100, None, 9), packet(0x100, None, 3)];
        assert_eq!(run("d.m2ts", &none), none.concat());
        let back = [
            packet(0x100, Some(90_000), 9),
            packet(0x100, Some(80_000), 3),
        ];
        assert_eq!(run("e.m2ts", &back), back.concat());
    }

    #[test]
    fn only_standalone_m2ts() {
        assert!(standalone("/out/cut.m2ts"));
        assert!(standalone("cut.M2TS"));
        assert!(!standalone("/out/cut.ts"));
        assert!(!standalone("/out/BDAV/STREAM/00001.m2ts"));
        assert!(!standalone("/out/bdav/stream/00001.m2ts"));
    }
}
