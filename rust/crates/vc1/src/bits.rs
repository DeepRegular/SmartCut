//! Reading and writing a VC-1 bitstream a bit at a time.
//!
//! Everything in VC-1 above the byte-aligned start codes is a bit field, and
//! most of them are variable length, so both the parser and the encoder work
//! through here rather than on bytes.

/// Remove the encapsulation bytes a VC-1 bitstream is carried with.
///
/// A start code is three bytes, `00 00 01`, so the payload between two of
/// them must never contain that pattern. VC-1 avoids it the way H.264 does:
/// wherever two zero bytes are followed by a byte of 3 or less, a `03` is
/// pushed in between. Taking it back out is the first thing any reader does.
pub fn unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 3 {
            out.push(0);
            out.push(0);
            i += 3;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

/// Put the encapsulation bytes back, for a payload being written out.
///
/// The inverse of [`unescape`]. Two zero bytes followed by anything the
/// decoder could mistake for the tail of a start code -- or for an
/// encapsulation byte itself -- get a `03` pushed in between.
pub fn escape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 64 + 4);
    let mut zeros = 0;
    for &b in data {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// A bit reader over an already unescaped payload.
///
/// Reading past the end yields zeroes and is remembered rather than
/// signalled at once: a header parse is a straight run of reads, and testing
/// every one of them would bury the syntax it is meant to describe. The
/// caller asks [`Reader::ok`] when it is done.
pub struct Reader<'a> {
    data: &'a [u8],
    bit: usize,
    overrun: bool,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit: 0,
            overrun: false,
        }
    }

    /// Whether every read so far landed inside the payload.
    pub fn ok(&self) -> bool {
        !self.overrun
    }

    /// How many bits have been taken.
    pub fn position(&self) -> usize {
        self.bit
    }

    pub fn u(&mut self, n: usize) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit1() as u32;
        }
        v
    }

    pub fn bit1(&mut self) -> bool {
        let byte = match self.data.get(self.bit >> 3) {
            Some(&b) => b,
            None => {
                self.overrun = true;
                0
            }
        };
        let b = (byte >> (7 - (self.bit & 7))) & 1;
        self.bit += 1;
        b != 0
    }

    /// VC-1's two-bit selector: `0`, `10` or `11` meaning 0, 1 or 2.
    pub fn u012(&mut self) -> u32 {
        if !self.bit1() {
            0
        } else if !self.bit1() {
            1
        } else {
            2
        }
    }

    /// Count the ones before the next zero, giving up after `limit`.
    pub fn unary(&mut self, limit: u32) -> u32 {
        let mut n = 0;
        while n < limit && self.bit1() {
            n += 1;
        }
        n
    }
}

/// A bit writer, byte-aligned only when it is finished.
#[derive(Default)]
pub struct Writer {
    out: Vec<u8>,
    partial: u8,
    filled: u8,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many bits have been written.
    pub fn position(&self) -> usize {
        self.out.len() * 8 + self.filled as usize
    }

    pub fn u(&mut self, value: u32, n: usize) {
        for k in (0..n).rev() {
            self.bit1((value >> k) & 1 != 0);
        }
    }

    pub fn bit1(&mut self, b: bool) {
        self.partial = (self.partial << 1) | b as u8;
        self.filled += 1;
        if self.filled == 8 {
            self.out.push(self.partial);
            self.partial = 0;
            self.filled = 0;
        }
    }

    /// Write a variable-length code: the low `bits` of `code`, most
    /// significant first.
    pub fn vlc(&mut self, code: u32, bits: u8) {
        self.u(code, bits as usize);
    }

    /// VC-1's two-bit selector, the counterpart of [`Reader::u012`].
    pub fn u012(&mut self, value: u32) {
        match value {
            0 => self.bit1(false),
            1 => {
                self.bit1(true);
                self.bit1(false);
            }
            _ => {
                self.bit1(true);
                self.bit1(true);
            }
        }
    }

    /// Finish the last byte with zero bits and hand the payload over.
    pub fn finish(mut self) -> Vec<u8> {
        if self.filled > 0 {
            let pad = 8 - self.filled;
            self.partial <<= pad;
            self.out.push(self.partial);
            self.partial = 0;
            self.filled = 0;
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_round_trips() {
        let raw: &[u8] = &[0, 0, 1, 2, 0, 0, 0, 3, 0, 0, 4, 9];
        let wrapped = escape(raw);
        assert!(!wrapped.windows(3).any(|w| w == [0, 0, 1]));
        assert_eq!(unescape(&wrapped), raw);
    }

    #[test]
    fn bits_round_trip() {
        let mut w = Writer::new();
        w.u(5, 3);
        w.u012(2);
        w.u(0x3ff, 12);
        w.bit1(true);
        let data = w.finish();

        let mut r = Reader::new(&data);
        assert_eq!(r.u(3), 5);
        assert_eq!(r.u012(), 2);
        assert_eq!(r.u(12), 0x3ff);
        assert!(r.bit1());
        assert!(r.ok());
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
