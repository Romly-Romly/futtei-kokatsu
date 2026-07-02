import { defineConfig } from "vite";

// Tauri はフロントエンドを固定ポートの開発サーバから読み込むため、Vite 側をそれに合わせて構成する
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
	// index.html とフロントエンド資産は src/ 配下に置くため、Vite の root を src に向ける
	root: "src",
	build: {
		// 成果物は tauri.conf.json の frontendDist が指すプロジェクト直下の dist へ出力する
		outDir: "../dist",
		emptyOutDir: true,
	},
	// Tauri CLI のログを消さないようにする
	clearScreen: false,
	server: {
		// Tauri が期待する固定ポート。空いていなければ起動を失敗させて気付けるようにする
		port: 1420,
		strictPort: true,
		host: host || false,
		hmr: host
			? { protocol: "ws", host, port: 1421 }
			: undefined,
		watch: {
			// Rust 側の変更で Vite が再読み込みしないよう src-tauri を監視対象から除外する
			ignored: ["**/src-tauri/**"],
		},
	},
	// Tauri がフロントエンドへ渡す環境変数を露出する
	envPrefix: ["VITE_", "TAURI_ENV_*"],
});
