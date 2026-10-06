; Remove the "Start with Windows" entry (see src/autostart.rs) so it does not outlive the app.
; Upgrades also run the old uninstaller (with /UPDATE); keep the entry then.
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Elgato Control Center"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run" "Elgato Control Center"
  ${EndIf}
!macroend
