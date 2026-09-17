//! Reading and writing an MPEG-2 video bitstream a bit at a time.
//!
//! Above the byte-aligned start codes, nothing in a picture is a whole number
//! of bytes: a macroblock is a run of variable-length codes and the next one
//! starts wherever the last one stopped. So both the walk and the rewrite
//! work through here.
//!
//! The rewrite copies far more than it changes -- an address increment, a
//! macroblock type, a motion vector are all written back exactly as they
//! arrived -- so [`Sink::copy`] is the operation that matters most, and it
//! moves bits in words rather than one at a time.

/// Where bits are written.
///
/// Two things are done with a rewritten picture and only one of them needs
/// the bytes: the rate control asks how large the picture *would* be at a
/// given strength, several times, before anything is written at all. So the
/// emit routine is written once against this, and counting is a sink that
/// keeps nothing.
pub trait Sink {
    /// The low `n` bits of `value`, most significant first.
    fn u(&mut self, value: u32, n: u32);
    /// `len` bits of `src` starting at bit `from`.
    fn copy(&mut self, src: &[u8], from: usize, len: usize);
    /// How many bits have been written.
    fn position(&self) -> usize;
    /// Zero bits up to the next byte boundary, which is how every slice ends.
    fn align(&mut self) {
        let pad = (8 - (self.position() & 7)) & 7;
        if pad > 0 {
            self.u(0, pad as u32);
        }
    }
}

/// A sink that writes the bits down.
#[derive(Default)]
pub struct Writer {
    out: Vec<u8>,
    /// Bits written but not yet a whole byte, in the low `filled` bits.
    acc: u64,
    filled: u32,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Room for a picture of about this size, so the rewrite does not spend
    /// its time growing a buffer.
    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            out: Vec::with_capacity(bytes),
            acc: 0,
            filled: 0,
        }
    }

    /// Whole bytes, where the writer is already on a byte boundary. Nothing
    /// else can use it, which is why it is checked rather than assumed.
    pub fn bytes(&mut self, src: &[u8]) {
        debug_assert_eq!(self.filled, 0);
        self.out.extend_from_slice(src);
    }

    /// Pad to a byte boundary and hand the picture over.
    pub fn finish(mut self) -> Vec<u8> {
        self.align();
        self.out
    }

    #[inline]
    fn drain(&mut self) {
        while self.filled >= 8 {
            self.filled -= 8;
            self.out.push((self.acc >> self.filled) as u8);
        }
    }
}

impl Sink for Writer {
    #[inline]
    fn u(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 32);
        if n == 0 {
            return;
        }
        let mask = if n >= 32 { u32::MAX } else { (1 << n) - 1 };
        self.acc = (self.acc << n) | u64::from(value & mask);
        self.filled += n;
        self.drain();
    }

    fn copy(&mut self, src: &[u8], from: usize, len: usize) {
        // A byte-aligned run of whole bytes is the common case for the parts
        // of a picture that are not slices, and it is worth not taking it a
        // word at a time.
        if self.filled == 0 && from & 7 == 0 && len & 7 == 0 {
            let (a, b) = (from / 8, (from + len) / 8);
            if b <= src.len() {
                self.out.extend_from_slice(&src[a..b]);
                return;
            }
        }
        let mut r = Reader::at(src, from);
        let mut left = len;
        while left >= 32 {
            let v = r.u(32);
            self.u(v, 32);
            left -= 32;
        }
        if left > 0 {
            let v = r.u(left as u32);
            self.u(v, left as u32);
        }
    }

    fn position(&self) -> usize {
        self.out.len() * 8 + self.filled as usize
    }
}

/// A sink that writes nothing and remembers only how long the picture came
/// out. What the rate control asks with.
#[derive(Default)]
pub struct Counter(usize);

impl Counter {
    pub fn new() -> Self {
        Self(0)
    }

    /// The length in bytes, rounded up the way a finished picture is.
    pub fn bytes(&self) -> usize {
        self.0.div_ceil(8)
    }
}

impl Sink for Counter {
    #[inline]
    fn u(&mut self, _value: u32, n: u32) {
        self.0 += n as usize;
    }

    fn copy(&mut self, _src: &[u8], _from: usize, len: usize) {
        self.0 += len;
    }

    fn position(&self) -> usize {
        self.0
    }
}

/// A bit reader over a picture.
///
/// Reading past the end gives zeroes and is remembered rather than signalled
/// at once: a macroblock is a straight run of reads and testing every one of
/// them would bury the syntax. Whoever walked a slice asks [`Reader::ok`]
/// when it has finished, and a slice that ran off the end is one this
/// program declines to rewrite.
pub struct Reader<'a> {
    data: &'a [u8],
    bit: usize,
    overrun: bool,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self::at(data, 0)
    }

    pub fn at(data: &'a [u8], bit: usize) -> Self {
        Self {
            data,
            bit,
            overrun: false,
        }
    }

    pub fn ok(&self) -> bool {
        !self.overrun
    }

    pub fn position(&self) -> usize {
        self.bit
    }

    pub fn seek(&mut self, bit: usize) {
        self.bit = bit;
    }

    /// Whether there is anything left to read at all.
    pub fn exhausted(&self) -> bool {
        self.bit >= self.data.len() * 8
    }

    /// The next `n` bits without taking them. `n` up to 25.
    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        debug_assert!(n <= 25);
        let at = self.bit >> 3;
        let mut window: u32 = 0;
        for k in 0..4 {
            window = (window << 8) | u32::from(self.data.get(at + k).copied().unwrap_or(0));
        }
        let off = (self.bit & 7) as u32;
        (window << off) >> (32 - n)
    }

    #[inline]
    pub fn skip(&mut self, n: u32) {
        self.bit += n as usize;
        if self.bit > self.data.len() * 8 {
            self.overrun = true;
        }
    }

    #[inline]
    pub fn u(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if n <= 25 {
            let v = self.peek(n);
            self.skip(n);
            return v;
        }
        let high = self.u(n - 16);
        let low = self.u(16);
        (high << 16) | low
    }

    #[inline]
    pub fn bit1(&mut self) -> bool {
        self.u(1) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_round_trip() {
        let mut w = Writer::new();
        w.u(5, 3);
        w.u(0x3ff, 12);
        w.u(1, 1);
        w.u(0xdead_beef, 32);
        let data = w.finish();

        let mut r = Reader::new(&data);
        assert_eq!(r.u(3), 5);
        assert_eq!(r.u(12), 0x3ff);
        assert_eq!(r.u(1), 1);
        assert_eq!(r.u(32), 0xdead_beef);
        assert!(r.ok());
    }

    #[test]
    fn copying_bits_moves_them_unchanged() {
        let src: &[u8] = &[0b1011_0010, 0b0100_1111, 0b1110_0001, 0x5a, 0xa5, 0x0f];
        for from in 0..9 {
            for len in 0..33 {
                let mut w = Writer::new();
                w.u(0b101, 3);
                w.copy(src, from, len);
                let out = w.finish();

                let mut r = Reader::new(&out);
                assert_eq!(r.u(3), 0b101);
                let mut want = Reader::at(src, from);
                for _ in 0..len {
                    assert_eq!(r.u(1), want.u(1), "from {from} len {len}");
                }
            }
        }
    }

    #[test]
    fn counting_agrees_with_writing() {
        let src: &[u8] = &[0x12, 0x34, 0x56, 0x78];
        let mut w = Writer::new();
        let mut c = Counter::new();
        for (v, n) in [(3u32, 2u32), (0, 7), (511, 9), (1, 1)] {
            w.u(v, n);
            c.u(v, n);
        }
        w.copy(src, 3, 20);
        c.copy(src, 3, 20);
        w.align();
        c.align();
        assert_eq!(w.finish().len(), c.bytes());
    }

    #[test]
    fn reading_past_the_end_is_remembered() {
        let mut r = Reader::new(&[0xff]);
        r.u(8);
        assert!(r.ok());
        r.u(1);
        assert!(!r.ok());
    }
}
