// What the two windows say, in the language the machine is set to.
//
// The program was written in Japanese throughout -- the wording follows
// TMPGEnc MPEG Smart Renderer 6, which is where the shape of these screens
// comes from -- and the strings stayed where they were used. That is fine
// until the same sentence has to exist twice, at which point the place it is
// written down has to stop being the place it is printed.
//
// So: every line either window shows lives in the catalogue below, under a
// name, and is fetched with `t`. Nothing else about the code changes -- the
// callers still build their sentences where they built them, they merely ask
// for the words rather than holding them.
//
// The language is settled once, synchronously, before anything is drawn:
// what the user chose in 環境設定 if they chose anything, and otherwise what
// the machine is set to. That matters more than it sounds -- an answer that
// arrived a tick later would mean every window painting in Japanese first
// and correcting itself, which is exactly the flicker a preference is
// supposed to spare you. The one asynchronous part is a second opinion on
// what the machine is set to (`os_locale`), because the webview's own idea
// of it is not to be trusted on every platform; it only ever confirms.

/// The languages there are. Japanese first: it is what the program was
/// written in, and what every fallback lands on.
export const LANGS = ["ja", "en"];

const CATALOG = {
  ja: {
    // --- shared ---------------------------------------------------------
    "sep": "　／　",
    "dur.h": "{h}時間 ",
    "dur.m": "{m}分 ",
    "dur.s": "{s}秒",
    "cm.how.captions": "字幕リセット {n} 箇所",
    "cm.how.logo": "ロゴ＋無音",
    "cm.how.silence": "無音のみ（ロゴなし）",
    "cm.found": "{how}: {n} ブロック / 合計 {total}",
    "cm.none": "{how}: CM らしい区間は見つかりませんでした",

    // --- the window furniture -------------------------------------------
    "tab.input": "入力設定",
    "tab.outset": "出力設定",
    "tab.out": "出力",
    "ui.menu.title": "メニュー",
    "menu.open": "プロジェクトを開く…",
    "menu.save": "プロジェクトを保存",
    "menu.saveAs": "名前を付けて保存…",
    "menu.prefs": "環境設定…",
    "menu.about": "SmartCut について",

    // --- プロジェクト -----------------------------------------------------
    "project.untitled": "無題",
    "project.windowTitle": "{mark}{name} — SmartCut",
    "project.saved": "プロジェクトを保存しました: {name}",
    "project.opened": "プロジェクトを開きました: {name}（クリップ {n} 本）",
    "project.nothingToSave": "一覧が空です。保存するものがありません",
    "project.cannotOpen": "プロジェクトを開けません: {name}（{e}）",
    "project.wrongFormat":
      "{name} は SmartCut のプロジェクトではないか、新しい版で作られています",
    "project.replaceTitle": "プロジェクトを開く",
    "project.replaceBody":
      "現在の一覧と編集内容は置き換えられます。保存していない作業は失われます。続けますか？",
    "project.quitTitle": "SmartCut を終了",
    "project.quitBody":
      "保存していない作業があります。保存せずに終了しますか？",
    "project.quitOk": "終了する",
    "project.quitCancel": "キャンセル",

    // --- 環境設定 --------------------------------------------------------
    "prefs.title": "環境設定",
    "prefs.language": "表示言語:",
    "prefs.lang.auto": "自動（OS の設定に従う）",
    "prefs.lang.ja": "日本語",
    "prefs.lang.en": "English（英語）",
    "prefs.langNote":
      "「自動」は OS の言語設定に従います。変更はすぐに反映され、次回起動時も引き継がれます。",
    "prefs.close": "閉じる",

    // --- バージョン情報 ----------------------------------------------------
    "about.title": "バージョン情報",
    "about.version": "バージョン {v}",
    "about.tagline":
      "スマートレンダリング対応の動画カットツール。カット点にかかる部分だけを再エンコードし、"
      + "残りはビット単位でそのままコピーします。",
    "about.engineLbl": "エンジン:",
    "about.libavLbl": "FFmpeg ライブラリ:",
    "about.libavLicenseLbl": "FFmpeg ライセンス:",
    "about.platformLbl": "実行環境:",
    "about.licenseLbl": "ライセンス:",
    "about.repoLbl": "リポジトリ:",
    "about.libav": "libavformat {f}　／　libavcodec {c}　／　libavutil {u}",
    "about.unknown": "不明",
    "about.close": "閉じる",

    // --- 入力設定画面 ----------------------------------------------------
    "input.total": "クリップ合計数: {n}　合計時間: {t}",
    "input.totalPending": "（未解析 {n} 本を除く）",
    "input.dropHint.title": "クリップ（映像ファイル）を追加してください",
    "input.dropHint.body":
      "「ファイルを追加」で選ぶか、ここへドラッグ＆ドロップしてください。<br />読み込んだ順にシーク用インデックスを作ります。",
    "input.dropHint.keys":
      "ダブルクリックで編集　／　ドラッグで並べ替え　／　Ctrl+A 全選択　／　Ctrl+D 選択した動画の CM 検出　／　Delete 削除",
    "side.fileInput": "ファイル入力",
    "side.addFiles": "＋　ファイルを追加",
    "side.clipEdit": "クリップ編集",
    "side.editClip": "✂　カット編集",
    "side.duplicate": "⧉　クリップを複製",
    "side.detect": "CM を検出",
    "side.stopBatch": "解析を中止",
    "side.resumeBatch": "解析を再開",
    "side.other": "その他",
    "side.moveUp": "上に移動",
    "side.moveDown": "下に移動",
    "side.selectAll": "全選択",
    "side.removeClip": "クリップ削除",
    "side.removeAll": "全削除",
    "props.head": "クイックプロパティ",
    "props.none": "クリップが選択されていません",
    "props.many": "{n} 個のクリップを選択中",
    "props.queued": "{name}\n解析待ちです",
    "props.error": "{name}\n{error}",
    "props.body":
      "クリップ名:　{name}{copy}\n{path}\n映像:　{codec}, {w}x{h}, {fps} fps, {flags}\n" +
      "音声:　{audio}\n長さ:　{len} ({frames} フレーム)　無劣化点 {points} 個{unusable}" +
      "\nシーン {scenes} 箇所　索引 {index}{cm}",
    "props.copyOf": "（同じ録画の {n} 本目）",
    "props.unusable": "（うち {n} 個は開始に使えません）",
    "props.cm": "\nCM:　{note}",
    "media.interlaced": "インターレース (TFF)",
    "media.progressive": "プログレッシブ",
    "media.pulldown": "2:3プルダウン",
    "media.audioYes": "あり",
    "media.audioNo": "なし",

    // --- the list's rows -------------------------------------------------
    "list.copyLabel": "{name}（{n}）",
    "list.kill": "この行を一覧から外す",
    "list.cannotRead": "{clip} は読み込めませんでした",
    "list.cannotOpenEditor": "編集画面を開けません: {e}",
    "list.andMore": " ほか {n} 件",
    "list.unsupported": "対応していない形式のため無視しました: {names}",
    "list.stopping": "中止しています…",
    "list.stopped": "解析を止めました。残りは「解析を再開」で続けられます",
    "dialog.video": "動画",
    "dialog.disc": "BDAV ディスクイメージ",
    "dialog.project": "SmartCut プロジェクト",
    "queue.indexing": "シーク用インデックスを作成中: {clip}",
    "queue.detecting": "CM を検出中: {clip}",
    "row.sub":
      "{len} ({frames} フレーム)　00:00:00.00-{end}　{w}x{h}　{fps} fps　{codec}{audio}",
    "row.noAudio": "　音声なし",
    "row.cmRunning": "CM 検出中 {pct}% — {phase}",
    "row.cmQueued": "CM 検出 待機中",
    "row.cmNote": "CM: {note}",
    "row.cuts": "カット {n} 箇所 — 出力 {kept}",
    "row.keyframes": "キーフレーム {n}",
    "badge.smart": "Smart",
    "badge.error": "エラー",
    "badge.indexing": "解析中",
    "badge.editing": "編集中",
    "badge.queued": "解析待ち",
    "badge.cm": "CM {n}",
    "badge.cmNone": "CM なし",
    "ptext.running": "{phase} {pct}%",
    "ptext.cm": "CM 検出",
    "phase.queued": "待機中",
    "phase.reading": "読み込み中",
    "phase.detecting": "検出中",
    "phase.stopped": "中止しました",
    "phase.indexReused": "前回の索引を再利用",
    "phase.indexBuilt": "索引 {s} 秒",
    "cm.previous": "{note}（前回の検出）",
    "cm.failed": "検出できません: {e}",

    // --- 出力設定画面 ----------------------------------------------------
    "outset.bar": "ファイル出力",
    "outset.barNote": "ここでの設定は一覧のすべてのクリップに使われます",
    "outset.formatHead": "設定出力フォーマット",
    "outset.clipPick": "クリップ選択:",
    "outset.noClips": "クリップがありません",
    "outset.noReady": "解析の済んだクリップがありません",
    "outset.fileHead": "ファイル設定",
    "outset.outDir": "出力先フォルダー(F):",
    "outset.sameAsInput": "（入力ファイルと同じ場所）",
    "outset.browse": "参照",
    "outset.prefix": "ファイル名の接頭辞:",
    "outset.container": "コンテナタイプ(Y):",
    "outset.audio": "音声(A):",
    "outset.audioChannels": "音声チャンネル(C):",
    "outset.audioBitrate": "音声ビットレート:",
    "outset.keyframeSidecar": "キーフレーム情報を別ファイル (.keyframe) で出力する",
    "container.same": "入力と同じ",
    "container.ts": "MPEG-2 トランスポート (.ts)",
    "audio.smart": "スマートレンダリング（既定）",
    "audio.copy": "そのままコピー",
    "audio.reencode": "すべて再エンコード",
    "audio.smart.short": "スマートレンダリング",
    "audio.copy.short": "そのままコピー",
    "audio.reencode.short": "再エンコード",
    "channels.same": "入力と同じ",
    "channels.mono": "1ch（モノラル）",
    "channels.stereo": "2ch（ステレオ）",
    "channels.surround51": "5.1ch（6ch）",
    "bitrate.auto": "おまかせ",
    "outset.audioLine": "{mode}（{detail}）",
    "outset.format":
      "映像:　{codec}, {w}x{h}, {fps} fps, {scan}\n音声:　{audio}\n" +
      "区間:　{keeps} 区間 / 出力 {kept}（元 {dur}、カット {cuts} 箇所）\n出力先:　{out}{side}",
    "outset.interlaced": "インターレース (トップフィールド優先)",
    "outset.sidecar": "\n　　　　{path}",

    // --- 出力画面 --------------------------------------------------------
    "out.idle": "出力するクリップを一覧に追加してください",
    "out.run": "出力開始",
    "out.abort": "出力中止",
    "out.stateLbl": "状況:",
    "out.reencodeLbl": "再エンコード:",
    "out.progressLbl": "進捗:",
    "out.waiting": "待機中",
    "out.elapsed": "経過 {t}",
    "out.left": "残り {t}",
    "out.leftUnknown": "残り --:--:--",
    "out.looking": "調べています…",
    "out.lookingAt": "{clip} — 調べています…",
    "out.cannotLook": "調べられません: {e}",
    "out.losslessNote": "{clip} — なし。全編を無劣化コピーします",
    "out.losslessStage": "再エンコードなし — 全編を無劣化コピー",
    "out.audioReencoded": "（音声は再エンコードします）",
    "out.audioDownmixed": "（音声は {from} → {to} へダウンミックスして再エンコードします）",
    "out.audioUpmixed": "（音声は {from} → {to} へ広げて再エンコードします）",
    "out.shots": "{clip} — {n} 箇所 / {frames} フレーム（ほかはバイト単位でコピー）",
    "out.ovlKind": "再エンコード {i} / {n}",
    "out.ovlNote": "{n} フレーム",
    "out.aborting": "中止します（いま書き出しているクリップは最後まで書き終えます）",
    "out.skipped": "中止",
    "out.sameName": "入力と同じ名前になります",
    "out.writing": "\"{name}\" を出力中: 映像を無劣化出力しています…",
    "out.done": "完了{extra}",
    "out.doneKeyframes": " / キーフレーム {n} 個",
    "out.summary": "{done} / {all} 本を出力しました{failed}{aborted}　経過 {elapsed}",
    "out.summaryFailed": "　失敗 {n} 本",
    "out.summaryAborted": "　（中止されました）",

    // --- the cut editor --------------------------------------------------
    "editor.title": "カット編集",
    "editor.windowTitle": "カット編集 — {clip}",
    "editor.loading": "読み込み中…",
    "editor.analysing": "解析中…",
    "editor.detectCm": "CM を検出",
    "editor.detectCm.title": "CM らしい区間を探してキーフレームを立てる (Ctrl+D)",
    "editor.detecting": "検出中…（映像も読みます）",
    "editor.detectingPct": "検出中 {pct}%",
    "editor.keyframes": "キーフレーム",
    "editor.keyCount": "{n} 個",
    "editor.keyframes.empty":
      "まだありません。「⚑ キーフレーム」でいまの位置を登録できます。CM を検出すると、本編と CM それぞれの先頭が自動で並びます。",
    "editor.keyframes.kill": "このキーフレームを消す",
    "editor.searching": "サーチ中",
    "editor.searchKind": "サーチ",
    "editor.selection": "選択 {a} - {b} : {len}",
    "editor.selectionNone": "選択 —",
    "editor.counter": "{at} / {all}   {t}",
    "editor.frameKind": "{kind} フレーム",
    "editor.frameKindPoint": "{kind} フレーム — 無劣化点",
    "editor.previewFailed": "プレビュー失敗: {e}",
    "editor.stripHint":
      "クリックで移動／<b>右ドラッグ</b>で前後にサーチ（右へ＝送り・左へ＝戻し）／中クリックで次のシーン／ホイールで 1 フレーム送り（Shift で GOP 単位）／Space で再生",
    "editor.stripShow": "表示",
    "strip.gop3": "GOP・3 秒",
    "strip.gop6": "GOP・6 秒",
    "strip.gop30": "GOP・30 秒",
    "strip.gop180": "GOP・3 分",
    "strip.frame": "フレーム",
    "t.addKey": "⚑ キーフレーム",
    "t.addKey.title": "いまのフレームをキーフレームに登録 (K)",
    "t.prevScene": "⇤ シーン",
    "t.prevScene.title": "前のシーンの変わり目へ (Shift+S)",
    "t.nextScene": "シーン ⇥",
    "t.nextScene.title": "次のシーンの変わり目へ (S)",
    "t.play": "▶ 再生",
    "t.stop": "■ 停止",
    "t.play.title": "ここから再生 (Space)",
    "t.goStart": "先頭へ",
    "t.prevKf": "前の無劣化点へ",
    "t.stepBack": "1 フレーム戻る (←) ─ 押しっぱなしで連続",
    "t.gotoIn": "選択の開始位置へ移動",
    "t.setIn": "ここを選択の開始に (I)",
    "t.cut": "✂ カット",
    "t.cutRange": "いまの選択を出力から取り除く",
    "t.setOut": "ここを選択の終わりに (O)",
    "t.gotoOut": "選択の終わり位置へ移動",
    "t.stepFwd": "1 フレーム進む (→) ─ 押しっぱなしで連続",
    "t.nextKf": "次の無劣化点へ",
    "t.goEnd": "末尾へ",
    "t.cutOutside": "外側をカット",
    "t.cutOutside.title": "選択の外側をすべて取り除く",
    "t.snap": "無劣化点へ吸着",
    "t.snap.title": "選択の両端をいちばん近い無劣化点へ寄せる",
    "t.undo": "↺ 取消",
    "t.undo.title": "直前のカットを取り消す",
    "t.clearAll": "全消去",
    "t.clearAll.title": "カットとキーフレームをすべて消す",
    "editor.ok": "OK",
    "editor.cancel": "キャンセル",

    // --- トラック -------------------------------------------------------
    "tracks.button": "トラック",
    "tracks.button.title": "この録画のどのストリームを書き出すか選ぶ",
    "tracks.title": "書き出すトラック",
    "tracks.note":
      "外したトラックは出力に入りません。字幕は TS に書き出すときだけ残せます。",
    "tracks.audio": "音声",
    "tracks.caption": "字幕",
    "tracks.main": "主音声",
    "tracks.pid": "PID 0x{pid}",
    "tracks.dropped": "持ち出せません: {what}   PID 0x{pid}",
    "tracks.superimpose": "文字スーパー",
    "tracks.data": "データ放送",
    "tracks.droppedNote":
      "これらは切ったタイムラインに載せられません。文字スーパーはパケットに時刻がなく、データ放送はストリームではなく断片の繰り返しだからです。",
    "tracks.tablesNote":
      "番組情報（EIT）・放送局名・放送時刻は、TS に書き出すときはそのまま引き継ぎます。トラックではないので、ここには並びません。",
    "tracks.none": "この録画には選べるトラックがありません。",
    "tracks.failed": "トラックを読めませんでした: {e}",
    "tracks.close": "閉じる",
    "tracks.summary": "{n} 本を除外",
    "editor.playFailed": "再生: {e}",
    "editor.sceneFailed": "シーン検索: {e}",
    "editor.audioFailed": "音声再生エラー: {e}",
    "editor.openFailed": "開けません: {e}",
    "editor.info":
      "無劣化点: {points}   {w}x{h}   {fps} fps   {flags}   {audio}   {codec}{unusable}",
    "editor.infoAudioYes": "音声あり",
    "editor.infoAudioNo": "音声なし",
    "editor.infoUnusable": "   （うち {n} 個は開始に使えません）",
    "editor.hoverWarming": "準備中",
    "editor.hoverScene": "シーン",
    "warm.start": "準備中 0%",
    "warm.progress": "{phase}準備中 {pct}%",
    "warm.failed": "準備: {e}",
    "warm.proxy": "プロキシ {w}x{h} {mb}MB（{how}{s}秒）",
    "warm.proxyReused": "前回のを再利用 ",
    "warm.proxyBuilt": "作成 ",
    "warm.noProxy": "プロキシなし（{note}）",
    "warm.index": "シーク用インデックス {mb}MB{how}",
    "warm.indexReused": "（前回のを再利用）",
    "warm.indexBuilt": "（作成 {s}秒）",
    "warm.noIndex": "シーク用インデックスは保存できず",
    "warm.thumbs": "サムネイル {n} 枚 {gap}s 間隔",
    "warm.scenes": "シーン {n} 箇所",
    "plan.openFile": "ファイルを開いてください",
    "plan.allCut": "すべてカットされています",
    "plan.text":
      "出力 {total}（{ranges} 区間、カット {cuts} 箇所）— 無劣化コピー {copied}s ({pct})" +
      " / 再エンコード {reencoded}s",
    "plan.lossless": "映像 完全無劣化",
    "plan.reencoded": "再エンコード {n} フレーム",
    "plan.segCopy": "コピー　　",
    "plan.segEncode": "再エンコード",
    "plan.failed": "計画できません: {e}",
    "keyframes.readFailed": "キーフレームを読めません: {e}",
    "keyframes.read": "キーフレーム {n} 個を {file} から読み込みました",
    "keyframes.chapters": "ディスクのチャプター {n} 個をキーフレームにしました",
  },

  en: {
    // --- shared ---------------------------------------------------------
    "sep": "  /  ",
    "dur.h": "{h}h ",
    "dur.m": "{m}m ",
    "dur.s": "{s}s",
    "cm.how.captions": "{n} caption resets",
    "cm.how.logo": "logo + silence",
    "cm.how.silence": "silence only (no logo)",
    "cm.found": "{how}: {n} blocks / {total} in total",
    "cm.none": "{how}: nothing that looks like a commercial",

    // --- the window furniture -------------------------------------------
    "tab.input": "Input",
    "tab.outset": "Output settings",
    "tab.out": "Export",
    "ui.menu.title": "Menu",
    "menu.open": "Open project…",
    "menu.save": "Save project",
    "menu.saveAs": "Save project as…",
    "menu.prefs": "Preferences…",
    "menu.about": "About SmartCut",

    // --- projects ---------------------------------------------------------
    "project.untitled": "Untitled",
    "project.windowTitle": "{mark}{name} — SmartCut",
    "project.saved": "Project saved: {name}",
    "project.opened": "Project opened: {name} ({n} clips)",
    "project.nothingToSave": "The list is empty — there is nothing to save",
    "project.cannotOpen": "Cannot open the project: {name} ({e})",
    "project.wrongFormat":
      "{name} is not a SmartCut project, or was written by a later version",
    "project.replaceTitle": "Open project",
    "project.replaceBody":
      "The list and everything cut in it will be replaced. Any work you have not saved will be lost. Continue?",
    "project.quitTitle": "Quit SmartCut",
    "project.quitBody": "There is work here that has not been saved. Quit without saving it?",
    "project.quitOk": "Quit",
    "project.quitCancel": "Cancel",

    // --- preferences -----------------------------------------------------
    "prefs.title": "Preferences",
    "prefs.language": "Language:",
    "prefs.lang.auto": "Automatic (follow the system)",
    "prefs.lang.ja": "日本語 (Japanese)",
    "prefs.lang.en": "English",
    "prefs.langNote":
      "“Automatic” follows the language the machine is set to. A change takes effect at once and is remembered for next time.",
    "prefs.close": "Close",

    // --- about -----------------------------------------------------------
    "about.title": "About SmartCut",
    "about.version": "Version {v}",
    "about.tagline":
      "A cutter that re-encodes only the frames a cut lands among, and copies "
      + "everything else through bit for bit.",
    "about.engineLbl": "Engine:",
    "about.libavLbl": "FFmpeg libraries:",
    "about.libavLicenseLbl": "FFmpeg licence:",
    "about.platformLbl": "Platform:",
    "about.licenseLbl": "Licence:",
    "about.repoLbl": "Repository:",
    "about.libav": "libavformat {f} / libavcodec {c} / libavutil {u}",
    "about.unknown": "unknown",
    "about.close": "Close",

    // --- input screen ----------------------------------------------------
    "input.total": "Clips: {n}   Total length: {t}",
    "input.totalPending": " ({n} not yet read)",
    "input.dropHint.title": "Add clips — the recordings you want to cut",
    "input.dropHint.body":
      "Pick them with “Add files”, or drag and drop them here.<br />Seek indexes are built in the order they arrive.",
    "input.dropHint.keys":
      "Double-click to edit  /  drag to reorder  /  Ctrl+A select all  /  Ctrl+D detect commercials  /  Delete to remove",
    "side.fileInput": "Files",
    "side.addFiles": "＋　Add files",
    "side.clipEdit": "Clip",
    "side.editClip": "✂　Cut editor",
    "side.duplicate": "⧉　Duplicate clip",
    "side.detect": "Detect commercials",
    "side.stopBatch": "Stop analysis",
    "side.resumeBatch": "Resume analysis",
    "side.other": "Other",
    "side.moveUp": "Move up",
    "side.moveDown": "Move down",
    "side.selectAll": "Select all",
    "side.removeClip": "Remove clip",
    "side.removeAll": "Remove all",
    "props.head": "Quick properties",
    "props.none": "No clip selected",
    "props.many": "{n} clips selected",
    "props.queued": "{name}\nWaiting to be read",
    "props.error": "{name}\n{error}",
    "props.body":
      "Clip:  {name}{copy}\n{path}\nVideo:  {codec}, {w}x{h}, {fps} fps, {flags}\n" +
      "Audio:  {audio}\nLength:  {len} ({frames} frames)   {points} lossless points{unusable}" +
      "\n{scenes} scenes   index {index}{cm}",
    "props.copyOf": " (copy {n} of this recording)",
    "props.unusable": " ({n} of them cannot start a cut)",
    "props.cm": "\nCommercials:  {note}",
    "media.interlaced": "interlaced (TFF)",
    "media.progressive": "progressive",
    "media.pulldown": "2:3 pulldown",
    "media.audioYes": "yes",
    "media.audioNo": "none",

    // --- the list's rows -------------------------------------------------
    "list.copyLabel": "{name} ({n})",
    "list.kill": "Take this row out of the list",
    "list.cannotRead": "{clip} could not be read",
    "list.cannotOpenEditor": "Cannot open the cut editor: {e}",
    "list.andMore": " and {n} more",
    "list.unsupported": "Ignored, not a supported format: {names}",
    "list.stopping": "Stopping…",
    "list.stopped": "Analysis stopped. “Resume analysis” picks up the rest",
    "dialog.video": "Video",
    "dialog.disc": "BDAV disc image",
    "dialog.project": "SmartCut project",
    "queue.indexing": "Building seek index: {clip}",
    "queue.detecting": "Detecting commercials: {clip}",
    "row.sub":
      "{len} ({frames} frames)   00:00:00.00-{end}   {w}x{h}   {fps} fps   {codec}{audio}",
    "row.noAudio": "   no audio",
    "row.cmRunning": "Detecting commercials {pct}% — {phase}",
    "row.cmQueued": "Commercial detection queued",
    "row.cmNote": "Commercials: {note}",
    "row.cuts": "{n} cuts — {kept} out",
    "row.keyframes": "{n} keyframes",
    "badge.smart": "Smart",
    "badge.error": "Error",
    "badge.indexing": "Reading",
    "badge.editing": "Editing",
    "badge.queued": "Queued",
    "badge.cm": "CM {n}",
    "badge.cmNone": "No CM",
    "ptext.running": "{phase} {pct}%",
    "ptext.cm": "Detecting",
    "phase.queued": "Queued",
    "phase.reading": "Reading",
    "phase.detecting": "Detecting",
    "phase.stopped": "Stopped",
    "phase.indexReused": "Index from an earlier run",
    "phase.indexBuilt": "Indexed in {s}s",
    "cm.previous": "{note} (from an earlier run)",
    "cm.failed": "Cannot detect: {e}",

    // --- output settings screen ------------------------------------------
    "outset.bar": "File output",
    "outset.barNote": "These settings are used for every clip in the list",
    "outset.formatHead": "Output format",
    "outset.clipPick": "Clip:",
    "outset.noClips": "No clips",
    "outset.noReady": "No clip has been read yet",
    "outset.fileHead": "File settings",
    "outset.outDir": "Output folder (F):",
    "outset.sameAsInput": "(the same folder as the input)",
    "outset.browse": "Browse",
    "outset.prefix": "Filename prefix:",
    "outset.container": "Container (Y):",
    "outset.audio": "Audio (A):",
    "outset.audioChannels": "Audio channels (C):",
    "outset.audioBitrate": "Audio bitrate:",
    "outset.keyframeSidecar": "Write the keyframes to a separate .keyframe file",
    "container.same": "Same as the input",
    "container.ts": "MPEG-2 transport (.ts)",
    "audio.smart": "Smart rendering (default)",
    "audio.copy": "Copy through",
    "audio.reencode": "Re-encode everything",
    "audio.smart.short": "smart rendering",
    "audio.copy.short": "copied through",
    "audio.reencode.short": "re-encoded",
    "channels.same": "Same as the input",
    "channels.mono": "1ch (mono)",
    "channels.stereo": "2ch (stereo)",
    "channels.surround51": "5.1ch (6 channels)",
    "bitrate.auto": "Leave it to the engine",
    "outset.audioLine": "{mode} ({detail})",
    "outset.format":
      "Video:  {codec}, {w}x{h}, {fps} fps, {scan}\nAudio:  {audio}\n" +
      "Ranges:  {keeps} kept / {kept} out (of {dur}, {cuts} cuts)\nWritten to:  {out}{side}",
    "outset.interlaced": "interlaced (top field first)",
    "outset.sidecar": "\n             {path}",

    // --- export screen ---------------------------------------------------
    "out.idle": "Add clips to the list to export them",
    "out.run": "Start export",
    "out.abort": "Stop export",
    "out.stateLbl": "Status:",
    "out.reencodeLbl": "Re-encoded:",
    "out.progressLbl": "Progress:",
    "out.waiting": "Waiting",
    "out.elapsed": "Elapsed {t}",
    "out.left": "Left {t}",
    "out.leftUnknown": "Left --:--:--",
    "out.looking": "Working it out…",
    "out.lookingAt": "{clip} — working it out…",
    "out.cannotLook": "Cannot work it out: {e}",
    "out.losslessNote": "{clip} — none. The whole clip is copied losslessly",
    "out.losslessStage": "Nothing re-encoded — the whole clip is copied losslessly",
    "out.audioReencoded": "(the audio is re-encoded)",
    "out.audioDownmixed": "(the audio is downmixed {from} → {to} and re-encoded)",
    "out.audioUpmixed": "(the audio is spread {from} → {to} and re-encoded)",
    "out.shots": "{clip} — {n} places / {frames} frames (everything else is copied byte for byte)",
    "out.ovlKind": "Re-encode {i} of {n}",
    "out.ovlNote": "{n} frames",
    "out.aborting": "Stopping (the clip being written now is finished first)",
    "out.skipped": "Stopped",
    "out.sameName": "This would overwrite the input",
    "out.writing": "Writing \"{name}\": copying the video losslessly…",
    "out.done": "Done{extra}",
    "out.doneKeyframes": " / {n} keyframes",
    "out.summary": "{done} of {all} written{failed}{aborted}   elapsed {elapsed}",
    "out.summaryFailed": "   {n} failed",
    "out.summaryAborted": "   (stopped)",

    // --- the cut editor --------------------------------------------------
    "editor.title": "Cut editor",
    "editor.windowTitle": "Cut editor — {clip}",
    "editor.loading": "Loading…",
    "editor.analysing": "Reading…",
    "editor.detectCm": "Detect commercials",
    "editor.detectCm.title": "Look for commercials and mark them with keyframes (Ctrl+D)",
    "editor.detecting": "Detecting… (the video is read too)",
    "editor.detectingPct": "Detecting {pct}%",
    "editor.keyframes": "Keyframes",
    "editor.keyCount": "{n}",
    "editor.keyframes.empty":
      "None yet. “⚑ Keyframe” marks wherever you are. Detecting commercials lines up the start of each break and of each part of the programme.",
    "editor.keyframes.kill": "Remove this keyframe",
    "editor.searching": "Searching",
    "editor.searchKind": "Search",
    "editor.selection": "Selection {a} - {b} : {len}",
    "editor.selectionNone": "Selection —",
    "editor.counter": "{at} / {all}   {t}",
    "editor.frameKind": "{kind} frame",
    "editor.frameKindPoint": "{kind} frame — lossless point",
    "editor.previewFailed": "Preview failed: {e}",
    "editor.stripHint":
      "Click to move  /  <b>right-drag</b> to search back and forth (right = forwards, left = back)  /  middle-click for the next scene  /  wheel steps a frame (Shift for a GOP)  /  Space plays",
    "editor.stripShow": "View",
    "strip.gop3": "GOP · 3 s",
    "strip.gop6": "GOP · 6 s",
    "strip.gop30": "GOP · 30 s",
    "strip.gop180": "GOP · 3 min",
    "strip.frame": "Frame",
    "t.addKey": "⚑ Keyframe",
    "t.addKey.title": "Mark the frame you are on as a keyframe (K)",
    "t.prevScene": "⇤ Scene",
    "t.prevScene.title": "To the previous scene change (Shift+S)",
    "t.nextScene": "Scene ⇥",
    "t.nextScene.title": "To the next scene change (S)",
    "t.play": "▶ Play",
    "t.stop": "■ Stop",
    "t.play.title": "Play from here (Space)",
    "t.goStart": "To the start",
    "t.prevKf": "To the previous lossless point",
    "t.stepBack": "Back one frame (←) ─ hold to repeat",
    "t.gotoIn": "Go to the start of the selection",
    "t.setIn": "Start the selection here (I)",
    "t.cut": "✂ Cut",
    "t.cutRange": "Take the selection out of the output",
    "t.setOut": "End the selection here (O)",
    "t.gotoOut": "Go to the end of the selection",
    "t.stepFwd": "Forward one frame (→) ─ hold to repeat",
    "t.nextKf": "To the next lossless point",
    "t.goEnd": "To the end",
    "t.cutOutside": "Cut outside",
    "t.cutOutside.title": "Take out everything outside the selection",
    "t.snap": "Snap to lossless",
    "t.snap.title": "Move both ends of the selection to the nearest lossless point",
    "t.undo": "↺ Undo",
    "t.undo.title": "Undo the last cut",
    "t.clearAll": "Clear all",
    "t.clearAll.title": "Remove every cut and every keyframe",
    "editor.ok": "OK",
    "editor.cancel": "Cancel",

    // --- tracks ---------------------------------------------------------
    "tracks.button": "Tracks",
    "tracks.button.title": "Choose which of this recording's streams are written",
    "tracks.title": "Tracks to write",
    "tracks.note":
      "A track switched off is left out of the output. Captions can only be kept when writing a .ts.",
    "tracks.audio": "Sound",
    "tracks.caption": "Captions",
    "tracks.main": "main",
    "tracks.pid": "PID 0x{pid}",
    "tracks.dropped": "not carried: {what}   PID 0x{pid}",
    "tracks.superimpose": "superimposed text",
    "tracks.data": "data broadcast",
    "tracks.droppedNote":
      "These cannot be put on a cut timeline: superimposed text arrives with no time on its packets, and a data broadcast is a carousel of sections rather than a stream.",
    "tracks.tablesNote":
      "The programme information, the station name and the broadcast clock are carried across as they are when writing a .ts. They are not tracks, so they are not listed here.",
    "tracks.none": "This recording has no tracks to choose between.",
    "tracks.failed": "Could not read the tracks: {e}",
    "tracks.close": "Close",
    "tracks.summary": "{n} left out",
    "editor.playFailed": "Playback: {e}",
    "editor.sceneFailed": "Scene search: {e}",
    "editor.audioFailed": "Audio playback error: {e}",
    "editor.openFailed": "Cannot open: {e}",
    "editor.info":
      "Lossless points: {points}   {w}x{h}   {fps} fps   {flags}   {audio}   {codec}{unusable}",
    "editor.infoAudioYes": "with audio",
    "editor.infoAudioNo": "no audio",
    "editor.infoUnusable": "   ({n} of them cannot start a cut)",
    "editor.hoverWarming": "preparing",
    "editor.hoverScene": "scene",
    "warm.start": "Preparing 0%",
    "warm.progress": "Preparing {phase} {pct}%",
    "warm.failed": "Preparing: {e}",
    "warm.proxy": "Proxy {w}x{h} {mb}MB ({how}{s}s)",
    "warm.proxyReused": "reused, ",
    "warm.proxyBuilt": "built in ",
    "warm.noProxy": "No proxy ({note})",
    "warm.index": "Seek index {mb}MB{how}",
    "warm.indexReused": " (reused)",
    "warm.indexBuilt": " (built in {s}s)",
    "warm.noIndex": "Seek index could not be saved",
    "warm.thumbs": "{n} thumbnails every {gap}s",
    "warm.scenes": "{n} scenes",
    "plan.openFile": "Open a file",
    "plan.allCut": "Everything has been cut",
    "plan.text":
      "Output {total} ({ranges} ranges, {cuts} cuts) — copied losslessly {copied}s ({pct})" +
      " / re-encoded {reencoded}s",
    "plan.lossless": "Video completely lossless",
    "plan.reencoded": "{n} frames re-encoded",
    "plan.segCopy": "copy      ",
    "plan.segEncode": "re-encode ",
    "plan.failed": "Cannot plan: {e}",
    "keyframes.readFailed": "Cannot read the keyframes: {e}",
    "keyframes.read": "Read {n} keyframes from {file}",
    "keyframes.chapters": "Read {n} chapters off the disc as keyframes",
  },
};

/// Where the choice is kept. The webview's own store rather than a file
/// through the backend: both windows are the same origin, so the editor
/// reads what the list window wrote without anything having to be passed
/// over the wire, and a preference this small is not worth a round trip on
/// every window that opens.
const PREF_KEY = "smartcut.lang";

/// What was chosen: one of `LANGS`, or `"auto"` for whatever the machine is
/// set to. Read straight out of storage, because a preference nobody has
/// expressed is `"auto"` and that is also what a store that cannot be read
/// should come to.
export function preference() {
  try {
    const v = localStorage.getItem(PREF_KEY);
    return v === "auto" || LANGS.includes(v) ? v : "auto";
  } catch {
    return "auto";
  }
}

/// Turn anything that names a locale -- "ja", "ja-JP", "ja_JP.UTF-8",
/// "en-GB" -- into one of `LANGS`, or nothing when it names neither.
function langOf(tag) {
  if (!tag) return null;
  const base = String(tag).toLowerCase().replace(/[_.].*$/, "").split("-")[0];
  return LANGS.includes(base) ? base : null;
}

/// What the machine is set to, as far as the webview knows.
///
/// `navigator.languages` first, in order, so a machine set to Japanese with
/// English second lands on Japanese. Anything the program has no words for
/// is passed over rather than settled on -- a machine set to German should
/// come out in English, not in whatever the first entry happened to be.
function fromNavigator() {
  const tags = navigator.languages && navigator.languages.length
    ? navigator.languages
    : [navigator.language];
  for (const tag of tags) {
    const l = langOf(tag);
    if (l) return l;
  }
  return null;
}

/// Japanese unless something says otherwise: it is the language the program
/// was written in, and every string is guaranteed to exist in it.
let lang = preference() === "auto" ? fromNavigator() || "ja" : preference();

export const currentLang = () => lang;

/// Fill `{name}` in a catalogue line from `vars`.
///
/// A name with nothing to put in it is left standing rather than blanked,
/// because a line printing `{clip}` is a bug you can see and read, and a line
/// that quietly lost half its sentence is one you cannot.
function fill(text, vars) {
  if (!vars) return text;
  return text.replace(/\{(\w+)\}/g, (whole, name) =>
    name in vars ? String(vars[name]) : whole
  );
}

/// One line, in the language in force.
///
/// Falls through to Japanese for anything the other catalogue is missing --
/// a half-translated line is worth more than a key printed on screen -- and
/// to the key itself if it is in neither, which is what a typo looks like.
export function t(key, vars) {
  const text = CATALOG[lang]?.[key] ?? CATALOG.ja[key] ?? key;
  return fill(text, vars);
}

/// Everything the two documents say for themselves.
///
/// `data-i18n` is the element's text, `data-i18n-html` its markup (for the
/// two or three lines that carry a `<br>` or a `<b>`), `data-i18n-title` its
/// tooltip, `data-i18n-aria` the name a screen reader gives it and
/// `data-i18n-ph` an input's placeholder. An element may carry more than one
/// of them; a button with a tooltip carries two.
export function applyStatic(root = document) {
  for (const node of root.querySelectorAll("[data-i18n]")) {
    node.textContent = t(node.dataset.i18n);
  }
  for (const node of root.querySelectorAll("[data-i18n-html]")) {
    node.innerHTML = t(node.dataset.i18nHtml);
  }
  for (const node of root.querySelectorAll("[data-i18n-title]")) {
    node.title = t(node.dataset.i18nTitle);
  }
  for (const node of root.querySelectorAll("[data-i18n-aria]")) {
    node.setAttribute("aria-label", t(node.dataset.i18nAria));
  }
  for (const node of root.querySelectorAll("[data-i18n-ph]")) {
    node.placeholder = t(node.dataset.i18nPh);
  }
}

/// Told whenever the language changes, so that whatever a window has drawn
/// out of `t` can be drawn again. The static markup is not their business --
/// `applyStatic` has already run by the time these are called.
const listeners = [];
export const onLangChange = (fn) => listeners.push(fn);

/// Put a language in force. `which` is a language or `"auto"`; `remember`
/// says whether this is the user's choice or merely this window catching up
/// with it.
export function setLang(which, remember = true) {
  const next = which === "auto" ? fromNavigator() || "ja" : which;
  if (remember) {
    try {
      localStorage.setItem(PREF_KEY, which);
    } catch {
      // A store that will not take it still leaves this session in the
      // language asked for, which is the half of it that was visible.
    }
  }
  if (next === lang) return false;
  lang = next;
  applyStatic();
  for (const fn of listeners) fn(lang);
  return true;
}

/// Tell the backend, whose own messages -- the ones that come back as errors
/// and as the phases under a progress bar -- are written on that side.
///
/// Awaited by the callers that have something to start afterwards: a pass
/// that began before this landed would report its phases in the language the
/// backend had guessed rather than the one in force.
export async function tellBackend(invoke) {
  if (!invoke) return;
  try {
    await invoke("set_lang", { lang });
  } catch (e) {
    // An older backend without the command. The frontend is still in the
    // right language; only the backend's own sentences are not.
    void e;
  }
}

/// Ask the backend what the machine is set to, and follow it if it disagrees.
///
/// The webview's `navigator.language` is the answer used to paint the first
/// frame because it is there synchronously, but it is not the same answer on
/// every platform -- WebKitGTK's comes from the process locale, WebView2's
/// from the browser's preferred languages, and the two need not agree with
/// what the desktop is actually set to. So the backend, which can read the
/// environment directly, gets the last word -- and only ever while the
/// preference is "auto", because a user who chose a language did not ask the
/// machine's opinion.
export async function confirmWithOs(invoke) {
  if (!invoke || preference() !== "auto") return false;
  let locale;
  try {
    locale = await invoke("os_locale");
  } catch {
    return false;
  }
  const said = langOf(locale);
  if (!said || said === lang) return false;
  return setLang(said, false);
}
