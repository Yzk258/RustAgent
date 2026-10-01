<#
  stitch-png.ps1 -- rebuild a binary file from base64 arriving on stdin.

  Why this exists:
    the file tools that can write into this workspace can only write TEXT, while
    the process that renders the cover PNG cannot write into the workspace at
    all -- the repo carries "Mandatory Label\Medium Mandatory Level:(NW)"
    (no-write-up), and a low-integrity process is refused everywhere under it
    (verified: .NET WriteAllText, cmd copy, cmd move and robocopy are all
    "Access is denied"). Routing ~580 KB of base64 through a tool call does not
    work either, because oversized tool output is truncated.

    So this is a recovery hatch only. If modsmith-cover-dark.png is ever lost
    and cannot be regenerated, save the base64 somewhere and pipe it in.

  Normal regeneration needs none of this -- run make-cover.ps1 from a regular
  (medium-integrity) terminal and it writes the PNG directly.

  Usage:
    # base64 of the PNG, one or more lines, piped in
    Get-Content cover.b64 | powershell -File assets/cover/stitch-png.ps1 -OutPath modsmith-cover-dark.png
#>
param(
  [Parameter(Mandatory = $true)][string]$OutPath
)

$ErrorActionPreference = 'Stop'

$raw = [Console]::In.ReadToEnd()
if ([string]::IsNullOrWhiteSpace($raw)) { throw 'no base64 on stdin' }

$b64 = ($raw -replace '\s', '')
if ($b64 -notmatch '^[A-Za-z0-9+/]+=*$') { throw 'stdin is not clean base64' }
Write-Output ('base64: ' + $b64.Length + ' chars')

$bytes = [Convert]::FromBase64String($b64)
Write-Output ('decoded: ' + $bytes.Length + ' bytes')

# PNG signature 89 50 4E 47 0D 0A 1A 0A
$sig = @(0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A)
for ($i = 0; $i -lt $sig.Count; $i++) {
  if ($bytes[$i] -ne $sig[$i]) { throw ('not a PNG: byte ' + $i + ' is 0x' + ('{0:X2}' -f $bytes[$i])) }
}

[System.IO.File]::WriteAllBytes($OutPath, $bytes)

# load it back through GDI+ to prove the image is really intact
Add-Type -AssemblyName System.Drawing
$bmp = [System.Drawing.Bitmap]::FromFile((Resolve-Path $OutPath).Path)
Write-Output ('verified: ' + $bmp.Width + 'x' + $bmp.Height + ' loads in GDI+')
$bmp.Dispose()

$out = Get-Item $OutPath
Write-Output ('written : ' + $out.FullName + '  ' + $out.Length + ' bytes')
Write-Output ('sha256  : ' + (Get-FileHash $out.FullName -Algorithm SHA256).Hash)
