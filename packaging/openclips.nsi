; NSIS installer for OpenClips.
;
; Build the staged folder first:
;   powershell -File scripts\package.ps1 -BundleRuntime
; then compile this script with NSIS 3:
;   makensis packaging\openclips.nsi
; The installer ships the executable together with the GStreamer runtime
; staged by the packaging script, so nothing else has to be installed. It
; installs per user (no administrator prompt) under %LOCALAPPDATA%\Programs.

!ifndef VERSION
  !define VERSION "0.1.0"
!endif
!define APP_NAME "OpenClips"
!define APP_EXE "openclips.exe"
!define STAGE "..\dist\${APP_NAME}-${VERSION}-win64"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

Unicode true
SetCompressor /SOLID lzma
Name "${APP_NAME}"
OutFile "..\dist\${APP_NAME}-${VERSION}-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\${APP_NAME}"
InstallDirRegKey HKCU "Software\${APP_NAME}" "InstallDir"
RequestExecutionLevel user
BrandingText "${APP_NAME} ${VERSION}"

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"

!define MUI_ABORTWARNING
!define MUI_ICON "..\crates\app\assets\icon.ico"
!define MUI_UNICON "..\crates\app\assets\icon.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "$(RunText)"

; The language dialog preselects the Windows display language; the choice
; is remembered for the uninstaller and handed to the app for its first
; start (the app reads "Language" once, when it has no config file yet).
!define MUI_LANGDLL_REGISTRY_ROOT "HKCU"
!define MUI_LANGDLL_REGISTRY_KEY "Software\${APP_NAME}"
!define MUI_LANGDLL_REGISTRY_VALUENAME "InstallerLanguage"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "..\LICENSE"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "Spanish"
!insertmacro MUI_LANGUAGE "French"
!insertmacro MUI_LANGUAGE "German"
!insertmacro MUI_LANGUAGE "Russian"
!insertmacro MUI_LANGUAGE "Portuguese"
!insertmacro MUI_LANGUAGE "PortugueseBR"
!insertmacro MUI_LANGUAGE "Italian"

LangString RunText ${LANG_ENGLISH} "Start ${APP_NAME} now"
LangString RunText ${LANG_SPANISH} "Iniciar ${APP_NAME} ahora"
LangString RunText ${LANG_FRENCH} "Lancer ${APP_NAME} maintenant"
LangString RunText ${LANG_GERMAN} "${APP_NAME} jetzt starten"
LangString RunText ${LANG_RUSSIAN} "Запустить ${APP_NAME} сейчас"
LangString RunText ${LANG_PORTUGUESE} "Iniciar o ${APP_NAME} agora"
LangString RunText ${LANG_PORTUGUESEBR} "Iniciar o ${APP_NAME} agora"
LangString RunText ${LANG_ITALIAN} "Avvia ${APP_NAME} adesso"

LangString SecMainName ${LANG_ENGLISH} "${APP_NAME} (required)"
LangString SecMainName ${LANG_SPANISH} "${APP_NAME} (obligatorio)"
LangString SecMainName ${LANG_FRENCH} "${APP_NAME} (requis)"
LangString SecMainName ${LANG_GERMAN} "${APP_NAME} (erforderlich)"
LangString SecMainName ${LANG_RUSSIAN} "${APP_NAME} (обязательно)"
LangString SecMainName ${LANG_PORTUGUESE} "${APP_NAME} (obrigatório)"
LangString SecMainName ${LANG_PORTUGUESEBR} "${APP_NAME} (obrigatório)"
LangString SecMainName ${LANG_ITALIAN} "${APP_NAME} (obbligatorio)"

LangString SecDesktopName ${LANG_ENGLISH} "Desktop shortcut"
LangString SecDesktopName ${LANG_SPANISH} "Acceso directo en el escritorio"
LangString SecDesktopName ${LANG_FRENCH} "Raccourci sur le bureau"
LangString SecDesktopName ${LANG_GERMAN} "Desktopverknüpfung"
LangString SecDesktopName ${LANG_RUSSIAN} "Ярлык на рабочем столе"
LangString SecDesktopName ${LANG_PORTUGUESE} "Atalho no ambiente de trabalho"
LangString SecDesktopName ${LANG_PORTUGUESEBR} "Atalho na área de trabalho"
LangString SecDesktopName ${LANG_ITALIAN} "Collegamento sul desktop"

LangString SecStartupName ${LANG_ENGLISH} "Launch when Windows starts (minimized to the tray)"
LangString SecStartupName ${LANG_SPANISH} "Iniciar con Windows (minimizado en la bandeja)"
LangString SecStartupName ${LANG_FRENCH} "Lancer au démarrage de Windows (réduit dans la zone de notification)"
LangString SecStartupName ${LANG_GERMAN} "Mit Windows starten (minimiert im Infobereich)"
LangString SecStartupName ${LANG_RUSSIAN} "Запускать вместе с Windows (свёрнутым в трей)"
LangString SecStartupName ${LANG_PORTUGUESE} "Iniciar com o Windows (minimizado na bandeja)"
LangString SecStartupName ${LANG_PORTUGUESEBR} "Iniciar com o Windows (minimizado na bandeja)"
LangString SecStartupName ${LANG_ITALIAN} "Avvia con Windows (ridotto nell'area di notifica)"

LangString SecMainDesc ${LANG_ENGLISH} "The application and the GStreamer runtime it needs."
LangString SecMainDesc ${LANG_SPANISH} "La aplicación y el runtime de GStreamer que necesita."
LangString SecMainDesc ${LANG_FRENCH} "L'application et le runtime GStreamer dont elle a besoin."
LangString SecMainDesc ${LANG_GERMAN} "Die Anwendung und die benötigte GStreamer-Laufzeit."
LangString SecMainDesc ${LANG_RUSSIAN} "Приложение и необходимая ему среда GStreamer."
LangString SecMainDesc ${LANG_PORTUGUESE} "A aplicação e o runtime GStreamer de que necessita."
LangString SecMainDesc ${LANG_PORTUGUESEBR} "O aplicativo e o runtime GStreamer de que ele precisa."
LangString SecMainDesc ${LANG_ITALIAN} "L'applicazione e il runtime GStreamer di cui ha bisogno."

LangString SecDesktopDesc ${LANG_ENGLISH} "A shortcut on the desktop."
LangString SecDesktopDesc ${LANG_SPANISH} "Un acceso directo en el escritorio."
LangString SecDesktopDesc ${LANG_FRENCH} "Un raccourci sur le bureau."
LangString SecDesktopDesc ${LANG_GERMAN} "Eine Verknüpfung auf dem Desktop."
LangString SecDesktopDesc ${LANG_RUSSIAN} "Ярлык на рабочем столе."
LangString SecDesktopDesc ${LANG_PORTUGUESE} "Um atalho no ambiente de trabalho."
LangString SecDesktopDesc ${LANG_PORTUGUESEBR} "Um atalho na área de trabalho."
LangString SecDesktopDesc ${LANG_ITALIAN} "Un collegamento sul desktop."

LangString SecStartupDesc ${LANG_ENGLISH} "Start OpenClips with Windows so the replay buffer is always ready. Can be changed later in Settings."
LangString SecStartupDesc ${LANG_SPANISH} "Inicia OpenClips con Windows para que el búfer de repetición esté siempre listo. Se puede cambiar después en Ajustes."
LangString SecStartupDesc ${LANG_FRENCH} "Lance OpenClips avec Windows pour que le tampon de relecture soit toujours prêt. Modifiable plus tard dans les Paramètres."
LangString SecStartupDesc ${LANG_GERMAN} "Startet OpenClips mit Windows, damit der Replay-Puffer immer bereit ist. Kann später in den Einstellungen geändert werden."
LangString SecStartupDesc ${LANG_RUSSIAN} "Запускает OpenClips вместе с Windows, чтобы буфер повтора всегда был готов. Можно изменить позже в настройках."
LangString SecStartupDesc ${LANG_PORTUGUESE} "Inicia o OpenClips com o Windows para que o buffer de repetição esteja sempre pronto. Pode ser alterado depois nas Definições."
LangString SecStartupDesc ${LANG_PORTUGUESEBR} "Inicia o OpenClips com o Windows para que o buffer de replay esteja sempre pronto. Pode ser alterado depois nas Configurações."
LangString SecStartupDesc ${LANG_ITALIAN} "Avvia OpenClips con Windows così il buffer di replay è sempre pronto. Modificabile in seguito nelle Impostazioni."

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP_NAME}"
VIAddVersionKey "FileDescription" "${APP_NAME} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "MIT License"

Function .onInit
  !insertmacro MUI_LANGDLL_DISPLAY
FunctionEnd

Function un.onInit
  !insertmacro MUI_UNGETLANGUAGE
FunctionEnd

; The two letter code the app uses for the language chosen above.
Function LanguageCode
  ${Switch} $LANGUAGE
    ${Case} ${LANG_SPANISH}
      StrCpy $0 "es"
      ${Break}
    ${Case} ${LANG_FRENCH}
      StrCpy $0 "fr"
      ${Break}
    ${Case} ${LANG_GERMAN}
      StrCpy $0 "de"
      ${Break}
    ${Case} ${LANG_RUSSIAN}
      StrCpy $0 "ru"
      ${Break}
    ${Case} ${LANG_PORTUGUESE}
    ${Case} ${LANG_PORTUGUESEBR}
      StrCpy $0 "pt"
      ${Break}
    ${Case} ${LANG_ITALIAN}
      StrCpy $0 "it"
      ${Break}
    ${Default}
      StrCpy $0 "en"
  ${EndSwitch}
FunctionEnd

Function StopRunningApp
  nsExec::ExecToLog 'taskkill /IM ${APP_EXE} /F'
  Pop $0
  Sleep 500
FunctionEnd

Section "$(SecMainName)" SecMain
  SectionIn RO
  Call StopRunningApp
  SetOutPath "$INSTDIR"
  File /r "${STAGE}\*.*"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  WriteRegStr HKCU "Software\${APP_NAME}" "InstallDir" "$INSTDIR"
  ; Silent runs (updates) never showed the dialog, so they keep whatever the
  ; user picked when installing.
  ${IfNot} ${Silent}
    Call LanguageCode
    WriteRegStr HKCU "Software\${APP_NAME}" "Language" "$0"
  ${EndIf}
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${APP_NAME} contributors"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${APP_EXE}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1

  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk" "$INSTDIR\Uninstall.exe"

  ; Started by the app's updater: bring the app back once the files are in.
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/UPDATE" $R1
  ${IfNot} ${Errors}
    Exec '"$INSTDIR\${APP_EXE}"'
  ${EndIf}
SectionEnd

Section "$(SecDesktopName)" SecDesktop
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
SectionEnd

Section /o "$(SecStartupName)" SecStartup
  WriteRegStr HKCU "${RUN_KEY}" "${APP_NAME}" '"$INSTDIR\${APP_EXE}" --minimized'
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecMain} "$(SecMainDesc)"
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "$(SecDesktopDesc)"
  !insertmacro MUI_DESCRIPTION_TEXT ${SecStartup} "$(SecStartupDesc)"
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  nsExec::ExecToLog 'taskkill /IM ${APP_EXE} /F'
  Pop $0
  Sleep 500
  Delete "$DESKTOP\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk"
  RMDir "$SMPROGRAMS\${APP_NAME}"
  DeleteRegValue HKCU "${RUN_KEY}" "${APP_NAME}"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegKey HKCU "Software\${APP_NAME}"
  ; Only the installed files go; clips, settings and logs stay where they are.
  RMDir /r "$INSTDIR\gstreamer"
  RMDir /r "$INSTDIR\obs-capture"
  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
SectionEnd
