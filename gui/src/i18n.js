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
    "tab.batch": "バッチ出力",
    "ui.menu.title": "メニュー",
    "menu.new": "新規作成",
    "menu.open": "プロジェクトを開く",
    "menu.save": "プロジェクトを保存",
    "menu.saveAs": "名前を付けて保存",
    "menu.batch": "バッチ出力ツール",
    "menu.prefs": "環境設定",
    "menu.about": "SmartCut について",
    "menu.quit": "終了",

    // --- プロジェクト -----------------------------------------------------
    "project.untitled": "無題",
    "project.windowTitle": "{mark}{name} — SmartCut",
    "project.saved": "プロジェクトを保存しました: {name}",
    "project.opened": "プロジェクトを開きました: {name}（クリップ {n} 本）",
    "project.refused":
      "プロジェクトを開きました: {name}（クリップ {n} 本、{bad} 本はファイル名ではないので外しました）",
    "project.nothingToSave": "一覧が空です。保存するものがありません",
    "project.cannotOpen": "プロジェクトを開けません: {name}（{e}）",
    "project.wrongFormat":
      "{name} は SmartCut のプロジェクトではないか、より新しいバージョンで作られています",
    "project.replaceTitle": "プロジェクトを開く",
    "project.replaceBody":
      "現在の一覧と編集内容は置き換えられます。保存していない作業は失われます。続けますか？",
    "project.newTitle": "新規作成",
    "project.newBody":
      "現在の一覧と編集内容は破棄されます。保存していない作業は失われます。続けますか？",
    "project.newDone": "新しいプロジェクトを始めました",
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
    "prefs.groupView": "表示",
    "prefs.counter": "カット編集で、フレーム番号と時刻を画面に重ねる",
    "prefs.meter": "カット編集で、音声レベルメーターを表示する",
    "prefs.subs": "カット編集で、最初から字幕を表示する",
    "prefs.subsNote":
      "字幕のある録画でのみ有効です。編集中に切り替えられます。",
    "prefs.groupEdit": "カット編集",
    "prefs.pageStep": "PageUp / PageDown:",
    "prefs.pageStepShift": "Shift を押しながら:",
    "prefs.pageStepCtrl": "Ctrl を押しながら:",
    "prefs.pageStepShiftCtrl": "Shift+Ctrl を押しながら:",
    "prefs.seconds": "秒",
    "prefs.step.frame": "フレーム移動",
    "prefs.step.sec": "秒移動",
    "prefs.step.pct": "% でスクロール",
    // The same two words as the steps above, and not the same answer: those
    // are a distance to move, these are how long a stretch has to last.
    "prefs.len.frame": "フレーム",
    "prefs.len.sec": "秒",
    "prefs.pageStepNote":
      "カット編集で PageUp / PageDown を押したときの動きです。0 にすると、そのキーは効きません。" +
      "フレームと秒は 1 回押すごとの移動量です。% は移動速度で、いちばん速いスクロール（60 倍速）" +
      "に対する割合です。25 なら 15 倍速で、1 秒押しっぱなしにすると録画の 15 秒ぶん進みます。" +
      "録画の長さには左右されません。" +
      "左右キーはこの設定の対象外です（1 フレーム、Shift で 1 秒）。",
    "prefs.sidecarPriority": "複数あるときに読み込むのは:",
    "prefs.sidecar.keyframe": "キーフレーム情報（.keyframe）",
    "prefs.sidecar.trim": "AviSynth Trim（.trim.avs）",
    "prefs.sidecar.cm": "CM 検出結果（.cm.json）",
    "prefs.sidecarNote":
      "カット編集を開くとき、録画と同じ名前で同じ場所にあるファイルを読み込みます。" +
      "キーフレーム情報は位置の一覧なので、印が並ぶだけです。" +
      "Trim は残す区間そのものなので、カット済みの状態で開きます。" +
      "CM 検出結果は、検出した直後と同じ帯と印に戻ります。" +
      "見つかったのが 1 つだけなら、この設定に関わらずそれを読み込みます。" +
      "どれかを読み込んだときは、CM 検出の結果に印は置きません。帯だけ出ます。",
    "prefs.cmKeyframes": "CM を検出したら、自動でキーフレームを置く",
    "prefs.cmInserts": "番組中の短い挿入（局の ID など）も検出する",
    "prefs.cmInsertsNote":
      "CS の一部の局は、地上波で CM が入っていたところに自局の動画 ID を 2〜9 秒だけ" +
      "差し込みます。ふつうの CM ブロックより短いので既定では拾いません。" +
      "見分けているのは音で、挿入は音がいったん切れます。" +
      "ただし番組の全画面テロップも同じように隅のロゴを隠すので、" +
      "テロップの間だけ音を落とす番組では、それも拾います。" +
      "ある番組で 20 話ぶん測ったところ、出たものの 3 分の 1 ほどがテロップでした。" +
      "カット編集の「CM を検出」だけが対象で、入力一覧からの検出では拾いません。",
    "prefs.blankRun": "黒・白の区間とみなす長さ:",
    "prefs.blankRunNote":
      "これ以上続いた区間だけを扱います。既定は 3 秒です。" +
      "放送の CM の切れ目に入る黒は 2〜4 フレームなので、" +
      "既定のままでは拾いません。録画と録画のあいだのような長い黒だけが出ます。" +
      "切れ目の黒を拾いたいときは、単位をフレームにして 2 くらいにしてください。",
    "prefs.blankShades": "黒白検出で探すもの:",
    "prefs.blankShadesNote":
      "既定は「黒だけ」です。CM の切れ目に入るのは黒だからです。" +
      "白はタイトルの頭やニュースのフラッシュなど、番組の中身であることも多く、" +
      "数十箇所になることがあるので、必要なときに選んでください。" +
      "探さなかったほうは記録にも残らないので、あとから変えると録画を読み直します。",
    "prefs.shades.both": "黒と白",
    "prefs.shades.black": "黒だけ",
    "prefs.shades.white": "白だけ",
    "prefs.blankBlackLevel": "黒とみなす明るさ:",
    "prefs.blankWhiteLevel": "白とみなす明るさ:",
    "prefs.blankCoverage": "その明るさが占める割合:",
    "prefs.blankLevelNote":
      "画素がどれだけ暗ければ黒とみなすか、どれだけ明るければ白とみなすか、" +
      "そしてそういう画素が画面のどれだけを占めていれば、" +
      "そのフレームを黒（白）と呼ぶかです。既定は黒 10%、白 92%、割合 98% です。" +
      "フェードの終わりが真っ黒まで落ちない放送では、黒の値を 14〜16% に上げると拾えます。" +
      "局のロゴや焼き込みの字幕が出たままの録画では、割合を 95% ほどまで下げてください。" +
      "画面のいちばん外側 2% は、どの値でも判定に使いません。" +
      "変えると、それまでの検出結果は答えにならないので、録画を読み直します。",
    "prefs.flatMarkAt": "黒白区間の終わりの印:",
    "prefs.flatMarkAtNote":
      "既定は「黒でなくなったフレーム」です。区間の始まりと終わりの 2 つで切ると、" +
      "黒がちょうど無くなります。他のツールは「最後の黒いフレーム」を区間の終わりと" +
      "呼ぶので、数字を突き合わせると 1 フレームずれて見えます。" +
      "そちらに合わせたいときは下を選んでください。" +
      "無音の区間は変わりません。音が戻るのはサンプル単位なので、" +
      "2 つの答えのあいだにフレームがありません。" +
      "すでに置いた印は動きません。次に置く印から変わります。",
    "prefs.markAt.after": "黒でなくなったフレーム",
    "prefs.markAt.last": "最後の黒いフレーム",
    "prefs.blankKeyframes": "黒白を検出したら、自動でキーフレームを置く",
    "prefs.quietRun": "無音の区間とみなす長さ:",
    "prefs.quietRunNote":
      "これ以上続いた区間だけを扱います。既定は 3 秒です。" +
      "会話の間は 0.1〜0.4 秒、CM の切れ目は 1 秒前後なので、" +
      "短くすると息継ぎまで拾って数十箇所になります。" +
      "意図して空けた無音だけを見たいときは、このくらいが目安です。",
    "prefs.quietLevel": "無音とみなす音量:",
    "prefs.quietLevelNote":
      "1 サンプルごとに判定します。0 dB が最大で、既定は -50 dB です。" +
      "小さくすると本当に何も鳴っていないところだけを拾います。" +
      "区間の始まりと終わりは、音が止まった／戻ったサンプルそのものです。",
    "prefs.cmKeyframesNote":
      "CM ブロックの先頭と終わりに印を置きます。" +
      "外すと印は置かず、タイムラインに帯が出るだけになります。" +
      "見てから決めたいときは、≡ の「CM 検出結果をキーフレームにする」で置けます。" +
      "録画と同じ名前のファイルを読み込んだときは、入れてあっても CM 検出の結果に印は置きません。" +
      "CM 検出結果のファイルを読み込んだときは、外してあってもそのファイルの印を置きます。",
    "prefs.quietKeyframes": "無音を検出したら、自動でキーフレームを置く",
    "prefs.flatKeyframesNote":
      "区間の始まりと終わりの両方に印を置きます。" +
      "外すと印は置かず、タイムラインの下に帯が出るだけになります。" +
      "キーフレーム一覧では、黒白から置いた印と無音から置いた印が分かるようになっています。" +
      "録画と同じ名前のファイルを読み込んだときは、カット編集を開いた時点の検出結果に印は置きません。" +
      "≡ のメニューから検出し直せば置きます。",
    "prefs.quietOverwrite": "ショートカットでの保存は、確認せずに上書きする",
    "prefs.quietOverwriteNote":
      "Ctrl+H（キーフレーム情報）、Ctrl+Shift+H（Trim）、Ctrl+Alt+H（CM 検出結果）は、" +
      "録画と同じ名前で、画面を出さずに保存します。" +
      "すでに同じ名前のファイルがあるとき、確認するかどうかをここで決めます。" +
      "メニューからの保存は保存先を選ぶ画面が出るので、この設定とは関係ありません。",
    "prefs.groupOut": "出力設定",
    "prefs.prefix": "ファイル名の接頭辞:",
    "prefs.prefixNote":
      "新しいプロジェクトの初期値です。ここで変更すると、いま開いている出力設定にも反映されます。" +
      "プロジェクトを開いたときは、そのプロジェクトの設定が優先されます。",
    "prefs.number": "接頭辞のうしろに一覧の連番を付ける",
    "prefs.numberNote":
      "出力するファイル名に一覧の行番号を付けます。例: cut_03_録画.ts",
    "prefs.digits": "連番の桁数:",
    "prefs.dataBroadcast": "データ放送も残す（.ts のみ）",
    "prefs.dataBroadcastNote":
      "リモコンの d ボタンで見られるページを、カットした出力にも残します。" +
      "残せるのは .ts で出力するときだけです。ディスクにも MP4 にも入れる場所がありません。" +
      "データ放送は録画の 1〜20% を占めるので、外せばその分だけ小さくなります。",
    "prefs.keepOutput": "出力設定を次回の起動に引き継ぐ",
    "prefs.keepOutputNote":
      "保存先・ファイル名・コンテナ・音声の扱いを保存し、次回の起動と新規作成時に復元します。" +
      "プロジェクトを開いたときは、そのプロジェクトの設定が優先されます。",
    "prefs.forgetOutput": "既定に戻す",
    "prefs.keepWhat": "保存された出力先: {what}",
    "prefs.keepBeside": "録画と同じ場所",
    "prefs.keepNone": "まだ保存されていません",
    "prefs.groupRun": "カットの精度と速度",
    "prefs.cleanJoins": "範囲の先頭を整え直す（継ぎ目の乱れを防ぐ）",
    "prefs.cleanJoinsNote":
      "開いた GOP から始まる範囲で、先頭の最大 2 秒を再エンコードします。" +
      "継ぎ目の乱れは減りますが、その分、無劣化でコピーされる区間が短くなります。",
    "prefs.audioFade": "継ぎ目の音のフェード:",
    "prefs.audioFadeNote":
      "カットの継ぎ目で、音をいったん下げてから戻します。秒数で指定し、0 でフェードなし（既定）。" +
      "継ぎ目で音がいきなり変わるのを防げますが、そのぶん継ぎ目の前後では、本編の音も指定した秒数だけ小さくなります。" +
      "掛かるのは音を書き直すときだけです。音声の設定がコピーのときは掛からず、その旨が出力画面に表示されます。" +
      "出力の先頭と末尾は継ぎ目ではないので、掛かりません。",
    "prefs.proxy": "プロキシを作ってから編集する",
    "prefs.proxyNote":
      "録画全体を再エンコードし、軽い映像で編集します。録画 1 時間につき、数分の処理時間と数 GB の容量が必要です。" +
      "1 フレームの表示に時間がかかる素材で効果があります。",
    "prefs.proxyWidth": "プロキシの幅:",
    "prefs.proxyWidth.auto": "自動（1280）",
    "prefs.groupData": "作業データ",
    "prefs.cacheDir": "置き場:",
    "prefs.cacheDirDefault": "既定の場所",
    "prefs.cacheDirPick": "参照",
    "prefs.cacheDirReset": "既定",
    "prefs.cacheDirNote":
      "シーク用インデックス・プロキシ・検出結果を保存する場所です。" +
      "変更後に作成したものだけが新しい場所に保存され、これまでのものは元の場所に残ります。",
    "prefs.cacheDirFailed": "その場所には書き込めません: {e}",
    "prefs.cacheKind.index": "シーク用インデックス",
    "prefs.cacheKind.proxy": "プロキシ",
    "prefs.cacheKind.cm": "CM 検出",
    "prefs.cacheKind.flat": "黒白・無音の検出",
    "prefs.cacheFiles": "{n} 件",
    "prefs.cacheTotal": "合計 {size}",
    "prefs.cacheEmpty": "作業データはありません",
    "prefs.cacheClear": "すべて削除",
    "prefs.cacheClearTitle": "作業データの削除",
    "prefs.cacheClearBody":
      "{size} を削除します。削除されるのは再解析で復元できるデータだけで、" +
      "カット内容やプロジェクトには影響しません。",
    "prefs.cacheClearOk": "削除",
    "prefs.cacheClearCancel": "キャンセル",
    "prefs.cacheClearFailed": "削除できませんでした: {e}",
    "prefs.groupLog": "ログ",
    "prefs.ffmpegLog": "FFmpeg のログ:",
    "prefs.ffmpegLog.off": "出力しない（既定）",
    "prefs.ffmpegLog.warn": "警告のみ",
    "prefs.ffmpegLog.all": "すべて",
    "prefs.ffmpegLogNote":
      "FFmpeg 自身のメッセージを標準エラーに出力します。そのほとんどは不具合ではありません。" +
      "不具合を報告するときにだけ使ってください。",

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
    // カットが入っている一覧では、合計時間もカット後の長さです。録画そのものの
    // 長さは括弧に入れて残します。
    "input.totalCut": "クリップ合計数: {n}　合計時間: {t}（カット前 {full}）",
    "input.totalPending": "（未解析 {n} 本を除く）",
    "input.dropHint.title": "クリップ（映像ファイル）を追加してください",
    "input.dropHint.body":
      "「ファイルを追加」で選ぶか、ここへドラッグ＆ドロップしてください。<br />読み込んだ順にシーク用インデックスを作ります。",
    "input.dropHint.keys":
      "ダブルクリックで編集　／　ドラッグで並べ替え　／　F2 名前を変更　／　Ctrl+A 全選択　／　" +
      "Ctrl+D 選択した動画の CM 検出　／　Delete 削除",
    "side.fileInput": "ファイル入力",
    "side.addFiles": "＋　ファイルを追加",
    "side.clipEdit": "クリップ編集",
    "side.editClip": "✂　カット編集",
    "side.duplicate": "⧉　クリップを複製",
    "side.rename": "名前を変更",
    "side.detect": "CM を検出",
    // 探す色を黒だけ・白だけにしてあるときの名前。ボタンや行の右クリック、
    // 編集画面のメニューは、その設定で実際に走る検出を名乗る。
    "side.detectBlank": "黒白を検出",
    "side.detectBlankBlack": "黒を検出",
    "side.detectBlankWhite": "白を検出",
    "side.detectQuiet": "無音を検出",
    "side.stopBatch": "解析を中止",
    "side.resumeBatch": "解析を再開",
    "side.other": "その他",
    "side.moveUp": "上に移動",
    "side.moveDown": "下に移動",
    "side.selectAll": "全選択",
    "side.removeClip": "クリップ削除",
    "side.removeAll": "全削除",

    // --- 右クリックメニュー ------------------------------------------------
    //
    // The same commands as the buttons down the side, and deliberately not
    // the same strings: two of those carry a mark in front of the words
    // (`✂　カット編集`), which is furniture for a column of buttons. Seven
    // menu items of which two are marked would read as though those two were
    // the special ones.
    "rowmenu.edit": "カット編集",
    "rowmenu.rename": "名前を変更",
    "rowmenu.duplicate": "クリップを複製",
    "rowmenu.detect": "CM を検出",
    "rowmenu.detectBlank": "黒白を検出",
    "rowmenu.detectBlankBlack": "黒を検出",
    "rowmenu.detectBlankWhite": "白を検出",
    "rowmenu.detectQuiet": "無音を検出",
    "rowmenu.moveUp": "上に移動",
    "rowmenu.moveDown": "下に移動",
    "rowmenu.remove": "クリップ削除",
    "props.head": "クイックプロパティ",
    "props.none": "クリップが選択されていません",
    "props.many": "クリップ {n} 本を選択中",
    "props.queued": "{name}\n解析待ちです",
    "props.error": "{name}\n{error}",
    "props.body":
      "クリップ名:　{name}{copy}\n{path}\n映像:　{codec}, {w}x{h}, {fps} fps, {flags}\n" +
      "音声:　{audio}\n長さ:　{len} ({frames} フレーム){cut}　無劣化点 {points} 個{unusable}" +
      "\nシーン {scenes} 箇所　インデックス {index}{cm}{flat}",
    "props.copyOf": "（同じ録画の {n} 本目）",
    // 「長さ」の行に続けて入ります。カットが入っていない行では空です。
    "props.cut": "　カット後 {len}",
    "props.manyAudio": "\n音声:　{audio}",
    // 録画の音声トラックのうち、行に書いたもの以外の本数。
    "props.moreTracks": "　ほか {n} 本",
    // 行の音声を何で書くかの選択肢の先頭。出力設定の答えをそのまま名乗ります。
    "props.initial": "{name} (初期値)",
    "props.followOutput": "出力設定に従う",
    "props.noteOpen": "（",
    "props.noteClose": "）",
    "layout.mono": "モノラル",
    "layout.stereo": "ステレオ",
    "layout.dualMono": "デュアルモノラル",
    // 選んだ行で答えが割れているとき。選び直せば全部そろいます。
    "props.audioMixed": "選んだ行で設定が違います",
    "props.audioReencoded": "この行の音声は再エンコードされます",
    "props.audioCopying": "出力設定の音声が「そのままコピー」のあいだは選べません",
    "props.unusable": "（うち {n} 個は開始位置には使えません）",
    // Sits inside "無劣化点 {points} 個" and two lines like it, so it has
    // to read as a missing number rather than as a word: 無劣化点 解析待ち
    // 個 is not a sentence anybody wants to read.
    "props.pending": "—",
    "props.cm": "\nCM:　{note}",
    "props.blank": "\n黒白:　{note}",
    "props.blankBlack": "\n黒:　{note}",
    "props.blankWhite": "\n白:　{note}",
    "props.quiet": "\n無音:　{note}",
    "media.interlaced": "インターレース (TFF)",
    "media.progressive": "プログレッシブ",
    "media.pulldown": "2:3 プルダウン",
    "media.variable": "可変フレームレート",
    "media.audioYes": "あり",
    "media.audioNo": "なし",

    // --- the list's rows -------------------------------------------------
    "list.copyLabel": "{name}（{n}）",
    "list.kill": "この行を一覧から外す",
    "list.cannotRead": "{clip} は読み込めませんでした",
    "list.gone": "ファイルが見つかりません: {path}",
    "list.goneNote": "{clip} のファイルが見つかりません。名前が変わったか、移動したようです",
    "list.cannotOpenEditor": "編集画面を開けません: {e}",
    "list.andMore": " ほか {n} 件",
    "list.unsupported": "対応していない形式のため無視しました: {names}",
    "list.stopping": "中止しています…",
    "list.stopped": "解析を止めました。残りは「解析を再開」で続けられます",
    "dialog.video": "動画",
    "dialog.disc": "ディスクイメージ (BDMV / BDAV / DVD-Video)",
    "disc.title": "ディスクの読み込み",
    "disc.what": "{label} — {kind}　クリップ {n} 本",
    "disc.protected": "このディスクは AACS で暗号化されています。管理情報は読めるので一覧は出ますが、クリップは開けません。",
    "disc.kind.bdav": "BDAV（録画ディスク）",
    "disc.kind.bdmv": "BDMV（市販ディスク）",
    "disc.kind.dvd": "DVD-Video",
    "disc.all": "全選択",
    "disc.none": "全解除",
    "disc.showAll": "短いクリップも表示",
    "disc.ok": "読み込む",
    "disc.cancel": "キャンセル",
    "disc.count": "クリップ {n} 本を読み込みます",
    "disc.countNone": "クリップが選ばれていません",
    "disc.tracks": "トラック {n} 本",
    "disc.noTracks": "トラック情報なし",
    "disc.video": "映像",
    "disc.audio": "音声",
    "disc.subtitle": "字幕",
    "disc.menu": "メニュー",
    "disc.other": "その他",
    "disc.pid": "PID 0x{pid}",
    "disc.gone": "カットした出力には残せません",
    "disc.needed": "映像は外せません",
    "disc.apply": "同じ構成のクリップすべてに適用",
    "disc.applied": "クリップ {n} 本に同じ選択を適用しました",
    "disc.hidden": "短いクリップ {n} 本は表示していません",
    "dialog.project": "SmartCut プロジェクト",
    "queue.indexing": "シーク用インデックスを作成中: {clip}",
    "queue.picturing": "サムネイルを作成中: {clip}",
    "queue.detecting": "CM を検出中: {clip}",
    "queue.blank": "黒白を検出中: {clip}",
    "queue.quiet": "無音を検出中: {clip}",
    "row.sub":
      "{len} ({frames} フレーム)　00:00:00.00-{end}　{w}x{h}　{fps} fps　{codec}{audio}",
    // カットが入っている行の同じ行。長さはカット後のもので、録画そのものの
    // 長さは「カット前」として残します。
    "row.subCut":
      "{len} ({frames} フレーム)　カット前 {full}　{w}x{h}　{fps} fps　{codec}{audio}",
    "row.noAudio": "　音声なし",
    "row.cmRunning": "CM 検出中 {pct}% — {phase}",
    "row.cmQueued": "CM 検出 待機中",
    "row.cmReserved": "解析後に CM 検出",
    "row.cmNote": "CM: {note}",
    // 黒白の 4 行は、環境設定で探すものを絞ると名前が変わります。探して
    // いないものについて「なし」と言わないためで、ボタンの名前と同じ
    // 決まりです。`blankKey` を通して選びます。
    "row.blankRunning": "黒白 検出中 {pct}%",
    "row.blankRunningBlack": "黒 検出中 {pct}%",
    "row.blankRunningWhite": "白 検出中 {pct}%",
    "row.blankQueued": "黒白の検出 待機中",
    "row.blankQueuedBlack": "黒の検出 待機中",
    "row.blankQueuedWhite": "白の検出 待機中",
    "row.blankReserved": "解析後に黒白を検出",
    "row.blankReservedBlack": "解析後に黒を検出",
    "row.blankReservedWhite": "解析後に白を検出",
    "row.blankNote": "黒白: {note}",
    "row.blankNoteBlack": "黒: {note}",
    "row.blankNoteWhite": "白: {note}",
    "row.quietRunning": "無音 検出中 {pct}%",
    "row.quietQueued": "無音の検出 待機中",
    "row.quietReserved": "解析後に無音を検出",
    "row.quietNote": "無音: {note}",
    "row.cuts": "カット {n} 箇所",
    "row.keyframes": "キーフレーム {n}",
    "badge.smart": "Smart",
    "badge.error": "エラー",
    "badge.indexing": "解析中",
    "badge.editing": "編集中",
    "badge.queued": "解析待ち",
    "badge.edited": "編集済",
    "badge.editedTitle": "カット編集画面で編集された行です。検出しただけの行には付きません",
    "badge.cm": "CM {n}",
    "badge.cmNone": "CM なし",
    "badge.cmQueued": "CM 検出予約",
    "badge.cmRunning": "CM 検出中",
    "badge.blank": "黒白 {n}",
    "badge.blankBlack": "黒 {n}",
    "badge.blankWhite": "白 {n}",
    "badge.blankNone": "黒白なし",
    "badge.blankNoneBlack": "黒なし",
    "badge.blankNoneWhite": "白なし",
    "badge.blankQueued": "黒白 検出予約",
    "badge.blankQueuedBlack": "黒 検出予約",
    "badge.blankQueuedWhite": "白 検出予約",
    "badge.blankRunning": "黒白 検出中",
    "badge.blankRunningBlack": "黒 検出中",
    "badge.blankRunningWhite": "白 検出中",
    "badge.quiet": "無音 {n}",
    "badge.quietNone": "無音なし",
    "badge.quietQueued": "無音 検出予約",
    "badge.quietRunning": "無音 検出中",
    "ptext.running": "{phase} {pct}%",
    "ptext.cm": "CM 検出",
    "ptext.blank": "黒白",
    "ptext.quiet": "無音",
    "phase.queued": "待機中",
    "phase.reading": "読み込み中",
    "phase.pictures": "サムネイル",
    "phase.noPictures": "サムネイルを作成できませんでした（切り出しには影響しません）",
    "phase.detecting": "検出中",
    "phase.stopped": "中止しました",
    "phase.indexReused": "前回のインデックスを再利用",
    "phase.indexBuilt": "インデックス {s} 秒",
    "flat.previous": "{note}（前回の検出）",
    "blank.rowNote": "{n} 箇所",
    "quiet.rowNote": "{n} 箇所",
    "flat.detecting": "検出中…",
    // 何を探したのかを言わないと、2 つのうちどちらの答えか分かりません。
    "flat.what.blank": "黒白の区間",
    "flat.what.blankBlack": "黒の区間",
    "flat.what.blankWhite": "白の区間",
    "flat.what.quiet": "無音の区間",
    "flat.detectingPct": "{what}を検出中 {pct}%",
    "flat.found": "{what}を {n} 箇所見つかりました。両端にキーフレームを置いています",
    "flat.foundUnmarked": "{what}を {n} 箇所見つかりました。キーフレームは置いていません",
    "flat.none": "{what}は見つかりませんでした",
    "flat.cached": "検出済みの {n} 箇所を表示しています",
    "flat.cachedBeside":
      "検出済みの {n} 箇所を表示しています" +
      "（録画と同じ名前のファイルを読み込んだので、印は置いていません）",
    "flat.marked": "{what} {n} 箇所の両端をキーフレームにしました",
    "flat.failed": "検出できません: {e}",
    "cm.previous": "{note}（前回の検出）",
    "cm.failed": "検出できません: {e}",
    "cm.besideMarks": "{note}（録画と同じ名前のファイルを読み込んだので、印は置いていません）",

    // --- 出力設定画面 ----------------------------------------------------
    "outset.barNote": "ここでの設定は一覧のすべてのクリップに使われます",
    "outset.formatHead": "出力フォーマット",
    "outset.clipPick": "クリップ選択:",
    "outset.noClips": "クリップがありません",
    "outset.noReady": "解析の済んだクリップがありません",
    "outset.fileHead": "ファイル設定",
    "outset.outDir": "出力先フォルダー(F):",
    "outset.sameAsInput": "（入力ファイルと同じ場所）",
    "outset.sameAsInputSub": "（入力ファイルと同じ場所の「{name}」）",
    "outset.browse": "参照",
    "outset.prefix": "ファイル名の接頭辞:",
    "outset.number": "連番",
    "outset.digits": "桁数:",
    "outset.joinedInto": "{path}（クリップ {n} 本をまとめて 1 ファイル）",
    "outset.joinAll": "クリップを結合して出力",
    "outset.master": "基準クリップ:",
    "outset.masterLooking": "調べています…",
    "outset.masterFits": "ほかの {n} 本は同じ形式なので、そのままコピーされます",
    "outset.masterDiffer":
      "{of} 本中 {n} 本が基準クリップと形式が違うので、全編再エンコードになります",
    "outset.masterWhy": "{n}: {clip} — {why}",
    // 差が音声だけのときに、映像側の理由として出す言葉。音声の違いは
    // 下の「音声」の行が別に説明します。
    "fit.unstated": "音声だけが違います",
    "outset.crossHead": "継ぎ目の効果",
    "outset.crossRow": "継ぎ目の効果:",
    "outset.crossClip": "対象クリップ:",
    "outset.crossAfter": "{n}: {name} の後ろ",
    "outset.crossKind": "効果:",
    "outset.crossSecs": "適用時間:",
    "outset.seconds": "秒",
    "outset.crossEase": "イージング:",
    "outset.crossImage": "重ねる画像:",
    "outset.crossImageNone": "なし",
    "outset.crossImageKind": "画像",
    "outset.crossAll": "すべての継ぎ目に適用",
    "outset.crossNone": "すべて解除",
    "outset.crossNoJoins": "クリップが1本だけなので継ぎ目がありません",
    "outset.crossNoneSet": "継ぎ目 {of} か所、効果は設定されていません",
    "outset.crossSet": "継ぎ目 {of} か所のうち {n} か所に設定",
    "outset.crossSetShort": "継ぎ目 {of} か所のうち {n} か所に設定（出力は {secs} 秒短くなります）",
    "outset.crossKeeps": "フェードは前後のクリップから半分ずつ使うので、出力の長さは変わりません。効果のかかる範囲は再エンコードされます",
    "outset.crossShortens": "クリップ 2 本が重なって映るので、出力は {secs} 秒短くなります。重なる範囲は再エンコードされます",
    "cross.none": "なし",
    "cross.fadeBlack": "フェード（黒）",
    "cross.fadeWhite": "フェード（白）",
    "cross.dissolve": "ディゾルブ",
    "cross.wipeLeft": "ワイプ（左から）",
    "cross.wipeRight": "ワイプ（右から）",
    "cross.wipeTop": "ワイプ（上から）",
    "cross.wipeBottom": "ワイプ（下から）",
    "cross.slideLeft": "スライド（左から）",
    "cross.slideRight": "スライド（右から）",
    "cross.slideTop": "スライド（上から）",
    "cross.slideBottom": "スライド（下から）",
    "ease.none": "なし",
    "ease.back": "Back",
    "ease.bounce": "Bounce",
    "ease.circle": "Circle",
    "ease.elastic": "Elastic",
    "ease.exponential": "Exponential",
    "ease.power": "Power",
    "ease.sine": "Sine",
    "ease.quadratic": "Quadratic",
    "ease.cubic": "Cubic",
    "ease.quartic": "Quartic",
    "ease.quintic": "Quintic",
    "ease.in": "イン",
    "ease.out": "アウト",
    "ease.inOut": "イン-アウト",
    "ease.outIn": "アウト-イン",
    "container.sound": "音声のみ（映像なし）",
    "outset.soundOnlyNote":
      "カットも結合も継ぎ目のフェードも入ったまま、音声だけのファイルを書き出します。" +
      "映像は読みも書きもしないので、そのぶん速く終わります。" +
      "拡張子は音声の種類で決まります（この一覧では .{ext}）。" +
      "音声コーデックでリニア PCM を選べば .wav になります。" +
      "音声トラックは 1 本だけで、二か国語の録画の 2 本目は入りません。",
    "outset.container": "コンテナタイプ(Y):",
    "outset.audio": "音声(A):",
    "outset.audioCodec": "音声コーデック:",
    "outset.audioChannels": "音声チャンネル(C):",
    "outset.audioRate": "サンプリング周波数:",
    "outset.audioBits": "量子化ビット数:",
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
    "codec.same": "入力と同じ",
    "codec.aac": "AAC",
    "codec.ac3": "AC-3（ドルビーデジタル）",
    "codec.dts": "DTS",
    "codec.lpcm": "リニア PCM（非圧縮）",
    "codec.lpcm.short": "リニア PCM",
    "channels.same": "入力と同じ",
    "channels.mono": "1ch（モノラル）",
    "channels.stereo": "2ch（ステレオ）",
    "channels.surround51": "5.1ch（6ch）",
    "rate.same": "入力と同じ",
    "rate.96": "96 kHz",
    "rate.48": "48 kHz",
    "rate.441": "44.1 kHz",
    "rate.32": "32 kHz",
    "bits.same": "入力と同じ",
    "bits.16": "16 bit",
    "bits.24": "24 bit",
    "bitrate.auto": "おまかせ",
    "bitrate.none": "—",
    "bitrate.fixed": "{rate} kbps（固定）",
    "bitrate.fixedRange": "{from}〜{to} kbps（固定）",
    "outset.audioLine": "{mode}（{detail}）",
    "outset.format":
      "映像:　{codec}, {w}x{h}, {fps} fps, {scan}\n音声:　{audio}\n" +
      "区間:　{keeps} 区間 / 出力 {kept}（元 {dur}、カット {cuts} 箇所）\n出力先:　{out}{side}",
    // 同じ 3 行に、ディスクの索引へ書かれるものを足したもの。ファイル名の
    // 代わりに、この録画がディスクのどこに入るかを言う。
    "outset.formatBdav":
      "映像:　{codec}, {w}x{h}, {fps} fps, {scan}\n音声:　{audio}\n" +
      "区間:　{keeps} 区間 / 出力 {kept}（元 {dur}、カット {cuts} 箇所）\n" +
      "チャプター:　{marks} 個\nディスク:　{out}",
    // 索引の欄が空のまま書かれるときに、その欄が見せるもの。録画が何も
    // 言わなかったときも、消したときも同じ結果になる——どちらもディスクには
    // 何も入らない。番号だけ短いのは、欄が 3 桁分しかないからです。
    "outset.fieldBlank": "（記入なし）",
    "outset.tabFile": "ファイル出力",
    "outset.tabBdav": "BDAV 出力",
    "outset.discHead": "ディスク設定",
    "outset.subfolder": "サブフォルダー:",
    "outset.subfolderNone": "（作らずに、上のフォルダーへ直接出力）",
    "outset.discTitle": "ディスクタイトル:",
    "out.discSize": "出力サイズは {used} です（{disc} のディスクに収まります）",
    // 「これ以上小さくできませんでした」と言ってよいのは、実際に小さく
    // しようとしたときだけである。トランスコードを指示されていない実行が
    // 同じことを表示すると、指示すれば入ったかもしれない、という事実が消える。
    "out.discTooBig": "出力サイズは {used} で、{disc} のディスクの容量を {over} 超えています。映像をこれ以上小さくできませんでした",
    "out.discOver": "出力サイズは {used} で、{disc} のディスクの容量を {over} 超えています。「トランスコードする」を有効にすると、映像を書き直して収まることがあります",
    "out.shrinking": "ディスクに収めるため、映像を元の {share}% のサイズに書き直します",
    "outset.disc": "ディスク:",
    "disc.bd25": "BD-R / BD-RE 25GB（1層）",
    "disc.bd50": "BD-R / BD-RE DL 50GB（2層）",
    "disc.bd100": "BD-R XL 100GB（3層）",
    "disc.bd128": "BD-R XL 128GB（4層）",
    "outset.fit": "トランスコードする",
    "outset.gauge": "ディスクの使用量:",
    "gauge.total": "合計",
    "gauge.asIs": "変換前",
    "gauge.fitted": "変換後",
    "gauge.fits": "{used} / {disc}（{pct}%）・{n} 本、{dur}",
    "gauge.over": "{used} / {disc}（{pct}%）・{n} 本、{dur}。{over} 超過しています",
    "gauge.willFit": "{used} / {disc}（{pct}%）・{n} 本、{dur}。映像を {share}% のサイズにトランスコードして収めます",
    "gauge.unreachable": "{used} / {disc}（{pct}%）・{n} 本、{dur}。トランスコードしても収まりません（映像を {share}% まで小さくする必要がありますが、下限は {floor}% です）",
    "gauge.notMpeg2": "　※ MPEG-2 以外の録画はトランスコードできないため、元のサイズのまま見積もっています",
    "gauge.empty": "一覧が空です",
    "outset.image": "イメージ:",
    "image.none": "作らない（フォルダーのまま）",
    "image.udf250": "ISO イメージ（UDF 2.50）",
    "image.udf260": "ISO イメージ（UDF 2.60）",
    "outset.imageAccess": "ディスクの種別:",
    "access.readOnly": "追記しない（BD-R / 再生専用）",
    "access.overwritable": "レコーダーで編集する（BD-RE）",
    "outset.imageOnly": "イメージを作成後、フォルダーを削除",
    "outset.imageLine": "\nイメージ:　{path}（UDF {udf}）",
    "outset.imageOnlyLine": "\nイメージ:　{path}（UDF {udf}・フォルダーは残しません）",
    "outset.discFolder": "ディスクの場所(F):",
    "outset.discHere": "（フォルダーを選んでください）",
    "outset.programme": "番組名:",
    "outset.channel": "チャンネル:",
    "outset.channelNumber": "番号:",
    "outset.numberNone": "（なし）",
    "outset.made": "記録日時:",
    "outset.about": "番組内容:",
    // 欄の残り。ARIB のバイト数で、UTF-8 でも文字数でもない——プレイリストが
    // 数えるのがこれだから。
    "outset.bytes": "{n}/{room}",
    "outset.madeBad": "YYYY-MM-DD HH:MM:SS",
    "outset.discPath": "{dir}/BDAV",
    "outset.interlaced": "インターレース (トップフィールド優先)",
    "outset.sidecar": "\n　　　　{path}",

    // --- 出力画面 --------------------------------------------------------
    "out.idle": "出力するクリップを一覧に追加してください",
    "out.needDiscFolder":
      "BDAV 出力にはディスクを作成する場所が必要です。出力先フォルダーを選んでください",
    "out.madeUnreadable":
      "「{name}」の記録日時が読み取れません。YYYY-MM-DD HH:MM:SS の形式で入力してください",
    // 出力一覧に並ぶ、カットのあとに続く 2 行。
    "out.stepIndex": "ディスクの管理情報",
    "out.stepImage": "ディスクイメージ（UDF {udf}）",
    "out.stepFailed": "失敗",
    "out.bdavIndexing": "ディスクの管理情報を作成中: 録画 {clip}",
    "out.imaging": "イメージを作成中（UDF {udf}）: {pct}%",
    "out.imageDone": "イメージを作成しました: {path}",
    "out.imageFailed": "イメージを作成できませんでした: {e}",
    "out.folderGone": "フォルダーを削除しました: {path}",
    "out.folderStays": "フォルダーを削除できませんでした: {e}",
    "out.bdavDone": "ディスクを作成しました: {path}（録画 {n} 本）",
    "out.discarded": "中止したため取り消しました",
    "out.bdavDiscarded": "管理情報を作れなかった録画 {n} 本をディスクから取り除きました",
    "out.bdavLeftover": "書きかけの録画を削除できませんでした: {e}",
    "out.bdavFailed": "ディスクの管理情報を作成できませんでした: {e}",
    "out.run": "出力開始",
    "out.abort": "出力中止",
    "out.enlist": "バッチに登録",
    "out.overwrite": "バッチを上書き",
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
    // The same "nothing to re-encode" as above, on a run that is writing
    // the pictures back smaller to fit a disc: the seams cost nothing and
    // every frame is still rewritten, so "losslessly" would be a lie.
    "out.shrinkNote": "{clip} — 継ぎ目の再エンコードはなし。ディスクに収めるため、全編を元の {share}% のサイズに書き直します",
    "out.shrinkStage": "ディスクに収めるため全編をトランスコード",
    // 結合のとき、基準クリップと形式が違うクリップは継ぎ目だけでなく全編が
    // 書き直されます。継ぎ目の計画からは分からないので、別の文言にしています。
    "out.conformNote":
      "{clip} — 基準クリップ「{master}」と映像の形式が違うので、全編を再エンコードします（{why}）",
    "out.conformStage": "基準クリップの形式に合わせて全編を再エンコード",
    "out.shotsAll": "{clip} — 全 {frames} フレームを再エンコードします（コピーできる区間がありません）",
    // 形式の違いを 1 件ずつ並べるときの書き方。左がそのクリップの値、右が
    // 基準クリップの値で、矢印はこれから行う変換の向きです。
    "fit.line": "{what} {theirs} → {master}",
    "fit.codec": "コーデック",
    "fit.size": "画面サイズ",
    "fit.pixels": "画素形式",
    "fit.rate": "フレームレート",
    "fit.scan": "走査方式",
    "fit.aspect": "画素アスペクト比",
    "fit.colour": "色",
    "fit.audioCodec": "音声コーデック",
    "fit.audioRate": "サンプリング周波数",
    "fit.audioChannels": "チャンネル数",
    "fit.audioTracks": "音声トラック数",
    "out.allCutNote": "{clip} — すべてカットされています。書き出すものがありません",
    "out.allCutStage": "すべてカットされています",
    "out.allCut": "すべてカットされています",
    "out.audioReencoded": "（音声は再エンコードします）",
    "out.audioAsCodec": "（音声は {codec} で再エンコードします）",
    "out.audioDownmixed": "（音声は {from} → {to} へダウンミックスして再エンコードします）",
    "out.audioUpmixed": "（音声は {from} → {to} へ広げて再エンコードします）",
    "out.audioConformed": "（音声も基準クリップに合わせて再エンコードします）",
    "out.shots": "{clip} — {n} 箇所 / {frames} フレーム（ほかはバイト単位でコピー）",
    "out.ovlKind": "再エンコード {i} / {n}",
    "out.ovlNote": "{n} フレーム",
    "out.aborting": "中止します（いま書き出しているクリップは最後まで書き終えます）",
    "out.skipped": "中止",
    "outset.branched": "同名のフォルダーあり → {name}",
    "out.sameName": "入力と同じ名前になります",
    "out.branched": "「{asked}」フォルダーが既にあるので「{name}」に書き出します",
    "out.writing": "\"{name}\" を出力中…",
    "out.writingCopy": "\"{name}\" を出力中: 映像を無劣化でコピーしています…",
    "out.writingMost":
      "\"{name}\" を出力中: {n} 箇所を再エンコードし、ほかは無劣化でコピーしています…",
    "out.writingAll": "\"{name}\" を出力中: 映像を再エンコードしています…",
    "out.writingShrink": "\"{name}\" を出力中: ディスクに収めるため、映像を元の {share}% のサイズに書き直しています…",
    // 結合は 1 ファイルなので、状況の行は「いまどのクリップを書いているか」を
    // 併せて示します。クリップごとに書き方が変わるため、単体出力とは別のキーです。
    "out.joinCopy": "\"{name}\" を出力中: {clip} は映像を無劣化でコピーしています…",
    "out.joinMost":
      "\"{name}\" を出力中: {clip} は {n} 箇所を再エンコードし、ほかは無劣化でコピーしています…",
    "out.joinAll": "\"{name}\" を出力中: {clip} は映像を再エンコードしています…",
    "out.joinShrink":
      "\"{name}\" を出力中: {clip} はディスクに収めるため、映像を元の {share}% のサイズに書き直しています…",
    "out.joinConform":
      "\"{name}\" を出力中: {clip} は基準クリップの形式に合わせて全編を書き直しています…",
    "out.writingTables": "\"{name}\" を仕上げ中: 放送の番組情報を書き戻しています…",
    "out.done": "完了{extra}",
    "out.doneKeyframes": " / キーフレーム {n} 個",
    "out.joined": "{n} 本を \"{name}\" にまとめました",
    "out.writingCrossings": "\"{name}\" を出力中: 継ぎ目 {n} か所（計 {secs} 秒）を再エンコードします…",
    "out.summary": "{done} / {all} 本を出力しました{failed}{aborted}　経過 {elapsed}",
    "out.summaryFailed": "　失敗 {n} 本",
    "out.summaryAborted": "　（中止されました）",

    // --- バッチ出力 --------------------------------------------------------
    "batch.counts":
      "全ジョブ: {all}　実行中: {run}　待ち: {wait}　完了: {done}　失敗: {bad}　中止: {off}",
    "batch.elapsed": "経過 {t}",
    "batch.left": "残り {t}",
    "batch.andElsewhere": "{dir} ほか {n} か所",
    "batch.andMore": "{name} ほか {n} 本",
    "batch.toDisc": "ディスクを作成",
    "batch.toImage": "ディスク＋イメージ (UDF {udf})",
    "batch.add": "ジョブ追加",
    "batch.drop": "ジョブ削除",
    "jobmenu.toTop": "先頭へ移動",
    "jobmenu.toBottom": "末尾へ移動",
    "jobmenu.requeue": "もう一度出力する",
    "jobmenu.openProject": "プロジェクトを開く",
    "jobmenu.openFolder": "出力先フォルダーを開く",
    "batch.moreTitle": "まとめて削除",
    "batch.clearDone": "出力済みのジョブを削除",
    "batch.clearAll": "すべて削除",
    "batch.clearedDone": "出力済みのジョブを {n} 件削除しました",
    "batch.windowTitle": "バッチ出力 — SmartCut",
    "batch.opened": "バッチ出力ツールを起動しました",
    "batch.alreadyUp": "バッチ出力ツールはすでに起動しています",
    "batch.queueFailed": "キューを保存できませんでした: {e}",
    "batch.run": "バッチ開始",
    "batch.stopAll": "すべて中止",
    "batch.stopJob": "中止",
    "batch.jobStopped": "このジョブは中止されました",
    "batch.afterHead": "完了後",
    "batch.afterNothing": "何もしない",
    "batch.afterSleep": "スリープ",
    "batch.afterShutdown": "シャットダウン",
    "batch.afterCancel": "中止",
    "batch.clips": "クリップ {n} 本",
    "batch.waiting": "待機中",
    "batch.opening": "プロジェクトを開いています…",
    "batch.reading": "一覧を読み込んでいます…",
    "batch.writing": "出力中…",
    "batch.skipped": "中止されました",
    "batch.interrupted": "出力の途中でツールが閉じられました",
    "batch.stoppedHere": "出力の途中で中止されました",
    "batch.cannotOpen": "プロジェクトを開けませんでした",
    "batch.nothingReadable": "読める録画が 1 本もありません",
    "batch.refused": "出力設定が足りず、開始できませんでした",
    "batch.threw": "出力中にエラーが発生しました: {e}",
    "batch.wrote": "{n} 本を出力しました",
    "batch.someFailed": "{all} 本中 {n} 本が失敗しました",
    "batch.already": "すでにキューに入っています: {name}",
    "batch.added": "キューに追加しました: {name}",
    "batch.overwritten": "キューのジョブを上書きしました: {name}",
    "batch.stopping": "中止しています…",
    "batch.replaceTitle": "バッチ出力の開始",
    "batch.replaceBody":
      "バッチ出力はキューのプロジェクトを順番に開きます。いまの一覧は破棄され、保存していない変更は失われます。続けますか？",
    "batch.clearTitle": "キューの全消去",
    "batch.clearBody": "キューのジョブ {n} 件をすべて消します。よろしいですか？",
    "batch.sleepIn": "{s} 秒後にスリープします",
    "batch.shutdownIn": "{s} 秒後にシャットダウンします",
    "batch.afterCancelled": "完了後の動作を取り消しました",
    "batch.afterFailed": "完了後の動作を実行できませんでした: {e}",

    // --- the cut editor --------------------------------------------------
    "editor.title": "カット編集",
    "editor.windowTitle": "カット編集 — {clip}",
    "editor.loading": "読み込み中…",
    "editor.analysing": "解析中…",
    "editor.counterShow": "カウンタ",
    "editor.counterShow.title": "フレーム番号と時刻をプレビュー映像の上に表示する",

    // --- 拡大表示 ---------------------------------------------------------
    // --- 継ぎ目の編集 -----------------------------------------------------
    "xw.title": "継ぎ目の編集",
    "xw.windowTitle": "継ぎ目の編集 - {before} → {after}",
    "xw.loading": "読み込み中…",
    "xw.whichHead": "対象のクリップ",
    "xw.setHead": "継ぎ目の設定",
    "xw.soundHead": "音声のフェード",
    "xw.fadeOut": "前のクリップ（フェードアウト）:",
    "xw.fadeIn": "次のクリップ（フェードイン）:",
    "xw.fadeTitle":
      "0.1 秒単位、10 秒まで。0 でフェードなしです。" +
      "映像の効果とは別で、どちらか片方だけでも設定できます。" +
      "音声をコピーする設定では効きません（出力設定の「音声」を" +
      "スマートレンダリングか再エンコードにしてください）",
    "xw.patternHead": "プレビュー",
    "xw.allHead": "一括適用／削除",
    "xw.clipTime": "実クリップ時間:",
    "xw.crossTime": "実トランジション時間:",
    "xw.beforeClip": "前のクリップ",
    "xw.afterClip": "次のクリップ",
    "xw.crossing": "継ぎ目",
    "xw.reading": "クリップを読み込んでいます…",
    "xw.cannotRead": "クリップを読み込めませんでした: {e}",
    "xw.open": "継ぎ目の設定…",
    "xw.noJoins": "継ぎ目がありません",
    "xw.cannotOpen": "継ぎ目の編集を開けませんでした: {e}",
    "zoom.title": "拡大表示",
    "zoom.windowTitle": "拡大表示",
    "zoom.open": "拡大表示",
    "zoom.scale": "倍率",
    "zoom.waiting": "カット編集のプレビューをクリックすると、その部分を拡大表示します",
    "zoom.at": "{time}　{scale} 倍　({x}, {y})",
    "zoom.cannotOpen": "拡大表示を開けません: {e}",
    "editor.detectCm": "CM を検出",
    "editor.detectCm.title": "CM らしい区間を探して、その先頭と終わりにキーフレームを置きます (Ctrl+D)",
    "editor.detectBlank": "黒白を検出",
    "editor.detectBlankBlack": "黒を検出",
    "editor.detectBlankWhite": "白を検出",
    "editor.detectBlank.title":
      "映像が黒い区間・白い区間を探します（どちらを探すかは環境設定）(Ctrl+B)" +
      "／既定では両端にキーフレームを置きます（環境設定で変えられます）" +
      "／Alt+↑ Alt+↓ でその端をたどれます",
    "editor.detectSilence": "無音を検出",
    "editor.detectSilence.title":
      "音声が無音の区間を探します (Ctrl+Q)" +
      "／既定では両端にキーフレームを置きます（環境設定で変えられます）" +
      "／Alt+Shift+↑ Alt+Shift+↓ でその端をたどれます",
    "editor.detecting": "検出中…（映像も読み込みます）",
    "editor.detectingPct": "検出中 {pct}%",
    "editor.keyframes": "キーフレーム",
    "editor.keyCount": "{n} 個",
    "editor.keyframes.empty":
      "まだありません。「⚑ キーフレーム」でいまの位置を登録できます。CM を検出すると、本編と CM の先頭が自動で並びます。",
    "editor.keyframes.emptyManual":
      "まだありません。「⚑ キーフレーム」でいまの位置を登録できます。CM を検出したあと、≡ の「CM 検出結果をキーフレームにする」を押すと並びます。",
    "editor.keyframes.kill": "このキーフレームを消す",
    // 印そのものに出どころは書かれていません。いま出ている帯と突き合わせて、
    // その位置が黒・白・無音のどれかの端であれば、そのしるしを付けています。
    "editor.keyframes.from.black": "黒",
    "editor.keyframes.from.white": "白",
    "editor.keyframes.from.quiet": "無音",
    "editor.keyframes.fromTitle.black": "黒の区間の端",
    "editor.keyframes.fromTitle.white": "白の区間の端",
    "editor.keyframes.fromTitle.quiet": "無音の区間の端",
    "editor.searching": "サーチ中",
    "editor.searchKind": "サーチ",
    "editor.selection": "選択 {a} - {b} : {len}",
    "editor.selectionTime": "選択 {a} - {b} : {len}",
    "editor.selectionNone": "選択 —",
    "editor.counter": "{at} / {all}   {t}",
    "editor.frameKind": "{kind} フレーム",
    "editor.frameKindPoint": "{kind} フレーム — 無劣化点",
    "editor.frameKindNear": "{kind} フレーム — 近くのフレーム（解析中）",
    "editor.previewFailed": "プレビュー失敗: {e}",
    "editor.stripHint":
      "クリックで移動／<b>右ドラッグ</b>で前後にサーチ（右へ＝送り・左へ＝戻し）／<b>中クリック</b>で場面の変わり目へ（右半分で次・左半分で前）／ホイールで 1 フレーム送り（Shift で GOP 単位）／Space で再生",
    "editor.stripShow": "1 画面",
    "strip.win3": "3 秒",
    "strip.win6": "6 秒",
    "strip.win30": "30 秒",
    "strip.win180": "3 分",
    "strip.frame": "1 フレームずつ",
    "t.addKey": "⚑ キーフレーム",
    "t.addKey.title": "いまのフレームをキーフレームに登録 (K、Insert で登録と解除)",
    "t.prevScene": "⇤ シーン",
    "t.prevScene.title": "前のシーンの変わり目へ (↑ / Shift+S)",
    "t.nextScene": "シーン ⇥",
    "t.nextScene.title": "次のシーンの変わり目へ (↓ / S)",
    "t.play": "▶ 再生",
    "t.stop": "■ 停止",
    "t.play.title": "ここから再生 (Space)",
    "t.rewind": "巻き戻し。押すたびに 2→4→8→16 倍、もう一度で止まります",
    "t.fastFwd": "早送り。押すたびに 2→4→8→16 倍、もう一度で止まります",
    "t.loop": "⟲ ループ",
    "t.loop.title": "選択した範囲を繰り返し再生する",
    "t.mute": "音を消す / 戻す (M)",
    "t.volume": "プレビューの音量。出力の音は変わりません",
    "t.volume.aria": "プレビューの音量",
    "t.goStart": "先頭へ (Home)",
    "t.prevKf": "前の無劣化点へ (Shift+↑)",
    "t.stepBack": "1 フレーム戻る (←)。押しっぱなしで連続",
    "t.gotoIn": "選択の開始位置へ移動",
    "t.setIn": "ここを選択の開始に (I / [)",
    "t.cut": "✂ カット",
    "t.cutRange": "いまの選択を出力から取り除く (Del、Ctrl+Del で選択の内側だけ)",
    "t.setOut": "ここを選択の終わりに (O / ])",
    "t.gotoOut": "選択の終わり位置へ移動",
    "t.stepFwd": "1 フレーム進む (→)。押しっぱなしで連続",
    "t.nextKf": "次の無劣化点へ (Shift+↓)",
    "t.goEnd": "末尾へ (End)",
    "t.cutOutside": "外側をカット",
    "t.cutOutside.title": "選択の外側をすべて取り除く",
    "t.snap": "無劣化点へ吸着",
    "t.snap.title": "選択の両端をいちばん近い無劣化点へ寄せる",
    "t.undo": "↺ 取消",
    "t.undo.title": "直前の操作を取り消す (Ctrl+Z)",
    "t.redo": "↻ やり直し",
    "t.redo.title": "取り消した操作をやり直す (Ctrl+Y)",
    "t.clearAll": "全消去",
    "t.clearAll.title": "カットとキーフレームをすべて消す",
    "editor.ok": "OK",
    "editor.cancel": "キャンセル",

    // --- プレビューの字幕 -----------------------------------------------
    "subs.label": "字幕",
    "subs.off": "表示しない",
    "subs.kind.caption": "字幕",
    "subs.kind.superimpose": "文字スーパー",
    "subs.kind.ttml": "字幕（4K）",
    "subs.kind.graphics": "PGS",
    "subs.kind.subpicture": "サブピクチャ",
    "subs.failed": "字幕を読めませんでした: {e}",
    "lang.jpn": "日本語",
    "lang.eng": "英語",
    "lang.und": "言語不明",

    // --- トラック -------------------------------------------------------
    "tracks.button": "トラック",
    "tracks.button.title": "この録画のどのストリームを書き出すか選ぶ",
    "tracks.title": "書き出すトラック",
    "tracks.note":
      "チェックを外したトラックは出力に含まれません。字幕を残せるのは TS で出力するときだけです。",
    "tracks.audio": "音声",
    "tracks.caption": "字幕",
    "tracks.superimpose": "文字スーパー",
    "tracks.graphics": "字幕（ディスクの図形）",
    "tracks.beside": "字幕 {lang}: 出力先は出力設定で決めます（出力ファイルの中／別ファイル）   id 0x{pid}",
    "outset.subtitles": "ディスクの字幕:",
    "subtitles.beside": "別ファイルに出力 (.idx / .sub)",
    "subtitles.sup": "別ファイルに出力 (.sup／PGS のまま)",
    "subtitles.pgs": "出力ファイルに含める (PGS)",
    // どの行き先が無加工かはディスクによって違うので、それを言う。
    // `paintSubtitleChoices` を参照。
    "outset.subtitles.dvd": "DVD の字幕:",
    "subtitles.pgs.dvd": "変換して出力ファイルに含める (PGS)",
    "subtitles.beside.dvd": "そのまま別ファイルに出力 (.idx / .sub)",
    "subtitles.sup.dvd": "変換して別ファイルに出力 (.sup)",
    "outset.subtitles.bdmv": "Blu-ray の字幕:",
    "subtitles.pgs.bdmv": "そのまま出力ファイルに含める (PGS)",
    "subtitles.beside.bdmv": "変換して別ファイルに出力 (.idx / .sub)",
    "subtitles.sup.bdmv": "そのまま別ファイルに出力 (.sup)",
    "tracks.main": "主音声",
    "tracks.pid": "PID 0x{pid}",
    "tracks.dropped": "出力できません: {what}   PID 0x{pid}",
    "tracks.gone.superimpose": "文字スーパー",
    "tracks.data": "データ放送",
    "tracks.substream": "同じ PID に畳み込まれた互換用ストリーム",
    "tracks.menu": "メニュー",
    "tracks.textst": "テキスト字幕（書体はディスク側にあります）",
    "tracks.settled": "{what} — 環境設定で決めます   PID 0x{pid}",
    "tracks.settledNote":
      "データ放送を残すかどうかは環境設定で決めます。残せるのは .ts のときだけで、既定では残します。",
    "tracks.droppedNote":
      "これらは選べません。文字スーパーはパケットに時刻を持たず、メニューとテキスト字幕はディスクでしか働かないからです。",
    "tracks.substreamNote":
      "Blu-ray のロスレス音声には、再生できない機器のための AC-3 が同じ PID に重ねて入っています。1 つの PID に 2 本は書き戻せないので、本体の TrueHD だけを出力し、内側の AC-3 は外します。",
    "tracks.tablesNote":
      "番組情報（EIT）・放送局名・放送時刻は、TS に書き出すときはそのまま引き継ぎます。トラックではないので、この一覧には並びません。",
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
    "editor.infoUnusable": "   （うち {n} 個は開始位置には使えません）",
    "editor.hoverWarming": "準備中",
    "editor.hoverScene": "シーン",
    "warm.start": "準備中 0%",
    "warm.progress": "{phase}準備中 {pct}%",
    "warm.failed": "準備: {e}",
    "warm.proxy": "プロキシ {w}x{h} {mb}MB（{how}{s}秒）",
    "warm.proxyReused": "前回のものを再利用 ",
    "warm.proxyBuilt": "作成 ",
    "warm.noProxy": "プロキシなし（{note}）",
    "warm.index": "シーク用インデックス {mb}MB{how}",
    "warm.indexReused": "（前回のものを再利用）",
    "warm.indexBuilt": "（作成 {s}秒）",
    "warm.noIndex": "シーク用インデックスを保存できませんでした",
    "warm.thumbs": "サムネイル {n} 枚 {gap}s 間隔",
    "warm.scenes": "シーン {n} 箇所",
    "plan.openFile": "ファイルを開いてください",
    "plan.reading": "録画を読み込んでいます。どこを無劣化で残せるかは、読み終えてから分かります",
    "plan.allCut": "すべてカットされています",
    "plan.text":
      "出力 {total}（{ranges} 区間、カット {cuts} 箇所）— 無劣化コピー {copied}s ({pct})" +
      " / 再エンコード {reencoded}s",
    "plan.lossless": "映像 完全無劣化",
    "plan.reencoded": "再エンコード {n} フレーム",
    "plan.reencodedTime": "再エンコード {t} 秒",
    "plan.segCopy": "コピー　　",
    "plan.segEncode": "再エンコード",
    "plan.failed": "計画できません: {e}",
    "keyframes.readFailed": "キーフレームを読み込めません: {e}",
    "keyframes.read": "キーフレーム {n} 個を {file} から読み込みました",
    "keyframes.chapters": "ディスクのチャプター {n} 個をキーフレームにしました",
    "editor.more.title":
      "印の読み書き、ディスクのチャプター、キーフレームの全消去。" +
      "キーフレーム情報（.keyframe）は印の位置だけ、AviSynth スクリプト（.trim.avs）は残る区間そのもの、" +
      "CM 検出結果（.cm.json）は検出した直後の帯と印です。" +
      "Ctrl+H・Ctrl+Shift+H・Ctrl+Alt+H なら、録画と同じ名前で、画面を出さずに保存します。",
    "marks.save.keyframe": "キーフレーム情報を保存",
    "marks.save.trim": "AviSynth Trim にカットを保存",
    "marks.save.cm": "CM 検出結果を保存",
    "marks.load.keyframe": "キーフレーム情報を読み込む",
    "marks.load.trim": "AviSynth Trim からカットを読み込む",
    "marks.load.cm": "CM 検出結果を読み込む",
    "marks.readNone": "{file} には読み込めるものがありませんでした",
    "editor.chapterKeys": "ディスクのチャプターをキーフレームにする",
    "editor.cmKeys": "CM 検出結果をキーフレームにする",
    // ≡ メニューの 2 行。{what} には「黒白の区間」「無音の区間」が入ります。
    "editor.flatKeys": "{what}をキーフレームにする",
    "editor.clearKeys": "キーフレームをすべて消す",
    "marks.kind.keyframe": "キーフレーム情報",
    "marks.kind.trim": "AviSynth スクリプト",
    "marks.kind.cm": "CM 検出結果",
    "marks.saved": "キーフレーム {n} 個を {file} に保存しました",
    "marks.saveFailed": "保存できません: {e}",
    "marks.overwriteTitle": "上書きの確認",
    "marks.overwriteBody": "{file} はすでにあります。上書きしますか？",
    "editor.dropTitle": "編集の破棄",
    "editor.dropBody": "この画面で行った編集を破棄して閉じます。よろしいですか？",
    "trim.saved": "残す区間 {n} 個を {file} に保存しました",
    "trim.read": "{file} からカット {n} 箇所を読み込みました",
    "trim.readFailed": "Trim を読み込めません: {e}",
    "cm.saved": "CM ブロック {n} 個を {file} に保存しました",
    "cm.read": "CM ブロック {n} 個を {file} から読み込みました",
    "cm.readFailed": "CM 検出結果を読み込めません: {e}",
    "cm.marked": "CM ブロック {n} 個をキーフレームにしました",
    "cm.nothing": "まだ CM を検出していません",
  },

  en: {
    // --- shared ---------------------------------------------------------
    "sep": "  /  ",
    "dur.h": "{h}h ",
    "dur.m": "{m}m ",
    "dur.s": "{s}s",
    "cm.how.captions": "{n} caption reset{n?s}",
    "cm.how.logo": "logo + silence",
    "cm.how.silence": "silence only (no logo)",
    "cm.found": "{how}: {n} block{n?s} / {total} in total",
    "cm.none": "{how}: nothing that looks like a commercial",

    // --- the window furniture -------------------------------------------
    "tab.input": "Input",
    "tab.outset": "Output settings",
    "tab.out": "Export",
    "tab.batch": "Batch",
    "ui.menu.title": "Menu",
    "menu.new": "New project",
    "menu.open": "Open project…",
    "menu.save": "Save project",
    "menu.saveAs": "Save project as…",
    "menu.batch": "Batch tool…",
    "menu.prefs": "Preferences…",
    "menu.about": "About SmartCut",
    "menu.quit": "Quit",

    // --- projects ---------------------------------------------------------
    "project.untitled": "Untitled",
    "project.windowTitle": "{mark}{name} — SmartCut",
    "project.saved": "Project saved: {name}",
    "project.opened": "Project opened: {name} ({n} clip{n?s})",
    "project.refused":
      "Project opened: {name} ({n} clip{n?s}; {bad} left out, being names of something other than a file)",
    "project.nothingToSave": "The list is empty — there is nothing to save",
    "project.cannotOpen": "Cannot open the project: {name} ({e})",
    "project.wrongFormat":
      "{name} is not a SmartCut project, or was written by a later version",
    "project.replaceTitle": "Open project",
    "project.replaceBody":
      "The list and everything cut in it will be replaced. Any work you have not saved will be lost. Continue?",
    "project.newTitle": "New project",
    "project.newBody":
      "The list and everything cut in it will be discarded. Any work you have not saved will be lost. Continue?",
    "project.newDone": "Started a new project",
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
    "prefs.groupView": "Windows",
    "prefs.counter": "Draw the frame number and clock over the picture in the cut editor",
    "prefs.meter": "Show the audio level meter in the cut editor",
    "prefs.subs": "Show the subtitles in the cut editor from the start",
    "prefs.subsNote":
      "Only for recordings that carry any. It can still be turned off while cutting.",
    "prefs.groupEdit": "Cut editor",
    "prefs.pageStep": "PageUp / PageDown:",
    "prefs.pageStepShift": "Held with Shift:",
    "prefs.pageStepCtrl": "Held with Ctrl:",
    "prefs.pageStepShiftCtrl": "Held with Shift and Ctrl:",
    "prefs.seconds": "seconds",
    "prefs.step.frame": "frames",
    "prefs.step.sec": "seconds",
    "prefs.step.pct": "% a second, scrolling",
    "prefs.len.frame": "pictures",
    "prefs.len.sec": "seconds",
    "prefs.pageStepNote":
      "What PageUp and PageDown do to the playhead in the cut editor. A step of 0 is a key that " +
      "does nothing. Frames and seconds are amounts, one per press. A percentage is a speed: a share " +
      "of the fastest scroll here, which is sixty times the recording's own speed. 25 is fifteen " +
      "times, so a second of holding covers fifteen seconds of the recording, whatever is open. The " +
      "left and right keys are separate and unchanged: one picture, and one second with Shift.",
    "prefs.sidecarPriority": "When more than one is there, read:",
    "prefs.sidecar.keyframe": "the keyframe list (.keyframe)",
    "prefs.sidecar.trim": "the AviSynth Trim line (.trim.avs)",
    "prefs.sidecar.cm": "a saved detection (.cm.json)",
    "prefs.sidecarNote":
      "A file left beside the recording is picked up when the cut editor opens it. A keyframe list is " +
      "a list of places, so it arrives as marks and cuts nothing; a Trim line is the cut itself, so " +
      "the recording opens with the material already taken out; a saved detection arrives as the " +
      "detection it was, band and marks and all. Any of them on its own is read whatever this says. " +
      "Where one of them was read, a detection the list is holding is not mixed into the marks.",
    "prefs.cmKeyframes": "Turn a detection into keyframes",
    "prefs.cmInserts": "Also find the short inserts (a channel's own ident)",
    "prefs.cmInsertsNote":
      "Some subscription channels drop two to nine seconds of their own animated ident into a " +
      "programme where the terrestrial broadcast had its commercials. A break that short is not " +
      "looked for out of the box. What finds one is the sound: an insert is cut in, so the " +
      "programme's audio stops for it. A programme's own full-screen caption card takes the " +
      "corner just as thoroughly, though, and some programmes lay one over silence — measured " +
      "over twenty episodes of one such programme, about a third of what came out was a caption " +
      "card. Only the cut editor's Detect commercials asks this; the clip list's own pass does " +
      "not.",
    "prefs.blankRun": "Counts as blank after:",
    "prefs.blankRunNote":
      "Shorter stretches are left out. The default of 3 s reports only long gaps -- the black " +
      "between one recording and the next, a break in the feed -- because broadcast black at a " +
      "junction runs two to four pictures. For those, say 2 and pick pictures.",
    "prefs.blankShades": "The blank pass looks for:",
    "prefs.blankShadesNote":
      "Black out of the box, that being where a junction is laid. White belongs as often to what " +
      "was being watched -- a title sequence cuts on a flash, and so does a camera in a news item " +
      "-- so on some material it is dozens of stretches nobody asked about. A shade that is not " +
      "looked for is not written down either, so changing this reads the recording again.",
    "prefs.shades.both": "Black and white",
    "prefs.shades.black": "Black only",
    "prefs.shades.white": "White only",
    "prefs.blankBlackLevel": "Counts as black below:",
    "prefs.blankWhiteLevel": "Counts as white above:",
    "prefs.blankCoverage": "...over this much of the picture:",
    "prefs.blankLevelNote":
      "How dark a pixel has to be to count as black, how bright to count as white, and how much " +
      "of the frame has to be one of them before the frame is called that. 10%, 92% and 98% out " +
      "of the box. A channel that fades to a very dark grey rather than to black is caught by " +
      "raising the first to 14 or 16; a recording with a station logo or burnt-in text standing " +
      "in a corner wants the third down around 95. The outermost 2% of each edge is left out of " +
      "the judgement whatever these say. Changing any of them leaves every saved detection " +
      "unable to answer, so the recording is read again.",
    "prefs.flatMarkAt": "A blank stretch ends on:",
    "prefs.flatMarkAtNote":
      "The first picture that is no longer flat, out of the box: cut at a stretch's two ends and " +
      "the black is gone exactly. Other tools call the last black picture the end of the stretch, " +
      "so a reading held up against one is a frame out every time -- pick the other answer to " +
      "match. Silences are unaffected: the sound comes back on a sample, and there is no picture " +
      "between the two answers. Marks already down do not move; this is about the next ones.",
    "prefs.markAt.after": "The picture after it",
    "prefs.markAt.last": "Its own last picture",
    "prefs.blankKeyframes": "Turn a blank detection into keyframes",
    "prefs.quietRun": "Counts as silence after:",
    "prefs.quietRunNote":
      "Shorter stretches are left out. A pause in dialogue runs 0.1 to 0.4 s and a junction's " +
      "silence about a second, so anything much under the default of 3 s comes back as dozens of " +
      "stretches, most of them somebody drawing breath.",
    "prefs.quietLevel": "Silence is quieter than:",
    "prefs.quietLevelNote":
      "Measured sample by sample, 0 dB being full scale. Lower than the default of -50 dB finds " +
      "only what is truly silent. A stretch begins and ends on the sample the sound stopped or " +
      "came back on, rather than on the frame that sample is in.",
    "prefs.cmKeyframesNote":
      "A detection marks the start and the end of every block it found. Off, it leaves the band " +
      "under the timeline and the sentence beside it, and ≡ → 「Turn the detection into keyframes」 " +
      "puts the marks down when you ask for them. Where a mark file beside the recording was read a " +
      "detection is not marked whatever this says, and a detection read from a file is marked " +
      "whatever this says.",
    "prefs.quietKeyframes": "Turn a silence detection into keyframes",
    "prefs.flatKeyframesNote":
      "Both ends of every stretch are marked. Off, the band under the timeline is all that is left, " +
      "and the marks go down by hand. The keyframe list says which detection a mark came from. " +
      "Where a mark file beside the recording was read, what the window opens holding is not marked " +
      "whatever this says; the two lines in the ≡ menu mark it.",
    "prefs.quietOverwrite": "Let the save shortcut write over a file without asking",
    "prefs.quietOverwriteNote":
      "Ctrl+H (the keyframe list), Ctrl+Shift+H (the Trim line) and Ctrl+Alt+H (the detection) write " +
      "straight to the name beside the recording. This is whether they stop to ask when something is " +
      "already there. Saving from the menu puts a picker up, which asks for itself.",
    "prefs.groupOut": "Output settings",
    "prefs.prefix": "Filename prefix:",
    "prefs.prefixNote":
      "What a new project starts with. Changing it here puts it into the settings in force as well. " +
      "A project that is opened brings its own and wins.",
    "prefs.number": "Put the row's number in the list behind the prefix",
    "prefs.numberNote":
      "Carries the order of the list into the names that are written. The number is the one beside " +
      "the row: cut_03_recording.ts",
    "prefs.digits": "Digits in the number:",
    "prefs.dataBroadcast": "Keep the data broadcast (.ts only)",
    "prefs.dataBroadcastNote":
      "Carries the pages behind the d button into the cut. Only a .ts that keeps the broadcast's own " +
      "tables can hold one; a disc's framing and an MP4 have nowhere to put it. A carousel is between a " +
      "hundredth and a fifth of what a multiplex spends, so clearing this is what makes the file smaller.",
    "prefs.keepOutput": "Carry the output settings over to the next start",
    "prefs.keepOutputNote":
      "Remembers the folder, the file name, the container and what is done to the audio, and puts them " +
      "back at the next start and on a new project. A project that is opened brings its own and wins.",
    "prefs.forgetOutput": "Back to the defaults",
    "prefs.keepWhat": "Remembered folder: {what}",
    "prefs.keepBeside": "beside the recording",
    "prefs.keepNone": "Nothing remembered yet",
    "prefs.groupRun": "How cuts are made",
    "prefs.cleanJoins": "Tidy the start of each range (steadier joins)",
    "prefs.cleanJoinsNote":
      "Re-encodes up to the first two seconds of a range that begins on an open GOP. " +
      "The join is steadier; that much less of the output is copied losslessly.",
    "prefs.audioFade": "Fade the sound at each seam:",
    "prefs.audioFadeNote":
      "Takes the level down into a join and brings it back out over that many seconds; 0 is no " +
      "fade, which is the default. The step in the sound becomes a pause instead — and that much " +
      "of the programme either side of every join is quieter than it was recorded. It needs sound " +
      "this program is writing, so smart rendering or a re-encode; a track set to be copied is " +
      "copied, and the export says so. The beginning and the end of the output are left alone: a " +
      "fade is for a place where two pieces meet.",
    "prefs.proxy": "Build a proxy before cutting",
    "prefs.proxyNote":
      "Re-encodes the whole recording and cuts against the lighter copy. Costs minutes and " +
      "gigabytes per hour, and only pays where decoding one picture is itself slow.",
    "prefs.proxyWidth": "Proxy width:",
    "prefs.proxyWidth.auto": "Automatic (1280)",
    "prefs.groupData": "Scratch files",
    "prefs.cacheDir": "Kept in:",
    "prefs.cacheDirDefault": "the usual place",
    "prefs.cacheDirPick": "Browse…",
    "prefs.cacheDirReset": "Default",
    "prefs.cacheDirNote":
      "Where the seek indexes, proxies and detections go. Only what is written " +
      "after the change goes to the new folder; what is already cached stays where it is.",
    "prefs.cacheDirFailed": "Nothing can be written there: {e}",
    "prefs.cacheKind.index": "Seek indexes",
    "prefs.cacheKind.proxy": "Proxies",
    "prefs.cacheKind.cm": "Commercial detections",
    "prefs.cacheKind.flat": "Blank and silence detections",
    "prefs.cacheFiles": "{n} file{n?s}",
    "prefs.cacheTotal": "{size} in all",
    "prefs.cacheEmpty": "Nothing here yet",
    "prefs.cacheClear": "Delete all",
    "prefs.cacheClearTitle": "Delete the scratch files",
    "prefs.cacheClearBody":
      "{size} will be deleted. What is lost is what another pass would build again, " +
      "never a cut or a project.",
    "prefs.cacheClearOk": "Delete",
    "prefs.cacheClearCancel": "Cancel",
    "prefs.cacheClearFailed": "Cannot delete: {e}",
    "prefs.groupLog": "Logging",
    "prefs.ffmpegLog": "FFmpeg log:",
    "prefs.ffmpegLog.off": "Silent (default)",
    "prefs.ffmpegLog.warn": "Warnings only",
    "prefs.ffmpegLog.all": "Everything",
    "prefs.ffmpegLogNote":
      "Lets FFmpeg's own messages through to standard error. Almost none of them are faults; " +
      "turn this on to quote them in a bug report.",

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
    // Where anything has been cut, the total is what the cuts leave; what was
    // recorded follows in brackets.
    "input.totalCut": "Clips: {n}   Total length: {t} (before cutting {full})",
    "input.totalPending": " ({n} not yet read)",
    "input.dropHint.title": "Add clips — the recordings you want to cut",
    "input.dropHint.body":
      "Pick them with “Add files”, or drag and drop them here.<br />Seek indexes are built in the order they arrive.",
    "input.dropHint.keys":
      "Double-click to edit  /  drag to reorder  /  F2 rename  /  Ctrl+A select all  /  " +
      "Ctrl+D detect commercials  /  Delete to remove",
    "side.fileInput": "Files",
    "side.addFiles": "＋　Add files",
    "side.clipEdit": "Clip",
    "side.editClip": "✂　Cut editor",
    "side.duplicate": "⧉　Duplicate clip",
    "side.rename": "Rename clip",
    "side.detect": "Detect commercials",
    "side.detectBlank": "Detect blank",
    "side.detectBlankBlack": "Detect black",
    "side.detectBlankWhite": "Detect white",
    "side.detectQuiet": "Detect silence",
    "side.stopBatch": "Stop analysis",
    "side.resumeBatch": "Resume analysis",
    "side.other": "Other",
    "side.moveUp": "Move up",
    "side.moveDown": "Move down",
    "side.selectAll": "Select all",
    "side.removeClip": "Remove clip",
    "side.removeAll": "Remove all",

    // --- the menu on the right button --------------------------------------
    "rowmenu.edit": "Cut editor",
    "rowmenu.rename": "Rename",
    "rowmenu.duplicate": "Duplicate clip",
    "rowmenu.detect": "Detect commercials",
    "rowmenu.detectBlank": "Detect blank",
    "rowmenu.detectBlankBlack": "Detect black",
    "rowmenu.detectBlankWhite": "Detect white",
    "rowmenu.detectQuiet": "Detect silence",
    "rowmenu.moveUp": "Move up",
    "rowmenu.moveDown": "Move down",
    "rowmenu.remove": "Remove clip",
    "props.head": "Quick properties",
    "props.none": "No clip selected",
    "props.many": "{n} clip{n?s} selected",
    "props.queued": "{name}\nWaiting to be read",
    "props.error": "{name}\n{error}",
    "props.body":
      "Clip:  {name}{copy}\n{path}\nVideo:  {codec}, {w}x{h}, {fps} fps, {flags}\n" +
      "Audio:  {audio}\nLength:  {len} ({frames} frame{frames?s}){cut}   {points} lossless points{unusable}" +
      "\n{scenes} scenes   index {index}{cm}{flat}",
    "props.copyOf": " (copy {n} of this recording)",
    // Follows the length. Empty on a row nothing has been cut out of.
    "props.cut": "   {len} after cutting",
    "props.manyAudio": "\nAudio: {audio}",
    "props.moreTracks": " and {n} more",
    "props.initial": "{name} (default)",
    "props.followOutput": "As the output settings say",
    "props.noteOpen": "(",
    "props.noteClose": ")",
    "layout.mono": "Mono",
    "layout.stereo": "Stereo",
    "layout.dualMono": "Dual mono",
    // The chosen rows do not agree. Picking one gives them all that answer.
    "props.audioMixed": "the chosen rows differ",
    "props.audioReencoded": "this row's sound is written afresh",
    "props.audioCopying": "not while the output settings copy the sound as it is",
    "props.unusable": " ({n} of them cannot start a cut)",
    "props.pending": "—",
    "props.cm": "\nCommercials:  {note}",
    "props.blank": "\nBlank:  {note}",
    "props.blankBlack": "\nBlack:  {note}",
    "props.blankWhite": "\nWhite:  {note}",
    "props.quiet": "\nSilence:  {note}",
    "media.interlaced": "interlaced (TFF)",
    "media.progressive": "progressive",
    "media.pulldown": "2:3 pulldown",
    "media.variable": "variable frame rate",
    "media.audioYes": "yes",
    "media.audioNo": "none",

    // --- the list's rows -------------------------------------------------
    "list.copyLabel": "{name} ({n})",
    "list.kill": "Take this row out of the list",
    "list.cannotRead": "{clip} could not be read",
    "list.gone": "The file is no longer there: {path}",
    "list.goneNote": "{clip} is no longer where it was: the file has been renamed or moved",
    "list.cannotOpenEditor": "Cannot open the cut editor: {e}",
    "list.andMore": " and {n} more",
    "list.unsupported": "Ignored, not a supported format: {names}",
    "list.stopping": "Stopping…",
    "list.stopped": "Analysis stopped. “Resume analysis” picks up the rest",
    "dialog.video": "Video",
    "dialog.disc": "Disc image (BDMV / BDAV / DVD-Video)",
    "disc.title": "Reading a disc",
    "disc.what": "{label} — {kind}, {n} clip(s)",
    "disc.protected": "This disc is encrypted with AACS. Its index reads, which is why the list is here; the clips themselves cannot be opened.",
    "disc.kind.bdav": "BDAV (a disc of recordings)",
    "disc.kind.bdmv": "BDMV (a pressed disc)",
    "disc.kind.dvd": "DVD-Video",
    "disc.all": "All",
    "disc.none": "None",
    "disc.showAll": "Show the short clips too",
    "disc.ok": "Add these",
    "disc.cancel": "Cancel",
    "disc.count": "{n} clip(s) will be added",
    "disc.countNone": "Nothing chosen",
    "disc.tracks": "{n} track(s)",
    "disc.noTracks": "the disc does not say what this carries",
    "disc.video": "Video",
    "disc.audio": "Sound",
    "disc.subtitle": "Subtitles",
    "disc.menu": "Menu",
    "disc.other": "Other",
    "disc.pid": "PID 0x{pid}",
    "disc.gone": "a cut cannot carry this",
    "disc.needed": "the video cannot be left out",
    "disc.apply": "Use these tracks for every clip like this one",
    "disc.applied": "The same tracks are now chosen on {n} clip(s)",
    "disc.hidden": "{n} more clip(s) are hidden for being short",
    "dialog.project": "SmartCut project",
    "queue.indexing": "Building seek index: {clip}",
    "queue.picturing": "Building thumbnails: {clip}",
    "queue.detecting": "Detecting commercials: {clip}",
    "queue.blank": "Detecting blank stretches: {clip}",
    "queue.quiet": "Detecting silent stretches: {clip}",
    "row.sub":
      "{len} ({frames} frames)   00:00:00.00-{end}   {w}x{h}   {fps} fps   {codec}{audio}",
    // The same line on a row that has been cut: the length is what the cuts
    // leave, and what was recorded follows it.
    "row.subCut":
      "{len} ({frames} frames)   before cutting {full}   {w}x{h}   {fps} fps   {codec}{audio}",
    "row.noAudio": "   no audio",
    "row.cmRunning": "Detecting commercials {pct}% — {phase}",
    "row.cmQueued": "Commercial detection queued",
    "row.cmReserved": "Commercial detection after the read",
    "row.cmNote": "Commercials: {note}",
    "row.blankRunning": "Blank detection {pct}%",
    "row.blankRunningBlack": "Black detection {pct}%",
    "row.blankRunningWhite": "White detection {pct}%",
    "row.blankQueued": "Blank detection queued",
    "row.blankQueuedBlack": "Black detection queued",
    "row.blankQueuedWhite": "White detection queued",
    "row.blankReserved": "Blank detection after the read",
    "row.blankReservedBlack": "Black detection after the read",
    "row.blankReservedWhite": "White detection after the read",
    "row.blankNote": "Blank: {note}",
    "row.blankNoteBlack": "Black: {note}",
    "row.blankNoteWhite": "White: {note}",
    "row.quietRunning": "Silence detection {pct}%",
    "row.quietQueued": "Silence detection queued",
    "row.quietReserved": "Silence detection after the read",
    "row.quietNote": "Silence: {note}",
    "row.cuts": "{n} cut{n?s}",
    "row.keyframes": "{n} keyframe{n?s}",
    "badge.smart": "Smart",
    "badge.error": "Error",
    "badge.indexing": "Reading",
    "badge.editing": "Editing",
    "badge.queued": "Queued",
    "badge.edited": "Edited",
    "badge.editedTitle": "Settled in the cut editor. A row that has only been detected does not carry it",
    "badge.cm": "CM {n}",
    "badge.cmNone": "No CM",
    "badge.cmQueued": "CM booked",
    "badge.cmRunning": "Detecting CM",
    "badge.blank": "Blank {n}",
    "badge.blankBlack": "Black {n}",
    "badge.blankWhite": "White {n}",
    "badge.blankNone": "No blank",
    "badge.blankNoneBlack": "No black",
    "badge.blankNoneWhite": "No white",
    "badge.blankQueued": "Blank booked",
    "badge.blankQueuedBlack": "Black booked",
    "badge.blankQueuedWhite": "White booked",
    "badge.blankRunning": "Detecting blank",
    "badge.blankRunningBlack": "Detecting black",
    "badge.blankRunningWhite": "Detecting white",
    "badge.quiet": "Silence {n}",
    "badge.quietNone": "No silence",
    "badge.quietQueued": "Silence booked",
    "badge.quietRunning": "Detecting silence",
    "ptext.running": "{phase} {pct}%",
    "ptext.cm": "Detecting",
    "ptext.blank": "Blank",
    "ptext.quiet": "Silence",
    "phase.queued": "Queued",
    "phase.reading": "Reading",
    "phase.pictures": "Thumbnails",
    "phase.noPictures": "No thumbnails (cutting is unaffected)",
    "phase.detecting": "Detecting",
    "phase.stopped": "Stopped",
    "phase.indexReused": "Index from an earlier run",
    "phase.indexBuilt": "Indexed in {s}s",
    "flat.previous": "{note} (from an earlier run)",
    "blank.rowNote": "{n} stretch{n?es}",
    "quiet.rowNote": "{n} stretch{n?es}",
    "flat.detecting": "Detecting…",
    // Which of the two answered has to be in the sentence; "found 3" alone
    // does not say what was looked for.
    "flat.what.blank": "blank",
    "flat.what.blankBlack": "black",
    "flat.what.blankWhite": "white",
    "flat.what.quiet": "silent",
    "flat.detectingPct": "Detecting {what} stretches {pct}%",
    "flat.found": "Found {n} {what} stretch{n?es}; both ends of each are marked",
    "flat.foundUnmarked": "Found {n} {what} stretch{n?es}; nothing is marked",
    "flat.none": "No {what} stretch that long",
    "flat.cached": "Showing {n} stretch{n?es} already detected",
    "flat.cachedBeside":
      "Showing {n} stretch{n?es} already detected — not marked: a mark file beside the " +
      "recording was read",
    "flat.marked": "Turned the ends of {n} {what} stretch{n?es} into keyframes",
    "flat.failed": "Cannot detect: {e}",
    "cm.previous": "{note} (from an earlier run)",
    "cm.failed": "Cannot detect: {e}",
    "cm.besideMarks": "{note} — not marked: a mark file beside the recording was read",

    // --- output settings screen ------------------------------------------
    "outset.barNote": "These settings are used for every clip in the list",
    "outset.formatHead": "Output format",
    "outset.clipPick": "Clip:",
    "outset.noClips": "No clips",
    "outset.noReady": "No clip has been read yet",
    "outset.fileHead": "File settings",
    "outset.outDir": "Output folder (F):",
    "outset.sameAsInput": "(the same folder as the input)",
    "outset.sameAsInputSub": "(a folder called \"{name}\", beside the input)",
    "outset.browse": "Browse",
    "outset.prefix": "Filename prefix:",
    "outset.number": "Number",
    "outset.digits": "digits:",
    "outset.joinedInto": "{path} ({n} clips, written as one)",
    "outset.joinAll": "Write the list as one file",
    "outset.master": "Master clip:",
    "outset.masterLooking": "Working it out…",
    "outset.masterFits": "The other {n} clip{n?s} are this shape, so they are copied",
    "outset.masterDiffer":
      "{n} of {of} clips are not this shape, so every picture of them is written afresh",
    "outset.masterWhy": "{n}: {clip} — {why}",
    // What stands in for the video reason where only the sound differs. The
    // audio note below says what about the sound.
    "fit.unstated": "only the sound differs",
    "outset.crossHead": "Between the clips",
    "outset.crossRow": "Between the clips:",
    "outset.crossClip": "After clip:",
    "outset.crossAfter": "{n}: after {name}",
    "outset.crossKind": "Effect:",
    "outset.crossSecs": "Duration:",
    "outset.seconds": "s",
    "outset.crossEase": "Easing:",
    "outset.crossImage": "Image over it:",
    "outset.crossImageNone": "none",
    "outset.crossImageKind": "Image",
    "outset.crossAll": "Apply to every join",
    "outset.crossNone": "Clear them all",
    "outset.crossNoJoins": "One clip, so there is no join for anything to happen at",
    "outset.crossNoneSet": "{of} joins, none of them with an effect on",
    "outset.crossSet": "{n} of {of} joins carry an effect",
    "outset.crossSetShort": "{n} of {of} joins carry an effect (the file comes out {secs}s shorter)",
    "outset.crossKeeps": "A fade takes half its time from each side, so the file keeps its length. What it covers is re-encoded",
    "outset.crossShortens": "Both clips are on screen at once, so the file comes out {secs}s shorter. What they share is re-encoded",
    "cross.none": "None",
    "cross.fadeBlack": "Fade through black",
    "cross.fadeWhite": "Fade through white",
    "cross.dissolve": "Dissolve",
    "cross.wipeLeft": "Wipe (from the left)",
    "cross.wipeRight": "Wipe (from the right)",
    "cross.wipeTop": "Wipe (from the top)",
    "cross.wipeBottom": "Wipe (from the bottom)",
    "cross.slideLeft": "Slide (from the left)",
    "cross.slideRight": "Slide (from the right)",
    "cross.slideTop": "Slide (from the top)",
    "cross.slideBottom": "Slide (from the bottom)",
    "ease.none": "None",
    "ease.back": "Back",
    "ease.bounce": "Bounce",
    "ease.circle": "Circle",
    "ease.elastic": "Elastic",
    "ease.exponential": "Exponential",
    "ease.power": "Power",
    "ease.sine": "Sine",
    "ease.quadratic": "Quadratic",
    "ease.cubic": "Cubic",
    "ease.quartic": "Quartic",
    "ease.quintic": "Quintic",
    "ease.in": "In",
    "ease.out": "Out",
    "ease.inOut": "In-out",
    "ease.outIn": "Out-in",
    "container.sound": "Sound only, no pictures",
    "outset.soundOnlyNote":
      "An audio file with the cuts, the joins and the seam fades already in it. Nothing is read " +
      "or written for the pictures, so it finishes in a fraction of the time. The extension " +
      "follows what the sound is — .{ext} for this list, and .wav if Audio codec is set to " +
      "linear PCM. One sound track: a bilingual recording's second is left out.",
    "outset.container": "Container (Y):",
    "outset.audio": "Audio (A):",
    "outset.audioCodec": "Audio codec:",
    "outset.audioChannels": "Audio channels (C):",
    "outset.audioRate": "Sample rate:",
    "outset.audioBits": "Bit depth:",
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
    "codec.same": "Same as the input",
    "codec.aac": "AAC",
    "codec.ac3": "AC-3 (Dolby Digital)",
    "codec.dts": "DTS",
    "codec.lpcm": "Linear PCM (uncompressed)",
    "codec.lpcm.short": "linear PCM",
    "channels.same": "Same as the input",
    "channels.mono": "1ch (mono)",
    "channels.stereo": "2ch (stereo)",
    "channels.surround51": "5.1ch (6 channels)",
    "rate.same": "Same as the input",
    "rate.96": "96 kHz",
    "rate.48": "48 kHz",
    "rate.441": "44.1 kHz",
    "rate.32": "32 kHz",
    "bits.same": "Same as the input",
    "bits.16": "16 bit",
    "bits.24": "24 bit",
    "bitrate.auto": "Leave it to the engine",
    "bitrate.none": "—",
    "bitrate.fixed": "{rate} kbps (fixed)",
    "bitrate.fixedRange": "{from}–{to} kbps (fixed)",
    "outset.audioLine": "{mode} ({detail})",
    "outset.format":
      "Video:  {codec}, {w}x{h}, {fps} fps, {scan}\nAudio:  {audio}\n" +
      "Ranges:  {keeps} kept / {kept} out (of {dur}, {cuts} cut{cuts?s})\nWritten to:  {out}{side}",
    "outset.formatBdav":
      "Video:  {codec}, {w}x{h}, {fps} fps, {scan}\nAudio:  {audio}\n" +
      "Ranges:  {keeps} kept / {kept} out (of {dur}, {cuts} cut{cuts?s})\n" +
      "Chapters:  {marks}\nDisc:  {out}",
    "outset.fieldBlank": "(left blank)",
    "outset.tabFile": "Files",
    "outset.tabBdav": "BDAV disc",
    "outset.discHead": "Disc settings",
    "outset.subfolder": "Subfolder:",
    "outset.subfolderNone": "(none: straight into the folder above)",
    "outset.discTitle": "Disc title:",
    "out.discSize": "The disc came to {used}, which fits a {disc} disc",
    "out.discTooBig": "The disc came to {used}, which is {over} more than a {disc} disc holds: the pictures would not transcode any smaller",
    "out.discOver": "The disc came to {used}, which is {over} more than a {disc} disc holds. Turning Transcode on can write the pictures back smaller until they fit",
    "out.shrinking": "To fit the disc, the pictures are being transcoded to {share}% of their own size",
    "outset.disc": "Disc:",
    "disc.bd25": "BD-R / BD-RE 25GB (single layer)",
    "disc.bd50": "BD-R / BD-RE DL 50GB (dual layer)",
    "disc.bd100": "BD-R XL 100GB (triple layer)",
    "disc.bd128": "BD-R XL 128GB (quadruple layer)",
    "outset.fit": "Transcode",
    "outset.gauge": "Disc used:",
    "gauge.total": "total",
    "gauge.asIs": "before",
    "gauge.fitted": "after",
    "gauge.fits": "{used} of {disc} ({pct}%) -- {n} recording(s), {dur}",
    "gauge.over": "{used} of {disc} ({pct}%) -- {n} recording(s), {dur}. {over} too much",
    "gauge.willFit": "{used} of {disc} ({pct}%) -- {n} recording(s), {dur}. The pictures will be transcoded to {share}% of their own size",
    "gauge.unreachable": "{used} of {disc} ({pct}%) -- {n} recording(s), {dur}. It will not fit even then: the pictures would have to come to {share}%, and {floor}% is as far as this goes",
    "gauge.notMpeg2": "  Recordings that are not MPEG-2 cannot be transcoded, and are counted at their full size",
    "gauge.empty": "nothing in the list",
    "outset.image": "Image:",
    "image.none": "None (leave it a folder)",
    "image.udf250": "ISO image (UDF 2.50)",
    "image.udf260": "ISO image (UDF 2.60)",
    "outset.imageAccess": "Disc kind:",
    "access.readOnly": "Nothing writes to it again (BD-R, playback)",
    "access.overwritable": "The recorder may go on editing it (BD-RE)",
    "outset.imageOnly": "Remove the folder once the image is written",
    "outset.imageLine": "\nImage:  {path} (UDF {udf})",
    "outset.imageOnlyLine": "\nImage:  {path} (UDF {udf}, and the folder goes)",
    "outset.discFolder": "Disc folder (F):",
    "outset.discHere": "(choose a folder)",
    "outset.programme": "Programme name:",
    "outset.channel": "Channel:",
    "outset.channelNumber": "No.:",
    "outset.numberNone": "(none)",
    "outset.made": "Recorded:",
    "outset.about": "About:",
    "outset.bytes": "{n}/{room}",
    "outset.madeBad": "YYYY-MM-DD HH:MM:SS",
    "outset.discPath": "{dir}/BDAV",
    "outset.interlaced": "interlaced (top field first)",
    "outset.sidecar": "\n             {path}",

    // --- export screen ---------------------------------------------------
    "out.idle": "Add clips to the list to export them",
    "out.needDiscFolder": "A BDAV disc needs somewhere to be built: choose an output folder",
    "out.madeUnreadable":
      "{name}: that is not a moment a playlist can carry. Write it as YYYY-MM-DD HH:MM:SS",
    // The two rows that follow the cuts in the output list.
    "out.stepIndex": "The disc index",
    "out.stepImage": "The disc image (UDF {udf})",
    "out.stepFailed": "Failed",
    "out.bdavIndexing": "Writing the disc index: recording {clip}",
    "out.imaging": "Writing the image (UDF {udf}): {pct}%",
    "out.imageDone": "Wrote the image: {path}",
    "out.imageFailed": "The image could not be written: {e}",
    "out.folderGone": "Removed the folder: {path}",
    "out.folderStays": "The folder could not be removed: {e}",
    "out.bdavDone": "Wrote the disc: {path} ({n} recording(s))",
    "out.discarded": "Taken back: the run was stopped",
    "out.bdavDiscarded": "Took {n} unindexed recording(s) back off the disc",
    "out.bdavLeftover": "The unfinished recording could not be cleared away: {e}",
    "out.bdavFailed": "The disc index could not be written: {e}",
    "out.run": "Start export",
    "out.abort": "Stop export",
    "out.enlist": "Add to batch",
    "out.overwrite": "Overwrite the job",
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
    "out.shrinkNote": "{clip} — no seam to re-encode, but the whole clip is transcoded to {share}% to fit the disc",
    "out.shrinkStage": "The whole clip is transcoded to fit the disc",
    "out.conformNote":
      "{clip} — its pictures are not the shape of the master ({master}), so every one of them is written afresh ({why})",
    "out.conformStage": "Written afresh at the master's shape",
    "out.shotsAll": "{clip} — all {frames} frame{frames?s} re-encoded (no stretch of it can be copied)",
    "fit.line": "{what} {theirs} → {master}",
    "fit.codec": "codec",
    "fit.size": "frame size",
    "fit.pixels": "pixel format",
    "fit.rate": "frame rate",
    "fit.scan": "scan",
    "fit.aspect": "pixel aspect",
    "fit.colour": "colour",
    "fit.audioCodec": "audio codec",
    "fit.audioRate": "sample rate",
    "fit.audioChannels": "channels",
    "fit.audioTracks": "audio tracks",
    "out.allCutNote": "{clip} — everything has been cut, so there is nothing to write",
    "out.allCutStage": "Everything has been cut",
    "out.allCut": "Everything has been cut",
    "out.audioReencoded": "(the audio is re-encoded)",
    "out.audioAsCodec": "(the audio is re-encoded as {codec})",
    "out.audioDownmixed": "(the audio is downmixed {from} → {to} and re-encoded)",
    "out.audioUpmixed": "(the audio is spread {from} → {to} and re-encoded)",
    "out.audioConformed": "(the sound is re-encoded to match the master as well)",
    "out.shots": "{clip} — {n} place{n?s} / {frames} frame{frames?s} (everything else is copied byte for byte)",
    "out.ovlKind": "Re-encode {i} of {n}",
    "out.ovlNote": "{n} frame{n?s}",
    "out.aborting": "Stopping (the clip being written now is finished first)",
    "out.skipped": "Stopped",
    "outset.branched": "already there → {name}",
    "out.sameName": "This would overwrite the input",
    "out.branched": "A folder called \"{asked}\" was already there, so this run writes into \"{name}\"",
    "out.writing": "Writing \"{name}\"…",
    "out.writingCopy": "Writing \"{name}\": copying the video losslessly…",
    "out.writingMost":
      "Writing \"{name}\": re-encoding {n} place{n?s}, copying the rest losslessly…",
    "out.writingAll": "Writing \"{name}\": re-encoding the video…",
    "out.writingShrink": "Writing \"{name}\": transcoding the video to {share}% to fit the disc…",
    "out.joinCopy": "Writing \"{name}\": {clip} — copying the video losslessly…",
    "out.joinMost":
      "Writing \"{name}\": {clip} — re-encoding {n} place{n?s}, copying the rest losslessly…",
    "out.joinAll": "Writing \"{name}\": {clip} — re-encoding the video…",
    "out.joinShrink":
      "Writing \"{name}\": {clip} — transcoding the pictures to {share}% to fit the disc…",
    "out.joinConform": "Writing \"{name}\": {clip} — written afresh at the master's shape…",
    "out.writingTables": "Finishing \"{name}\": putting the broadcast's own tables back…",
    "out.done": "Done{extra}",
    "out.doneKeyframes": " / {n} keyframe{n?s}",
    "out.joined": "{n} clips written into \"{name}\"",
    "out.writingCrossings": "Writing \"{name}\": {n} crossing{n?s} written afresh, {secs}s of it…",
    "out.summary": "{done} of {all} written{failed}{aborted}   elapsed {elapsed}",
    "out.summaryFailed": "   {n} failed",
    "out.summaryAborted": "   (stopped)",

    // --- batch export ----------------------------------------------------
    "batch.counts":
      "All: {all}   Running: {run}   Waiting: {wait}   Done: {done}   Failed: {bad}   Stopped: {off}",
    "batch.elapsed": "Elapsed {t}",
    "batch.left": "Left {t}",
    "batch.andElsewhere": "{dir} and {n} other folder{n?s}",
    "batch.andMore": "{name} and {n} more",
    "batch.toDisc": "Writes a disc",
    "batch.toImage": "Disc and image (UDF {udf})",
    "batch.add": "Add jobs",
    "batch.drop": "Remove job",
    "jobmenu.toTop": "Move to the top",
    "jobmenu.toBottom": "Move to the bottom",
    "jobmenu.requeue": "Write it again",
    "jobmenu.openProject": "Open the project",
    "jobmenu.openFolder": "Open the output folder",
    "batch.moreTitle": "Remove in bulk",
    "batch.clearDone": "Remove the jobs already written",
    "batch.clearAll": "Remove all",
    "batch.clearedDone": "{n} written job{n?s} removed",
    "batch.windowTitle": "Batch — SmartCut",
    "batch.opened": "The batch tool has been started",
    "batch.alreadyUp": "The batch tool is already running",
    "batch.queueFailed": "The queue could not be saved: {e}",
    "batch.run": "Start batch",
    "batch.stopAll": "Stop everything",
    "batch.stopJob": "Stop",
    "batch.jobStopped": "Called off",
    "batch.afterHead": "When done",
    "batch.afterNothing": "Nothing",
    "batch.afterSleep": "Sleep",
    "batch.afterShutdown": "Shut down",
    "batch.afterCancel": "Cancel",
    "batch.clips": "{n} clip{n?s}",
    "batch.waiting": "Waiting",
    "batch.opening": "Opening the project…",
    "batch.reading": "Reading the list…",
    "batch.writing": "Writing…",
    "batch.skipped": "Stopped",
    "batch.interrupted": "The tool was closed part way through",
    "batch.stoppedHere": "Stopped part way through",
    "batch.cannotOpen": "The project would not open",
    "batch.nothingReadable": "Not one recording could be read",
    "batch.refused": "The output settings were incomplete",
    "batch.threw": "Something went wrong while writing: {e}",
    "batch.wrote": "{n} written",
    "batch.someFailed": "{n} of {all} failed",
    "batch.already": "Already in the queue: {name}",
    "batch.added": "Added to the queue: {name}",
    "batch.overwritten": "The job in the queue has been overwritten: {name}",
    "batch.stopping": "Stopping…",
    "batch.replaceTitle": "Start the batch",
    "batch.replaceBody":
      "The batch opens each project in the queue in turn, which replaces the list on screen. Unsaved changes to it will be lost. Go ahead?",
    "batch.clearTitle": "Clear the queue",
    "batch.clearBody": "Remove all {n} job{n?s} from the queue?",
    "batch.sleepIn": "Sleeping in {s} second{s?s}",
    "batch.shutdownIn": "Shutting down in {s} second{s?s}",
    "batch.afterCancelled": "The action after the batch was cancelled",
    "batch.afterFailed": "The action after the batch would not run: {e}",

    // --- the cut editor --------------------------------------------------
    "editor.title": "Cut editor",
    "editor.windowTitle": "Cut editor — {clip}",
    "editor.loading": "Loading…",
    "editor.analysing": "Reading…",
    "editor.counterShow": "Counter",
    "editor.counterShow.title": "Draw the frame number and time over the picture",

    // --- the magnifier ----------------------------------------------------
    // --- the seam window ------------------------------------------------
    "xw.title": "Between the clips",
    "xw.windowTitle": "Between the clips - {before} → {after}",
    "xw.loading": "Reading…",
    "xw.whichHead": "Which join",
    "xw.setHead": "The transition",
    "xw.soundHead": "The sound",
    "xw.fadeOut": "The clip before, fading out:",
    "xw.fadeIn": "The clip after, fading in:",
    "xw.fadeTitle":
      "Tenths of a second, up to ten; 0 is no fade. Nothing to do with the transition above -- " +
      "either end can be asked for on its own. It needs sound this program is writing, so set " +
      "Audio on the output screen to smart rendering or a whole re-encode",
    "xw.patternHead": "What it does",
    "xw.allHead": "Every join",
    "xw.clipTime": "Previewed length:",
    "xw.crossTime": "Transition length:",
    "xw.beforeClip": "the clip before",
    "xw.afterClip": "the clip after",
    "xw.crossing": "the crossing",
    "xw.reading": "Reading the two clips…",
    "xw.cannotRead": "The clips could not be read: {e}",
    "xw.open": "Transition…",
    "xw.noJoins": "No joins",
    "xw.cannotOpen": "The seam window would not open: {e}",
    "zoom.title": "Magnifier",
    "zoom.windowTitle": "Magnifier",
    "zoom.open": "Magnifier",
    "zoom.scale": "Zoom",
    "zoom.waiting": "Click the cut editor's picture to magnify that part of it",
    "zoom.at": "{time}   {scale}x   ({x}, {y})",
    "zoom.cannotOpen": "Cannot open the magnifier: {e}",
    "editor.detectCm": "Detect commercials",
    "editor.detectCm.title": "Look for commercials and mark them with keyframes (Ctrl+D)",
    "editor.detectBlank": "Detect blank",
    "editor.detectBlankBlack": "Detect black",
    "editor.detectBlankWhite": "Detect white",
    "editor.detectBlank.title":
      "Find where the picture is flat black or white, whichever Preferences asks for (Ctrl+B); " +
      "both ends of each stretch are " +
      "marked unless Preferences says otherwise, and Alt with the up and down keys walks them",
    "editor.detectSilence": "Detect silence",
    "editor.detectSilence.title":
      "Find where the sound is under the level (Ctrl+Q); both ends of each stretch are marked " +
      "unless Preferences says otherwise, and Alt+Shift with the up and down keys walks them",
    "editor.detecting": "Detecting… (the video is read too)",
    "editor.detectingPct": "Detecting {pct}%",
    "editor.keyframes": "Keyframes",
    "editor.keyCount": "{n}",
    "editor.keyframes.empty":
      "None yet. “⚑ Keyframe” marks wherever you are. Detecting commercials lines up the start of each break and of each part of the programme.",
    "editor.keyframes.emptyManual":
      "None yet. “⚑ Keyframe” marks wherever you are. After a detection, ≡ → “Turn the detection into keyframes” lines up the start of each break and of each part of the programme.",
    "editor.keyframes.kill": "Remove this keyframe",
    "editor.keyframes.from.black": "Black",
    "editor.keyframes.from.white": "White",
    "editor.keyframes.from.quiet": "Quiet",
    "editor.keyframes.fromTitle.black": "The end of a black stretch",
    "editor.keyframes.fromTitle.white": "The end of a white stretch",
    "editor.keyframes.fromTitle.quiet": "The end of a quiet stretch",
    "editor.searching": "Searching",
    "editor.searchKind": "Search",
    "editor.selection": "Selection {a} - {b} : {len}",
    "editor.selectionTime": "Selection {a} - {b} : {len}",
    "editor.selectionNone": "Selection —",
    "editor.counter": "{at} / {all}   {t}",
    "editor.frameKind": "{kind} frame",
    "editor.frameKindPoint": "{kind} frame — lossless point",
    "editor.frameKindNear": "{kind} frame — nearest picture, still reading",
    "editor.previewFailed": "Preview failed: {e}",
    "editor.stripHint":
      "Click to move  /  <b>right-drag</b> to search back and forth (right = forwards, left = back)  /  <b>middle-click</b> for a scene change (right half forwards, left half back)  /  wheel steps a frame (Shift for a GOP)  /  Space plays",
    "editor.stripShow": "Window",
    "strip.win3": "3 s",
    "strip.win6": "6 s",
    "strip.win30": "30 s",
    "strip.win180": "3 min",
    "strip.frame": "Frame by frame",
    "t.addKey": "⚑ Keyframe",
    "t.addKey.title": "Mark the frame you are on as a keyframe (K, or Insert to put one down and take it away)",
    "t.prevScene": "⇤ Scene",
    "t.prevScene.title": "To the previous scene change (↑ / Shift+S)",
    "t.nextScene": "Scene ⇥",
    "t.nextScene.title": "To the next scene change (↓ / S)",
    "t.play": "▶ Play",
    "t.stop": "■ Stop",
    "t.play.title": "Play from here (Space)",
    "t.rewind": "Rewind ─ each press doubles it, 2 to 16, and once more stops",
    "t.fastFwd": "Fast forward ─ each press doubles it, 2 to 16, and once more stops",
    "t.loop": "⟲ Loop",
    "t.loop.title": "Play the selection over and over",
    "t.mute": "Silence the sound, or bring it back (M)",
    "t.volume": "How loud the preview plays. The output is unchanged",
    "t.volume.aria": "Preview volume",
    "t.goStart": "To the start (Home)",
    "t.prevKf": "To the previous lossless point (Shift+↑)",
    "t.stepBack": "Back one frame (←) ─ hold to repeat",
    "t.gotoIn": "Go to the start of the selection",
    "t.setIn": "Start the selection here (I or [)",
    "t.cut": "✂ Cut",
    "t.cutRange": "Take the selection out of the output (Del, or Ctrl+Del for the inside of it alone)",
    "t.setOut": "End the selection here (O or ])",
    "t.gotoOut": "Go to the end of the selection",
    "t.stepFwd": "Forward one frame (→) ─ hold to repeat",
    "t.nextKf": "To the next lossless point (Shift+↓)",
    "t.goEnd": "To the end (End)",
    "t.cutOutside": "Cut outside",
    "t.cutOutside.title": "Take out everything outside the selection",
    "t.snap": "Snap to lossless",
    "t.snap.title": "Move both ends of the selection to the nearest lossless point",
    "t.undo": "↺ Undo",
    "t.undo.title": "Step back to before the last edit (Ctrl+Z)",
    "t.redo": "↻ Redo",
    "t.redo.title": "Put back the edit that was undone (Ctrl+Y)",
    "t.clearAll": "Clear all",
    "t.clearAll.title": "Remove every cut and every keyframe",
    "editor.ok": "OK",
    "editor.cancel": "Cancel",

    // --- subtitles over the preview --------------------------------------
    "subs.label": "Subtitles",
    "subs.off": "Off",
    "subs.kind.caption": "subtitles",
    "subs.kind.superimpose": "crawl",
    "subs.kind.ttml": "subtitles (4K)",
    "subs.kind.graphics": "PGS",
    "subs.kind.subpicture": "subpicture",
    "subs.failed": "Could not read the subtitles: {e}",
    "lang.jpn": "Japanese",
    "lang.eng": "English",
    "lang.und": "unnamed",

    // --- tracks ---------------------------------------------------------
    "tracks.button": "Tracks",
    "tracks.button.title": "Choose which of this recording's streams are written",
    "tracks.title": "Tracks to write",
    "tracks.note":
      "A track switched off is left out of the output. Captions can only be kept when writing a .ts.",
    "tracks.audio": "Sound",
    "tracks.caption": "Captions",
    "tracks.superimpose": "Crawl",
    "tracks.graphics": "Subtitles",
    "tracks.beside": "Subtitles {lang}: inside the cut or beside it, as the output settings say   id 0x{pid}",
    "outset.subtitles": "A disc's subtitles:",
    "subtitles.beside": "Beside the cut (.idx / .sub)",
    "subtitles.sup": "Beside the cut (.sup, PGS untouched)",
    "subtitles.pgs": "Inside the cut (PGS)",
    // Which destination leaves them untouched depends on the disc, so the
    // line says which. See `paintSubtitleChoices`.
    "outset.subtitles.dvd": "A DVD's subtitles:",
    "subtitles.pgs.dvd": "Converted, inside the cut (PGS)",
    "subtitles.beside.dvd": "Beside the cut, untouched (.idx / .sub)",
    "subtitles.sup.dvd": "Converted, beside the cut (.sup)",
    "outset.subtitles.bdmv": "A Blu-ray's subtitles:",
    "subtitles.pgs.bdmv": "Inside the cut, untouched (PGS)",
    "subtitles.beside.bdmv": "Converted, beside the cut (.idx / .sub)",
    "subtitles.sup.bdmv": "Beside the cut, untouched (.sup)",
    "tracks.main": "main",
    "tracks.pid": "PID 0x{pid}",
    "tracks.dropped": "not carried: {what}   PID 0x{pid}",
    "tracks.gone.superimpose": "superimposed text",
    "tracks.data": "data broadcast",
    "tracks.substream": "a compatibility stream folded into this PID",
    "tracks.menu": "a menu",
    "tracks.textst": "text subtitles (the fonts are on the disc)",
    "tracks.settled": "{what} — answered in the preferences   PID 0x{pid}",
    "tracks.settledNote":
      "The data broadcast is answered in the preferences rather than per track here. Only a .ts can hold one, and it is kept by default.",
    "tracks.droppedNote":
      "These are not choices here: superimposed text arrives with no time on its packets, and a menu or a disc's text subtitles live on the disc rather than in the stream.",
    "tracks.substreamNote":
      "A Blu-ray's lossless sound carries an AC-3 core folded into the same PID, for players that cannot decode the rest. Two streams cannot be written back onto one PID, so the track itself (the TrueHD) is written and the core folded inside it is left out.",
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
    "warm.thumbs": "{n} thumbnail{n?s} every {gap}s",
    "warm.scenes": "{n} scene{n?s}",
    "plan.openFile": "Open a file",
    "plan.reading": "Reading the recording. What copies losslessly is known once it has been read",
    "plan.allCut": "Everything has been cut",
    "plan.text":
      "Output {total} ({ranges} range{ranges?s}, {cuts} cut{cuts?s}) — copied losslessly {copied}s ({pct})" +
      " / re-encoded {reencoded}s",
    "plan.lossless": "Video completely lossless",
    "plan.reencoded": "{n} frame{n?s} re-encoded",
    "plan.reencodedTime": "{t} s re-encoded",
    "plan.segCopy": "copy      ",
    "plan.segEncode": "re-encode ",
    "plan.failed": "Cannot plan: {e}",
    "keyframes.readFailed": "Cannot read the keyframes: {e}",
    "keyframes.read": "Read {n} keyframe{n?s} from {file}",
    "keyframes.chapters": "Read {n} chapter{n?s} off the disc as keyframes",
    "editor.more.title":
      "The mark files, the disc's chapters, and clearing the marks. A keyframe list (.keyframe) " +
      "is the marks alone, an AviSynth script (.trim.avs) is the ranges that survive, a saved " +
      "detection (.cm.json) is what a detection made of the recording. Ctrl+H, Ctrl+Shift+H and " +
      "Ctrl+Alt+H write all three beside the recording with nothing to answer.",
    "marks.save.keyframe": "Save the keyframe list…",
    "marks.save.trim": "Save the cuts as an AviSynth Trim…",
    "marks.save.cm": "Save the detection…",
    "marks.load.keyframe": "Read a keyframe list…",
    "marks.load.trim": "Read cuts from an AviSynth Trim…",
    "marks.load.cm": "Read a saved detection…",
    "marks.readNone": "Nothing to read in {file}",
    "editor.chapterKeys": "Turn the disc's chapters into keyframes",
    "editor.cmKeys": "Turn the detection into keyframes",
    "editor.flatKeys": "Turn the {what} stretches into keyframes",
    "editor.clearKeys": "Remove every keyframe",
    "marks.kind.keyframe": "Keyframe list",
    "marks.kind.trim": "AviSynth script",
    "marks.kind.cm": "Saved detection",
    "marks.saved": "Saved {n} keyframe{n?s} to {file}",
    "marks.saveFailed": "Cannot save: {e}",
    "marks.overwriteTitle": "Already there",
    "marks.overwriteBody": "{file} is already there. Write over it?",
    "editor.dropTitle": "Discard the edit",
    "editor.dropBody":
      "Everything done to this recording in here will be discarded. Close the window?",
    "trim.saved": "Saved {n} range{n?s} to {file}",
    "trim.read": "Read {n} cut{n?s} from {file}",
    "trim.readFailed": "Cannot read the Trim line: {e}",
    "cm.saved": "Saved {n} block{n?s} to {file}",
    "cm.read": "Read {n} block{n?s} from {file}",
    "cm.readFailed": "Cannot read the detection: {e}",
    "cm.marked": "Turned {n} block{n?s} into keyframes",
    "cm.nothing": "Nothing has been detected yet",
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
///
/// `{name?s}` is the other form: not the number but what the number does to
/// the word after it. It writes `s` unless `vars.name` is exactly 1, so
/// `{cuts} cut{cuts?s}` is "1 cut" and "2 cuts". English needs it and
/// Japanese does not, which is why it appears in one catalogue and not the
/// other -- and why the line it belongs to is the English line rather than
/// something `t` works out for both. The suffix is whatever is written
/// there, so `{n} box{n?es}` works the same way.
function fill(text, vars) {
  if (!vars) return text;
  return text.replace(/\{(\w+)(?:\?([^{}]*))?\}/g, (whole, name, plural) => {
    if (!(name in vars)) return whole;
    if (plural === undefined) return String(vars[name]);
    return Number(vars[name]) === 1 ? "" : plural;
  });
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
