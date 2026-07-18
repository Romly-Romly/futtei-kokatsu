//! ウィンドウの地をネイティブアプリらしく見せるための処理。半透明のシステム背景(バックドロップ)と、OS のアクセント色の取得を担う。

use tauri::{AppHandle, Manager, Runtime};

use crate::MAIN_WINDOW_LABEL;

/// Mica を使える最小の Windows ビルド番号。Mica は Windows 11 以降でのみ利用できる。
const MICA_MIN_BUILD: u32 = 22000;




/// Windows のビルド番号を返す。Mica が使えるのは Windows 11(ビルド22000以上)に限られるため、バックドロップの種類を選ぶのに使う。GetVersionEx は実行ファイルのマニフェスト次第で古い版を詐称するため、詐称されない RtlGetVersion から読む。取得できなければ 0 を返す。
#[cfg(windows)]
pub fn windows_build_number() -> u32 {
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










/// Windows 以外ではビルド番号の概念が無いため常に 0 を返す。
#[cfg(not(windows))]
pub fn windows_build_number() -> u32 {
	0
}










/// メインウィンドウへ半透明のシステム背景(バックドロップ)を当て、地をネイティブアプリらしくする。背景が透けるよう、ウィンドウは tauri.conf.json で transparent:true として生成し、フロント側も最上位の地色を透過にしておく必要がある。Windows 11 では Mica、Mica の無い古い Windows では全 Windows で使える Acrylic へ退く。Mica はウィンドウのテーマに追従するため、set_theme で設定した明暗にそのまま揃う。macOS ではタイトルバー相当の Vibrancy を当てる。この素材はウィンドウの明暗アピアランスに追従し、背後の壁紙を薄く透かす。set_effects は結果を返さないため、効果を当てられない環境では静かに無効となり、フロント側の地色のまま見える。
pub fn apply_backdrop<R: Runtime>(app: &AppHandle<R>) {
	let window = match app.get_webview_window(MAIN_WINDOW_LABEL) {
		Some(w) => w,
		None => return,
	};

	#[cfg(windows)]
	{
		use tauri::utils::config::WindowEffectsConfig;
		use tauri::window::Effect;

		let effect = if windows_build_number() >= MICA_MIN_BUILD {
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










/// OS のアクセント色を "#rrggbb" 形式で読む。Windows では DWM が現在のアクセント色を ABGR の DWORD でレジストリへ持つため、そこから R,G,B を取り出す。値が無い・読めないときは None を返し、呼び出し側の既定色へフォールバックさせる。
#[cfg(windows)]
pub fn read_os_accent_color() -> Option<String> {
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










/// Windows 以外では OS のアクセント色を読まず None を返す。macOS の WKWebView をはじめ、CSS の system-color AccentColor を OS のアクセント色へ解決する WebView では、フロント側で AccentColor キーワードを書けばそのまま OS のアクセントへ追従するため、Rust から実値を流し込む必要がない。Windows の WebView2(Chromium)だけは AccentColor を固定の青へ丸めるため、Windows 実装で実値を読んで補う。
#[cfg(not(windows))]
pub fn read_os_accent_color() -> Option<String> {
	None
}










/// フロントエンドへ OS のアクセント色を "#rrggbb" で返すコマンド。フロントはこの値をテーマ変数へ流し込み、選択・オン状態の色を OS のアクセントへ追従させる。None のときはフロント側の既定値に委ねる。
#[tauri::command]
pub fn accent_color() -> Option<String> {
	read_os_accent_color()
}
