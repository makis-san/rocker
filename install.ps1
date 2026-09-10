<#
.SYNOPSIS
  Rocker installer for Windows. Downloads the latest release, verifies it, and
  runs `rocker install` so you get the binary on PATH plus a Start-menu
  shortcut, an "Apps & features" entry, and the docker:// URL handler.

.EXAMPLE
  irm https://raw.githubusercontent.com/makis-san/rocker/main/install.ps1 | iex

.EXAMPLE
  # with options:
  & ([scriptblock]::Create((irm https://raw.githubusercontent.com/makis-san/rocker/main/install.ps1))) -Tag v0.1.4 -ModifyPath
#>
[CmdletBinding()]
param(
    [string]$Tag = "",
    [switch]$ModifyPath,
    [switch]$System
)

$ErrorActionPreference = 'Stop'
$Repo = "makis-san/rocker"
# minisign public key that signs SHA256SUMS (base64 blob from rocker.pub). The
# checksum is always verified; the signature is verified too when `minisign` is
# on PATH, and skipped with a note otherwise (`rocker self-update` always
# verifies it).
$MinisignPubKey = "RWTfmkhmM6bfkPa36B5q/LZZ4LEY5tVCqAO5t5fkiGbOBp5ztbkwc3VE"

function Say($m)  { Write-Host "rocker-install: $m" }
function Die($m)  { Write-Error "rocker-install: $m"; exit 1 }

# --- target triple ---------------------------------------------------------
switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { $cpu = "x86_64" }
    "ARM64" { $cpu = "aarch64" }
    default { Die "unsupported architecture: $($env:PROCESSOR_ARCHITECTURE)" }
}
$Triple  = "$cpu-pc-windows-msvc"
$Archive = "rocker-$Triple.zip"

# --- resolve the release tag --------------------------------------------------
$headers = @{ "User-Agent" = "rocker-install"; "Accept" = "application/vnd.github+json" }
if (-not $Tag) {
    Say "resolving latest release..."
    $rel = Invoke-RestMethod -Headers $headers "https://api.github.com/repos/$Repo/releases/latest"
    $Tag = $rel.tag_name
    if (-not $Tag) { Die "couldn't determine the latest release tag" }
}
Say "installing $Tag for $Triple"

$Base = "https://github.com/$Repo/releases/download/$Tag"
$Tmp  = Join-Path $env:TEMP ("rocker-install-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    Say "downloading $Archive"
    Invoke-WebRequest -Headers $headers "$Base/$Archive"    -OutFile (Join-Path $Tmp $Archive)
    Invoke-WebRequest -Headers $headers "$Base/SHA256SUMS"  -OutFile (Join-Path $Tmp "SHA256SUMS")
    try {
        Invoke-WebRequest -Headers $headers "$Base/SHA256SUMS.minisig" -OutFile (Join-Path $Tmp "SHA256SUMS.minisig")
    } catch { }

    # --- verify checksum -------------------------------------------------
    $want = (Select-String -Path (Join-Path $Tmp "SHA256SUMS") -Pattern ([regex]::Escape($Archive)) |
             Select-Object -First 1).Line -replace '\s.*$', ''
    if (-not $want) { Die "$Archive not listed in SHA256SUMS" }
    $got = (Get-FileHash -Algorithm SHA256 (Join-Path $Tmp $Archive)).Hash
    if ($want.ToLower() -ne $got.ToLower()) {
        Die "checksum mismatch for $Archive (expected $want, got $got)"
    }
    Say "checksum ok"

    # --- verify signature --------------------------------------------
    # The checksum above is fetched over HTTPS from GitHub; a minisign
    # signature closes the supply-chain gap when the tool is present.
    $minisig = Join-Path $Tmp "SHA256SUMS.minisig"
    if (-not $MinisignPubKey) {
        Say "note: no signing key in this installer; relying on the HTTPS checksum"
    } elseif (-not (Test-Path $minisig)) {
        Say "note: this release has no SHA256SUMS.minisig; relying on the HTTPS checksum"
    } elseif (Get-Command minisign -ErrorAction SilentlyContinue) {
        $MinisignPubKey | Set-Content -NoNewline (Join-Path $Tmp "rocker.pub")
        & minisign -Vm (Join-Path $Tmp "SHA256SUMS") -p (Join-Path $Tmp "rocker.pub") | Out-Null
        if ($LASTEXITCODE -ne 0) { Die "minisign signature verification failed" }
        Say "signature ok"
    } else {
        Say "note: minisign not installed; skipping signature check (checksum verified over HTTPS)"
    }

    # --- unpack and hand off to the binary ---------------------------
    Say "unpacking"
    Expand-Archive -Path (Join-Path $Tmp $Archive) -DestinationPath $Tmp -Force
    $exe = Get-ChildItem -Path $Tmp -Recurse -Filter "rocker.exe" | Select-Object -First 1
    if (-not $exe) { Die "archive did not contain rocker.exe" }

    $fwd = @("install")
    if ($ModifyPath) { $fwd += "--modify-path" }
    if ($System)     { $fwd += "--system" }
    Say "running: rocker $($fwd -join ' ')"
    & $exe.FullName @fwd

    Say "done. launch Rocker from the Start menu, or run: rocker"
}
finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
