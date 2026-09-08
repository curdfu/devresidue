# DevResidue source packaging script
# Usage: powershell -ExecutionPolicy Bypass -File package-src.ps1
# Output: <repo>/devresidue-src-YYYYMMDD.zip
#
# Included: product sources + docs (spec/plan/review) + engineering configs
#           + resources (rules) + tests
# Excluded: build outputs (target/node_modules/dist), portable dist
#           (dist-portable), reference repo clones (.slim/clonedeps/repos),
#           generated probe bundle (store-probes.bundle.mjs)
# Kept:     .slim/clonedeps.json (dependency source record),
#           .gitignore/.ignore (engineering norm)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $MyInvocation.MyCommand.Path
$date = Get-Date -Format "yyyyMMdd"
$out = Join-Path $repo "devresidue-src-$date.zip"

if (Test-Path $out) { Remove-Item $out -Force }

$topFiles = @(
    "AGENTS.md", ".gitignore", ".ignore",
    "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
    "rustfmt.toml", "clippy.toml",
    "package.json", "package-lock.json",
    "tsconfig.json", "vite.config.ts", "index.html",
    "package-src.ps1", "clean-repo.ps1", "devresidue-toolbox.bat"
)
$topDirs = @("crates", "src", "src-tauri", "doc", "resources", "tests")

$staging = Join-Path $env:TEMP "devresidue-src-pack-$date"
if (Test-Path $staging) { Remove-Item $staging -Recurse -Force }
New-Item -ItemType Directory -Path $staging | Out-Null

function Copy-Tree([string]$relSrc) {
    $src = Join-Path $repo $relSrc
    $dest = Join-Path $staging $relSrc
    New-Item -ItemType Directory -Path (Split-Path $dest -Parent) -Force | Out-Null
    $robArgs = @($src, $dest, "/E", "/NFL", "/NDL", "/NJH", "/NJS", "/NP")
    foreach ($d in @("node_modules", "dist", "target", "gen")) {
        $robArgs += @("/XD", (Join-Path $src $d))
    }
    $robArgs += @("/XF", "store-probes.bundle.mjs")
    robocopy @robArgs | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "robocopy failed for $relSrc (code $LASTEXITCODE)" }
}

foreach ($f in $topFiles) {
    if (Test-Path (Join-Path $repo $f)) {
        Copy-Item (Join-Path $repo $f) -Destination $staging -Force
    }
}
foreach ($d in $topDirs) {
    if (Test-Path (Join-Path $repo $d)) { Copy-Tree $d }
}

# .slim: keep only clonedeps.json (the source record)
New-Item -ItemType Directory -Path (Join-Path $staging ".slim") -Force | Out-Null
Copy-Item (Join-Path $repo ".slim/clonedeps.json") -Destination (Join-Path $staging ".slim") -Force

Compress-Archive -Path (Join-Path $staging "*") -DestinationPath $out -Force
Remove-Item $staging -Recurse -Force

$size = "{0:N2} MB" -f ((Get-Item $out).Length / 1MB)
Write-Host "Packaged: $out ($size)"
