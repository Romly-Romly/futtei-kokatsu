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
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
// クリック種別の判定は Windows 側のトレイイベント処理でしか使わない。macOS はメニュー表示をビルダー設定に委ねるためこれらの型を参照しない。
#[cfg(not(target_os = "macos"))]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::image::Image;
use tauri::{Emitter, Manager, PhysicalPosition, PhysicalSize, Theme};

// 利用枠を取得する間隔。窓の開閉と無関係に常駐スレッドがこの周期で測り続ける。
const POLL_INTERVAL: Duration = Duration::from_secs(600);

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


// 設定ウィンドウで操作する永続設定。theme と language は将来の全面ローカライズも見据えて文字列で持つ。show_trend は消費傾向ヒートマップの表示有無、date_format は日付の表示形式、heat_palette は消費傾向ヒートマップの配色(standard/parula/turbo/gray)。serde(default) を付け、項目が増えても古い設定ファイルが読めるようにする。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
	theme: String,
	language: String,
	show_trend: bool,
	date_format: String,
	heat_palette: String,
}

impl Default for Settings {
	fn default() -> Self {
		Settings {
			theme: "system".to_string(),
			language: "system".to_string(),
			show_trend: true,
			date_format: "intl".to_string(),
			heat_palette: "standard".to_string(),
		}
	}
}










// 次回起動時に復元するためのウィンドウ位置・サイズ。座標 x,y と寸法 width,height は物理ピクセルで持つ。物理ピクセルはマルチモニターでも一意な仮想スクリーン座標になり、論理ピクセルのようにモニターごとの拡大率で意味が変わらないため、複数画面をまたいだ位置の検証を取り違えない。maximized は最大化状態。scale は保存時の拡大率で、復元先モニターの拡大率が異なるとき見た目の大きさを保つよう寸法を換算するのに使う。serde(default) を付け、項目が増えても古いファイルが読めるようにする。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct WindowState {
	x: i32,
	y: i32,
	width: u32,
	height: u32,
	maximized: bool,
	scale: f64,
}

impl Default for WindowState {
	fn default() -> Self {
		WindowState { x: 0, y: 0, width: 800, height: 600, maximized: false, scale: 1.0 }
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
	let output = Command::new(&bin)
		.args(["-p", "/usage", "--output-format", "json"])
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
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










// Windows のビルド番号を返す。Mica が使えるのは Windows 11(ビルド22000以上)に限られるため、バックドロップの種類を選ぶのに使う。GetVersionEx は実行ファイルのマニフェスト次第で古い版を詐称するため、詐称されない RtlGetVersion から読む。取得できなければ 0 を返す。
#[cfg(windows)]
fn windows_build_number() -> u32 {
	use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
	use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

	let mut info = OSVERSIONINFOW {
		dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
		..Default::default()
	};
	// RtlGetVersion は現在の OS バージョンを書き込み、成功時に STATUS_SUCCESS(0)を返す。
	if unsafe { RtlGetVersion(&mut info) } == 0 {
		info.dwBuildNumber
	} else {
		0
	}
}










// メインウィンドウへ半透明のシステム背景(バックドロップ)を当て、地をネイティブアプリらしくする。背景が透けるよう、ウィンドウは tauri.conf.json で transparent:true として生成し、CSS でも最上位の地色を透過にしてある。Windows 11 では Mica、Mica の無い古い Windows では全 Windows で使える Acrylic へ退く。Mica はウィンドウのテーマに追従するため、apply_theme(set_theme)で設定した明暗にそのまま揃う。macOS ではタイトルバー相当の Vibrancy を当てる。この素材はウィンドウの明暗アピアランスに追従し、背後の壁紙を薄く透かす。set_effects は結果を返さないため、効果を当てられない環境では静かに無効となり CSS の地色のまま見える。
fn apply_backdrop(app: &tauri::AppHandle) {
	let window = match app.get_webview_window("main") {
		Some(w) => w,
		None => return,
	};

	#[cfg(windows)]
	{
		use tauri::utils::config::WindowEffectsConfig;
		use tauri::window::Effect;
		let effect = if windows_build_number() >= 22000 {
			Effect::Mica
		} else {
			Effect::Acrylic
		};
		let _ = window.set_effects(WindowEffectsConfig { effects: vec![effect], ..Default::default() });
	}

	#[cfg(target_os = "macos")]
	{
		use tauri::utils::config::WindowEffectsConfig;
		use tauri::window::Effect;
		let _ = window.set_effects(WindowEffectsConfig { effects: vec![Effect::Titlebar], ..Default::default() });
	}

	// Windows・macOS 以外ではバックドロップを当てないため window を使わない。未使用変数の警告を避ける。
	#[cfg(not(any(windows, target_os = "macos")))]
	let _ = window;
}










// 最後に観測した、最大化していない通常状態のウィンドウ位置・サイズ。Moved/Resized のたびに更新し、トレイへ隠す時と終了時にこの値をファイルへ書き出す。ウィンドウイベントハンドラと終了処理の双方から触るため LazyLock+Mutex で持つ。一度も観測していない間は None。
static LAST_WINDOW_STATE: LazyLock<Mutex<Option<WindowState>>> = LazyLock::new(|| Mutex::new(None));

// カスタムタイトルバーの最大化ボタンの図形を最大化状態へ追従させるため、最後にフロントへ通知した最大化状態を覚えておく。Resized のたびに比較し、変化したときだけ通知して無駄打ちを避ける。
static LAST_MAXIMIZED: AtomicBool = AtomicBool::new(false);

// 復元時に「ウィンドウを掴める」と見なす最小の可視領域(物理ピクセル)。どのモニターともこれ未満しか重ならない位置は画面外とみなしてモニター内へ収め直す。タイトルバーをドラッグできる程度の幅と高さを確保する値にする。
const MIN_VISIBLE_W: i32 = 120;
const MIN_VISIBLE_H: i32 = 60;










// ウィンドウ状態ファイル(JSON)のパス。履歴・設定と同じくアプリのデータディレクトリ直下に置く。
fn window_state_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
	let dir = app
		.path()
		.app_data_dir()
		.map_err(|e| format!("データディレクトリの取得に失敗しました: {}", e))?;
	Ok(dir.join("window-state.json"))
}










// ウィンドウ状態を読む。ファイルが無い・壊れている場合は None を返し、初回起動では tauri.conf.json の既定配置に任せる。
fn read_window_state(app: &tauri::AppHandle) -> Option<WindowState> {
	let path = window_state_path(app).ok()?;
	let text = fs::read_to_string(&path).ok()?;
	serde_json::from_str(&text).ok()
}










// ウィンドウ状態をファイルへ書き出す。親ディレクトリが無ければ作る。後から見て分かるよう整形して保存する。
fn write_window_state(app: &tauri::AppHandle, state: &WindowState) -> Result<(), String> {
	let path = window_state_path(app)?;
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).map_err(|e| format!("ディレクトリの作成に失敗しました: {}", e))?;
	}
	let text = serde_json::to_string_pretty(state).map_err(|e| format!("ウィンドウ状態の直列化に失敗しました: {}", e))?;
	fs::write(&path, text).map_err(|e| format!("ウィンドウ状態の書き込みに失敗しました: {}", e))
}










// モニターの作業領域(タスクバー等を除いた領域)を物理ピクセルの矩形 (x, y, 幅, 高さ) として取り出す。復元位置の検証と収め直しはこの作業領域を基準にし、復元したウィンドウがタスクバーに潜らないようにする。
fn monitor_work_rect(m: &tauri::Monitor) -> (i32, i32, i32, i32) {
	let wa = m.work_area();
	(wa.position.x, wa.position.y, wa.size.width as i32, wa.size.height as i32)
}










// 2つの物理ピクセル矩形が重なる幅と高さを返す。重なりが無い辺は0になる。復元先モニターの選定(面積比較)と可視量の判定の双方に使う。引数の矩形は (x, y, 幅, 高さ)。
fn overlap_extent(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> (i32, i32) {
	let w = ((a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0)).max(0);
	let h = ((a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1)).max(0);
	(w, h)
}










// 指定の物理ピクセル矩形が、いずれかのモニターの作業領域と「掴める」だけ重なっているかを返す。重なりが幅・高さとも最小可視量に満たない位置は画面外とみなす。あわせて、上端(タイトルバー)が同じモニターの作業領域の縦範囲に収まっていることも求め、上へはみ出してタイトルバーを掴めない位置を弾く。モニターは (作業領域矩形, 拡大率) の並びで受け取り、UI 型に依存しない純粋な判定にする。
fn is_visible_enough(rect: (i32, i32, i32, i32), monitors: &[((i32, i32, i32, i32), f64)]) -> bool {
	monitors.iter().any(|(work, _)| {
		let (ow, oh) = overlap_extent(rect, *work);
		let grabbable = ow >= MIN_VISIBLE_W && oh >= MIN_VISIBLE_H;
		let title_reachable = rect.1 >= work.1 && rect.1 <= work.1 + work.3 - MIN_VISIBLE_H;
		grabbable && title_reachable
	})
}










// 復元位置・サイズを求める純粋な計算。モニター群を (作業領域矩形, 拡大率) の並びで、主モニターをその添字で受け取り、UI 型に依存しない形で保存値を安全な配置へ補正する。tauri::Monitor から値を取り出した sanitize_window_state がこれを呼ぶ。戻り値は物理ピクセルの (x, y, 幅, 高さ)。手順は次の通り。
// 1. 保存位置に最も大きく重なるモニターを復元先に選ぶ。どれとも重ならなければ(モニターを外した等)主モニター、それも無ければ先頭のモニターへ。
// 2. 保存時と復元先で拡大率が違えば、見た目の大きさを保つよう寸法を換算する。
// 3. 寸法を復元先の作業領域に収まる大きさへ抑える。
// 4. 元の位置のまま十分な可視領域が確保できるならその位置を尊重し、確保できなければ復元先の作業領域内へ収め直す。
fn compute_restore_geometry(state: &WindowState, monitors: &[((i32, i32, i32, i32), f64)], primary: Option<usize>) -> (i32, i32, i32, i32) {
	let saved = (state.x, state.y, state.width as i32, state.height as i32);

	// 保存位置に最も大きく重なるモニターの添字を選ぶ。重なりが全く無ければ主モニター、それも無ければ先頭へ退く。
	let target_idx = monitors
		.iter()
		.enumerate()
		.map(|(i, (rect, _))| {
			let (w, h) = overlap_extent(saved, *rect);
			(i, (w as i64) * (h as i64))
		})
		.filter(|(_, area)| *area > 0)
		.max_by_key(|(_, area)| *area)
		.map(|(i, _)| i)
		.or(primary)
		.or(if monitors.is_empty() { None } else { Some(0) });

	let target_idx = match target_idx {
		Some(i) => i,
		// モニター情報が一切得られなければ補正のしようがないため保存値をそのまま返す。
		None => return saved,
	};
	let (work, target_scale) = monitors[target_idx];

	// 拡大率の差を寸法へ反映してから、作業領域に収まる大きさへ抑える。min/max で上下限を取り、作業領域が極端に狭くても破綻しないようにする。
	let scale_ratio = if state.scale > 0.0 { target_scale / state.scale } else { 1.0 };
	let w = (((state.width as f64) * scale_ratio).round() as i32).min(work.2).max(MIN_VISIBLE_W);
	let h = (((state.height as f64) * scale_ratio).round() as i32).min(work.3).max(MIN_VISIBLE_H);

	// 元の位置のまま十分に見えているならそこを尊重する。見えていなければ作業領域内へ収め直す。右端・下端からはみ出さないよう上限を取り、左端・上端を下限にする。
	let mut x = state.x;
	let mut y = state.y;
	if !is_visible_enough((x, y, w, h), monitors) {
		x = x.clamp(work.0, (work.0 + work.2 - w).max(work.0));
		y = y.clamp(work.1, (work.1 + work.3 - h).max(work.1));
	}

	(x, y, w, h)
}










// 保存しておいたウィンドウ状態を、現在のモニター構成へ合わせて安全な位置・サイズへ補正する。tauri::Monitor 群から作業領域と拡大率を取り出し、主モニターを作業領域の一致で添字へ対応付けてから、純粋計算の compute_restore_geometry へ委ねる。モニターの取り外し・解像度変更・拡大率変更があっても画面内へ復元できるようにするのが目的。
fn sanitize_window_state(state: &WindowState, monitors: &[tauri::Monitor], primary: Option<&tauri::Monitor>) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
	let rects: Vec<((i32, i32, i32, i32), f64)> = monitors
		.iter()
		.map(|m| (monitor_work_rect(m), m.scale_factor()))
		.collect();
	let primary_idx = primary.and_then(|p| {
		let pr = monitor_work_rect(p);
		rects.iter().position(|(r, _)| *r == pr)
	});
	let (x, y, w, h) = compute_restore_geometry(state, &rects, primary_idx);
	(PhysicalPosition::new(x, y), PhysicalSize::new(w as u32, h as u32))
}










// 現在のメインウィンドウから通常状態(最大化していない)の位置・サイズを読み取り、メモリ上の LAST_WINDOW_STATE を更新する。最大化中は通常寸法を上書きせず最大化フラグだけ立て、復元時に通常サイズへ戻せるようにする。最小化中は (-32000,-32000) のような無効値を掴むため何もしない。
fn capture_window_state(app: &tauri::AppHandle) {
	let window = match app.get_webview_window("main") {
		Some(w) => w,
		None => return,
	};
	if window.is_minimized().unwrap_or(false) {
		return;
	}
	let mut guard = LAST_WINDOW_STATE.lock().unwrap();
	if window.is_maximized().unwrap_or(false) {
		// 通常寸法は最後に観測した値を保ち、最大化フラグだけ更新する。
		if let Some(state) = guard.as_mut() {
			state.maximized = true;
		}
		return;
	}
	let pos = match window.outer_position() {
		Ok(p) => p,
		Err(_) => return,
	};
	let size = match window.inner_size() {
		Ok(s) => s,
		Err(_) => return,
	};
	let scale = window.scale_factor().unwrap_or(1.0);
	*guard = Some(WindowState {
		x: pos.x,
		y: pos.y,
		width: size.width,
		height: size.height,
		maximized: false,
		scale,
	});
}










// 現在のウィンドウ状態を捕捉してファイルへ書き出す。トレイへ隠す時と終了時に呼ぶ。書き込みに失敗しても致命扱いはせず標準エラーへ記録する。
fn save_window_state(app: &tauri::AppHandle) {
	capture_window_state(app);
	let state = match *LAST_WINDOW_STATE.lock().unwrap() {
		Some(s) => s,
		None => return,
	};
	if let Err(e) = write_window_state(app, &state) {
		eprintln!("ウィンドウ状態の保存に失敗しました: {}", e);
	}
}










// 起動時に保存済みのウィンドウ状態を読み、現在のモニター構成へ合わせて補正してから適用する。位置・サイズを整えてから、保存時に最大化していたなら最大化する。ウィンドウは visible:false で生成されるため、隠れたまま配置を整えておき、後で表示した時に正しい場所へ出るようにする。状態が無い初回起動では何もせず tauri.conf.json の既定配置に任せる。
fn restore_window_state(app: &tauri::AppHandle) {
	let state = match read_window_state(app) {
		Some(s) => s,
		None => return,
	};
	// 以後の捕捉の起点として、読み込んだ状態をメモリへ載せておく。
	*LAST_WINDOW_STATE.lock().unwrap() = Some(state);

	let window = match app.get_webview_window("main") {
		Some(w) => w,
		None => return,
	};

	let monitors = window.available_monitors().unwrap_or_default();
	let primary = window.primary_monitor().ok().flatten();
	let (pos, size) = sanitize_window_state(&state, &monitors, primary.as_ref());

	let _ = window.set_size(size);
	let _ = window.set_position(pos);
	if state.maximized {
		let _ = window.maximize();
	}
}










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










// フロントエンドから受け取った設定を保存し、テーマを即座にウィンドウへ反映するコマンド。
#[tauri::command]
fn set_settings(app: tauri::AppHandle, settings: Settings) -> Result<(), String> {
	write_settings(&app, &settings)?;
	apply_theme(&app, &settings);
	Ok(())
}










// OS のアクセント色を #RRGGBB で読み取る。Windows では DWM が現在のアクセント色を ABGR の DWORD でレジストリへ持つため、そこから R,G,B を取り出す。値が無い・読めないときは None を返す。
#[cfg(windows)]
fn read_os_accent_color() -> Option<String> {
	use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

	let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\DWM\0".encode_utf16().collect();
	let value: Vec<u16> = "AccentColor\0".encode_utf16().collect();
	let mut data: u32 = 0;
	let mut size = std::mem::size_of::<u32>() as u32;
	let status = unsafe {
		RegGetValueW(
			HKEY_CURRENT_USER,
			subkey.as_ptr(),
			value.as_ptr(),
			RRF_RT_REG_DWORD,
			std::ptr::null_mut(),
			(&mut data as *mut u32).cast(),
			&mut size,
		)
	};
	// RegGetValueW は成功時に ERROR_SUCCESS(0) を返す。
	if status != 0 {
		return None;
	}
	// DWM の AccentColor は 0xAABBGGRR(ABGR)で格納され、最下位バイトが赤になる。
	let r = (data & 0xFF) as u8;
	let g = ((data >> 8) & 0xFF) as u8;
	let b = ((data >> 16) & 0xFF) as u8;
	Some(format!("#{:02x}{:02x}{:02x}", r, g, b))
}










// 非 Windows では OS のアクセント色を読まず None を返す。macOS の WKWebView をはじめ、CSS の system-color AccentColor を OS のアクセント色へ解決する WebView では、styles.css の既定値(AccentColor キーワード)がそのまま OS のアクセントへ追従するため、Rust から実値を流し込む必要がない。Windows の WebView2(Chromium)だけは AccentColor を固定の青へ丸めるため、上の Windows 実装で実値を読んで補う。
#[cfg(not(windows))]
fn read_os_accent_color() -> Option<String> {
	None
}










// フロントエンドへ OS のアクセント色を #RRGGBB で返すコマンド。フロントはこの値を CSS 変数 --accent へ流し込み、選択・オン状態の色を OS のアクセントへ追従させる。None のときはフロントが上書きを当てず styles.css の既定値に委ねる。
#[tauri::command]
fn get_accent_color() -> Option<String> {
	read_os_accent_color()
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










// カスタムタイトルバーの最小化ボタンから呼ぶ。OS 標準の枠を外したぶん、最小化を自前で起こす。ウィンドウ操作は JS プラグインを使わず invoke 経由に揃え、capabilities を据え置きにする。
#[tauri::command]
fn win_minimize(app: tauri::AppHandle) {
	if let Some(window) = app.get_webview_window("main") {
		let _ = window.minimize();
	}
}










// カスタムタイトルバーの最大化/元に戻すボタンとタイトルバーのダブルクリックから呼ぶ。最大化中なら戻し、そうでなければ最大化し、操作後の最大化状態を返す。フロントはこの戻り値でボタンの図形を切り替える。
#[tauri::command]
fn win_toggle_maximize(app: tauri::AppHandle) -> bool {
	if let Some(window) = app.get_webview_window("main") {
		if window.is_maximized().unwrap_or(false) {
			let _ = window.unmaximize();
			false
		} else {
			let _ = window.maximize();
			true
		}
	} else {
		false
	}
}










// 現在の最大化状態を返す。タイトルバーのボタン図形を初期化・同期するのに使う。
#[tauri::command]
fn win_is_maximized(app: tauri::AppHandle) -> bool {
	app.get_webview_window("main").and_then(|w| w.is_maximized().ok()).unwrap_or(false)
}










// カスタムタイトルバーのドラッグ領域の押下から呼び、ウィンドウの移動を始める。OS 標準のタイトルバーが無いぶん、ドラッグ移動を自前で起こす。
#[tauri::command]
fn win_start_drag(app: tauri::AppHandle) {
	if let Some(window) = app.get_webview_window("main") {
		let _ = window.start_dragging();
	}
}










// カスタムタイトルバーの閉じるボタンから呼ぶ。close は CloseRequested を発火するため、窓を閉じる操作と同じ経路(prevent_close してトレイへ隠す)へ合流し、計測を継続する。
#[tauri::command]
fn win_close(app: tauri::AppHandle) {
	if let Some(window) = app.get_webview_window("main") {
		let _ = window.close();
	}
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










// 取得した利用枠から、外周リング(週間)と中央の円(5時間枠)を重ねたトレイアイコンを描き、現在の DPI に合う寸法で差し替える。窓を隠していてもトレイは生きているため、隠したままでも最新の消費率をアイコンへ反映できる。どちらの枠も取得できないときは既定アイコンのままにする。
fn update_tray_icon(app: &tauri::AppHandle, usage: &Usage) {
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










// トレイアイコンとそのメニューを構成する。窓を閉じてもトレイへ常駐し計測を続けられるようにする。
fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
	use tauri::menu::PredefinedMenuItem;

	// メニュー先頭にアプリ名とバージョンを見出しとして置く。文字列は tauri.conf.json 由来の PackageInfo から引くため、名称やバージョンを変えてもここは追従する。クリックしても何もしない見出しなので非活性にする。
	let pkg = app.package_info();
	let header = MenuItem::with_id(app, "app_header", format!("{} v{}", pkg.name, pkg.version), false, None::<&str>)?;
	let header_separator = PredefinedMenuItem::separator(app)?;
	let show = MenuItem::with_id(app, "show", "表示", true, None::<&str>)?;
	let settings = MenuItem::with_id(app, "settings", "設定", true, None::<&str>)?;
	let quit = MenuItem::with_id(app, "quit", "終了", true, None::<&str>)?;
	let menu = Menu::with_items(app, &[&header, &header_separator, &show, &settings, &quit])?;

	let mut builder = TrayIconBuilder::with_id(TRAY_ID)
		.tooltip("払底枯渇")
		.menu(&menu)
		.on_menu_event(|app, event| match event.id.as_ref() {
			"show" => show_main_window(app),
			"settings" => open_settings(app),
			// 終了前に現在のウィンドウ配置を保存する。トレイから直接終了する場合は CloseRequested を経ないため、ここで保存しないと最後の移動・リサイズが残らない。
			"quit" => {
				save_window_state(app);
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
			save_window_state(app);
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
		.setup(|app| {
			setup_tray(app)?;
			// macOS では既定メニューを最小構成(設定・更新・終了)へ差し替える。Windows はメニューバーを持たないため何もしない。
			#[cfg(target_os = "macos")]
			setup_app_menu(app)?;
			// 保存済みのテーマ設定を起動直後のウィンドウへ適用し、初期表示から選択中の配色にする。
			let settings = read_settings(app.handle());
			apply_theme(app.handle(), &settings);
			// 半透明のシステム背景(Mica 等)を当てて地をネイティブにする。Mica はウィンドウのテーマに追従するため、テーマ適用の後に当てて初期の明暗を揃える。
			apply_backdrop(app.handle());
			// 前回のウィンドウ位置・サイズを、現在のモニター構成へ合わせて補正してから復元する。表示の前に整えることで、初期表示で位置やサイズが飛ぶのを避ける。
			restore_window_state(app.handle());
			start_poller(app.handle().clone());
			// 手動起動ならウィンドウを表示し、自動起動ならトレイのみで常駐する。ウィンドウは visible:false で生成されるため、表示する場合だけここで前面に出す。テーマ適用の後に表示することで、初期表示で配色がちらつくのを避ける。
			if !launched_at_startup() {
				show_main_window(app.handle());
			}
			Ok(())
		})
		.on_window_event(|window, event| match event {
			// 移動・リサイズのたびに最新の通常状態をメモリへ捕捉しておく。ディスクへはここでは書かず、トレイへ隠す時と終了時にまとめて書き出す。
			tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
				capture_window_state(window.app_handle());
				// 最大化状態が変わったらカスタムタイトルバーのボタン図形を追従させるため通知する。Win+↑やスナップなどボタン以外の操作にも追従する。
				let maximized = window.is_maximized().unwrap_or(false);
				if LAST_MAXIMIZED.swap(maximized, Ordering::Relaxed) != maximized {
					let _ = window.app_handle().emit("win-maximized", maximized);
				}
			}
			// 窓を閉じる操作ではプロセスを終了させず、現在の配置を保存してからトレイへ隠して計測を継続する。
			tauri::WindowEvent::CloseRequested { api, .. } => {
				api.prevent_close();
				save_window_state(window.app_handle());
				let _ = window.hide();
			}
			_ => {}
		})
		.invoke_handler(tauri::generate_handler![get_usage, get_history, set_tray_tooltip, get_settings, set_settings, get_accent_color, get_autostart, set_autostart, win_minimize, win_toggle_maximize, win_is_maximized, win_start_drag, win_close, gauge_icon_rgba])
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










	// 与えた位置・サイズと拡大率でウィンドウ状態を作るテスト補助。最大化はしていない状態とする。
	fn ws(x: i32, y: i32, width: u32, height: u32, scale: f64) -> WindowState {
		WindowState { x, y, width, height, maximized: false, scale }
	}










	// モニター構成が変わっていなければ、保存した位置・サイズをそのまま復元する。
	#[test]
	fn restore_keeps_position_when_unchanged() {
		let monitors = [((0, 0, 1920, 1080), 1.0)];
		let got = compute_restore_geometry(&ws(100, 100, 800, 600, 1.0), &monitors, Some(0));
		assert_eq!(got, (100, 100, 800, 600));
	}










	// 複数モニターでも、変化が無ければ副モニター上の位置を保つ。
	#[test]
	fn restore_keeps_position_on_secondary_monitor() {
		let monitors = [((0, 0, 1920, 1080), 1.0), ((1920, 0, 1920, 1080), 1.0)];
		let got = compute_restore_geometry(&ws(2000, 100, 800, 600, 1.0), &monitors, Some(0));
		assert_eq!(got, (2000, 100, 800, 600));
	}










	// 副モニターを取り外して保存位置が画面外になったら、残ったモニター内へ収め直す。
	#[test]
	fn restore_relocates_when_monitor_removed() {
		let monitors = [((0, 0, 1920, 1080), 1.0)];
		let (x, y, w, h) = compute_restore_geometry(&ws(2100, 200, 800, 600, 1.0), &monitors, Some(0));
		assert_eq!((w, h), (800, 600));
		// 作業領域(0..1920, 0..1080)に完全に収まること。
		assert!(x >= 0 && x + w <= 1920);
		assert!(y >= 0 && y + h <= 1080);
	}










	// 解像度が縮んで保存位置が画面外へ出たら、新しい作業領域内へ収め直す。
	#[test]
	fn restore_relocates_after_resolution_shrink() {
		let monitors = [((0, 0, 1280, 720), 1.0)];
		let (x, y, w, h) = compute_restore_geometry(&ws(1500, 900, 400, 300, 1.0), &monitors, Some(0));
		assert_eq!((w, h), (400, 300));
		assert!(x >= 0 && x + w <= 1280);
		assert!(y >= 0 && y + h <= 720);
	}










	// 拡大率が上がったら、見た目の大きさを保つよう物理寸法を換算する。
	#[test]
	fn restore_rescales_size_on_dpi_change() {
		let monitors = [((0, 0, 2560, 1440), 1.5)];
		let got = compute_restore_geometry(&ws(100, 100, 800, 600, 1.0), &monitors, Some(0));
		// 800x600 を 1.5 倍した 1200x900 になり、位置はそのまま。
		assert_eq!(got, (100, 100, 1200, 900));
	}










	// モニターより大きいウィンドウは作業領域の大きさへ抑える。
	#[test]
	fn restore_clamps_oversized_window() {
		let monitors = [((0, 0, 1920, 1080), 1.0)];
		let (_, _, w, h) = compute_restore_geometry(&ws(0, 0, 3000, 2000, 1.0), &monitors, Some(0));
		assert_eq!((w, h), (1920, 1080));
	}










	// タイトルバーが全モニターの上へはみ出した位置は、掴めるよう縦方向を画面内へ引き戻す。
	#[test]
	fn restore_pulls_down_offscreen_titlebar() {
		let monitors = [((0, 0, 1920, 1080), 1.0)];
		let (x, y, w, h) = compute_restore_geometry(&ws(100, -500, 800, 600, 1.0), &monitors, Some(0));
		assert_eq!((x, w, h), (100, 800, 600));
		// 上端が作業領域の縦範囲へ戻ること。
		assert!(y >= 0 && y + h <= 1080);
	}










	// モニター情報が一切得られなければ、補正せず保存値をそのまま返す。
	#[test]
	fn restore_passes_through_without_monitors() {
		let got = compute_restore_geometry(&ws(50, 60, 800, 600, 1.0), &[], None);
		assert_eq!(got, (50, 60, 800, 600));
	}
}
