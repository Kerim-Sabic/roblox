@echo off
setlocal
chcp 65001 >nul
cd /d "%~dp0"
title NectarPilot

echo.
echo   NectarPilot - safe Bee Swarm automation dashboard
echo   Starting the new Tauri interface...
echo.

if not exist "Cargo.toml" goto packaged

where pnpm >nul 2>nul
if errorlevel 1 (
  echo ERROR: pnpm is required to run this development checkout.
  echo Install Node.js 22 and pnpm 10, then run START.bat again.
  pause
  exit /b 1
)

call pnpm dev
if errorlevel 1 (
  echo.
  echo NectarPilot did not start. Review the error above, then run:
  echo   pnpm install
  echo   pnpm check
  pause
  exit /b 1
)
exit /b 0

:packaged
if exist "NectarPilot.exe" (
  start "" "%~dp0NectarPilot.exe"
  exit /b 0
)
if exist "nectarpilot.exe" (
  start "" "%~dp0nectarpilot.exe"
  exit /b 0
)

echo ERROR: NectarPilot.exe was not found.
echo Re-extract the complete portable ZIP or reinstall NectarPilot.
pause
exit /b 1
