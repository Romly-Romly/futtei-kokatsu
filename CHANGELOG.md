# Changelog

> 🌐 **[日本語 →](CHANGELOG.ja.md)**

## [1.4.0] - 2026-07-21

- Added a "Weekly (Fable)" outer ring to the tray pie chart, giving it a three-layer structure: session, weekly (all models), and weekly (Fable).
- Fixed the in-chart labels (now, depleted, reset) sometimes staying in English because they followed the date-format setting; they now follow the display-language setting.
- macOS: Fixed an issue where launching from Finder or the Dock failed to find the `claude` command and could not fetch usage limits.
- Made error logs viewable after the fact.

## [1.3.0] - 2026-07-19

- Added support for the newly introduced "Current week (Fable)" usage limit, and reworked the layout to accommodate the extra panel.
- Added a "Weekly (Fable)" chart to the tray icon designs as well.
- Made the usage-limit panels generate dynamically from the fetched data, so future additions or removals of limits are picked up automatically.

## [1.2.0] - 2026-07-18

- Moved the last-updated time and accumulated data count to the title bar, and removed the buttons that were in the header.
- Made the consumption-trend heatmap palette switchable on the spot from the right-click menu.
- Added a "GitHub-style" palette to the consumption-trend heatmap.

## [1.1.1] - 2026-07-11

- Made the tray icon chart (session burndown, weekly burndown, or pie chart) selectable from the tray's right-click menu as well.
- Collapsed time labels to a single unit when the lower value is exactly zero, so "1 hour 0 minutes ago" or "2 days 0 hours" now read as "1 hour ago" or "2 days ago".

## [1.1.0] - 2026-07-06

- Added a choice of chart to show in the tray icon (menu bar): session burndown, weekly burndown, or pie chart.
- Added a setting to automatically tuck the window into the tray when it loses focus.
- Added Ctrl+W to hide the window to the tray.
- Added a right-click menu to the main window.
- Highlighted the current position in the consumption-trend heatmap.
- Changed the session chart to always show the full range.
- Added a macOS version.

## [1.0.2] - 2026-07-03

- Suppressed the console window that briefly flashed on startup of the installed build.

## [1.0.1] - 2026-07-03

- Fixed a bug where the chart grew endlessly taller in the packaged app.

## [1.0.0] - 2026-07-02

- Initial release.
