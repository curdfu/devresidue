<#
  DevResidue 构建 / 绿色部署脚本（Windows）

  用法：
    pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1 `
         [-Action Build|Portable|Check] [-SkipFrontend] [-SkipTests] [-NonInteractive]

    -Action Build     完整构建（前端 + cargo workspace release + src-tauri release + 测试）
    -Action Portable  先执行 Build，再生成绿色便携包与 zip
    -Action Check     仅检测/报告环境，不构建、不安装
    -SkipFrontend     跳过 npm run build（仍要求 dist/index.html 存在才会构建 src-tauri）
    -SkipTests        跳过 cargo test
    -NonInteractive   禁用一切交互提示；缺失即失败（绝不自动安装）

  说明：
    * Tauri bundle active=false，因此这里手动组装 portable 包，不调用 tauri bundle。
    * 本文件以 UTF-8 + BOM 保存，兼容 pwsh 7 与 powershell.exe(5.1)。
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('Build', 'Portable', 'Check')]
    [string]$Action = 'Build',

    [switch]$SkipFrontend,
    [switch]$SkipTests,
    [switch]$NonInteractive
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$RepoRoot = Split-Path -Parent $PSScriptRoot

function Write-Step { param([string]$Msg) Write-Host ''; Write-Host ('=' * 64); Write-Host "==> $Msg"; Write-Host ('=' * 64) }
function Write-Ok   { param([string]$Msg) Write-Host "[OK]   $Msg" }
function Write-Warn { param([string]$Msg) Write-Host "[WARN] $Msg" }
function Write-Err  { param([string]$Msg) Write-Host "[ERROR] $Msg" }

# ---------------------------------------------------------------------------
# 小工具
# ---------------------------------------------------------------------------
function Resolve-OnPath {
    param([string]$Name, [string[]]$PreferExts)
    $cmd = Get-Command -Name $Name -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $cmd) { return $null }
    $src = $cmd.Source
    if (-not $src) { return $null }
    if ([System.IO.Path]::GetExtension($src).ToLowerInvariant() -eq '.ps1') {
        # npm 常以 npm.ps1 shim 形式存在，优先其同名 .cmd/.exe，便于跨宿主调用
        $base = [System.IO.Path]::GetFileNameWithoutExtension($src)
        $dir = Split-Path -Parent $src
        foreach ($ext in ($PreferExts + @('.exe', '.cmd', '.bat', '.com'))) {
            $alt = Join-Path $dir ($base + $ext)
            if (Test-Path -LiteralPath $alt) { return $alt }
        }
    }
    return $src
}

function Find-ToolPath {
    # 优先当前 PATH；cargo/rustc 再回退到 %USERPROFILE%\.cargo\bin\<name>.exe
    param([string]$Name, [switch]$WithFallback)
    $p = Resolve-OnPath $Name @('.exe')
    if ($p) { return $p }
    if ($WithFallback) {
        $fb = Join-Path $env:USERPROFILE ('.cargo\bin\' + $Name + '.exe')
        if (Test-Path -LiteralPath $fb) { return $fb }
    }
    return $null
}

function Get-Tools {
    $cargo = Find-ToolPath 'cargo' -WithFallback
    $rustc = Find-ToolPath 'rustc' -WithFallback
    $node  = Find-ToolPath 'node'
    $npm   = Resolve-OnPath 'npm' @('.cmd')
    return [pscustomobject]@{ Cargo = $cargo; Rustc = $rustc; Node = $node; Npm = $npm }
}

function Get-VersionString {
    param([string]$FilePath)
    if (-not $FilePath) { return '' }
    try {
        $v = & $FilePath --version 2>&1
        return (($v | Out-String).Trim())
    } catch { return '' }
}

function Update-SessionPath {
    # 安装后刷新当前 session 的 Machine/User PATH
    $m = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $u = [Environment]::GetEnvironmentVariable('Path', 'User')
    $env:Path = ((($m + ';' + $u) -replace ';+', ';').TrimEnd(';'))
}

function Confirm-YesNo {
    # 可靠的 Read-Host Yes/No 询问（不用于 NonInteractive 路径）
    param([string]$Question)
    while ($true) {
        $a = Read-Host "$Question [Y/N]"
        $a = ([string]$a).Trim().ToLowerInvariant()
        if ($a -eq 'y' -or $a -eq 'yes') { return $true }
        if ($a -eq 'n' -or $a -eq 'no')  { return $false }
        Write-Host '请输入 Y（是）或 N（否）。'
    }
}

function Invoke-Native {
    # 外部命令失败立即失败并输出清楚错误
    param([string]$FilePath, [string[]]$Arguments, [string]$WorkingDir = $RepoRoot, [string]$Label)
    if (-not $FilePath) { throw "命令不可用（找不到可执行文件）：$Label" }
    $disp = $Label
    if (-not $disp) { $disp = "$FilePath $($Arguments -join ' ')" }
    Write-Host ''
    Write-Host "RUN> $disp"
    if ($WorkingDir) { Push-Location -LiteralPath $WorkingDir }
    try {
        & $FilePath @Arguments
        $rc = $LASTEXITCODE
        if ($rc -ne 0) {
            Write-Err "命令失败（退出码 $rc）：$disp"
            throw "命令失败（退出码 $rc）：$disp"
        }
    }
    finally {
        if ($WorkingDir) { Pop-Location }
    }
}

function Get-PackageVersion {
    # 优先 [package] version；根 workspace manifest 无 [package] 时回退 [workspace.package]
    $cargo = Join-Path $RepoRoot 'Cargo.toml'
    if (-not (Test-Path -LiteralPath $cargo)) { throw "找不到根 Cargo.toml：$cargo" }
    $section = ''
    $fallback = $null
    foreach ($line in (Get-Content -LiteralPath $cargo)) {
        $trim = $line.Trim()
        if ($trim -match '^\[[^\]]*\]$') {
            $section = ''
            if ($trim -match '^\[(package|workspace\.package)\]$') { $section = $Matches[1] }
            continue
        }
        if ($section -and $trim -match '^version\s*=\s*"([^"]+)"') {
            if ($section -eq 'package') { return $Matches[1] }
            if ($section -eq 'workspace.package' -and -not $fallback) { $fallback = $Matches[1] }
        }
    }
    if ($fallback) { return $fallback }
    throw '无法从根 Cargo.toml 解析版本号（需要 [package] 或 [workspace.package] 的 version）'
}

# ---------------------------------------------------------------------------
# 工具链安装（仅在交互模式下经 winget；NonInteractive / 缺 winget 一律失败）
# ---------------------------------------------------------------------------
function Invoke-WingetInstall {
    param([string]$DisplayName, [string]$WingetId)
    $wg = Resolve-OnPath 'winget' @('.exe')
    if (-not $wg) {
        throw "未找到 winget，无法自动安装 $DisplayName；请手动安装后重新运行。"
    }
    Write-Step "通过 winget 安装 $DisplayName (id=$WingetId)"
    Invoke-Native $wg @('install', '--id', $WingetId, '-e', '--silent', '--accept-package-agreements', '--accept-source-agreements') $RepoRoot "winget install $DisplayName"
    Update-SessionPath
    Write-Ok "winget 安装 $DisplayName 完成，已刷新 PATH"
}

function Ensure-RustToolchain {
    $t = Get-Tools
    if ($t.Cargo -and $t.Rustc) {
        Write-Ok ("Rust 工具链就绪：cargo = {0}" -f $t.Cargo)
        return
    }
    Write-Warn '缺少 Rust 工具链（cargo / rustc）。'
    if ($NonInteractive) {
        throw 'Rust 工具链缺失且处于 NonInteractive 模式：不自动安装。请先手动安装 rustup，或取消 -NonInteractive 以便交互安装。'
    }
    if (-not (Confirm-YesNo '检测到未安装 Rust 工具链（cargo/rustc）。是否通过 winget 自动安装 Rust？')) {
        throw '已取消 Rust 安装，无法继续构建。'
    }
    Invoke-WingetInstall 'Rust' 'Rustlang.Rustup'
    $t = Get-Tools
    if (-not ($t.Cargo -and $t.Rustc)) { throw 'winget 安装后仍无法定位 cargo/rustc，请检查安装与 PATH 后重试。' }
    Write-Ok ("Rust 工具链已就绪：cargo = {0}" -f $t.Cargo)
}

function Ensure-NodeToolchain {
    $t = Get-Tools
    if ($t.Node -and $t.Npm) {
        Write-Ok ("Node/npm 就绪：node = {0}，npm = {1}" -f $t.Node, $t.Npm)
        return
    }
    Write-Warn '缺少 Node.js / npm。'
    if ($NonInteractive) {
        throw 'Node.js/npm 缺失且处于 NonInteractive 模式：不自动安装。请先手动安装 Node.js，或取消 -NonInteractive 以便交互安装。'
    }
    if (-not (Confirm-YesNo '检测到未安装 Node.js/npm。是否通过 winget 自动安装 Node.js？')) {
        throw '已取消 Node.js 安装，无法继续构建。'
    }
    Invoke-WingetInstall 'Node.js LTS' 'OpenJS.NodeJS.LTS'
    $t = Get-Tools
    if (-not ($t.Node -and $t.Npm)) { throw 'winget 安装后仍无法定位 node/npm，请检查安装与 PATH 后重试。' }
    Write-Ok ("Node/npm 已就绪：node = {0}" -f $t.Node)
}

function Ensure-FrontendDeps {
    # 仅 Build/Portable 路径调用；Check 不调用（Check 不安装 node_modules）
    $nm = Join-Path $RepoRoot 'node_modules'
    if (Test-Path -LiteralPath $nm) {
        Write-Ok '前端依赖就绪（node_modules 存在）'
        return
    }
    Write-Warn '缺少前端依赖 node_modules。'
    if ($NonInteractive) {
        throw '缺少 node_modules 且处于 NonInteractive 模式：不会自动安装依赖。请先手动运行 npm install（或 npm ci），再重试。'
    }
    if (-not (Confirm-YesNo '未找到前端依赖 node_modules。是否现在安装依赖？')) {
        throw '已取消前端依赖安装，无法继续构建。'
    }
    $npm = (Get-Tools).Npm
    $lock = Join-Path $RepoRoot 'package-lock.json'
    if (Test-Path -LiteralPath $lock) {
        Write-Step '安装前端依赖（package-lock.json 存在 → npm ci）'
        Invoke-Native $npm @('ci') $RepoRoot 'npm ci'
    }
    else {
        Write-Step '安装前端依赖（无 package-lock.json → npm install）'
        Invoke-Native $npm @('install') $RepoRoot 'npm install'
    }
    if (-not (Test-Path -LiteralPath $nm)) { throw '依赖安装后仍未生成 node_modules，构建中止。' }
    Write-Ok '前端依赖安装完成'
}

# ---------------------------------------------------------------------------
# Check：仅检测/报告环境，不构建、不安装
# ---------------------------------------------------------------------------
function Invoke-EnvironmentCheck {
    Write-Step '环境检查（Check）：仅检测/报告，不构建、不安装'
    $ok = $true

    $isWin = ($env:OS -eq 'Windows_NT')
    if ($isWin) { Write-Ok '操作系统：Windows' } else { Write-Err '操作系统：非 Windows（本脚本仅支持 Windows）'; $ok = $false }

    Write-Ok ("PowerShell 版本：{0}" -f $PSVersionTable.PSVersion.ToString())

    $t = Get-Tools
    if ($t.Cargo) { Write-Ok ("cargo 就绪：{0}" -f $t.Cargo) }       else { Write-Err 'cargo 缺失（Check 不自动安装）'; $ok = $false }
    if ($t.Rustc) { Write-Ok ("rustc 就绪：{0}" -f $t.Rustc) }       else { Write-Err 'rustc 缺失（Check 不自动安装）'; $ok = $false }
    if ($t.Node)  { Write-Ok ("node 就绪：{0}" -f $t.Node) }         else { Write-Err 'node 缺失（Check 不自动安装）'; $ok = $false }
    if ($t.Npm)   { Write-Ok ("npm 就绪：{0}" -f $t.Npm) }           else { Write-Err 'npm 缺失（Check 不自动安装）'; $ok = $false }

    $nm = Join-Path $RepoRoot 'node_modules'
    if (Test-Path -LiteralPath $nm) { Write-Ok '前端依赖：node_modules 存在' }
    else                            { Write-Warn '前端依赖：node_modules 缺失（Check 不安装；Build 时交互安装）' }

    $distHtml = Join-Path $RepoRoot 'dist\index.html'
    if (Test-Path -LiteralPath $distHtml) { Write-Ok '前端产物：dist/index.html 存在' }
    else                                  { Write-Warn '前端产物：dist/index.html 缺失（运行 Build 生成）' }

    if ($ok) { Write-Ok '环境检查通过（Rust + Node/npm 就绪）' }
    else     { Write-Err '环境检查未通过：存在缺失工具（见上）。可用 -Action Build 进入交互安装，或先手动安装。' }
    return $ok
}

# ---------------------------------------------------------------------------
# Build：前端 + 工作区 release + src-tauri release + 测试
# ---------------------------------------------------------------------------
function Invoke-Build {
    Write-Step '构建开始（Build）'

    # 环境/工具链就绪（缺失时仅在交互模式经 winget 安装）
    Ensure-RustToolchain
    Ensure-NodeToolchain
    Ensure-FrontendDeps

    $frontReady = $false
    if ($SkipFrontend) {
        Write-Warn '已通过 -SkipFrontend 跳过前端构建'
        $distHtml = Join-Path $RepoRoot 'dist\index.html'
        if (Test-Path -LiteralPath $distHtml) {
            Write-Ok 'dist/index.html 已存在，将按现有前端产物构建 src-tauri'
            $frontReady = $true
        }
        else {
            Write-Warn 'dist/index.html 不存在 → 跳过 src-tauri（Tauri shell）构建'
        }
    }
    else {
        $npm = (Get-Tools).Npm
        if (-not $npm) { throw '无法定位 npm，无法构建前端。' }
        Write-Step '前端构建（npm run build）'
        Invoke-Native $npm @('run', 'build') $RepoRoot 'npm run build'
        $frontReady = $true
    }

    $cargo = (Get-Tools).Cargo
    if (-not $cargo) { throw '无法定位 cargo，无法构建 Rust。' }

    Write-Step '工作区 release 构建：cargo build --workspace --release --locked'
    Invoke-Native $cargo @('build', '--workspace', '--release', '--locked') $RepoRoot 'cargo build --workspace --release --locked'

    if ($frontReady) {
        $manifest = Join-Path $RepoRoot 'src-tauri\Cargo.toml'
        if (-not (Test-Path -LiteralPath $manifest)) { throw "找不到 src-tauri 清单：$manifest" }
        Write-Step 'Tauri shell release 构建：cargo build --manifest-path src-tauri/Cargo.toml --release --locked --features custom-protocol'
        Invoke-Native $cargo @('build', '--manifest-path', $manifest, '--release', '--locked', '--features', 'custom-protocol') $RepoRoot 'cargo build --manifest-path src-tauri/Cargo.toml --release --locked --features custom-protocol'
    }

    if ($SkipTests) {
        Write-Warn '已通过 -SkipTests 跳过测试'
    }
    else {
        Write-Step '工作区测试：cargo test --workspace --locked'
        Invoke-Native $cargo @('test', '--workspace', '--locked') $RepoRoot 'cargo test --workspace --locked'

        $manifest = Join-Path $RepoRoot 'src-tauri\Cargo.toml'
        if (Test-Path -LiteralPath $manifest) {
            Write-Step 'Tauri shell 测试：cargo test --manifest-path src-tauri/Cargo.toml --locked --features custom-protocol'
            Invoke-Native $cargo @('test', '--manifest-path', $manifest, '--locked', '--features', 'custom-protocol') $RepoRoot 'cargo test --manifest-path src-tauri/Cargo.toml --locked --features custom-protocol'
        }
        else {
            Write-Warn "未找到 $manifest，跳过 src-tauri 测试"
        }
    }

    Write-Ok '构建完成（Build）'
}

# ---------------------------------------------------------------------------
# Portable：完整 Build 后组装绿色便携包并压缩为 zip
# ---------------------------------------------------------------------------
function Invoke-Portable {
    Write-Step '生成绿色部署包（Portable）'

    $version = Get-PackageVersion
    $dirName = "DevResidue-$version-windows-x64"
    $portRoot = Join-Path $RepoRoot 'dist-portable'
    $pkgDir = Join-Path $portRoot $dirName
    $zip = Join-Path $portRoot ($dirName + '.zip')

    # 仅删除自己即将重新生成的同名目录及 ZIP；不动 dist-portable 中其它文件
    if (Test-Path -LiteralPath $pkgDir) { Remove-Item -LiteralPath $pkgDir -Recurse -Force }
    if (Test-Path -LiteralPath $zip)    { Remove-Item -LiteralPath $zip -Force }

    # 源文件
    $gui   = Join-Path $RepoRoot 'src-tauri\target\release\devresidue-app.exe'
    $cli   = Join-Path $RepoRoot 'target\release\devresidue.exe'
    $rules = Join-Path $RepoRoot 'resources\rules'

    # 每个必要源文件均需存在，否则失败
    $missing = @()
    if (-not (Test-Path -LiteralPath $gui))   { $missing += $gui }
    if (-not (Test-Path -LiteralPath $cli))   { $missing += $cli }
    if (-not (Test-Path -LiteralPath $rules)) { $missing += $rules }
    if ($missing.Count -gt 0) {
        throw "便携包所需源文件缺失（请先完成 Build）：`n" + ($missing -join "`n")
    }

    New-Item -ItemType Directory -Path $pkgDir -Force | Out-Null

    # GUI（DevResidue.exe）+ CLI（devresidue-cli.exe）。
    # 注意：Windows 文件系统大小写不敏感，CLI 不可命名为 devresidue.exe
    #（与 DevResidue.exe 仅在大小写不同，会覆盖 GUI）。因此 CLI 使用 devresidue-cli.exe。
    Copy-Item -LiteralPath $gui -Destination (Join-Path $pkgDir 'DevResidue.exe') -Force
    Copy-Item -LiteralPath $cli -Destination (Join-Path $pkgDir 'devresidue-cli.exe') -Force
    Write-Ok '已复制 GUI（DevResidue.exe）与 CLI（devresidue-cli.exe）'

    # 完整 resources/rules/
    $resRules = Join-Path $pkgDir 'resources\rules'
    New-Item -ItemType Directory -Path $resRules -Force | Out-Null
    Copy-Item -Path (Join-Path $rules '*') -Destination $resRules -Recurse -Force
    Write-Ok '已复制 resources/rules（完整规则集）'

    # README.md（存在则复制）
    $readme = Join-Path $RepoRoot 'README.md'
    if (Test-Path -LiteralPath $readme) {
        Copy-Item -LiteralPath $readme -Destination (Join-Path $pkgDir 'README.md') -Force
        Write-Ok '已复制 README.md'
    }

    # LICENSE：根存在则复制为 LICENSE.txt；否则写入项目元数据声明
    $licDest = Join-Path $pkgDir 'LICENSE.txt'
    $licRoot = $null
    foreach ($cand in @('LICENSE', 'LICENSE.txt', 'LICENSE.md', 'LICENCE', 'LICENCE.txt')) {
        $p = Join-Path $RepoRoot $cand
        if (Test-Path -LiteralPath $p) { $licRoot = $p; break }
    }
    if ($licRoot) {
        Copy-Item -LiteralPath $licRoot -Destination $licDest -Force
        Write-Ok ("已复制根许可证（{0}）为 LICENSE.txt" -f (Split-Path -Leaf $licRoot))
    }
    else {
        $meta = @(
            'DevResidue License'
            ''
            '本软件项目元数据声明的许可证为 MIT（见根 Cargo.toml [workspace.package] license="MIT"'
            '及 workspace 内各 crate 的 license 元数据）。'
            ''
            '注意：当前便携包内暂未附带完整的 MIT 许可证全文。'
            '正式发布前，请将 MIT 许可证完整文本放入本文件（LICENSE.txt）后再对外分发。'
            ''
        )
        [System.IO.File]::WriteAllLines($licDest, $meta, [System.Text.UTF8Encoding]::new($false))
        Write-Warn '根目录无 LICENSE 文件：已在包内写入 MIT 元数据声明（发布前请补充完整许可证文本）'
    }

    # Data 目录（便携运行数据），以空 .gitkeep 占位
    $dataDir = Join-Path $pkgDir 'Data'
    New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
    New-Item -ItemType File -Path (Join-Path $dataDir '.gitkeep') -Force | Out-Null

    # 压缩为同名 zip（内容与 pkgDir 目录一致）
    Write-Step "压缩便携包：$zip"
    Compress-Archive -Path (Join-Path $pkgDir '*') -DestinationPath $zip -CompressionLevel Optimal -Force
    if (-not (Test-Path -LiteralPath $zip)) { throw "便携包 zip 生成失败：$zip" }

    $size = '{0:N2} MB' -f ((Get-Item -LiteralPath $zip).Length / 1MB)
    Write-Ok "便携包目录：$pkgDir"
    Write-Ok "便携包 ZIP ：$zip（$size）"
}

# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------
try {
    switch ($Action) {
        'Check' {
            if (-not (Invoke-EnvironmentCheck)) { throw '环境检查未通过（存在缺失工具）。' }
        }
        'Build' {
            Invoke-Build
        }
        'Portable' {
            Invoke-Build
            Invoke-Portable
        }
    }
    Write-Ok "流程完成（Action=$Action），退出码 0"
    exit 0
}
catch {
    Write-Err "错误：$($_.Exception.Message)"
    if ($_.ScriptStackTrace) { Write-Host $_.ScriptStackTrace }
    Write-Host ''
    Write-Err "流程失败（Action=$Action），退出码 1"
    exit 1
}
