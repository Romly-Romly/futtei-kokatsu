; アンインストール時に自動起動の登録を後始末するフック。Tauri の NSIS テンプレート(installer.nsi)から !insertmacro NSIS_HOOK_PREUNINSTALL で呼ばれる。
; tauri-plugin-autostart が使う auto-launch は Windows で二か所へ書き込む。一方は HKCU の Run キー、もう一方はタスクマネージャーのスタートアップ有効状態を保持する StartupApproved\Run で、いずれも値名はアプリ名(=productName)になる。
; Run キー側は Tauri の NSIS テンプレートが既に削除するが、StartupApproved\Run 側はテンプレートが触れず孤児として残るため、ここで同じ値名を消す。
; 値名はテンプレートの Run キー削除と揃えて ${PRODUCTNAME}(tauri.conf.json の productName 由来)を引く。これでアプリ名を変えても削除対象が自動で追従する。
; 更新時(/UPDATE)は自動起動の有効状態を温存するため、テンプレートの Run キー削除と同じく $UpdateMode が立っているときは消さない。

!macro NSIS_HOOK_PREUNINSTALL
	${If} $UpdateMode <> 1
		DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run" "${PRODUCTNAME}"
	${EndIf}
!macroend
