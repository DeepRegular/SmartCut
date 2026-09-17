//! Turning the tables of [`crate::tables`] into something a walk can use.
//!
//! Every table in Annex B is a prefix code, so the fastest way to read one is
//! to take as many bits as the longest code in it, look the whole lot up, and
//! find both which row it is and how many of those bits actually belonged to
//! it. That is what a [`Lut`] is: one entry per possible window, built once.
//!
//! The coefficient tables have codes of sixteen bits, so their lookup is
//! 65536 entries of two bytes. A quarter of a megabyte for the pair of them,
//! built the first time a recording needs one.

use std::sync::OnceLock;

use crate::tables::{self, Code, MbType};

/// A decoded row: which row of the table, and how many bits it took.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hit {
    pub row: u8,
    pub bits: u32,
}

/// A lookup built from a table of codes.
pub struct Lut {
    /// How many bits the window is, which is the longest code in the table.
    window: u32,
    /// `(row << 5) | bits`, or zero where those bits are no code at all.
    rows: Vec<u16>,
}

impl Lut {
    /// Build the lookup. Row `i` of `codes` is what a window matching
    /// `codes[i]` decodes to; rows with zero bits are holes and are skipped,
    /// which is how a table names a code it does not have.
    pub fn build(codes: &[Code]) -> Self {
        let window = codes.iter().map(|&(_, b)| u32::from(b)).max().unwrap_or(1);
        assert!(window <= 16, "a window wider than sixteen bits");
        let mut rows = vec![0u16; 1 << window];
        for (i, &(code, bits)) in codes.iter().enumerate() {
            if bits == 0 {
                continue;
            }
            let bits = u32::from(bits);
            let spare = window - bits;
            let base = (u32::from(code) & ((1 << bits) - 1)) << spare;
            let packed = ((i as u16) << 5) | bits as u16;
            for k in 0..(1u32 << spare) {
                let at = (base | k) as usize;
                debug_assert_eq!(rows[at], 0, "two rows claim the same bits");
                rows[at] = packed;
            }
        }
        Self { window, rows }
    }

    /// What the next bits say, or `None` where they are no code in this
    /// table -- which in a recording that decodes means the walk has lost the
    /// thread and the picture is one to leave alone.
    #[inline]
    pub fn read(&self, r: &mut crate::bits::Reader) -> Option<Hit> {
        let packed = self.rows[r.peek(self.window) as usize];
        if packed == 0 {
            return None;
        }
        let hit = Hit {
            row: (packed >> 5) as u8,
            bits: u32::from(packed & 31),
        };
        r.skip(hit.bits);
        Some(hit)
    }
}

/// The two coefficient tables, and the small ones every macroblock uses.
pub struct Tables {
    pub coefficients: [Lut; 2],
    pub address_increment: Lut,
    pub coded_block_pattern: Lut,
    pub motion_code: Lut,
    pub dc_size: [Lut; 2],
    pub mb_type: [Lut; 3],
    /// The other direction: which row of the coefficient table carries a
    /// given run and level, or none where it has to be an escape. Indexed
    /// `run * (MAX_LEVEL + 1) + level`.
    pub coefficient_rows: Vec<u8>,
}

/// The one copy, built on first use.
pub fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut coefficient_rows = vec![u8::MAX; (tables::MAX_RUN + 1) * (tables::MAX_LEVEL + 1)];
        for row in 0..tables::ESCAPE {
            let run = tables::RUN[row] as usize;
            let level = tables::LEVEL[row] as usize;
            if run <= tables::MAX_RUN && level <= tables::MAX_LEVEL {
                coefficient_rows[run * (tables::MAX_LEVEL + 1) + level] = row as u8;
            }
        }
        Tables {
            coefficients: [
                Lut::build(&tables::COEFFICIENTS_ZERO),
                Lut::build(&tables::COEFFICIENTS_ONE),
            ],
            address_increment: Lut::build(&tables::ADDRESS_INCREMENT),
            coded_block_pattern: Lut::build(&tables::CODED_BLOCK_PATTERN),
            motion_code: Lut::build(&tables::MOTION_CODE),
            dc_size: [
                Lut::build(&tables::DC_SIZE_LUMA),
                Lut::build(&tables::DC_SIZE_CHROMA),
            ],
            mb_type: [
                Lut::build(&strip(&tables::MB_TYPE_I)),
                Lut::build(&strip(&tables::MB_TYPE_P)),
                Lut::build(&strip(&tables::MB_TYPE_B)),
            ],
            coefficient_rows,
        }
    })
}

/// A macroblock type table carries what the type *means* as well as its
/// code; the lookup only wants the code, and the meaning is read back out of
/// the table by row.
fn strip(types: &[MbType]) -> Vec<Code> {
    types.iter().map(|&(code, bits, _)| (code, bits)).collect()
}

/// Which row of a coefficient table carries this run and level, if any.
#[inline]
pub fn coefficient_row(t: &Tables, run: u32, level: u32) -> Option<u8> {
    if run as usize > tables::MAX_RUN || level as usize > tables::MAX_LEVEL || level == 0 {
        return None;
    }
    let row = t.coefficient_rows[run as usize * (tables::MAX_LEVEL + 1) + level as usize];
    (row != u8::MAX).then_some(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bits::{Reader, Sink, Writer};

    #[test]
    fn every_coefficient_row_reads_back_as_itself() {
        let t = tables();
        for table in 0..2 {
            let codes: &[Code] = if table == 0 {
                &tables::COEFFICIENTS_ZERO
            } else {
                &tables::COEFFICIENTS_ONE
            };
            for (row, &(code, bits)) in codes.iter().enumerate() {
                let mut w = Writer::new();
                w.u(u32::from(code), u32::from(bits));
                // Something after it, so a lookup that takes too many bits is
                // not quietly reading zeroes off the end.
                w.u(0xffff, 16);
                let data = w.finish();
                let mut r = Reader::new(&data);
                let hit = t.coefficients[table].read(&mut r).expect("no row");
                assert_eq!(hit.row as usize, row, "table {table} row {row}");
                assert_eq!(hit.bits, u32::from(bits));
            }
        }
    }

    #[test]
    fn a_run_and_level_finds_its_own_row() {
        let t = tables();
        for row in 0..tables::ESCAPE {
            let run = u32::from(tables::RUN[row]);
            let level = u32::from(tables::LEVEL[row]);
            assert_eq!(coefficient_row(t, run, level), Some(row as u8));
        }
        // Past the end of both tables is where an escape has to be written.
        assert_eq!(coefficient_row(t, 31, 40), None);
        assert_eq!(coefficient_row(t, 0, 41), None);
    }

    #[test]
    fn the_small_tables_read_back_too() {
        let t = tables();
        for (lut, codes) in [
            (&t.address_increment, &tables::ADDRESS_INCREMENT[..]),
            (&t.coded_block_pattern, &tables::CODED_BLOCK_PATTERN[1..]),
            (&t.motion_code, &tables::MOTION_CODE[..]),
            (&t.dc_size[0], &tables::DC_SIZE_LUMA[..]),
            (&t.dc_size[1], &tables::DC_SIZE_CHROMA[..]),
        ] {
            for &(code, bits) in codes {
                let mut w = Writer::new();
                w.u(u32::from(code), u32::from(bits));
                w.u(0xffff, 16);
                let data = w.finish();
                let mut r = Reader::new(&data);
                assert_eq!(lut.read(&mut r).map(|h| h.bits), Some(u32::from(bits)));
            }
        }
    }
}
