// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Romly

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
	futtei_kokatsu_lib::run()
}
