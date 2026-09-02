// The handful of things both windows have to say the same way.
//
// The list window and the cut editor are separate documents now, so anything
// they both print has to live somewhere neither of them owns. Two wordings
// for one answer is two things to keep in step, and the timecode under a
// clip in the list and the timecode over the picture in the editor are one
// answer.

/// HH:MM:SS.cc, the way the reference tool writes an instant.
export function fmt(t) {
  if (!isFinite(t)) return "--:--:--.--";
  const sign = t < 0 ? "-" : "";
  t = Math.abs(t);
  const p = (v) => String(v).padStart(2, "0");
  return `${sign}${p(Math.floor(t / 3600))}:${p(Math.floor((t % 3600) / 60))}:${p(
    Math.floor(t % 60)
  )}.${p(Math.floor((t % 1) * 100))}`;
}

/// HH:MM:SS, for a stretch of time being counted rather than pointed at.
export function clock(t) {
  if (!isFinite(t) || t < 0) return "--:--:--";
  const p = (v) => String(v).padStart(2, "0");
  return `${p(Math.floor(t / 3600))}:${p(Math.floor((t % 3600) / 60))}:${p(Math.floor(t % 60))}`;
}

/// "28分5秒", the way the reference tool puts a clip's length.
export function coarse(t) {
  if (!isFinite(t)) return "—";
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const s = Math.floor(t % 60);
  return (h ? `${h}時間 ` : "") + (h || m ? `${m}分 ` : "") + `${s}秒`;
}

/// How a commercial detection was arrived at and what it came to, in one line.
///
/// Both windows print this: the editor under its own button, the list under
/// the clip a batch detection was run on.
export function cmNote(res) {
  const how =
    res.resets > 0
      ? `字幕リセット ${res.resets} 箇所`
      : res.logo_found
        ? "ロゴ＋無音"
        : "無音のみ（ロゴなし）";
  return res.blocks.length
    ? `${how}: ${res.blocks.length} ブロック / 合計 ` +
        fmt(res.blocks.reduce((n, b) => n + (b.end - b.start), 0))
    : `${how}: CM らしい区間は見つかりませんでした`;
}

/// Filenames and error messages go into rows built as markup, and a recording
/// named with an ampersand is not an excuse to mangle the list.
export const esc = (t) =>
  String(t).replace(
    /[&<>"]/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]
  );

/// Take the right button away from the webview.
///
/// Its menu is a browser's -- reload, back, "open image in new tab", and on a
/// debug build an inspector -- offering to do things to a page in a program
/// that does not have pages. Neither window has a menu of its own to put
/// there instead, so the button does nothing at all; in the editor it is
/// already the search drag's, which is what wanted this first.
export function noBrowserMenu() {
  window.addEventListener("contextmenu", (ev) => ev.preventDefault());
}
