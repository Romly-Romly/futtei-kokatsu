# <ruby>払底枯渇<rt>ふっていこかつ</rt></ruby> (Futtei-Kokatsu)

> 🌐 **[English README →](README.md)**

Claudeの使用量を定期的に取得し、棒グラフ表示の他、折れ線による予測、曜日と時間帯による消費傾向などを標示するデスクトップ常駐アプリです。このまま使うとリセット前に枯れてしまうですとか、まだ攻めても平気などが一目瞭然になるよう可視化します。

仕組みとしては `claude -p "/usage"` を定期的に呼んで残量を読み取り、履歴を蓄積、理想ペースの対角線と「リセット時点での着地」の投影を重ねたバーンダウンチャートを描きます。トレイアイコンにも円グラフで描くので、ウィンドウを開かずとも現在の使用量がわかります。

> **これは初期リリースです。** スパイクに毛が生えた程度のものとお考え下さい。

![払底枯渇](images/screenshot_dark.png)


## ダウンロード

[最新リリース](https://github.com/Romly-Romly/futtei-kokatsu/releases/latest) からインストーラをダウンロードして下さい。

### Windows

- **インストーラ版** — `*-setup.exe`

コード署名をしていないため、初回起動時に Windows SmartScreen の警告が表示されます。「詳細情報」をクリックし、「実行」を選ぶと起動できます。自己責任でどうぞ。

### macOS

リリース予定。



## 動作環境

| OS | バージョン |
|---|---|
| Windows | 10 / 11 (64bit) |

**Claude Code が必要です。** 本アプリは `claude` コマンドラインツールを呼び出して利用枠を読み取るため、[Claude Code](https://www.claude.com/product/claude-code) がインストールされ、Claude のサブスクにログイン済みである必要があります。



## 使い方

起動してトレイに常駐させておくだけです。10分ごと（と起動直後に1回）自動で取得するので、メーターとチャートは放っておいても埋まっていきます。 `claude -p "/usage"` で使用量が返ってくるようになっていれば、特に設定は不要です。

- チャートはホイールで横ズーム、Ctrl+ホイールで縦ズーム、ドラッグで移動できます。ダブルクリックで全体表示に戻ります。

- ウィンドウを閉じても終了せずトレイへ隠れます。完全に終了するにはトレイアイコンから **終了** を選んで下さい。

- 設定（テーマ・表示言語・日付の表示形式・消費傾向の表示・ログイン時に起動）は歯車アイコンの中にあります。



## 設定

### 設定の保存先

設定と蓄積した履歴は OS のユーザーデータ領域に保存されます。

| OS | パス |
|---|---|
| Windows | `%APPDATA%\com.romly.futteikokatsu\` |
| macOS | `~/Library/Application Support/com.romly.futteikokatsu/` |

`settings.json` に各種設定、`history.jsonl` に取得した利用枠の履歴が入ります。アンインストール時は必要に応じて削除して下さい。

### 自動起動

ログイン時に起動を有効にしていた場合、 `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run` に登録されます。これはアンインストール時に削除されるはずです。



## 更新履歴

**[CHANGELOG](CHANGELOG.ja.md)** を見てね。



## ライセンス

[GNU General Public License version 3](LICENSE) (GPL-3.0)

Copyright (C) 2026 Romly

このプログラムはフリーソフトウェアです。GPL-3 に従い、再頒布および改変ができます。改変版を頒布する場合は、同じ GPL-3.0 の下でソースコードを公開する必要があります。
