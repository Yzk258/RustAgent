# ============================================================
#  check-ps51.ps1 -- PowerShell 5.1 gotcha regression probe
#
#  Why this exists:
#     generating the cover hit traps in Windows PowerShell 5.1 that fail
#     SILENTLY rather than loudly, so they are pinned down here as assertions
#     before anyone edits make-cover.ps1 again.
#
#  This file is deliberately 100% ASCII -- that IS the fix for trap 1.
#  Usage: powershell -File assets/cover/check-ps51.ps1
# ============================================================

$ErrorActionPreference = 'Stop'
$script:fail = 0

function Check([string]$label, [bool]$ok, [string]$detail) {
  if ($ok) { Write-Output ('  PASS  ' + $label) }
  else { Write-Output ('  FAIL  ' + $label + '  -- ' + $detail); $script:fail++ }
}

Write-Output ('PowerShell ' + $PSVersionTable.PSVersion + ' (edition ' + $PSVersionTable.PSEdition + ')')
Write-Output ''

# --- trap 1: .ps1 files are decoded as ANSI/GBK, so inlined UTF-8 CJK --------
# literals get truncated (a trailing quote is swallowed and the parser breaks).
# The file stays ASCII; this probe builds the CJK sample from codepoints.
$cjk = [string][char]0x6574 + [string][char]0x5408 + [string][char]0x5305   # "zheng he bao"
Check 'trap 1: CJK built from codepoints keeps its length (file stays ASCII)' `
      ($cjk.Length -eq 3) ('length=' + $cjk.Length)

# --- trap 2: variable names ARE case-insensitive ----------------------------
# $t and $T are the same variable, so a foreach loop over theme names silently
# overwrote the $T copy object handed to the renderer (the real cover bug).
$t = 'theme-name'
$T = 'COPY-OBJECT'
Check 'trap 2: $t and $T are one variable, so assigning one clobbers the other' `
      ($T -eq 'COPY-OBJECT' -and $t -eq 'COPY-OBJECT') ('$t is now ' + $t)

# --- trap 2b: same for $sf / $SF (font vs StringFormat) --------------------
$SF = 'FORMAT'
$sf = 'FONT'
Check 'trap 2b: $sf (font) and $SF (format) collide the same way' `
      ($SF -eq 'FONT') ('$SF is now ' + $SF)

# --- control: distinctly named variables bind and survive ------------------
function Probe-Safe([string]$themeKey) { return $themeKey }
Check 'fix: distinct names like $themeKey / $copy / $fmtNear do not collide' `
      ((Probe-Safe 'dark') -eq 'dark') 'binding broken'

# --- note: $name as a PARAMETER is actually fine ---------------------------
# (PowerShell only shadows automatic variables such as $input/$args/$error for
#  direct $name *references*, not parameter binding; the cover bug was never
#  caused by the parameter name. Renaming to $themeKey was for clarity only.)
function Probe-Name([string]$name) { return $name }
Check 'note: $name as a function parameter binds normally' `
      ((Probe-Name 'dark') -eq 'dark') 'binding broken'

# --- trap 3: Add-Type runs a pre-C# 7 compiler -----------------------------
# no local functions in the injected source; use delegates instead.
$csOk = $true
try {
  Add-Type -TypeDefinition @'
public static class Ps51Probe {
    public static int Add(int a, int b) { return a + b; }
}
'@ -ErrorAction Stop
} catch { $csOk = $false }
Check 'trap 3: Add-Type works (avoid C# 7 local functions, use delegates)' $csOk 'Add-Type failed'

# --- trap 4: Low integrity + no-write-up blocks repo writes ----------------
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$canWrite = $true
try { [System.IO.File]::WriteAllText((Join-Path $here '_writetest.tmp'), 'x') } catch { $canWrite = $false }
if ($canWrite) { Remove-Item (Join-Path $here '_writetest.tmp') -Force -ErrorAction SilentlyContinue }
Write-Output ('  INFO  this process can write assets/cover directly: ' + $canWrite +
              '  (False means Low+NW, so make-cover.ps1 falls back to $env:TEMP)')

Write-Output ''
if ($script:fail -eq 0) { Write-Output 'OK: all assertions passed'; exit 0 }
else { Write-Output ('FAILED: ' + $script:fail + ' assertion(s)'); exit 1 }
