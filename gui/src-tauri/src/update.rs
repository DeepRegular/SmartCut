//! 自動更新: finding a newer release, fetching it, and putting it in place.
//!
//! **Four of the six packagings, and only those.** The Windows installer, the
//! Windows portable zip, the Linux tar.gz and the AppImage are files somebody
//! put on their machine by hand, and nothing else will ever replace them. The
//! `.deb` comes from the APT repository and the Flatpak from its remote, and
//! each of those already has an updater the system runs -- a second one in
//! here would fight it. So those two are told where their updates come from
//! and are not checked at all; nor is a build run out of a source tree.
//!
//! **What is trusted is the signature, not the server.** Each file of a
//! release carries a minisign signature (`<file>.minisig`) made with a key
//! whose public half is built into this program (`update-key.pub`). The
//! manifest that says which release is newest is not signed -- it is a few
//! lines of JSON next to the files -- so the signature's trusted comment,
//! which is covered by it, names both the version and the file:
//! `SmartCut 0.9.1 SmartCut_0.9.1_amd64.AppImage`. A manifest that pointed at
//! an older signed release, or at one packaging's file for another, finds a
//! comment that does not match and nothing is installed. The addresses are
//! built here out of the version and the name, never read from the manifest,
//! and a name is a plain file name or it is refused.
//!
//! **When the files change.** Not under a running program where that can be
//! helped:
//!
//! | packaging | fetched to | put in place |
//! |---|---|---|
//! | AppImage | beside the image | at once: the running one is a mount of the old file, which stays open |
//! | tar.gz | beside the folder | on the way out: the folder is renamed aside and the new one put in its name |
//! | zip | inside the folder | on the way out: each file renamed aside (Windows lets a running `.exe` be renamed, not written) |
//! | installer | the cache | on 再起動して更新 only: the installer runs passive, with `/R` to start the program again |
//!
//! What is renamed aside is removed at the next start, by the new program
//! (`tidy`). The installer closes every SmartCut of this user before it
//! writes -- without asking, in passive mode -- so it is not started while
//! another is open, the batch tool included.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;

/// Where the newest release says what it is. `latest` is GitHub's own
/// redirect to the release marked Latest, which a pre-release or a draft
/// never is.
const MANIFEST_URL: &str =
    "https://github.com/DeepRegular/SmartCut/releases/latest/download/update.json";
/// The files of release `v{version}` are under here.
const DOWNLOAD_BASE: &str = "https://github.com/DeepRegular/SmartCut/releases/download";
/// The release's page, for the notes.
const PAGE_BASE: &str = "https://github.com/DeepRegular/SmartCut/releases/tag";

/// Somewhere else to read the manifest and the files from, for trying the
/// updater against a server of one's own: `$SMARTCUT_UPDATE_BASE/update.json`
/// and `$SMARTCUT_UPDATE_BASE/<file>`. The signatures are checked against the
/// same built-in key, so this moves where the files come from and nothing
/// about whether they are believed.
fn test_base() -> Option<String> {
    std::env::var("SMARTCUT_UPDATE_BASE")
        .ok()
        .map(|b| b.trim_end_matches('/').to_string())
        .filter(|b| !b.is_empty())
}

fn manifest_url() -> String {
    test_base().map_or_else(|| MANIFEST_URL.to_string(), |b| format!("{b}/update.json"))
}

fn file_url(version: &str, name: &str) -> String {
    test_base().map_or_else(|| format!("{DOWNLOAD_BASE}/v{version}/{name}"), |b| format!("{b}/{name}"))
}

/// The public half of the key the releases are signed with, as minisign
/// writes it. The private half is not in the repository.
const PUBLIC_KEY: &str = include_str!("../update-key.pub");

/// Nothing a release has ever shipped is near this; a manifest that says a
/// file is bigger than it is not believed.
const LARGEST: u64 = 2 << 30;

/// How this copy of the program was put on the machine.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// The NSIS installer's: `uninstall.exe` beside the program.
    #[cfg_attr(not(windows), allow(dead_code))]
    WindowsSetup,
    /// The portable zip, unpacked into `dir`.
    #[cfg_attr(not(windows), allow(dead_code))]
    WindowsPortable { dir: PathBuf },
    /// The tar.gz, unpacked into `root` (the folder holding `smartcut`,
    /// `smartcut-cli` and `app/`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    LinuxTar { root: PathBuf },
    /// The AppImage at `image`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    AppImage { image: PathBuf },
    /// Updated by something else, or by nobody: `"deb"`, `"flatpak"`,
    /// `"other"`.
    Elsewhere(&'static str),
}

impl Kind {
    /// The key the manifest lists this packaging's file under.
    fn asset(&self) -> Option<&'static str> {
        Some(match self {
            Kind::WindowsSetup => "windows-setup",
            Kind::WindowsPortable { .. } => "windows-portable",
            Kind::LinuxTar { .. } => "linux-tar",
            Kind::AppImage { .. } => "appimage",
            Kind::Elsewhere(_) => return None,
        })
    }

    /// The folder files are fetched into, which for every packaging but the
    /// installer is on the same file system as what they replace, so that
    /// putting them in place is a rename.
    fn staging(&self, app: &tauri::AppHandle) -> Option<PathBuf> {
        use tauri::Manager;
        match self {
            Kind::WindowsSetup => app.path().app_cache_dir().ok().map(|d| d.join("update")),
            Kind::WindowsPortable { dir } => Some(dir.join(".update")),
            Kind::LinuxTar { root } => root.parent().map(|p| p.join(".smartcut-update")),
            Kind::AppImage { image } => image.parent().map(|p| p.join(".smartcut-update")),
            Kind::Elsewhere(_) => None,
        }
    }
}

/// Work out how this copy was installed. Read once: none of it changes while
/// the program runs.
pub fn kind() -> &'static Kind {
    static KIND: std::sync::OnceLock<Kind> = std::sync::OnceLock::new();
    KIND.get_or_init(detect)
}

#[cfg(windows)]
fn detect() -> Kind {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf))
    else {
        return Kind::Elsewhere("other");
    };
    if dir.join("uninstall.exe").is_file() {
        Kind::WindowsSetup
    } else if dir.join("smartcut-cli.exe").is_file() {
        // The zip carries the cutter beside the program; a build out of a
        // source tree does not.
        Kind::WindowsPortable { dir }
    } else {
        Kind::Elsewhere("other")
    }
}

#[cfg(target_os = "linux")]
fn detect() -> Kind {
    if std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists() {
        return Kind::Elsewhere("flatpak");
    }
    let Ok(exe) = std::env::current_exe() else {
        return Kind::Elsewhere("other");
    };
    if exe.starts_with("/usr") {
        return Kind::Elsewhere("deb");
    }
    // <root>/app/usr/bin/smartcut, with the two launchers in <root>. Before
    // the AppImage: the tar.gz's launcher sets `APPDIR` too.
    if let Some(root) = exe.ancestors().nth(4) {
        if exe.ends_with("app/usr/bin/smartcut")
            && root.join("smartcut").is_file()
            && root.join("smartcut-cli").is_file()
        {
            return Kind::LinuxTar { root: root.to_path_buf() };
        }
    }
    // Set by the AppImage runtime to the image itself, and `APPDIR` to where
    // it is mounted -- which this program runs out of only when it is that
    // image's. Both are handed down to everything started from inside any
    // AppImage (an editor's terminal, say), so a SmartCut started from there
    // would otherwise take the other program's image for its own, and an
    // update would be renamed over it.
    //
    // Not a tar.gz's program either, when one of its launchers is gone: its
    // `smartcut` launcher sets `APPDIR` to its own `app/` and leaves an
    // inherited `APPIMAGE` as it was, which would pass the test below. An
    // image's own mount is never laid out as `app/usr/bin`.
    if let Some(image) = std::env::var_os("APPIMAGE").map(PathBuf::from) {
        if exe.ends_with("app/usr/bin/smartcut") {
            return Kind::Elsewhere("other");
        }
        let ours = std::env::var_os("APPDIR")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .is_some_and(|d| exe.starts_with(&d) || std::fs::canonicalize(&d).is_ok_and(|d| exe.starts_with(d)));
        if ours && image.is_file() {
            return Kind::AppImage { image };
        }
    }
    Kind::Elsewhere("other")
}

#[cfg(not(any(windows, target_os = "linux")))]
fn detect() -> Kind {
    Kind::Elsewhere("other")
}

/// This program's version, as three numbers.
fn current() -> (u64, u64, u64) {
    parse_version(env!("CARGO_PKG_VERSION")).unwrap_or((0, 0, 0))
}

/// `0.9.1` as three numbers, and nothing else: a version with anything after
/// it (`-rc1`) is not one this updater offers.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.');
    let mut next = || parts.next().filter(|p| !p.is_empty() && p.len() <= 6)?.parse().ok();
    let v = (next()?, next()?, next()?);
    parts.next().is_none().then_some(v)
}

/// A name that can only be a file in the release: no folders, nothing
/// hidden, nothing a URL would read as more than a name.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Whether `name` is a file of the packaging the manifest lists it under.
/// The signature's comment names the file the manifest names, so it binds the
/// version but not the packaging: every file of a release is signed alike,
/// and without this a manifest could hand an AppImage the release's tar.gz
/// (or its installer) -- signed, checked, and renamed over the image.
fn fits(asset: &str, name: &str) -> bool {
    let tail = match asset {
        "windows-setup" => "-setup.exe",
        "windows-portable" => ".zip",
        "linux-tar" => ".tar.gz",
        "appimage" => ".AppImage",
        _ => return false,
    };
    name.len() > tail.len() && name.ends_with(tail)
}

/// The manifest: `{"version": "0.9.1", "files": {"appimage": {"name": ...,
/// "size": ...}, ...}}`. Anything else in it is ignored, so a later release
/// can say more.
#[derive(serde::Deserialize)]
struct Manifest {
    version: String,
    files: std::collections::HashMap<String, ManifestFile>,
}

#[derive(serde::Deserialize, Clone)]
struct ManifestFile {
    name: String,
    size: u64,
}

/// A newer release, for this packaging.
#[derive(Clone, Serialize)]
pub struct Found {
    pub version: String,
    pub current: String,
    pub page: String,
    pub size: u64,
    #[serde(skip)]
    name: String,
}

/// What [`status`] says about this copy, for the window to decide what to
/// offer.
#[derive(Serialize)]
pub struct Status {
    /// `windows-setup`, `windows-portable`, `linux-tar`, `appimage`, or
    /// `deb`, `flatpak`, `other` for the ones this does not update.
    kind: String,
    current: String,
    /// Where the files would go cannot be written to: a portable folder under
    /// Program Files, an AppImage in `/opt`. Said before anything is fetched.
    blocked: Option<String>,
    /// A release already fetched and waiting to be put in place.
    ready: Option<String>,
}

/// One in force at a time: what was found, what has been fetched, whether to
/// start again on the way out.
#[derive(Default)]
struct State {
    found: Option<Found>,
    /// Fetched and checked, waiting for the way out (or, for an AppImage,
    /// already in place).
    staged: Option<Staged>,
    restart: bool,
}

#[derive(Clone)]
struct Staged {
    version: String,
    /// The installer, the unpacked folder, or for an AppImage nothing left
    /// to do but start it.
    path: PathBuf,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static STOP: AtomicBool = AtomicBool::new(false);
static FETCHING: AtomicBool = AtomicBool::new(false);

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(State::default))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        // Between two reads, not for the whole file: a slow line takes
        // minutes over 200 MB and that is not a failure.
        .timeout_per_read(Some(Duration::from_secs(60)))
        .user_agent(concat!("SmartCut/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

pub fn status(app: &tauri::AppHandle) -> Status {
    let k = kind();
    let name = match k {
        Kind::Elsewhere(what) => what.to_string(),
        other => other.asset().unwrap_or("other").to_string(),
    };
    let blocked = k.staging(app).and_then(|dir| writable(&dir).err()).or_else(|| swappable(k).err());
    // Nothing waiting can be put in place there: not offered as ready at
    // every start.
    let ready = with_state(|s| s.staged.as_ref().map(|st| st.version.clone())).filter(|_| blocked.is_none());
    Status { kind: name, current: env!("CARGO_PKG_VERSION").into(), blocked, ready }
}

/// Whether files can be put in `dir` (made if it is not there), said as the
/// reason they cannot.
fn writable(dir: &Path) -> Result<(), String> {
    let fail = |e: std::io::Error| {
        trf!(
            "{} に書き込めないため、自動更新できません（{}）",
            "Cannot update automatically: {} cannot be written to ({})",
            dir.parent().unwrap_or(dir).display(),
            e
        )
    };
    // Asked every time the window or 環境設定 looks: a folder made only to
    // ask is not left beside somebody's AppImage.
    //
    // Under a name of this asking's own: a fetch asks while the window does,
    // and another SmartCut beside this one asks at its start, and with one
    // name between them the one that had made the folder removed it, or the
    // file, under the other -- whose write failed, and that copy said
    // "cannot be written to" for the rest of its session. The folder removed
    // meanwhile by another asking that made it is made again once.
    static ASKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let probe = dir.join(format!(".probe-{}-{}", std::process::id(), ASKED.fetch_add(1, Ordering::Relaxed)));
    let mut made = false;
    let mut wrote = Ok(());
    for _ in 0..2 {
        made |= !dir.is_dir();
        std::fs::create_dir_all(dir).map_err(fail)?;
        wrote = std::fs::write(&probe, b"").map_err(fail);
        if wrote.is_ok() || dir.is_dir() {
            break;
        }
    }
    let _ = std::fs::remove_file(&probe);
    if made {
        let _ = std::fs::remove_dir(dir);
    }
    wrote
}

/// Whether the tar.gz's folder itself can be swapped: a folder moved to
/// another folder needs its own write permission (its `..` changes), so one
/// made read-only refused the swap at every way out, and the window offered
/// the same update as ready at every start. Asked by making something in it.
fn swappable(k: &Kind) -> Result<(), String> {
    match k {
        Kind::LinuxTar { root } => {
            static ASKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = ASKED.fetch_add(1, Ordering::Relaxed);
            writable(&root.join(format!(".smartcut-probe-{}-{n}", std::process::id())))
        }
        _ => Ok(()),
    }
}

/// Whether `path` was last written more than `secs` ago: a fetch or an
/// unpack of another SmartCut still going on keeps its own fresh.
fn stale(path: &Path, secs: u64) -> bool {
    std::fs::symlink_metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_none_or(|age| age.as_secs() > secs)
}

/// Ask whether there is a newer release for this packaging. `None` where
/// there is not.
pub fn check() -> Result<Option<Found>, String> {
    let asset = kind().asset().ok_or_else(|| {
        tr!("この形式のパッケージは自動更新の対象外です", "This packaging is not updated from here")
            .to_string()
    })?;
    let body = agent()
        .get(&manifest_url())
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let found = read_manifest(&body, asset, current())?;
    with_state(|s| s.found = found.clone());
    Ok(found)
}

/// The manifest, read for `asset`, against version `now`.
fn read_manifest(body: &str, asset: &str, now: (u64, u64, u64)) -> Result<Option<Found>, String> {
    let manifest: Manifest = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let version = parse_version(&manifest.version)
        .ok_or_else(|| format!("version {:?}", manifest.version))?;
    if version <= now {
        return Ok(None);
    }
    // Newer, but nothing of this packaging in it: as good as no release, for
    // this copy.
    let Some(file) = manifest.files.get(asset) else {
        return Ok(None);
    };
    if !plain_name(&file.name) || !fits(asset, &file.name) || file.size == 0 || file.size > LARGEST {
        return Err(format!("file {:?}", file.name));
    }
    let v = format!("{}.{}.{}", version.0, version.1, version.2);
    Ok(Some(Found {
        page: format!("{PAGE_BASE}/v{v}"),
        current: env!("CARGO_PKG_VERSION").into(),
        size: file.size,
        name: file.name.clone(),
        version: v,
    }))
}

/// Ask a fetch under way to stop.
pub fn cancel() {
    STOP.store(true, Ordering::SeqCst);
}

/// Fetch what [`check`] found, check its signature, and make it ready to put
/// in place. `progress` hears `(done, total)` in bytes.
pub fn fetch(app: &tauri::AppHandle, progress: impl FnMut(u64, u64)) -> Result<String, String> {
    if FETCHING.swap(true, Ordering::SeqCst) {
        return Err(tr!("ダウンロードは実行中です", "Already downloading").into());
    }
    STOP.store(false, Ordering::SeqCst);
    let out = fetch_now(app, progress);
    FETCHING.store(false, Ordering::SeqCst);
    out
}

fn fetch_now(app: &tauri::AppHandle, mut progress: impl FnMut(u64, u64)) -> Result<String, String> {
    let found = with_state(|s| s.found.clone())
        .ok_or_else(|| tr!("先に更新を確認してください", "Check for updates first").to_string())?;
    if with_state(|s| s.staged.as_ref().is_some_and(|st| st.version == found.version)) {
        return Ok(found.version);
    }
    let k = kind();
    let dir = k.staging(app).ok_or("no staging folder")?;
    writable(&dir)?;
    swappable(k)?;
    let url = file_url(&found.version, &found.name);
    let file = dir.join(&found.name);

    // The signature first: it is small, and a release without one is not
    // worth 200 MB of anybody's line.
    let sig_text = agent()
        .get(&format!("{url}.minisig"))
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(4096)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let key = minisign_verify::PublicKey::decode(PUBLIC_KEY.trim()).map_err(|e| e.to_string())?;
    let sig = minisign_verify::Signature::decode(sig_text.trim()).map_err(|e| e.to_string())?;
    let expect = format!("SmartCut {} {}", found.version, found.name);
    if sig.trusted_comment() != expect {
        return Err(trf!(
            "署名の内容がファイルと一致しません: {}",
            "The signature is for something else: {}",
            sig.trusted_comment()
        ));
    }
    let mut verifier = key.verify_stream(&sig).map_err(|e| e.to_string())?;

    // This process's own: another SmartCut fetching into the same folder
    // would otherwise write the same file, and the bytes checked as they came
    // would not be the bytes on the disk.
    let part = dir.join(format!("{}.{}.part", found.name, std::process::id()));
    let mut resp = agent().get(&url).call().map_err(|e| e.to_string())?;
    let mut reader = resp.body_mut().with_config().limit(found.size + 1).reader();
    // Made here, not before the signature was fetched: until a file is in
    // it, the start of another SmartCut removes it as left empty.
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    let failed = |e: String| {
        let _ = std::fs::remove_file(&part);
        e
    };
    loop {
        if STOP.load(Ordering::SeqCst) {
            drop(out);
            return Err(failed(tr!("中止しました", "Stopped").into()));
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(failed(e.to_string())),
        };
        verifier.update(&buf[..n]);
        if let Err(e) = out.write_all(&buf[..n]) {
            return Err(failed(e.to_string()));
        }
        done += n as u64;
        progress(done, found.size);
    }
    if let Err(e) = out.sync_all() {
        return Err(failed(e.to_string()));
    }
    drop(out);
    if done != found.size {
        return Err(failed(trf!(
            "ダウンロードしたファイルの大きさが違います（{} / {} バイト）",
            "The download is the wrong size ({} of {} bytes)",
            done,
            found.size
        )));
    }
    if verifier.finalize().is_err() {
        return Err(failed(
            tr!("署名を確認できませんでした。ファイルを破棄しました", "The signature did not verify; the file was discarded")
                .into(),
        ));
    }
    std::fs::rename(&part, &file).map_err(|e| failed(e.to_string()))?;

    if matches!(k, Kind::LinuxTar { .. } | Kind::WindowsPortable { .. }) {
        // Whatever was waiting in `new` is replaced by this one.
        with_state(|s| s.staged = None);
    }
    let path = prepare(k, &dir, &file).inspect_err(|_| {
        let _ = std::fs::remove_file(&file);
    })?;
    // Which version the unpacked folder is, for a start after a way out that
    // could not put it in place (see `tidy`).
    if matches!(k, Kind::LinuxTar { .. } | Kind::WindowsPortable { .. }) {
        std::fs::write(dir.join(NEW_VERSION), &found.version).map_err(|e| e.to_string())?;
    }
    with_state(|s| s.staged = Some(Staged { version: found.version.clone(), path }));
    smartcut_core::log::line(&format!("update: {} fetched and ready", found.version));
    Ok(found.version)
}

/// Beside the unpacked `new` folder: the version it holds.
const NEW_VERSION: &str = "new.version";

/// Make the fetched `file` ready to put in place, and say what is to be
/// started on the way out.
fn prepare(k: &Kind, dir: &Path, file: &Path) -> Result<PathBuf, String> {
    match k {
        Kind::WindowsSetup => Ok(file.to_path_buf()),
        Kind::AppImage { image } => {
            // In place now: the running program is a mount of the file it was
            // started from, which keeps it open whatever its name comes to.
            // An ELF with the AppImage type-2 mark, or it does not replace
            // the program.
            let head = head_of(file);
            if !runs_here(&head) || &head[8..11] != b"AI\x02" {
                return Err("the file is not an AppImage".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| e.to_string())?;
            }
            std::fs::rename(file, image).map_err(|e| e.to_string())?;
            // Nothing else is kept there: not left lying beside the image.
            let _ = std::fs::remove_dir(dir);
            Ok(image.clone())
        }
        Kind::LinuxTar { .. } | Kind::WindowsPortable { .. } => {
            // Unpacked under a name of this process's own and only then made
            // `new`: the start of another SmartCut clears a `new` it finds
            // with no `new.version` beside it, which is what a `new` being
            // unpacked looks like.
            let unpacking = dir.join(format!("unpack-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&unpacking);
            let unpacked = if matches!(k, Kind::LinuxTar { .. }) {
                unpack_tar(file, &unpacking)
            } else {
                unpack_zip(file, &unpacking)
            };
            let _ = std::fs::remove_file(file);
            let checked = unpacked.and_then(|()| {
                let top = only_folder(&unpacking)?;
                if is_tree(k, &top) {
                    Ok(top)
                } else {
                    Err("the archive is not a SmartCut folder".to_string())
                }
            });
            let top = match checked {
                Ok(top) => top,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&unpacking);
                    return Err(e);
                }
            };
            let new = dir.join("new");
            // What was waiting there (another version) is gone with it.
            let _ = std::fs::remove_file(dir.join(NEW_VERSION));
            let _ = std::fs::remove_dir_all(&new);
            if let Err(e) = std::fs::rename(&unpacking, &new) {
                let _ = std::fs::remove_dir_all(&unpacking);
                return Err(e.to_string());
            }
            Ok(new.join(top.file_name().ok_or("no name")?))
        }
        Kind::Elsewhere(_) => Err("nothing to update".into()),
    }
}

/// Whether `top` is a whole SmartCut of packaging `k`.
fn is_tree(k: &Kind, top: &Path) -> bool {
    match k {
        Kind::LinuxTar { .. } => {
            top.join("smartcut").is_file()
                && top.join("smartcut-cli").is_file()
                && runs_here(&head_of(&top.join("app/usr/bin/smartcut")))
        }
        Kind::WindowsPortable { .. } => top.join("smartcut.exe").is_file(),
        _ => false,
    }
}

/// The first bytes of `file`, zeros where it is shorter or unreadable.
fn head_of(file: &Path) -> [u8; 20] {
    let mut head = [0u8; 20];
    if let Ok(mut f) = std::fs::File::open(file) {
        let _ = f.read(&mut head);
    }
    head
}

/// The ELF machine this build runs on. Every file of a release is signed
/// alike, so a release with a build for another processor would hand this
/// one a program that does not start.
#[cfg(target_arch = "x86_64")]
const ELF_MACHINE: Option<u16> = Some(0x3E);
#[cfg(target_arch = "aarch64")]
const ELF_MACHINE: Option<u16> = Some(0xB7);
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const ELF_MACHINE: Option<u16> = None;

/// Whether `head` starts a little-endian ELF program for this processor.
fn runs_here(head: &[u8; 20]) -> bool {
    &head[..4] == b"\x7fELF"
        && head[5] == 1
        && ELF_MACHINE.is_none_or(|m| u16::from_le_bytes([head[18], head[19]]) == m)
}

/// The one folder an archive unpacked into `dir` holds.
fn only_folder(dir: &Path) -> Result<PathBuf, String> {
    let mut found = None;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if found.replace(path).is_some() {
            return Err("the archive holds more than one folder".into());
        }
    }
    found.filter(|p| p.is_dir()).ok_or_else(|| "the archive is empty".into())
}

#[cfg(target_os = "linux")]
fn unpack_tar(file: &Path, into: &Path) -> Result<(), String> {
    let f = std::fs::File::open(file).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(f));
    archive.set_preserve_permissions(true);
    // `unpack` refuses an entry that would land outside `into`.
    archive.unpack(into).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "linux"))]
fn unpack_tar(_: &Path, _: &Path) -> Result<(), String> {
    Err("tar.gz on Linux only".into())
}

#[cfg(windows)]
fn unpack_zip(file: &Path, into: &Path) -> Result<(), String> {
    let f = std::fs::File::open(file).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    // `extract` refuses an entry whose name would land outside `into`.
    archive.extract(into).map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn unpack_zip(_: &Path, _: &Path) -> Result<(), String> {
    Err("zip on Windows only".into())
}

/// Start again into the new version on the way out, once it has been put in
/// place. Refused while another SmartCut is open where putting it in place
/// would close it.
pub fn want_restart(app: &tauri::AppHandle) -> Result<(), String> {
    if with_state(|s| s.staged.is_none()) {
        return Err(tr!("更新はまだダウンロードされていません", "The update has not been downloaded").into());
    }
    if crate::batch_live(app.clone()) {
        return Err(tr!(
            "バッチ出力ツールが動いています。ツールを閉じてから更新してください",
            "The batch tool is running. Close it before updating"
        )
        .into());
    }
    if let Kind::LinuxTar { root } = kind() {
        if tree_in_use(root) {
            return Err(tr!(
                "同じフォルダーからほかの SmartCut かコマンドライン版が動いています。閉じてから更新してください",
                "Another SmartCut or the command-line tool is running from the same folder. Close it before updating"
            )
            .into());
        }
    }
    if matches!(kind(), Kind::WindowsSetup | Kind::WindowsPortable { .. }) && others_running() {
        return Err(tr!(
            "ほかの SmartCut のウィンドウが開いています。閉じてから更新してください",
            "Another SmartCut window is open. Close it before updating"
        )
        .into());
    }
    with_state(|s| s.restart = true);
    Ok(())
}

/// The close that was to start the new version was called off.
pub fn no_restart() {
    with_state(|s| s.restart = false);
}

/// Whether a SmartCut of this user other than this one is running, as the
/// installer would find it.
#[cfg(windows)]
fn others_running() -> bool {
    use std::os::windows::process::CommandExt;
    const NO_WINDOW: u32 = 0x0800_0000;
    let user = std::env::var("USERNAME").unwrap_or_default();
    let out = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq smartcut.exe", "/FI", &format!("USERNAME eq {user}"), "/FO", "CSV", "/NH"])
        .creation_flags(NO_WINDOW)
        .output();
    match out {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout);
            text.lines().filter(|l| l.to_ascii_lowercase().contains("smartcut.exe")).count() > 1
        }
        // Not known: say so the safe way.
        Err(_) => true,
    }
}

#[cfg(not(windows))]
fn others_running() -> bool {
    false
}

/// Whether a SmartCut other than this one runs out of `root`. The tar.gz is
/// put in place by renaming its folder, and a SmartCut still running out of
/// the old one goes on starting WebKit's helper processes by the path it was
/// given -- which would then be the new version's.
///
/// The program's own binary and nothing else: WebKit's helpers are in the
/// folder too, and this one's own are still running on the way out. Counted
/// as another SmartCut, they kept the folder from ever being replaced.
#[cfg(target_os = "linux")]
fn tree_in_use(root: &Path) -> bool {
    let me = std::process::id().to_string();
    let program = root.join("app/usr/bin/smartcut");
    // The cutter too: a cut it is writing into the folder by a relative name
    // would go on into the folder put aside.
    let cutter = root.join("app/usr/bin/smartcut-cli");
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return true;
    };
    procs.flatten().any(|p| {
        let name = p.file_name();
        let name = name.to_string_lossy();
        name.bytes().all(|b| b.is_ascii_digit())
            && name != me
            && std::fs::read_link(p.path().join("exe")).is_ok_and(|exe| exe == program || exe == cutter)
    })
}

#[cfg(not(target_os = "linux"))]
fn tree_in_use(_: &Path) -> bool {
    false
}

/// On the way out: put what was fetched in place, and start the new version
/// if that was asked for. Last of what the program does.
pub fn on_exit() {
    let (staged, restart) = with_state(|s| (s.staged.take(), s.restart));
    let Some(staged) = staged else { return };
    let started = match kind() {
        Kind::WindowsSetup => {
            // Only when asked: the installer is a window of its own, and one
            // turning up after the program was simply closed would be a
            // surprise. Left in the cache for the next start to offer again.
            if !restart {
                return;
            }
            let mut run = std::process::Command::new(&staged.path);
            run.args(["/P", "/R", "/UPDATE"]);
            run.spawn().map(drop)
        }
        Kind::AppImage { image } => {
            if !restart {
                return;
            }
            let mut run = crate::desktop_command(&image.to_string_lossy());
            run.spawn().map(drop)
        }
        Kind::LinuxTar { root } => {
            // Left unpacked, for the next start to offer and the next way
            // out to put in place (see `tidy`). The folder is renamed only
            // over a whole tree: the start of another SmartCut may have
            // cleared `new` since it was made.
            let left = if tree_in_use(root) {
                Some("another SmartCut runs out of the folder; left for later".to_string())
            } else if !is_tree(kind(), &staged.path) {
                Some(format!("{} is not a whole SmartCut; not put in place", staged.path.display()))
            } else {
                swap_folder(root, &staged.path).err()
            };
            if let Some(e) = &left {
                smartcut_core::log::line(&format!("update: {e}"));
            }
            // Asked to start again: the window said it would, so the one
            // there is is started, new or not.
            if !restart {
                return;
            }
            crate::desktop_command(&root.join("smartcut").to_string_lossy()).spawn().map(drop)
        }
        Kind::WindowsPortable { dir } => {
            // Another SmartCut would go on with the old program and the new
            // libraries. Which folder it runs from is not known here, so any
            // of this user's: left for a later way out, as the tar.gz is.
            let left = if others_running() {
                Some("another SmartCut is running; left for later".to_string())
            } else if !is_tree(kind(), &staged.path) {
                Some(format!("{} is not a whole SmartCut; not put in place", staged.path.display()))
            } else {
                swap_files(dir, &staged.path).err()
            };
            if let Some(e) = &left {
                smartcut_core::log::line(&format!("update: {e}"));
            }
            if !restart {
                return;
            }
            std::process::Command::new(dir.join("smartcut.exe")).spawn().map(drop)
        }
        Kind::Elsewhere(_) => return,
    };
    if let Err(e) = started {
        smartcut_core::log::line(&format!("update: could not start the new version: {e}"));
    }
}

/// Where what is replaced goes, to be removed at the next start.
fn aside(staging: &Path) -> PathBuf {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    staging.join(format!("old-{now}-{}", std::process::id()))
}

/// The tar.gz: `root` renamed aside and `new` put in its name. Put back as
/// it was if the second rename fails. What the user kept in the folder --
/// anything at its top the release does not have, a cut written there, a
/// project -- is moved into the new folder: what is aside is removed at the
/// next start.
fn swap_folder(root: &Path, new: &Path) -> Result<(), String> {
    let staging = root.parent().ok_or("no parent")?.join(".smartcut-update");
    let old = aside(&staging);
    if exchange(root, new) {
        // One step: there is no moment with nothing in `root`'s name. The old
        // folder is now where the new one was.
        if std::fs::rename(new, &old).is_err() {
            // Still in `new`, which goes with the next start's tidy all the
            // same; the user's files are taken from there.
            keep_theirs(new, root);
            return Ok(());
        }
    } else {
        std::fs::rename(root, &old).map_err(|e| format!("{}: {e}", root.display()))?;
        if let Err(e) = std::fs::rename(new, root) {
            let _ = std::fs::rename(&old, root);
            return Err(format!("{}: {e}", new.display()));
        }
    }
    keep_theirs(&old, root);
    let _ = std::fs::remove_dir_all(staging.join("new"));
    let _ = std::fs::remove_file(staging.join(NEW_VERSION));
    Ok(())
}

/// The names at the top of the tar.gz's folder, in every release so far: the
/// release's, never the user's. A release that adds a name at the top adds it
/// here too, or its old copy is left aside at every update (kept, not lost).
const TAR_TOP: [&str; 5] = ["LICENSE", "README.txt", "app", "smartcut", "smartcut-cli"];

fn is_release_name(name: &std::ffi::OsStr) -> bool {
    TAR_TOP.iter().any(|t| name == *t)
}

/// Move what is at the top of `old` and not of `new` into `new`: the user's,
/// so never one of the release's own names.
fn keep_theirs(old: &Path, new: &Path) {
    let Ok(entries) = std::fs::read_dir(old) else { return };
    for entry in entries.flatten() {
        if is_release_name(&entry.file_name()) {
            continue;
        }
        let to = new.join(entry.file_name());
        if std::fs::symlink_metadata(&to).is_err() {
            if let Err(e) = move_kept(&entry.path(), &to) {
                smartcut_core::log::line(&format!("update: {} not kept: {e}", entry.path().display()));
            }
        }
    }
}

/// `rename`, and for a folder that is read-only -- one copied off a disc keeps
/// the disc's `r-x` -- with its write permission lent for the move: a folder
/// moved into another folder needs it (its `..` changes), and without it the
/// user's folder stayed aside, where the next start's tidy removed it.
fn move_kept(from: &Path, to: &Path) -> std::io::Result<()> {
    let refused = match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => e,
        done => return done,
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::symlink_metadata(from) else { return Err(refused) };
        let mode = meta.permissions().mode();
        if !meta.is_dir() || mode & 0o200 != 0 {
            return Err(refused);
        }
        if std::fs::set_permissions(from, std::fs::Permissions::from_mode(mode | 0o200)).is_err() {
            return Err(refused);
        }
        let moved = std::fs::rename(from, to);
        let _ = std::fs::set_permissions(if moved.is_ok() { to } else { from }, std::fs::Permissions::from_mode(mode));
        moved
    }
    #[cfg(not(unix))]
    Err(refused)
}

/// Clear `aside`, a tar.gz folder put aside by a swap: the user's things moved
/// into `root`, the release's removed, and whatever could be neither left
/// where it is -- only ever a release's own name is removed.
fn clear_aside(aside: &Path, root: &Path) {
    keep_theirs(aside, root);
    if let Ok(entries) = std::fs::read_dir(aside) {
        for entry in entries.flatten() {
            let path = entry.path();
            if is_release_name(&entry.file_name()) {
                let real_dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
                let _ = if real_dir { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            } else {
                smartcut_core::log::line(&format!("update: {} left where it is", path.display()));
            }
        }
    }
    let _ = std::fs::remove_dir(aside);
}

/// `a` and `b` swapped in one rename, where the file system can.
#[cfg(target_os = "linux")]
fn exchange(a: &Path, b: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    extern "C" {
        fn renameat2(
            olddirfd: i32,
            oldpath: *const std::ffi::c_char,
            newdirfd: i32,
            newpath: *const std::ffi::c_char,
            flags: u32,
        ) -> i32;
    }
    const AT_FDCWD: i32 = -100;
    const RENAME_EXCHANGE: u32 = 2;
    let (Ok(a), Ok(b)) = (
        std::ffi::CString::new(a.as_os_str().as_bytes()),
        std::ffi::CString::new(b.as_os_str().as_bytes()),
    ) else {
        return false;
    };
    // SAFETY: two NUL-terminated paths that live across the call.
    unsafe { renameat2(AT_FDCWD, a.as_ptr(), AT_FDCWD, b.as_ptr(), RENAME_EXCHANGE) == 0 }
}

#[cfg(not(target_os = "linux"))]
fn exchange(_: &Path, _: &Path) -> bool {
    false
}

/// The zip: each thing at the top of `new` put into `dir`, what it replaces
/// renamed aside first. Windows lets a running program and the libraries it
/// has loaded be renamed but not written. Everything moved is moved back if
/// any of it fails.
fn swap_files(dir: &Path, new: &Path) -> Result<(), String> {
    let staging = dir.join(".update");
    let old = aside(&staging);
    std::fs::create_dir_all(&old).map_err(|e| e.to_string())?;
    let mut done: Vec<(PathBuf, Option<PathBuf>, PathBuf)> = Vec::new();
    // Back into `new`, not thrown away: it stays waiting for the next way
    // out, and with a file missing from it that would put half a release
    // in place.
    let undo = |done: &[(PathBuf, Option<PathBuf>, PathBuf)]| {
        for (target, was, from) in done.iter().rev() {
            if std::fs::rename(target, from).is_err() {
                let _ = std::fs::remove_file(target);
            }
            if let Some(was) = was {
                let _ = std::fs::rename(was, target);
            }
        }
    };
    let entries = std::fs::read_dir(new).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let target = dir.join(&name);
        let mut was = None;
        if target.exists() {
            let to = old.join(&name);
            if let Err(e) = std::fs::rename(&target, &to) {
                undo(&done);
                return Err(format!("{}: {e}", target.display()));
            }
            was = Some(to);
        }
        if let Err(e) = std::fs::rename(entry.path(), &target) {
            if let Some(was) = &was {
                let _ = std::fs::rename(was, &target);
            }
            undo(&done);
            return Err(format!("{}: {e}", target.display()));
        }
        done.push((target, was, entry.path()));
    }
    let _ = std::fs::remove_dir_all(staging.join("new"));
    let _ = std::fs::remove_file(staging.join(NEW_VERSION));
    Ok(())
}

/// At the start: remove what an update renamed aside and anything a fetch
/// left half done. For the installer, offer what was fetched last time and
/// not yet installed.
pub fn tidy(app: &tauri::AppHandle) {
    let Some(dir) = kind().staging(app) else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let swaps = matches!(kind(), Kind::LinuxTar { .. } | Kind::WindowsPortable { .. });
        if name.starts_with("old-") {
            // Made only by the two that swap, so not an AppImage's to clear:
            // a tar.gz's start does it, and takes back first what was written
            // into it after the swap -- a terminal whose folder was the one
            // put aside writes `./smartcut-cli ... -o cut.ts` there.
            if let Kind::LinuxTar { root } = kind() {
                clear_aside(&path, root);
            } else if swaps {
                // The zip's: only the files the release replaced.
                let _ = std::fs::remove_dir_all(&path);
            }
        } else if name.starts_with("unpack-") {
            // Left by an unpack that never finished; one still going on is
            // younger than this.
            if stale(&path, 3600) {
                let _ = std::fs::remove_dir_all(&path);
            }
        } else if (name == "new" || name == NEW_VERSION) && !swaps {
            // The folder is shared by everything in the same parent folder:
            // a tar.gz unpacked beside this AppImage waits here for its own
            // way out.
        } else if name == "new" {
            // Unpacked by a session whose way out could not put it in place
            // -- another SmartCut was running out of the folder. Offered
            // again, as ready, if it is still newer than this one.
            let version = std::fs::read_to_string(dir.join(NEW_VERSION))
                .ok()
                .and_then(|v| parse_version(v.trim()));
            let top = only_folder(&path).ok().filter(|top| is_tree(kind(), top));
            match (version, top) {
                (Some(v), Some(top)) if v > current() => {
                    with_state(|s| {
                        s.staged = Some(Staged { version: format!("{}.{}.{}", v.0, v.1, v.2), path: top })
                    })
                }
                (v, _) => {
                    // This very version waiting in `new`: a swap cut short
                    // between its two renames (a power cut) left there the
                    // tree it replaced, with the user's things still in it.
                    // Put aside as any old tree is, not thrown away.
                    if let (Kind::LinuxTar { root }, Some(v), Ok(top)) = (kind(), v, only_folder(&path)) {
                        if v == current() {
                            let old = aside(&dir);
                            if std::fs::rename(&top, &old).is_ok() {
                                clear_aside(&old, root);
                            }
                        }
                    }
                    let _ = std::fs::remove_dir_all(&path);
                    let _ = std::fs::remove_file(dir.join(NEW_VERSION));
                }
            }
        } else if name == NEW_VERSION {
            // Read with `new`, above; gone with it when that is.
            if !dir.join("new").is_dir() {
                let _ = std::fs::remove_file(&path);
            }
        } else if name.ends_with(".part") {
            // Not one another SmartCut is still writing.
            if stale(&path, 600) {
                let _ = std::fs::remove_file(&path);
            }
        } else if matches!(kind(), Kind::WindowsSetup) && name.ends_with("-setup.exe") {
            // A version this one already is or has passed is of no more use.
            let version = name.split('_').nth(1).and_then(parse_version);
            match version {
                Some(v) if v > current() => with_state(|s| {
                    s.staged = Some(Staged {
                        version: format!("{}.{}.{}", v.0, v.1, v.2),
                        path: path.clone(),
                    })
                }),
                _ => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
    // Nothing left in it: not left lying about beside somebody's AppImage.
    let _ = std::fs::remove_dir(&dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_three_numbers() {
        assert_eq!(parse_version("0.9.1"), Some((0, 9, 1)));
        assert_eq!(parse_version("1.0.0"), Some((1, 0, 0)));
        assert_eq!(parse_version("1.0"), None);
        assert_eq!(parse_version("1.0.0-rc1"), None);
        assert_eq!(parse_version("1.0.0.0"), None);
        assert_eq!(parse_version("1..0"), None);
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
    }

    #[test]
    fn names_are_plain() {
        assert!(plain_name("SmartCut_0.9.1_amd64.AppImage"));
        assert!(plain_name("smartcut-portable-x64-0.9.1.zip"));
        assert!(!plain_name("../x"));
        assert!(!plain_name("a/b"));
        assert!(!plain_name(".hidden"));
        assert!(!plain_name("a?b"));
        assert!(!plain_name(""));
    }

    #[test]
    fn manifest_is_read_for_one_packaging() {
        let body = r#"{"version":"0.9.1","files":{
            "appimage":{"name":"SmartCut_0.9.1_amd64.AppImage","size":10},
            "linux-tar":{"name":"../evil","size":10}}}"#;
        let found = read_manifest(body, "appimage", (0, 9, 0)).unwrap().unwrap();
        assert_eq!(found.version, "0.9.1");
        assert_eq!(found.name, "SmartCut_0.9.1_amd64.AppImage");
        assert_eq!(found.page, "https://github.com/DeepRegular/SmartCut/releases/tag/v0.9.1");
        // Not newer: nothing.
        assert!(read_manifest(body, "appimage", (0, 9, 1)).unwrap().is_none());
        // Newer, but not for this packaging.
        assert!(read_manifest(body, "windows-setup", (0, 9, 0)).unwrap().is_none());
        // A name that is not a file name is refused.
        assert!(read_manifest(body, "linux-tar", (0, 9, 0)).is_err());
        // Another packaging's file, signed alike, is refused.
        let other = r#"{"version":"0.9.1","files":{
            "appimage":{"name":"SmartCut-0.9.1-linux-x86_64.tar.gz","size":10},
            "linux-tar":{"name":"SmartCut_0.9.1_amd64.AppImage","size":10},
            "windows-setup":{"name":"smartcut-portable-x64-0.9.1.zip","size":10},
            "windows-portable":{"name":"SmartCut_0.9.1_x64-setup.exe","size":10}}}"#;
        for asset in ["appimage", "linux-tar", "windows-setup", "windows-portable"] {
            assert!(read_manifest(other, asset, (0, 9, 0)).is_err(), "{asset}");
        }
        assert!(fits("linux-tar", "SmartCut-0.9.1-linux-x86_64.tar.gz"));
        assert!(fits("windows-setup", "SmartCut_0.9.1_x64-setup.exe"));
        assert!(fits("windows-portable", "smartcut-portable-x64-0.9.1.zip"));
    }

    #[test]
    fn the_built_in_key_reads() {
        assert!(minisign_verify::PublicKey::decode(PUBLIC_KEY.trim()).is_ok());
    }

    #[test]
    fn folder_swap_puts_back_on_failure() {
        let base = std::env::temp_dir().join(format!("sc-upd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("SmartCut");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("v"), "old").unwrap();
        // No new folder: the second rename fails and the old one comes back.
        assert!(swap_folder(&root, &base.join("missing")).is_err());
        assert_eq!(std::fs::read_to_string(root.join("v")).unwrap(), "old");
        let new = base.join(".smartcut-update/new/SmartCut-x");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("v"), "new").unwrap();
        std::fs::write(root.join("mine.ts"), "cut").unwrap();
        std::fs::create_dir_all(root.join("projects")).unwrap();
        std::fs::write(root.join("projects/a.scproj"), "{}").unwrap();
        swap_folder(&root, &new).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("v")).unwrap(), "new");
        // What the user kept in the folder comes along; nothing of the old
        // release does.
        assert_eq!(std::fs::read_to_string(root.join("mine.ts")).unwrap(), "cut");
        assert_eq!(std::fs::read_to_string(root.join("projects/a.scproj")).unwrap(), "{}");
        assert!(!new.exists());
        let aside: Vec<_> = std::fs::read_dir(base.join(".smartcut-update"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(aside.len() == 1 && aside[0].starts_with("old-"), "{aside:?}");
        let old = base.join(".smartcut-update").join(&aside[0]);
        assert_eq!(std::fs::read_to_string(old.join("v")).unwrap(), "old");
        assert!(!old.join("mine.ts").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn aside_keeps_every_user_thing() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("sc-upd-a-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("SmartCut");
        let old = base.join(".smartcut-update/old-1-1");
        for d in [&root, &old] {
            std::fs::create_dir_all(d.join("app/usr")).unwrap();
            std::fs::write(d.join("smartcut"), d.to_string_lossy().as_bytes()).unwrap();
        }
        // The user's: a folder copied off a disc (read-only), one whose name
        // the new folder has too, and a file.
        std::fs::create_dir_all(old.join("VIDEO_TS")).unwrap();
        std::fs::write(old.join("VIDEO_TS/VTS_01_1.VOB"), "vob").unwrap();
        std::fs::set_permissions(old.join("VIDEO_TS"), std::fs::Permissions::from_mode(0o555)).unwrap();
        std::fs::create_dir_all(old.join("docs")).unwrap();
        std::fs::write(old.join("docs/mine.txt"), "mine").unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(old.join("notes.txt"), "n").unwrap();
        clear_aside(&old, &root);
        assert_eq!(std::fs::read_to_string(root.join("VIDEO_TS/VTS_01_1.VOB")).unwrap(), "vob");
        let mode = std::fs::metadata(root.join("VIDEO_TS")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o555);
        assert_eq!(std::fs::read_to_string(root.join("notes.txt")).unwrap(), "n");
        // The release's own names are never carried over, and gone from aside.
        assert_eq!(std::fs::read_to_string(root.join("smartcut")).unwrap(), root.to_string_lossy());
        assert!(!old.join("smartcut").exists() && !old.join("app").exists());
        // What clashes is left aside, not removed.
        assert_eq!(std::fs::read_to_string(old.join("docs/mine.txt")).unwrap(), "mine");
        std::fs::set_permissions(root.join("VIDEO_TS"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn programs_for_this_processor() {
        let mut head = [0u8; 20];
        head[..4].copy_from_slice(b"\x7fELF");
        head[4] = 2;
        head[5] = 1;
        head[8..11].copy_from_slice(b"AI\x02");
        head[18] = 0x3E;
        assert_eq!(runs_here(&head), cfg!(target_arch = "x86_64"));
        head[18] = 0xB7;
        assert_eq!(runs_here(&head), cfg!(target_arch = "aarch64"));
        assert!(!runs_here(&[0u8; 20]));
    }

    #[test]
    fn file_swap_replaces_and_adds() {
        let base = std::env::temp_dir().join(format!("sc-upd-f-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("smartcut");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.dll"), "old").unwrap();
        std::fs::write(dir.join("keep.txt"), "mine").unwrap();
        let new = dir.join(".update/new/smartcut");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("a.dll"), "new").unwrap();
        std::fs::write(new.join("b.dll"), "added").unwrap();
        swap_files(&dir, &new).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.dll")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(dir.join("b.dll")).unwrap(), "added");
        assert_eq!(std::fs::read_to_string(dir.join("keep.txt")).unwrap(), "mine");
        let _ = std::fs::remove_dir_all(&base);
    }
}
