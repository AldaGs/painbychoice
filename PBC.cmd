@echo off
rem Double-click launcher for the release editor built with --features libav.
rem Puts FFmpeg's shared DLLs on PATH (appended, so C:\ffmpeg stays first) and starts pbc.exe.
rem Set FFMPEG_DIR to override the SDK location.
setlocal
if not defined FFMPEG_DIR set "FFMPEG_DIR=%USERPROFILE%\.pbc\ffmpeg-sdk\ffmpeg-n9.0-latest-win64-lgpl-shared-9.0"
set "EXE=%~dp0target\release\pbc.exe"
if not exist "%EXE%" (
  echo pbc.exe not found. Build it first:
  echo   cargo build --release -p motion-live --features libav
  pause
  exit /b 1
)
if not exist "%FFMPEG_DIR%\bin\avcodec-*.dll" (
  echo FFmpeg DLLs not found in "%FFMPEG_DIR%\bin". Set FFMPEG_DIR to the FFmpeg shared SDK.
  pause
  exit /b 1
)
set "PATH=%PATH%;%FFMPEG_DIR%\bin"
start "" /D "%~dp0" "%EXE%" %*
