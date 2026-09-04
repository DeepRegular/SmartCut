//! What a recording is named, and how that name is opened.
//!
//! Everything in this program is keyed on one string: the list holds it, the
//! seek index and the proxy are cached against it, the output is named beside
//! it, and the demuxer is handed it. That was true when every recording was a
//! file, and it is worth keeping true now that one can be a clip inside a
//! disc image.
//!
//! So a recording inside an image is named as though the image were a
//! directory:
//!
//! ```text
//! /rec/Anime.iso/BDAV/STREAM/00001.m2ts
//! ```
//!
//! Nothing in that path is invented -- the image really does hold a `BDAV`
//! directory with that file in it -- and the one thing that is unusual about
//! it, that `/rec/Anime.iso` is a file rather than a directory, is exactly
//! what this module notices. Splitting there gives three answers at once:
//!
//!   * the **URL** to open, which for a clip inside an image is libavformat's
//!     `subfile` protocol pointed at the bytes the clip occupies -- the
//!     demuxer reads them as a stream and never learns there is a filesystem
//!     around them;
//!   * the **file** to ask the operating system about, which is the image, so
//!     that a cache keyed on size and modification time still has something
//!     to weigh;
//!   * the **range** those bytes occupy, for the passes that read the
//!     transport stream themselves rather than through libavformat.
//!
//! A path that is simply a file comes back unchanged, which is the case that
//! has to cost nothing.

use crate::udf;
use anyhow::{anyhow, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// A stretch of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub at: u64,
    pub len: u64,
}

#[derive(Debug, Clone)]
pub struct Input {
    /// The name the user, the list and every cache know it by.
    pub spec: String,
    /// What libavformat is given.
    pub url: String,
    /// The file on disk that holds the bytes: the recording itself, or the
    /// image it is inside.
    pub file: PathBuf,
    /// Which bytes of that file, when it is not all of them.
    pub range: Option<Range>,
}

impl Input {
    /// Work out how to open what this names.
    ///
    /// A name that is neither a file nor a path into an image is handed on as
    /// it stands: libavformat opens more than files, and a URL it understands
    /// and this does not is not this module's business to reject.
    pub fn parse(spec: &str) -> Result<Input> {
        let path = Path::new(spec);
        if path.is_file() {
            return Ok(Input::plain(spec));
        }
        let Some((image, inside)) = split_at_image(path) else {
            return Ok(Input::plain(spec));
        };

        let img = udf::Image::open(&image)?;
        let entry = img
            .find(&inside)
            .ok_or_else(|| anyhow!("{inside} is not on {}", image.display()))?
            .clone();
        let range = entry.contiguous().ok_or_else(|| {
            anyhow!(
                "{inside} is written in pieces on {}, which cannot be read in place",
                image.display()
            )
        })?;
        // A file inside an image is a range of the image, and this is the
        // protocol that says so. The inner name is given as `file:` because
        // the option list ends at the first colon: a Windows path would
        // otherwise be read as the protocol `c`.
        let url = format!(
            "subfile,,start,{},end,{},,:file:{}",
            range.at,
            range.at + range.len,
            image.to_string_lossy()
        );
        Ok(Input {
            spec: spec.to_string(),
            url,
            file: image,
            range: Some(Range { at: range.at, len: range.len }),
        })
    }

    fn plain(spec: &str) -> Input {
        Input {
            spec: spec.to_string(),
            url: spec.to_string(),
            file: PathBuf::from(spec),
            range: None,
        }
    }

    /// Whether the bytes are inside something larger.
    pub fn nested(&self) -> bool {
        self.range.is_some()
    }

    /// Open the bytes for reading directly, as the passes over the transport
    /// stream's own tables do.
    ///
    /// What comes back is positioned and bounded: offset zero is the start of
    /// the recording whether or not there is an image around it, and reading
    /// past the end of it stops, rather than running on into the next clip.
    pub fn open(&self) -> Result<Reader> {
        let file = File::open(&self.file)
            .with_context(|| format!("cannot open {}", self.file.display()))?;
        let range = match self.range {
            Some(r) => r,
            None => Range { at: 0, len: file.metadata()?.len() },
        };
        Ok(Reader { file, range, pos: 0 })
    }

    /// How long the recording is in bytes.
    pub fn bytes(&self) -> Result<u64> {
        Ok(match self.range {
            Some(r) => r.len,
            None => std::fs::metadata(&self.file)?.len(),
        })
    }
}

/// A window onto a file, counted from the start of the window.
pub struct Reader {
    file: File,
    range: Range,
    pos: u64,
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.range.len.saturating_sub(self.pos);
        if left == 0 {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(left) as usize;
        self.file.seek(SeekFrom::Start(self.range.at + self.pos))?;
        let n = self.file.read(&mut buf[..want])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Reader {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::End(n) => self.range.len as i64 + n,
            SeekFrom::Current(n) => self.pos as i64 + n,
        };
        if pos < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek before the start of the recording",
            ));
        }
        self.pos = pos as u64;
        Ok(self.pos)
    }
}

/// Split a path at the file in the middle of it, if there is one.
///
/// Returns the image and the path inside it, forward-slashed the way the
/// image itself spells it.
fn split_at_image(path: &Path) -> Option<(PathBuf, String)> {
    let mut inside: Vec<String> = Vec::new();
    let mut here = path;
    while let Some(parent) = here.parent() {
        inside.push(here.file_name()?.to_string_lossy().into_owned());
        // An empty parent is the end of a relative path, and the root of an
        // absolute one is never a file.
        if parent.as_os_str().is_empty() {
            return None;
        }
        if parent.is_file() {
            inside.reverse();
            return Some((parent.to_path_buf(), inside.join("/")));
        }
        here = parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn an_ordinary_path_is_left_alone() {
        let dir = std::env::temp_dir().join("smartcut-input-plain");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("rec.ts");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"0123456789").unwrap();

        let spec = path.to_string_lossy().into_owned();
        let input = Input::parse(&spec).unwrap();
        assert_eq!(input.url, spec);
        assert!(!input.nested());
        assert_eq!(input.bytes().unwrap(), 10);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_name_that_is_nothing_yet_is_handed_on() {
        // libavformat opens more than files; this is not the place to say no.
        let input = Input::parse("http://example.invalid/a.ts").unwrap();
        assert_eq!(input.url, "http://example.invalid/a.ts");
    }

    #[test]
    fn a_window_reads_and_seeks_inside_its_own_bounds() {
        let dir = std::env::temp_dir().join("smartcut-input-window");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("image.bin");
        File::create(&path).unwrap().write_all(b"AAAAhello worldZZZZ").unwrap();

        let input = Input {
            spec: "x".into(),
            url: "x".into(),
            file: path.clone(),
            range: Some(Range { at: 4, len: 11 }),
        };
        let mut r = input.open().unwrap();
        let mut buf = [0u8; 32];
        let n = r.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello world");
        // The window ends where it ends: the Zs are another clip's.
        assert_eq!(r.read(&mut buf).unwrap(), 0);
        r.seek(SeekFrom::Start(6)).unwrap();
        let n = r.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"world");
        let _ = std::fs::remove_file(&path);
    }
}
