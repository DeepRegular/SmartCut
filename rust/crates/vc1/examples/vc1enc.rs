//! Encode raw pictures as VC-1, so the result can be held against a decoder.
//!
//! ```text
//! cargo run --release --example vc1enc -- source.vc1 frames.yuv 1920 1080 4 8 out.vc1
//! ```
//!
//! `source.vc1` is any piece of the stream being cut into: its sequence and
//! entry-point headers are what the encoded pictures are introduced by, and
//! they decide how the pictures have to be written. `frames.yuv` is planar
//! 4:2:0, which is what a decoder hands over.
//!
//! The point of the whole exercise is the next step, which is to decode
//! `out.vc1` again and compare it with `frames.yuv`. See
//! `tests/vc1_encoder.sh`.

use smartcut_vc1::{intra::Plane, Encoder, Frame, Shape};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 8 {
        eprintln!(
            "usage: vc1enc <source.vc1> <frames.yuv> <width> <height> <count> <pquant> <out.vc1>"
        );
        std::process::exit(2);
    }
    let source = std::fs::read(&args[1]).expect("source");
    let raw = std::fs::read(&args[2]).expect("frames");
    let width: usize = args[3].parse().expect("width");
    let height: usize = args[4].parse().expect("height");
    let count: usize = args[5].parse().expect("count");
    let step: u8 = args[6].parse().expect("pquant");

    let shape = Shape::read(&source).expect("no sequence header in the source");
    let encoder =
        Encoder::new(&shape, width as u32, height as u32, step).expect("cannot encode for this");

    let (cw, ch) = ((width + 1) / 2, (height + 1) / 2);
    let frame_size = width * height + 2 * cw * ch;
    assert!(raw.len() >= frame_size * count, "not enough raw frames");

    let began = std::time::Instant::now();
    let mut out = Vec::new();
    for n in 0..count {
        let base = n * frame_size;
        let y = &raw[base..base + width * height];
        let u = &raw[base + width * height..base + width * height + cw * ch];
        let v = &raw[base + width * height + cw * ch..base + frame_size];
        let frame = Frame {
            y: Plane { data: y, stride: width, width, height },
            u: Plane { data: u, stride: cw, width: cw, height: ch },
            v: Plane { data: v, stride: cw, width: cw, height: ch },
            tff: true,
            rff: false,
            rptfrm: 0,
        };
        out.extend_from_slice(&encoder.encode(&frame));
    }
    let took = began.elapsed();
    std::fs::write(&args[7], &out).expect("write");
    println!(
        "{count} pictures at pquant {}, {} bytes ({:.1} Mb/s at 29.97), {:.2}s ({:.0}ms a picture)",
        encoder.step(),
        out.len(),
        out.len() as f64 * 8.0 * 29.97 / count as f64 / 1e6,
        took.as_secs_f64(),
        took.as_secs_f64() * 1000.0 / count as f64,
    );
}
