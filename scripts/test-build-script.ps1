# DevResidue build script 测试（先红后绿）
# 约定：
#   * 绝不真实删除 / 安装 / 构建（仅运行 -Action Check 与参数校验失败路径）。
#   * 行为断言：a) Check + NonInteractive 成功(exit 0)且不创建 dist-portable；
#               b) -Action Unsupported 被参数验证拒绝(exit != 0)。
#   * 静态内容断言 build.ps1：CmdletBinding / Build|Portable|Check / 默认 Build /
#     SkipFrontend,SkipeTests 与 NonInteractive / winget+Read-Host 交互安装询问 /
#     package-lock->npm ci 与 npm install / cargo --release --locked /
#     Compress-Archive + dist-portable。
#   * 静态内容断言 build.bat（GBK/cp936 中文菜单）：调用 scripts/build.ps1、
#     -Action Build/-Action Portable、choice 菜单、返回进程退出码。
# 运行：pwsh -NoProfile -File scripts/test-build-script.ps1   （退出码 0=通过）
$ErrorActionPreference = 'Stop'

$script:Pass = 0
$script:Fail = 0

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if ($Condition) { Write-Host "  [PASS] $Message"; $script:Pass++ }
    else            { Write-Host "  [FAIL] $Message"; $script:Fail++ }
}

# --- 定位被测文件 ---
$RepoRoot     = Split-Path -Parent $PSScriptRoot
$BuildPs1     = Join-Path $PSScriptRoot 'build.ps1'
$BuildBat     = Join-Path $RepoRoot 'build.bat'
$TauriManifest = Join-Path $RepoRoot 'src-tauri\Cargo.toml'
$DistPortable = Join-Path $RepoRoot 'dist-portable'

# --- 编码安全的文本读取 ---
function Get-Utf8Text {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    try { return [System.Text.Encoding]::UTF8.GetString([System.IO.File]::ReadAllBytes($Path)) }
    catch { return $null }
}

function Get-GbkText {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    try {
        try { $enc = [System.Text.Encoding]::GetEncoding(936) }
        catch {
            try { Add-Type -AssemblyName System.Text.Encoding.CodePages -ErrorAction Stop } catch {}
            [System.Text.Encoding]::RegisterProvider([System.Text.CodePagesEncodingProvider]::Instance)
            $enc = [System.Text.Encoding]::GetEncoding(936)
        }
        return $enc.GetString([System.IO.File]::ReadAllBytes($Path))
    } catch { return $null }
}

$ps1 = Get-Utf8Text $BuildPs1   # build.ps1（UTF-8 BOM）
$bat = Get-GbkText  $BuildBat   # build.bat（GBK/cp936，与 devresidue-toolbox.bat 一致）
$tm  = Get-Utf8Text $TauriManifest  # src-tauri/Cargo.toml（UTF-8 无 BOM）

Write-Host ''
Write-Host '================ 1) build.ps1 静态约定 ================'
if ($null -eq $ps1) {
    Assert-True $false 'build.ps1 存在且可读（red 阶段预期：文件尚未实现）'
} else {
    Assert-True $true 'build.ps1 存在且可读'
    Assert-True ($ps1 -match '\[CmdletBinding\(\)\]')                       '声明 [CmdletBinding()]'
    Assert-True ($ps1 -match "ValidateSet\(\s*'Build'\s*,\s*'Portable'\s*,\s*'Check'\s*\)") '-Action 参数 ValidateSet 含 Build/Portable/Check'
    Assert-True ($ps1 -match '\$Action\s*=\s*''Build''')                    '-Action 默认值为 Build'
    Assert-True (($ps1 -match 'SkipFrontend') -and ($ps1 -match 'SkipTests') -and ($ps1 -match 'NonInteractive')) '含 SkipFrontend/SkipTests/NonInteractive 参数'
    Assert-True (($ps1 -match 'winget') -and ($ps1 -match 'Read-Host'))     '交互安装询问（winget + Read-Host Yes/No）'
    Assert-True (($ps1 -match 'npm ci') -and ($ps1 -match 'npm install') -and ($ps1 -match 'package-lock')) '前端依赖：package-lock 用 npm ci，否则 npm install'
    Assert-True (($ps1 -match '--release') -and ($ps1 -match '--locked'))    'cargo release 构建（--release --locked）'
    Assert-True (($ps1 -match '\$CargoTargetDir\s*=\s*Join-Path\s+\$RepoRoot\s+''target''') -and ($ps1 -match '\$env:CARGO_TARGET_DIR\s*=\s*\$CargoTargetDir')) 'Cargo 构建统一使用当前仓库 target，避免跨路径缓存污染'
    Assert-True (($ps1 -match '\$gui\s*=\s*Join-Path\s+\$CargoTargetDir') -and ($ps1 -match '\$cli\s*=\s*Join-Path\s+\$CargoTargetDir')) 'Portable GUI/CLI 源文件均从统一 Cargo target 读取'
    Assert-True (($ps1 -match 'RedirectStandardOutput') -and ($ps1 -match 'RedirectStandardError') -and ($ps1 -match 'ReadToEndAsync')) 'heartbeat helper 保留并转发 Cargo stdout/stderr，失败时可见原始错误'
    Assert-True ($ps1 -match 'Compress-Archive')                            '压缩包逻辑（Compress-Archive）'
    Assert-True ($ps1 -match 'dist-portable')                               '便携输出目录 dist-portable'
    # Portable CLI 绿色包文件名：Windows 文件系统大小写不敏感，GUI 与 CLI 目标名
    # 不能仅为大小写不同（否则后者覆盖前者）。CLI 固定为 devresidue-cli.exe。
    # 注：仅精确匹配 “Destination (Join-Path $pkgDir '<name>.exe')” 的复制目标，
    #     避免误把 CLI 源路径 target\release\devresidue.exe 当作目标名。
    Assert-True ($ps1 -match "'DevResidue\.exe'")                            'GUI 复制目标为 DevResidue.exe'
    Assert-True ($ps1 -match "'devresidue-cli\.exe'")                        'CLI 复制目标为 devresidue-cli.exe'
    $destRx = [regex]::new('Destination\s*\(Join-Path\s*\$pkgDir\s*''([^'']+\.exe)''\)')
    $destNames = @($destRx.Matches($ps1) | ForEach-Object { $_.Groups[1].Value })
    $destLowers = @($destNames | ForEach-Object { $_.ToLowerInvariant() })
    $hasGuiAndCli = ($destLowers -contains 'devresidue.exe') -and
                    ($destLowers -contains 'devresidue-cli.exe')
    Assert-True $hasGuiAndCli 'Portable 打包目标含两个大小写不同的文件名：DevResidue.exe + devresidue-cli.exe'
    # 两个复制目标 lower 名互不相同 => Windows 大小写不敏感下不发生覆盖；
    # 且恰为这两个成员（无第三个 .exe 复制目标混入）
    $targetsAreDistinct = ($destLowers.Count -eq 2) -and
                          ($destLowers -contains 'devresidue.exe') -and
                          ($destLowers -contains 'devresidue-cli.exe')
    Assert-True $targetsAreDistinct '打包 .exe 目标恰好两个且 lower 名互异，无大小写覆盖'

    # --- custom-protocol 契约：Tauri 构建/测试必须显式启用 feature ---
    # 纯 cargo release 的 GUI 必须启用 tauri/custom-protocol（嵌入 dist 前端资源，
    # 经 tauri://localhost 提供），否则绿色包回退 devUrl http://localhost:1420
    # 导致首屏“页面无法访问”。构建与测试调用均需显式传 --features custom-protocol，
    # 防止 src-tauri default features 被改动后绿色包失效。
    # （标签文本为 "--features custom-protocol"；参数数组为 '--features', 'custom-protocol'，
    #  二者形态不同，可分别精确断言。）
    Assert-True ($ps1 -match "--features custom-protocol") 'build.ps1 显示文本含 --features custom-protocol'
    # 注：正则须用单引号字符串字面量（内部单引号翻倍），使 $manifest 保持字面文本，
    # 避免双引号内 PowerShell 将 $manifest 当变量插值为空导致误判。
    $tauriBuildArgs = [regex]::new('''build'',\s*''--manifest-path'',\s*\$manifest,\s*''--release'',\s*''--locked'',\s*''--features'',\s*''custom-protocol''')
    $tauriTestArgs  = [regex]::new('''test'',\s*''--manifest-path'',\s*\$manifest,\s*''--locked'',\s*''--features'',\s*''custom-protocol''')
    Assert-True ($tauriBuildArgs.IsMatch($ps1)) 'Tauri release 构建显式传 --features custom-protocol'
    Assert-True ($tauriTestArgs.IsMatch($ps1))  'Tauri test 显式传 --features custom-protocol'

    # --- Tauri release 构建心跳契约（仅用于 Tauri shell release build，不改命令/参数）---
    # 心跳：终端 final link 长时间无输出时的可观测状态（非虚假百分比）。
    Assert-True ($ps1 -match 'Invoke-NativeWithHeartbeat')       '存在专用 heartbeat helper'
    Assert-True ($ps1 -match '\$HeartbeatSeconds\s*=\s*10')       '心跳默认间隔常量 10 秒'
    Assert-True ($ps1 -match 'Elapsed')                            '含已耗时(elapsed)计时输出'
    Assert-True ($ps1 -match "'cargo'")                           '检测 cargo 子进程'
    Assert-True ($ps1 -match "'rustc'")                           '检测 rustc 子进程'
    Assert-True (($ps1 -match "'link'") -and ($ps1 -match "'lld-link'")) '检测 link / lld-link 子进程'
    # 必须明确心跳非百分比
    Assert-True ($ps1 -match '进行中')                             '心跳状态字样“进行中”'
    Assert-True ($ps1 -match '不是.*百分比|百分比.*不是|虚假|并非进度百分比') '明确非虚假百分比说明'
    # Tauri release 调用经 heartbeat helper（保留原命令/参数/features）
    $tauriHbCall = [regex]::new('Invoke-NativeWithHeartbeat[^\r\n]*\$cargo[^\r\n]*''build''[^\r\n]*''--manifest-path''[^\r\n]*\$manifest[^\r\n]*''--release''[^\r\n]*''--locked''[^\r\n]*''--features''[^\r\n]*''custom-protocol''')
    Assert-True ($tauriHbCall.IsMatch($ps1)) 'Tauri release 构建经 heartbeat helper 且保留 --features custom-protocol'
    Assert-True ($ps1 -match '(?m)^function Invoke-NativeWithHeartbeat') 'helper 为独立函数'
    # 失败 exit code 传播：helper 抛错文本含退出码
    Assert-True ($ps1 -match '退出码|exit code|ExitCode')          '失败时传播退出码'
}

Write-Host ''
Write-Host '================ 1b) src-tauri/Cargo.toml custom-protocol 契约 ================'
if ($null -eq $tm) {
    Assert-True $false 'src-tauri/Cargo.toml 存在且可读'
} else {
    Assert-True $true 'src-tauri/Cargo.toml 存在且可读'
    Assert-True ([regex]::IsMatch($tm, '(?m)^\[features\]\s*$')) '含 [features] 段'
    Assert-True ([regex]::IsMatch($tm, '(?m)^default\s*=\s*\["custom-protocol"\]\s*$')) 'default = ["custom-protocol"]'
    Assert-True ([regex]::IsMatch($tm, '(?m)^custom-protocol\s*=\s*\["tauri/custom-protocol"\]\s*$')) 'custom-protocol = ["tauri/custom-protocol"]'
}

Write-Host ''
Write-Host '================ 2) build.bat 静态约定 ================'
if ($null -eq $bat) {
    Assert-True $false 'build.bat 存在且可读（仓库根目录）'
} else {
    Assert-True $true 'build.bat 存在且可读'
    Assert-True ($bat -match 'build\.ps1')                '调用 scripts/build.ps1'
    Assert-True (($bat -match '-Action Build') -and ($bat -match '-Action Portable')) '分别传 -Action Build / -Action Portable'
    Assert-True ($bat -match 'choice')                    '使用 choice 交互菜单'
    Assert-True (($bat -match '构建') -and ($bat -match '绿色') -and ($bat -match '退出')) '中文交互菜单（构建/绿色部署/退出）'
    Assert-True ($bat -match 'exit /b')                   '返回 PowerShell 进程退出码'
}

Write-Host ''
Write-Host '================ 3) 行为断言（非破坏） ================'
$pwshExe = (Get-Command pwsh -ErrorAction SilentlyContinue).Source
if (-not $pwshExe) { $pwshExe = Join-Path $PSHOME 'pwsh.exe' }
Write-Host "  子进程 PowerShell: $pwshExe"

$existedBefore = Test-Path -LiteralPath $DistPortable

# a) Check + NonInteractive：成功且不创建 dist-portable
Write-Host ''
Write-Host '--- 运行 build.ps1 -Action Check -NonInteractive ---'
$out1 = & $pwshExe -NoProfile -File $BuildPs1 -Action Check -NonInteractive 2>&1
$code1 = $LASTEXITCODE
$out1 | ForEach-Object { Write-Host "    $_" }
Write-Host "   [exit] $code1"
Assert-True ($code1 -eq 0) "a) Check + NonInteractive 成功（退出码 0，实际 $code1）"
if (-not $existedBefore) {
    Assert-True (-not (Test-Path -LiteralPath $DistPortable)) 'a) Check 未创建 dist-portable'
} else {
    Write-Host '   [SKIP] dist-portable 事先存在，跳过“未创建”断言'
}

# b) -Action Unsupported 被参数验证拒绝
Write-Host ''
Write-Host '--- 运行 build.ps1 -Action Unsupported -NonInteractive ---'
$out2 = & $pwshExe -NoProfile -File $BuildPs1 -Action Unsupported -NonInteractive 2>&1
$code2 = $LASTEXITCODE
$out2 | ForEach-Object { Write-Host "    $_" }
Write-Host "   [exit] $code2"
Assert-True ($code2 -ne 0) "b) -Action Unsupported 被参数验证拒绝（退出码非 0，实际 $code2）"
if (-not $existedBefore) {
    Assert-True (-not (Test-Path -LiteralPath $DistPortable)) 'b) 校验失败未创建 dist-portable'
}

Write-Host ''
Write-Host '================ 4) 动态 heartbeat helper 测试（真实调用，非破坏） ================'
# 让真实 helper 运行一个短暂的睡眠子进程，验证：心跳输出确实出现、退出码 0；
# 再运行一个非 0 退出码子进程，验证失败被传播。HeartbeatSeconds=1 以加速测试。
$heartbeatScript = @'
param([string]$BuildPs1, [string]$Mode, [string]$ShellPath)
$ErrorActionPreference = 'Stop'
. $BuildPs1   # dot-source：定义 RepoRoot、Write-* 辅助与 Invoke-NativeWithHeartbeat
if ($Mode -eq 'ok') {
    # 瞬时成功进程：验证 helper 正常返回、无心跳刷屏
    $out = Invoke-NativeWithHeartbeat -FilePath $ShellPath -Arguments @('-NoProfile','-Command','exit 0') -WorkingDir $RepoRoot -Label '心跳测试-成功' -HeartbeatSeconds 1 2>&1 | Out-String
    Write-Output "CAPTURED_OUTPUT_BEGIN"
    Write-Output $out
    Write-Output "CAPTURED_OUTPUT_END"
}
elseif ($Mode -eq 'slow') {
    # 睡眠足够久，确保至少跨越一次心跳间隔（1 秒间隔 -> 睡 3 秒）
    $out = Invoke-NativeWithHeartbeat -FilePath $ShellPath -Arguments @('-NoProfile','-Command','Start-Sleep -Seconds 3; exit 0') -WorkingDir $RepoRoot -Label '心跳测试-慢' -HeartbeatSeconds 1 2>&1 | Out-String
    Write-Output "SLOW_OUTPUT_BEGIN"
    Write-Output $out
    Write-Output "SLOW_OUTPUT_END"
}
elseif ($Mode -eq 'fail') {
    try {
        Invoke-NativeWithHeartbeat -FilePath $ShellPath -Arguments @('-NoProfile','-Command','exit 7') -WorkingDir $RepoRoot -Label '心跳测试-失败' -HeartbeatSeconds 1 2>&1 | Out-String
        Write-Output "NO_THROW"
    } catch {
        Write-Output ("THREW: " + $_.Exception.Message)
    }
}
'@
$tmpScript = Join-Path $env:TEMP ("dr-hb-test-" + [guid]::NewGuid().ToString('N') + ".ps1")
[System.IO.File]::WriteAllText($tmpScript, $heartbeatScript, [System.Text.UTF8Encoding]::new($false))
try {
    # ok：成功路径，退出码 0（helper 正常返回，输出含 RUN>）
    $okOut = & $pwshExe -NoProfile -File $tmpScript -BuildPs1 $BuildPs1 -Mode ok -ShellPath $pwshExe 2>&1 | Out-String
    $hasRun = $okOut -match 'RUN>|进行中|等待'
    Assert-True $hasRun '动态-成功进程：helper 正常运行（含 RUN>/心跳字样）'
    # 失败：验证异常被抛出并含退出码
    $failOut = & $pwshExe -NoProfile -File $tmpScript -BuildPs1 $BuildPs1 -Mode fail -ShellPath $pwshExe 2>&1 | Out-String
    $threw = $failOut -match 'THREW:'
    $hasRc  = $failOut -match '7'
    Assert-True ($threw -and $hasRc) '动态-失败进程：非 0 退出码被传播（抛出含 7）'

    # slow：验证至少出现一次 [进行中] 心跳行
    $slowOut = & $pwshExe -NoProfile -File $tmpScript -BuildPs1 $BuildPs1 -Mode slow -ShellPath $pwshExe 2>&1 | Out-String
    $hbLine = ($slowOut -split "`r?`n") | Where-Object { $_ -match '进行中' } | Select-Object -First 1
    if ($hbLine) {
        Write-Host "   [heartbeat sample] $hbLine"
        Assert-True $true '动态-慢进程：心跳行出现（真实 helper 执行）'
    } else {
        Assert-True $false '动态-慢进程：心跳行出现（真实 helper 执行）'
    }
}
finally {
    if (Test-Path -LiteralPath $tmpScript) { Remove-Item -LiteralPath $tmpScript -Force }
}

Write-Host ''
Write-Host "============ 结果：PASS=$($script:Pass)  FAIL=$($script:Fail) ============"
if ($script:Fail -eq 0) { Write-Host '测试通过。'; exit 0 }
else                    { Write-Host '测试失败。'; exit 1 }
