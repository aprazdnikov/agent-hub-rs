; Per-user installer. The release executable and portable ZIP are not modified.
Unicode true
RequestExecutionLevel user
Name "agent-hub ${VERSION}"
OutFile "${OUTPUT_EXE}"
InstallDir "$LOCALAPPDATA\Programs\agent-hub"
ShowInstDetails show
ShowUninstDetails show

!include "MUI2.nsh"
!ifdef TEST_UNINSTALL_KEY
  !define UNINSTALL_KEY "${TEST_UNINSTALL_KEY}"
!else
  !define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\agent-hub"
!endif
Var ShortcutDir
Var LockHandle
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; The profile directory is protected by the user's ACL. Keep an exclusive file
; handle open for the entire process; Windows releases it on exit or crash.
; Never delete the lock file: deleting it would let another process lock a new
; file while an existing process still owns the old one.
!macro DefineInit PREFIX
Function ${PREFIX}.onInit
  SetShellVarContext current
!ifdef TEST_SHORTCUT_DIR
  StrCpy $ShortcutDir "${TEST_SHORTCUT_DIR}"
!else
  StrCpy $ShortcutDir "$SMPROGRAMS\agent-hub"
!endif
!ifdef TEST_LOCK_FILE
  StrCpy $0 "${TEST_LOCK_FILE}"
!else
  StrCpy $0 "$LOCALAPPDATA\agent-hub-installer.lock"
!endif
  ; OPEN_ALWAYS, no sharing. No waits and no inherited handle.
  System::Call 'kernel32::CreateFileW(w r0, i 0xC0000000, i 0, p 0, i 4, i 0x80, p 0) p.r1'
  IntCmp $1 -1 lock_error
  StrCpy $LockHandle $1
  Return
lock_error:
  MessageBox MB_OK|MB_ICONSTOP "Another agent-hub installer or uninstaller is running, or the installation lock is unavailable." /SD IDOK
  SetErrorLevel 1
  Abort
FunctionEnd
!macroend
!insertmacro DefineInit ""
!insertmacro DefineInit "un"

; Check both before staging and immediately before replacement. The checked
; rename below handles a process starting after this probe.
Function CheckExecutableAvailable
  IfFileExists "$INSTDIR\agent-hub.exe" 0 done
  System::Call 'kernel32::CreateFileW(w "$INSTDIR\agent-hub.exe", i 0x80000000, i 0, p 0, i 3, i 0x80, p 0) p.r0'
  IntCmp $0 -1 locked
  System::Call 'kernel32::CloseHandle(p r0)'
  Goto done
locked:
  ; Only remove staging paths after this invocation has claimed them.
  StrCmp $8 1 0 +2
  Call RemoveStaged
  MessageBox MB_OK|MB_ICONSTOP "Close agent-hub and any process using its executable, then run the installer again." /SD IDOK
  SetErrorLevel 1
  Abort
 done:
FunctionEnd

; Return 0 if the uninstall key exists, 2 if it is absent, another Windows
; error otherwise. This avoids overwriting an unrelated or inaccessible key.
Function CheckUninstallKey
  System::Call 'advapi32::RegOpenKeyExW(p 0x80000001, w "${UNINSTALL_KEY}", i 0, i 0x20019, *p .r1) i.r0'
  StrCmp $0 0 0 done
  System::Call 'advapi32::RegCloseKey(p r1)'
 done:
FunctionEnd

Function RemoveBackups
  ClearErrors
  Delete "$INSTDIR\LICENSE.previous"
  Delete "$INSTDIR\uninstall.exe.previous"
  Delete "$INSTDIR\agent-hub.exe.previous"
  IfFileExists "$INSTDIR\LICENSE.previous" backup_error
  IfFileExists "$INSTDIR\uninstall.exe.previous" backup_error
  IfFileExists "$INSTDIR\agent-hub.exe.previous" backup_error
  ClearErrors
  DeleteRegValue HKCU "${UNINSTALL_KEY}" "UpgradeCommitted"
  IfErrors backup_error
  ClearErrors
  Return
backup_error:
  SetErrors
FunctionEnd

Function RemoveStaged
  Delete "$INSTDIR\agent-hub.exe.staged"
  Delete "$INSTDIR\LICENSE.staged"
  Delete "$INSTDIR\uninstall.exe.staged"
  Delete "$INSTDIR\agent-hub.install-kind.staged"
FunctionEnd

Section "Install"
  SetShellVarContext current
  StrCpy $8 0
  ; The recovery flag belongs to the existing uninstall registry entry. No
  ; user file is interpreted as an uninstall journal.
  IfFileExists "$INSTDIR\uninstall-recovery.exe.staged" recovery_staged
  Call CheckUninstallKey
  StrCmp $0 2 uninstall_recovered
  StrCmp $0 0 0 recovery
  ClearErrors
  ReadRegStr $1 HKCU "${UNINSTALL_KEY}" "UninstallPending"
  IfErrors uninstall_recovered
  StrCmp $1 "pending" 0 recovery
  ClearErrors
  ReadRegStr $1 HKCU "${UNINSTALL_KEY}" "InstallLocation"
  IfErrors recovery
  StrCmp $1 $INSTDIR 0 recovery
  ReadRegStr $1 HKCU "${UNINSTALL_KEY}" "UninstallString"
  IfErrors recovery
  StrCmp $1 '"$INSTDIR\uninstall.exe"' 0 recovery
  IfFileExists "$INSTDIR\agent-hub.exe" recovery
  IfFileExists "$INSTDIR\LICENSE" recovery
  IfFileExists "$INSTDIR\agent-hub.install-kind" recovery
  IfFileExists "$ShortcutDir\agent-hub.lnk" recovery
  IfFileExists "$ShortcutDir\Uninstall agent-hub.lnk" recovery
  IfFileExists "$INSTDIR\uninstall.exe" uninstall_executable_restored
  ; Publish only a complete uninstaller. If interrupted while staging, leave
  ; the ambiguous file untouched for manual inspection instead of replacing it.
  ClearErrors
  WriteUninstaller "$INSTDIR\uninstall-recovery.exe.staged"
  IfErrors recovery
  Rename "$INSTDIR\uninstall-recovery.exe.staged" "$INSTDIR\uninstall.exe"
  IfErrors recovery
uninstall_executable_restored:
  ClearErrors
  DeleteRegValue HKCU "${UNINSTALL_KEY}" "UninstallPending"
  IfErrors recovery
  MessageBox MB_OK|MB_ICONSTOP "An interrupted uninstall was recovered. Run the uninstaller again before installing." /SD IDOK
  SetErrorLevel 1
  Abort
uninstall_recovered:
  ; Staging files represent an incomplete transaction and require inspection.
  IfFileExists "$INSTDIR\agent-hub.exe.staged" recovery
  IfFileExists "$INSTDIR\LICENSE.staged" recovery
  IfFileExists "$INSTDIR\uninstall.exe.staged" recovery
  IfFileExists "$INSTDIR\agent-hub.install-kind.staged" recovery
  ; Only a commit flag in this installation's registry entry authorizes
  ; deleting backups. An unrecognized .previous file is never removed.
  Call CheckUninstallKey
  StrCmp $0 2 no_committed_upgrade
  StrCmp $0 0 0 recovery
  ClearErrors
  ReadRegStr $1 HKCU "${UNINSTALL_KEY}" "UpgradeCommitted"
  IfErrors no_committed_upgrade
  StrCmp $1 "committed" 0 recovery
  ClearErrors
  ReadRegStr $1 HKCU "${UNINSTALL_KEY}" "InstallLocation"
  IfErrors recovery
  StrCmp $1 $INSTDIR 0 recovery
  ReadRegStr $1 HKCU "${UNINSTALL_KEY}" "UninstallString"
  IfErrors recovery
  StrCmp $1 '"$INSTDIR\uninstall.exe"' 0 recovery
  Goto check_completed_upgrade
no_committed_upgrade:
  IfFileExists "$INSTDIR\agent-hub.exe.previous" recovery
  IfFileExists "$INSTDIR\LICENSE.previous" recovery
  IfFileExists "$INSTDIR\uninstall.exe.previous" recovery
  Goto backups_clean
check_completed_upgrade:
  IfFileExists "$INSTDIR\agent-hub.exe" 0 recovery
  IfFileExists "$INSTDIR\LICENSE" 0 recovery
  IfFileExists "$INSTDIR\uninstall.exe" 0 recovery
  ClearErrors
  FileOpen $0 "$INSTDIR\agent-hub.install-kind" r
  IfErrors recovery
  FileRead $0 $1
  FileClose $0
  StrCmp $1 "nsis" 0 recovery
  Call CheckUninstallKey
  StrCmp $0 0 0 recovery
  Call RemoveBackups
  IfErrors backup_failed
backups_clean:
  StrCpy $9 "fresh"
  IfFileExists "$INSTDIR\agent-hub.exe" 0 fresh_check
  StrCpy $9 "upgrade"
  ; The old executable is never exposed without the existing marker.
  ClearErrors
  FileOpen $0 "$INSTDIR\agent-hub.install-kind" r
  IfErrors recovery
  FileRead $0 $1
  FileClose $0
  StrCmp $1 "nsis" 0 recovery
  IfFileExists "$INSTDIR\LICENSE" 0 recovery
  IfFileExists "$INSTDIR\uninstall.exe" 0 recovery
  Call CheckUninstallKey
  StrCmp $0 0 0 recovery
  ClearErrors
  ReadRegStr $1 HKCU "${UNINSTALL_KEY}" "InstallLocation"
  IfErrors recovery
  StrCmp $1 $INSTDIR 0 recovery
  ReadRegStr $1 HKCU "${UNINSTALL_KEY}" "UninstallString"
  IfErrors recovery
  StrCmp $1 '"$INSTDIR\uninstall.exe"' 0 recovery
  Call CheckExecutableAvailable
  Goto stage
fresh_check:
  ; Do not take ownership of another installation or pre-existing shortcuts.
  IfFileExists "$INSTDIR\agent-hub.install-kind" recovery
  IfFileExists "$INSTDIR\LICENSE" recovery
  IfFileExists "$INSTDIR\uninstall.exe" recovery
  IfFileExists "$ShortcutDir\agent-hub.lnk" recovery
  IfFileExists "$ShortcutDir\Uninstall agent-hub.lnk" recovery
  Call CheckUninstallKey
  StrCmp $0 2 0 recovery
stage:
  CreateDirectory "$INSTDIR"
  SetOutPath "$INSTDIR"
  ClearErrors
  File /oname=agent-hub.exe.staged "${INPUT_EXE}"
  IfErrors stage_failed
  File /oname=LICENSE.staged "${LICENSE_FILE}"
  IfErrors stage_failed
  WriteUninstaller "$INSTDIR\uninstall.exe.staged"
  IfErrors stage_failed
  StrCpy $8 1
  StrCmp $9 "upgrade" upgrade fresh

fresh:
  ClearErrors
  FileOpen $0 "$INSTDIR\agent-hub.install-kind.staged" w
  IfErrors fresh_failed
  FileWrite $0 "nsis"
  FileClose $0
  IfErrors fresh_failed
  Rename "$INSTDIR\agent-hub.install-kind.staged" "$INSTDIR\agent-hub.install-kind"
  IfErrors fresh_failed
  Rename "$INSTDIR\LICENSE.staged" "$INSTDIR\LICENSE"
  IfErrors fresh_failed
  Rename "$INSTDIR\uninstall.exe.staged" "$INSTDIR\uninstall.exe"
  IfErrors fresh_failed
  CreateDirectory "$ShortcutDir"
  CreateShortcut "$ShortcutDir\agent-hub.lnk" "$INSTDIR\agent-hub.exe"
  CreateShortcut "$ShortcutDir\Uninstall agent-hub.lnk" "$INSTDIR\uninstall.exe"
  IfErrors fresh_failed
  ClearErrors
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "agent-hub"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\agent-hub.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  IfErrors fresh_failed
  ; Publish the executable only after marker, uninstaller and integration exist.
  ClearErrors
  Rename "$INSTDIR\agent-hub.exe.staged" "$INSTDIR\agent-hub.exe"
  IfErrors fresh_failed
  IfFileExists "$INSTDIR\agent-hub.exe" success fresh_failed

upgrade:
  ; Shortcuts and registry values are stable across versions and left intact.
  ; Back up each replaced file, and restore all of them on any failed rename.
  Call CheckExecutableAvailable
  ClearErrors
  Rename "$INSTDIR\LICENSE" "$INSTDIR\LICENSE.previous"
  IfErrors upgrade_failed
  Rename "$INSTDIR\uninstall.exe" "$INSTDIR\uninstall.exe.previous"
  IfErrors upgrade_failed
  Rename "$INSTDIR\agent-hub.exe" "$INSTDIR\agent-hub.exe.previous"
  IfFileExists "$INSTDIR\agent-hub.exe" upgrade_failed
  IfErrors upgrade_failed
  Rename "$INSTDIR\LICENSE.staged" "$INSTDIR\LICENSE"
  IfErrors upgrade_failed
  Rename "$INSTDIR\uninstall.exe.staged" "$INSTDIR\uninstall.exe"
  IfErrors upgrade_failed
  Rename "$INSTDIR\agent-hub.exe.staged" "$INSTDIR\agent-hub.exe"
  IfErrors upgrade_failed
  IfFileExists "$INSTDIR\agent-hub.exe" 0 upgrade_failed
  ; A registry flag makes backup cleanup retryable without treating an
  ; unrelated user file as a journal. Failure before the flag is written
  ; leaves ambiguous backups for manual recovery.
  ClearErrors
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UpgradeCommitted" "committed"
  IfErrors rollback_failed
  Call RemoveBackups
  IfErrors backup_failed
  Goto success

backup_failed:
  MessageBox MB_OK|MB_ICONSTOP "Installation files are present, but old backup files could not be removed. Close programs using files in $INSTDIR and run the installer again to finish cleanup." /SD IDOK
  SetErrorLevel 1
  Abort
upgrade_failed:
  ; Only replace files for which a backup exists; marker is never changed.
  IfFileExists "$INSTDIR\agent-hub.exe.previous" 0 restore_license
  Delete "$INSTDIR\agent-hub.exe"
  IfFileExists "$INSTDIR\agent-hub.exe" rollback_failed
  ClearErrors
  Rename "$INSTDIR\agent-hub.exe.previous" "$INSTDIR\agent-hub.exe"
  IfErrors rollback_failed
restore_license:
  IfFileExists "$INSTDIR\LICENSE.previous" 0 restore_uninstaller
  Delete "$INSTDIR\LICENSE"
  ClearErrors
  Rename "$INSTDIR\LICENSE.previous" "$INSTDIR\LICENSE"
  IfErrors rollback_failed
restore_uninstaller:
  IfFileExists "$INSTDIR\uninstall.exe.previous" 0 restored
  Delete "$INSTDIR\uninstall.exe"
  ClearErrors
  Rename "$INSTDIR\uninstall.exe.previous" "$INSTDIR\uninstall.exe"
  IfErrors rollback_failed
restored:
  Call RemoveStaged
  MessageBox MB_OK|MB_ICONSTOP "The upgrade failed; the previous files were restored. Close agent-hub and try again." /SD IDOK
  SetErrorLevel 1
  Abort
rollback_failed:
  MessageBox MB_OK|MB_ICONSTOP "The upgrade failed and automatic recovery could not finish. Keep the .previous files in $INSTDIR for recovery." /SD IDOK
  SetErrorLevel 1
  Abort
fresh_failed:
  ; Only paths and the registry key proved absent before this attempt are removed.
  Delete "$INSTDIR\agent-hub.exe"
  IfFileExists "$INSTDIR\agent-hub.exe" rollback_failed
  Delete "$INSTDIR\agent-hub.install-kind"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  Delete "$ShortcutDir\agent-hub.lnk"
  Delete "$ShortcutDir\Uninstall agent-hub.lnk"
  RMDir "$ShortcutDir"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  Call RemoveStaged
  RMDir "$INSTDIR"
  MessageBox MB_OK|MB_ICONSTOP "The installation failed; its files and shortcuts were removed." /SD IDOK
  SetErrorLevel 1
  Abort
stage_failed:
  Call RemoveStaged
  MessageBox MB_OK|MB_ICONSTOP "Could not stage the installer files in $INSTDIR." /SD IDOK
  SetErrorLevel 1
  Abort
recovery_staged:
  MessageBox MB_OK|MB_ICONSTOP "Recovery is blocked by $INSTDIR\uninstall-recovery.exe.staged. Inspect that file and resolve it manually before retrying." /SD IDOK
  SetErrorLevel 1
  Abort
recovery:
  MessageBox MB_OK|MB_ICONSTOP "An existing or incomplete installation was found in $INSTDIR. Resolve it before installing." /SD IDOK
  SetErrorLevel 1
  Abort
success:
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  IfFileExists "$INSTDIR\uninstall-recovery.exe.staged" un_recovery_staged
  ClearErrors
  ReadRegStr $1 HKCU "${UNINSTALL_KEY}" "InstallLocation"
  IfErrors cleanup_failed
  StrCmp $1 $INSTDIR 0 cleanup_failed
  ReadRegStr $1 HKCU "${UNINSTALL_KEY}" "UninstallString"
  IfErrors cleanup_failed
  StrCmp $1 '"$INSTDIR\uninstall.exe"' 0 cleanup_failed
  ClearErrors
  ReadRegStr $1 HKCU "${UNINSTALL_KEY}" "UninstallPending"
  IfErrors uninstall_identity_verified
  StrCmp $1 "pending" 0 cleanup_failed
uninstall_identity_verified:
  ; An incomplete upgrade must be recovered before uninstall can claim success.
  ClearErrors
  ReadRegStr $1 HKCU "${UNINSTALL_KEY}" "UpgradeCommitted"
  IfErrors no_upgrade_flag
  Goto remnants
no_upgrade_flag:
  IfFileExists "$INSTDIR\agent-hub.exe.previous" remnants
  IfFileExists "$INSTDIR\LICENSE.previous" remnants
  IfFileExists "$INSTDIR\uninstall.exe.previous" remnants
  IfFileExists "$INSTDIR\agent-hub.exe.staged" remnants
  IfFileExists "$INSTDIR\LICENSE.staged" remnants
  IfFileExists "$INSTDIR\uninstall.exe.staged" remnants
  IfFileExists "$INSTDIR\agent-hub.install-kind.staged" remnants
  IfFileExists "$INSTDIR\agent-hub.exe" 0 remove
  System::Call 'kernel32::CreateFileW(w "$INSTDIR\agent-hub.exe", i 0x80000000, i 0, p 0, i 3, i 0x80, p 0) p.r0'
  IntCmp $0 -1 locked
  System::Call 'kernel32::CloseHandle(p r0)'
  Delete "$INSTDIR\agent-hub.exe"
  IfFileExists "$INSTDIR\agent-hub.exe" locked
remove:
  ; Keep the marker until the executable is gone, and keep the uninstaller
  ; and registration until all other owned files have been removed.
  Delete "$INSTDIR\LICENSE"
  IfFileExists "$INSTDIR\LICENSE" cleanup_failed
  Delete "$ShortcutDir\agent-hub.lnk"
  IfFileExists "$ShortcutDir\agent-hub.lnk" cleanup_failed
  Delete "$ShortcutDir\Uninstall agent-hub.lnk"
  IfFileExists "$ShortcutDir\Uninstall agent-hub.lnk" cleanup_failed
  Delete "$INSTDIR\agent-hub.install-kind"
  IfFileExists "$INSTDIR\agent-hub.install-kind" cleanup_failed
  ; This single registry value is written before deleting the executable.
  ; Deleting the key later removes the flag without a separate crash window.
  ClearErrors
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallPending" "pending"
  IfErrors cleanup_failed
  Delete "$INSTDIR\uninstall.exe"
  IfFileExists "$INSTDIR\uninstall.exe" cleanup_failed
  ClearErrors
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  IfErrors registry_failed
  RMDir "$ShortcutDir"
  ; Deliberately do not recurse: settings, user data, and keyring are retained.
  RMDir "$INSTDIR"
  Goto done
un_recovery_staged:
  MessageBox MB_OK|MB_ICONSTOP "Recovery is blocked by $INSTDIR\uninstall-recovery.exe.staged. Inspect that file and resolve it manually before retrying." /SD IDOK
  SetErrorLevel 1
  Abort
remnants:
  MessageBox MB_OK|MB_ICONSTOP "An incomplete upgrade was found in $INSTDIR. Run the installer again to recover it before uninstalling." /SD IDOK
  SetErrorLevel 1
  Abort
registry_failed:
  MessageBox MB_OK|MB_ICONSTOP "Uninstallation could not remove its registry entry. Run the installer again to restore the uninstaller, then retry." /SD IDOK
  SetErrorLevel 1
  Abort
cleanup_failed:
  MessageBox MB_OK|MB_ICONSTOP "Uninstallation could not remove all installation files or registration. Close programs using them and retry from the uninstall entry." /SD IDOK
  SetErrorLevel 1
  Abort
locked:
  MessageBox MB_OK|MB_ICONSTOP "Close agent-hub and any process using its executable, then run the uninstaller again." /SD IDOK
  SetErrorLevel 1
  Abort
 done:
SectionEnd
