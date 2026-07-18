# Romly.Tauri.Common

複数の Tauri アプリで共有する処理をまとめた Rust クレート。ウィンドウ配置の永続化、ネイティブな外観、枠なしウィンドウ向けのタイトルバー操作を提供する。

正本リポジトリはこれ一つで、各アプリへは git subtree で内包する。取り込み先ではアプリの `Cargo.toml` から path 依存で参照するため、アプリのリポジトリは単体で clone すればビルドできる。

## 提供するもの

### `window_state` — ウィンドウ配置の永続化

位置・サイズ・最大化状態を、アプリのデータディレクトリ直下の `window-state.json` へ書き出し、次回起動時に復元する。座標と寸法は物理ピクセルで持ち、保存時の拡大率もあわせて記録する。

復元にあたっては現在のモニター構成へ合わせて配置を補正する。保存位置に最も大きく重なるモニターを復元先に選び、拡大率が変わっていれば見た目の大きさを保つよう寸法を換算し、モニターの作業領域(タスクバー等を除いた領域)に収まる大きさへ抑え、タイトルバーを掴めない位置であれば作業領域内へ収め直す。モニターの取り外し・解像度変更・拡大率変更のいずれが起きても、ウィンドウが画面外へ消えない。

復元は位置とサイズを整えるだけでウィンドウを表示はしない。`visible: false` で生成し、バックドロップを当ててから自前で `show()` するアプリと衝突しないようにするため。

復元はプラグインではなくアプリの `setup` から `restore` を呼んで行う。プラグインのウィンドウ生成フックはメインスレッドのキューを経由して呼ばれるため `setup` より後ろへずれ込むうえ、その時点ではウィンドウが `Window` としてしか登録されておらず `WebviewWindow` としては引けないため、表示前に配置を整える用途には使えない。

### `appearance` — ネイティブな外観

システムバックドロップ(Windows 11 では Mica、それ未満では Acrylic、macOS では Vibrancy)の適用と、OS のアクセント色の取得。アクセント色はレジストリの `HKCU\Software\Microsoft\Windows\DWM\AccentColor` から読み、`#rrggbb` で返す。

### `titlebar` — 自作タイトルバー向けのウィンドウ操作

`decorations: false` のウィンドウで、最小化・最大化・ドラッグ移動・閉じるを起こすコマンド群。ウィンドウ操作を JS プラグイン経由ではなく invoke へ揃えることで、アプリの capabilities に window 系の権限を並べずに済ませる。

## 使い方

`Cargo.toml` で path 依存として参照する。

```toml
[dependencies]
romly-tauri-common = { path = "External/romly-tauri-common" }
```

`lib.rs` でプラグインを組み込み、コマンドを登録する。

```rust
tauri::Builder::default()
    .plugin(romly_tauri_common::window_state::plugin())
    .setup(|app| {
        // 前回の配置を復元し、バックドロップを当ててから見せる。
        romly_tauri_common::window_state::restore(app.handle());
        romly_tauri_common::apply_backdrop(app.handle());
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
        }
        Ok(())
    })
    .invoke_handler(tauri::generate_handler![
        romly_tauri_common::accent_color,
        romly_tauri_common::win_minimize,
        romly_tauri_common::win_toggle_maximize,
        romly_tauri_common::win_is_maximized,
        romly_tauri_common::win_start_drag,
        romly_tauri_common::win_close,
    ])
```

終了時の書き出しはプラグインが行う。トレイへ畳むなど、終了を経ずに配置を確定させたい契機では `romly_tauri_common::window_state::save(app)` を明示的に呼ぶ。

フロントは最大化状態の変化を `win-maximized` イベントで受け取り、タイトルバーのボタン図形を追従させる。
