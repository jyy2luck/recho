$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
cargo build --release --locked -p recho
if ($LASTEXITCODE -ne 0) { throw 'Rust desktop build failed' }
Write-Output 'Built: app/build/release/recho.exe'
