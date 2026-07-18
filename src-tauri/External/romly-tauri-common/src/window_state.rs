//! ウィンドウの位置・サイズ・最大化状態を次回起動へ引き継ぐ処理。プラグインとして組み込むと、ウィンドウの生成時に前回の配置を復元し、移動・リサイズのたびに状態を捕捉し、終了時にファイルへ書き出す。トレイへ畳むなど終了以外の契機で書き出したいアプリは save を明示的に呼ぶ。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, RunEvent, Runtime, WindowEvent};

use crate::MAIN_WINDOW_LABEL;

/// 最大化状態が変わったときにフロントへ知らせるイベント名。ペイロードは最大化中かどうかの真偽値。フロントはこれを受けて自作タイトルバーの最大化ボタンの図形を切り替える。ボタン以外の操作(Win+↑やスナップ)による変化にも追従できる。
pub const MAXIMIZED_EVENT: &str = "win-maximized";

/// ウィンドウ状態を書き出すファイルの名前。アプリのデータディレクトリ直下に置く。
const STATE_FILENAME: &str = "window-state.json";

/// 復元時に「ウィンドウを掴める」と見なす最小の可視領域(物理ピクセル)。どのモニターともこれ未満しか重ならない位置は画面外とみなしてモニター内へ収め直す。タイトルバーをドラッグできる程度の幅と高さを確保する値にする。
const MIN_VISIBLE_W: i32 = 120;
const MIN_VISIBLE_H: i32 = 60;

/// 最後に観測した、最大化していない通常状態のウィンドウ位置・サイズ。移動・リサイズのたびに更新し、書き出しの際にこの値をファイルへ落とす。ウィンドウイベントハンドラと終了処理の双方から触るため LazyLock+Mutex で持つ。一度も観測していない間は None。
static LAST_WINDOW_STATE: LazyLock<Mutex<Option<WindowState>>> = LazyLock::new(|| Mutex::new(None));

/// 最後にフロントへ通知した最大化状態。リサイズのたびに比較し、変化したときだけ通知して無駄打ちを避ける。
static LAST_MAXIMIZED: AtomicBool = AtomicBool::new(false);




/// 次回起動時に復元するためのウィンドウ位置・サイズ。座標 x,y と寸法 width,height は物理ピクセルで持つ。物理ピクセルはマルチモニターでも一意な仮想スクリーン座標になり、論理ピクセルのようにモニターごとの拡大率で意味が変わらないため、複数画面をまたいだ位置の検証を取り違えない。maximized は最大化状態。scale は保存時の拡大率で、復元先モニターの拡大率が異なるとき見た目の大きさを保つよう寸法を換算するのに使う。serde(default) を付け、項目が増えても古いファイルが読めるようにする。
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










/// ウィンドウ配置の永続化を担うプラグインを返す。tauri::Builder へ plugin() で組み込む。移動・リサイズのたびに状態を捕捉して最大化の変化をフロントへ通知し、終了時にファイルへ書き出す。前回の配置の復元はここでは行わないため、アプリの setup から restore を呼ぶこと。
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
	PluginBuilder::new("romly-window-state")
		.on_window_ready(|window| {
			if window.label() != MAIN_WINDOW_LABEL {
				return;
			}

			let app = window.app_handle().clone();
			let win = window.clone();
			window.on_window_event(move |event| {
				// 移動・リサイズのたびに最新の通常状態をメモリへ捕捉しておく。ディスクへはここでは書かず、終了時と save の呼び出しでまとめて書き出す。
				if !matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_)) {
					return;
				}

				capture(&app);

				// 最大化状態が変わったら自作タイトルバーのボタン図形を追従させるため通知する。Win+↑やスナップなどボタン以外の操作にも追従する。
				let maximized = win.is_maximized().unwrap_or(false);
				if LAST_MAXIMIZED.swap(maximized, Ordering::Relaxed) != maximized {
					let _ = app.emit(MAXIMIZED_EVENT, maximized);
				}
			});
		})
		.on_event(|app, event| {
			if let RunEvent::Exit = event {
				save(app);
			}
		})
		.build()
}










/// 現在のウィンドウ状態を捕捉してファイルへ書き出す。終了時はプラグインが自動で呼ぶ。トレイへ畳むなど、終了を経ずに配置を確定させたい契機ではアプリから明示的に呼ぶ。書き込みに失敗しても致命扱いはせず標準エラーへ記録する。
pub fn save<R: Runtime>(app: &AppHandle<R>) {
	capture(app);

	let state = match *LAST_WINDOW_STATE.lock().unwrap() {
		Some(s) => s,
		None => return,
	};

	if let Err(e) = write_state(app, &state) {
		eprintln!("ウィンドウ状態の保存に失敗しました: {}", e);
	}
}










/// 保存済みのウィンドウ状態を読み、現在のモニター構成へ合わせて補正してから適用する。位置・サイズを整えてから、保存時に最大化していたなら最大化する。ウィンドウの表示はここでは行わないため、visible:false で生成したウィンドウは隠れたまま配置だけが整う。状態が無い初回起動では何もせず tauri.conf.json の既定配置に任せる。
///
/// アプリの setup から、ウィンドウを表示する前に呼ぶこと。プラグインのウィンドウ生成フックはメインスレッドのキューを経由して呼ばれるため setup より後ろへずれ込むうえ、その時点ではウィンドウが Window としてしか登録されておらず WebviewWindow としては引けない。表示前に配置を整えるには、この関数を setup から明示的に呼ぶ必要がある。
pub fn restore<R: Runtime>(app: &AppHandle<R>) {
	let state = match read_state(app) {
		Some(s) => s,
		None => return,
	};

	// 以後の捕捉の起点として、読み込んだ状態をメモリへ載せておく。
	*LAST_WINDOW_STATE.lock().unwrap() = Some(state);
	LAST_MAXIMIZED.store(state.maximized, Ordering::Relaxed);

	let window = match app.get_webview_window(MAIN_WINDOW_LABEL) {
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










/// 現在のメインウィンドウから通常状態(最大化していない)の位置・サイズを読み取り、メモリ上の状態を更新する。最大化中は通常寸法を上書きせず最大化フラグだけ立て、復元時に通常サイズへ戻せるようにする。最小化中は (-32000,-32000) のような無効値を掴むため何もしない。
fn capture<R: Runtime>(app: &AppHandle<R>) {
	let window = match app.get_webview_window(MAIN_WINDOW_LABEL) {
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










/// ウィンドウ状態ファイルのパス。アプリのデータディレクトリ直下に置く。
fn state_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
	let dir = app
		.path()
		.app_data_dir()
		.map_err(|e| format!("データディレクトリの取得に失敗しました: {}", e))?;
	Ok(dir.join(STATE_FILENAME))
}










/// ウィンドウ状態を読む。ファイルが無い・壊れている場合は None を返し、初回起動では tauri.conf.json の既定配置に任せる。
fn read_state<R: Runtime>(app: &AppHandle<R>) -> Option<WindowState> {
	let path = state_path(app).ok()?;
	let text = fs::read_to_string(&path).ok()?;
	serde_json::from_str(&text).ok()
}










/// ウィンドウ状態をファイルへ書き出す。親ディレクトリが無ければ作る。後から見て分かるよう整形して保存する。
fn write_state<R: Runtime>(app: &AppHandle<R>, state: &WindowState) -> Result<(), String> {
	let path = state_path(app)?;
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).map_err(|e| format!("ディレクトリの作成に失敗しました: {}", e))?;
	}

	let text = serde_json::to_string_pretty(state).map_err(|e| format!("ウィンドウ状態の直列化に失敗しました: {}", e))?;
	fs::write(&path, text).map_err(|e| format!("ウィンドウ状態の書き込みに失敗しました: {}", e))
}










/// モニターの作業領域(タスクバー等を除いた領域)を物理ピクセルの矩形 (x, y, 幅, 高さ) として取り出す。復元位置の検証と収め直しはこの作業領域を基準にし、復元したウィンドウがタスクバーに潜らないようにする。
fn monitor_work_rect(m: &Monitor) -> (i32, i32, i32, i32) {
	let wa = m.work_area();
	(wa.position.x, wa.position.y, wa.size.width as i32, wa.size.height as i32)
}










/// 2つの物理ピクセル矩形が重なる幅と高さを返す。重なりが無い辺は0になる。復元先モニターの選定(面積比較)と可視量の判定の双方に使う。引数の矩形は (x, y, 幅, 高さ)。
fn overlap_extent(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> (i32, i32) {
	let w = ((a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0)).max(0);
	let h = ((a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1)).max(0);
	(w, h)
}










/// 指定の物理ピクセル矩形が、いずれかのモニターの作業領域と「掴める」だけ重なっているかを返す。重なりが幅・高さとも最小可視量に満たない位置は画面外とみなす。あわせて、上端(タイトルバー)が同じモニターの作業領域の縦範囲に収まっていることも求め、上へはみ出してタイトルバーを掴めない位置を弾く。モニターは (作業領域矩形, 拡大率) の並びで受け取り、UI 型に依存しない純粋な判定にする。
fn is_visible_enough(rect: (i32, i32, i32, i32), monitors: &[((i32, i32, i32, i32), f64)]) -> bool {
	monitors.iter().any(|(work, _)| {
		let (ow, oh) = overlap_extent(rect, *work);
		let grabbable = ow >= MIN_VISIBLE_W && oh >= MIN_VISIBLE_H;
		let title_reachable = rect.1 >= work.1 && rect.1 <= work.1 + work.3 - MIN_VISIBLE_H;
		grabbable && title_reachable
	})
}










/// 復元位置・サイズを求める純粋な計算。モニター群を (作業領域矩形, 拡大率) の並びで、主モニターをその添字で受け取り、UI 型に依存しない形で保存値を安全な配置へ補正する。tauri::Monitor から値を取り出した sanitize_window_state がこれを呼ぶ。戻り値は物理ピクセルの (x, y, 幅, 高さ)。手順は次の通り。
/// 1. 保存位置に最も大きく重なるモニターを復元先に選ぶ。どれとも重ならなければ(モニターを外した等)主モニター、それも無ければ先頭のモニターへ。
/// 2. 保存時と復元先で拡大率が違えば、見た目の大きさを保つよう寸法を換算する。
/// 3. 寸法を復元先の作業領域に収まる大きさへ抑える。
/// 4. 元の位置のまま十分な可視領域が確保できるならその位置を尊重し、確保できなければ復元先の作業領域内へ収め直す。
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










/// 保存しておいたウィンドウ状態を、現在のモニター構成へ合わせて安全な位置・サイズへ補正する。tauri::Monitor 群から作業領域と拡大率を取り出し、主モニターを作業領域の一致で添字へ対応付けてから、純粋計算の compute_restore_geometry へ委ねる。モニターの取り外し・解像度変更・拡大率変更があっても画面内へ復元できるようにするのが目的。
fn sanitize_window_state(state: &WindowState, monitors: &[Monitor], primary: Option<&Monitor>) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
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










#[cfg(test)]
mod tests {
	use super::*;

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
