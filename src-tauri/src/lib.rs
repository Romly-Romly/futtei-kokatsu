// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Romly

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde::{Deserialize, Serialize};
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
// クリック種別の判定は Windows 側のトレイイベント処理でしか使わない。macOS はメニュー表示をビルダー設定に委ねるためこれらの型を参照しない。
#[cfg(not(target_os = "macos"))]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::image::Image;
use tauri::{Emitter, Manager, Theme};

// 利用枠を取得する間隔。窓の開閉と無関係に常駐スレッドがこの周期で測り続ける。
const POLL_INTERVAL: Duration = Duration::from_secs(600);

// フォーカスを失ってから自動でトレイへ畳むまでの猶予。枠なしウィンドウの縁を掴んでリサイズを始めると、窓は前面のままなのに一過性のフォーカス喪失イベントが飛ぶ。これを真に受けて即座に畳むとリサイズできないため、この間だけ待って本当に前面を失ったままかを確かめてから畳む。
const BLUR_HIDE_GRACE: Duration = Duration::from_millis(150);

// トレイアイコンを生成後に取り出して操作するための固定 id。ツールチップ更新コマンドが tray_by_id で参照する。
const TRAY_ID: &str = "main-tray";

// /usage の各メーター行から消費%とリセット時刻文字列を取り出す正規表現。中点はU+00B7。
static RE_SESSION: LazyLock<Regex> = LazyLock::new(|| {
	Regex::new(r"Current session:\s*(\d+)% used(?:\s*·\s*resets\s*(.+))?").unwrap()
});

// 週次枠(全モデル)のメーター行を照合する。
static RE_WEEK_ALL: LazyLock<Regex> = LazyLock::new(|| {
	Regex::new(r"Current week \(all models\):\s*(\d+)% used(?:\s*·\s*resets\s*(.+))?").unwrap()
});

// 週次枠(Sonnet のみ)のメーター行を照合する。この枠は reset 表記を伴わない。
static RE_WEEK_SONNET: LazyLock<Regex> = LazyLock::new(|| {
	Regex::new(r"Current week \(Sonnet only\):\s*(\d+)% used").unwrap()
});

// 1つの利用枠メーター。resets は Sonnet 専用枠のように省略される場合があるため Option とする。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meter {
	pub used_pct: u8,
	pub resets: Option<String>,
}

// /usage が返す3メーターをまとめた結果。raw は表示・診断のため整形前テキストを保持する。
#[derive(Debug, Clone, Serialize)]
pub struct Usage {
	pub session: Option<Meter>,
	pub week_all: Option<Meter>,
	pub week_sonnet: Option<Meter>,
	pub raw: String,
}

// 1時点の測定結果。時系列として履歴ファイルへ1行ずつ蓄積する。嵩む raw は保存しない。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Sample {
	ts: u64,
	session: Option<Meter>,
	week_all: Option<Meter>,
	week_sonnet: Option<Meter>,
}


// 設定ウィンドウで操作する永続設定。theme と language は将来の全面ローカライズも見据えて文字列で持つ。show_trend は消費傾向ヒートマップの表示有無、date_format は日付の表示形式、heat_palette は消費傾向ヒートマップの配色(standard/parula/turbo/gray)。tray_style はトレイ(メニューバー)アイコンの図柄で "burndown-session"(セッション枠の簡易バーンダウン)・"burndown-week"(週次枠の簡易バーンダウン)・"gauge"(リング＋扇形のゲージ)のいずれか。hide_on_blur はウィンドウがフォーカスを失った時に自動でトレイへ隠すか。serde(default) を付け、項目が増えても古い設定ファイルが読めるようにする。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
	theme: String,
	language: String,
	show_trend: bool,
	date_format: String,
	heat_palette: String,
	tray_style: String,
	hide_on_blur: bool,
}

impl Default for Settings {
	fn default() -> Self {
		Settings {
			theme: "system".to_string(),
			language: "system".to_string(),
			show_trend: true,
			date_format: "intl".to_string(),
			heat_palette: "standard".to_string(),
			tray_style: "burndown-session".to_string(),
			hide_on_blur: false,
		}
	}
}










// 起動する claude 実行ファイルのパスを決める。シェルを介さず直接起動して引数化けを避けるため、Windows では npm グローバル配下の claude.exe を優先する。
fn resolve_claude_bin() -> PathBuf {
	// 環境変数による明示指定があれば最優先で使う。
	if let Ok(p) = std::env::var("CLAUDE_BIN") {
		return PathBuf::from(p);
	}

	#[cfg(windows)]
	{
		if let Ok(appdata) = std::env::var("APPDATA") {
			let exe = PathBuf::from(appdata)
				.join("npm/node_modules/@anthropic-ai/claude-code/bin/claude.exe");
			if exe.exists() {
				return exe;
			}
		}
		PathBuf::from("claude.exe")
	}

	#[cfg(not(windows))]
	{
		PathBuf::from("claude")
	}
}










// claude を直接起動して /usage の JSON を取得し、結果テキストを返す。標準入力は空のまま閉じることで非対話起動時の待ち時間を避ける。
fn fetch_usage_text() -> Result<String, String> {
	let bin = resolve_claude_bin();
	let mut cmd = Command::new(&bin);
	cmd.args(["-p", "/usage", "--output-format", "json"])
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped());

	// 起動する claude はコンソールアプリのため、GUI プロセスから直接起動すると既定では起動のたびにコンソール窓が開く。Windows では CREATE_NO_WINDOW を付けてこれを抑止する。
	#[cfg(windows)]
	{
		use std::os::windows::process::CommandExt;
		const CREATE_NO_WINDOW: u32 = 0x0800_0000;
		cmd.creation_flags(CREATE_NO_WINDOW);
	}

	let output = cmd
		.output()
		.map_err(|e| format!("claude の起動に失敗しました ({}): {}", bin.display(), e))?;

	if !output.status.success() {
		return Err(format!(
			"claude が異常終了しました (code {:?}): {}",
			output.status.code(),
			String::from_utf8_lossy(&output.stderr).trim()
		));
	}

	let stdout = String::from_utf8_lossy(&output.stdout);
	let envelope: serde_json::Value = serde_json::from_str(&stdout)
		.map_err(|e| format!("JSON エンベロープの解析に失敗しました: {}", e))?;

	// 取得が正常に完了したことを is_error と subtype の双方で確認する。
	let ok = envelope.get("is_error").and_then(|v| v.as_bool()) == Some(false)
		&& envelope.get("subtype").and_then(|v| v.as_str()) == Some("success");
	if !ok {
		return Err("claude が success 以外の結果を返しました".to_string());
	}

	let result = envelope
		.get("result")
		.and_then(|v| v.as_str())
		.ok_or("エンベロープに result テキストがありません")?;
	Ok(result.to_string())
}










// 1メーター分を正規表現で照合し、消費%とリセット時刻文字列を取り出す。
fn parse_meter(re: &Regex, text: &str) -> Option<Meter> {
	let caps = re.captures(text)?;
	let used_pct = caps.get(1)?.as_str().parse().ok()?;
	let resets = caps.get(2).map(|m| m.as_str().trim().to_string());
	Some(Meter { used_pct, resets })
}










// /usage の整形テキストから3メーターを抽出する。
fn parse_usage(text: &str) -> Usage {
	Usage {
		session: parse_meter(&RE_SESSION, text),
		week_all: parse_meter(&RE_WEEK_ALL, text),
		week_sonnet: parse_meter(&RE_WEEK_SONNET, text),
		raw: text.to_string(),
	}
}










// 診断メッセージへ載せるため、応答テキストを空白を畳んだ1行にして先頭だけ切り出す。多バイト文字を割らないよう文字単位で数える。
fn excerpt(text: &str) -> String {
	let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
	const LIMIT: usize = 200;
	let chars: Vec<char> = collapsed.chars().collect();
	if chars.len() > LIMIT {
		format!("{}…", chars[..LIMIT].iter().collect::<String>())
	} else {
		collapsed
	}
}










// claude から利用枠を取得し3メーターへ分解する。表示と履歴蓄積の双方がこの経路を通る。1枠も読み取れなかったときは応答の冒頭を添えてエラーとし、原因を追える形にするとともに空サンプルの蓄積を防ぐ。
fn fetch_usage() -> Result<Usage, String> {
	let text = fetch_usage_text()?;
	let usage = parse_usage(&text);
	if usage.session.is_none() && usage.week_all.is_none() && usage.week_sonnet.is_none() {
		return Err(format!(
			"claude の応答から利用枠を読み取れませんでした。応答冒頭: {}",
			excerpt(&usage.raw)
		));
	}
	Ok(usage)
}










// 現在時刻を Unix エポックからのミリ秒で返す。
fn now_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_millis() as u64)
		.unwrap_or(0)
}










// 履歴ファイル(JSON Lines)のパス。アプリのデータディレクトリ直下に置く。
fn history_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
	let dir = app
		.path()
		.app_data_dir()
		.map_err(|e| format!("データディレクトリの取得に失敗しました: {}", e))?;
	Ok(dir.join("history.jsonl"))
}










// 1サンプルを履歴ファイルへ追記する。親ディレクトリが無ければ作る。
fn append_sample_to(path: &Path, sample: &Sample) -> Result<(), String> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).map_err(|e| format!("ディレクトリの作成に失敗しました: {}", e))?;
	}
	let line = serde_json::to_string(sample).map_err(|e| format!("サンプルの直列化に失敗しました: {}", e))?;
	let mut file = OpenOptions::new()
		.create(true)
		.append(true)
		.open(path)
		.map_err(|e| format!("履歴ファイルを開けませんでした: {}", e))?;
	writeln!(file, "{}", line).map_err(|e| format!("履歴への書き込みに失敗しました: {}", e))?;
	Ok(())
}










// 取得した利用枠を現在時刻付きのサンプルにして履歴へ追記する。
fn append_sample(app: &tauri::AppHandle, usage: &Usage) -> Result<(), String> {
	let sample = Sample {
		ts: now_ms(),
		session: usage.session.clone(),
		week_all: usage.week_all.clone(),
		week_sonnet: usage.week_sonnet.clone(),
	};
	append_sample_to(&history_path(app)?, &sample)
}










// 履歴ファイルを読み、各行をサンプルへ復元する。ファイルが無ければ空とし、壊れた行は飛ばす。
fn read_history_from(path: &Path) -> Result<Vec<Sample>, String> {
	let text = match fs::read_to_string(path) {
		Ok(t) => t,
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
		Err(e) => return Err(format!("履歴ファイルの読み込みに失敗しました: {}", e)),
	};
	let samples = text
		.lines()
		.filter(|line| !line.trim().is_empty())
		.filter_map(|line| serde_json::from_str::<Sample>(line).ok())
		.collect();
	Ok(samples)
}










// 蓄積済みの時系列サンプルを古い順に返す。
fn read_history(app: &tauri::AppHandle) -> Result<Vec<Sample>, String> {
	read_history_from(&history_path(app)?)
}










// 設定ファイル(JSON)のパス。履歴と同じくアプリのデータディレクトリ直下に置く。
fn settings_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
	let dir = app
		.path()
		.app_data_dir()
		.map_err(|e| format!("データディレクトリの取得に失敗しました: {}", e))?;
	Ok(dir.join("settings.json"))
}










// 設定ファイルを読む。ファイルが無い・壊れている場合は既定値を返し、初回起動でも破綻しないようにする。
fn read_settings(app: &tauri::AppHandle) -> Settings {
	let path = match settings_path(app) {
		Ok(p) => p,
		Err(_) => return Settings::default(),
	};
	match fs::read_to_string(&path) {
		Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
		Err(_) => Settings::default(),
	}
}










// 設定ファイルへ書き出す。親ディレクトリが無ければ作る。後から見て分かるよう整形して保存する。
fn write_settings(app: &tauri::AppHandle, settings: &Settings) -> Result<(), String> {
	let path = settings_path(app)?;
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).map_err(|e| format!("ディレクトリの作成に失敗しました: {}", e))?;
	}
	let text = serde_json::to_string_pretty(settings).map_err(|e| format!("設定の直列化に失敗しました: {}", e))?;
	fs::write(&path, text).map_err(|e| format!("設定ファイルの書き込みに失敗しました: {}", e))?;
	Ok(())
}










// 設定の theme 文字列を Tauri のテーマ指定へ移す。light/dark 以外(system など)は None とし、OS のテーマに従わせる。
fn theme_from_setting(theme: &str) -> Option<Theme> {
	match theme {
		"light" => Some(Theme::Light),
		"dark" => Some(Theme::Dark),
		_ => None,
	}
}










// 設定のテーマをメインウィンドウへ適用する。set_theme は webview の prefers-color-scheme を切り替えるため、これだけで CSS のダーク配色が追従する。Windows ではネイティブのタイトルバーの明暗もこの呼び出しに追従する。
fn apply_theme(app: &tauri::AppHandle, settings: &Settings) {
	if let Some(window) = app.get_webview_window("main") {
		let _ = window.set_theme(theme_from_setting(&settings.theme));
	}
}










// フォーカスを失った時に自動でトレイへ隠す設定の現在値。フォーカス変化イベントのたびに設定ファイルを読み直さずに済むよう、起動時と設定保存時にここへ写し取り、ウィンドウイベントハンドラからはこの値を見る。
static HIDE_ON_BLUR: AtomicBool = AtomicBool::new(false);










// 利用枠を周期取得して履歴へ蓄積する常駐スレッドを起こす。起動直後に1度測り、以後 POLL_INTERVAL ごとに測る。取得や追記の失敗は標準エラーへ記録し、計測自体は止めない。
fn start_poller(app: tauri::AppHandle) {
	std::thread::spawn(move || loop {
		match fetch_usage() {
			Ok(usage) => {
				if let Err(e) = append_sample(&app, &usage) {
					eprintln!("履歴の追記に失敗しました: {}", e);
				}
				// 新しいサンプルが採れたことをフロントエンドへ通知し、画面を自動で追従させる。
				let _ = app.emit("usage-updated", &usage);
				// 取得したばかりの消費率をトレイアイコンとウィンドウアイコンへ反映する。
				update_tray_icon(&app, &usage);
				update_window_icon(&app, &usage);
			}
			Err(e) => eprintln!("利用枠の取得に失敗しました: {}", e),
		}
		std::thread::sleep(POLL_INTERVAL);
	});
}










// フロントエンドから呼ばれる残量取得コマンド。現在の利用枠を取得して返す。履歴の蓄積は常駐スレッドが担う。取得した値はトレイアイコンとウィンドウアイコンへも反映し、手動更新でもアイコンが追従するようにする。
#[tauri::command]
fn get_usage(app: tauri::AppHandle) -> Result<Usage, String> {
	let usage = fetch_usage()?;
	update_tray_icon(&app, &usage);
	update_window_icon(&app, &usage);
	Ok(usage)
}










// 蓄積済みの時系列サンプルをフロントエンドへ返すコマンド。
#[tauri::command]
fn get_history(app: tauri::AppHandle) -> Result<Vec<Sample>, String> {
	read_history(&app)
}










// 永続設定をフロントエンドへ返すコマンド。初回はファイルが無いため既定値が返る。
#[tauri::command]
fn get_settings(app: tauri::AppHandle) -> Settings {
	read_settings(&app)
}










// フロントエンドから受け取った設定を保存し、テーマとトレイアイコンを即座に反映するコマンド。トレイの図柄(バーンダウン/ゲージ)や対象枠の切替を、新たな取得を待たずに直近の値で描き直す。
#[tauri::command]
fn set_settings(app: tauri::AppHandle, settings: Settings) -> Result<(), String> {
	write_settings(&app, &settings)?;
	apply_theme(&app, &settings);
	// フォーカス喪失時に隠す設定はウィンドウイベントハンドラが参照するため、保存のたびにランタイムのフラグへ写す。
	HIDE_ON_BLUR.store(settings.hide_on_blur, Ordering::Relaxed);
	// 設定画面の図柄ピッカーから変えたときも、トレイメニューの図柄選択のチェックを現在値へ合わせる。
	sync_tray_style_checks(&app, &settings.tray_style);
	if let Some(usage) = latest_usage(&app) {
		update_tray_icon(&app, &usage);
	}
	Ok(())
}










// ログイン時の自動起動が現在有効かを返すコマンド。登録状態は settings.json ではなくレジストリの Run キーが持つため、プラグイン経由で実際の登録有無を読む。利用者がタスクマネージャー等から外部で変えていても実状態を映せる。読み取りに失敗したときは未登録として false を返す。
#[tauri::command]
fn get_autostart(app: tauri::AppHandle) -> bool {
	use tauri_plugin_autostart::ManagerExt;
	app.autolaunch().is_enabled().unwrap_or(false)
}










// ログイン時の自動起動を登録・解除するコマンド。enable でレジストリの Run キーへ起動コマンドを書き、disable でその値を消す。
#[tauri::command]
fn set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
	use tauri_plugin_autostart::ManagerExt;
	let manager = app.autolaunch();
	let outcome = if enabled { manager.enable() } else { manager.disable() };
	outcome.map_err(|e| format!("自動起動の設定変更に失敗しました: {}", e))
}










// フロントが組み立てた要約一行をトレイのツールチップへ反映する。窓を隠していても webview は生きているため、隠したままでも最新の判定をトレイへ降ろせる。
#[tauri::command]
fn set_tray_tooltip(app: tauri::AppHandle, text: String) -> Result<(), String> {
	if let Some(tray) = app.tray_by_id(TRAY_ID) {
		tray
			.set_tooltip(Some(text))
			.map_err(|e| format!("トレイのツールチップ更新に失敗しました: {}", e))?;
	}
	Ok(())
}










// 右クリックメニューの「終了」から呼ぶ。トレイの終了と同じく、CloseRequested を経ずにプロセスを終えるため、ここで現在のウィンドウ配置を保存してから抜ける。保存しないと最後の移動・リサイズが次回起動へ残らない。
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
	romly_tauri_common::window_state::save(&app);
	app.exit(0);
}




// タイトルバーへ表示するアプリのバージョンを返すコマンド。文字列は tauri.conf.json 由来の PackageInfo から引くため、バージョンを変えてもここは追従する。トレイメニューの見出しと同じ出所にすることで、表示が二重管理にならないようにする。
#[tauri::command]
fn app_version(app: tauri::AppHandle) -> String {
	app.package_info().version.to_string()
}










// メインウィンドウを表示して前面に出す。トレイからの復帰に使う。
fn show_main_window(app: &tauri::AppHandle) {
	if let Some(window) = app.get_webview_window("main") {
		let _ = window.show();
		let _ = window.set_focus();
	}
}




// 自動起動のランチャから起動されたかを、起動引数に --autostart が含まれるかで判定する。ログイン時の自動起動はウィンドウを出さずトレイのみで常駐させ、手動起動と区別するために使う。自動起動を仕込む側は、登録する起動コマンドへ --autostart を付ける。
fn launched_at_startup() -> bool {
	std::env::args().any(|arg| arg == "--autostart")
}










// 設定ビューを開く。メインウィンドウ内で表示を切り替える方式のため、窓を前面に出してからフロントへ設定ビューへ移る合図を送る。窓を隠している状態からトレイ経由で開く場合にも前面化が要る。
fn open_settings(app: &tauri::AppHandle) {
	show_main_window(app);
	let _ = app.emit("open-settings", ());
}




// macOS のアプリメニューの「更新」から呼ぶ。窓を前面に出してからフロントへ再取得の合図を送り、フロントの refresh(get_usage 経由の再取得と履歴・アイコンの更新)を起こす。窓を隠した状態から呼んでも結果が見えるよう、設定を開く経路と同じく前面化を伴う。
#[cfg(target_os = "macos")]
fn trigger_refresh(app: &tauri::AppHandle) {
	show_main_window(app);
	let _ = app.emit("trigger-refresh", ());
}










// 配色定数の [R, G, B, A] バイト列を tiny-skia の色へ変換する。色定数は素のバイト列で持つため、描画へ渡す手前でこの関数を通す。
fn icon_color(rgba: [u8; 4]) -> tiny_skia::Color {
	tiny_skia::Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3])
}




// トレイへ描く円グラフの色を消費%から決める。消費%の閾値で ICON_GAUGE_LOW / MID / HIGH を選び、枠が逼迫するほど警戒色へ寄せる。
fn gauge_color(pct: f32) -> tiny_skia::Color {
	if pct >= 80.0 {
		icon_color(ICON_GAUGE_HIGH)
	} else if pct >= 50.0 {
		icon_color(ICON_GAUGE_MID)
	} else {
		icon_color(ICON_GAUGE_LOW)
	}
}










// トレイアイコンの寸法を調整する定数。いずれも辺長に対する比率(0.0〜0.5)で、小さい寸法でも比率が保たれる。値を変えてビルドし直すと見た目へ反映される。

// 外周リングの外側に空ける余白(小さいほどリングが大きい)
const ICON_RING_MARGIN: f32 = 0.00;
// 外周リング(週間枠)の帯の太さ
const ICON_RING_THICKNESS: f32 = 0.150;
// 外周リングの内縁と中央円の間の隙間。
const ICON_INNER_GAP: f32 = 0.08;

// 中央の円(5時間枠)の消費%の見せ方を選ぶ。CenterStyle::Fill は下から上へ水位のように塗り上げる方式、CenterStyle::Pie は12時起点・時計回りの扇形(円グラフ)方式。
const ICON_CENTER_STYLE: CenterStyle = CenterStyle::Pie;



// トレイアイコンの配色。状態を表す色はテーマに追従させず固定値にする。
// いずれも [R, G, B, A] のバイト列で、末尾の A は不透明度(0〜255)。使う側は icon_color で tiny-skia の色へ変換する。

// ゲージ一式の背後に敷く下地円。タスクバーの地色やテーマ色にアイコンが同化して見えなくならないよう、ゲージの背後を覆って図形を浮かせる。A を下げると下地が薄くなる。
const ICON_BACKDROP_COLOR: [u8; 4] = [0, 0, 0, 200];
// 空きを示すトラック。黒い下地の上に乗せて空き部分を示す不透明の灰。外周リングのトラックと中央円の容器に共通で使う。
const ICON_TRACK_COLOR: [u8; 4] = [98, 98, 98, 255];
// 消費%ごとのゲージ色。枠が逼迫するほど警戒色へ寄せる。50%未満は LOW、50%以上80%未満は MID、80%以上は HIGH を使う。
const ICON_GAUGE_LOW: [u8; 4] = [51, 248, 159, 255];
const ICON_GAUGE_MID: [u8; 4] = [246, 180, 48, 255];
const ICON_GAUGE_HIGH: [u8; 4] = [247, 113, 98, 255];



// ICON_CENTER_STYLE で選ばれていない側の variant は値として構築されず dead_code 警告が出るが、切り替え用に両方を常に残すため許容する。
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq)]
enum CenterStyle {
	Fill,
	Pie,
}



// 指定の辺長(px)で、外周のリングゲージと中央の円ゲージを一枚に描き、ストレートアルファの RGBA バイト列を返す。外側のリングは週間枠(week)の消費%を12時方向から時計回りの弧で表す。中央の円はセッション(5時間枠, session)の消費%を表し、ICON_CENTER_STYLE で下から上への塗り上げ(Fill)か扇形の円グラフ(Pie)かを選ぶ。値の無い枠は薄い灰のトラック・容器だけを残す。tiny-skia の画素は乗算済みアルファのため、CreateIcon が前提とするストレートアルファへ戻してから返す。
fn render_gauge_rgba(size: u32, session: Option<f32>, week: Option<f32>) -> Vec<u8> {
	let s = size as f32;
	let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("ゲージ用 Pixmap の確保に失敗しました");

	let cx = s / 2.0;
	let cy = s / 2.0;

	// 外周リング。余白を切り詰めて範囲一杯まで広げ、帯は控えめに細くして中央の円へ余地を残す。ring_mid は帯の中央を通す半径、ring_inner は帯の内縁の半径。
	let margin = (s * ICON_RING_MARGIN).max(0.5);
	let outer = s / 2.0 - margin;
	let thickness = (s * ICON_RING_THICKNESS).max(1.5);
	let ring_mid = outer - thickness / 2.0;
	let ring_inner = outer - thickness;

	// 中央の塗り上がり円。リングの内縁から少し離し、独立した円に見せる。
	let gap = (s * ICON_INNER_GAP).max(0.5);
	let inner_r = (ring_inner - gap).max(1.0);

	let track = icon_color(ICON_TRACK_COLOR);

	// タスクバーの地色から切り離すための下地円。最初に敷くことでゲージ一式の背後に回し、外周リングの外縁(outer)へ半径を合わせて全体を一つの円形へ収める。
	let mut bb = tiny_skia::PathBuilder::new();
	bb.push_circle(cx, cy, outer);
	if let Some(backdrop) = bb.finish() {
		let mut paint = tiny_skia::Paint::default();
		paint.anti_alias = true;
		paint.set_color(icon_color(ICON_BACKDROP_COLOR));
		pixmap.fill_path(&backdrop, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
	}

	// 外周リングの背景トラック(全周)。
	let mut tb = tiny_skia::PathBuilder::new();
	tb.push_circle(cx, cy, ring_mid);
	if let Some(track_path) = tb.finish() {
		let mut paint = tiny_skia::Paint::default();
		paint.anti_alias = true;
		paint.set_color(track);
		let stroke = tiny_skia::Stroke { width: thickness, line_cap: tiny_skia::LineCap::Butt, ..Default::default() };
		pixmap.stroke_path(&track_path, &paint, &stroke, tiny_skia::Transform::identity(), None);
	}

	// 週間枠の消費%ぶんの弧。12時(-90度)を起点に時計回りへ掃く。tiny-skia に弧の基本図形が無いため、約2度刻みの折れ線を帯の太さへ太らせてリングにする。線端は角(Butt)で切って弧の長さを消費%ぴったりに合わせる。丸めると両端が帯の太さの半分ぶん膨らみ、実際より多い消費量に見えてしまう。
	if let Some(w) = week {
		let w = w.clamp(0.0, 100.0);
		if w > 0.0 {
			let sweep = w / 100.0 * std::f32::consts::TAU;
			let start = -std::f32::consts::FRAC_PI_2;
			let steps = ((sweep / (std::f32::consts::PI / 90.0)).ceil() as i32).max(1);
			let mut ab = tiny_skia::PathBuilder::new();
			for i in 0..=steps {
				let a = start + sweep * (i as f32 / steps as f32);
				let x = cx + ring_mid * a.cos();
				let y = cy + ring_mid * a.sin();
				if i == 0 {
					ab.move_to(x, y);
				} else {
					ab.line_to(x, y);
				}
			}
			if let Some(arc) = ab.finish() {
				let mut paint = tiny_skia::Paint::default();
				paint.anti_alias = true;
				paint.set_color(gauge_color(w));
				let stroke = tiny_skia::Stroke { width: thickness, line_cap: tiny_skia::LineCap::Butt, ..Default::default() };
				pixmap.stroke_path(&arc, &paint, &stroke, tiny_skia::Transform::identity(), None);
			}
		}
	}

	// 中央の円の容器(全体)。空き部分が見えるよう薄い灰で塗る。
	let mut cb = tiny_skia::PathBuilder::new();
	cb.push_circle(cx, cy, inner_r);
	let inner_circle = cb.finish();
	if let Some(circle) = inner_circle.as_ref() {
		let mut paint = tiny_skia::Paint::default();
		paint.anti_alias = true;
		paint.set_color(track);
		pixmap.fill_path(circle, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
	}

	// セッション(5時間枠)の消費%を中央の円に表す。見せ方は ICON_CENTER_STYLE で切り替える。
	if let (Some(circle), Some(sess)) = (inner_circle.as_ref(), session) {
		let sess = sess.clamp(0.0, 100.0);
		if sess > 0.0 {
			let mut paint = tiny_skia::Paint::default();
			paint.anti_alias = true;
			paint.set_color(gauge_color(sess));
			match ICON_CENTER_STYLE {
				// 下から上へ塗り上げる。容器の円を、水位より下を覆う矩形のマスクで切り抜いて色を流し込む。水位の y は消費0%で円の下端(cy + inner_r)、100%で上端(cy - inner_r)に達する。
				CenterStyle::Fill => {
					let water_y = cy + inner_r * (1.0 - 2.0 * sess / 100.0);
					if let (Some(rect), Some(mut mask)) = (tiny_skia::Rect::from_xywh(0.0, water_y, s, s - water_y), tiny_skia::Mask::new(size, size)) {
						mask.fill_path(&tiny_skia::PathBuilder::from_rect(rect), tiny_skia::FillRule::Winding, true, tiny_skia::Transform::identity());
						pixmap.fill_path(circle, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), Some(&mask));
					}
				}
				// 扇形の円グラフ。中心から12時方向へ伸ばし、消費%ぶんを時計回りに掃いた扇形を塗る。tiny-skia に弧の基本図形が無いため、弧の縁を約2度刻みの折れ線で近似する。
				CenterStyle::Pie => {
					let sweep = sess / 100.0 * std::f32::consts::TAU;
					let start = -std::f32::consts::FRAC_PI_2;
					let steps = ((sweep / (std::f32::consts::PI / 90.0)).ceil() as i32).max(1);
					let mut pb = tiny_skia::PathBuilder::new();
					pb.move_to(cx, cy);
					for i in 0..=steps {
						let a = start + sweep * (i as f32 / steps as f32);
						pb.line_to(cx + inner_r * a.cos(), cy + inner_r * a.sin());
					}
					pb.close();
					if let Some(wedge) = pb.finish() {
						pixmap.fill_path(&wedge, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
					}
				}
			}
		}
	}

	// 乗算済みアルファをストレートアルファへ戻し、R,G,B,A の順で詰め直す。
	let mut rgba = Vec::with_capacity((size * size * 4) as usize);
	for px in pixmap.pixels() {
		let c = px.demultiply();
		rgba.push(c.red());
		rgba.push(c.green());
		rgba.push(c.blue());
		rgba.push(c.alpha());
	}
	rgba
}



// トレイのバーンダウンが表す利用枠の窓長(ミリ秒)。セッションは5時間枠、週間は週次(全モデル)枠。窓の起点(リセット時刻から遡った時刻)の算出に使う。
const SESSION_WINDOW_MS: i64 = 5 * 3600 * 1000;
const WEEK_WINDOW_MS: i64 = 7 * 24 * 3600 * 1000;

// 直近ペースを見る窓(経過率)。この区間の区間傾きの中央値を直近の消費ペースとみなす。
const BURNDOWN_RECENT_FRAC: f32 = 0.25;
// 投影率の下限割合。直近ペースが落ちても累積平均ペースのこの割合は下回らせない。短い休止で投影が水平化するのを防ぐ。
const BURNDOWN_FLOOR_FRAC: f32 = 0.5;
// 投影を出すのに要る最小のデータ隔たり(経過率)。実測点の張る範囲がこれ未満の序盤は投影を控え、過大な早期枯渇判定を避ける。
const BURNDOWN_MIN_SPAN: f32 = 0.08;

// トレイのバーンダウンの固定色([R, G, B, A])。塗り面・実測線・投影線はペース色(gauge と共通の ICON_GAUGE_LOW/MID/HIGH)で塗るため、ここには理想線と現在ドットの色だけを持つ。
// 窓の始点(0%)からリセット(100%)を結ぶ理想の対角線。淡い基準線にする。
const BURN_IDEAL_COLOR: [u8; 4] = [150, 150, 150, 150];
// 実測線の先端に置く現在位置ドット。明るい印にする。
const BURN_NOW_COLOR: [u8; 4] = [240, 244, 252, 255];



// tiny-skia の乗算済みアルファ画素を、トレイやウィンドウのアイコンが前提とするストレートアルファへ戻し、R,G,B,A の順の RGBA バイト列にして返す。
fn demultiply_rgba(pixmap: &tiny_skia::Pixmap) -> Vec<u8> {
	let mut rgba = Vec::with_capacity((pixmap.width() * pixmap.height() * 4) as usize);
	for px in pixmap.pixels() {
		let c = px.demultiply();
		rgba.push(c.red());
		rgba.push(c.green());
		rgba.push(c.blue());
		rgba.push(c.alpha());
	}
	rgba
}



// 角を半径 r で丸めた矩形のパスを作る。トレイのバーンダウンの下地に使う。四隅は制御点を角へ置く2次ベジェで丸める。
fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
	let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
	let mut pb = tiny_skia::PathBuilder::new();
	pb.move_to(x + r, y);
	pb.line_to(x + w - r, y);
	pb.quad_to(x + w, y, x + w, y + r);
	pb.line_to(x + w, y + h - r);
	pb.quad_to(x + w, y + h, x + w - r, y + h);
	pb.line_to(x + r, y + h);
	pb.quad_to(x, y + h, x, y + h - r);
	pb.line_to(x, y + r);
	pb.quad_to(x, y, x + r, y);
	pb.close();
	pb.finish()
}



// リセット時刻文字列から壁時計成分を取り出す正規表現。日と時刻の区切りはカンマと " at " の双方を受け、分は省略されうる。タイムゾーン表記は表示ローカルと同じとみなして照合対象にしない。
static RE_RESET: LazyLock<Regex> = LazyLock::new(|| {
	Regex::new(r"(?i)([A-Za-z]{3})\s+(\d{1,2})(?:,|\s+at)\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)").unwrap()
});



// 3文字の英語月名を1起点の月番号(1〜12)へ変換する。先頭を大文字・続く2字を小文字へ正規化してから引く。
fn month_index(name: &str) -> Option<u32> {
	const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
	let mut norm = String::with_capacity(3);
	for (i, c) in name.chars().take(3).enumerate() {
		if i == 0 {
			norm.extend(c.to_uppercase());
		} else {
			norm.extend(c.to_lowercase());
		}
	}
	MONTHS.iter().position(|m| *m == norm).map(|i| i as u32 + 1)
}



// リセット時刻文字列から (月(1〜12), 日, 時(0〜23), 分) の壁時計成分を取り出す。"Jun 23, 4:10am ..." や "Jul 2 at 5:29am ..." の双方に対応し、分は省略されうる。範囲外の値は None として弾く。
fn parse_reset_components(reset: &str) -> Option<(u32, u32, u32, u32)> {
	let caps = RE_RESET.captures(reset)?;
	let month = month_index(caps.get(1)?.as_str())?;
	let day: u32 = caps.get(2)?.as_str().parse().ok()?;
	let mut hour: u32 = caps.get(3)?.as_str().parse().ok()?;
	let min: u32 = match caps.get(4) {
		Some(m) => m.as_str().parse().ok()?,
		None => 0,
	};
	let ap = caps.get(5)?.as_str().to_ascii_lowercase();
	if ap == "pm" && hour != 12 {
		hour += 12;
	}
	if ap == "am" && hour == 12 {
		hour = 0;
	}
	if day == 0 || day > 31 || hour > 23 || min > 59 {
		return None;
	}
	Some((month, day, hour, min))
}



// リセット時刻文字列を Unix ミリ秒へ解釈する。壁時計成分を現在の年の現地時間として組み立て、24時間以上過去へ落ちる場合は年境界とみなして翌年で組み直す。now_ms は現在時刻(Unixミリ秒)で、年の決定と過去判定の基準に使う。壁時計から現地の絶対時刻を得るには現地タイムゾーンの解決が要るため chrono の Local を通す。
fn parse_reset_ms(reset: &str, now_ms: i64) -> Option<i64> {
	use chrono::{Datelike, Local, TimeZone};

	let (month, day, hour, min) = parse_reset_components(reset)?;
	let now = Local.timestamp_millis_opt(now_ms).single()?;
	let build = |year: i32| -> Option<i64> {
		Local
			.with_ymd_and_hms(year, month, day, hour, min, 0)
			.earliest()
			.map(|dt| dt.timestamp_millis())
	};
	let when = build(now.year())?;
	if when < now_ms - 24 * 3600 * 1000 {
		return build(now.year() + 1);
	}
	Some(when)
}



// 線形投影の結果。end は投影線の終点(経過率, 使用%)で、枠を割る見込みならその手前(100%到達点)、割らなければリセット時点まで。枠を割らない場合の end の使用%が、リセット時点の投影使用%になる。hit_t は100%到達の経過率で、到達しないなら None。warn は到達がリセット前(hit_t<1)であること。
struct Projection {
	end: (f32, f32),
	hit_t: Option<f32>,
	warn: bool,
}



// 昇順済み配列の中央値を返す。要素数が偶数のときは中央2点の平均を取る。空なら None。
fn median(sorted: &[f32]) -> Option<f32> {
	if sorted.is_empty() {
		return None;
	}
	let m = sorted.len() / 2;
	if sorted.len() % 2 == 1 {
		Some(sorted[m])
	} else {
		Some((sorted[m - 1] + sorted[m]) / 2.0)
	}
}



// 実測点列(経過率, 使用%)から、現在地(f, used)以降の消費を線形に前方投影する。簡易版として、直近 BURNDOWN_RECENT_FRAC ぶんの区間傾きの中央値を消費率とし、累積平均ペースの一定割合で下支えする(短い休止で投影が寝るのを防ぐ)。実測点の張る範囲が狭い序盤は過大判定を避けるため投影を控えて None を返す。
fn project_linear(samples: &[(f32, f32)], f: f32, used: f32) -> Option<Projection> {
	if samples.len() < 2 || f <= 0.0 || f >= 1.0 {
		return None;
	}
	let first_t = samples.first()?.0;
	let last_t = samples.last()?.0;
	if last_t - first_t < BURNDOWN_MIN_SPAN {
		return None;
	}

	let r_avg = used / f;
	// 直近区間の傾きを集め、中央値を直近ペースとする。端点差分でなく分布の中央値にすることで、一時的な平坦区間に率が引きずられないようにする。
	let cutoff = f - BURNDOWN_RECENT_FRAC;
	let mut slopes: Vec<f32> = Vec::new();
	for pair in samples.windows(2) {
		let (t0, v0) = pair[0];
		let (t1, v1) = pair[1];
		if t1 < cutoff {
			continue;
		}
		let dt = t1 - t0;
		if dt > 0.0 {
			slopes.push((v1 - v0) / dt);
		}
	}
	slopes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
	let rate = match median(&slopes) {
		Some(r) => r.max(BURNDOWN_FLOOR_FRAC * r_avg),
		None => r_avg,
	}
	.max(0.0);

	// used が100以上なら既に使い切りなので枯渇は現在地。それ未満は残容量を率で割って到達を見積もる。率が0なら到達しない。
	let hit_t = if used >= 100.0 {
		Some(f)
	} else if rate > 0.0 {
		Some(f + (100.0 - used) / rate)
	} else {
		None
	};
	let end_t = hit_t.map(|h| h.min(1.0)).unwrap_or(1.0);
	let end_v = used + rate * (end_t - f);
	let warn = hit_t.map(|h| h < 1.0).unwrap_or(false);
	Some(Projection { end: (end_t, end_v), hit_t, warn })
}



// 指定の幅・高さ(px)で、対象枠の簡易バーンダウンを Pixmap へ描く。理想の対角線・使用%の塗り面と実測線・線形投影・現在位置ドットを重ね、リセット前に枯渇する見込みなら枯渇点も打つ。塗り面と線と投影はペース(理想より先行か・枠を割りそうか)で ICON_GAUGE_LOW/MID/HIGH に色づける。samples は窓内の実測点を (経過率0〜1, 使用%0〜100) で古い順に並べ、末尾を現在地とする。空のときは背景と理想線だけを描く。
fn draw_burndown_pixmap(width: u32, height: u32, samples: &[(f32, f32)], proj: Option<&Projection>) -> tiny_skia::Pixmap {
	let w = width as f32;
	let h = height as f32;
	let mut pixmap = tiny_skia::Pixmap::new(width, height).expect("バーンダウン用 Pixmap の確保に失敗しました");

	// 作画域。線やドットが端で欠けない程度の余白を上下左右に取る。
	let pad = (h * 0.16).max(1.5);
	let plot_l = pad;
	let plot_t = pad;
	let plot_w = (w - pad * 2.0).max(1.0);
	let plot_h = (h - pad * 2.0).max(1.0);
	let x = |t: f32| plot_l + t.clamp(0.0, 1.0) * plot_w;
	let y = |v: f32| plot_t + (1.0 - v.clamp(0.0, 100.0) / 100.0) * plot_h;

	// タスクバー/メニューバーの地色から切り離すための下地。角を丸めた矩形で全体を覆う。
	let radius = (h.min(w) * 0.2).max(0.0);
	if let Some(backdrop) = rounded_rect_path(0.5, 0.5, w - 1.0, h - 1.0, radius) {
		let mut paint = tiny_skia::Paint::default();
		paint.anti_alias = true;
		paint.set_color(icon_color(ICON_BACKDROP_COLOR));
		pixmap.fill_path(&backdrop, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
	}

	// 理想の対角線(0%→100%)。淡い基準線として先に敷く。
	let mut ib = tiny_skia::PathBuilder::new();
	ib.move_to(x(0.0), y(0.0));
	ib.line_to(x(1.0), y(100.0));
	if let Some(path) = ib.finish() {
		let mut paint = tiny_skia::Paint::default();
		paint.anti_alias = true;
		paint.set_color(icon_color(BURN_IDEAL_COLOR));
		let stroke = tiny_skia::Stroke { width: (h * 0.03).max(0.8), ..Default::default() };
		pixmap.stroke_path(&path, &paint, &stroke, tiny_skia::Transform::identity(), None);
	}

	// 実測点が無ければ背景と理想線だけで返す。
	let (f, used) = match samples.last() {
		Some(p) => *p,
		None => return pixmap,
	};

	// ペース色。既に使い切り・リセット前に枯渇見込みなら HIGH、理想より先行なら MID、余裕があれば LOW。
	let warn = proj.map(|p| p.warn).unwrap_or(false);
	let pace = if used >= 100.0 || warn {
		ICON_GAUGE_HIGH
	} else if used > 100.0 * f {
		ICON_GAUGE_MID
	} else {
		ICON_GAUGE_LOW
	};
	let line_w = (h * 0.05).max(1.0);

	// 使用%の塗り面と実測線。面は実測線の下(使用0%まで)をペース色の淡い塗りで埋める。
	if samples.len() >= 2 {
		let mut area = tiny_skia::PathBuilder::new();
		area.move_to(x(samples[0].0), y(0.0));
		for &(t, v) in samples {
			area.line_to(x(t), y(v));
		}
		area.line_to(x(f), y(0.0));
		area.close();
		if let Some(path) = area.finish() {
			let mut paint = tiny_skia::Paint::default();
			paint.anti_alias = true;
			paint.set_color(tiny_skia::Color::from_rgba8(pace[0], pace[1], pace[2], 70));
			pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
		}

		let mut ln = tiny_skia::PathBuilder::new();
		for (i, &(t, v)) in samples.iter().enumerate() {
			if i == 0 {
				ln.move_to(x(t), y(v));
			} else {
				ln.line_to(x(t), y(v));
			}
		}
		if let Some(path) = ln.finish() {
			let mut paint = tiny_skia::Paint::default();
			paint.anti_alias = true;
			paint.set_color(icon_color(pace));
			let stroke = tiny_skia::Stroke { width: line_w, line_cap: tiny_skia::LineCap::Round, line_join: tiny_skia::LineJoin::Round, ..Default::default() };
			pixmap.stroke_path(&path, &paint, &stroke, tiny_skia::Transform::identity(), None);
		}
	}

	// 線形投影。現在地から終点(枠を割るならその手前まで)をペース色の細線で伸ばす。実測線と見分けるため少し細く、わずかに透かす。
	if let Some(p) = proj {
		let mut pb = tiny_skia::PathBuilder::new();
		pb.move_to(x(f), y(used));
		pb.line_to(x(p.end.0), y(p.end.1));
		if let Some(path) = pb.finish() {
			let mut paint = tiny_skia::Paint::default();
			paint.anti_alias = true;
			paint.set_color(tiny_skia::Color::from_rgba8(pace[0], pace[1], pace[2], 210));
			let stroke = tiny_skia::Stroke { width: (line_w * 0.8).max(0.8), line_cap: tiny_skia::LineCap::Round, ..Default::default() };
			pixmap.stroke_path(&path, &paint, &stroke, tiny_skia::Transform::identity(), None);
		}
		// リセット前に枯渇する見込みなら、100%到達点を警戒色で打つ。現在地より先で、かつ枠内のときだけ。
		if p.warn {
			if let Some(hit) = p.hit_t {
				if hit > f && hit <= 1.0 {
					let mut cb = tiny_skia::PathBuilder::new();
					cb.push_circle(x(hit), y(100.0), (h * 0.09).max(1.5));
					if let Some(path) = cb.finish() {
						let mut paint = tiny_skia::Paint::default();
						paint.anti_alias = true;
						paint.set_color(icon_color(ICON_GAUGE_HIGH));
						pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
					}
				}
			}
		}
	}

	// 現在位置ドット。実測線の先端(現在地)へ明るい印を置く。
	let mut nb = tiny_skia::PathBuilder::new();
	nb.push_circle(x(f), y(used), (h * 0.09).max(1.5));
	if let Some(path) = nb.finish() {
		let mut paint = tiny_skia::Paint::default();
		paint.anti_alias = true;
		paint.set_color(icon_color(BURN_NOW_COLOR));
		pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
	}

	pixmap
}



// 対象枠の簡易バーンダウンを描き、トレイが前提とするストレートアルファの RGBA バイト列にして返す。draw_burndown_pixmap の描いた乗算済みアルファをストレートアルファへ戻す。
fn render_burndown_rgba(width: u32, height: u32, samples: &[(f32, f32)], proj: Option<&Projection>) -> Vec<u8> {
	demultiply_rgba(&draw_burndown_pixmap(width, height, samples, proj))
}










// フロントエンドの自前タイトルバー左上へ、トレイと同じ消費率ゲージを描いて返す。指定の辺長(px)で render_gauge_rgba を呼び、ストレートアルファの RGBA バイト列をそのまま渡す。webview 側は受け取った画素を canvas へ putImageData し、タスクバーやトレイと寸分違わぬ図柄をタイトルバーへ出す。表示倍率に合う実画素数は webview が決めるため、寸法は引数で受け取る。両枠とも値が無いときは空を返し、webview にゲージを隠させる。
#[cfg(windows)]
#[tauri::command]
fn gauge_icon_rgba(size: u32, session: Option<f32>, week: Option<f32>) -> Vec<u8> {
	if session.is_none() && week.is_none() {
		return Vec::new();
	}
	let size = size.clamp(1, 256);
	render_gauge_rgba(size, session, week)
}










// macOS などウィンドウ左上にアイコンを置かない文化のプラットフォームでは、ゲージ画素を返さず空にする。webview 側は空を受け取るとタイトルバーのゲージを隠す。
#[cfg(not(windows))]
#[tauri::command]
fn gauge_icon_rgba(_size: u32, _session: Option<f32>, _week: Option<f32>) -> Vec<u8> {
	Vec::new()
}










// トレイ(通知領域)の小アイコンの一辺の px を求める。通知領域が載るプライマリのタスクバー(Shell_TrayWnd)の DPI を起点に SM_CXSMICON を測り、その画面の拡大率ちょうどの寸法(100%→16, 125%→20, 150%→24, 200%→32 など)を得る。これにより描画側が等倍の1枚を渡せ、シェル側のスケーリングによるぼけを避けられる。タスクバーを掴めない場合は呼び出しスレッドの DPI 基準の値へ退く。
#[cfg(windows)]
fn tray_icon_px() -> u32 {
	use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
	use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, GetSystemMetrics, SM_CXSMICON};

	let class: Vec<u16> = "Shell_TrayWnd\0".encode_utf16().collect();
	let size = unsafe {
		let taskbar = FindWindowW(class.as_ptr(), std::ptr::null());
		let dpi = if taskbar.is_null() { 0 } else { GetDpiForWindow(taskbar) };
		if dpi != 0 {
			GetSystemMetricsForDpi(SM_CXSMICON, dpi)
		} else {
			GetSystemMetrics(SM_CXSMICON)
		}
	};
	if size > 0 {
		size as u32
	} else {
		16
	}
}










// Windows 以外では DPI 連動のトレイ寸法取得を行わず、一般的な小アイコン寸法を返す。
#[cfg(not(windows))]
fn tray_icon_px() -> u32 {
	32
}










// タスクバーのボタンや Alt+Tab に出る大アイコン(ICON_BIG)の寸法を、タスクバーのモニタの DPI に合わせて求める。ウィンドウアイコンは大小2スロットへ同じ画像が入り、小アイコン(タイトルバー)はこれを縮小して使うため、大きい方の SM_CXICON へ合わせて焼けば双方が綺麗になる。タスクバーが見つからないときは標準の大アイコン寸法へ退避する。
#[cfg(windows)]
fn window_icon_px() -> u32 {
	use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
	use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, GetSystemMetrics, SM_CXICON};

	let class: Vec<u16> = "Shell_TrayWnd\0".encode_utf16().collect();
	let size = unsafe {
		let taskbar = FindWindowW(class.as_ptr(), std::ptr::null());
		let dpi = if taskbar.is_null() { 0 } else { GetDpiForWindow(taskbar) };
		if dpi != 0 {
			GetSystemMetricsForDpi(SM_CXICON, dpi)
		} else {
			GetSystemMetrics(SM_CXICON)
		}
	};
	if size > 0 {
		size as u32
	} else {
		32
	}
}










// 直近に取得した session と week(全モデル)のメーター。設定変更など、新たな取得を伴わずにトレイアイコンを描き直すときの現在値の供給元にする。取得のたびに update_tray_icon が更新する。
static LAST_METERS: LazyLock<Mutex<(Option<Meter>, Option<Meter>)>> = LazyLock::new(|| Mutex::new((None, None)));



// 新たな取得を伴わずにトレイを描き直すための現在値を返す。直近取得のメーターを優先し、無ければ履歴の最新サンプルへ退く。どちらも得られなければ None。
fn latest_usage(app: &tauri::AppHandle) -> Option<Usage> {
	let (session, week_all) = LAST_METERS.lock().unwrap().clone();
	if session.is_some() || week_all.is_some() {
		return Some(Usage { session, week_all, week_sonnet: None, raw: String::new() });
	}
	let history = read_history(app).ok()?;
	let last = history.iter().rev().find(|s| s.session.is_some() || s.week_all.is_some() || s.week_sonnet.is_some())?;
	Some(Usage {
		session: last.session.clone(),
		week_all: last.week_all.clone(),
		week_sonnet: last.week_sonnet.clone(),
		raw: String::new(),
	})
}



// トレイのバーンダウンアイコンの (幅, 高さ) を px で求める。Windows はトレイが正方形のため一辺を tray_icon_px に揃える。macOS などメニューバーの横幅が可変の環境では、高さをトレイ寸法に合わせつつ横へ広げ、時間軸を読み取りやすくする。
fn tray_burndown_px() -> (u32, u32) {
	let h = tray_icon_px();
	#[cfg(windows)]
	{
		(h, h)
	}
	#[cfg(not(windows))]
	{
		(((h as f32) * 1.8).round() as u32, h)
	}
}



// 設定の対象枠(session/week)について、履歴と現在値から簡易バーンダウンを描いて (RGBA, 幅, 高さ) を返す。現在値・リセット時刻・窓の起点が揃わず描けないときは None を返し、呼び出し側でゲージへ退かせる。
fn try_render_tray_burndown(app: &tauri::AppHandle, usage: &Usage, target: &str) -> Option<(Vec<u8>, u32, u32)> {
	let is_week = target == "week";
	let window_ms = if is_week { WEEK_WINDOW_MS } else { SESSION_WINDOW_MS };
	let cur = if is_week { usage.week_all.as_ref() } else { usage.session.as_ref() };
	let used = cur.map(|m| m.used_pct as f32)?;

	let history = read_history(app).unwrap_or_default();
	// リセット時刻文字列。現在値のものを優先し、無ければ履歴の新しい方から探す。
	let reset_str = cur.and_then(|m| m.resets.clone()).or_else(|| {
		history.iter().rev().find_map(|s| {
			let m = if is_week { s.week_all.as_ref() } else { s.session.as_ref() };
			m.and_then(|x| x.resets.clone())
		})
	})?;

	let now = now_ms() as i64;
	let reset_ms = parse_reset_ms(&reset_str, now)?;
	let start_ms = reset_ms - window_ms;
	let span = window_ms as f32;

	// 窓内の実測点を (経過率, 使用%) へ移す。履歴は時刻順のため経過率も昇順に並ぶ。
	let mut samples: Vec<(f32, f32)> = history
		.iter()
		.filter_map(|s| {
			let m = if is_week { s.week_all.as_ref() } else { s.session.as_ref() }?;
			let ts = s.ts as i64;
			if ts < start_ms || ts > reset_ms {
				return None;
			}
			Some(((ts - start_ms) as f32 / span, m.used_pct as f32))
		})
		.collect();

	// 現在値を最新点として末尾へ足し、履歴が古くても現在地を映す。経過率は窓内へ収める。
	let f_now = ((now - start_ms) as f32 / span).clamp(0.0, 1.0);
	samples.push((f_now, used));

	let proj = project_linear(&samples, f_now, used);
	let (w, h) = tray_burndown_px();
	Some((render_burndown_rgba(w, h, &samples, proj.as_ref()), w, h))
}



// 取得した利用枠をトレイアイコンへ反映する。設定 tray_style が "burndown-session"/"burndown-week" なら対象枠の簡易バーンダウンを、"gauge"(または描けないとき)なら外周リング(週間)＋中央の円(5時間枠)のゲージを、現在の DPI に合う寸法で描いて差し替える。窓を隠していてもトレイは生きているため、隠したままでも最新の消費率をアイコンへ反映できる。値が揃わないときは既定アイコンのままにする。
fn update_tray_icon(app: &tauri::AppHandle, usage: &Usage) {
	// 設定変更時の描き直しに使えるよう、現在値を控えておく。
	*LAST_METERS.lock().unwrap() = (usage.session.clone(), usage.week_all.clone());

	let settings = read_settings(app);
	// バーンダウン指定なら対象枠を取り出す。ゲージ指定や未知の値は None にしてゲージへ回す。
	let target = match settings.tray_style.as_str() {
		"burndown-session" => Some("session"),
		"burndown-week" => Some("week"),
		_ => None,
	};
	if let Some(target) = target {
		if let Some((rgba, w, h)) = try_render_tray_burndown(app, usage, target) {
			if let Some(tray) = app.tray_by_id(TRAY_ID) {
				let _ = tray.set_icon(Some(Image::new_owned(rgba, w, h)));
			}
			return;
		}
	}

	// ゲージ。バーンダウンを描けないときの退避も兼ねる。どちらの枠も無ければ既定アイコンのまま。
	let session = usage.session.as_ref().map(|m| m.used_pct as f32);
	let week = usage.week_all.as_ref().map(|m| m.used_pct as f32);
	if session.is_none() && week.is_none() {
		return;
	}
	let size = tray_icon_px();
	let rgba = render_gauge_rgba(size, session, week);
	if let Some(tray) = app.tray_by_id(TRAY_ID) {
		let _ = tray.set_icon(Some(Image::new_owned(rgba, size, size)));
	}
}










// 取得した利用枠を、トレイと同じゲージ図柄でウィンドウアイコンへも反映する。タスクバーのボタンや Alt+Tab のアイコンが、焼き込んだ固定値ではなく現在の消費率を表すようにする。効くのは窓を表示している間に限る。どちらの枠も取得できないときは既定のウィンドウアイコンのままにする。
#[cfg(windows)]
fn update_window_icon(app: &tauri::AppHandle, usage: &Usage) {
	let session = usage.session.as_ref().map(|m| m.used_pct as f32);
	let week = usage.week_all.as_ref().map(|m| m.used_pct as f32);
	if session.is_none() && week.is_none() {
		return;
	}
	if let Some(window) = app.get_webview_window("main") {
		let size = window_icon_px();
		let rgba = render_gauge_rgba(size, session, week);
		let _ = window.set_icon(Image::new_owned(rgba, size, size));
	}
}










// Windows 以外ではウィンドウアイコンの動的差し替えを行わない。タイトルバーやアイコンの扱いがプラットフォームで異なるため、対応は各プラットフォームの実装時に詰める。
#[cfg(not(windows))]
fn update_window_icon(_app: &tauri::AppHandle, _usage: &Usage) {}










// トレイメニューの図柄選択で使う CheckMenuItem 3種を保持する。メニューイベントと設定コマンドの双方から選択状態を書き換えられるよう、Tauri の管理状態として持ち回す。
struct TrayStyleItems {
	session: CheckMenuItem<tauri::Wry>,
	week: CheckMenuItem<tauri::Wry>,
	gauge: CheckMenuItem<tauri::Wry>,
}




// 図柄選択の CheckMenuItem 3種のうち、指定の図柄に対応する1つだけへチェックを立てて単一選択を保つ。muda はラジオ項目を持たないため CheckMenuItem のチェックで代用する。管理状態が未登録(トレイ構成前)のときは何もしない。
fn sync_tray_style_checks(app: &tauri::AppHandle, style: &str) {
	if let Some(items) = app.try_state::<TrayStyleItems>() {
		let _ = items.session.set_checked(style == "burndown-session");
		let _ = items.week.set_checked(style == "burndown-week");
		let _ = items.gauge.set_checked(style == "gauge");
	}
}




// トレイメニューの図柄選択から呼ぶ。選んだ図柄を設定へ保存し、メニューのチェックを1つへ揃え、直近の値でトレイアイコンを描き直す。あわせてフロントへ変更を通知し、設定画面のピッカーの表示も追従させる。
fn select_tray_style(app: &tauri::AppHandle, style: &str) {
	let mut settings = read_settings(app);
	settings.tray_style = style.to_string();
	if let Err(e) = write_settings(app, &settings) {
		eprintln!("トレイ図柄の設定保存に失敗しました: {}", e);
	}
	sync_tray_style_checks(app, style);
	if let Some(usage) = latest_usage(app) {
		update_tray_icon(app, &usage);
	}
	let _ = app.emit("tray-style-changed", style.to_string());
}




// トレイアイコンとそのメニューを構成する。窓を閉じてもトレイへ常駐し計測を続けられるようにする。
fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
	use tauri::menu::{PredefinedMenuItem, Submenu};

	// メニュー先頭にアプリ名とバージョンを見出しとして置く。文字列は tauri.conf.json 由来の PackageInfo から引くため、名称やバージョンを変えてもここは追従する。クリックしても何もしない見出しなので非活性にする。
	let pkg = app.package_info();
	let header = MenuItem::with_id(app, "app_header", format!("{} v{}", pkg.name, pkg.version), false, None::<&str>)?;
	let header_separator = PredefinedMenuItem::separator(app)?;
	let show = MenuItem::with_id(app, "show", "表示", true, None::<&str>)?;
	let settings = MenuItem::with_id(app, "settings", "設定", true, None::<&str>)?;

	// トレイアイコンに表示する図柄を切り替えるラジオ選択。muda はラジオ項目を持たないため CheckMenuItem を単一選択として扱い、選択のたびに他をチェック解除する。初期チェックは現在の設定に合わせる。ラベルは設定画面の図柄ピッカーの表記に揃える。
	let style = read_settings(app.handle()).tray_style;
	let style_session = CheckMenuItem::with_id(app, "tray_style:burndown-session", "セッションバーンダウン", true, style == "burndown-session", None::<&str>)?;
	let style_week = CheckMenuItem::with_id(app, "tray_style:burndown-week", "週間バーンダウン", true, style == "burndown-week", None::<&str>)?;
	let style_gauge = CheckMenuItem::with_id(app, "tray_style:gauge", "円グラフ", true, style == "gauge", None::<&str>)?;
	let style_menu = Submenu::with_items(app, "アイコン", true, &[&style_session, &style_week, &style_gauge])?;

	let quit = MenuItem::with_id(app, "quit", "終了", true, None::<&str>)?;
	let menu = Menu::with_items(app, &[&header, &header_separator, &show, &settings, &style_menu, &quit])?;

	// メニューイベントと設定コマンドの双方から図柄選択のチェックを更新できるよう、CheckMenuItem 3種を管理状態へ預ける。
	app.manage(TrayStyleItems { session: style_session, week: style_week, gauge: style_gauge });

	let mut builder = TrayIconBuilder::with_id(TRAY_ID)
		.tooltip("払底枯渇")
		.menu(&menu)
		.on_menu_event(|app, event| match event.id.as_ref() {
			"show" => show_main_window(app),
			"settings" => open_settings(app),
			// アイコンの図柄のラジオ選択。選んだ図柄を保存し、チェックを1つへ揃えてアイコンを描き直す。
			"tray_style:burndown-session" => select_tray_style(app, "burndown-session"),
			"tray_style:burndown-week" => select_tray_style(app, "burndown-week"),
			"tray_style:gauge" => select_tray_style(app, "gauge"),
			// 終了前に現在のウィンドウ配置を保存する。トレイから直接終了する場合は CloseRequested を経ないため、ここで保存しないと最後の移動・リサイズが残らない。
			"quit" => {
				romly_tauri_common::window_state::save(app);
				app.exit(0);
			}
			_ => {}
		});

	// Windows は通知領域アイコンの左クリックでメインウィンドウを表示し、メニューは右クリックで出す慣例。左クリックでメニューを開かせないよう抑止したうえで、左ボタンの離しでウィンドウを表示する。
	#[cfg(not(target_os = "macos"))]
	{
		builder = builder
			.show_menu_on_left_click(false)
			.on_tray_icon_event(|tray, event| {
				if let TrayIconEvent::Click {
					button: MouseButton::Left,
					button_state: MouseButtonState::Up,
					..
				} = event
				{
					show_main_window(tray.app_handle());
				}
			});
	}

	// macOS はステータスバーアイコンの左クリックでメニューを出すのが慣例で、アイコン単体ではウィンドウを開かない。左クリックでメニューを表示させ、クリックでウィンドウを開く処理は付けない。
	#[cfg(target_os = "macos")]
	{
		builder = builder.show_menu_on_left_click(true);
	}

	if let Some(icon) = app.default_window_icon() {
		builder = builder.icon(icon.clone());
	}

	let tray = builder.build(app)?;

	// macOS のステータスバーアイコンは右クリックに反応させない慣例に合わせ、右クリックでのメニュー表示を切る。tray-icon の既定は左右どちらのクリックでもメニューを出すが、Tauri のビルダーは左クリック側しか公開していないため、内部の tray-icon を直接操作して右クリック表示だけを無効化する。
	#[cfg(target_os = "macos")]
	{
		let _ = tray.with_inner_tray_icon(|inner| {
			inner.set_show_menu_on_right_click(false);
		});
	}

	#[cfg(not(target_os = "macos"))]
	let _ = tray;

	Ok(())
}




// macOS のアプリ名メニューを、既定メニュー(File/Edit/View/Window/Help 一式)の代わりに最小構成へ差し替える。テキスト入力も選択も持たないアプリのため Edit 等の既定項目は不要で、設定・更新・終了だけを残す。トレイの on_menu_event と同じグローバルのメニューイベント列へ流れる都合上、id はトレイ(show/settings/quit)と重ならないよう app_ 接頭辞で分ける。重なると同一イベントで双方のハンドラが発火して二重動作になる。Windows はカスタムタイトルバーでメニューバーを持たないため、この差し替えは macOS のみ行う。
#[cfg(target_os = "macos")]
fn setup_app_menu(app: &tauri::App) -> tauri::Result<()> {
	use tauri::menu::{PredefinedMenuItem, Submenu};

	let settings = MenuItem::with_id(app, "app_settings", "設定…", true, Some("Cmd+Comma"))?;
	let update = MenuItem::with_id(app, "app_update", "更新", true, Some("Cmd+R"))?;
	let separator = PredefinedMenuItem::separator(app)?;
	let quit = MenuItem::with_id(app, "app_quit", "終了", true, Some("Cmd+Q"))?;
	// 先頭の Submenu が macOS のアプリ名メニューになる。表示名は OS がプロセス名へ差し替えるため、ここで与える文字列は表に出ない。
	let app_menu = Submenu::with_items(app, "払底枯渇", true, &[&settings, &update, &separator, &quit])?;
	let menu = Menu::with_items(app, &[&app_menu])?;
	app.set_menu(menu)?;

	app.on_menu_event(|app, event| match event.id.as_ref() {
		"app_settings" => open_settings(app),
		"app_update" => trigger_refresh(app),
		// トレイの終了と同じく、終了前に現在のウィンドウ配置を保存してから抜ける。
		"app_quit" => {
			romly_tauri_common::window_state::save(app);
			app.exit(0);
		}
		_ => {}
	});

	Ok(())
}










#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
	let mut builder = tauri::Builder::default();

	// 二重起動を防ぎ、二個目の起動は一個目のウィンドウ復帰に変える。複数プロセスが同じ history.jsonl を同時に書いて奪い合うのを避けるため。二個目の起動を検知する都合上、単一インスタンスは他のプラグインより先に登録する。
	#[cfg(desktop)]
	{
		builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
			show_main_window(app);
		}));
		// ログイン時の自動起動を登録・解除できるようにする。登録する起動コマンドへ --autostart を付け、自動起動からの起動を launched_at_startup で見分けてトレイのみで常駐させる。第1引数は macOS でのみ意味を持ち、Windows では無視される。
		builder = builder.plugin(tauri_plugin_autostart::init(
			tauri_plugin_autostart::MacosLauncher::LaunchAgent,
			Some(vec!["--autostart"]),
		));
	}

	builder
		// ウィンドウの位置・サイズ・最大化状態を次回起動へ引き継ぐ。移動・リサイズの捕捉と終了時の書き出しをプラグインが担い、前回の配置の復元は setup で行う。トレイへ畳むときのように終了を経ない契機では window_state::save を明示的に呼ぶ。
		.plugin(romly_tauri_common::window_state::plugin())
		.setup(|app| {
			setup_tray(app)?;
			// macOS では既定メニューを最小構成(設定・更新・終了)へ差し替える。Windows はメニューバーを持たないため何もしない。
			#[cfg(target_os = "macos")]
			setup_app_menu(app)?;
			// 保存済みのテーマ設定を起動直後のウィンドウへ適用し、初期表示から選択中の配色にする。
			let settings = read_settings(app.handle());
			apply_theme(app.handle(), &settings);
			// フォーカス喪失時に隠す設定を、ウィンドウイベントハンドラが参照するランタイムのフラグへ写す。
			HIDE_ON_BLUR.store(settings.hide_on_blur, Ordering::Relaxed);
			// 半透明のシステム背景(Mica 等)を当てて地をネイティブにする。Mica はウィンドウのテーマに追従するため、テーマ適用の後に当てて初期の明暗を揃える。
			romly_tauri_common::apply_backdrop(app.handle());
			// 前回のウィンドウ位置・サイズを、現在のモニター構成へ合わせて補正してから復元する。表示の前に整えることで、初期表示で位置やサイズが飛ぶのを避ける。
			romly_tauri_common::window_state::restore(app.handle());
			start_poller(app.handle().clone());
			// 手動起動ならウィンドウを表示し、自動起動ならトレイのみで常駐する。ウィンドウは visible:false で生成されるため、表示する場合だけここで前面に出す。テーマ適用の後に表示することで、初期表示で配色がちらつくのを避ける。
			if !launched_at_startup() {
				show_main_window(app.handle());
			}
			Ok(())
		})
		.on_window_event(|window, event| match event {
			// 窓を閉じる操作ではプロセスを終了させず、現在の配置を保存してからトレイへ隠して計測を継続する。
			tauri::WindowEvent::CloseRequested { api, .. } => {
				api.prevent_close();
				romly_tauri_common::window_state::save(window.app_handle());
				let _ = window.hide();
			}
			// フォーカスを失った時に自動でトレイへ隠す設定が有効なら、閉じる操作と同じく配置を保存してから隠し、計測を継続する。ただし枠なしウィンドウの縁を掴んでリサイズを始めると、窓は前面のままなのに一過性のフォーカス喪失イベントが飛ぶ。それで畳むとリサイズできないため、BLUR_HIDE_GRACE だけ待ってから改めてフォーカス状態を見て、本当に前面を失ったままの時だけ畳む。リサイズ中は自分が前面(is_focused が true)なので畳まれず、他アプリへ切り替えた本物の喪失だけが畳まれる。表示中で最小化していない時だけ隠し、既に隠れている時やトレイへ畳んだ直後の余分な発火では何もしない。
			tauri::WindowEvent::Focused(false) if HIDE_ON_BLUR.load(Ordering::Relaxed) => {
				let window = window.clone();
				std::thread::spawn(move || {
					std::thread::sleep(BLUR_HIDE_GRACE);
					let app = window.app_handle().clone();
					let _ = app.run_on_main_thread(move || {
						if HIDE_ON_BLUR.load(Ordering::Relaxed)
							&& !window.is_focused().unwrap_or(false)
							&& window.is_visible().unwrap_or(false)
							&& !window.is_minimized().unwrap_or(false)
						{
							romly_tauri_common::window_state::save(window.app_handle());
							let _ = window.hide();
						}
					});
				});
			}
			_ => {}
		})
		.invoke_handler(tauri::generate_handler![
			get_usage,
			get_history,
			set_tray_tooltip,
			get_settings,
			set_settings,
			get_autostart,
			set_autostart,
			quit_app,
			app_version,
			gauge_icon_rgba,
			romly_tauri_common::accent_color,
			romly_tauri_common::win_minimize,
			romly_tauri_common::win_toggle_maximize,
			romly_tauri_common::win_is_maximized,
			romly_tauri_common::win_start_drag,
			romly_tauri_common::win_close
		])
		.run(tauri::generate_context!())
		.expect("error while running tauri application");
}










#[cfg(test)]
mod tests {
	use super::*;

	// /usage の実出力に倣ったサンプル。リセット時刻は分の無い表記(7pm)も含めて検証する。
	const SAMPLE: &str = "You are currently using your subscription to power your Claude Code usage\n\nCurrent session: 33% used · resets Jun 23, 4:10am (Asia/Tokyo)\nCurrent week (all models): 50% used · resets Jun 27, 7pm (Asia/Tokyo)\nCurrent week (Sonnet only): 0% used\n";

	#[test]
	fn parses_three_meters() {
		let u = parse_usage(SAMPLE);

		let s = u.session.expect("session メーターが取れること");
		assert_eq!(s.used_pct, 33);
		assert_eq!(s.resets.as_deref(), Some("Jun 23, 4:10am (Asia/Tokyo)"));

		let w = u.week_all.expect("week_all メーターが取れること");
		assert_eq!(w.used_pct, 50);
		assert_eq!(w.resets.as_deref(), Some("Jun 27, 7pm (Asia/Tokyo)"));

		let ws = u.week_sonnet.expect("week_sonnet メーターが取れること");
		assert_eq!(ws.used_pct, 0);
		assert_eq!(ws.resets, None);
	}










	// 利用枠以外のテキスト(/usage が地の文を返した場合など)では3枠とも None になることを確認する。fetch_usage はこの状態を原因付きのエラーへ変える。
	#[test]
	fn unparseable_text_yields_no_meters() {
		let u = parse_usage("これは利用枠の出力ではない任意のテキスト。");
		assert!(u.session.is_none());
		assert!(u.week_all.is_none());
		assert!(u.week_sonnet.is_none());
	}










	#[test]
	fn excerpt_collapses_whitespace_and_truncates() {
		assert_eq!(excerpt("  a\n\nb\tc  "), "a b c");
		let long: String = std::iter::repeat('あ').take(250).collect();
		let e = excerpt(&long);
		// 200文字に切り詰め、末尾へ省略記号を1つ付ける。
		assert_eq!(e.chars().count(), 201);
		assert!(e.ends_with('…'));
	}




	// リセット文字列の壁時計成分を、カンマ区切り・" at " 区切り・分なし・12時制の境目まで取り出せること。
	#[test]
	fn parses_reset_components() {
		assert_eq!(parse_reset_components("Jun 23, 4:10am (Asia/Tokyo)"), Some((6, 23, 4, 10)));
		assert_eq!(parse_reset_components("Jul 2 at 5:29am (Asia/Tokyo)"), Some((7, 2, 5, 29)));
		// 分なしの午後表記。7pm は19時。
		assert_eq!(parse_reset_components("Jun 27, 7pm (Asia/Tokyo)"), Some((6, 27, 19, 0)));
		// 12am は0時、12pm は12時。
		assert_eq!(parse_reset_components("Dec 31, 12:00am"), Some((12, 31, 0, 0)));
		assert_eq!(parse_reset_components("Jan 1, 12:30pm"), Some((1, 1, 12, 30)));
		// 利用枠以外の文字列は成分を持たない。
		assert_eq!(parse_reset_components("no reset here"), None);
	}




	// 中央値は奇数個で中央、偶数個で中央2点の平均。空なら None。
	#[test]
	fn median_odd_even_empty() {
		assert_eq!(median(&[]), None);
		assert_eq!(median(&[3.0]), Some(3.0));
		assert_eq!(median(&[1.0, 3.0]), Some(2.0));
		assert_eq!(median(&[1.0, 2.0, 9.0]), Some(2.0));
	}




	// 一定ペースで先行し残容量を早く食う系列では、リセット前の枯渇(warn)と到達経過率を投影できること。
	#[test]
	fn projects_early_depletion() {
		let samples = [(0.0, 0.0), (0.25, 40.0), (0.5, 80.0)];
		let p = project_linear(&samples, 0.5, 80.0).expect("投影が立つこと");
		assert!(p.warn);
		// 率160(%/経過率)で残20%を食うと 0.5 + 20/160 = 0.625 で100%到達。
		assert!((p.hit_t.unwrap() - 0.625).abs() < 1e-3);
	}




	// 余裕のある系列ではリセットまで枠を割らず(warn=false)、投影線の終点がリセット時点の投影使用%になること。
	#[test]
	fn projects_comfortable() {
		let samples = [(0.0, 0.0), (0.25, 10.0), (0.5, 20.0)];
		let p = project_linear(&samples, 0.5, 20.0).expect("投影が立つこと");
		assert!(!p.warn);
		// 枠を割らないため end は経過率1.0(リセット時点)まで伸び、その使用%が投影使用%。
		assert!((p.end.0 - 1.0).abs() < 1e-6);
		assert!((p.end.1 - 40.0).abs() < 1e-3);
	}




	// 実測点の張る範囲が狭い序盤は投影を控えて None を返すこと。
	#[test]
	fn projection_withheld_when_span_too_small() {
		let samples = [(0.48, 79.0), (0.5, 80.0)];
		assert!(project_linear(&samples, 0.5, 80.0).is_none());
	}










	#[test]
	fn history_roundtrip() {
		let path = std::env::temp_dir().join("cusspike_test_history.jsonl");
		let _ = std::fs::remove_file(&path);

		let s1 = Sample {
			ts: 1000,
			session: Some(Meter {
				used_pct: 33,
				resets: Some("Jun 23, 4:10am".to_string()),
			}),
			week_all: None,
			week_sonnet: Some(Meter {
				used_pct: 0,
				resets: None,
			}),
		};
		let s2 = Sample {
			ts: 2000,
			session: None,
			week_all: None,
			week_sonnet: None,
		};

		append_sample_to(&path, &s1).unwrap();
		append_sample_to(&path, &s2).unwrap();

		let got = read_history_from(&path).unwrap();
		assert_eq!(got.len(), 2);
		assert_eq!(got[0].ts, 1000);
		assert_eq!(got[0].session.as_ref().unwrap().used_pct, 33);
		assert_eq!(got[1].ts, 2000);
		assert!(got[1].session.is_none());

		let _ = std::fs::remove_file(&path);
	}
}
