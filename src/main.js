// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Romly

import { renderUsageChart, parseResetTime, paceMetrics, renderHeatmap, setDateFormat, setHeatPalette, setLocale, heatPaletteOptions, heatGradientCss, formatDateTime } from "./chart.js";
import { resolveLocale, buildDict, applyI18n, translate } from "./i18n.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// 実行中プラットフォームが macOS か。ショートカットの表記(Ctrl↔⌘)や、ネイティブメニューと重複するキー処理の抑制に使う。
const isMac = navigator.userAgent.includes("Macintosh");

// 表示する2つの利用枠。key は get_usage が返すフィールド名に対応する。shortKey はトレイのツールチップ向け短縮ラベルの辞書キー。
const METERS = [
	{ key: "session", shortKey: "meter.session.short", windowMs: 5 * 3600 * 1000 },
	{ key: "week_all", shortKey: "meter.week.short", windowMs: 7 * 24 * 3600 * 1000 },
];

// 現在の表示言語の辞書。applyLanguage が言語設定から組み立てて差し替える。JS で組み立てる文言はこの辞書を通して訳す。applyLanguage が走る前の既定言語として日本語を入れておく。
let dict = buildDict("ja");

// 辞書からキーの文言を引き、{name} 形式のプレースホルダを vars で差し替える。
function t(key, vars)
{
	return translate(dict, key, vars);
}

// 画面状態を単一のオブジェクトへ集約し、更新のたびに render() を1回だけ通す一方向の流れにする。
const state = {
	phase: "idle",
	usage: null,
	error: null,
	updatedAt: null,
	history: [],
};

let statusEl;
let errorEl;
let refreshEl;
let titlebarGaugeEl;
let sessionChartEl;
let sessionVerdictEl;
let weekChartEl;
let weekVerdictEl;
let heatmapEl;
let viewMainEl;
let viewSettingsEl;
let trendSectionEl;

// パネル列(バーンダウン2枚)の要素。段組み切り替えのアニメーションで位置を測るために保持する。
let panelEls = [];

// 直近の段組み状態(2カラムか)と各パネルの位置。ResizeObserver の発火ごとに更新し、段組みが切り替わった瞬間の旧位置として使う。
let prevTwoColumn = null;
let prevPanelRects = null;

// 進行中の段組みアニメーション。連続リサイズで多重に走らないよう、新たに始める前に取り消す。
let panelFlipAnims = [];

// パネル列が2カラムになるブレークポイント。styles.css の @media と同じ閾値にして、CSS の段組み切り替えと歩調を合わせる。
const twoColumnQuery = window.matchMedia("(min-width: 820px)");

// アニメーションを控える OS 設定。立っていれば段組みの移動アニメーションは省く。
const reduceMotionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");

// 設定ウィンドウで操作する設定値。Rust の settings.json と同じ形をそのまま保持する。
let settings = null;

// 各利用枠のメーター差込先パネルを利用枠キーで引く。renderMeters がここへ本体を差し込む。
const meterMounts = {};

// 直前にトレイへ送ったツールチップ文字列。同じ内容なら IPC を省いて毎秒の無駄打ちを抑える。
let lastTooltip = null;

// Date を hh:mm:ss 形式の文字列にする。
function clock(date)
{
	const pad = (n) => String(n).padStart(2, "0");
	return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}










// メーターのピル(リセット時点の投影使用%/ペース)の表示文言と状態を決める。
function pillFor(metrics)
{
	if (!metrics || metrics.idle) {
		return { state: "idle", text: "idle" };
	}
	if (metrics.headroom != null) {
		return { state: metrics.projEnd <= 100 ? "ok" : "warn", text: t("pill.projected", { pct: Math.round(metrics.projEnd) }) };
	}
	if (metrics.P != null) {
		return metrics.P > 1 ? { state: "warn", text: t("pill.ahead") } : { state: "ok", text: t("pill.comfortable") };
	}
	return { state: "idle", text: "—" };
}










// メーターのリセット時刻を表示形式に合わせて整形する。
// claude が返す生文字列(英語表記)を解釈できれば選択中の形式へ直し、解釈できなければ生のまま見せる。
function formatResetLabel(resetStr)
{
	const when = parseResetTime(resetStr, new Date());
	return when ? formatDateTime(when) : resetStr;
}










// 1枠分のメーターを組み立てる。
// 見出し行(余裕ピルと使用%)とバー本体(ペース色の塗り・理想キャレット・リセット残り)を別々の断片にして head と body で返す。
// 見出しは枠名と同じ行へ並べるためパネル見出し側へ差し込み、本体はメーター差込先へ入れる。タイトルはパネル側が持つ。meter が null のときは取得失敗として表示する。
function renderMeter(meter, metrics)
{
	const active = metrics && !metrics.idle;
	const over = active && metrics.P != null && metrics.P > 1;

	const pill = document.createElement("span");
	const p = pillFor(metrics);
	pill.className = "pill " + p.state;
	pill.textContent = p.text;
	const pctEl = document.createElement("span");
	pctEl.className = "meter-pct";
	pctEl.textContent = meter ? `${meter.used_pct}%` : "—";
	// 使用%(実際の値)を一番右へ置くため、ピル・使用%の順に並べる。
	const head = document.createDocumentFragment();
	head.append(pill, pctEl);

	const track = document.createElement("div");
	track.className = "meter-track";
	const fill = document.createElement("div");
	fill.className = "meter-fill " + (active ? (over ? "warn" : "ok") : "idle");

	// 100%のときは塗りが土台の右端まで届き、角ばった右端が土台の丸い右キャップを塗り潰す。右側も丸めて土台の丸みを保つ。
	if (meter && meter.used_pct >= 100)
		fill.classList.add("full");

	fill.style.width = meter ? `${meter.used_pct}%` : "0%";
	track.appendChild(fill);
	if (active && metrics.f != null)
	{
		const caret = document.createElement("div");
		caret.className = "meter-caret";
		caret.style.left = `${Math.max(0, Math.min(100, metrics.f * 100))}%`;
		track.appendChild(caret);
		track.title = t("meter.caretHint");
	}

	const reset = document.createElement("div");
	reset.className = "meter-reset";
	if (!meter)
	{
		reset.textContent = t("meter.fetchFailed");
	}
	else if (meter.resets)
	{
		reset.append(t("meter.resetAt", { when: formatResetLabel(meter.resets) }));
		const remain = document.createElement("span");
		remain.className = "meter-remain";
		remain.dataset.reset = meter.resets;
		reset.append(remain);
	}
	else
	{
		reset.textContent = t("meter.noReset");
	}

	const body = document.createDocumentFragment();
	body.append(track, reset);
	return { head, body };
}










// 画面へ出す利用枠とその取得時刻を決める。
// 現在値があればそれを、無ければ履歴の最新サンプルへ退いてその時刻を併せて返す。
// 取得に失敗して現在値が得られなくても、蓄積した履歴から表示を保つための土台。at は状態行がデータの古さを示すのに使う。
function displayUsage()
{
	if (state.usage)
		return { usage: state.usage, at: state.updatedAt };

	for (let i = state.history.length - 1; i >= 0; i--)
	{
		const s = state.history[i];
		if (s.session || s.week_all || s.week_sonnet)
			return { usage: s, at: new Date(s.ts) };
	}

	return { usage: null, at: null };
}










// 各枠の現在値・ペース指標を現在時刻基準で計算する。
// メーター描画とツールチップ生成が同じ値を共有するためまとめて返す。現在値が無いときは履歴の最新サンプルを使い、貯めた値で描き続ける。
function computeRows(now)
{
	const usage = displayUsage().usage;
	return METERS.map((def) =>
	{
		const meter = usage ? usage[def.key] : null;
		const metrics = meter ? paceMetrics(state.history, def.key, def.windowMs, resetStringFor(def.key), now, meter.used_pct) : { idle: true };
		return { def, meter, metrics };
	});
}










// メーターを現在時刻基準で組み立て直す。
// 経過率(キャレット)・ペース色・余裕ピルは時間とともに動くため、毎秒呼んで追従させる。トレイのツールチップも同じ値で更新する。
function renderMeters()
{
	const now = new Date();
	const rows = computeRows(now);
	for (const r of rows)
	{
		const slot = meterMounts[r.def.key];
		if (!slot) continue;

		const { head, body } = renderMeter(r.meter, r.metrics);
		slot.head.replaceChildren(head);
		slot.mount.replaceChildren(body);
	}
	tickCountdowns();
	updateTooltip(rows, now);
}










// 取得時刻から現在までの隔たりを「たった今」「X秒前」「X分前」「X時間Y分前」「X日前」へ整形する。
function relativeAgo(from, now)
{
	const sec = Math.max(0, Math.floor((now.getTime() - from.getTime()) / 1000));
	if (sec < 5)
		return t("time.justNow");

	if (sec < 60)
		return t("time.secondsAgo", { n: sec });

	const min = Math.floor(sec / 60);
	if (min < 60)
		return t("time.minutesAgo", { n: min });

	const h = Math.floor(min / 60);
	if (h < 24)
		return t("time.hoursMinutesAgo", { h, m: min % 60 });

	return t("time.daysAgo", { n: Math.floor(h / 24) });
}










// 状態行とエラー帯を組み立てる。
// 状態行は表示中データの取得時刻に「何秒/何分前」を添え、毎秒の tick からも呼んで相対表示を進める。
// エラーは状態行を置き換えず別の帯として添えるだけにして、取得に失敗しても貯めたデータの表示を残す。
function renderStatus()
{
	const shown = displayUsage();
	if (state.phase === "loading" && !shown.at)
	{
		statusEl.textContent = t("status.loading");
		statusEl.className = "status status-loading";
	}
	else if (shown.at)
	{
		statusEl.textContent = t("status.lastUpdated", { time: clock(shown.at), ago: relativeAgo(shown.at, new Date()), count: state.history.length });
		statusEl.className = "status";
	}
	else
	{
		statusEl.textContent = t("history.empty");
		statusEl.className = "status";
	}

	if (state.error)
	{
		errorEl.textContent = t("status.fetchError", { error: state.error });
		errorEl.hidden = false;
	}
	else
	{
		errorEl.hidden = true;
	}
}










// state を画面へ反映する唯一の出口。DOM の書き換えはここに集約する。
function render()
{
	renderStatus();

	refreshEl.disabled = state.phase === "loading";

	renderMeters();

	renderCharts();
	renderHeatmap(heatmapEl, state.history, "session", new Date());

	// タイトルバー左上のゲージも最新の消費率へ追従させる。IPC を挟むため非同期に更新する。
	updateTitlebarGauge();
}










// 直前にタイトルバーのゲージへ渡した寸法と消費率。同じなら再描画と IPC を省く。
let lastTitlebarGaugeKey = null;

// タイトルバー左上のゲージを、現在の消費率でトレイと同じ図柄へ更新する。実画素数は表示倍率に合わせて求め、Rust の render_gauge_rgba が返す RGBA を canvas へ putImageData する。両枠とも値が無いとき、または空が返るプラットフォーム(macOS 等)では隠す。
async function updateTitlebarGauge()
{
	if (!titlebarGaugeEl)
		return;

	const usage = state.usage;
	const session = usage && usage.session ? usage.session.used_pct : null;
	const week = usage && usage.week_all ? usage.week_all.used_pct : null;
	if (session == null && week == null)
	{
		titlebarGaugeEl.hidden = true;
		lastTitlebarGaugeKey = null;
		return;
	}

	// CSS 上は 16px 角。表示倍率を掛けた実画素数で焼くことで、別 DPI でもぼけさせない。
	const phys = Math.max(1, Math.round(16 * (window.devicePixelRatio || 1)));
	const key = `${phys}|${session}|${week}`;
	if (key === lastTitlebarGaugeKey)
		return;

	let rgba;
	try
	{
		rgba = await invoke("gauge_icon_rgba", { size: phys, session, week });
	}
	catch (e)
	{
		titlebarGaugeEl.hidden = true;
		return;
	}

	if (!rgba || rgba.length === 0)
	{
		// 空が返るのは macOS 等、ウィンドウ左上にアイコンを置かないプラットフォーム。
		titlebarGaugeEl.hidden = true;
		return;
	}

	// 返ってきた画素数から実際の辺長を求め、Rust 側のクランプと食い違っても破綻しないようにする。
	const n = Math.round(Math.sqrt(rgba.length / 4));
	titlebarGaugeEl.width = n;
	titlebarGaugeEl.height = n;
	const ctx = titlebarGaugeEl.getContext("2d");
	ctx.putImageData(new ImageData(new Uint8ClampedArray(rgba), n, n), 0, 0);
	titlebarGaugeEl.hidden = false;
	lastTitlebarGaugeKey = key;
}




// get_usage を呼んで state を更新する。呼び出しの前後で必ず render() を通す。
async function refresh()
{
	state.phase = "loading";
	state.error = null;
	render();

	try
	{
		state.usage = await invoke("get_usage");
		state.phase = "ready";
		state.updatedAt = new Date();
	}
	catch (e)
	{
		state.phase = "error";
		state.error = typeof e === "string" ? e : String(e);
	}

	try
	{
		state.history = await invoke("get_history");
	}
	catch (e)
	{
		// 履歴の取得失敗は表示の主目的ではないため握りつぶす
	}

	render();
}










// ポーラーからの通知を受け取り、採れたばかりの利用枠で画面を更新する。
async function applyUsage(usage)
{
	state.usage = usage;
	state.phase = "ready";
	state.error = null;
	state.updatedAt = new Date();

	try
	{
		state.history = await invoke("get_history");
	}
	catch (e)
	{
		// 履歴の取得失敗は表示の主目的ではないため握りつぶす
	}

	render();
}










// 蓄積した時系列から session と week のバーンダウンを描き、判定バッジを更新する。
function renderCharts()
{
	const now = new Date();
	const windows = [
		// セッション枠は5時間ぶんと短く、チャートが最小サイズでも全体が読めるため、後半での自動枠取り(右上へのズーム)を効かせず常に全体表示にする。週枠は7日と長いので自動枠取りを残す。
		{ key: "session", mount: sessionChartEl, badge: sessionVerdictEl, windowMs: 5 * 3600 * 1000, timeLabel: "time", fullView: true },
		{ key: "week_all", mount: weekChartEl, badge: weekVerdictEl, windowMs: 7 * 24 * 3600 * 1000, timeLabel: "date" },
	];

	for (const w of windows)
	{
		const resetStr = resetStringFor(w.key);
		if (!resetStr)
		{
			w.mount.replaceChildren();
			setBadge(w.badge, { state: "idle", label: "" });
			continue;
		}
		const verdict = renderUsageChart(w.mount, {
			history: state.history,
			key: w.key,
			resetStr,
			windowMs: w.windowMs,
			now,
			timeLabel: w.timeLabel,
			fullView: w.fullView,
		});
		setBadge(w.badge, verdict);
	}
}










// パネル列の各パネルの現在位置を測って返す。段組みアニメーションの旧位置・新位置の記録に使う。
function capturePanelRects()
{
	return panelEls.map((el) => el.getBoundingClientRect());
}




// パネルを旧位置から新位置へ滑らせる(FLIP)。
// 1列⇔2列の段組み切り替えは離散的で CSS の transition では補間できないため、切り替え後の位置で一旦旧位置へ戻す平行移動を当て、それを0へアニメーションして移動を見せる。OS がアニメーション抑制を望むときは何もしない。
function animatePanelReflow(firstRects, lastRects)
{
	if (reduceMotionQuery.matches)
		return;

	for (const anim of panelFlipAnims)
	{
		anim.cancel();
	}

	panelFlipAnims = [];
	panelEls.forEach((el, i) => {
		const first = firstRects[i];
		const last = lastRects[i];
		if (!first || !last) {
			return;
		}
		const dx = first.left - last.left;
		const dy = first.top - last.top;
		if (Math.abs(dx) < 1 && Math.abs(dy) < 1) {
			return;
		}
		const anim = el.animate(
			[{ transform: `translate(${dx}px, ${dy}px)` }, { transform: "translate(0, 0)" }],
			{ duration: 260, easing: "cubic-bezier(0.2, 0, 0, 1)" }
		);
		panelFlipAnims.push(anim);
	});
}




// チャートのマウント寸法が変わるたびに呼ぶ。
// 今の寸法でバーンダウンを描き直し、段組みが1列⇔2列で切り替わっていればパネルの移動をアニメーションする。
// 旧位置は直前の発火時に記録したものを使う。発火の時点ではレイアウトは既に新しい段組みのため、同期読み取りでは旧位置を得られないことへの対処。
function onChartBoxResize()
{
	if (viewMainEl.hidden)
		return;

	const lastRects = capturePanelRects();
	const nowTwoColumn = twoColumnQuery.matches;
	if (prevPanelRects && prevTwoColumn !== null && nowTwoColumn !== prevTwoColumn)
		animatePanelReflow(prevPanelRects, lastRects);

	prevTwoColumn = nowTwoColumn;
	prevPanelRects = lastRects;
	renderCharts();
}




// 利用枠のリセット文字列を、現在値があればそれを、無ければ履歴の最新サンプルから得る。
function resetStringFor(key)
{
	if (state.usage && state.usage[key] && state.usage[key].resets)
		return state.usage[key].resets;

	for (let i = state.history.length - 1; i >= 0; i--)
	{
		const meter = state.history[i][key];
		if (meter && meter.resets)
			return meter.resets;
	}

	return null;
}










// 判定バッジに文言と状態クラスを反映する。
function setBadge(badgeEl, verdict)
{
	badgeEl.textContent = verdict.label;
	badgeEl.className = "verdict " + verdict.state;
}










// 残りミリ秒を「X時間YY分」「Y分ZZ秒」形式へ整形する。
function formatRemaining(ms)
{
	if (ms <= 0)
		return t("duration.soon");

	const totalSec = Math.floor(ms / 1000);
	const d = Math.floor(totalSec / 86400);
	const h = Math.floor((totalSec % 86400) / 3600);
	const m = Math.floor((totalSec % 3600) / 60);
	const sec = totalSec % 60;
	if (d > 0)
		return t("duration.dayHour", { d, h });

	if (h > 0)
		return t("duration.hourMin", { h, m: String(m).padStart(2, "0") });

	if (m > 0)
		return t("duration.minSec", { m, s: String(sec).padStart(2, "0") });

	return t("duration.sec", { s: sec });
}










// メーターのリセット残り時間を毎秒更新する。reset 文字列は各 span の data 属性から読む。
function tickCountdowns()
{
	const now = new Date();
	for (const span of document.querySelectorAll(".meter-remain"))
	{
		const reset = parseResetTime(span.dataset.reset, now);
		span.textContent = reset ? t("meter.remaining", { time: formatRemaining(reset.getTime() - now.getTime()) }) : "";
	}
}










// 残りミリ秒を分粒度の「X日とY時間」「X時間Y分」「X分」へ整形する。ツールチップは秒まで見せず、文字列の変化を1分に1度に抑えて IPC を節約する。
function coarseRemaining(ms)
{
	if (ms <= 0)
		return t("duration.soon");

	const totalMin = Math.floor(ms / 60000);
	const d = Math.floor(totalMin / 1440);
	const h = Math.floor((totalMin % 1440) / 60);
	const m = totalMin % 60;
	if (d > 0)
		return t("duration.dayHour", { d, h });

	if (h > 0)
		return t("duration.hourMin", { h, m });

	return t("duration.min", { m });
}










// 1枠分の要約行を作る。使用%・ペースピル・リセットまでの残り時間を1行へまとめる。
function tooltipLine(row, now)
{
	const short = t(row.def.shortKey);
	if (!row.meter)
		return t("tooltip.fetchFailed", { name: short });

	const pct = `${row.meter.used_pct}%`;
	if (!row.metrics || row.metrics.idle)
		return `${short} ${pct}`;

	let line = `${short} ${pct} · ${pillFor(row.metrics).text}`;
	const resetStr = resetStringFor(row.def.key);
	const reset = resetStr ? parseResetTime(resetStr, now) : null;
	if (reset)
		line += " · " + t("tooltip.remaining", { time: coarseRemaining(reset.getTime() - now.getTime()) });

	return line;
}










// トレイのツールチップ全文を組み立てる。各枠の要約行を並べる。表示できる利用枠が一つも無いときはアプリ名だけにする。rows は computeRows が現在値か履歴の最新サンプルから組んだものなので、現在値の有無ではなく rows の中身で判定する。
function buildTooltip(rows, now)
{
	if (!rows.some((r) => r.meter))
		return t("app.name");

	const lines = [];
	for (const r of rows)
	{
		lines.push(tooltipLine(r, now));
	}

	return lines.join("\n");
}










// 要約一行をトレイのツールチップへ送る。直前と同じ内容なら IPC を省く。窓を隠していても webview は生きているため隠れたまま更新できる。
function updateTooltip(rows, now)
{
	const text = buildTooltip(rows, now);
	if (text === lastTooltip)
		return;

	lastTooltip = text;
	invoke("set_tray_tooltip", { text }).catch(() => {});
}










// 設定を Rust から読み込んで画面へ反映する。ファイルが無い初回は既定値が返る。日付形式が localStorage に保存されている場合は設定ファイルへ移して取り込み、保存先を一本化する。
async function initSettings()
{
	try
	{
		settings = await invoke("get_settings");
	}
	catch (e)
	{
		settings = { theme: "system", language: "system", show_trend: true, date_format: "intl", heat_palette: "standard", tray_style: "burndown-session", hide_on_blur: false };
	}

	const legacy = localStorage.getItem("dateFormat");
	if (legacy === "jp" || legacy === "intl")
	{
		settings.date_format = legacy;
		localStorage.removeItem("dateFormat");
		saveSettings();
	}

	applySettingsToUi();

	// 自動起動の登録状態は settings.json ではなくレジストリの Run キーが持つため、設定ファイルとは別に Rust から現在値を読んでトグルへ反映する。取得失敗時はトグルを既定のオフ表示のままにする。
	try
	{
		document.querySelector("#autostart-toggle").checked = await invoke("get_autostart");
	}
	catch (e)
	{
		// 取得失敗は表示の主目的ではないため握りつぶす
	}
}










// 設定の各値を画面へ反映する。日付形式・消費傾向の表示・各セグメントの選択状態・言語をまとめて合わせる。テーマの配色適用は Rust 側(set_theme)が起動時と保存時に行うため、ここでは選択状態の見た目だけ整える。
function applySettingsToUi()
{
	setDateFormat(settings.date_format === "jp" ? "jp" : "intl");
	setHeatPalette(settings.heat_palette || "standard");
	trendSectionEl.hidden = !settings.show_trend;
	document.querySelector("#trend-toggle").checked = !!settings.show_trend;
	document.querySelector("#blur-hide-toggle").checked = !!settings.hide_on_blur;
	setSegmentedActive("#theme-seg", settings.theme);
	setSegmentedActive("#lang-seg", settings.language);
	setSegmentedActive("#datefmt-seg", settings.date_format);
	applyLanguage();
	render();
}










// 言語設定から実際の表示言語を決め、辞書を組み立ててモジュールへ保持する。data-i18n を持つ静的要素を差し替え、JS で組み立てる文言とチャート側も同じ辞書へ揃える。html の lang 属性も合わせる。
function applyLanguage()
{
	const locale = resolveLocale(settings.language, navigator.language);
	dict = buildDict(locale);
	applyI18n(document, dict);
	setLocale(dict);
	document.documentElement.lang = locale;
	// 選択肢名は applyI18n が入れ替えるため、その後でピッカーの表示(選択中ボタンの名前と各帯)を取り直す。
	updateHeatCombo();
	updateTrayCombo();
}










// 現在の設定を Rust へ保存する。保存と同時にテーマがウィンドウへ反映される。
async function saveSettings()
{
	try
	{
		await invoke("set_settings", { settings });
	}
	catch (e)
	{
		// 設定の保存失敗は表示の主目的ではないため握りつぶす
	}
}










// セグメント切り替え群のうち、指定値のボタンだけに active を付ける。
function setSegmentedActive(selector, value)
{
	const seg = document.querySelector(selector);
	if (!seg)
		return;

	for (const btn of seg.querySelectorAll("button"))
	{
		btn.classList.toggle("active", btn.dataset.value === value);
	}
}










// 1つのセグメント切り替え群へ click 配線を施す。押されたボタンを選択状態にし、その data-value を onChange へ渡す。
function bindSegmented(selector, onChange)
{
	const seg = document.querySelector(selector);
	if (!seg)
		return;

	seg.addEventListener("click", (event) => {
		const btn = event.target.closest("button");
		if (!btn || !seg.contains(btn))
			return;

		setSegmentedActive(selector, btn.dataset.value);
		onChange(btn.dataset.value);
	});
}










// 自前ドロップダウン(コンボボックス)の共通の対話を配線する。トリガーでの開閉、一覧・外側クリックや Esc での閉じ、上下キーでの移動、Enter/Space・クリックでの選択を受け持ち、選ばれた値で choose(value) を呼ぶ。選択肢の中身(名前や配色の帯)は呼び出し側が用意する。
function wireCombo(combo, trigger, list, choose)
{
	const options = () => Array.from(list.querySelectorAll(".combo-option"));
	const focusOption = (i) => {
		const opts = options();
		if (opts[i]) {
			opts[i].focus();
		}
	};
	const close = () => {
		list.hidden = true;
		trigger.setAttribute("aria-expanded", "false");
	};
	const open = () => {
		list.hidden = false;
		trigger.setAttribute("aria-expanded", "true");
	};
	const pick = (value) => {
		choose(value);
		close();
		trigger.focus();
	};

	trigger.addEventListener("click", () => {
		if (list.hidden) {
			open();
		} else {
			close();
		}
	});

	list.addEventListener("click", (event) => {
		const opt = event.target.closest(".combo-option");
		if (!opt || !list.contains(opt)) {
			return;
		}
		pick(opt.dataset.value);
	});

	// ピッカーの外側をクリックしたら閉じる。設定ビュー内の別のコントロールへ移っても開きっぱなしにしない。
	document.addEventListener("click", (event) => {
		if (!combo.contains(event.target)) {
			close();
		}
	});

	combo.addEventListener("keydown", (event) => {
		if (event.key === "Escape") {
			if (!list.hidden) {
				close();
				trigger.focus();
			}
			return;
		}
		if (list.hidden) {
			// 閉じている時は下キーで開いて先頭へ移る。Enter/Space はボタンの既定動作(開閉の切り替え)に任せる。
			if (event.key === "ArrowDown") {
				event.preventDefault();
				open();
				focusOption(0);
			}
			return;
		}
		const opts = options();
		const at = opts.indexOf(document.activeElement);
		if (event.key === "ArrowDown") {
			event.preventDefault();
			focusOption(at < 0 ? 0 : Math.min(opts.length - 1, at + 1));
		} else if (event.key === "ArrowUp") {
			event.preventDefault();
			focusOption(at <= 0 ? 0 : at - 1);
		} else if (event.key === "Enter" || event.key === " ") {
			if (at >= 0) {
				event.preventDefault();
				pick(opts[at].dataset.value);
			}
		}
	});
}









// 配色ピッカーの表示を現在の設定へ合わせる。各選択肢と閉じた時のボタンへ、パレット名とその帯グラデを反映する。グレイスケールの帯は現テーマで解決されるため、テーマ切替時もこれを呼ぶ。名前は applyI18n が選択肢へ入れた文字を写すので、言語反映の後に呼ぶ。
function updateHeatCombo()
{
	const list = document.querySelector("#heat-combo-list");
	if (!list)
		return;

	const current = settings.heat_palette || "standard";
	for (const opt of list.querySelectorAll(".combo-option"))
	{
		const value = opt.dataset.value;
		const selected = value === current;
		opt.setAttribute("aria-selected", selected ? "true" : "false");
		const swatch = opt.querySelector(".combo-swatch");
		if (swatch)
			swatch.style.background = heatGradientCss(value);

		if (selected)
		{
			const name = opt.querySelector(".combo-name");
			document.querySelector("#heat-combo-name").textContent = name ? name.textContent : value;
			document.querySelector("#heat-combo-swatch").style.background = heatGradientCss(value);
		}
	}
}










// 配色ピッカーの選択肢を組み立てて配線する。素の select では帯を出せないため、名前と帯を載せた自前のドロップダウンにする。開閉・外側クリックや Esc での close・キーボード操作・選択時の即時反映と保存をまとめて受け持つ。
function buildHeatCombo()
{
	const combo = document.querySelector("#heat-combo");
	const trigger = document.querySelector("#heat-combo-trigger");
	const list = document.querySelector("#heat-combo-list");
	if (!combo || !trigger || !list)
		return;


	// 選択肢を生成する。名前は data-i18n で言語切替に追従し、帯は updateHeatCombo がパレットから描く。
	for (const item of heatPaletteOptions())
	{
		const opt = document.createElement("li");
		opt.className = "combo-option";
		opt.setAttribute("role", "option");
		opt.dataset.value = item.value;
		opt.tabIndex = -1;
		const name = document.createElement("span");
		name.className = "combo-name";
		name.dataset.i18n = item.i18n;
		const swatch = document.createElement("span");
		swatch.className = "combo-swatch";
		swatch.setAttribute("aria-hidden", "true");
		opt.appendChild(name);
		opt.appendChild(swatch);
		list.appendChild(opt);
	}

	wireCombo(combo, trigger, list, (value) => {
		settings.heat_palette = value;
		setHeatPalette(value);
		updateHeatCombo();
		render();
		saveSettings();
	});
}










// トレイアイコンのピッカーの表示を現在の設定へ合わせる。選択中の選択肢へ aria-selected を立て、閉じた時のボタンへその名前を写す。名前は applyI18n が選択肢へ入れた文字を写すため、言語反映の後に呼ぶ。
function updateTrayCombo()
{
	const list = document.querySelector("#tray-combo-list");
	if (!list)
		return;

	const current = settings.tray_style || "burndown-session";
	for (const opt of list.querySelectorAll(".combo-option"))
	{
		const selected = opt.dataset.value === current;
		opt.setAttribute("aria-selected", selected ? "true" : "false");
		if (selected)
		{
			const name = opt.querySelector(".combo-name");
			document.querySelector("#tray-combo-name").textContent = name ? name.textContent : opt.dataset.value;
		}
	}
}









// トレイアイコンのピッカーを配線する。選択肢は index.html に静的に置いてあるため、開閉・選択の対話だけを共通処理へ委ねる。選択時は設定を書き換えて保存し、Rust 側がトレイアイコンを描き直す。
function buildTrayCombo()
{
	const combo = document.querySelector("#tray-combo");
	const trigger = document.querySelector("#tray-combo-trigger");
	const list = document.querySelector("#tray-combo-list");
	if (!combo || !trigger || !list)
		return;

	wireCombo(combo, trigger, list, (value) => {
		settings.tray_style = value;
		updateTrayCombo();
		saveSettings();
	});
}









// 消費傾向ヒートマップの表示/非表示を切り替える。設定値・セクションの表示・設定画面のトグルの見えをまとめて合わせ、右クリックメニューと設定トグルのどちらから変えても両者が食い違わないようにする。
function setShowTrend(on)
{
	settings.show_trend = on;
	trendSectionEl.hidden = !on;
	const toggle = document.querySelector("#trend-toggle");
	if (toggle)
		toggle.checked = on;

	saveSettings();
}




// 設定ビューのコントロールを配線する。歯車ボタンと戻るボタンでビューを切り替え、各コントロールは設定値を書き換えて保存し、即時の反映が要るもの(言語・消費傾向・日付形式)はその場で画面へ通す。
function wireSettings()
{
	document.querySelector("#open-settings").addEventListener("click", showSettings);
	document.querySelector("#settings-back").addEventListener("click", showMain);

	bindSegmented("#theme-seg", (value) => {
		settings.theme = value;
		saveSettings();
	});
	bindSegmented("#lang-seg", (value) => {
		settings.language = value;
		applyLanguage();
		render();
		saveSettings();
	});
	bindSegmented("#datefmt-seg", (value) => {
		settings.date_format = value;
		setDateFormat(value === "jp" ? "jp" : "intl");
		render();
		saveSettings();
	});
	buildHeatCombo();
	buildTrayCombo();

	document.querySelector("#trend-toggle").addEventListener("change", (event) => {
		setShowTrend(event.target.checked);
	});

	document.querySelector("#blur-hide-toggle").addEventListener("change", (event) => {
		settings.hide_on_blur = event.target.checked;
		saveSettings();
	});

	document.querySelector("#autostart-toggle").addEventListener("change", async (event) => {
		const enabled = event.target.checked;
		try
		{
			await invoke("set_autostart", { enabled });
		}
		catch (e)
		{
			// 登録・解除に失敗したら、トグルを実際の登録状態(変更前)へ戻す
			event.target.checked = !enabled;
		}
	});
}










// 設定ビューへ切り替える。メイン表示を隠し、設定画面を出す。
function showSettings()
{
	viewMainEl.hidden = true;
	viewSettingsEl.hidden = false;
}










// メイン表示へ戻す。
function showMain()
{
	viewSettingsEl.hidden = true;
	viewMainEl.hidden = false;
	// 設定表示中に段組みや窓の寸法が変わっていることがあるため、戻った今の寸法でバーンダウンを描き直す。FLIP の基準も今の段組みで取り直し、復帰そのものでパネルの移動アニメーションが走らないようにする。
	prevTwoColumn = twoColumnQuery.matches;
	prevPanelRects = capturePanelRects();
	renderCharts();
}










// カスタムタイトルバーを配線する。ウィンドウ操作は capabilities を増やさないため Rust コマンド経由で呼ぶ。ドラッグはダブルクリック(最大化)を奪わないよう、押下位置から一定量動いてから移動を起こす。
function wireTitlebar()
{
	const drag = document.querySelector("#titlebar-drag");
	const maxIco = document.querySelector("#tb-max .tb-ico");

	// 最大化状態に合わせてボタンの図形(最大化↔元に戻す)を切り替える。
	const applyMaxIcon = (maximized) => {
		maxIco.textContent = maximized ? "" : "";
	};

	document.querySelector("#tb-min").addEventListener("click", () => invoke("win_minimize"));
	document.querySelector("#tb-max").addEventListener("click", () => invoke("win_toggle_maximize").then(applyMaxIcon));
	document.querySelector("#tb-close").addEventListener("click", () => invoke("win_close"));

	// 押下位置からしきい値(4px)を超えて動いたときだけ移動を始める。わずかな動きで始めるとダブルクリックでの最大化を奪うため。
	let down = null;
	drag.addEventListener("mousedown", (event) => {
		if (event.button === 0) {
			down = { x: event.screenX, y: event.screenY };
		}
	});
	drag.addEventListener("mousemove", (event) => {
		if (down && (event.buttons & 1) && (Math.abs(event.screenX - down.x) > 4 || Math.abs(event.screenY - down.y) > 4)) {
			down = null;
			invoke("win_start_drag");
		}
	});
	window.addEventListener("mouseup", () => {
		down = null;
	});
	drag.addEventListener("dblclick", () => invoke("win_toggle_maximize").then(applyMaxIcon));

	// 初期状態と、ボタン以外(Win+↑・スナップ等)による最大化の変化に図形を追従させる。
	invoke("win_is_maximized").then(applyMaxIcon).catch(() => {});
	listen("win-maximized", (event) => applyMaxIcon(event.payload));
}










// 右クリックメニューの各項目に対応する動作を起こす。更新・設定・消費傾向の切替はフロントの既存処理をそのまま呼び、ウィンドウを閉じる/終了は Rust コマンドへ回す。閉じる(win_close)は CloseRequested 経由でトレイへ隠して計測を続け、終了(quit_app)は配置を保存してプロセスを終える。
function runMenuAction(action)
{
	switch (action)
	{
		case "refresh":
			refresh();
			break;
		case "settings":
			showSettings();
			break;
		case "trend":
			if (settings)
				setShowTrend(!settings.show_trend);
			break;
		case "close":
			invoke("win_close");
			break;
		case "quit":
			invoke("quit_app");
			break;
	}
}




// 右クリックメニューのショートカット表記を実行中プラットフォームへ合わせる。HTML には Windows の Ctrl 表記を持たせてあり、macOS では ⌘ 表記へ差し替える。実際のキー処理は Windows が keydown、macOS がネイティブのアプリメニューで担う。
function applyMenuShortcutLabels()
{
	if (!isMac)
		return;

	const labels = { refresh: "⌘R", settings: "⌘,", close: "⌘W" };
	for (const item of document.querySelectorAll("#context-menu .ctx-item"))
	{
		const shortcut = item.querySelector(".ctx-shortcut");
		const label = labels[item.dataset.action];
		if (shortcut && label)
			shortcut.textContent = label;
	}
}




// メインウィンドウの右クリックメニューを配線する。右クリックで自前メニューを開き、項目のクリックで動作を起こす。外側クリック・Esc・スクロール・リサイズ・フォーカス喪失で閉じ、上下キーで項目を辿れるようにする。文言と表示切替のチェック状態は開くたびに現在の設定へ合わせる。
function wireContextMenu()
{
	const menu = document.querySelector("#context-menu");
	if (!menu)
		return;

	applyMenuShortcutLabels();

	const items = () => Array.from(menu.querySelectorAll(".ctx-item"));

	const focusItem = (i) => {
		const list = items();
		if (list[i])
			list[i].focus();
	};

	const closeMenu = () => {
		if (menu.hidden)
			return;
		menu.hidden = true;
	};

	const openMenuAt = (x, y) => {
		// チェック項目(消費傾向の表示)の見えを現在の設定へ合わせてから開く。
		const trendItem = menu.querySelector('[data-action="trend"]');
		if (trendItem)
			trendItem.setAttribute("aria-checked", settings && settings.show_trend ? "true" : "false");

		// クリック位置へ置いてから表示し、寸法を測ってビューポートからはみ出す分だけ内側へ寄せる。先に位置を当てることで、旧位置での一瞬のちらつきを避ける。
		const margin = 6;
		menu.style.left = `${x}px`;
		menu.style.top = `${y}px`;
		menu.hidden = false;
		const rect = menu.getBoundingClientRect();
		let left = x;
		let top = y;
		if (left + rect.width + margin > window.innerWidth)
			left = window.innerWidth - rect.width - margin;
		if (top + rect.height + margin > window.innerHeight)
			top = window.innerHeight - rect.height - margin;
		menu.style.left = `${Math.max(margin, left)}px`;
		menu.style.top = `${Math.max(margin, top)}px`;

		// キーボード操作の受け皿としてコンテナへ focus を移す。項目へ直接移さないことで、右クリックで開いた直後に項目へ枠が付くのを避ける。
		menu.focus();
	};

	// 右クリック(・メニューキー)で既定のメニューを止め、自前メニューを開く。dev・release とも自前メニューへ置き換える。
	window.addEventListener("contextmenu", (event) => {
		event.preventDefault();
		openMenuAt(event.clientX, event.clientY);
	});

	// 項目を選んだらメニューを閉じて対応する動作を起こす。
	menu.addEventListener("click", (event) => {
		const item = event.target.closest(".ctx-item");
		if (!item || !menu.contains(item))
			return;
		closeMenu();
		runMenuAction(item.dataset.action);
	});

	// メニュー外の押下で閉じる。押下時点で判定することで、項目クリックの流れ(押下はメニュー内)を邪魔しない。
	document.addEventListener("mousedown", (event) => {
		if (!menu.hidden && !menu.contains(event.target))
			closeMenu();
	});
	// 窓のフォーカス喪失・リサイズ・本体のスクロールでは位置がずれるため閉じる。スクロールは window-body が担うため、そこで捕捉する。
	window.addEventListener("blur", closeMenu);
	window.addEventListener("resize", closeMenu);
	document.querySelector(".window-body").addEventListener("scroll", closeMenu, true);

	menu.addEventListener("keydown", (event) => {
		const list = items();
		const at = list.indexOf(document.activeElement);
		if (event.key === "Escape")
		{
			closeMenu();
		}
		else if (event.key === "ArrowDown")
		{
			event.preventDefault();
			focusItem(at < 0 ? 0 : Math.min(list.length - 1, at + 1));
		}
		else if (event.key === "ArrowUp")
		{
			event.preventDefault();
			focusItem(at < 0 ? list.length - 1 : Math.max(0, at - 1));
		}
		// Enter/Space は項目(button)の既定動作(click)へ任せる。
	});
}




// release ビルドでのみ、WebView2 のリロード・devtools 起動キーを封じてネイティブアプリらしくする。F5・Ctrl+R によるリロードと、F12 などの devtools 起動キーを抑止する。右クリックの既定メニューは wireContextMenu が dev・release とも自前メニューへ置き換えるためここでは扱わない。dev ビルドではキー操作は調査の妨げになるため抑止しない。
function suppressWebChrome()
{
	if (!import.meta.env.PROD)
		return;

	window.addEventListener("keydown", (e) => {
		const ctrl = e.ctrlKey || e.metaKey;
		// F5 / Ctrl+R(・Ctrl+Shift+R): ページのリロード
		const reload = e.code === "F5" || (ctrl && e.code === "KeyR");
		// F12 / Ctrl+Shift+I / Ctrl+Shift+J / Ctrl+Shift+C: devtools の起動
		const devtools = e.code === "F12" || (ctrl && e.shiftKey && (e.code === "KeyI" || e.code === "KeyJ" || e.code === "KeyC"));
		if (reload || devtools) {
			e.preventDefault();
		}
	});
}










// アクセント色の上に乗せる文字色を、背景の明るさから白か黒で選ぶ。明るいアクセント色のとき白文字が潰れるのを避ける。各成分を相対輝度へ直し、白とのコントラストと黒とのコントラストが等しくなる輝度(約0.179)をしきい値に振り分ける。
function accentTextColor(hex)
{
	const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
	if (!m)
		return "#ffffff";

	const lin = (h) => {
		const c = parseInt(h, 16) / 255;
		return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
	};
	const luminance = 0.2126 * lin(m[1]) + 0.7152 * lin(m[2]) + 0.0722 * lin(m[3]);
	return luminance > 0.179 ? "#0a0a0a" : "#ffffff";
}










// OS のアクセント色を CSS 変数 --accent / --accent-text へ流し込み、選択・オン状態の色を OS のアクセントへ追従させる。Windows の WebView2(Chromium)は CSS の system-color AccentColor を OS アクセントではなく固定の青へ丸めるため、Rust が読んだ OS の実値をここで当てて補う。Rust が値を返さないプラットフォーム(AccentColor を OS アクセントへ解決する WebView)では上書きを外し、styles.css の既定値に委ねる。
async function applyAccentColor()
{
	let hex;
	try
	{
		hex = await invoke("get_accent_color");
	}
	catch
	{
		return;
	}

	const root = document.documentElement.style;
	if (hex)
	{
		root.setProperty("--accent", hex);
		root.setProperty("--accent-text", accentTextColor(hex));
	}
	else
	{
		root.removeProperty("--accent");
		root.removeProperty("--accent-text");
	}
}










// タイトルバーのアプリ名の右へバージョンを表示する。値は tauri.conf.json 由来の PackageInfo から Rust の app_version が返すため、トレイメニューの見出しと出所が揃い、二重管理にならない。取得に失敗したらバージョン表示は空のままにする。
async function applyAppVersion()
{
	try
	{
		const version = await invoke("app_version");
		const el = document.querySelector("#titlebar-version");
		if (el && version)
			el.textContent = `v${version}`;
	}
	catch (e)
	{
		// バージョン表示は補助情報のため、取得失敗は握りつぶす
	}
}




// macOS では OS 標準のタイトルバー(信号機ボタン)を活かすため、<html> に is-mac クラスを付ける。styles.css がこのクラスで自前のキャプションボタンを隠し、左上へ重なる信号機ボタンのぶんドラッグ領域に余白を空ける。Windows・Linux ではマッチせずクラスは付かないため、既定のレイアウトのまま変わらない。
function applyPlatformClass()
{
	if (isMac)
		document.documentElement.classList.add("is-mac");
}










window.addEventListener("DOMContentLoaded", () => {
	applyPlatformClass();
	suppressWebChrome();
	applyAccentColor();
	applyAppVersion();

	statusEl = document.querySelector("#status");
	errorEl = document.querySelector("#error");
	refreshEl = document.querySelector("#refresh");
	titlebarGaugeEl = document.querySelector("#titlebar-gauge");
	sessionChartEl = document.querySelector("#session-chart");
	sessionVerdictEl = document.querySelector("#session-verdict");
	weekChartEl = document.querySelector("#week-chart");
	weekVerdictEl = document.querySelector("#week-verdict");
	heatmapEl = document.querySelector("#heatmap");
	viewMainEl = document.querySelector("#view-main");
	viewSettingsEl = document.querySelector("#view-settings");
	trendSectionEl = document.querySelector("#trend-section");

	meterMounts.session = { mount: document.querySelector("#session-meter"), head: document.querySelector("#session-head"), panel: document.querySelector("#panel-session") };
	meterMounts.week_all = { mount: document.querySelector("#week-meter"), head: document.querySelector("#week-head"), panel: document.querySelector("#panel-week") };

	panelEls = Array.from(document.querySelectorAll(".panels > .panel"));

	wireTitlebar();
	wireContextMenu();
	wireSettings();
	initSettings();

	refreshEl.addEventListener("click", refresh);
	listen("usage-updated", (event) => {
		applyUsage(event.payload);
	});
	listen("open-settings", () => {
		showSettings();
	});
	// macOS のアプリメニューの「更新」から届く合図。手動更新ボタンと同じ経路で利用枠を取り直す。
	listen("trigger-refresh", () => {
		refresh();
	});
	// メインウィンドウのキーボードショートカット。修飾キー(Windows は Ctrl、macOS は Cmd)ちょうどの押下に限り、Alt/Shift 併用や押しっぱなしの連続発火は弾く。更新(Ctrl+R)と設定(Ctrl+,)は Windows で受け、macOS は同じ動作をネイティブのアプリメニュー(Cmd+R / Cmd+,)が担うため二重発火を避けてここでは扱わない。閉じる(Ctrl+W / Cmd+W)は両プラットフォームで受ける。
	window.addEventListener("keydown", (event) => {
		const mod = (event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey;
		if (!mod || event.repeat) {
			return;
		}
		if (event.code === "KeyW") {
			event.preventDefault();
			// タイトルバーの閉じるボタンと同じ win_close を呼ぶ。Rust 側が CloseRequested を横取りしてトレイへ隠すため、アプリは終了せず計測を続ける。
			invoke("win_close");
		} else if (!isMac && event.code === "KeyR") {
			// 手動更新ボタンと同じく利用枠を取り直す。WebView 既定の再読み込みは止める。
			event.preventDefault();
			refresh();
		} else if (!isMac && event.code === "Comma") {
			// 設定ビューを開く。
			event.preventDefault();
			showSettings();
		}
	});
	// ウィンドウが前面に戻るたびにアクセント色を読み直し、起動後に OS のアクセントを変えても追従させる。
	window.addEventListener("focus", () => {
		applyAccentColor();
		// 別 DPI のモニタへ移った場合に備え、再フォーカス時はゲージを実画素から描き直す。
		lastTitlebarGaugeKey = null;
		updateTitlebarGauge();
	});
	// グレイスケール配色は解決済みテーマで濃淡が反転するため、明暗が切り替わったらヒートマップを描き直す。テーマ切替(set_theme)も OS の自動切替も prefers-color-scheme の変化として届く。
	if (window.matchMedia) {
		window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
			render();
			updateHeatCombo();
		});
	}
	// チャートのマウント寸法の変化に追随してバーンダウンを描き直し、viewBox の寸法を新しい横幅・高さへ合わせる。ResizeObserver は窓のリサイズに限らず、消費傾向の表示切り替えや段組みの変化などレイアウト由来の寸法変化も拾う。連続する通知は次の描画フレームまで畳み、観測ループの警告も避ける。ヒートマップは CSS グリッドが寸法へ自動追従するため対象外。
	let chartResizePending = false;
	const chartResizeObserver = new ResizeObserver(() => {
		if (chartResizePending) {
			return;
		}
		chartResizePending = true;
		requestAnimationFrame(() => {
			chartResizePending = false;
			onChartBoxResize();
		});
	});
	chartResizeObserver.observe(sessionChartEl);
	chartResizeObserver.observe(weekChartEl);
	setInterval(() => {
		renderMeters();
		renderStatus();
	}, 1000);
	refresh();
});
