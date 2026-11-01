//! Rebuild M2TS arrival prefixes from the muxed PCR clock, not a fictitious
//! VBR mux_rate of one bit/second. PES, PCR and all 188-byte packets stay put.
use anyhow::{bail, Context, Result};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};

const STRIDE: usize = 192;
const WRAP: u64 = (1u64 << 33) * 300;

fn pcr(packet: &[u8]) -> Option<u64> {
    if packet.len() != 188
        || packet[0] != 0x47
        || packet[3] & 0x20 == 0
        || packet[4] < 7
        || packet[4] > 183
        || packet[5] & 0x10 == 0
    {
        return None;
    }
    let b = &packet[6..12];
    let base = (u64::from(b[0]) << 25)
        | (u64::from(b[1]) << 17)
        | (u64::from(b[2]) << 9)
        | (u64::from(b[3]) << 1)
        | u64::from(b[4] >> 7);
    let ext = (u64::from(b[4] & 1) << 8) | u64::from(b[5]);
    (ext < 300).then_some(base * 300 + ext)
}

/// PCR anchors refer to the completed output, after SI insertion/PID changes.
/// Between anchors packets are spread along that clock. Prefix/suffix packets
/// share the nearest anchor's arrival (a burst), including a one-picture cut.
#[allow(clippy::chunks_exact_to_as_chunks)] // Keep the workspace's Rust 1.82 MSRV.
pub(crate) fn restamp(path: &str) -> Result<()> {
    let size = std::fs::metadata(path)?.len();
    if size == 0 || size % STRIDE as u64 != 0 {
        bail!("M2TS output is not 192-byte aligned");
    }
    let mut input = BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
    let mut frame = [0u8; STRIDE];
    let mut anchors = Vec::new();
    let mut selected = None;
    let mut last = None;
    let mut clock = 0u64;
    for at in 0..size / STRIDE as u64 {
        input.read_exact(&mut frame)?;
        if frame[4] != 0x47 {
            bail!("lost M2TS packet sync");
        }
        let pid = (u16::from(frame[5] & 31) << 8) | u16::from(frame[6]);
        let Some(raw) = pcr(&frame[4..]) else {
            continue;
        };
        let pid_chosen = *selected.get_or_insert(pid);
        if pid != pid_chosen {
            continue;
        }
        if let Some(before) = last {
            let delta = (raw + WRAP - before) % WRAP;
            if delta > WRAP / 2 {
                bail!("M2TS PCR runs backwards");
            }
            clock += delta;
        } else {
            clock = raw;
        }
        last = Some(raw);
        anchors.push((at, clock));
    }
    drop(input);
    if anchors.is_empty() {
        bail!("M2TS output has no PCR to clock its arrivals");
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let mut block = vec![0u8; STRIDE * 4096];
    let mut at = 0;
    let mut next = 0;
    while at < size {
        let take = (size - at).min(block.len() as u64) as usize;
        file.read_exact(&mut block[..take])
            .context("reading M2TS arrival prefixes")?;
        for (i, frame) in block[..take].chunks_exact_mut(STRIDE).enumerate() {
            let packet = at / STRIDE as u64 + i as u64;
            while next + 1 < anchors.len() && anchors[next + 1].0 <= packet {
                next += 1;
            }
            let (from, time) = anchors[next];
            let arrival = if packet <= from || next + 1 == anchors.len() {
                time
            } else {
                let (until, end) = anchors[next + 1];
                time + ((u128::from(end - time) * u128::from(packet - from))
                    / u128::from(until - from)) as u64
            };
            let flags = u32::from_be_bytes(frame[..4].try_into().unwrap()) & 0xc0000000;
            let prefix = flags | (arrival as u32 & 0x3fffffff);
            frame[..4].copy_from_slice(&prefix.to_be_bytes());
        }
        file.seek(SeekFrom::Start(at))?;
        file.write_all(&block[..take])
            .context("writing M2TS arrival prefixes")?;
        at += take as u64;
    }
    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(clock: Option<u64>) -> [u8; STRIDE] {
        let mut p = [0xff; STRIDE];
        p[..4].copy_from_slice(&0x80000000u32.to_be_bytes());
        p[4..8].copy_from_slice(&[0x47, 0x10, 0x11, 0x20]);
        p[8] = 183;
        p[9] = if clock.is_some() { 0x10 } else { 0 };
        if let Some(t) = clock {
            let base = t / 300;
            let ext = t % 300;
            p[10..16].copy_from_slice(&[
                (base >> 25) as u8,
                (base >> 17) as u8,
                (base >> 9) as u8,
                (base >> 1) as u8,
                ((base & 1) << 7) as u8 | 0x7e | (ext >> 8) as u8,
                ext as u8,
            ]);
        }
        p
    }
    #[test]
    #[allow(clippy::chunks_exact_to_as_chunks)] // as_chunks requires Rust 1.88.
    fn arrival_wrap_does_not_change_a_transport_packet() {
        let path =
            std::env::temp_dir().join(format!("smartcut-arrivals-{}.m2ts", std::process::id()));
        let begin = (1u64 << 30) - 27000;
        let frames = [
            packet(None),
            packet(Some(begin)),
            packet(None),
            packet(Some(begin + 54000)),
            packet(None),
        ];
        let before: Vec<u8> = frames.iter().flatten().copied().collect();
        std::fs::write(&path, &before).unwrap();
        restamp(path.to_str().unwrap()).unwrap();
        let after = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let mut last = None;
        for (a, b) in before.chunks_exact(192).zip(after.chunks_exact(192)) {
            assert_eq!(&a[4..], &b[4..]);
            let t = u32::from_be_bytes(b[..4].try_into().unwrap());
            assert_eq!(t & 0xc0000000, 0x80000000);
            let t = t & 0x3fffffff;
            if let Some(prev) = last {
                assert!((t.wrapping_sub(prev) & 0x3fffffff) < 1 << 29);
            }
            last = Some(t);
        }
    }
    #[test]
    fn pcr_wrap_and_a_single_pcr_are_usable() {
        for (n, frames) in [
            vec![packet(Some(1000)), packet(None)],
            vec![packet(Some(WRAP - 300)), packet(None), packet(Some(300))],
        ]
        .iter()
        .enumerate()
        {
            let path =
                std::env::temp_dir().join(format!("smartcut-pcr-{}-{n}.m2ts", std::process::id()));
            std::fs::write(&path, frames.iter().flatten().copied().collect::<Vec<_>>()).unwrap();
            restamp(path.to_str().unwrap()).unwrap();
            std::fs::remove_file(path).unwrap();
        }
    }
}
