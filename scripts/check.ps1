$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
cargo fmt --all --check
if ($LASTEXITCODE -ne 0) { throw 'Rust formatting failed' }
cargo build --workspace --locked
if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
python -c 'import base64, contextlib, email.policy, http.client, json, traceback, types, urllib.parse, uuid'
if ($LASTEXITCODE -ne 0) { throw 'Engine hook checks require Python 3' }
cargo test --workspace --locked
if ($LASTEXITCODE -ne 0) { throw 'Rust tests failed' }
$previousOpenSSL = $env:RECHO_TEST_OPENSSL
try {
    if (!$env:RECHO_TEST_OPENSSL -and !(Get-Command openssl -ErrorAction SilentlyContinue)) {
        $gitOpenSSL = Join-Path $env:ProgramFiles 'Git\usr\bin\openssl.exe'
        if (!(Test-Path -LiteralPath $gitOpenSSL)) { throw 'Engine tests require OpenSSL in PATH or RECHO_TEST_OPENSSL' }
        $env:RECHO_TEST_OPENSSL = $gitOpenSSL
    }
    node --test engine/server.test.mjs engine/extensions.test.mjs engine/protocol-editors.test.mjs
    if ($LASTEXITCODE -ne 0) { throw 'Engine integration tests failed' }
} finally { $env:RECHO_TEST_OPENSSL = $previousOpenSSL }
