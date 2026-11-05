param(
    [string] $Compiler = (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
)

$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $Compiler -PathType Leaf)) {
    throw 'The existing Inno Setup 6 compiler is required. This script never installs tooling.'
}
$root = Split-Path -Parent $PSScriptRoot
$env:RUSTUP_AUTO_INSTALL = '0'
$env:PATH = (Join-Path $env:USERPROFILE '.cargo\bin') + ';' + $env:PATH
$manifest = Join-Path $root 'rust\Cargo.toml'
& cargo build --manifest-path $manifest --offline --locked --release -p voiceprompt-windows-pilot
if ($LASTEXITCODE -ne 0) { throw 'Native release build failed.' }
$stage = Join-Path $root 'rust\target\installer-staging'
New-Item -ItemType Directory -Path $stage -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $root 'rust\target\release\VoicePromptRustPilot.exe') -Destination (Join-Path $stage 'VoicePrompt.exe') -Force
Copy-Item -LiteralPath (Join-Path $root 'assets\windows\app.ico') -Destination (Join-Path $stage 'app.ico') -Force
$unexpected = Get-ChildItem -LiteralPath $stage -File | Where-Object Name -NotIn @('VoicePrompt.exe', 'app.ico')
if ($unexpected) { throw 'Installer staging contains unexpected files. No installer was built.' }
& $Compiler /Qp "/DSourceDir=$stage" (Join-Path $root 'rust\windows-pilot\installer\VoicePrompt-Rust.iss')
if ($LASTEXITCODE -ne 0) { throw 'Native migration installer build failed.' }
$installer = Join-Path $root 'rust\target\installer-output\VoicePrompt-Rust-Setup.exe'
if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) { throw 'Installer output was not produced.' }
Get-FileHash -LiteralPath $installer -Algorithm SHA256
Write-Output 'Built only. Installation/upgrade and release publishing were not performed or approved by this script.'
