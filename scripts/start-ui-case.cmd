@echo off
setlocal EnableExtensions

rem Start one Neon3 UI case in separate CMD windows.
rem Usage: scripts\start-ui-case.cmd <case> [--projectd]

set "ROOT=%~dp0.."
for %%I in ("%ROOT%") do set "ROOT=%%~fI"
set "UI_PORT=40100"
set "WGPU_PORT=40101"
set "PROJECT_PORT=40102"
set "CASE=%~1"
set "WITH_PROJECTD="
if /I "%~2"=="--projectd" set "WITH_PROJECTD=1"

if "%CASE%"=="" goto :usage
call :case_command "%CASE%"
if errorlevel 1 goto :usage

echo Neon3 UI case: %CASE%
echo Root: %ROOT%
echo UI runtime: 127.0.0.1:%UI_PORT%
echo WGPU runtime: 127.0.0.1:%WGPU_PORT%

if defined WITH_PROJECTD (
    start "Neon3 projectd" /D "%ROOT%" cmd /K "cargo run -p neon-projectd -- --server 127.0.0.1:%PROJECT_PORT%"
    call :wait_port %PROJECT_PORT% "projectd"
    if errorlevel 1 exit /B 1
)

start "Neon3 WGPU runtime" /D "%ROOT%" cmd /K "cargo run -p neon-wgpu-runtime -- --window-server 127.0.0.1:%WGPU_PORT% 127.0.0.1:%UI_PORT%"
call :wait_port %WGPU_PORT% "WGPU runtime"
if errorlevel 1 exit /B 1

start "Neon3 UI runtime" /D "%ROOT%" cmd /K "cargo run -p neon-ui-runtime -- --forward-server 127.0.0.1:%UI_PORT% 127.0.0.1:%WGPU_PORT%"
call :wait_port %UI_PORT% "UI runtime"
if errorlevel 1 exit /B 1

start "Neon3 React case: %CASE%" /D "%ROOT%\packages\neon-ui-react-client" cmd /K "npm run %NPM_SCRIPT% -- %UI_PORT%"
exit /B 0

:case_command
set "NPM_SCRIPT="
if /I "%~1"=="terrain" set "NPM_SCRIPT=demo:terrain"
if /I "%~1"=="terrain-generation" set "NPM_SCRIPT=demo:terrain-generation"
if /I "%~1"=="workbench" set "NPM_SCRIPT=demo:workbench"
if /I "%~1"=="workbench-interactive" set "NPM_SCRIPT=demo:workbench:interactive"
if /I "%~1"=="animation" set "NPM_SCRIPT=demo:animation"
if /I "%~1"=="nested-animation" set "NPM_SCRIPT=demo:nested-animation"
if not defined NPM_SCRIPT exit /B 1
exit /B 0

:wait_port
set "WAIT_PORT=%~1"
set "WAIT_NAME=%~2"
for /L %%N in (1,1,30) do (
    powershell -NoProfile -ExecutionPolicy Bypass -Command "$client = New-Object Net.Sockets.TcpClient; try { $client.Connect('127.0.0.1', %WAIT_PORT%); exit 0 } catch { exit 1 } finally { $client.Dispose() }" >NUL 2>&1
    if not errorlevel 1 exit /B 0
    timeout /T 1 /NOBREAK >NUL
)
echo Timed out waiting for %WAIT_NAME% on port %WAIT_PORT%.
exit /B 1

:usage
echo Usage: scripts\start-ui-case.cmd ^<case^> [--projectd]
echo Cases: terrain, terrain-generation, workbench, workbench-interactive, animation, nested-animation
exit /B 2
