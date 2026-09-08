@echo off
setlocal EnableExtensions
cd /d "%~dp0"

rem 定位 PowerShell 7+（pwsh），否则回退到 powershell.exe
where pwsh >nul 2>nul
if not errorlevel 1 (
    set "PSRUN=pwsh"
) else (
    set "PSRUN=powershell.exe"
)
set "LAST_RC=0"

:menu
cls
echo ============================================================
echo            DevResidue 构建 / 绿色部署（Windows）
echo ============================================================
echo.
echo   [1] 构建 release（前端 + Rust 工作区 + Tauri shell + 测试）
echo   [2] 构建并生成绿色部署包（dist-portable 目录 + zip）
echo   [0] 退出
echo.
choice /C 120 /N /M "请选择 [1/2/0]: "
if errorlevel 3 goto :quit
if errorlevel 2 goto :portable
if errorlevel 1 goto :build
goto :menu

:build
echo.
echo ------------------------------------------------------------
echo   构建 release ...
echo ------------------------------------------------------------
%PSRUN% -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build.ps1" -Action Build
set "LAST_RC=%ERRORLEVEL%"
call :report %LAST_RC%
goto :menu

:portable
echo.
echo ------------------------------------------------------------
echo   构建并生成绿色部署包 ...
echo ------------------------------------------------------------
%PSRUN% -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\build.ps1" -Action Portable
set "LAST_RC=%ERRORLEVEL%"
call :report %LAST_RC%
goto :menu

:report
if "%~1"=="0" (
    echo.
    echo   [成功] 操作完成（退出码 0），详细输出见上方。
) else (
    echo.
    echo   [失败] 操作未完成（退出码 %~1）。
)
echo.
pause
exit /b 0

:quit
endlocal & exit /b %LAST_RC%
