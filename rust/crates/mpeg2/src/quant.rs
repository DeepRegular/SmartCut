//! What a coarser quantiser step means for a coefficient.
//!
//! A coefficient is written down as a level, and what the decoder makes of it
//! is that level times the step in force -- times the entry of the
//! quantisation matrix for its place in the block, and shifted. Writing the
//! same coefficient at a coarser step means solving that for the new level:
//!
//! ```text
//! intra      value = (level * step * weight) >> 4
//! non-intra  value = ((2 * level + 1) * step * weight) >> 5
//! ```
//!
//! **The weight falls out of both.** It is the same coefficient in the same
//! place of the same block, so whatever the matrix says about it says the
//! same thing on either side of the division -- which is why nothing here
//! reads the quantisation matrices, and why a recording that carries its own
//! matrices needs no special case.
//!
//! What is left is the ratio of the two steps, and the rounding. The level
//! that comes back is the one whose value lands nearest the old one; where
//! that is zero the coefficient is dropped, and dropping coefficients is
//! where the bytes actually go.

use crate::tables::NON_LINEAR_QUANTISER;

/// The policy: how coarse this is allowed to get.
#[derive(Clone, Copy, Debug)]
pub struct Quantiser {
    /// The largest quantiser scale code to write. 31 is what the field can
    /// hold; a lower ceiling is a floor under the picture, at the cost of
    /// not reaching every size that might be asked for.
    pub ceiling: u8,
}

impl Default for Quantiser {
    fn default() -> Self {
        Self { ceiling: 31 }
    }
}

impl Quantiser {
    /// What one code is worth. The codes run 1..31 either way: doubled where
    /// the picture says its scale is linear, and through
    /// [`NON_LINEAR_QUANTISER`] where it does not.
    pub fn step_of(code: u8, non_linear: bool) -> u32 {
        let code = code.clamp(1, 31);
        if non_linear {
            u32::from(NON_LINEAR_QUANTISER[code as usize])
        } else {
            u32::from(code) * 2
        }
    }

    /// Work out, once for the whole picture, what each of the 31 codes
    /// becomes when the scale is lifted this far.
    ///
    /// **The lift is a number of codes, not a factor.** A recording does not
    /// write one scale: it writes a fine one where the picture is flat and a
    /// coarse one where it is busy, macroblock by macroblock, because that is
    /// where the eye looks. Multiplying those scales by a common factor tears
    /// that apart -- a macroblock already at a coarse scale is pushed far
    /// past it while a fine one barely moves, and the error that appears is
    /// worst exactly where the encoder had already decided it could least
    /// afford it. Adding a common number of codes keeps the recording's own
    /// ordering and moves everything by about the same amount.
    ///
    /// Measured against the reference material, at the size where the two
    /// land together: a factor costs five decibels that an offset does not.
    /// It is also what the tool the material came out of does -- every
    /// macroblock of its first picture is written two codes coarser than the
    /// same macroblock of the source, and every macroblock of the next, and
    /// the next.
    ///
    /// **The codes are coarse and the sizes they buy are not.** Two codes
    /// coarser is a step this recording cannot split, and a run that has to
    /// land on a disc needs what is in between. So a code becomes two codes
    /// and how often to use each: the fraction says what share of macroblocks
    /// take the coarser one, spread evenly over the picture rather than in a
    /// patch, which is what the phase is for.
    pub fn map(&self, non_linear: bool, lift: f64) -> Map {
        let mut map = Map {
            lo: [0; 32],
            hi: [0; 32],
            frac: [0.0; 32],
            old: [0; 32],
            step_lo: [0; 32],
            step_hi: [0; 32],
        };
        let ceiling = self.ceiling.clamp(1, 31);
        let lift = lift.clamp(0.0, 30.0);
        let whole = lift.floor();
        let frac = (lift - whole) as f32;
        for code in 1..32u8 {
            let top = ceiling.max(code);
            let lo = (u32::from(code) + whole as u32).min(u32::from(top)) as u8;
            let hi = (lo + 1).min(top);
            map.lo[code as usize] = lo;
            map.hi[code as usize] = hi;
            map.frac[code as usize] = if hi > lo { frac } else { 0.0 };
            map.old[code as usize] = Self::step_of(code, non_linear) as u16;
            map.step_lo[code as usize] = Self::step_of(lo, non_linear) as u16;
            map.step_hi[code as usize] = Self::step_of(hi, non_linear) as u16;
        }
        map
    }

    pub fn at_the_top(&self, code: u8) -> bool {
        code >= self.ceiling.min(31)
    }
}

/// What each quantiser scale code becomes, for one picture at one strength.
pub struct Map {
    lo: [u8; 32],
    hi: [u8; 32],
    frac: [f32; 32],
    old: [u16; 32],
    step_lo: [u16; 32],
    step_hi: [u16; 32],
}

/// Where in the spread a macroblock falls.
///
/// A macroblock's place in the picture, turned into a number between nothing
/// and one by the golden ratio, which is the cheapest way to hand out a share
/// of something evenly rather than in runs: every prefix of the sequence is
/// spread about as evenly as that many numbers can be.
#[inline]
pub fn phase(index: u32) -> f32 {
    (f64::from(index) * 0.618_033_988_749_895).fract() as f32
}

impl Map {
    /// The code to write where the picture wrote `code`, for a macroblock at
    /// this phase.
    #[inline]
    pub fn step(&self, code: u8, phase: f32) -> u8 {
        let at = usize::from(code.clamp(1, 31));
        if phase < self.frac[at] {
            self.hi[at]
        } else {
            self.lo[at]
        }
    }

    /// How a coefficient quantised at `code` is written again.
    #[inline]
    pub fn ratio(&self, code: u8, phase: f32) -> Ratio {
        let at = usize::from(code.clamp(1, 31));
        let new = if phase < self.frac[at] {
            self.step_hi[at]
        } else {
            self.step_lo[at]
        };
        Ratio {
            old: u32::from(self.old[at]),
            new: u32::from(new),
        }
    }

    /// Whether this strength changes anything at all.
    pub fn is_identity(&self) -> bool {
        (1..32).all(|i| self.lo[i] as usize == i && self.frac[i] == 0.0)
    }
}

/// One macroblock's worth of division: the step it was written at, and the
/// step it is being written at now.
#[derive(Clone, Copy, Debug)]
pub struct Ratio {
    old: u32,
    new: u32,
}

impl Ratio {
    /// The level to write instead of this one. Zero means the coefficient
    /// goes.
    #[inline]
    pub fn apply(&self, level: i16, intra: bool) -> i16 {
        if self.new == self.old {
            return level;
        }
        let mag = u32::from(level.unsigned_abs());
        let mag = if intra {
            // Nearest: the value is the level times the step, so the new
            // level is the old one scaled by the ratio of the steps.
            (2 * mag * self.old + self.new) / (2 * self.new)
        } else {
            // A non-intra level of n stands for the value 2n+1 in steps, so
            // the arithmetic is on 2n+1 and the halving is what rounds it.
            ((2 * mag + 1) * self.old) / (2 * self.new)
        };
        let mag = mag.min(2047) as i16;
        if level < 0 {
            -mag
        } else {
            mag
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_lift_changes_nothing() {
        for non_linear in [false, true] {
            let map = Quantiser::default().map(non_linear, 0.0);
            assert!(map.is_identity());
            for code in 1..32u8 {
                let r = map.ratio(code, 0.5);
                for level in [-2047i16, -33, -1, 1, 7, 100, 2047] {
                    assert_eq!(r.apply(level, true), level);
                    assert_eq!(r.apply(level, false), level);
                }
            }
        }
    }

    #[test]
    fn a_coarser_step_never_makes_a_level_larger() {
        for non_linear in [false, true] {
            for lift in [0.5, 2.0, 3.7, 8.0] {
                let map = Quantiser::default().map(non_linear, lift);
                for code in 1..32u8 {
                    assert!(map.step(code, 0.5) >= code);
                    let r = map.ratio(code, 0.5);
                    for level in 1..=2047i16 {
                        let out = r.apply(level, true);
                        assert!((0..=level).contains(&out), "{level} -> {out}");
                        assert_eq!(r.apply(-level, true), -out);
                        let out = r.apply(level, false);
                        assert!((0..=level).contains(&out), "{level} -> {out}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_step_climbs_with_the_lift() {
        let q = Quantiser::default();
        let mild = q.map(true, 1.0);
        let hard = q.map(true, 3.0);
        for code in 1..32u8 {
            assert!(hard.step(code, 0.99) >= mild.step(code, 0.99));
        }
        // The broadcast material this was written for sits at code 1 or 2,
        // and the non-linear scale climbs steeply from there: doubling the
        // step of code 1 lands on code 2, and the same again on code 4.
        // A whole number of codes moves every macroblock and splits none of
        // them: the two reference files differ by exactly this much.
        assert_eq!(mild.step(1, 0.0), 2);
        assert_eq!(mild.step(1, 0.99), 2);
        assert_eq!(hard.step(1, 0.0), 4);
        assert_eq!(hard.step(5, 0.99), 8);
    }

    #[test]
    fn a_fraction_of_the_macroblocks_take_the_coarser_step() {
        // A lift of one code and a half: half the macroblocks take one code
        // coarser and half take two.
        let map = Quantiser::default().map(true, 0.5);
        let coarse = (0..1000)
            .filter(|&i| map.step(1, phase(i)) == 2)
            .count();
        assert!((480..=520).contains(&coarse), "{coarse} of 1000");
        // And every prefix of the sequence is about as even, which is what
        // keeps the coarser macroblocks from landing in a patch.
        let early = (0..40).filter(|&i| map.step(1, phase(i)) == 2).count();
        assert!((17..=23).contains(&early), "{early} of 40");
    }

    #[test]
    fn a_ceiling_holds_the_step_down() {
        let q = Quantiser { ceiling: 8 };
        let map = q.map(true, 20.0);
        for code in 1..32u8 {
            assert!(map.step(code, 0.5) <= 8.max(code));
        }
    }
}
