//! 複数の Tauri アプリで共有する処理をまとめたクレート。ウィンドウ配置の永続化、システムバックドロップやアクセント色といったネイティブ外観、枠なしウィンドウ向けのタイトルバー操作を提供する。

pub mod appearance;
pub mod titlebar;
pub mod window_state;

pub use appearance::{accent_color, apply_backdrop, read_os_accent_color, windows_build_number};
pub use titlebar::{win_close, win_is_maximized, win_minimize, win_start_drag, win_toggle_maximize};

/// このクレートが操作の対象にするウィンドウのラベル。配置の永続化もタイトルバー操作も、このラベルを持つウィンドウへ向ける。
pub const MAIN_WINDOW_LABEL: &str = "main";
