@echo off
setlocal enabledelayedexpansion

set VERSION=%1
if "%VERSION%"=="" set VERSION=0.3.0

set SCRIPT_DIR=%~dp0
set PROJECT_ROOT=%SCRIPT_DIR%..\..\
set DIST_DIR=%PROJECT_ROOT%dist

echo Building Savfox CLI v%VERSION% for Windows...

if exist "%DIST_DIR%" rmdir /s /q "%DIST_DIR%"
mkdir "%DIST_DIR%"

set TARGET=x86_64-pc-windows-msvc
echo.
echo =========================================
echo Building for target: %TARGET%
echo =========================================

set OUTPUT_DIR=%DIST_DIR%\%TARGET%\savfox
mkdir "%OUTPUT_DIR%"

rustup target add %TARGET% 2>nul

cargo build --release --target %TARGET% -p savfox-cli --bin savfox
if errorlevel 1 (
    echo Build failed!
    exit /b 1
)

copy "%PROJECT_ROOT%target\%TARGET%\release\savfox.exe" "%OUTPUT_DIR%\"
echo Built binary at: %OUTPUT_DIR%\savfox.exe

echo.
echo =========================================
echo Build complete!
echo =========================================
echo Binaries are available in: %DIST_DIR%
