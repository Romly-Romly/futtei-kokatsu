// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Romly

import ja from "./locales/ja.json";
import en from "./locales/en.json";

// 表示に使える言語と、訳が欠けたときに当てる言語。辞書は locales/<locale>.json に置く。
const DICTS = { ja, en };
const SUPPORTED = ["ja", "en"];
const FALLBACK = "ja";




// 言語設定('system'/'ja'/'en')と OS のロケール文字列から、実際に使う言語を決める。'system' のときは OS ロケールの先頭が ja なら日本語、en なら英語にし、対応外なら既定言語へ落とす。
export function resolveLocale(setting, systemLocale) {
	if (SUPPORTED.includes(setting)) {
		return setting;
	}
	const base = String(systemLocale || "").toLowerCase();
	if (base.startsWith("ja")) {
		return "ja";
	}
	if (base.startsWith("en")) {
		return "en";
	}
	return FALLBACK;
}




// 指定言語の辞書を組み立てる。既定言語を土台に指定言語を上書きし、指定言語に訳の無いキーは既定言語の訳で埋める。
export function buildDict(locale) {
	const base = DICTS[FALLBACK] || {};
	if (locale === FALLBACK) {
		return base;
	}
	return { ...base, ...(DICTS[locale] || {}) };
}




// 辞書からキーに対応する文言を引き、{name} 形式のプレースホルダを vars の値で差し替える。キーが辞書に無ければキー文字列をそのまま返す。
export function translate(dict, key, vars) {
	let text = (dict && dict[key] !== undefined) ? dict[key] : key;
	if (vars) {
		for (const name of Object.keys(vars)) {
			text = text.split("{" + name + "}").join(String(vars[name]));
		}
	}
	return text;
}




// data-i18n 系の属性を持つ要素の文言を辞書で差し替える。data-i18n は本文を、data-i18n-title と data-i18n-aria-label はそれぞれ title 属性と aria-label 属性を差し替える。いずれも属性値をキーとして引く。
export function applyI18n(root, dict) {
	for (const el of root.querySelectorAll("[data-i18n]")) {
		el.textContent = translate(dict, el.dataset.i18n);
	}
	for (const el of root.querySelectorAll("[data-i18n-title]")) {
		el.setAttribute("title", translate(dict, el.dataset.i18nTitle));
	}
	for (const el of root.querySelectorAll("[data-i18n-aria-label]")) {
		el.setAttribute("aria-label", translate(dict, el.dataset.i18nAriaLabel));
	}
}
