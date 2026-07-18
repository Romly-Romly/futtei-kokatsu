//! OS 標準の枠を外した(decorations:false)ウィンドウで、自作タイトルバーから呼ぶウィンドウ操作コマンド。ウィンドウ操作を JS プラグイン経由ではなく invoke へ揃えることで、アプリの capabilities に window 系の権限を並べずに済ませる。

use tauri::{AppHandle, Manager, Runtime};

use crate::MAIN_WINDOW_LABEL;




/// 自作タイトルバーの最小化ボタンから呼ぶ。OS 標準の枠を外したぶん、最小化を自前で起こす。
#[tauri::command]
pub fn win_minimize<R: Runtime>(app: AppHandle<R>) {
	if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
		let _ = window.minimize();
	}
}










/// 自作タイトルバーの最大化/元に戻すボタンと、タイトルバーのダブルクリックから呼ぶ。最大化中なら戻し、そうでなければ最大化し、操作後の最大化状態を返す。フロントはこの戻り値でボタンの図形を切り替える。
#[tauri::command]
pub fn win_toggle_maximize<R: Runtime>(app: AppHandle<R>) -> bool {
	if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
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










/// 現在の最大化状態を返す。タイトルバーのボタン図形を初期化・同期するのに使う。
#[tauri::command]
pub fn win_is_maximized<R: Runtime>(app: AppHandle<R>) -> bool {
	app.get_webview_window(MAIN_WINDOW_LABEL).and_then(|w| w.is_maximized().ok()).unwrap_or(false)
}










/// 自作タイトルバーのドラッグ領域の押下から呼び、ウィンドウの移動を始める。OS 標準のタイトルバーが無いぶん、ドラッグ移動を自前で起こす。
#[tauri::command]
pub fn win_start_drag<R: Runtime>(app: AppHandle<R>) {
	if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
		let _ = window.start_dragging();
	}
}










/// 自作タイトルバーの閉じるボタンから呼ぶ。close は CloseRequested を発火するため、ウィンドウの閉じる操作と同じ経路へ合流する。閉じる操作を横取りするかどうかはアプリ側の CloseRequested ハンドラが決める。
#[tauri::command]
pub fn win_close<R: Runtime>(app: AppHandle<R>) {
	if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
		let _ = window.close();
	}
}
