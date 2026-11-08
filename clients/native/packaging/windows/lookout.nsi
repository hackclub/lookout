; Lookout Native installer (NSIS 3.08+).
;
; Per-user, like the Tauri app's NSIS bundle (installMode currentUser): no
; admin prompt, installs to %LOCALAPPDATA%\Lookout and reuses the Tauri
; installer's registry keys, so it upgrades a Tauri install in place
; (the binary is lookout.exe in both) and the Tauri entry in
; "Installed apps" becomes this one.
;
; Build (paths are Windows paths):
;   makensis -DVERSION=0.4.0 -DSRC_DIR=<dir with lookout.exe> ^
;            -DOUTFILE=<...\Lookout-native_0.4.0_windows-x86_64-setup.exe> lookout.nsi
; Optional: -DICON=<.ico> (default: the Tauri app's icon.ico)
;           -DSIGN_CMD=<script taking one path> signs the uninstaller and the
;            installer while makensis builds them (sign lookout.exe before).
;
; Command line:
;   /S        silent (what the updater uses)
;   /UPDATE   marks an in-app update (accepted; behaves like /S today)
;   /R        start Lookout when the install finishes (updater "Restart")
;   /D=<dir>  install folder (NSIS built-in; must be last)

Unicode true
ManifestDPIAware true
RequestExecutionLevel user
SetCompressor /SOLID lzma

!ifndef VERSION
  !error "pass -DVERSION=x.y.z"
!endif
!ifndef SRC_DIR
  !error "pass -DSRC_DIR=<folder containing lookout.exe>"
!endif
!ifndef OUTFILE
  !define OUTFILE "Lookout-native_${VERSION}_windows-x86_64-setup.exe"
!endif
!ifndef ICON
  !define ICON "${__FILEDIR__}\..\..\..\desktop\src-tauri\icons\icon.ico"
!endif

!define PRODUCT "Lookout"
!define PUBLISHER "Hack Club"
!define EXE "lookout.exe"
; The Tauri bundle's keys (manufacturer = "hackclub" from com.hackclub.lookout).
!define REG_APP "Software\hackclub\Lookout"
!define REG_UNINSTALL "Software\Microsoft\Windows\CurrentVersion\Uninstall\Lookout"
!define REG_PROTOCOL "Software\Classes\lookout"

; VIProductVersion needs four numbers: drop any pre-release suffix.
!searchparse /noerrors "${VERSION}" "" V_MAJOR "." V_MINOR "." V_PATCH_RAW
!searchparse /noerrors "${V_PATCH_RAW}" "" V_PATCH "-"
!ifndef V_PATCH
  !define V_PATCH "${V_PATCH_RAW}"
!endif

!ifdef SIGN_CMD
  !finalize '"${SIGN_CMD}" "%1"' = 0
  !uninstfinalize '"${SIGN_CMD}" "%1"' = 0
!endif

Name "${PRODUCT}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\${PRODUCT}"
InstallDirRegKey HKCU "${REG_APP}" ""
BrandingText "${PRODUCT} ${VERSION}"

VIProductVersion "${V_MAJOR}.${V_MINOR}.${V_PATCH}.0"
VIAddVersionKey "ProductName" "${PRODUCT}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${PRODUCT} installer"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "© Hack Club, a 501(c)(3) nonprofit (EIN: 81-2908499)"

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Open Lookout"

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Var Relaunch

; Wait (up to ~15 s) for a running Lookout to exit, then end it. The updater
; starts this installer as it quits, so normally the wait is short.
!macro WaitForLookout UN
Function ${UN}WaitForLookout
  StrCpy $1 0
  ${Do}
    IfFileExists "$INSTDIR\${EXE}" 0 done
    ClearErrors
    FileOpen $0 "$INSTDIR\${EXE}" a
    ${IfNot} ${Errors}
      FileClose $0
      Goto done
    ${EndIf}
    IntOp $1 $1 + 1
    ${If} $1 > 60
      nsExec::Exec 'taskkill /F /IM ${EXE}'
      Pop $0
      Sleep 1000
      Goto done
    ${EndIf}
    Sleep 250
  ${Loop}
  done:
FunctionEnd
!macroend
!insertmacro WaitForLookout ""
!insertmacro WaitForLookout "un."

Function .onInit
  StrCpy $Relaunch 0
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/R" $R1
  ${IfNot} ${Errors}
    StrCpy $Relaunch 1
  ${EndIf}
FunctionEnd

Section "Install"
  Call WaitForLookout
  SetOutPath "$INSTDIR"
  SetOverwrite on
  File "${SRC_DIR}\${EXE}"
  File "/oname=lookout.ico" "${ICON}"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; Start menu shortcut (the Tauri installer's name, so it's replaced, not doubled).
  CreateShortCut "$SMPROGRAMS\${PRODUCT}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\lookout.ico" 0

  ; lookout:// links (session links and program pairing callbacks).
  WriteRegStr HKCU "${REG_PROTOCOL}" "" "URL:Lookout"
  WriteRegStr HKCU "${REG_PROTOCOL}" "URL Protocol" ""
  WriteRegStr HKCU "${REG_PROTOCOL}\DefaultIcon" "" "$\"$INSTDIR\lookout.ico$\""
  WriteRegStr HKCU "${REG_PROTOCOL}\shell\open\command" "" "$\"$INSTDIR\${EXE}$\" $\"%1$\""

  WriteRegStr HKCU "${REG_APP}" "" "$INSTDIR"
  WriteRegStr HKCU "${REG_UNINSTALL}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKCU "${REG_UNINSTALL}" "DisplayIcon" "$\"$INSTDIR\lookout.ico$\""
  WriteRegStr HKCU "${REG_UNINSTALL}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${REG_UNINSTALL}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${REG_UNINSTALL}" "URLInfoAbout" "https://lookout.hackclub.com"
  WriteRegStr HKCU "${REG_UNINSTALL}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${REG_UNINSTALL}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegStr HKCU "${REG_UNINSTALL}" "QuietUninstallString" "$\"$INSTDIR\uninstall.exe$\" /S"
  WriteRegDWORD HKCU "${REG_UNINSTALL}" "NoModify" 1
  WriteRegDWORD HKCU "${REG_UNINSTALL}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${REG_UNINSTALL}" "EstimatedSize" "$0"

  ${If} $Relaunch = 1
    Exec '"$INSTDIR\${EXE}"'
  ${EndIf}
SectionEnd

Section "Uninstall"
  Call un.WaitForLookout
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\lookout.ico"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${PRODUCT}.lnk"
  Delete "$DESKTOP\${PRODUCT}.lnk"
  DeleteRegKey HKCU "${REG_PROTOCOL}"
  DeleteRegKey HKCU "${REG_UNINSTALL}"
  DeleteRegKey HKCU "${REG_APP}"
  ; User data (%APPDATA%\com.hackclub.lookout, %LOCALAPPDATA%\com.hackclub.lookout) is kept.
SectionEnd
