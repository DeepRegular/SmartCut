//! What happens where one clip gives way to the next.
//!
//! **A transition is the one thing in this program that cannot be smart
//! rendered, and it is worth saying why before describing it.** Everything
//! else here exists to leave the recording's own pictures alone: a cut
//! re-encodes the handful of frames at each end of a range and copies the
//! rest, and a clip written into another's shape is re-encoded because there
//! is no picture in it the file could carry. A transition is different in
//! kind. It asks for pictures that are not in either recording -- a frame
//! that is half of one and half of the other, a frame that is a quarter of
//! the way to black -- so every frame it covers is made here, whatever the
//! two clips have in common. Asked for two seconds, it costs two seconds of
//! encoding at each join and nothing anywhere else.
//!
//! ## Which clip a transition belongs to
//!
//! To the clip **before** it: a transition is that clip giving way, and the
//! list reads in the order the file is written. On the last clip there is
//! nothing to give way to, so it runs to the colour instead -- which is the
//! arrangement the reference tool arrives at, and for the same reason.
//!
//! ## The two timings, and why there are two
//!
//! | | |
//! |---|---|
//! | [`Crossing::Fade`] | The clip before goes down to the colour over half the time, and the clip after comes up out of it over the other half. Each clip gives the part of itself the transition covers, and **the output is as long as it would have been without one** |
//! | Everything else | The two clips are on screen together for the whole of it. Both give that many seconds, and the output is **that much shorter** than the two clips put together |
//!
//! Not a choice: it is what the two kinds *are*. A fade through black never
//! has both clips up at once, so asking it to consume both at once would be
//! asking it to throw one of them away; a dissolve has nothing to show but
//! both at once.

/// How one clip gives way to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Crossing {
    /// A hard join, which is what a cut has always made.
    #[default]
    None,
    /// Down to a colour and back up out of it.
    Fade(Shade),
    /// Both at once, the one fading through the other.
    Dissolve,
    /// The next clip arrives as an edge travelling across the frame, the one
    /// before it standing still behind it.
    Wipe(Side),
    /// The same edge, with both pictures moving: the one before is pushed
    /// off the side the next one comes in from.
    Slide(Side),
}

impl Crossing {
    /// Whether both clips are on screen at once, which is what decides
    /// whether the output comes out shorter. See the page head.
    pub fn overlaps(self) -> bool {
        !matches!(self, Crossing::None | Crossing::Fade(_))
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Crossing::None => "none",
            Crossing::Fade(Shade::Black) => "fade-black",
            Crossing::Fade(Shade::White) => "fade-white",
            Crossing::Dissolve => "dissolve",
            Crossing::Wipe(s) => match s {
                Side::Left => "wipe-left",
                Side::Right => "wipe-right",
                Side::Top => "wipe-top",
                Side::Bottom => "wipe-bottom",
            },
            Crossing::Slide(s) => match s {
                Side::Left => "slide-left",
                Side::Right => "slide-right",
                Side::Top => "slide-top",
                Side::Bottom => "slide-bottom",
            },
        }
    }

    /// The name back again, for a project file and a command line.
    pub fn parse(s: &str) -> Option<Crossing> {
        Some(match s {
            "none" | "" => Crossing::None,
            "fade" | "fade-black" => Crossing::Fade(Shade::Black),
            "fade-white" => Crossing::Fade(Shade::White),
            "dissolve" => Crossing::Dissolve,
            "wipe-left" => Crossing::Wipe(Side::Left),
            "wipe-right" => Crossing::Wipe(Side::Right),
            "wipe-top" => Crossing::Wipe(Side::Top),
            "wipe-bottom" => Crossing::Wipe(Side::Bottom),
            "slide-left" => Crossing::Slide(Side::Left),
            "slide-right" => Crossing::Slide(Side::Right),
            "slide-top" => Crossing::Slide(Side::Top),
            "slide-bottom" => Crossing::Slide(Side::Bottom),
            _ => return None,
        })
    }
}

/// The colour a fade goes through.
///
/// Two, and not a colour picker. What a fade through a colour is *for* is a
/// moment where the programme is not on screen, and there are two of those:
/// black, which is every broadcast's own, and white, which is what a
/// photograph fades through. A third would be a decision nobody has to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shade {
    Black,
    White,
}

impl Shade {
    /// The colour as a decoder would hand it over: luma, then the two
    /// chroma planes, which are neutral for both of these.
    pub fn yuv(self, depth: u32) -> (u16, u16) {
        let top = (1u32 << depth) - 1;
        let neutral = (1u32 << (depth - 1)) as u16;
        match self {
            // Studio black and studio white, not 0 and full scale: the
            // pictures around this were coded in the same range, and a frame
            // that goes past where they can reach is a frame that flashes.
            Shade::Black => ((16 << (depth - 8)) as u16, neutral),
            Shade::White => (((235 << (depth - 8)) as u32).min(top) as u16, neutral),
        }
    }
}

/// Which side of the frame the next clip comes in from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

/// How fast the crossing runs at each point of itself.
///
/// The names are the reference tool's, which are the ones a person who has
/// used one of these before will look for. What each of them is, is the
/// standard curve of that name; there is nothing invented here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Curve {
    /// A straight line: the crossing runs at one speed throughout.
    #[default]
    None,
    Back,
    Bounce,
    Circle,
    Elastic,
    Exponential,
    Power,
    Sine,
    Quadratic,
    Cubic,
    Quartic,
    Quintic,
}

/// Which end of the crossing the curve is applied at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Slow at the start.
    #[default]
    In,
    /// Slow at the end.
    Out,
    /// Slow at both ends.
    InOut,
    /// Fast at both ends.
    OutIn,
}

/// A curve and the end it is applied at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Easing {
    pub curve: Curve,
    pub mode: Mode,
}

impl Easing {
    /// How far through the crossing is *shown* when `t` of it has passed.
    ///
    /// Both in and out are 0 at 0 and 1 at 1. Some of these leave the
    /// interval in the middle -- Back overshoots by design, Elastic rings
    /// past both ends -- and that is left alone rather than clamped here:
    /// what reads it decides what a value outside means, and for a
    /// dissolve's mix it is clamped where it is used. A wipe's edge is not
    /// clamped at all, because an edge a little off the frame is simply an
    /// edge off the frame.
    pub fn at(&self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self.mode {
            Mode::In => ease_in(self.curve, t),
            Mode::Out => 1.0 - ease_in(self.curve, 1.0 - t),
            Mode::InOut => {
                if t < 0.5 {
                    ease_in(self.curve, t * 2.0) / 2.0
                } else {
                    1.0 - ease_in(self.curve, (1.0 - t) * 2.0) / 2.0
                }
            }
            Mode::OutIn => {
                if t < 0.5 {
                    (1.0 - ease_in(self.curve, 1.0 - t * 2.0)) / 2.0
                } else {
                    0.5 + ease_in(self.curve, (t - 0.5) * 2.0) / 2.0
                }
            }
        }
    }

    pub fn parse(curve: &str, mode: &str) -> Easing {
        let curve = match curve {
            "back" => Curve::Back,
            "bounce" => Curve::Bounce,
            "circle" => Curve::Circle,
            "elastic" => Curve::Elastic,
            "exponential" => Curve::Exponential,
            "power" => Curve::Power,
            "sine" => Curve::Sine,
            "quadratic" => Curve::Quadratic,
            "cubic" => Curve::Cubic,
            "quartic" => Curve::Quartic,
            "quintic" => Curve::Quintic,
            _ => Curve::None,
        };
        let mode = match mode {
            "out" => Mode::Out,
            "in-out" | "inout" => Mode::InOut,
            "out-in" | "outin" => Mode::OutIn,
            _ => Mode::In,
        };
        Easing { curve, mode }
    }
}

/// The slow-at-the-start half of each curve. Everything else is this one
/// read backwards or twice over; see [`Easing::at`].
fn ease_in(curve: Curve, t: f64) -> f64 {
    use std::f64::consts::PI;
    match curve {
        Curve::None => t,
        Curve::Quadratic => t * t,
        // Power is the name the reference tool gives the cubic that most
        // interfaces call "ease": the same curve as Cubic, kept apart
        // because the list a person is choosing from has both names on it.
        Curve::Cubic | Curve::Power => t * t * t,
        Curve::Quartic => t * t * t * t,
        Curve::Quintic => t * t * t * t * t,
        Curve::Sine => 1.0 - ((t * PI) / 2.0).cos(),
        Curve::Circle => 1.0 - (1.0 - t * t).max(0.0).sqrt(),
        Curve::Exponential => {
            if t <= 0.0 {
                0.0
            } else {
                (2.0f64).powf(10.0 * t - 10.0)
            }
        }
        // The two that leave the interval. The constants are the ones every
        // easing library uses, and they are what makes the overshoot the
        // size it is.
        Curve::Back => {
            const C: f64 = 1.701_58;
            (C + 1.0) * t * t * t - C * t * t
        }
        Curve::Elastic => {
            if t <= 0.0 {
                return 0.0;
            }
            if t >= 1.0 {
                return 1.0;
            }
            let c = (2.0 * PI) / 3.0;
            -(2.0f64).powf(10.0 * t - 10.0) * ((t * 10.0 - 10.75) * c).sin()
        }
        // Bounce is written as the out form and read backwards, which is how
        // it is defined: a ball dropped bounces at the *end*.
        Curve::Bounce => 1.0 - bounce_out(1.0 - t),
    }
}

fn bounce_out(t: f64) -> f64 {
    const N: f64 = 7.5625;
    const D: f64 = 2.75;
    if t < 1.0 / D {
        N * t * t
    } else if t < 2.0 / D {
        let t = t - 1.5 / D;
        N * t * t + 0.75
    } else if t < 2.5 / D {
        let t = t - 2.25 / D;
        N * t * t + 0.9375
    } else {
        let t = t - 2.625 / D;
        N * t * t + 0.984_375
    }
}

/// A transition, as one setting.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    pub kind: Crossing,
    /// How long it runs, in seconds.
    pub seconds: f64,
    pub easing: Easing,
    /// A still image drawn over the crossing: a title, a card, a logo.
    ///
    /// **Over the crossing and nowhere else**, which is what makes it
    /// something this program can offer at all. Drawing a picture over a
    /// programme means re-encoding every frame it covers, and a smart
    /// renderer that quietly re-encoded an hour because somebody put a
    /// caption on it would not be one. The frames a transition covers are
    /// being written afresh already, so an image over them costs the
    /// composite and nothing else.
    ///
    /// It comes up and goes down with the crossing's own ends -- see
    /// [`SHOW_IN`] -- so that it is never cut on or off.
    pub overlay: Option<String>,
}

impl Default for Transition {
    fn default() -> Self {
        Transition {
            kind: Crossing::None,
            seconds: 1.0,
            easing: Easing::default(),
            overlay: None,
        }
    }
}

/// What share of a crossing the overlay spends coming up, and the same again
/// going down.
///
/// A quarter at each end, so the middle half of the crossing has it at full
/// strength. Anything shorter is a card that appears; anything longer never
/// reaches full and the middle of the crossing is the only part it is
/// properly visible in.
pub const SHOW_IN: f64 = 0.25;

/// How solid the overlay is at `progress` through the whole crossing.
pub fn showing(progress: f64) -> f64 {
    let p = progress.clamp(0.0, 1.0);
    (p / SHOW_IN).min((1.0 - p) / SHOW_IN).clamp(0.0, 1.0)
}

/// The longest a transition may run.
///
/// The reference tool's own ceiling, and there is no reason to differ: what
/// is past it is not a transition between two programmes, it is a third
/// programme made of the two.
pub const LONGEST: f64 = 30.0;

impl Transition {
    /// Whether this one does anything at all.
    pub fn happens(&self) -> bool {
        self.kind != Crossing::None && self.seconds > 0.0
    }

    /// How much of the clip *before* the join it takes, and how much of the
    /// clip after.
    ///
    /// A fade takes half from each and the output keeps its length; every
    /// other kind takes the whole of it from both, and the output comes out
    /// that much shorter. See the page head.
    pub fn takes(&self) -> (f64, f64) {
        if !self.happens() {
            return (0.0, 0.0);
        }
        let d = self.seconds.clamp(0.0, LONGEST);
        if self.kind.overlaps() {
            (d, d)
        } else {
            (d / 2.0, d / 2.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every curve and every mode starts at nought and arrives at one. A
    /// transition that did not would show a step at one of its ends, which
    /// is the one thing a transition is there to avoid.
    #[test]
    fn every_easing_runs_from_end_to_end() {
        let curves = [
            Curve::None,
            Curve::Back,
            Curve::Bounce,
            Curve::Circle,
            Curve::Elastic,
            Curve::Exponential,
            Curve::Power,
            Curve::Sine,
            Curve::Quadratic,
            Curve::Cubic,
            Curve::Quartic,
            Curve::Quintic,
        ];
        for curve in curves {
            for mode in [Mode::In, Mode::Out, Mode::InOut, Mode::OutIn] {
                let e = Easing { curve, mode };
                assert!(
                    e.at(0.0).abs() < 1e-6,
                    "{curve:?}/{mode:?} starts at {}",
                    e.at(0.0)
                );
                assert!(
                    (e.at(1.0) - 1.0).abs() < 1e-6,
                    "{curve:?}/{mode:?} ends at {}",
                    e.at(1.0)
                );
            }
        }
    }

    /// The straight line is the one that has to be exactly what it says, in
    /// every mode: it is the default, and anything else would mean a
    /// transition nobody asked to be shaped was shaped.
    #[test]
    fn no_curve_is_a_straight_line() {
        for mode in [Mode::In, Mode::Out, Mode::InOut, Mode::OutIn] {
            let e = Easing {
                curve: Curve::None,
                mode,
            };
            for k in 0..=10 {
                let t = k as f64 / 10.0;
                assert!((e.at(t) - t).abs() < 1e-9, "{mode:?} at {t} is {}", e.at(t));
            }
        }
    }

    /// The curves that do not overshoot never leave the interval, which is
    /// what lets a mix use them without clamping.
    #[test]
    fn the_quiet_curves_stay_inside() {
        for curve in [
            Curve::Circle,
            Curve::Exponential,
            Curve::Sine,
            Curve::Quadratic,
            Curve::Cubic,
            Curve::Quartic,
            Curve::Quintic,
            Curve::Bounce,
        ] {
            for mode in [Mode::In, Mode::Out, Mode::InOut, Mode::OutIn] {
                let e = Easing { curve, mode };
                for k in 0..=100 {
                    let v = e.at(k as f64 / 100.0);
                    assert!((-1e-9..=1.0 + 1e-9).contains(&v), "{curve:?}/{mode:?}: {v}");
                }
            }
        }
    }

    /// The two timings, which are what the kind decides. A fade takes half
    /// from each side and the file keeps its length; a dissolve takes the
    /// whole of it from both and the file loses that much.
    #[test]
    fn a_fade_and_a_dissolve_take_differently() {
        let fade = Transition {
            kind: Crossing::Fade(Shade::Black),
            seconds: 2.0,
            easing: Easing::default(),
            overlay: None,
        };
        assert_eq!(fade.takes(), (1.0, 1.0));
        let dissolve = Transition {
            kind: Crossing::Dissolve,
            ..fade.clone()
        };
        assert_eq!(dissolve.takes(), (2.0, 2.0));
        assert_eq!(Transition::default().takes(), (0.0, 0.0));
    }

    /// The overlay is never cut on or off: it is at nothing at both ends of
    /// the crossing and at full strength across its middle.
    #[test]
    fn an_overlay_comes_up_and_goes_down() {
        assert_eq!(showing(0.0), 0.0);
        assert_eq!(showing(1.0), 0.0);
        assert_eq!(showing(0.5), 1.0);
        assert_eq!(showing(SHOW_IN), 1.0);
        assert_eq!(showing(1.0 - SHOW_IN), 1.0);
        assert!((showing(SHOW_IN / 2.0) - 0.5).abs() < 1e-9);
    }

    /// Every name a project file can hold reads back as the thing it names.
    #[test]
    fn every_name_round_trips() {
        for kind in [
            Crossing::None,
            Crossing::Fade(Shade::Black),
            Crossing::Fade(Shade::White),
            Crossing::Dissolve,
            Crossing::Wipe(Side::Left),
            Crossing::Wipe(Side::Right),
            Crossing::Wipe(Side::Top),
            Crossing::Wipe(Side::Bottom),
            Crossing::Slide(Side::Left),
            Crossing::Slide(Side::Right),
            Crossing::Slide(Side::Top),
            Crossing::Slide(Side::Bottom),
        ] {
            assert_eq!(Crossing::parse(kind.as_str()), Some(kind));
        }
    }
}
