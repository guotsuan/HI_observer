@echo off
setlocal
set "RELEASE_ROOT=%~dp0"
if not exist "%RELEASE_ROOT%bin\channelize.exe" set "RELEASE_ROOT=%~dp0..\"
cd /d "%RELEASE_ROOT%"
set "SOAPY_SDR_PLUGIN_PATH=%RELEASE_ROOT%lib\SoapySDR\modules0.8"

set "CENTER_FREQUENCY=1420.405751e6"
if not "%~1"=="" (
    set "CENTER_FREQUENCY=%~1"
    shift
)

bin\channelize.exe -f %CENTER_FREQUENCY% --nch 4096 --lna 5 --mix 5 --vga 5 %*
if errorlevel 1 (
    echo.
    echo channelize exited with an error. Check that the Airspy driver is installed
    echo and that the receiver is connected and not open in another application.
    pause
)
