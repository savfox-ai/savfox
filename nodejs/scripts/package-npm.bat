@echo off
setlocal enabledelayedexpansion

set VERSION=%1
if "%VERSION%"=="" set VERSION=0.3.0

set SCRIPT_DIR=%~dp0
set PROJECT_ROOT=%SCRIPT_DIR%..\..\
set DIST_DIR=%PROJECT_ROOT%dist

echo Packaging npm packages for version %VERSION%...

set PACKAGE_DIR=%PROJECT_ROOT%npm-packages
if exist "%PACKAGE_DIR%" rmdir /s /q "%PACKAGE_DIR%"
mkdir "%PACKAGE_DIR%"

echo.
echo Creating main package...
set MAIN_PKG_DIR=%PACKAGE_DIR%\savfox
mkdir "%MAIN_PKG_DIR%\bin"
mkdir "%MAIN_PKG_DIR%\vendor"

copy "%SCRIPT_DIR%..\package.json" "%MAIN_PKG_DIR%\"

powershell -Command "(Get-Content '%MAIN_PKG_DIR%\package.json') -replace '\"version\": \"[^\"]*\"', '\"version\": \"%VERSION%\"' | Set-Content '%MAIN_PKG_DIR%\package.json'"

copy "%SCRIPT_DIR%..\bin\savfox.js" "%MAIN_PKG_DIR%\bin\"

if exist "%DIST_DIR%" (
    xcopy "%DIST_DIR%\*" "%MAIN_PKG_DIR%\vendor\" /E /I /Y
)

echo.
echo Creating platform-specific packages...

set TARGET=x86_64-pc-windows-msvc
set PLATFORM=win32-x64
echo Creating package for %PLATFORM% (%TARGET%)...

set PLATFORM_PKG_DIR=%PACKAGE_DIR%\savfox-%PLATFORM%
mkdir "%PLATFORM_PKG_DIR%\vendor\%TARGET%"

(
echo {
echo   "name": "@savfox/savfox-%PLATFORM%",
echo   "version": "%VERSION%",
echo   "license": "MIT OR Apache-2.0",
echo   "type": "module",
echo   "os": ["win32"],
echo   "cpu": ["x64"],
echo   "repository": {
echo     "type": "git",
echo     "url": "git+https://github.com/savfox-ai/savfox.git",
echo     "directory": "nodejs"
echo   },
echo   "description": "Platform-specific binary for Savfox CLI (%PLATFORM%)",
echo   "optionalDependencies": {}
echo }
) > "%PLATFORM_PKG_DIR%\package.json"

if exist "%DIST_DIR%\%TARGET%" (
    xcopy "%DIST_DIR%\%TARGET%\*" "%PLATFORM_PKG_DIR%\vendor\%TARGET%\" /E /I /Y
)

echo.
echo =========================================
echo Packaging complete!
echo =========================================
echo Packages are available in: %PACKAGE_DIR%
echo.
echo To publish, run:
echo   cd %PACKAGE_DIR%\savfox && npm publish
echo   cd %PACKAGE_DIR%\savfox-win32-x64 && npm publish
