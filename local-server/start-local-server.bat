@echo off
setlocal

cd /d "%~dp0"
title 609 Local Server

echo.
echo ==========================================
echo   609 Local Server
echo ==========================================
echo.

if not exist ".env" (
  echo [ERROR] Missing .env file:
  echo         %cd%\.env
  echo.
  pause
  exit /b 1
)

set "EXE_PATH=%cd%\local-server.exe"
if not exist "%EXE_PATH%" set "EXE_PATH=%cd%\target\release\local-server.exe"
if not exist "%EXE_PATH%" set "EXE_PATH=%cd%\target\debug\local-server.exe"

if exist "%EXE_PATH%" (
  echo [INFO] Found compiled executable:
  echo        %EXE_PATH%
  echo [INFO] Starting local print server...
  echo [INFO] Public URL: http://127.0.0.1:8788
  echo [INFO] Admin  URL: http://127.0.0.1:8789/admin
  echo.
  "%EXE_PATH%"
  echo.
  echo [INFO] Server exited.
  pause
  exit /b %errorlevel%
)

where cargo >nul 2>nul
if errorlevel 1 (
  echo [ERROR] No compiled executable was found and cargo is not installed.
  echo [ERROR] Expected one of these paths:
  echo         %cd%\local-server.exe
  echo         %cd%\target\release\local-server.exe
  echo         %cd%\target\debug\local-server.exe
  echo.
  pause
  exit /b 1
)

echo [INFO] No compiled executable was found.
echo [INFO] Falling back to cargo run --release ...
echo [INFO] Public URL: http://127.0.0.1:8788
echo [INFO] Admin  URL: http://127.0.0.1:8789/admin
echo.

cargo run --release

echo.
echo [INFO] Server exited.
pause
exit /b %errorlevel%
