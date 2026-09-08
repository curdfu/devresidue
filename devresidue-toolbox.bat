@echo off
setlocal EnableExtensions
cd /d "%~dp0"

set "PATH=%PATH%;%USERPROFILE%\.cargoin"

echo.
echo ==========================================
echo   DevResidue 工具箱
echo ==========================================
echo.
echo   [1] 打包源码   (devresidue-src-日期.zip)
echo   [2] 清理仓库   (构建产物/生成物)
echo   [3] 深度清理   (含绿色版与 zip)
echo   [0] 退出
echo.
choice /C 1230 /N /M "请选择 [1/2/3/0]: "
if errorlevel 255 goto :end
if errorlevel 4 goto :end
if errorlevel 3 goto :clean_full
if errorlevel 2 goto :clean
if errorlevel 1 goto :package
goto :end

:package
echo.
echo --- 打包源码 ---
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0package-src.ps1"
if errorlevel 1 (
    echo.
    echo [失败] 打包未完成，请检查上方错误信息。
) else (
    echo.
    echo [完成] zip 已生成在仓库根目录。
)
pause
goto :end

:clean
echo.
echo --- 清理仓库，保留 dist-portable 与 zip ---
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0clean-repo.ps1"
echo.
echo [完成] 构建产物已清理。
pause
goto :end

:clean_full
echo.
echo --- 深度清理，连绿色版 dist-portable 与源码 zip 一并删除 ---
echo 即将删除 dist-portable 目录（含其中的 Data 数据记录），源码 zip 也会删除。
choice /C YN /N /M "确认继续 [Y/N]: "
if errorlevel 2 goto :end
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0clean-repo.ps1" -Full
echo.
echo [完成] 深度清理完成。
pause
goto :end

:end
endlocal
