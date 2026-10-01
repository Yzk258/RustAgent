param(
  [ValidateSet('dark', 'emerald', 'light', 'all')]
  [string]$Only = 'all'
)

# ============================================================
#  OPTIONAL HTML RENDER PATH -- needs a working headless browser.
#
#  This path is NOT what produced the committed cover PNGs. In a sandboxed
#  session whose process integrity is Low, Chromium cannot create its Mojo
#  named pipes and dies with:
#     FATAL:mojo\public\cpp\platform\platform_channel.cc:108 Check failed: (0x5)
#  so `make-cover.ps1` (GDI+, in-process) is the authoritative renderer.
#  Use this one on a normal desktop where a browser starts fine.
#
#  Both paths read the same design-token contract: the token names here match
#  :root in src/ui/static/style.css, exactly like the $palettes table in
#  make-cover.ps1.
# ============================================================

# 注意: 工作区被标成 Low 完整性时, Chrome 的 crashpad 会往 stderr 报
#       "OpenProcess 拒绝访问 / Lock file can not be created"，
#       那是无害噪音, 截图照样产出 —— 所以这里不能把 stderr 当致命错误。
$ErrorActionPreference = 'Continue'
Add-Type -AssemblyName System.Drawing

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$src = Join-Path $here 'cover.html'
$cfgPath = Join-Path $here 'themes.json'

$chrome = @(
  'C:\Program Files\Google\Chrome\Application\chrome.exe',
  'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe'
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $chrome) { throw 'no Chrome/Edge found for headless rendering' }

$cfg = [System.IO.File]::ReadAllText($cfgPath, [System.Text.Encoding]::UTF8) | ConvertFrom-Json
$html = [System.IO.File]::ReadAllText($src, [System.Text.Encoding]::UTF8)

$targets = if ($Only -eq 'all') { @($cfg.order) } else { @($Only) }

foreach ($name in $targets) {
  $t = $cfg.themes.$name

  # 1) inject this variant's design tokens before </head>
  $decl = ($t.tokens.PSObject.Properties | ForEach-Object { $_.Name + ':' + $_.Value }) -join ';'
  $inject = '<style>html.' + $t.class + '{' + $decl + '}</style>' + '</head>'
  $out = $html.Replace('</head>', $inject)

  # 2) literal colors for SVG gradients (var() in stop-color works in Chromium,
  #    but a data URL keeps this renderer-only detail out of the source template)
  $out = $out.Replace('var(--g1)', $t.tokens.'--g1').Replace('var(--g2)', $t.tokens.'--g2')

  # 3) hand the page to Chrome as a data URL: nothing is written to disk by this
  #    script, which matters on a workspace labelled Low + no-write-up (see README).
  $b64 = [Convert]::ToBase64String([System.Text.Encoding]::UTF8.GetBytes($out))
  $url = 'data:text/html;base64,' + $b64

  $png = Join-Path $here ('modsmith-cover-' + $name + '.png')
  & $chrome --headless=new --disable-gpu --no-sandbox --disable-crash-reporter --disable-breakpad --no-first-run --no-default-browser-check --hide-scrollbars --force-device-scale-factor=1 --screenshot="$png" --window-size=1920,1080 $url 2>$null | Out-Null

  if (-not (Test-Path $png)) { throw ('render failed: ' + $name) }

  $bmp = [System.Drawing.Bitmap]::FromFile($png)
  $w = $bmp.Width
  $h = $bmp.Height
  $c = $bmp.GetPixel(960, 540)
  $bmp.Dispose()

  $flag = if ($w -eq 1920 -and $h -eq 1080) { 'OK ' } else { 'BAD' }
  '{0,-8} {1}x{2} {3} center=#{4:X2}{5:X2}{6:X2}' -f $name, $w, $h, $flag, $c.R, $c.G, $c.B
}
