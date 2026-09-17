//! Where a coefficient sits in the block, and what the matrix says about it.
//!
//! A coefficient is written down as a run and a level, which says where it is
//! in the order the block is *scanned* -- and everything that weighs one
//! coefficient against another wants to know where it is in the block, which
//! is a different thing. Two scans are allowed and a picture says which it
//! uses; a quantisation matrix is transmitted in the first of them whichever
//! is in use.
//!
//! What the weight is for: dropping a coefficient costs what the coefficient
//! was worth, and what it was worth is its level times the step times the
//! matrix entry for its place in the block. A matrix that rises steeply with
//! frequency -- and a broadcaster's does -- is saying that its high
//! frequencies carry more, not less.

/// The scan a block is written in, unless the picture says otherwise.
pub const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10,
    17, 24, 32, 25, 18, 11, 4, 5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// The other one, which interlaced pictures use: down the block rather than
/// across it, because a field picture's rows are two apart.
pub const ALTERNATE: [u8; 64] = [
    0, 8, 16, 24, 1, 9, 2, 10,
    17, 25, 32, 40, 48, 56, 57, 49,
    41, 33, 26, 18, 3, 11, 4, 12,
    19, 27, 34, 42, 50, 58, 35, 43,
    51, 59, 20, 28, 5, 13, 6, 14,
    21, 29, 36, 44, 52, 60, 37, 45,
    53, 61, 22, 30, 7, 15, 23, 31,
    38, 46, 54, 62, 39, 47, 55, 63,
];

/// What a sequence header means by "no intra matrix of my own".
pub const DEFAULT_INTRA: [u8; 64] = [
    8, 16, 19, 22, 26, 27, 29, 34,
    16, 16, 22, 24, 27, 29, 34, 37,
    19, 22, 26, 27, 29, 34, 34, 38,
    22, 22, 26, 27, 29, 34, 37, 40,
    22, 26, 27, 29, 32, 35, 40, 48,
    26, 27, 29, 32, 35, 40, 48, 58,
    26, 27, 29, 34, 38, 46, 56, 69,
    27, 29, 35, 38, 46, 56, 69, 83,
];

/// And by "no non-intra matrix of my own": flat, every coefficient weighed
/// the same.
pub const DEFAULT_NON_INTRA: [u8; 64] = [16; 64];

/// Turn a matrix as it was transmitted -- in zigzag order -- into one indexed
/// by the place in the block.
pub fn unscan(sent: &[u8; 64]) -> [u8; 64] {
    let mut out = [0u8; 64];
    for (k, &v) in sent.iter().enumerate() {
        out[ZIGZAG[k] as usize] = v;
    }
    out
}

/// What each place in the *scan* weighs, for a picture scanning this way.
pub fn weights(matrix: &[u8; 64], alternate: bool) -> [u16; 64] {
    let scan = if alternate { &ALTERNATE } else { &ZIGZAG };
    let mut out = [0u16; 64];
    for (k, &at) in scan.iter().enumerate() {
        out[k] = u16::from(matrix[at as usize]);
    }
    out
}
