# Builds PumpkinX on Windows. The Linux version of this script is build.sh.
#   .\build.ps1                 release build with PatchBukkit
#   .\build.ps1 -Dev            faster unoptimized build
#   .\build.ps1 -NoPatchBukkit  leave PatchBukkit out of the binary
param(
    [switch]$Dev,
    [switch]$NoPatchBukkit
)

# Not 'Stop': Windows PowerShell treats any stderr line from gradle or cargo as fatal then.
# Failures are caught through the exit codes below.
$root = $PSScriptRoot
$profileName = if ($Dev) { 'dev' } else { 'release' }
$cargoArgs = @('build', '-p', 'pumpkin', '--profile', $profileName)

if ($NoPatchBukkit) {
    $cargoArgs += '--no-default-features'
} else {
    Write-Host '==> PatchBukkit Java bridge (gradle)'
    Push-Location (Join-Path $root 'builtin\patchbukkit\java')
    try {
        & .\gradlew.bat jar --no-daemon -q
        if ($LASTEXITCODE -ne 0) { throw 'Gradle build failed' }
    } finally {
        Pop-Location
    }
}

Write-Host '==> Keystone plugin (Wasm)'
Push-Location (Join-Path $root 'builtin\keystone')
try {
    & cargo build --release --target wasm32-wasip2
    if ($LASTEXITCODE -ne 0) { throw 'Keystone build failed' }
} finally {
    Pop-Location
}

Write-Host "==> PumpkinX ($profileName)"
Push-Location $root
try {
    # PatchBukkit's generated code needs a deeper compiler stack.
    $env:RUST_MIN_STACK = '33554432'
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw 'Cargo build failed' }
} finally {
    Pop-Location
}

$outDir = if ($Dev) { 'debug' } else { 'release' }
Write-Host "==> built $(Join-Path $root "target\$outDir\pumpkin.exe")"
