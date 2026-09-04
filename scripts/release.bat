@echo off
rem Local release: release.bat -Patch | -Minor | -Major | -V 0.3.0 [-SkipTests] [-NoPublish]
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0release.ps1" %*
