//! Entry pictures, decoded on more than one core.
//!
//! Three passes here read a recording for its entry pictures alone -- the
//! thumbnail track, the film strip's own decode, the one-read scan -- and all
//! three send one picture into a decoder, drain it, flush, and send the next.
//! The flush is not incidental: an entry picture decodes on its own, and a
//! decoder handed a *subset* of a stream's packets cannot order what comes
//! out of it, so each one has to be drained by itself. See
//! [`crate::EntryPictures`] and the note in [`crate::thumbs::build_with`].
//!
//! **That leaves the machine idle.** libavcodec threads a decode two ways: by
//! frame, which needs several pictures in flight and so is undone by the
//! flush, and by slice, which needs the picture to be coded in several. Of
//! this material only MPEG-2 is -- it carries a slice per macroblock row --
//! and that is the whole of why the pass feels different on everything else.
//! Whole recordings, on a 16-core machine:
//!
//! | material                             | as one decoder | on a pool |
//! |--------------------------------------|---------------:|----------:|
//! | MPEG-2 1920x1080 broadcast, 2 m 43 s  |  0.64 s (490/s) | 0.26 s (1208/s) |
//! | VC-1 1920x1080 Blu-ray, 1 m 2 s       |  0.72 s  (108/s) | 0.17 s  (459/s) |
//! | H.264 1440x1080 recorder, 46 min      |  43.9 s  (127/s) |  5.5 s (1011/s) |
//! | HEVC 3840x2160 recorder, 53 min       | 200.8 s   (30/s) | 34.2 s  (176/s) |
//!
//! So the pictures are spread instead: one demuxer hands each entry picture's
//! packets to whichever worker is free, every worker holds a decoder of its
//! own, and what comes back is put in order again before the caller sees it.
//! Each worker does to its picture exactly what the single decoder did to
//! every picture -- send, drain, flush -- so the answer is the same answer.
//!
//! The expensive part of what a caller *makes* of a picture goes with it:
//! `make` runs on the worker, so scaling a 4K frame down to a thumbnail and
//! encoding it are spread too, and what crosses back between the threads is
//! the small thing rather than the picture.

use anyhow::Result;
use ffmpeg_next as ff;

use crate::VideoInfo;

/// What the environment has to say about it, which is nothing unless somebody
/// is measuring: `SMARTCUT_PICTURE_CORES=1` is the pass as it was before the
/// spreading -- one decoder with the machine's threads inside it -- and is how
/// the two are compared on one machine.
fn asked() -> Option<usize> {
    std::env::var("SMARTCUT_PICTURE_CORES").ok()?.parse().ok()
}

/// How many workers to spread a pass over.
///
/// `cores` is what the caller is allowed, zero meaning the machine -- the
/// clip list hands its background passes a share while the cut editor is open,
/// and that share is what this is.
///
/// Brought down again where the pictures are large. Every worker holds a
/// decoder with frames of its own in it -- a handful of them, however
/// promptly it is flushed -- and a 4K 10-bit picture is 25 MB: on the
/// recorder's 4K material sixteen workers reached 1.4 GB resident against the
/// single decoder's 0.4 GB, which is about 90 MB a worker. So the budget
/// below is spent rather than the core count, and it costs little: the gain
/// flattens out well before sixteen anyway (161 pictures a second at eight
/// workers against 174 at sixteen), so 4K ends up with seven and everything
/// smaller with the lot.
pub fn width(cores: usize, video: &VideoInfo) -> usize {
    if let Some(n) = asked() {
        return n.max(1);
    }
    let cores = match cores {
        0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
        n => n,
    };
    /// What the frames held across the whole pool may come to.
    const BUDGET: usize = 768 << 20;
    /// How many pictures a worker's decoder turns out to be holding.
    const DEEP: usize = 4;
    // Three planes' worth, which is a 10-bit picture exactly and an 8-bit one
    // with room to spare -- and the reading the figures above were measured
    // against.
    let picture = (video.width as usize * video.height as usize * 3).max(1);
    let room = (BUDGET / (picture * DEEP)).max(1);
    cores.min(room).max(1)
}

/// Where a pool's workers stand while the rest of the machine is busy.
///
/// The two passes that open a pool want opposite things of it. See
/// [`crate::nice`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Standing {
    /// In front, with everything else: somebody is watching this pool fill,
    /// and a picture that arrives late is a picture that arrives wrong.
    Front,
    /// Behind everything else: this pool is reading a recording nobody is
    /// looking at, and it is welcome to whatever the machine has spare.
    Behind,
}

/// One entry picture's packets, and where it came in the file.
type Job = (usize, Vec<ff::Packet>);

/// A run of workers, each decoding one entry picture at a time.
///
/// Jobs go in with [`Self::push`] in the order the file holds them and come
/// back out of [`Self::ready`] and [`Self::drain`] in that same order, however
/// the workers happened to finish them.
pub struct Pool<T> {
    jobs: Option<std::sync::mpsc::SyncSender<Job>>,
    out: std::sync::mpsc::Receiver<(usize, Result<Vec<T>>)>,
    hands: Vec<std::thread::JoinHandle<()>>,
    /// How many jobs have gone in, which is also the next one's number.
    sent: usize,
    /// The next job the caller is owed.
    due: usize,
    /// Answers that arrived before the ones in front of them.
    early: std::collections::BTreeMap<usize, Result<Vec<T>>>,
}

impl<T: Send + 'static> Pool<T> {
    /// Open a pool of `workers` decoders over one stream.
    ///
    /// `standing` is where those workers queue for the machine against
    /// everything else running on it -- see [`Standing`].
    ///
    /// `make` is handed each picture on the worker that decoded it, with the
    /// picture's own presentation time in rebased seconds -- so it is the
    /// place to put whatever the caller wants done to a picture rather than
    /// to a frame it has to carry home first.
    pub fn new(
        params: ff::codec::Parameters,
        video: &VideoInfo,
        start_time: f64,
        workers: usize,
        standing: Standing,
        make: impl Fn(f64, &ff::frame::Video) -> Result<T> + Send + Sync + 'static,
    ) -> Result<Self> {
        crate::init()?;
        let workers = workers.max(1);
        // Room for a job apiece and one waiting, so a worker never stands
        // idle while the demuxer is between packets, and no more than that:
        // a job in the channel is a picture's packets held in memory.
        let (jobs_tx, jobs_rx) = std::sync::mpsc::sync_channel::<Job>(workers * 2);
        let jobs_rx = std::sync::Arc::new(std::sync::Mutex::new(jobs_rx));
        let (out_tx, out) = std::sync::mpsc::channel();
        let make = std::sync::Arc::new(make);
        let time_base = video.time_base;
        // All of it to one decoder where there is only one worker -- a
        // machine with a core or two is the case the spreading cannot help,
        // and MPEG-2 still threads inside a picture there.
        let each = if workers == 1 { 0 } else { 1 };
        let mut hands = Vec::with_capacity(workers);
        for _ in 0..workers {
            let params = params.clone();
            let rx = jobs_rx.clone();
            let tx = out_tx.clone();
            let make = make.clone();
            hands.push(std::thread::spawn(move || {
                // Before the decoder, because it is the decoding this is
                // about -- and on the worker itself, because on Windows a
                // thread is not born where the thread that made it stands.
                if standing == Standing::Behind {
                    crate::nice::behind();
                }
                let mut decoder = match crate::video_decoder_with(params, each) {
                    Ok(d) => d,
                    // Nothing can be decoded at all. The jobs are answered
                    // with nothing rather than left unanswered, or the caller
                    // waits for them for ever.
                    Err(_) => {
                        while let Ok((i, _)) = rx.lock().unwrap().recv() {
                            if tx.send((i, Ok(Vec::new()))).is_err() {
                                return;
                            }
                        }
                        return;
                    }
                };
                let mut frame = ff::frame::Video::empty();
                loop {
                    // Held only while one job is taken, never while it is
                    // decoded: the lock is the queue, not the work.
                    let job = rx.lock().unwrap().recv();
                    let Ok((i, packets)) = job else { return };
                    // A panic in `make` would take this worker down without
                    // answering job `i`, and `drain` would wait for it while
                    // the other workers, still alive, kept the channel open.
                    // It is answered as the failure it is instead.
                    let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let mut made = Vec::new();
                        let mut err = None;
                        for p in &packets {
                            if decoder.send_packet(p).is_err() {
                                break;
                            }
                        }
                        // Drained and flushed for this picture alone, which
                        // is what makes one picture a job at all.
                        let _ = decoder.send_eof();
                        while decoder.receive_frame(&mut frame).is_ok() {
                            let Some(pts) = frame.pts() else { continue };
                            match make(pts as f64 * time_base - start_time, &frame) {
                                Ok(t) => made.push(t),
                                Err(e) => {
                                    err = Some(e);
                                    break;
                                }
                            }
                        }
                        decoder.flush();
                        err.map_or(Ok(made), Err)
                    }));
                    let Ok(answer) = answer else {
                        let _ = tx.send((i, Err(anyhow::anyhow!("a decoding worker failed"))));
                        return;
                    };
                    if tx.send((i, answer)).is_err() {
                        return;
                    }
                }
            }));
        }
        Ok(Self {
            jobs: Some(jobs_tx),
            out,
            hands,
            sent: 0,
            due: 0,
            early: std::collections::BTreeMap::new(),
        })
    }

    /// Hand over one entry picture: the key packet, and the packet behind it
    /// where the material is field coded and the picture is two.
    ///
    /// Blocks while every worker is busy and the queue behind them is full,
    /// which is what keeps a fast demuxer from reading the whole recording
    /// into memory. Answers that have arrived meanwhile are taken in first,
    /// so [`Self::ready`] has them waiting.
    pub fn push(&mut self, packets: Vec<ff::Packet>) {
        self.gather();
        let i = self.sent;
        self.sent += 1;
        if let Some(tx) = &self.jobs {
            // A closed channel means every worker is gone, which is not a
            // failure to report here: the answers already in hand are still
            // owed to the caller, and the job that could not be sent comes
            // back as nothing.
            if tx.send((i, packets)).is_err() {
                self.early.insert(i, Ok(Vec::new()));
            }
        }
    }

    /// The next picture the caller is owed, if it has come back. `None` while
    /// it is still being decoded -- the ones behind it may well have arrived,
    /// and they wait their turn.
    pub fn ready(&mut self) -> Option<Result<Vec<T>>> {
        self.gather();
        self.take_due()
    }

    /// Everything still outstanding, in order, waiting for it.
    pub fn drain(&mut self) -> Vec<Result<Vec<T>>> {
        let mut out = Vec::new();
        while self.due < self.sent {
            while self.early.contains_key(&self.due) {
                out.extend(self.take_due());
            }
            if self.due >= self.sent {
                break;
            }
            match self.out.recv() {
                Ok((i, r)) => {
                    self.early.insert(i, r);
                }
                // Every worker has gone without answering. Nothing more is
                // coming, and saying so is better than waiting for it.
                Err(_) => break,
            }
        }
        out
    }

    fn take_due(&mut self) -> Option<Result<Vec<T>>> {
        let r = self.early.remove(&self.due)?;
        self.due += 1;
        Some(r)
    }

    fn gather(&mut self) {
        while let Ok((i, r)) = self.out.try_recv() {
            self.early.insert(i, r);
        }
    }
}

impl<T> Drop for Pool<T> {
    fn drop(&mut self) {
        // The workers stop when the queue closes. Waited for rather than
        // abandoned: they hold decoders, and a pass that has been asked to
        // stop should have given the machine back by the time it says so.
        self.jobs = None;
        for hand in std::mem::take(&mut self.hands) {
            let _ = hand.join();
        }
    }
}
