// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Romly

import { translate, buildDict } from "./i18n.js";

const SVGNS = "http://www.w3.org/2000/svg";

// UI 表示言語の辞書。setLocale で受け取り、判定バッジ・ヒートマップの文言をこの言語で出す。setLocale が呼ばれる前の既定言語として日本語を入れておく。
let i18nDict = buildDict("ja");

// 表示言語の辞書を差し替える。main 側の言語切り替えに合わせて呼ぶ。
export function setLocale(d) {
	i18nDict = d || {};
}

// 辞書からキーの文言を引き、{name} 形式のプレースホルダを vars で差し替える。
function t(key, vars) {
	return translate(i18nDict, key, vars);
}

// バーンダウンの viewBox 幅の代替値。実描画では mount の実測幅をそのまま viewBox 幅に使うため、窓が広いほど同じ期間を横へ広く描く。この定数は mount が非表示などで実測幅を取れないときにだけ使う。
const VBW = 520;

// バーンダウンの viewBox 高さの代替値。実描画では mount の実測高さをそのまま viewBox 高さに使うため、窓が高いほど作画域を縦へ広げる。この定数は mount が非表示などで実測高さを取れないときにだけ使う。
const VBH = 240;

const MONTHS = { Jan: 0, Feb: 1, Mar: 2, Apr: 3, May: 4, Jun: 5, Jul: 6, Aug: 7, Sep: 8, Oct: 9, Nov: 10, Dec: 11 };

const MONTH_NAMES = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

// 直近率を見る窓(経過率)。この区間の区間傾きの中央値を直近の消費ペースとみなす。
const RECENT_FRAC = 0.25;

// 投影率の下限割合。直近率が落ちても累積平均ペースのこの割合は下回らせない。短い休止で投影が水平化するのを防ぐ。
const FLOOR_FRAC = 0.5;

// プロファイル投影を有効にする窓長。これ以上長い窓では曜日×時間帯の癖で残り時間を積分する。
const PROFILE_MIN_WINDOW_MS = 24 * 3600 * 1000;

// プロファイル投影に必要な最小サンプル数。これ未満なら癖が定まらないとみなし線形投影へ退く。
const PROFILE_MIN_SAMPLES = 300;

// プロファイル積分量の有効下限(%)。これ以下は実質ゼロとして線形投影へ退く。
const PROFILE_EPS = 0.05;

// 今日係数(平常比の消費の濃さ)のクランプ範囲。少ない実績からの過大・過小な伸縮を抑える。
const FACTOR_MIN = 0.25;

const FACTOR_MAX = 4;

// 投影帯(コーン)の今日係数の上下振り幅。中央の今日係数をこの割合だけ広げ、上下のプロファイル曲線で帯を描く。
const PROFILE_BAND = 0.35;

// 各チャートの横方向の表示範囲 [t0, t1] を保持する。再描画やポーリングを跨いでズーム状態を維持するため、ここに残す。
const VIEWS = new Map();

// SVG 要素を1つ作って属性を付け、親があれば追加する。
function el(tag, attrs, parent) {
	const node = document.createElementNS(SVGNS, tag);
	for (const k in attrs) {
		node.setAttribute(k, attrs[k]);
	}
	if (parent) {
		parent.appendChild(node);
	}
	return node;
}










// SVG のテキスト要素を作って文字を入れる。
function txt(parent, x, y, s, attrs) {
	const node = el("text", Object.assign({ x, y }, attrs || {}), parent);
	node.textContent = s;
	return node;
}










// SVG text のおおよその表示幅(ユニット)を見積もる。CJK 帯は全角幅、その他は半角幅として概算する。描画中の SVG は未装着で getComputedTextLength が使えないため実測はしない。
function estimateTextWidth(s, fontSize) {
	let w = 0;
	for (const ch of s) {
		const wide = ch.charCodeAt(0) > 0x2e7f;
		w += fontSize * (wide ? 1 : 0.55);
	}
	return w;
}










// 点(px,py)の近くにラベルを置く。既定は右下で、SVG の箱(vbw)や床(bottomLimit)を越える側へは反転する。occupied に積まれた既存ラベルの矩形と重なるときは、配置候補を順に試し、箱に収まり衝突しない最初の場所を選ぶ。候補は下方向(基準→押し下げ)を左右ぶん先に並べ、最後に上方向を置く。重なったラベルは下へ逃がしたいので下方向を上方向より優先する。どれも収まらなければ既定位置へ置く。選んだ矩形は occupied へ追加し、後続ラベルの回避対象にする。作画域(プロット矩形)からのはみ出しは許し、SVG の箱とプロット床だけを境界にする。
function placeLabel(svg, px, py, s, bottomLimit, attrs, occupied, vbw) {
	const fontSize = attrs["font-size"] || 11;
	const w = estimateTextWidth(s, fontSize);
	const gapX = 6;
	const preferRight = px + gapX + w <= vbw;
	const preferDown = py + fontSize + 3 <= bottomLimit;
	const hOrder = preferRight ? ["start", "end"] : ["end", "start"];
	// 横アンカー(h)・縦向き(v)・下方向の押し下げ量(nudge)から、ラベルの占有矩形と描画位置を作る。
	const rectFor = (h, v, nudge) => {
		const tx = h === "start" ? px + gapX : px - gapX;
		const x0 = h === "start" ? tx : tx - w;
		const ty = (v === "down" ? py + fontSize + 3 : py - 5) + nudge;
		return { x0, x1: x0 + w, top: ty - fontSize, bottom: ty + 2, tx, ty, anchor: h };
	};
	const inBox = (r) => r.x0 >= 0 && r.x1 <= vbw && r.top >= 0 && r.bottom <= bottomLimit;
	const hits = (r) => (occupied || []).some((o) => r.x0 < o.x1 + 2 && r.x1 > o.x0 - 2 && r.top < o.bottom + 2 && r.bottom > o.top - 2);
	const candidates = [];
	for (const nudge of [0, fontSize + 3, 2 * (fontSize + 3)]) {
		for (const h of hOrder) {
			candidates.push(rectFor(h, "down", nudge));
		}
	}
	for (const h of hOrder) {
		candidates.push(rectFor(h, "up", 0));
	}
	let chosen = candidates.find((c) => inBox(c) && !hits(c));
	if (!chosen) {
		chosen = rectFor(preferRight ? "start" : "end", preferDown ? "down" : "up", 0);
	}
	txt(svg, chosen.tx, chosen.ty, s, Object.assign({}, attrs, { "text-anchor": chosen.anchor }));
	if (occupied) {
		occupied.push(chosen);
	}
}










// チャート id に対応する表示範囲を返す。未登録なら横全体 [0,1]・縦全体 [0,100]・自動枠取り有効(manual:false)で作る。
function getView(id) {
	if (!VIEWS.has(id)) {
		VIEWS.set(id, { t0: 0, t1: 1, v0: 0, v1: 100, manual: false });
	}
	return VIEWS.get(id);
}










// "Jun 23, 4:10am (Asia/Tokyo)" や "Jul 2 at 5:29am (Asia/Tokyo)" 形式のリセット文字列を絶対時刻へ変換する。日と時刻の区切りはカンマと " at " のどちらも受ける。タイムゾーン表記は表示ローカルと同じため無視する。
export function parseResetTime(resetStr, now) {
	const m = resetStr && resetStr.match(/([A-Za-z]{3})\s+(\d{1,2})(?:,|\s+at)\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)/i);
	if (!m) {
		return null;
	}
	const mon = m[1].charAt(0).toUpperCase() + m[1].slice(1, 3).toLowerCase();
	const month = MONTHS[mon];
	if (month === undefined) {
		return null;
	}
	let hour = parseInt(m[3], 10);
	const min = m[4] ? parseInt(m[4], 10) : 0;
	const ap = m[5].toLowerCase();
	if (ap === "pm" && hour !== 12) {
		hour += 12;
	}
	if (ap === "am" && hour === 12) {
		hour = 0;
	}
	let when = new Date(now.getFullYear(), month, parseInt(m[2], 10), hour, min, 0, 0);
	// リセットは未来のはず。大きく過去になったら年境界とみなして翌年へ送る。
	if (when.getTime() < now.getTime() - 24 * 3600 * 1000) {
		when = new Date(now.getFullYear() + 1, month, parseInt(m[2], 10), hour, min, 0, 0);
	}
	return when;
}










// 日付・時刻の表示形式。"intl" は英語の月名と午前午後("Jun 27, 2:40pm")、"jp" は数字主体の日本式("6/27 14:40")。
let dateFormat = "intl";

// チャート上のラベル語の日英対。表示言語(dateFormat)に合わせて localLabel で引く。
const LABEL_WORDS = {
	now: { jp: "現在", intl: "now" },
	depleted: { jp: "枯渇", intl: "depleted" },
};

// 日付・時刻の表示形式を切り替える。以後の整形・描画がこの設定に従う。
export function setDateFormat(mode) {
	dateFormat = mode === "jp" ? "jp" : "intl";
}










// ラベル語を表示言語に合わせて返す。日本式は日本語、海外式は英語。
function localLabel(key) {
	const w = LABEL_WORDS[key];
	return dateFormat === "jp" ? w.jp : w.intl;
}










// Date を時刻文字列にする。日本式は24時間制("14:40")、海外式は午前午後("2:40pm")。withSeconds を真にすると秒まで添える。
function formatClock(date, withSeconds) {
	const m = String(date.getMinutes()).padStart(2, "0");
	const s = withSeconds ? ":" + String(date.getSeconds()).padStart(2, "0") : "";
	if (dateFormat === "jp") {
		return date.getHours() + ":" + m + s;
	}
	let h = date.getHours() % 12;
	const ap = date.getHours() < 12 ? "am" : "pm";
	if (h === 0) {
		h = 12;
	}
	return h + ":" + m + s + ap;
}










// Date を短い日付文字列にする。日本式は "6/27"、海外式は "Jun 23"。
function formatShortDate(date) {
	if (dateFormat === "jp") {
		return (date.getMonth() + 1) + "/" + date.getDate();
	}
	return MONTH_NAMES[date.getMonth()] + " " + date.getDate();
}










// Date を日付+時刻の文字列にする。日本式は "6/27 14:40"、海外式は "Jun 23, 2:40pm"。
export function formatDateTime(date) {
	if (dateFormat === "jp") {
		return formatShortDate(date) + " " + formatClock(date);
	}
	return formatShortDate(date) + ", " + formatClock(date);
}










// SVG にホイール拡大・ドラッグ移動・ダブルクリック復帰の操作を付ける。素のホイールは横(経過率)、Ctrl+ホイールは縦(使用%)をカーソル位置中心に拡縮する。ドラッグは縦横とも移動する。いずれかを操作したら手動とみなして view.manual を立て、以後の再描画で自動枠取りを止める。ダブルクリックで manual を解き、データ由来の既定枠へ戻す。view を直接書き換えて redraw を呼ぶ。
function attachZoom(svg, mount, view, redraw, padL, plotW, padT, plotH, vbw, vbh) {
	// クライアント座標 X を、現在の表示範囲における経過率 t へ変換する。
	const tAt = (clientX) => {
		const rect = mount.getBoundingClientRect();
		const vbX = ((clientX - rect.left) / rect.width) * vbw;
		return view.t0 + ((vbX - padL) / plotW) * (view.t1 - view.t0);
	};
	// クライアント座標 Y を、現在の表示範囲における使用% v へ変換する。画面の下ほど v が小さい。
	const vAt = (clientY) => {
		const rect = mount.getBoundingClientRect();
		const vbY = ((clientY - rect.top) / rect.height) * vbh;
		return view.v1 - ((vbY - padT) / plotH) * (view.v1 - view.v0);
	};

	svg.addEventListener(
		"wheel",
		(e) => {
			e.preventDefault();
			view.manual = true;
			const factor = e.deltaY < 0 ? 0.82 : 1 / 0.82;
			if (e.ctrlKey) {
				const span = view.v1 - view.v0;
				const focus = Math.max(view.v0, Math.min(view.v1, vAt(e.clientY)));
				const newSpan = Math.max(2, Math.min(100, span * factor));
				let v0 = focus - ((focus - view.v0) / span) * newSpan;
				let v1 = v0 + newSpan;
				if (v0 < 0) {
					v1 -= v0;
					v0 = 0;
				}
				if (v1 > 100) {
					v0 -= v1 - 100;
					v1 = 100;
				}
				view.v0 = Math.max(0, v0);
				view.v1 = Math.min(100, v1);
			} else {
				const span = view.t1 - view.t0;
				const focus = Math.max(view.t0, Math.min(view.t1, tAt(e.clientX)));
				const newSpan = Math.max(0.01, Math.min(1, span * factor));
				let t0 = focus - ((focus - view.t0) / span) * newSpan;
				let t1 = t0 + newSpan;
				if (t0 < 0) {
					t1 -= t0;
					t0 = 0;
				}
				if (t1 > 1) {
					t0 -= t1 - 1;
					t1 = 1;
				}
				view.t0 = Math.max(0, t0);
				view.t1 = Math.min(1, t1);
			}
			redraw();
		},
		{ passive: false }
	);

	svg.addEventListener("pointerdown", (e) => {
		const startX = e.clientX;
		const startY = e.clientY;
		const startT0 = view.t0;
		const startV0 = view.v0;
		const spanT = view.t1 - view.t0;
		const spanV = view.v1 - view.v0;
		svg.style.cursor = "grabbing";
		// ドラッグ中の move/up は window に付ける。redraw で svg が差し替わっても追従するため。
		const onMove = (ev) => {
			view.manual = true;
			const rect = mount.getBoundingClientRect();
			const movedVbX = ((ev.clientX - startX) / rect.width) * vbw;
			let t0 = startT0 - (movedVbX / plotW) * spanT;
			t0 = Math.max(0, Math.min(1 - spanT, t0));
			view.t0 = t0;
			view.t1 = t0 + spanT;
			// 画面の下ほど v が小さいので、下へドラッグするほど高い使用%側を覗く向きへ v0 を増やす。
			const movedVbY = ((ev.clientY - startY) / rect.height) * vbh;
			let v0 = startV0 + (movedVbY / plotH) * spanV;
			v0 = Math.max(0, Math.min(100 - spanV, v0));
			view.v0 = v0;
			view.v1 = v0 + spanV;
			redraw();
		};
		const onUp = () => {
			window.removeEventListener("pointermove", onMove);
			window.removeEventListener("pointerup", onUp);
		};
		window.addEventListener("pointermove", onMove);
		window.addEventListener("pointerup", onUp);
	});

	svg.addEventListener("dblclick", () => {
		view.manual = false;
		redraw();
	});
}










// 履歴から曜日×時間帯(7×24)の平均消費プロファイルを作る。aggregateHeat の集計を1区間あたりの平均 Δ% に均し、記録のない時間帯は0として返す。前方投影で「その時間帯にどれだけ使う癖か」の重みに使う。
function buildProfile(history, key) {
	const { sum, count, total } = aggregateHeat(history, key);
	const w = Array.from({ length: 7 }, () => new Array(24).fill(0));
	for (let d = 0; d < 7; d++) {
		for (let h = 0; h < 24; h++) {
			if (count[d][h] > 0) {
				w[d][h] = sum[d][h] / count[d][h];
			}
		}
	}
	return { w, total };
}










// プロファイル w を fromMs〜toMs にわたって積む。各時間帯を正時で区切り、端の半端な時間は在籍時間の割合で按分する。記録のない時間帯は0。1区間あたり平均 Δ% を時間で重み付けした相対量を返す。ポーリング間隔が一定なら、この量どうしの比は標本密度に依らない。
function integrateProfile(w, fromMs, toMs) {
	if (toMs <= fromMs) {
		return 0;
	}
	let acc = 0;
	let cur = fromMs;
	while (cur < toMs) {
		const at = new Date(cur);
		const day = at.getDay();
		const hour = at.getHours();
		const hourEnd = new Date(at.getFullYear(), at.getMonth(), at.getDate(), hour + 1, 0, 0, 0).getTime();
		const segEnd = Math.min(hourEnd, toMs);
		acc += w[day][hour] * ((segEnd - cur) / 3600000);
		cur = segEnd;
	}
	return acc;
}










// 直近 recentFrac(経過率)ぶんの区間傾きを昇順で集めて返す。端点差分でなく分布として扱うことで、中央値を率に、四分位を投影帯の上下端に使える。一時的な平坦区間(短い休止)に率が引きずられないようにするねらいは中央値による。
function recentSlopes(samples, f, recentFrac) {
	const cutoff = f - recentFrac;
	const slopes = [];
	for (let i = 1; i < samples.length; i++) {
		const a = samples[i - 1];
		const b = samples[i];
		if (b.t < cutoff) {
			continue;
		}
		const dt = b.t - a.t;
		if (dt > 0) {
			slopes.push((b.v - a.v) / dt);
		}
	}
	slopes.sort((x, y) => x - y);
	return slopes;
}










// 昇順済み配列の p 分位(0〜1)を線形補間で返す。空配列なら null。
function percentile(sorted, p) {
	if (sorted.length === 0) {
		return null;
	}
	const idx = (sorted.length - 1) * p;
	const lo = Math.floor(idx);
	const hi = Math.ceil(idx);
	if (lo === hi) {
		return sorted[lo];
	}
	return sorted[lo] + (sorted[hi] - sorted[lo]) * (idx - lo);
}










// プロファイルで now〜reset の消費曲線を作る。今日係数 factor で平常の癖を伸縮させ、時間刻みで使用%を積み上げる。描画用の点列(t,v)、リセット時点の投影使用%(projEnd)、100%到達時刻(hitT)を返す。
function profileCurve(w, factor, used, startMs, nowMs, resetMs, span) {
	const points = [{ t: (nowMs - startMs) / span, v: used }];
	const remain = resetMs - nowMs;
	const steps = Math.max(1, Math.min(96, Math.ceil(remain / 3600000)));
	const stepMs = remain / steps;
	let acc = 0;
	// 現在地で既に使い切っているなら枯渇は今(=現在の経過率)とみなす。下から100%を跨ぐ検出は prevV<100 が前提のため、used が100以上だとこのループでは捕まらない。
	let hitT = used >= 100 ? (nowMs - startMs) / span : Infinity;
	let prevV = used;
	let prevMs = nowMs;
	for (let i = 1; i <= steps; i++) {
		const nextMs = nowMs + stepMs * i;
		acc += integrateProfile(w, prevMs, nextMs) * factor;
		const v = used + acc;
		if (hitT === Infinity && prevV < 100 && v >= 100) {
			const frac = (100 - prevV) / (v - prevV);
			hitT = (prevMs + (nextMs - prevMs) * frac - startMs) / span;
		}
		points.push({ t: (nextMs - startMs) / span, v });
		prevV = v;
		prevMs = nextMs;
	}
	return { points, projEnd: used + acc, hitT };
}










// 投影帯の片側の辺を、apex(f,used)から指定の傾きで t=1 まで引く。100% を超える辺は到達点を頂点として挟んでから水平へ折る。far 端の値だけを 100% へ丸めると、辺の描画が実際の傾きより寝てしまうため。
function clampedEdge(f, used, rate) {
	const vAt1 = used + rate * (1 - f);
	if (vAt1 <= 100) {
		return [{ t: f, v: used }, { t: 1, v: vAt1 }];
	}
	const crossT = f + (100 - used) / rate;
	return [{ t: f, v: used }, { t: crossT, v: 100 }, { t: 1, v: 100 }];
}










// 前方投影の中核。短い窓は堅牢な直近率(区間傾きの中央値)を累積平均ペースの下限で支えた線形投影。長い窓でプロファイルが育っていれば、これまでの消費が平常比でどれだけ濃いかを今日係数とし、残り時間を曜日×時間帯の癖で積分した曲線へ切り替える。mode・投影使用%(projEnd)・100%到達(hitT)・警告(warn)・描画点列(points)を返す。投影に足る蓄積が無ければ mode:"none"。withBand のときは投影帯(コーン)の閉じた多角形点列を band に積む。線形は直近傾きの四分位、曲線は今日係数を上下に振った曲線を上下端にする。
function projectForward(o) {
	const { samples, f, used, startMs, span, nowMs, resetMs, minSpan, profile } = o;
	const recentFrac = o.recentFrac == null ? RECENT_FRAC : o.recentFrac;
	const floorFrac = o.floorFrac == null ? FLOOR_FRAC : o.floorFrac;
	const withBand = o.withBand === true;
	if (!(samples.length >= 3 && f < 1 && used != null)) {
		return { mode: "none" };
	}
	if (samples[samples.length - 1].t - samples[0].t < minSpan) {
		return { mode: "none" };
	}

	if (profile && profile.total >= PROFILE_MIN_SAMPLES && used > 0) {
		const expSoFar = integrateProfile(profile.w, startMs, nowMs);
		const expRemain = integrateProfile(profile.w, nowMs, resetMs);
		if (expSoFar > PROFILE_EPS && expRemain > PROFILE_EPS) {
			const factor = Math.max(FACTOR_MIN, Math.min(FACTOR_MAX, used / expSoFar));
			const c = profileCurve(profile.w, factor, used, startMs, nowMs, resetMs, span);
			let band = null;
			if (withBand) {
				// 帯は既にクランプ済みの中央係数を基準に上下へ振る。FACTOR_MIN/MAX で挟み直すと、係数がクランプ端に張り付いたとき帯の片側が中央線へ潰れ、中央線が縁へ乗るため、ここでは挟まない。
				const cHi = profileCurve(profile.w, factor * (1 + PROFILE_BAND), used, startMs, nowMs, resetMs, span);
				const cLo = profileCurve(profile.w, factor * (1 - PROFILE_BAND), used, startMs, nowMs, resetMs, span);
				band = cHi.points.concat(cLo.points.slice().reverse());
			}
			return { mode: "profile", projEnd: c.projEnd, hitT: c.hitT, warn: c.hitT < 1, points: c.points, band };
		}
	}

	const rAvg = f > 0 ? used / f : 0;
	const slopes = recentSlopes(samples, f, recentFrac);
	const rRecent = slopes.length ? percentile(slopes, 0.5) : null;
	const rate = Math.max(0, rRecent == null ? rAvg : Math.max(rRecent, floorFrac * rAvg));
	// used が100以上なら既に使い切りなので枯渇は今(=f)。それ未満なら残容量を率で割って到達時刻を見積もる。
	const hitT = used >= 100 ? f : (rate > 0 ? f + (100 - used) / rate : Infinity);
	const endT = Math.min(hitT, 1);
	const endV = used + rate * (endT - f);
	let band = null;
	if (withBand && slopes.length >= 4) {
		const rateLo = Math.max(0, percentile(slopes, 0.25));
		const rateHi = Math.max(rate, percentile(slopes, 0.75));
		const hiEdge = clampedEdge(f, used, rateHi);
		const loEdge = clampedEdge(f, used, rateLo);
		band = hiEdge.concat(loEdge.slice().reverse());
	}
	return { mode: "linear", rate, projEnd: used + rate * (1 - f), hitT, warn: hitT < 1, points: [{ t: f, v: used }, { t: endT, v: endV }], band };
}










// 軸ラベルの粒度を切り替える境目。目盛り1区間がこの時間以上を覆うときは日付、未満のときは時刻にする。1区間が1日より細かくなると日付ラベルが重複して用をなさなくなるため、時刻へ切り替える。
const AXIS_DATE_MIN_TICK_MS = 24 * 3600 * 1000;




// リセットまでの隔たり(分)を「◯日と◯時間」「◯時間◯分」「◯分」へ整形する。早期枯渇の判定文「◯前に早期枯渇予定」の冒頭に使う。表示を簡潔に保つため粒度は2段までに留める。
function formatGap(min) {
	const d = Math.floor(min / 1440);
	const h = Math.floor((min % 1440) / 60);
	const m = min % 60;
	if (d > 0) {
		return t("duration.dayHour", { d, h });
	}
	if (h > 0) {
		return t("duration.hourMin", { h, m });
	}
	return t("duration.min", { m });
}










// 表示中の縦範囲 [lo,hi] に対し、1/2/2.5/5 系の見やすい刻みで目盛り値を作る。全体表示([0,100] を 4 分割)では 0/25/50/75/100 と一致する。
function niceTicks(lo, hi, target) {
	const span = hi - lo;
	if (span <= 0) {
		return [lo];
	}
	const raw = span / target;
	const pow = Math.pow(10, Math.floor(Math.log10(raw)));
	const step = [1, 2, 2.5, 5, 10].map((m) => m * pow).find((c) => c >= raw - 1e-9) || 10 * pow;
	const first = Math.ceil(lo / step - 1e-9) * step;
	const ticks = [];
	for (let v = first; v <= hi + 1e-9; v += step) {
		ticks.push(Math.round(v * 1000) / 1000);
	}
	return ticks;
}










// 自動枠取りを起動する経過率の下限。これを越えたら後半とみなし、左下の空きを切って右上の要所を拡大する。
const AUTOFRAME_NOW_MIN = 0.55;

// 自動枠取りの縦下端に残す余白(%)。枠内の最小値が切り口へ張り付かないよう、その分だけ下へ空ける。
const AUTOFRAME_V_PAD = 4;

// 自動枠取りで確保する横の最小表示幅(経過率)。寄せすぎて一点へ潰れるのを防ぐ。
const AUTOFRAME_MIN_TSPAN = 0.1;

// 自動枠取りで確保する縦の最小表示幅(%)。寄せすぎて一点へ潰れるのを防ぐ。
const AUTOFRAME_MIN_VSPAN = 12;

// 手動操作前の既定の表示範囲をデータから決める。経過率が後半に達していれば、now を横の中央へ置いて左に同じだけの実績・右に投影が載るよう横範囲を取り、枠内に入る実績・投影・帯の最小値より少し下を縦の下端、上端は常に 100% にして、右上の要所を大きく見せる。後半に達していなければ縦横とも全体表示にする。
function autoFrame(view, s, proj, nowT, lastV) {
	if (nowT < AUTOFRAME_NOW_MIN) {
		view.t0 = 0;
		view.t1 = 1;
		view.v0 = 0;
		view.v1 = 100;
		return;
	}
	const t0 = Math.min(1 - AUTOFRAME_MIN_TSPAN, Math.max(0, 2 * nowT - 1));
	let vMin = lastV;
	const consider = (p) => {
		if (p.t >= t0 - 1e-9 && p.v < vMin) {
			vMin = p.v;
		}
	};
	for (const p of s) {
		consider(p);
	}
	if (proj.points) {
		for (const p of proj.points) {
			consider(p);
		}
	}
	if (proj.band) {
		for (const p of proj.band) {
			consider(p);
		}
	}
	view.t0 = t0;
	view.t1 = 1;
	view.v0 = Math.min(100 - AUTOFRAME_MIN_VSPAN, Math.max(0, vMin - AUTOFRAME_V_PAD));
	view.v1 = 100;
}










// 実績線(実測点を繋ぐ折れ線)の太さ。
const ACTUAL_LINE_WIDTH = 0.8;

// 実測点の円の半径。
const MARKER_RADIUS = 2.0;

// 実測点の円を打つ最小の画面間隔(px)。可視範囲でこれより密に点が並ぶ区間では円を間引いて打ち、円が重なって瘤になるのを防ぐ。拡大して点の間隔が開けば打つ点が増え、十分寄れば全点に円が付く。線(polyline)は常に全点フル解像度で描き、間引くのは円だけにする。
const MARKER_MIN_GAP = 8;




// バーンダウンを SVG で描く。理想対角線・実績線・実測点・リセット壁・投影・now を重ね、判定(warn/ok/idle)を返す。表示範囲は view に従う。
function drawBurndown(draw, view, redraw) {
	const padL = 6;
	const padR = 6;
	const padT = 18;
	const padB = 32;
	// viewBox の寸法を mount の実測寸法へ合わせ、窓が広い・高いほど作画域を広げる。padL/padR/padT/padB は定数のまま据え置くので、作画域だけが伸び縮みし軸ラベルの余白は一定に保たれる。非表示などで実測できないときは代替値 VBW/VBH を使う。
	const box = draw.mount.getBoundingClientRect();
	const vbw = box.width >= 1 ? Math.round(box.width) : VBW;
	const vbh = box.height >= 1 ? Math.round(box.height) : VBH;
	const plotW = vbw - padL - padR;
	const plotH = vbh - padT - padB;

	const s = draw.samples;

	// 投影は自動枠取りより先に求める。枠取りで投影点・帯の縦範囲まで取り込むため、座標変換 x()/y() を確定する前に計算する。
	let proj = { mode: "none" };
	let nowT = null;
	let lastV = null;
	if (s.length > 0) {
		const last = s[s.length - 1];
		nowT = last.t;
		lastV = last.v;
		proj = projectForward({
			samples: s,
			f: nowT,
			used: lastV,
			startMs: draw.startMs,
			span: draw.span,
			nowMs: draw.startMs + nowT * draw.span,
			resetMs: draw.startMs + draw.span,
			minSpan: draw.minSpan,
			profile: draw.profile,
			withBand: true,
		});
	}

	// 手動操作がまだなら、データから既定の表示範囲を決める。手動でズーム・移動した後はその範囲を保つ。
	if (!view.manual) {
		if (s.length >= 2 && nowT != null) {
			autoFrame(view, s, proj, nowT, lastV);
		} else {
			view.t0 = 0;
			view.t1 = 1;
			view.v0 = 0;
			view.v1 = 100;
		}
	}

	const vspan = view.t1 - view.t0;
	const vspanV = view.v1 - view.v0;
	const x = (t) => padL + ((t - view.t0) / vspan) * plotW;
	const y = (v) => padT + (1 - (v - view.v0) / vspanV) * plotH;
	const visible = (t) => t >= view.t0 - 1e-9 && t <= view.t1 + 1e-9;
	const visibleV = (v) => v >= view.v0 - 1e-9 && v <= view.v1 + 1e-9;

	const svg = el("svg", { viewBox: `0 0 ${vbw} ${vbh}`, width: "100%", height: "100%", role: "img" });

	const clipId = "clip-" + draw.id;
	const defs = el("defs", {}, svg);
	const clip = el("clipPath", { id: clipId }, defs);
	el("rect", { x: padL, y: padT, width: plotW, height: plotH }, clip);

	for (const v of niceTicks(view.v0, view.v1, 4)) {
		if (!visibleV(v)) {
			continue;
		}
		el("line", { x1: padL, y1: y(v), x2: vbw - padR, y2: y(v), stroke: "var(--chart-grid)", "stroke-width": 1 }, svg);
		// ラベルはプロット内部の左端へ寄せ、対応する目盛線のすぐ下へ置く。最下端の目盛りは横軸の日付ラベルと重なるため描かない。
		if (Math.abs(v - view.v0) > 1e-6) {
			txt(svg, padL + 4, y(v) + 11, v + "%", { fill: "var(--chart-faint)", "font-size": 10, "text-anchor": "start" });
		}
	}

	// 軸ラベルの粒度を表示中の幅で決める。目盛り1区間が1日以上を覆うときは日付、それより細かいときは時刻にする。時刻表示では左端と、日付が直前の目盛りから変わる位置にだけ日付を添えて、今どの日を見ているか分かるようにする。
	const dateMode = (vspan * draw.span) / 4 >= AXIS_DATE_MIN_TICK_MS;
	let prevDayKey = null;
	for (let i = 0; i <= 4; i++) {
		const t = view.t0 + (i / 4) * vspan;
		const at = new Date(draw.startMs + t * draw.span);
		let label;
		if (dateMode) {
			label = formatShortDate(at);
		} else {
			const dayKey = at.getFullYear() * 10000 + at.getMonth() * 100 + at.getDate();
			label = i === 0 || dayKey !== prevDayKey ? formatShortDate(at) + " " + formatClock(at) : formatClock(at);
			prevDayKey = dayKey;
		}
		txt(svg, padL + (i / 4) * plotW, vbh - padB + 15, label, { fill: "var(--chart-faint)", "font-size": 9.5, "text-anchor": i === 0 ? "start" : i === 4 ? "end" : "middle" });
	}

	// 枠外へ出る実績・投影・対角線・壁を切り取るためのグループ
	const g = el("g", { "clip-path": `url(#${clipId})` }, svg);

	el("line", { x1: x(0), y1: y(0), x2: x(1), y2: y(100), stroke: "var(--chart-faint)", "stroke-width": 1.4, "stroke-dasharray": "3 4" }, g);
	el("line", { x1: x(1), y1: padT, x2: x(1), y2: vbh - padB, stroke: "var(--chart-wall)", "stroke-width": 2 }, g);

	let result = { state: "idle", label: "" };
	// このチャートで既に置いたラベルの占有矩形。後続ラベルの重なり回避に使う。
	const placed = [];

	if (s.length > 0) {
		if (s.length >= 2) {
			const pts = s.map((p) => x(p.t) + "," + y(p.v)).join(" ");
			el("polygon", { points: x(s[0].t) + "," + y(0) + " " + pts + " " + x(nowT) + "," + y(0), fill: "var(--chart-area)" }, g);
			el("polyline", { points: pts, fill: "none", stroke: "var(--chart-line)", "stroke-width": ACTUAL_LINE_WIDTH, "stroke-linejoin": "round", "stroke-linecap": "round" }, g);
		}

		// 円は画面上で MARKER_MIN_GAP px 以上離れた点にだけ打つ。可視点を左から順に走査し、直前に打った円との横距離が足りなければ飛ばす。画面外の点は打たないので、円の数は最大でもプロット幅/間隔ほどに収まる。
		let lastDotX = -Infinity;
		for (const p of s) {
			if (!visible(p.t)) {
				continue;
			}
			const px = x(p.t);
			if (px - lastDotX < MARKER_MIN_GAP) {
				continue;
			}
			el("circle", { cx: px, cy: y(p.v), r: MARKER_RADIUS, fill: "var(--chart-line)" }, g);
			lastDotX = px;
		}

		result = { state: "idle", label: t("verdict.waitingProjection") };
		if (proj.mode !== "none") {
			const color = proj.warn ? "var(--chart-warn)" : "var(--chart-ok)";
			if (proj.band) {
				const band = proj.band.map((p) => x(p.t) + "," + y(p.v)).join(" ");
				el("polygon", { points: band, fill: color, "fill-opacity": 0.13, stroke: "none" }, g);
			}
			const pts = proj.points.map((p) => x(p.t) + "," + y(p.v)).join(" ");
			el("polyline", { points: pts, fill: "none", stroke: color, "stroke-width": 2.2, "stroke-dasharray": "5 4", "stroke-linecap": "round", "stroke-linejoin": "round" }, g);
			if (proj.warn) {
				if (lastV >= 100) {
					// 現在地で既に使い切っている。枯渇マーカーは「現在」マーカーと同じ座標に重なるだけなので置かず、結果ラベルだけ超過量を伝える。超過が丸めて0なら「枠を使い切り」に落とす。
					const over = Math.round(proj.projEnd - 100);
					result = { state: "warn", label: over > 0 ? t("verdict.willExceed", { pct: over }) : t("verdict.windowUsedUp") };
				} else {
					el("circle", { cx: x(proj.hitT), cy: y(100), r: 4.5, fill: "var(--chart-warn)" }, g);
					if (visible(proj.hitT) && visibleV(100)) {
						placeLabel(svg, x(proj.hitT), y(100), localLabel("depleted") + " " + draw.fmt(proj.hitT), vbh - padB, { fill: "var(--chart-warn)", "font-size": 10.5, "font-weight": 600 }, placed, vbw);
					}
					const gap = Math.round((1 - proj.hitT) * draw.windowMin);
					result = { state: "warn", label: t("verdict.earlyDepletion", { gap: formatGap(gap) }) };
				}
			} else {
				const endV = proj.projEnd;
				el("line", { x1: x(1) - 7, y1: y(endV), x2: x(1), y2: y(endV), stroke: "var(--chart-ok)", "stroke-width": 2 }, g);
				el("line", { x1: x(1) - 7, y1: y(100), x2: x(1), y2: y(100), stroke: "var(--chart-ok)", "stroke-width": 2 }, g);
				el("line", { x1: x(1) - 3.5, y1: y(endV), x2: x(1) - 3.5, y2: y(100), stroke: "var(--chart-ok)", "stroke-width": 2 }, g);
				el("circle", { cx: x(1), cy: y(endV), r: 4, fill: "var(--chart-ok)" }, g);
				result = { state: "ok", label: t("verdict.willHaveLeft", { pct: Math.round(100 - endV) }) };
			}
		}

		el("line", { x1: x(nowT), y1: padT, x2: x(nowT), y2: vbh - padB, stroke: "var(--chart-now)", "stroke-width": 1, "stroke-dasharray": "2 4", "stroke-opacity": ".45" }, g);
		el("circle", { cx: x(nowT), cy: y(lastV), r: 4, fill: "var(--chart-now)" }, g);
		if (visible(nowT) && visibleV(lastV)) {
			placeLabel(svg, x(nowT), y(lastV), localLabel("now") + " " + lastV + "%", vbh - padB, { fill: "var(--chart-now)", "font-size": 11, "font-weight": 600 }, placed, vbw);
		}
	} else {
		txt(svg, padL + plotW / 2, padT + plotH / 2, t("chart.waitingSamples"), { fill: "var(--chart-faint)", "font-size": 12, "text-anchor": "middle" });
	}

	if (visible(1)) {
		txt(svg, x(1) - 6, vbh - padB - 5, "reset " + draw.resetLabel, { fill: "var(--chart-sub)", "font-size": 10, "text-anchor": "end" });
	}

	attachZoom(svg, draw.mount, view, redraw, padL, plotW, padT, plotH, vbw, vbh);
	draw.mount.replaceChildren(svg);
	return result;
}










// 1つの利用枠の投影付きバーンダウンを描く。履歴から現在の枠に入るサンプルを抽出し、経過率と使用%へ変換する。横方向ズームに対応する。
export function renderUsageChart(mount, cfg) {
	const reset = parseResetTime(cfg.resetStr, cfg.now);
	if (!reset) {
		mount.replaceChildren();
		return { state: "idle", label: t("verdict.noResetTime") };
	}
	const resetMs = reset.getTime();
	const startMs = resetMs - cfg.windowMs;
	const span = resetMs - startMs;

	const samples = cfg.history
		.filter((s) => s[cfg.key] && s.ts >= startMs && s.ts <= resetMs)
		.map((s) => ({ t: (s.ts - startMs) / span, v: s[cfg.key].used_pct }));

	const fmt = (t) => {
		const at = new Date(startMs + t * span);
		return cfg.timeLabel === "date" ? formatDateTime(at) : formatClock(at);
	};

	const profile = cfg.windowMs >= PROFILE_MIN_WINDOW_MS ? buildProfile(cfg.history, cfg.key) : null;

	const draw = {
		mount,
		id: cfg.key,
		samples,
		fmt,
		resetLabel: cfg.timeLabel === "date" ? formatShortDate(reset) : formatClock(reset),
		windowMin: cfg.windowMs / 60000,
		minSpan: 0.08,
		startMs,
		span,
		profile,
	};
	const view = getView(cfg.key);
	const redraw = () => drawBurndown(draw, view, redraw);
	return drawBurndown(draw, view, redraw);
}










// 1つの利用枠のペース指標を計算する。f(経過率)・used(使用%)・P(ペース比)・slope(線形投影時の前方消費率%/経過率、曲線投影時は null)・headroom(余裕係数)・projEnd(リセット時点の投影使用%)を返す。
export function paceMetrics(history, key, windowMs, resetStr, now, used) {
	const reset = parseResetTime(resetStr, now);
	if (!reset) {
		return { idle: true };
	}
	const resetMs = reset.getTime();
	const startMs = resetMs - windowMs;
	const span = resetMs - startMs;
	const f = Math.max(0, Math.min(1, (now.getTime() - startMs) / span));

	const samples = history
		.filter((s) => s[key] && s.ts >= startMs && s.ts <= resetMs)
		.map((s) => ({ t: (s.ts - startMs) / span, v: s[key].used_pct }));

	const profile = windowMs >= PROFILE_MIN_WINDOW_MS ? buildProfile(history, key) : null;
	const proj = projectForward({
		samples,
		f,
		used,
		startMs,
		span,
		nowMs: now.getTime(),
		resetMs,
		minSpan: 0.08,
		profile,
	});

	const P = used != null && f > 0 ? used / 100 / f : null;
	// 線形投影のときだけ直近の傾きを返す。曲線(プロファイル)投影では単一の傾きで表せないため null。
	const slope = proj.mode === "linear" ? proj.rate : null;
	// リセット時点の投影使用%。投影が立てば projectForward の結果を、立たなければ平均ペースで伸ばす。見込みピルの表示と余裕係数の算出に使う。
	let projEnd = null;
	if (proj.mode !== "none") {
		projEnd = proj.projEnd;
	} else if (used != null && f > 0) {
		projEnd = used / f;
	}
	// 余裕係数。残り容量(100-used)を、リセットまでに見込む追加消費(projEnd-used)で割る。見込み消費が無ければ無限大。投影が立ったときだけ出し、蓄積待ちの段階では null にしてピルを P ベースの表示に委ねる。
	let headroom = null;
	if (used != null && f < 1 && proj.mode !== "none") {
		headroom = proj.projEnd > used ? (100 - used) / (proj.projEnd - used) : Infinity;
	}
	return { idle: false, f, used, P, slope, headroom, projEnd };
}










// 日曜始まりの曜日ラベルを現在の表示言語で返す。getDay() が日曜=0 なので、その値をそのまま添字に使う。辞書は日曜から土曜までをカンマ区切りで持つ。
function dayLabels() {
	return t("heat.days").split(",");
}

// 連続サンプルの間隔がこれを超えたら、アプリ休止などによる飛びとみなし集計から外す。1サンプルの Δ% を1つの時間帯へ帰属させる前提が崩れるため。
const HEAT_MAX_GAP_MS = 30 * 60 * 1000;

// 消費強度のカラーランプ群。名前から RGB stop 列を引く。UI のテーマ追従色ではなくデータの強度を表す色なので、standard・parula・turbo は Light/Dark に依らず固定する。standard は teal→amber→red、parula と turbo は MATLAB のカラーマップを少数のアンカーで近似したもの。グレイスケールは resolveHeatStops でテーマに応じて作るためここには持たない。
const HEAT_PALETTES = {
	standard: [[31, 58, 74], [47, 110, 106], [202, 166, 74], [194, 90, 58], [255, 90, 90]],
	parula: [[53, 42, 135], [18, 124, 215], [42, 170, 144], [152, 190, 74], [249, 246, 30]],
	turbo: [[48, 18, 59], [54, 117, 237], [27, 208, 213], [123, 252, 76], [249, 151, 38], [122, 4, 3]],
	viridis: [[68, 1, 84], [59, 82, 139], [33, 145, 140], [94, 201, 98], [253, 231, 37]],
	plasma: [[13, 8, 135], [126, 3, 168], [204, 71, 120], [248, 149, 64], [240, 249, 33]],
	inferno: [[0, 0, 4], [87, 16, 110], [188, 55, 84], [249, 142, 9], [252, 255, 164]],
};

// 現在選択中のパレット名。setHeatPalette で切り替え、renderHeatmap が resolveHeatStops で解決して使う。
let heatPalette = "standard";

// 消費強度ヒートマップのパレットを切り替える。以後の描画がこの設定に従う。未知の名前は standard へ丸める。
export function setHeatPalette(name) {
	heatPalette = (HEAT_PALETTES[name] || name === "gray") ? name : "standard";
}




// 配色ピッカーへ並べるパレットの一覧。value は設定値、i18n は表示名の辞書キー。表示順もこの並びに従う。
export function heatPaletteOptions() {
	return [
		{ value: "standard", i18n: "settings.heat.standard" },
		{ value: "parula", i18n: "settings.heat.parula" },
		{ value: "turbo", i18n: "settings.heat.turbo" },
		{ value: "viridis", i18n: "settings.heat.viridis" },
		{ value: "plasma", i18n: "settings.heat.plasma" },
		{ value: "inferno", i18n: "settings.heat.inferno" },
		{ value: "gray", i18n: "settings.heat.gray" },
	];
}




// 指定パレットの帯を表す CSS の linear-gradient 文字列を返す。配色ピッカーのスウォッチに使う。グレイスケールは現在の解決済みテーマで濃淡が決まる。
export function heatGradientCss(name) {
	const stops = resolveHeatStops(name);
	return `linear-gradient(90deg, ${stops.map((c) => `rgb(${c[0]}, ${c[1]}, ${c[2]})`).join(", ")})`;
}




// 指定パレット(既定は現在選択中)の stop 列を返す。グレイスケールはカード地との明暗差を保つため、解決済みテーマ(prefers-color-scheme)で濃淡の向きを変える。暗いテーマでは消費が多いほど明るく、明るいテーマでは多いほど暗くする。
function resolveHeatStops(name = heatPalette) {
	if (name === "gray") {
		const dark = typeof window !== "undefined" && window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
		return dark ? [[70, 70, 70], [236, 236, 236]] : [[212, 212, 212], [38, 38, 38]];
	}
	return HEAT_PALETTES[name] || HEAT_PALETTES.standard;
}




// 0〜1 の強度を、渡された stop 列の上の rgb 文字列へ変換する。
function heatColor(v, stops) {
	const f = Math.max(0, Math.min(1, v)) * (stops.length - 1);
	const i = Math.min(stops.length - 2, Math.floor(f));
	const k = f - i;
	const a = stops[i];
	const b = stops[i + 1];
	const c = a.map((x, j) => Math.round(x + (b[j] - x) * k));
	return `rgb(${c[0]}, ${c[1]}, ${c[2]})`;
}




// 未消費セルの地色(--heat-zero)を現在のパレットの最も低い強度の色へ合わせる。記録はあるが未消費のセルはこの色を薄めて塗るため、配色を変えても下端の色と地続きに見える。
function applyZeroColor(stops) {
	const low = stops[0];
	document.documentElement.style.setProperty("--heat-zero", `rgb(${low[0]}, ${low[1]}, ${low[2]})`);
}




// 履歴から曜日×時間帯(7×24)の消費を集計する。累積%はリセットで戻るため、連続サンプル間の Δ% のうち正のものだけを消費とみなし、後側サンプルの時刻が属する曜日・時間帯のバケツへ積む。リセット境界の負の差と、飛び(休止)区間は除く。count は0消費の区間も数えることで、よく居るが使わない時間帯の平均を正しく下げる。
function aggregateHeat(history, key) {
	const sum = Array.from({ length: 7 }, () => new Array(24).fill(0));
	const count = Array.from({ length: 7 }, () => new Array(24).fill(0));
	let total = 0;
	for (let i = 1; i < history.length; i++) {
		const prev = history[i - 1];
		const cur = history[i];
		if (!prev[key] || !cur[key]) {
			continue;
		}
		const dt = cur.ts - prev.ts;
		if (dt <= 0 || dt > HEAT_MAX_GAP_MS) {
			continue;
		}
		const d = cur[key].used_pct - prev[key].used_pct;
		if (d < 0) {
			continue;
		}
		const at = new Date(cur.ts);
		const day = at.getDay();
		const hour = at.getHours();
		sum[day][hour] += d;
		count[day][hour] += 1;
		total += 1;
	}
	return { sum, count, total };
}




// 曜日×時間帯の消費ヒートマップを描く。蓄積した時系列から平均消費(1区間あたりの Δ%)を集計し、最大値で正規化した強度を色で表す。集計対象の枠は key で渡す(session の増分を消費レートとする)。
export function renderHeatmap(mount, history, key, now) {
	const { sum, count, total } = aggregateHeat(history, key);
	const stops = resolveHeatStops();
	applyZeroColor(stops);
	if (total === 0) {
		const msg = document.createElement("div");
		msg.className = "heat-hint";
		msg.textContent = t("heat.empty");
		mount.replaceChildren(msg);
		return;
	}

	const days = dayLabels();

	// 平均消費を求め、最大で正規化する。記録のないセルは null として空セルにする。
	let maxAvg = 0;
	const avg = Array.from({ length: 7 }, () => new Array(24).fill(null));
	for (let d = 0; d < 7; d++) {
		for (let h = 0; h < 24; h++) {
			if (count[d][h] > 0) {
				const a = sum[d][h] / count[d][h];
				avg[d][h] = a;
				if (a > maxAvg) {
					maxAvg = a;
				}
			}
		}
	}

	// 現在の曜日行をタブで、今この時刻のセルを枠で示すための「今」の位置。
	const today = now.getDay();
	const nowHour = now.getHours();

	const grid = document.createElement("div");
	grid.className = "heat";

	// 左上の角と、時間帯ヘッダ(0/4/8/12/16/20 のみ数字)。
	const corner = document.createElement("div");
	corner.className = "h-hour";
	grid.appendChild(corner);
	for (let h = 0; h < 24; h++) {
		const head = document.createElement("div");
		head.className = "h-hour";
		head.textContent = h % 4 === 0 ? String(h) : "";
		grid.appendChild(head);
	}

	for (let d = 0; d < 7; d++) {
		const lab = document.createElement("div");
		lab.className = "h-day";
		if (d === today) {
			lab.classList.add("today");
		}
		lab.textContent = days[d];
		grid.appendChild(lab);
		for (let h = 0; h < 24; h++) {
			const cell = document.createElement("div");
			cell.className = "h-cell";
			// 一日の左右端だけ角丸にして、横一列を地続きの帯として見せる。
			if (h === 0) {
				cell.classList.add("day-start");
			} else if (h === 23) {
				cell.classList.add("day-end");
			}
			// 今この時刻のセルに「今ここ」の枠を重ねる。データの有無に関わらず印を出す。
			if (d === today && h === nowHour) {
				cell.classList.add("now");
			}
			if (avg[d][h] == null) {
				cell.classList.add("empty");
				cell.title = t("heat.cell.noRecord", { day: days[d], hour: h });
			} else if (avg[d][h] === 0) {
				// 記録はあるが消費ゼロのセル。塗りつぶさず枠線だけにして「居たが使わなかった」を表す。
				cell.classList.add("zero");
				cell.title = t("heat.cell.unused", { day: days[d], hour: h, count: count[d][h] });
			} else {
				cell.style.background = heatColor(maxAvg > 0 ? avg[d][h] / maxAvg : 0, stops);
				cell.title = t("heat.cell.avg", { day: days[d], hour: h, avg: avg[d][h].toFixed(1), count: count[d][h] });
			}
			grid.appendChild(cell);
		}
	}

	mount.replaceChildren(grid);
}
