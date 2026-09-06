//! Where a network share is, once the machine has already mounted it.
//!
//! A recording on a NAS is named twice: `smb://nas/rec/2026-09-01.ts` is what
//! the file manager, the phone and the sticky note say, and
//! `/run/user/1000/gvfs/smb-share:server=nas,share=rec/2026-09-01.ts` is what
//! libavformat can open. Nothing here mounts anything -- mounting is where
//! the password lives, and that belongs to the desktop's own keyring, not to
//! a cut editor. This is only the translation between the two names, so that
//! a share the machine has *already* mounted can be pasted in the form the
//! user has it written down in.
//!
//! Two mounters are looked for, because a share arrives one of two ways:
//!
//!   * **cifs**, mounted by the kernel (`mount -t cifs`, an `fstab` line),
//!     which shows up in `/proc/self/mounts` as `//host/share`;
//!   * **gvfs**, mounted by the desktop (Files → "サーバーへ接続"), which is
//!     FUSE and shows up as a directory named `smb-share:server=…,share=…`
//!     under the session's runtime directory.
//!
//! Both end in an ordinary path, which is the point: everything downstream --
//! the packet scan, the seek index keyed on path/size/mtime, the output
//! written beside the input -- goes on working without knowing a network was
//! involved. Windows needs none of this. There the UNC path *is* the path, so
//! [`local`] hands `\\host\share\file` straight back.

#[cfg(not(windows))]
use anyhow::anyhow;
use anyhow::Result;
use std::path::PathBuf;

/// A share, and the path inside it. `rest` is empty when the input named the
/// share itself, and uses forward slashes whichever spelling it came in as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub host: String,
    pub share: String,
    pub rest: String,
}

impl Share {
    /// `\\host\share\dir\file`, the spelling to hand to Windows and the one
    /// to print when saying which share is missing.
    pub fn unc(&self) -> String {
        let mut s = format!(r"\\{}\{}", self.host, self.share);
        if !self.rest.is_empty() {
            s.push('\\');
            s.push_str(&self.rest.replace('/', r"\"));
        }
        s
    }

    /// `smb://host/share/dir/file`, the spelling a file manager takes.
    pub fn url(&self) -> String {
        let mut s = format!("smb://{}/{}", self.host, self.share);
        if !self.rest.is_empty() {
            s.push('/');
            s.push_str(&self.rest);
        }
        s
    }
}

/// A share named as `smb://host/share/…` or `\\host\share\…`, or `None` for
/// anything else -- which is to say for an ordinary path, the common case,
/// left alone.
///
/// `//host/share` is deliberately *not* accepted: on Unix that is a plain
/// path, and a recording under a directory called `nas` would be mistaken for
/// a share of that name.
pub fn parse(input: &str) -> Option<Share> {
    let text = input.trim();
    // Only the URL form is percent-encoded; a UNC path with a `%` in a file
    // name means the `%`.
    let (body, encoded) = if let Some(rest) = strip_ci(text, "smb://") {
        (rest.replace('\\', "/"), true)
    } else if text.starts_with(r"\\") && !text.starts_with(r"\\?\") && !text.starts_with(r"\\.\") {
        (text[2..].replace('\\', "/"), false)
    } else {
        return None;
    };
    let take = |s: &str| if encoded { decode(s) } else { s.to_string() };
    let mut parts = body.split('/').filter(|s| !s.is_empty());
    let host = take(parts.next()?);
    let share = take(parts.next()?);
    if host.is_empty() || share.is_empty() {
        return None;
    }
    let rest = parts.map(take).collect::<Vec<_>>().join("/");
    Some(Share { host, share, rest })
}

/// Where the machine has put a share it has mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub host: String,
    pub share: String,
    pub at: PathBuf,
}

/// Every SMB share mounted right now, kernel mounts first.
///
/// Read afresh on every call rather than cached: a share can be connected
/// from the file manager while the app is sitting on the error saying it was
/// not, and the next attempt should simply work.
pub fn mounts() -> Vec<Mount> {
    #[cfg(unix)]
    {
        let mut all = kernel_mounts();
        all.extend(gvfs_mounts());
        all
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

/// The mount point of `share`, or `None` when it is not mounted. Host and
/// share names are matched case-insensitively, the way SMB itself treats
/// them.
pub fn mount_of(share: &Share) -> Option<PathBuf> {
    mounts()
        .into_iter()
        .find(|m| eq_ci(&m.host, &share.host) && eq_ci(&m.share, &share.share))
        .map(|m| m.at)
}

/// The path to open for a share -- its mount point plus what was named inside
/// it. Fails only when the share is not mounted; that the file exists is the
/// caller's question.
#[cfg(not(windows))]
pub fn local(share: &Share) -> Result<PathBuf> {
    let at = mount_of(share).ok_or_else(|| anyhow!("{} is not mounted", share.unc()))?;
    Ok(if share.rest.is_empty() { at } else { at.join(&share.rest) })
}

/// On Windows the UNC path is already the path: the redirector does what gvfs
/// does here, and does it under the name the user typed.
#[cfg(windows)]
pub fn local(share: &Share) -> Result<PathBuf> {
    Ok(PathBuf::from(share.unc()))
}

/// [`parse`] and [`local`] in one step, for callers that just want a path
/// they can open. An ordinary path passes through untouched.
pub fn resolve(input: &str) -> Result<PathBuf> {
    match parse(input) {
        Some(share) => local(&share),
        None => Ok(PathBuf::from(input)),
    }
}

#[cfg(unix)]
fn kernel_mounts() -> Vec<Mount> {
    std::fs::read_to_string("/proc/self/mounts")
        .unwrap_or_default()
        .lines()
        .filter_map(mount_line)
        .collect()
}

/// One line of `/proc/self/mounts`, kept when it is an SMB one:
/// `//nas/rec /mnt/rec cifs rw,…`.
#[cfg(any(unix, test))]
fn mount_line(line: &str) -> Option<Mount> {
    let mut fields = line.split_whitespace();
    let source = unescape(fields.next()?);
    let at = unescape(fields.next()?);
    if !matches!(fields.next()?, "cifs" | "smb3" | "smbfs" | "smbfs2") {
        return None;
    }
    let rest = source.strip_prefix("//").or_else(|| source.strip_prefix(r"\\"))?;
    let (host, share) = rest.split_once(['/', '\\'])?;
    let share = share.trim_end_matches(['/', '\\']);
    if host.is_empty() || share.is_empty() {
        return None;
    }
    Some(Mount { host: host.to_string(), share: share.to_string(), at: PathBuf::from(at) })
}

#[cfg(unix)]
fn gvfs_mounts() -> Vec<Mount> {
    let mut out = Vec::new();
    for dir in gvfs_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some((host, share)) = gvfs_share(&name) {
                out.push(Mount { host, share, at: entry.path() });
            }
        }
    }
    out
}

/// Where gvfs puts its FUSE mounts. `XDG_RUNTIME_DIR` is the answer in a
/// desktop session; the other two are for a session that did not set it and
/// for the older home-directory placement.
#[cfg(unix)]
fn gvfs_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut push = |p: PathBuf| {
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    };
    if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR") {
        push(PathBuf::from(run).join("gvfs"));
    }
    if let Some(uid) = uid() {
        push(PathBuf::from(format!("/run/user/{uid}/gvfs")));
    }
    if let Some(home) = std::env::var_os("HOME") {
        push(PathBuf::from(home).join(".gvfs"));
    }
    dirs
}

/// The user this process runs as, without reaching for libc: `/proc/self` is
/// owned by it.
#[cfg(unix)]
fn uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

/// A gvfs mount directory name -- `smb-share:server=nas,share=rec`, with
/// `domain=` and `user=` fields when the connection had them.
#[cfg(any(unix, test))]
fn gvfs_share(name: &str) -> Option<(String, String)> {
    let body = name.strip_prefix("smb-share:")?;
    let (mut host, mut share) = (None, None);
    for field in body.split(',') {
        let Some((key, value)) = field.split_once('=') else { continue };
        match key {
            "server" => host = Some(decode(value)),
            "share" => share = Some(decode(value)),
            _ => {}
        }
    }
    Some((host?, share?))
}

/// `%20` and friends. An escape that is not one is left as it stands, since
/// the alternative is losing a file name over a stray `%`.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(byte) = hex(bytes[i + 1]).zip(hex(bytes[i + 2])).map(|(h, l)| h * 16 + l) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `/proc/self/mounts` writes a space as `\040`, and the backslash that
/// starts one as `\134`. A share called `録画 2026` is not exotic.
#[cfg(any(unix, test))]
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let digits = &bytes[i + 1..i + 4];
            if digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                let value = digits.iter().fold(0u32, |n, d| n * 8 + u32::from(d - b'0'));
                if value <= 0xff {
                    out.push(value as u8);
                    i += 4;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn strip_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &text[prefix.len()..])
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.to_lowercase() == b.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn share(host: &str, name: &str, rest: &str) -> Share {
        Share { host: host.into(), share: name.into(), rest: rest.into() }
    }

    #[test]
    fn parses_both_spellings() {
        assert_eq!(parse("smb://nas/rec/a.ts"), Some(share("nas", "rec", "a.ts")));
        assert_eq!(parse(r"\\nas\rec\sub\a.ts"), Some(share("nas", "rec", "sub/a.ts")));
        assert_eq!(parse("SMB://nas/rec/"), Some(share("nas", "rec", "")));
        assert_eq!(parse("  smb://nas/rec  "), Some(share("nas", "rec", "")));
    }

    #[test]
    fn leaves_ordinary_paths_alone() {
        assert_eq!(parse("/mnt/rec/a.ts"), None);
        assert_eq!(parse("//nas/rec/a.ts"), None);
        assert_eq!(parse(r"C:\rec\a.ts"), None);
        assert_eq!(parse("smb://nas"), None);
    }

    #[test]
    fn decodes_the_url_form_only() {
        assert_eq!(parse("smb://nas/録画%202026/a%20b.ts").unwrap().rest, "a b.ts");
        assert_eq!(parse("smb://nas/rec/100%.ts").unwrap().rest, "100%.ts");
        assert_eq!(parse(r"\\nas\rec\100%20.ts").unwrap().rest, "100%20.ts");
    }

    #[test]
    fn spells_a_share_back() {
        let s = parse("smb://nas/rec/sub/a.ts").unwrap();
        assert_eq!(s.unc(), r"\\nas\rec\sub\a.ts");
        assert_eq!(s.url(), "smb://nas/rec/sub/a.ts");
        assert_eq!(parse(r"\\nas\rec").unwrap().unc(), r"\\nas\rec");
    }

    #[test]
    fn reads_kernel_mount_lines() {
        let line = r"//nas/rec\040and\040more /mnt/rec cifs rw,vers=3.1.1 0 0";
        let m = mount_line(line).unwrap();
        assert_eq!(m.host, "nas");
        assert_eq!(m.share, "rec and more");
        assert_eq!(m.at, PathBuf::from("/mnt/rec"));
        assert!(mount_line("/dev/sda1 / ext4 rw 0 0").is_none());
        assert!(mount_line("//nas/rec /mnt/rec nfs rw 0 0").is_none());
    }

    #[test]
    fn reads_gvfs_directory_names() {
        assert_eq!(
            gvfs_share("smb-share:server=nas,share=rec"),
            Some(("nas".into(), "rec".into()))
        );
        assert_eq!(
            gvfs_share("smb-share:domain=WORKGROUP,server=NAS,share=%E9%8C%B2%E7%94%BB,user=kaz"),
            Some(("NAS".into(), "録画".into()))
        );
        assert_eq!(gvfs_share("sftp:host=nas"), None);
        assert_eq!(gvfs_share("smb-share:server=nas"), None);
    }

    #[test]
    fn matches_a_mount_case_insensitively() {
        assert!(eq_ci("NAS", "nas"));
        assert!(!eq_ci("nas2", "nas"));
    }
}
