// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Romly

// 画像の解像度の倍率。論理ピクセルにこの倍率を掛けた画素数で描き、高 DPI の画面や SNS での拡大表示でも文字や線がぼけないようにする。
const SCALE = 2;

// 画像の外周の余白(論理ピクセル)。
const FRAME_PAD = 16;

// カードどうしの間隔(論理ピクセル)。
const CARD_GAP = 12;

// カード1枚の画像の幅(論理ピクセル)。
const SINGLE_WIDTH = 640;

// 複数のカードを並べた画像の幅(論理ピクセル)。利用枠カードを2列に並べても、各カードの見出し(枠名・見込みピル・使用%)が1行に収まる幅にする。
const ALL_WIDTH = 880;

// 利用枠カードの高さ(論理ピクセル)。
const USAGE_CARD_HEIGHT = 320;

// 消費傾向カードの高さ(論理ピクセル)。
const HEAT_CARD_HEIGHT = 220;

// ヒートマップの曜日ラベルの列幅(論理ピクセル)。
const HEAT_LABEL_WIDTH = 24;

// ヒートマップの時間帯ヘッダの行の高さ(論理ピクセル)。
const HEAT_HEADER_HEIGHT = 14;

// カード列とフッターの間隔(論理ピクセル)。
const FOOTER_GAP = 10;

// フッター(アプリ名とデータの時点)の文字の大きさ(論理ピクセル)。
const FOOTER_FONT_SIZE = 11;










// 計算済みスタイルの長さ(px)を数値にする。解釈できなければ 0 を返す。
function px(value)
{
	const n = parseFloat(value);
	return Number.isFinite(n) ? n : 0;
}










// 計算済みスタイルの行の高さ(px)を返す。line-height が normal なら文字の大きさの 1.2 倍、単位の無い倍率なら文字の大きさにその倍率を掛けた値とみなす。
function lineHeightOf(cs)
{
	const n = parseFloat(cs.lineHeight);
	if (!Number.isFinite(n))
		return px(cs.fontSize) * 1.2;
	else if (cs.lineHeight.endsWith("px"))
		return n;
	else
		return n * px(cs.fontSize);
}










// 計算済みスタイルの字体を canvas の font 指定にする。
function fontOf(cs)
{
	return `${cs.fontWeight} ${cs.fontSize} ${cs.fontFamily}`;
}










// 文字列を計算済みスタイルの字体と文字色で描く。y は文字の縦の中心、align は canvas の textAlign に渡す値。
function drawText(ctx, cs, text, x, y, align)
{
	ctx.font = fontOf(cs);
	ctx.fillStyle = cs.color;
	ctx.textAlign = align;
	ctx.textBaseline = "middle";
	ctx.fillText(text, x, y);
}










// 文字列を計算済みスタイルの字体で描いた時の幅を返す。
function textWidth(ctx, cs, text)
{
	ctx.font = fontOf(cs);
	return ctx.measureText(text).width;
}










// 矩形を計算済みスタイルの地色と角丸で塗る。
function fillBox(ctx, cs, x, y, w, h)
{
	// canvas が解釈できない色を渡すと代入が無視され、直前に塗った色で塗ってしまう。先に透明を入れておき、その場合は何も塗らないようにする。
	ctx.fillStyle = "rgba(0, 0, 0, 0)";
	ctx.fillStyle = cs.backgroundColor;
	ctx.beginPath();
	ctx.roundRect(x, y, w, h, [px(cs.borderTopLeftRadius), px(cs.borderTopRightRadius), px(cs.borderBottomRightRadius), px(cs.borderBottomLeftRadius)]);
	ctx.fill();
}










// カードの地と枠線を、計算済みスタイルの地色・枠線・角丸で描く。
function drawCardBox(ctx, cs, x, y, w, h)
{
	fillBox(ctx, cs, x, y, w, h);
	const bw = px(cs.borderTopWidth);
	if (bw <= 0)
		return;

	ctx.strokeStyle = cs.borderTopColor;
	ctx.lineWidth = bw;
	ctx.beginPath();
	ctx.roundRect(x + bw / 2, y + bw / 2, w - bw, h - bw, px(cs.borderTopLeftRadius));
	ctx.stroke();
}










// SVG の塗り・線の色に書かれた CSS 変数の参照(var(--name))を、今のテーマで解決した値へ置き換える。
function resolveCssVars(svg)
{
	const root = getComputedStyle(document.documentElement);
	for (const node of [svg, ...svg.querySelectorAll("*")])
	{
		for (const attr of ["fill", "stroke"])
		{
			const m = /^var\((--[\w-]+)\)$/.exec(node.getAttribute(attr) || "");
			if (m)
				node.setAttribute(attr, root.getPropertyValue(m[1]).trim());
		}
	}
}










// SVG を画像として読み込み、canvas の指定の矩形へ描く。
// 単体の画像として読み込んだ SVG はページのスタイルシートを参照できないため、CSS 変数の色を今のテーマの値へ置き換え、字体もページと同じものを明示してから読み込む。
async function drawSvg(ctx, svg, x, y, w, h)
{
	resolveCssVars(svg);
	svg.setAttribute("font-family", getComputedStyle(document.documentElement).fontFamily);
	svg.setAttribute("width", String(w * SCALE));
	svg.setAttribute("height", String(h * SCALE));

	const blob = new Blob([new XMLSerializer().serializeToString(svg)], { type: "image/svg+xml" });
	const url = URL.createObjectURL(blob);
	try
	{
		const img = new Image();
		img.src = url;
		await img.decode();
		ctx.drawImage(img, x, y, w, h);
	}
	finally
	{
		URL.revokeObjectURL(url);
	}
}










// 利用枠パネル1枚をカードとして描く。
// 枠名・見込みピル・使用%・メーター・リセット時刻・判定・期間は、画面のパネル(main.js の createPanel と renderMeter が組み立てた DOM)から文言と計算済みスタイルを読み取り、画面と同じ内容と配色にする。期間は card.showPeriod が真の時だけ描く。
// バーンダウンは card.buildChart にグラフ域の寸法を渡して組み立てさせた SVG を描く。
async function drawUsageCard(ctx, card, x, y, w, h)
{
	const slot = card.slot;
	const panelCs = getComputedStyle(slot.panel);
	drawCardBox(ctx, panelCs, x, y, w, h);

	const left = x + px(panelCs.paddingLeft);
	const right = x + w - px(panelCs.paddingRight);
	const bottom = y + h - px(panelCs.paddingBottom);
	const gap = px(panelCs.rowGap);
	let top = y + px(panelCs.paddingTop);

	// 見出し行。枠名を左端に、使用%を右端に、見込みピルをその左に置く。
	const nameCs = getComputedStyle(slot.name);
	const pill = slot.head.querySelector(".pill");
	const pct = slot.head.querySelector(".meter-pct");
	const pillCs = pill ? getComputedStyle(pill) : null;
	const pctCs = pct ? getComputedStyle(pct) : null;
	const pillH = pill ? lineHeightOf(pillCs) + px(pillCs.paddingTop) + px(pillCs.paddingBottom) : 0;
	const capH = Math.max(lineHeightOf(nameCs), pillH, pct ? lineHeightOf(pctCs) : 0);
	const capMid = top + capH / 2;
	drawText(ctx, nameCs, slot.name.textContent, left, capMid, "left");
	let headRight = right;
	if (pct)
	{
		drawText(ctx, pctCs, pct.textContent, headRight, capMid, "right");
		headRight -= textWidth(ctx, pctCs, pct.textContent) + px(getComputedStyle(slot.head).columnGap);
	}
	if (pill)
	{
		const pillW = textWidth(ctx, pillCs, pill.textContent) + px(pillCs.paddingLeft) + px(pillCs.paddingRight);
		fillBox(ctx, pillCs, headRight - pillW, capMid - pillH / 2, pillW, pillH);
		drawText(ctx, pillCs, pill.textContent, headRight - pillW / 2, capMid, "center");
	}
	top += capH + gap;

	// メーター。塗りの長さとキャレットの位置は、画面と同じく要素に当てた幅・左端の割合から求める。
	const track = slot.mount.querySelector(".meter-track");
	if (track)
	{
		const trackCs = getComputedStyle(track);
		const trackW = right - left;
		const trackH = px(trackCs.height);
		fillBox(ctx, trackCs, left, top, trackW, trackH);
		const fill = track.querySelector(".meter-fill");
		if (fill)
			fillBox(ctx, getComputedStyle(fill), left, top, trackW * px(fill.style.width) / 100, trackH);

		const caret = track.querySelector(".meter-caret");
		if (caret)
		{
			const caretCs = getComputedStyle(caret);
			const caretTop = top + px(caretCs.top);
			const caretBottom = top + trackH - px(caretCs.bottom);
			fillBox(ctx, caretCs, left + trackW * px(caret.style.left) / 100, caretTop, px(caretCs.width), caretBottom - caretTop);
		}
		top += trackH + px(getComputedStyle(slot.mount).rowGap);
	}
	const reset = slot.mount.querySelector(".meter-reset");
	if (reset)
	{
		const resetCs = getComputedStyle(reset);
		const resetH = lineHeightOf(resetCs);
		drawText(ctx, resetCs, reset.textContent, left, top + resetH / 2, "left");
		top += resetH;
	}
	top += gap;

	// バーンダウンと、その右上へ重ねる判定。判定の位置は画面と同じく、計算済みスタイルの top・right から求める。
	const svg = card.buildChart(right - left, bottom - top);
	if (svg)
		await drawSvg(ctx, svg, left, top, right - left, bottom - top);

	const verdict = slot.verdict.textContent;
	if (verdict)
	{
		const verdictCs = getComputedStyle(slot.verdict);
		ctx.font = fontOf(verdictCs);
		ctx.fillStyle = verdictCs.color;
		ctx.textAlign = "right";
		ctx.textBaseline = "bottom";
		ctx.fillText(verdict, right - px(verdictCs.right), top + px(verdictCs.top));
	}

	// 期間は、画面と同じく判定と左右で対になるグラフ左上へ置く。画像には切り替えのボタンを描かないため、期間の文字を画面の操作の並びの左端から描く。
	if (card.showPeriod && slot.navLabel)
	{
		const navCs = getComputedStyle(slot.nav);
		drawText(ctx, getComputedStyle(slot.navLabel), slot.navLabel.textContent, left + px(navCs.left), top + px(navCs.top) + px(navCs.height) / 2, "left");
	}
}










// ヒートマップの「今この時刻」の升に重ねる枠を描く。画面(styles.css の .heat .h-cell.now)と同じく、どの塗り色の上でも埋没しないよう白と黒の二重枠を内側に重ねる。
function drawNowRing(ctx, x, y, w, h)
{
	ctx.lineWidth = 1;
	ctx.strokeStyle = "#000000";
	ctx.strokeRect(x + 1.5, y + 1.5, w - 3, h - 3);
	ctx.strokeStyle = "#ffffff";
	ctx.strokeRect(x + 0.5, y + 0.5, w - 1, h - 1);
}










// 消費傾向カードを描く。
// 見出しとヒートマップの升は、画面のカード(index.html の消費傾向カードと chart.js の renderHeatmap が組み立てた DOM)から文言と計算済みスタイルを読み取り、画面と同じ配色にする。
// 升は左上の角・時間帯ヘッダ・曜日ごとの行(曜日ラベルと時間帯の升)の順に並んでいるので、その並びのままカードの寸法に合わせて敷き直す。
function drawHeatCard(ctx, card, x, y, w, h)
{
	const cardCs = getComputedStyle(card.el);
	drawCardBox(ctx, cardCs, x, y, w, h);

	const left = x + px(cardCs.paddingLeft);
	const right = x + w - px(cardCs.paddingRight);
	const bottom = y + h - px(cardCs.paddingBottom);
	let top = y + px(cardCs.paddingTop);

	const name = card.el.querySelector(".chart-name");
	const nameCs = getComputedStyle(name);
	const capH = lineHeightOf(nameCs);
	drawText(ctx, nameCs, name.textContent, left, top + capH / 2, "left");
	top += capH + px(getComputedStyle(card.el.querySelector(".chart-cap")).marginBottom);

	const grid = card.el.querySelector(".heat");
	if (!grid)
	{
		// 蓄積が足りずヒートマップの代わりに案内文が出ている。
		const hint = card.el.querySelector(".heat-hint");
		if (hint)
		{
			const hintCs = getComputedStyle(hint);
			drawText(ctx, hintCs, hint.textContent, left, top + px(hintCs.paddingTop) + lineHeightOf(hintCs) / 2, "left");
		}
		return;
	}

	const hours = 24;
	const days = 7;
	const gridCs = getComputedStyle(grid);
	const colGap = px(gridCs.columnGap);
	const rowGap = px(gridCs.rowGap);
	const cellW = (right - left - HEAT_LABEL_WIDTH - hours * colGap) / hours;
	const cellH = (bottom - top - HEAT_HEADER_HEIGHT - days * rowGap) / days;
	const items = grid.children;
	for (let i = 0; i < items.length; i++)
	{
		const col = i % (hours + 1);
		const row = Math.floor(i / (hours + 1));
		const cx = col === 0 ? left : left + HEAT_LABEL_WIDTH + colGap + (col - 1) * (cellW + colGap);
		const cy = row === 0 ? top : top + HEAT_HEADER_HEIGHT + rowGap + (row - 1) * (cellH + rowGap);
		const cw = col === 0 ? HEAT_LABEL_WIDTH : cellW;
		const ch = row === 0 ? HEAT_HEADER_HEIGHT : cellH;
		const item = items[i];
		const cs = getComputedStyle(item);
		fillBox(ctx, cs, cx, cy, cw, ch);
		if (item.classList.contains("now"))
			drawNowRing(ctx, cx, cy, cw, ch);
		if (item.textContent)
			drawText(ctx, cs, item.textContent, cx + cw / 2, cy + ch / 2, "center");
	}
}










// カードの高さ(論理ピクセル)を返す。
function cardHeight(card)
{
	if (card.kind === "heat")
		return HEAT_CARD_HEIGHT;
	else
		return USAGE_CARD_HEIGHT;
}










// カードの並びを行へ割り付ける。wide が真のカードは1行を占め、それ以外は並び順に2枚ずつ横へ並べる。相方の無い1枚は1行を占める。
function layoutRows(cards)
{
	const rows = [];
	let pending = null;
	for (const card of cards)
	{
		if (card.wide)
		{
			if (pending)
				rows.push([pending]);
			pending = null;
			rows.push([card]);
		}
		else if (pending)
		{
			rows.push([pending, card]);
			pending = null;
		}
		else
		{
			pending = card;
		}
	}
	if (pending)
		rows.push([pending]);

	return rows;
}










// カードを並べた PNG 画像を作り、そのバイト列(Uint8Array)を返す。
// cards の各要素は描くカードの記述。kind が "usage" なら利用枠パネルで、slot に main.js のパネル一式、buildChart に寸法からバーンダウンの SVG を組み立てる関数、showPeriod にバーンダウンの左上へ期間を描くかを持つ。"heat" なら消費傾向カードで、el にカードの要素を持つ。wide が真のカードは1行を占める。
// カードが1枚ならグラフ1枚の幅、複数ならまとめた画像の幅で描き、下端の左右に footer.left と footer.right の文言を添える。
export async function renderCardsPng(cards, footer)
{
	const rows = layoutRows(cards);
	const width = cards.length > 1 ? ALL_WIDTH : SINGLE_WIDTH;
	const innerW = width - FRAME_PAD * 2;
	const rowHeights = rows.map((row) => Math.max(...row.map(cardHeight)));
	const footerH = FOOTER_FONT_SIZE * 1.5;
	const height = FRAME_PAD * 2 + rowHeights.reduce((a, b) => a + b, 0) + CARD_GAP * (rows.length - 1) + FOOTER_GAP + footerH;

	const canvas = document.createElement("canvas");
	canvas.width = Math.round(width * SCALE);
	canvas.height = Math.round(height * SCALE);
	const ctx = canvas.getContext("2d");
	ctx.scale(SCALE, SCALE);

	const rootCs = getComputedStyle(document.documentElement);
	ctx.fillStyle = rootCs.getPropertyValue("--export-bg").trim();
	ctx.fillRect(0, 0, width, height);

	let top = FRAME_PAD;
	for (let r = 0; r < rows.length; r++)
	{
		const row = rows[r];
		const cardW = (innerW - CARD_GAP * (row.length - 1)) / row.length;
		for (let i = 0; i < row.length; i++)
		{
			const x = FRAME_PAD + i * (cardW + CARD_GAP);
			if (row[i].kind === "heat")
				drawHeatCard(ctx, row[i], x, top, cardW, rowHeights[r]);
			else
				await drawUsageCard(ctx, row[i], x, top, cardW, rowHeights[r]);
		}
		top += rowHeights[r] + CARD_GAP;
	}

	// フッター。アプリ名を左端に、データの時点を右端に添える。
	const footerMid = height - FRAME_PAD - footerH / 2;
	ctx.font = `${FOOTER_FONT_SIZE}px ${rootCs.fontFamily}`;
	ctx.fillStyle = rootCs.getPropertyValue("--chart-sub").trim();
	ctx.textBaseline = "middle";
	ctx.textAlign = "left";
	ctx.fillText(footer.left, FRAME_PAD, footerMid);
	ctx.textAlign = "right";
	ctx.fillText(footer.right, width - FRAME_PAD, footerMid);

	const blob = await new Promise((resolve) => canvas.toBlob(resolve, "image/png"));
	if (!blob)
		throw new Error("PNG の生成に失敗しました");

	return new Uint8Array(await blob.arrayBuffer());
}
