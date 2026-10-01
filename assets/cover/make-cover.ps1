<#
  ============================================================
   make-cover.ps1 -- ModSmith cover generator (1920x1080, 16:9)

   Why GDI+ instead of a browser screenshot:
     this workspace is labelled Low integrity; Chromium's browser
     process needs Mojo named pipes for IPC and the sandbox forbids
     named pipes, so headless Chrome dies with
       FATAL:platform_channel.cc:108 Check failed: (0x5)
     GDI+ draws in-process, deterministically, with no external
     process and no IPC. (cover.html + themes.json are kept as the
     optional HTML path for machines where a browser does run.)

   Why the script contains no Chinese literals:
     Windows PowerShell 5.1 decodes .ps1 as ANSI/GBK, which truncates
     UTF-8 CJK string literals and breaks parsing. All copy lives in
     copy.json and is read back with an explicit UTF-8 decoder.

   Usage:
     powershell -File assets/cover/make-cover.ps1 -Theme dark
     powershell -File assets/cover/make-cover.ps1 -Theme all
     powershell -File assets/cover/make-cover.ps1 -Theme dark -Verify

   Colour tokens mirror src/ui/static/style.css (:root / .light) so the
   cover speaks the same visual language as the Web and desktop UI.
  ============================================================
#>

param(
  [ValidateSet('dark', 'emerald', 'light', 'all')]
  [string]$Theme = 'dark',
  [switch]$Verify,
  # Where to put the PNGs. Defaults to this script's own folder.
  #
  # This parameter exists because of the sandbox quirk documented in README.md:
  # the workspace carries "Mandatory Label\Medium Mandatory Level:(NW)"
  # (no-write-up), so a LOW-integrity process -- which is what an agent shell
  # gets -- cannot create files anywhere under the repository, and GDI+ surfaces
  # that as "A generic error occurred in GDI+". A normal desktop terminal is
  # Medium integrity and writes fine, so running this script by hand just works;
  # pass an explicit -OutDir only when the script itself was launched from a
  # low-integrity shell.
  [string]$OutDir = ''
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

# ============================================================
#  DrawingKit
# ============================================================
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Text;

public static class DrawingKit
{
    // ---------- primitives ----------

    public static GraphicsPath RoundedRect(float x, float y, float w, float h, float r)
    {
        float d = r * 2f;
        if (d > w) d = w;
        if (d > h) d = h;
        GraphicsPath p = new GraphicsPath();
        p.AddArc(x, y, d, d, 180, 90);
        p.AddArc(x + w - d, y, d, d, 270, 90);
        p.AddArc(x + w - d, y + h - d, d, d, 0, 90);
        p.AddArc(x, y + h - d, d, d, 90, 90);
        p.CloseFigure();
        return p;
    }

    public static void FillRound(Graphics g, Brush b, float x, float y, float w, float h, float r)
    {
        using (GraphicsPath p = RoundedRect(x, y, w, h, r)) g.FillPath(b, p);
    }

    public static void StrokeRound(Graphics g, Pen pen, float x, float y, float w, float h, float r)
    {
        using (GraphicsPath p = RoundedRect(x, y, w, h, r)) g.DrawPath(pen, p);
    }

    public static void SoftShadow(Graphics g, float x, float y, float w, float h, float r, int spread, int alpha)
    {
        for (int i = spread; i >= 1; i--)
        {
            int a = (int)(alpha * Math.Pow(1.0 - (double)i / (spread + 1), 2.2));
            if (a <= 0) continue;
            using (Pen pen = new Pen(Color.FromArgb(a, 0, 0, 0), 2f))
                StrokeRound(g, pen, x - i, y - i, w + i * 2, h + i * 2, r + i);
        }
    }

    public static LinearGradientBrush Linear(RectangleF rc, float angle, Color c1, Color c2)
    {
        LinearGradientBrush b = new LinearGradientBrush(rc, c1, c2, angle);
        b.WrapMode = WrapMode.TileFlipXY;
        return b;
    }

    /// straight per-channel mix, returned as "#rrggbb" so the result can be fed
    /// back through the C/CA helpers. It returns a STRING on purpose: CA expects
    /// a hex string, and passing it a Color makes it try to hex-parse
    /// "Color [A=255, R=22, G=25, B=33]".
    /// Used where a CSS colour would otherwise be composited onto a background
    /// of nearly the same value and disappear.
    public static string MixHex(string hex1, string hex2, double t)
    {
        if (t < 0.0) t = 0.0;
        if (t > 1.0) t = 1.0;
        int r1 = Convert.ToInt32(hex1.Substring(1, 2), 16);
        int g1 = Convert.ToInt32(hex1.Substring(3, 2), 16);
        int b1 = Convert.ToInt32(hex1.Substring(5, 2), 16);
        int r2 = Convert.ToInt32(hex2.Substring(1, 2), 16);
        int g2 = Convert.ToInt32(hex2.Substring(3, 2), 16);
        int b2 = Convert.ToInt32(hex2.Substring(5, 2), 16);
        return string.Format("#{0:x2}{1:x2}{2:x2}",
            (int)Math.Round(r1 + (r2 - r1) * t),
            (int)Math.Round(g1 + (g2 - g1) * t),
            (int)Math.Round(b1 + (b2 - b1) * t));
    }

    /// radial halo: core alpha in the middle, fully transparent at the rim
    public static void RadialGlow(Graphics g, float cx, float cy, float radius, Color core)
    {
        RectangleF rc = new RectangleF(cx - radius, cy - radius, radius * 2, radius * 2);
        using (GraphicsPath p = new GraphicsPath())
        {
            p.AddEllipse(rc);
            using (PathGradientBrush br = new PathGradientBrush(p))
            {
                br.CenterPoint = new PointF(cx, cy);
                br.CenterColor = core;
                br.SurroundColors = new Color[] { Color.FromArgb(0, core.R, core.G, core.B) };
                br.SetSigmaBellShape(0.42f);
                g.FillPath(br, p);
            }
        }
    }

    /// wipe drawn content with a radial alpha mask: strongest in the middle, gone at the rim
    public static void RadialFade(Graphics g, RectangleF area, float cx, float cy, float radius, int maxAlpha)
    {
        using (GraphicsPath p = new GraphicsPath())
        {
            p.AddEllipse(cx - radius, cy - radius, radius * 2, radius * 2);
            using (PathGradientBrush br = new PathGradientBrush(p))
            {
                br.CenterPoint = new PointF(cx, cy);
                br.CenterColor = Color.FromArgb(maxAlpha, 0, 0, 0);
                br.SurroundColors = new Color[] { Color.FromArgb(0, 0, 0, 0) };
                br.SetSigmaBellShape(0.38f);
                g.FillRectangle(br, area);
            }
        }
    }

    /// isometric grass block + pickaxe, built from pixel blocks (echoes the CLI banner)
    public static void Pickaxe(Graphics g, float ox, float oy, float s,
        Color g1, Color g2, Color dirtA, Color dirtB, Color dirtC)
    {
        float sy = 0.58f;
        float dep = 0.72f;

        // no local functions: the C# compiler behind Windows PowerShell 5.1 is pre-C# 7
        Func<float, float, PointF> pT = delegate(float u, float v) { return new PointF(ox + u * s, oy + v * s * sy); };
        Func<float, float, PointF> pL = delegate(float u, float v) { return new PointF(ox + u * s, oy + s * sy + v * s); };
        Func<float, float, PointF> pR = delegate(float u, float v) { return new PointF(ox + s + u * s, oy + s * sy + v * s); };

        PointF[] topFace   = { pT(-1, 0), pT(0, -1), pT(1, 0), pT(0, 1) };
        PointF[] leftFace  = { pT(-1, 0), pT(0, 1), pL(0, 1 + dep), pL(-1, dep) };
        PointF[] rightFace = { pT(0, 1), pT(1, 0), pR(1, dep), pR(0, 1 + dep) };

        using (SolidBrush b = new SolidBrush(dirtA)) g.FillPolygon(b, leftFace);
        using (SolidBrush b = new SolidBrush(dirtB)) g.FillPolygon(b, rightFace);
        using (SolidBrush b = new SolidBrush(dirtC)) g.FillPolygon(b, topFace);

        float gl = 0.20f;
        PointF[] grassL = { pT(-1, 0), pT(0, 1), pL(0, 1 + gl), pL(-1, gl) };
        PointF[] grassR = { pT(0, 1), pT(1, 0), pR(1, gl), pR(0, 1 + gl) };
        using (SolidBrush b = new SolidBrush(Color.FromArgb(255, 107, 163, 65))) g.FillPolygon(b, grassL);
        using (SolidBrush b = new SolidBrush(Color.FromArgb(255, 87, 138, 52))) g.FillPolygon(b, grassR);

        Random rnd = new Random(20260901);
        using (SolidBrush b = new SolidBrush(Color.FromArgb(90, 60, 40, 22)))
            for (int i = 0; i < 10; i++)
            {
                float u = (float)(rnd.NextDouble() * 1.4 - 0.95);
                float v = (float)(rnd.NextDouble() * (dep - 0.35) + 0.35);
                PointF q = (i % 2 == 0) ? pL(u, v) : pR(u, v);
                g.FillRectangle(b, q.X, q.Y, s * 0.09f, s * 0.09f);
            }

        float pw = s * 0.86f;
        float ph = s * 1.30f;
        float px = ox - pw * 0.5f;
        float py = oy - s * sy - ph - s * 0.10f;

        using (GraphicsPath p = new GraphicsPath())
        {
            p.AddBezier(px + pw * 0.50f, py + ph * 0.15f, px + pw * 0.50f, py, px + pw * 0.22f, py + ph * 0.02f, px + pw * 0.04f, py + ph * 0.22f);
            p.AddBezier(px + pw * 0.04f, py + ph * 0.22f, px + pw * 0.02f, py + ph * 0.32f, px + pw * 0.00f, py + ph * 0.40f, px + pw * 0.00f, py + ph * 0.46f);
            p.AddLine(px + pw * 0.00f, py + ph * 0.46f, px + pw * 0.18f, py + ph * 0.46f);
            p.AddBezier(px + pw * 0.18f, py + ph * 0.46f, px + pw * 0.20f, py + ph * 0.34f, px + pw * 0.28f, py + ph * 0.20f, px + pw * 0.50f, py + ph * 0.15f);
            p.CloseFigure();
            p.AddBezier(px + pw * 0.50f, py + ph * 0.15f, px + pw * 0.50f, py, px + pw * 0.78f, py + ph * 0.02f, px + pw * 0.96f, py + ph * 0.22f);
            p.AddBezier(px + pw * 0.96f, py + ph * 0.22f, px + pw * 0.98f, py + ph * 0.32f, px + pw * 1.00f, py + ph * 0.40f, px + pw * 1.00f, py + ph * 0.46f);
            p.AddLine(px + pw * 1.00f, py + ph * 0.46f, px + pw * 0.82f, py + ph * 0.46f);
            p.AddBezier(px + pw * 0.82f, py + ph * 0.46f, px + pw * 0.80f, py + ph * 0.34f, px + pw * 0.72f, py + ph * 0.20f, px + pw * 0.50f, py + ph * 0.15f);
            p.CloseFigure();
            using (LinearGradientBrush b = Linear(new RectangleF(px, py, pw, ph * 0.5f), 0f, g1, g2))
                g.FillPath(b, p);
        }

        using (SolidBrush b = new SolidBrush(Color.FromArgb(255, 169, 117, 74)))
            FillRound(g, b, ox - s * 0.09f, py + ph * 0.34f, s * 0.18f, ph * 0.72f, s * 0.06f);
    }

    // ---------- layout audit ----------

    public sealed class Item
    {
        public string Text;
        public string Tag;
        public string Box;
        public RectangleF Rect;   // MeasureString box (includes leading)
        public RectangleF Ink;    // glyph ink only -- what the eye sees
    }

    static List<Item> _items = new List<Item>();

    /// minimum clear gap (px) required between two glyph-ink boxes
    public const float MinGap = 6f;

    public static void ResetLog() { _items.Clear(); }

    /// measure a string and record it against a named container for the audit pass
    public static SizeF Logged(Graphics g, string s, Font f, PointF at, string tag, string container, float maxRight)
    {
        if (s == null) s = "";
        SizeF sz = g.MeasureString(s, f, new PointF(0, 0), StringFormat.GenericTypographic);

        RectangleF ink = InkBox(s, f);
        if (ink.Width > 0f && ink.Height > 0f)
        {
            // InkBox comes from a scratch bitmap drawn at (0,0) with the same
            // Typographic format, so the offset transfers directly.
            ink.X += at.X;
            ink.Y += at.Y;
        }
        else
        {
            ink = new RectangleF(at.X, at.Y, sz.Width, sz.Height);
        }

        Item it = new Item();
        it.Text = s;
        it.Tag = tag;
        it.Box = container;
        it.Rect = new RectangleF(at.X, at.Y, sz.Width, sz.Height);
        it.Ink = ink;
        _items.Add(it);

        if (maxRight > 0f && ink.Right > maxRight + 1f)
            it.Tag = tag + "!PANELEXCEED";
        return sz;
    }

    public static List<string> Audit(int w, int h, float margin)
    {
        List<string> issues = new List<string>();
        for (int i = 0; i < _items.Count; i++)
        {
            Item a = _items[i];
            string nm = a.Tag;

            if (nm.EndsWith("!PANELEXCEED"))
                issues.Add("PANEL-EXCEED  [" + nm + "] \"" + Clip(a.Text) + "\" ink " + R(a.Ink));

            // canvas test against the ink box: a box may stick out harmlessly
            // while the visible glyphs must never leave the image.
            if (a.Ink.Width > 0f &&
                (a.Ink.Left < margin - 0.5f || a.Ink.Right > w - margin + 0.5f ||
                 a.Ink.Top < margin - 0.5f || a.Ink.Bottom > h - margin + 0.5f))
                issues.Add("OUT-OF-CANVAS [" + nm + "] \"" + Clip(a.Text) + "\" ink " + R(a.Ink));

            for (int j = i + 1; j < _items.Count; j++)
            {
                Item b = _items[j];

                RectangleF inter = RectangleF.Intersect(a.Ink, b.Ink);
                if (inter.Width > 0.5f && inter.Height > 0.5f)
                {
                    issues.Add(string.Format("INK-OVERLAP   [{0}] \"{1}\" ink {2}  <->  [{3}] \"{4}\" ink {5}",
                        a.Tag, Clip(a.Text), R(a.Ink), b.Tag, Clip(b.Text), R(b.Ink)));
                    continue;
                }

                // near miss: report so that "almost touching" never ships silently
                float dx = Math.Max(0f, Math.Max(a.Ink.Left - b.Ink.Right, b.Ink.Left - a.Ink.Right));
                float dy = Math.Max(0f, Math.Max(a.Ink.Top - b.Ink.Bottom, b.Ink.Top - a.Ink.Bottom));
                float sep = (float)Math.Sqrt(dx * dx + dy * dy);
                if (sep < MinGap)
                    issues.Add(string.Format("INK-TOO-CLOSE gap={0:F1}px  [{1}] \"{2}\"  <->  [{3}] \"{4}\"",
                        sep, a.Tag, Clip(a.Text), b.Tag, Clip(b.Text)));
            }
        }
        return issues;
    }

    static string Clip(string s) { return s.Length <= 24 ? s : s.Substring(0, 23) + "..."; }

    static string R(RectangleF r)
    {
        return string.Format("({0:F0},{1:F0})-({2:F0},{3:F0})", r.Left, r.Top, r.Right, r.Bottom);
    }

    // ---------- glyph-ink measurement ----------
    //
    // MeasureString returns a BOX that is far taller than the glyphs it holds
    // (the 130px wordmark measures 173px, its caps only 97px). Auditing boxes
    // therefore missed the real defect: the wordmark box started at y=181
    // while the badge pill already ended at y=180, so the pill border showed
    // through the top of the letters even though "boxes did not overlap".
    // Render the run into a scratch bitmap and keep the alpha>=32 bounding box
    // -- that is what the eye actually sees.
    public static RectangleF InkBox(string s, Font f)
    {
        if (string.IsNullOrEmpty(s)) return new RectangleF(0, 0, 0, 0);

        int w = (int)Math.Ceiling((double)f.Size * s.Length) + 48;
        int h = (int)Math.Ceiling((double)f.Height) + 48;
        if (w < 8) w = 8;
        if (h < 8) h = 8;
        if (w > 2400) w = 2400;
        if (h > 400) h = 400;

        using (Bitmap bmp = new Bitmap(w, h, System.Drawing.Imaging.PixelFormat.Format32bppArgb))
        using (Graphics gg = Graphics.FromImage(bmp))
        {
            gg.TextRenderingHint = System.Drawing.Text.TextRenderingHint.AntiAliasGridFit;
            using (SolidBrush br = new SolidBrush(Color.White))
                gg.DrawString(s, f, br, 0f, 0f, StringFormat.GenericTypographic);

            int minX = int.MaxValue, minY = int.MaxValue, maxX = -1, maxY = -1;
            for (int y = 0; y < h; y++)
                for (int x = 0; x < w; x++)
                {
                    if (bmp.GetPixel(x, y).A >= 32)
                    {
                        if (x < minX) minX = x;
                        if (x > maxX) maxX = x;
                        if (y < minY) minY = y;
                        if (y > maxY) maxY = y;
                    }
                }

            if (maxX < 0) return new RectangleF(0, 0, 0, 0);
            return new RectangleF(minX, minY, maxX - minX + 1, maxY - minY + 1);
        }
    }

    // ---------- CJK aware wrapping ----------

    public static List<string> Wrap(Graphics g, string text, Font f, float maxW)
    {
        List<string> lines = new List<string>();
        StringBuilder cur = new StringBuilder();
        float curW = 0f;
        List<string> tokens = new List<string>();
        StringBuilder latin = new StringBuilder();

        foreach (char c in text)
        {
            bool isLatin = (c < 128) && !char.IsWhiteSpace(c);
            if (isLatin) { latin.Append(c); continue; }
            if (latin.Length > 0) { tokens.Add(latin.ToString()); latin.Clear(); }
            tokens.Add(c.ToString());
        }
        if (latin.Length > 0) tokens.Add(latin.ToString());

        foreach (string tk in tokens)
        {
            float w = g.MeasureString(tk, f, new PointF(0, 0), StringFormat.GenericTypographic).Width;
            if (curW + w > maxW && cur.Length > 0 && !char.IsWhiteSpace(tk[0]))
            {
                lines.Add(cur.ToString().TrimEnd());
                cur.Clear();
                curW = 0f;
            }
            cur.Append(tk);
            curW += w;
        }
        if (cur.Length > 0) lines.Add(cur.ToString().TrimEnd());
        return lines;
    }
}
'@ -ReferencedAssemblies System.Drawing, System.Drawing.Primitives

# ============================================================
#  palette
# ============================================================
$palettes = @{
  dark = @{
    bg='#0d0f14'; card='#161a22'; raise='#1b202b'; bubble='#1d2330'
    border='#262d3a'; text='#e9ecf2'; dim='#8a93a6'
    accent='#6d8dff'; accentBright='#93abff'; g1='#5b8cff'; g2='#8b5cf6'
    green='#34d399'; yellow='#fbbf24'
    panel='#151820'; panelAlpha=236; gridAlpha=15; vignette=80; shadow=155
  }
  emerald = @{
    bg='#0b1210'; card='#131c19'; raise='#18231f'; bubble='#1a2723'
    border='#23332d'; text='#e7f2ec'; dim='#86988f'
    accent='#4ade9e'; accentBright='#7ff0bd'; g1='#34d399'; g2='#22a06b'
    green='#34d399'; yellow='#fbbf24'
    panel='#131c19'; panelAlpha=236; gridAlpha=15; vignette=80; shadow=155
  }
  light = @{
    bg='#f4f6fa'; card='#eef1f6'; raise='#eef1f6'; bubble='#e9eef8'
    border='#d7ddea'; text='#1b2333'; dim='#68738a'
    accent='#4169d8'; accentBright='#3155bd'; g1='#4169d8'; g2='#8b5cf6'
    green='#16845b'; yellow='#a66b00'
    panel='#ffffff'; panelAlpha=255; gridAlpha=13; vignette=0; shadow=52
  }
}

function C([string]$hex) {
  $h = $hex.TrimStart('#')
  if ($h.Length -ne 6) {
    throw ("C() got a bad colour: [" + $hex + "] len=" + $h.Length +
           " (expected #rrggbb). Check the caller for an unbound palette key.")
  }
  [System.Drawing.Color]::FromArgb(
    [Convert]::ToInt32($h.Substring(0,2),16),
    [Convert]::ToInt32($h.Substring(2,2),16),
    [Convert]::ToInt32($h.Substring(4,2),16))
}
function CA([string]$hex, [int]$a) {
  $c = C $hex
  [System.Drawing.Color]::FromArgb($a, $c.R, $c.G, $c.B)
}

$FAM  = 'Microsoft YaHei'
$MONO = 'Consolas'
$UI   = 'Segoe UI'

function NewFont([string]$fam, [float]$size, [System.Drawing.FontStyle]$style) {
  New-Object System.Drawing.Font($fam, $size, $style, [System.Drawing.GraphicsUnit]::Pixel)
}

# ============================================================
#  renderer
# ============================================================
function Render-Cover([string]$themeKey, [string]$outPath, $Copy, [bool]$verify) {

  # NOTE: never name a parameter $name / $input / $args here -- PowerShell owns
  #       those automatic variables and silently replaces the bound value.
  $p = $palettes[$themeKey]
  $W = 1920; $H = 1080
  $bmp = New-Object System.Drawing.Bitmap($W, $H, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode     = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
  $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::ClearTypeGridFit
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $g.PixelOffsetMode   = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
  [DrawingKit]::ResetLog()

  $fmtTypo = [System.Drawing.StringFormat]::GenericTypographic
  $fmtNear = New-Object System.Drawing.StringFormat
  $fmtNear.Alignment = [System.Drawing.StringAlignment]::Near
  $fmtNear.LineAlignment = [System.Drawing.StringAlignment]::Near
  $fmtTypoC = New-Object System.Drawing.StringFormat
  $fmtTypoC.Alignment = [System.Drawing.StringAlignment]::Center
  $fmtTypoC.LineAlignment = [System.Drawing.StringAlignment]::Center

  function Q([double]$x, [double]$y) { New-Object System.Drawing.PointF([float]$x, [float]$y) }
  function Rc([double]$x, [double]$y, [double]$w, [double]$h) { New-Object System.Drawing.RectangleF([float]$x, [float]$y, [float]$w, [float]$h) }
  function SB([System.Drawing.Color]$c) { New-Object System.Drawing.SolidBrush($c) }

  # text = size, x, y, color, font-family, style, tag, container, maxRight
  function Tx([string]$s, [double]$size, [double]$x, [double]$y, [System.Drawing.Color]$col,
              [string]$fam, [System.Drawing.FontStyle]$style, [string]$tag, [string]$box, [double]$maxRight) {
    $f = NewFont $fam $size $style
    $g.DrawString($s, $f, (SB $col), [float]$x, [float]$y, $fmtNear)
    [DrawingKit]::Logged($g, $s, $f, (Q $x $y), $tag, $box, [float]$maxRight) | Out-Null
    $sz = $g.MeasureString($s, $f, (Q 0 0), $fmtTypo)
    $f.Dispose()
    return $sz
  }

  $canvas = Rc 0 0 $W $H

  # ---------- 1. background ----------
  $g.Clear((C $p.bg))
  $band = Rc 0 0 $W 7
  $bb = [DrawingKit]::Linear($band, 0, (C $p.g1), (C $p.g2))
  $g.FillRectangle($bb, $band); $bb.Dispose()

  [DrawingKit]::RadialGlow($g, 250, 40, 430, (CA $p.g1 96))
  [DrawingKit]::RadialGlow($g, 1870, 220, 420, (CA $p.g2 88))
  [DrawingKit]::RadialGlow($g, 1150, 1090, 540, (CA $p.g1 44))

  $gridPen = New-Object System.Drawing.Pen((CA '#ffffff' $p.gridAlpha), 1)
  for ($x = 0; $x -le $W; $x += 64) { $g.DrawLine($gridPen, $x, 0, $x, $H) }
  for ($y = 0; $y -le $H; $y += 64) { $g.DrawLine($gridPen, 0, $y, $W, $y) }
  $gridPen.Dispose()
  [DrawingKit]::RadialFade($g, $canvas, 1180, 480, 1020, 255)

  if ($p.vignette -gt 0) {
    foreach ($corner in @(@(0,0),@($W,0),@(0,$H),@($W,$H))) {
      $rc = Rc ($corner[0]-640) ($corner[1]-640) 1280 1280
      $gp = New-Object System.Drawing.Drawing2D.GraphicsPath
      $gp.AddEllipse($rc)
      $br = New-Object System.Drawing.Drawing2D.PathGradientBrush($gp)
      $br.CenterPoint = Q $corner[0] $corner[1]
      $br.CenterColor = (CA '#000000' $p.vignette)
      $br.SurroundColors = @([System.Drawing.Color]::FromArgb(0,0,0,0))
      $br.SetSigmaBellShape(0.5)
      $g.FillPath($br, $gp)
      $br.Dispose(); $gp.Dispose()
    }
  }

  # ============================================================
  #  2. left column -- brand
  #
  #  Vertical rhythm is written down explicitly because GDI+ reports a text
  #  box (MeasureString) that is much taller than the glyph ink: the 130px
  #  wordmark measures 173px tall while its caps only occupy 97px. Laying out
  #  against the measured box is what once glued the wordmark box (top 181)
  #  onto the badge pill (bottom 180) with zero gap, so the pill border showed
  #  through the tops of the letters. The numbers below are the box tops; the
  #  measured ink tops/bottoms are noted next to each one.
  # ============================================================
  $LX = 100
  $LEFT_MAX = 1050

  $Y_BADGE = 120          # box 120-166, ink 136-157
  $Y_WORD  = 200          # box 200-373, ink 236-332
  $Y_SUB   = 358          # box 358-386, ink 365-381
  $Y_RULE  = 402
  $Y_TAG   = 424          # ink 466-504
  $Y_DESC  = 510

  # badge pill
  $badgeFont = NewFont $FAM 20 ([System.Drawing.FontStyle]::Bold)
  $bs = $g.MeasureString($Copy.badge, $badgeFont, (Q 0 0), $fmtTypo)
  $bh = 46; $bw = [Math]::Ceiling($bs.Width) + 76; $byy = $Y_BADGE
  [DrawingKit]::FillRound($g, (SB (CA $p.accent 34)), $LX, $byy, $bw, $bh, 23)
  $bp = New-Object System.Drawing.Pen((CA $p.border 255), 1.5)
  [DrawingKit]::StrokeRound($g, $bp, $LX, $byy, $bw, $bh, 23); $bp.Dispose()
  $g.FillEllipse((SB (C $p.green)), $LX + 22, $byy + 18, 11, 11)
  $g.DrawString($Copy.badge, $badgeFont, (SB (C $p.accentBright)), $LX + 46, $byy + 12, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.badge, $badgeFont, (Q ($LX+46) ($byy+12)), 'badge', 'left', $LEFT_MAX) | Out-Null
  $badgeFont.Dispose()

  # wordmark (gradient fill)
  $wmFont = NewFont $UI 130 ([System.Drawing.FontStyle]::Bold)
  $wsz = $g.MeasureString($Copy.wordmark, $wmFont, (Q 0 0), $fmtTypo)
  $wx = $LX - 7; $wy = $Y_WORD
  $wb = [DrawingKit]::Linear((Rc $wx $wy ($wsz.Width + 8) $wsz.Height), 18, (C $p.g1), (C $p.g2))
  $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
  $g.DrawString($Copy.wordmark, $wmFont, $wb, $wx, $wy, $fmtNear)
  $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::ClearTypeGridFit
  [DrawingKit]::Logged($g, $Copy.wordmark, $wmFont, (Q $wx $wy), 'wordmark', 'left', $LEFT_MAX) | Out-Null
  $wb.Dispose(); $wmFont.Dispose()

  # pickaxe + grass block badge (stays clear of the wordmark ink: it lives at
  # x 906-984 while the wordmark's last glyph ends near x 735)
  [DrawingKit]::Pickaxe($g, 945, 232, 36, (C $p.g1), (C $p.g2), (C '#9c7248'), (C '#7d5a37'), (C '#a6dd6b'))

  # english subtitle
  Tx $Copy.subtitle 21 $LX $Y_SUB (C $p.dim) $UI ([System.Drawing.FontStyle]::Bold) 'subtitle' 'left' $LEFT_MAX | Out-Null

  # gradient rule
  $ruleRect = Rc $LX $Y_RULE 132 5
  $rb = [DrawingKit]::Linear($ruleRect, 0, (C $p.g1), (C $p.g2))
  [DrawingKit]::FillRound($g, $rb, $LX, $Y_RULE, 132, 5, 3); $rb.Dispose()

  # tagline: three runs, middle one gradient-filled.
  # Consecutive runs are positioned off the measured INK of the previous run
  # plus an explicit gap; using the advance width directly left the gradient run
  # and the tail run only 1px apart (they read as one smudged word).
  $GAP_TAG = 8
  $tgFont = NewFont $FAM 40 ([System.Drawing.FontStyle]::Bold)
  $ty = $Y_TAG
  $x = $LX
  $headInk = [DrawingKit]::InkBox($Copy.taglineHead, $tgFont)
  $g.DrawString($Copy.taglineHead, $tgFont, (SB (C $p.text)), $x, $ty, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.taglineHead, $tgFont, (Q $x $ty), 'tagline-head', 'left', $LEFT_MAX) | Out-Null
  $x += $headInk.Width + $GAP_TAG
  $gradInk = [DrawingKit]::InkBox($Copy.taglineGrad, $tgFont)
  $gb = [DrawingKit]::Linear((Rc $x $ty $gradInk.Width 50), 0, (C $p.g1), (C $p.g2))
  $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
  $g.DrawString($Copy.taglineGrad, $tgFont, $gb, $x, $ty, $fmtNear)
  $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::ClearTypeGridFit
  [DrawingKit]::Logged($g, $Copy.taglineGrad, $tgFont, (Q $x $ty), 'tagline-grad', 'left', $LEFT_MAX) | Out-Null
  $gb.Dispose()
  $x += $gradInk.Width + $GAP_TAG
  $g.DrawString($Copy.taglineTail, $tgFont, (SB (C $p.text)), $x, $ty, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.taglineTail, $tgFont, (Q $x $ty), 'tagline-tail', 'left', $LEFT_MAX) | Out-Null
  $tgFont.Dispose()

  # description (wrapped)
  $dsFont = NewFont $FAM 21 ([System.Drawing.FontStyle]::Regular)
  $lines = [DrawingKit]::Wrap($g, $Copy.desc, $dsFont, 900)
  $dy = $Y_DESC
  foreach ($ln in $lines) {
    $g.DrawString($ln, $dsFont, (SB (C $p.dim)), $LX, $dy, $fmtNear)
    [DrawingKit]::Logged($g, $ln, $dsFont, (Q $LX $dy), 'desc', 'left', $LEFT_MAX) | Out-Null
    $dy += 35
  }
  $dsFont.Dispose()

  # feature chips
  $chFont = NewFont $FAM 18 ([System.Drawing.FontStyle]::Bold)
  $cx = $LX
  $cy = 640
  foreach ($ct in $Copy.chips) {
    $cs = $g.MeasureString($ct, $chFont, (Q 0 0), $fmtTypo)
    $cw = [Math]::Ceiling($cs.Width) + 58
    [DrawingKit]::FillRound($g, (SB (C $p.card)), $cx, $cy, $cw, 44, 12)
    $cp = New-Object System.Drawing.Pen((C $p.border), 1.3)
    [DrawingKit]::StrokeRound($g, $cp, $cx, $cy, $cw, 44, 12); $cp.Dispose()
    $g.FillEllipse((SB (C $p.accent)), $cx + 20, $cy + 18, 9, 9)
    $g.DrawString($ct, $chFont, (SB (C $p.text)), $cx + 38, $cy + 11, $fmtNear)
    [DrawingKit]::Logged($g, $ct, $chFont, (Q ($cx+38) ($cy+11)), 'chip', 'left', $LEFT_MAX) | Out-Null
    $cx += $cw + 16
  }
  $chFont.Dispose()

  # ============================================================
  #  3. right column -- the real web UI, not a terminal
  #
  #  Structure mirrors src/ui/static/index.html one-for-one:
  #    #topbar   -> brand / model chip / status dot,  height 54 in CSS
  #    #messages -> .welcome, .msg-row.user, .msg-row.assistant,
  #                 .tool-line pills, .mod-cards grid
  #    #presetbar-> version field + loader .seg control + hint,  pill shape
  #    #inputbar -> #input textarea + #btn-send, gradient button
  #  Colour roles come from style.css: topbar/inputbar use --bg-panel,
  #  message rows use --bg-raise, mod cards --bg-raise with --border-soft,
  #  the assistant bubble --bg-bubble, the user bubble --grad.
  # ============================================================
  $PX = 1080; $PY = 120; $PW = 780; $PH = 800; $PR = 22
  $PMAX = $PX + $PW - 20

  [DrawingKit]::SoftShadow($g, $PX, $PY, $PW, $PH, $PR, 26, $p.shadow)
  # The panel body needs to read as a surface distinct from the page. Blending
  # --bg-panel at its CSS alpha over --bg lands almost exactly on --bg (both are
  # near #0d0f14), which left the panel defined by its 1.5px border alone. Mix
  # towards --bg-card so the window has real body while staying in palette.
  # MixHex returns "#rrggbb" so it can go straight back through CA.
  $panelHex = [DrawingKit]::MixHex($p.panel, $p.card, 0.55)
  [DrawingKit]::FillRound($g, (SB (CA $panelHex 252)), $PX, $PY, $PW, $PH, $PR)
  $pp = New-Object System.Drawing.Pen((C $p.border), 1.5)
  [DrawingKit]::StrokeRound($g, $pp, $PX, $PY, $PW, $PH, $PR); $pp.Dispose()

  # ---- topbar (54 tall, like #topbar) ----
  $barH = 54
  $clip = [DrawingKit]::RoundedRect($PX, $PY, $PW, $PH, $PR)
  $g.SetClip($clip)
  $g.FillRectangle((SB (CA $p.raise 250)), $PX, $PY, $PW, $barH)
  $g.ResetClip()
  $sep = New-Object System.Drawing.Pen((C $p.border), 1)
  $g.DrawLine($sep, $PX, $PY + $barH, $PX + $PW, $PY + $barH); $sep.Dispose()

  # .brand-icon: 28px gradient square with the pickaxe, then the brand text
  $bi = [DrawingKit]::Linear((Rc ($PX+20) ($PY+13) 28 28), 45, (C $p.g1), (C $p.g2))
  [DrawingKit]::FillRound($g, $bi, $PX + 20, $PY + 13, 28, 28, 9); $bi.Dispose()
  $brandIconFont = NewFont $FAM 14 ([System.Drawing.FontStyle]::Regular)
  $g.DrawString($Copy.glyphPick, $brandIconFont, (SB ([System.Drawing.Color]::White)), $PX + 26, $PY + 19, $fmtNear)
  $brandIconFont.Dispose()

  $brandFont = NewFont $FAM 15 ([System.Drawing.FontStyle]::Bold)
  $g.DrawString($Copy.mockBrand, $brandFont, (SB (C $p.text)), $PX + 56, $PY + 17, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.mockBrand, $brandFont, (Q ($PX+56) ($PY+17)), 'mock-brand', 'topbar', $PMAX) | Out-Null
  $brandFont.Dispose()

  # .ver pill
  $vf = NewFont $MONO 11 ([System.Drawing.FontStyle]::Regular)
  $vInk = [DrawingKit]::InkBox($Copy.mockVersion, $vf)
  $vx = $PX + 56 + [DrawingKit]::InkBox($Copy.mockBrand, (NewFont $FAM 15 ([System.Drawing.FontStyle]::Bold))).Width + 12
  $vw = [Math]::Ceiling($vInk.Width) + 18
  [DrawingKit]::FillRound($g, (SB (CA $p.border 120)), $vx, $PY + 17, $vw, 22, 11)
  $g.DrawString($Copy.mockVersion, $vf, (SB (C $p.dim)), $vx + 9, $PY + 20, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.mockVersion, $vf, (Q ($vx+9) ($PY+20)), 'mock-ver', 'topbar', $PMAX) | Out-Null
  $vf.Dispose()

  # .model chip
  $mf = NewFont $MONO 12 ([System.Drawing.FontStyle]::Regular)
  $mInk = [DrawingKit]::InkBox($Copy.mockModel, $mf)
  $mw = [Math]::Ceiling($mInk.Width) + 22
  $mx = $vx + $vw + 10
  [DrawingKit]::FillRound($g, (SB (CA $p.text 18)), $mx, $PY + 17, $mw, 22, 11)
  $g.DrawString($Copy.mockModel, $mf, (SB (C $p.dim)), $mx + 11, $PY + 20, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.mockModel, $mf, (Q ($mx+11) ($PY+20)), 'mock-model', 'topbar', $PMAX) | Out-Null
  $mf.Dispose()

  # .status: dot.ok + text, right aligned; then the two .icon-btn glyphs
  $gearF = NewFont $FAM 15 ([System.Drawing.FontStyle]::Regular)
  $g.DrawString($Copy.glyphGear, $gearF, (SB (C $p.dim)), $PX + $PW - 40, $PY + 18, $fmtNear)
  $sunF = NewFont $FAM 15 ([System.Drawing.FontStyle]::Regular)
  $g.DrawString($Copy.glyphSun, $sunF, (SB (C $p.dim)), $PX + $PW - 72, $PY + 18, $fmtNear)
  $sunF.Dispose(); $gearF.Dispose()

  $stF = NewFont $FAM 12 ([System.Drawing.FontStyle]::Regular)
  $stInk = [DrawingKit]::InkBox($Copy.mockStatus, $stF)
  $stX = $PX + $PW - 100 - $stInk.Width
  $g.FillEllipse((SB (C $p.green)), $stX - 15, $PY + 23, 9, 9)
  $g.DrawString($Copy.mockStatus, $stF, (SB (C $p.dim)), $stX, $PY + 20, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.mockStatus, $stF, (Q $stX ($PY+20)), 'mock-status', 'topbar', $PMAX) | Out-Null
  $stF.Dispose()

  # ---- chat body ----
  $bx = $PX + 26
  $bodyW = $PW - 52
  $by = $PY + $barH + 22

  # .welcome: 54px gradient logo tile, heading, hint line
  $wl = [DrawingKit]::Linear((Rc ($PX + $PW/2 - 31) $by 62 62), 45, (C $p.g1), (C $p.g2))
  [DrawingKit]::FillRound($g, $wl, $PX + $PW/2 - 31, $by, 62, 62, 16); $wl.Dispose()
  $wlF = NewFont $FAM 28 ([System.Drawing.FontStyle]::Regular)
  $g.DrawString($Copy.glyphPick, $wlF, (SB ([System.Drawing.Color]::White)), $PX + $PW/2 - 20, $by + 12, $fmtNear)
  $wlF.Dispose()

  $wcF = NewFont $FAM 22 ([System.Drawing.FontStyle]::Bold)
  $wcInk = [DrawingKit]::InkBox($Copy.mockWelcome, $wcF)
  $wcX = $PX + ($PW - $wcInk.Width) / 2
  $g.DrawString($Copy.mockWelcome, $wcF, (SB (C $p.text)), $wcX, $by + 76, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.mockWelcome, $wcF, (Q $wcX ($by+76)), 'mock-welcome', 'mock', $PMAX) | Out-Null
  $wcF.Dispose()

  $whF = NewFont $FAM 14 ([System.Drawing.FontStyle]::Regular)
  $whInk = [DrawingKit]::InkBox($Copy.mockWelcomeHint, $whF)
  $g.DrawString($Copy.mockWelcomeHint, $whF, (SB (C $p.dim)), $PX + ($PW - $whInk.Width) / 2, $by + 110, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.mockWelcomeHint, $whF, (Q ($PX + ($PW - $whInk.Width) / 2) ($by+110)), 'mock-welcome-hint', 'mock', $PMAX) | Out-Null
  $whF.Dispose()

  # .msg-row.user  (right aligned, --grad bubble, squared bottom-right corner)
  $uf = NewFont $FAM 15 ([System.Drawing.FontStyle]::Regular)
  $uInk = [DrawingKit]::InkBox($Copy.userMsg, $uf)
  $uw = [Math]::Ceiling($uInk.Width) + 30
  $ux = $PX + $PW - 26 - $uw
  $uy = $by + 158
  $ub = [DrawingKit]::Linear((Rc $ux $uy $uw 40), 0, (C $p.g1), (C $p.g2))
  $g.FillPath($ub, [DrawingKit]::RoundedRect($ux, $uy, $uw, 40, 14))
  $g.FillRectangle($ub, $ux + $uw - 14, $uy + 26, 14, 14)
  $ub.Dispose()
  $g.DrawString($Copy.userMsg, $uf, (SB ([System.Drawing.Color]::White)), $ux + 15, $uy + 10, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.userMsg, $uf, (Q ($ux+15) ($uy+10)), 'user-msg', 'mock', ($ux + $uw - 15)) | Out-Null
  $uf.Dispose()

  # .msg-row.assistant: gradient avatar + --bg-bubble bubble, squared bottom-left
  $ay = $uy + 58
  $av = [DrawingKit]::Linear((Rc $bx $ay 30 30), 45, (C $p.g1), (C $p.g2))
  [DrawingKit]::FillRound($g, $av, $bx, $ay, 30, 30, 10); $av.Dispose()
  $avF = NewFont $FAM 12 ([System.Drawing.FontStyle]::Bold)
  $g.DrawString($Copy.glyphSpark, $avF, (SB ([System.Drawing.Color]::White)), $bx + 8, $ay + 7, $fmtNear)
  $avF.Dispose()

  $af = NewFont $FAM 15 ([System.Drawing.FontStyle]::Regular)
  $aInk = [DrawingKit]::InkBox($Copy.assistantMsg, $af)
  $abW = $aInk.Width + 32
  $abX = $bx + 40
  $abH = 44
  [DrawingKit]::FillRound($g, (SB (C $p.bubble)), $abX, $ay - 2, $abW, $abH, 14)
  $g.FillRectangle((SB (C $p.bubble)), $abX, $ay + $abH - 16, 14, 14)
  $g.DrawString($Copy.assistantMsg, $af, (SB (C $p.text)), $abX + 16, $ay + 10, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.assistantMsg, $af, (Q ($abX+16) ($ay+10)), 'assistant-msg', 'mock', ($abX + $abW - 16)) | Out-Null
  $af.Dispose()

  # .tool-line pills (monospace, 999px radius, status colour per class)
  $ty2 = $ay + 54
  $tubeF = NewFont $FAM 13 ([System.Drawing.FontStyle]::Regular)
  $tnameF = NewFont $MONO 13 ([System.Drawing.FontStyle]::Bold)
  $targF = NewFont $MONO 12 ([System.Drawing.FontStyle]::Regular)
  foreach ($toolItem in $Copy.tools) {
    $rowH = 30
    $tnInk = [DrawingKit]::InkBox($toolItem.name, $tnameF)
    $taInk = [DrawingKit]::InkBox($toolItem.arg, $targF)
    $pillW = 30 + $tnInk.Width + 8 + $taInk.Width + 20
    $stateCol = $p.green
    if ($toolItem.state -eq 'pending') { $stateCol = $p.yellow }
    $tsInk = [DrawingKit]::InkBox($toolItem.state, $targF)

    [DrawingKit]::FillRound($g, (SB (CA $stateCol 26)), $bx + 40, $ty2, $pillW, $rowH, 15)
    $tp = New-Object System.Drawing.Pen((CA $stateCol 70), 1.2)
    [DrawingKit]::StrokeRound($g, $tp, $bx + 40, $ty2, $pillW, $rowH, 15); $tp.Dispose()

    $g.DrawString($Copy.glyphGear, $tubeF, (SB (C $stateCol)), $bx + 50, $ty2 + 6, $fmtNear)
    $g.DrawString($toolItem.name, $tnameF, (SB (C $stateCol)), $bx + 70, $ty2 + 6, $fmtNear)
    [DrawingKit]::Logged($g, $toolItem.name, $tnameF, (Q ($bx+70) ($ty2+6)), 'tool-name', 'mock', $PMAX) | Out-Null
    $g.DrawString($toolItem.arg, $targF, (SB (C $p.dim)), $bx + 78 + $tnInk.Width, $ty2 + 7, $fmtNear)
    [DrawingKit]::Logged($g, $toolItem.arg, $targF, (Q ($bx+78+$tnInk.Width) ($ty2+7)), 'tool-arg', 'mock', $PMAX) | Out-Null

    $sx = $bx + 40 + $pillW + 10
    $g.FillEllipse((SB (C $stateCol)), $sx, $ty2 + 11, 8, 8)
    $g.DrawString($toolItem.state, $targF, (SB (C $stateCol)), $sx + 14, $ty2 + 7, $fmtNear)
    [DrawingKit]::Logged($g, $toolItem.state, $targF, (Q ($sx+14) ($ty2+7)), 'tool-state', 'mock', $PMAX) | Out-Null

    $ty2 += $rowH + 9
  }
  $tubeF.Dispose(); $tnameF.Dispose(); $targF.Dispose()

  # progress bar + result count, styled as the real .progress tool-line
  $pgF = NewFont $MONO 15 ([System.Drawing.FontStyle]::Bold)
  $pgInk = [DrawingKit]::InkBox($Copy.progressBar, $pgF)
  [DrawingKit]::FillRound($g, (SB (CA $p.accent 40)), $bx + 40, $ty2, $pgInk.Width + 24, 28, 14)
  $g.DrawString($Copy.progressBar, $pgF, (SB (C $p.accentBright)), $bx + 52, $ty2 + 5, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.progressBar, $pgF, (Q ($bx+52) ($ty2+5)), 'progress', 'mock', $PMAX) | Out-Null
  $pgF.Dispose()
  $ptF = NewFont $FAM 13 ([System.Drawing.FontStyle]::Regular)
  $g.DrawString($Copy.progressText, $ptF, (SB (C $p.dim)), $bx + 40 + $pgInk.Width + 38, $ty2 + 6, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.progressText, $ptF, (Q ($bx+40+$pgInk.Width+38) ($ty2+6)), 'progress-text', 'mock', $PMAX) | Out-Null
  $ptF.Dispose()
  $ty2 += 42

  # .mod-cards grid: caption row, then 2 columns
  $capF = NewFont $MONO 12 ([System.Drawing.FontStyle]::Regular)
  $g.DrawString($Copy.modListHead, $capF, (SB (C $p.dim)), $bx + 40, $ty2, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.modListHead, $capF, (Q ($bx+40) $ty2), 'modlist-cap', 'mock', $PMAX) | Out-Null
  $capF.Dispose()
  $ty2 += 22

  $cardW = ($bodyW - 40 - 12) / 2
  $cardH = 86
  $modIconF = NewFont $FAM 17 ([System.Drawing.FontStyle]::Regular)
  $modNameF = NewFont $MONO 13 ([System.Drawing.FontStyle]::Bold)
  $modDescF = NewFont $FAM 11 ([System.Drawing.FontStyle]::Regular)
  $modMetaF = NewFont $MONO 11 ([System.Drawing.FontStyle]::Regular)
  for ($mi = 0; $mi -lt $Copy.mods.Count; $mi++) {
    $amd = $Copy.mods[$mi]
    $col = $mi % 2
    $row = [Math]::Floor($mi / 2)
    $mx2 = $bx + 40 + $col * ($cardW + 12)
    $my2 = $ty2 + $row * ($cardH + 12)

    [DrawingKit]::FillRound($g, (SB (C $p.raise)), $mx2, $my2, $cardW, $cardH, 11)
    $mp = New-Object System.Drawing.Pen((CA $p.border 150), 1.2)
    [DrawingKit]::StrokeRound($g, $mp, $mx2, $my2, $cardW, $cardH, 11); $mp.Dispose()

    # .mod-card-icon 36px
    $ib = [DrawingKit]::Linear((Rc ($mx2+12) ($my2+12) 36 36), 45, (CA $p.g1 80), (CA $p.g2 80))
    [DrawingKit]::FillRound($g, $ib, $mx2 + 12, $my2 + 12, 36, 36, 8); $ib.Dispose()
    $g.DrawString($amd.icon, $modIconF, (SB (C $p.accentBright)), $mx2 + 21, $my2 + 20, $fmtNear)

    # .mod-card-name (accent-bright, mono) + .mod-card-meta (dim)
    $g.DrawString($amd.title, $modNameF, (SB (C $p.accentBright)), $mx2 + 56, $my2 + 12, $fmtNear)
    [DrawingKit]::Logged($g, $amd.title, $modNameF, (Q ($mx2+56) ($my2+12)), 'mod-name', 'mock', $PMAX) | Out-Null
    $g.DrawString($amd.slug, $modMetaF, (SB (C $p.dim)), $mx2 + 56, $my2 + 32, $fmtNear)
    [DrawingKit]::Logged($g, $amd.slug, $modMetaF, (Q ($mx2+56) ($my2+32)), 'mod-slug', 'mock', $PMAX) | Out-Null

    # .mod-card-desc, wrapped to the card width (max 2 lines, -webkit-line-clamp)
    $descLines = [DrawingKit]::Wrap($g, $amd.desc, $modDescF, ($cardW - 24))
    $dl = 0
    foreach ($descLine in $descLines) {
      if ($dl -ge 2) { break }
      $g.DrawString($descLine, $modDescF, (SB (C $p.dim)), $mx2 + 12, $my2 + 48 + $dl * 14, $fmtNear)
      [DrawingKit]::Logged($g, $descLine, $modDescF, (Q ($mx2+12) ($my2+48+$dl*14)), 'mod-desc', 'mock', ($mx2 + $cardW - 12)) | Out-Null
      $dl++
    }

    # .mod-card-meta downloads, right aligned under the icon
    $dInk = [DrawingKit]::InkBox($amd.dl, $modMetaF)
    $dx = $mx2 + $cardW - 12 - $dInk.Width
    $g.DrawString($amd.dl, $modMetaF, (SB (C $p.green)), $dx, $my2 + 64, $fmtNear)
    [DrawingKit]::Logged($g, $amd.dl, $modMetaF, (Q $dx ($my2+64)), 'mod-dl', 'mock', ($mx2 + $cardW - 12)) | Out-Null
  }
  $modIconF.Dispose(); $modNameF.Dispose(); $modDescF.Dispose(); $modMetaF.Dispose()
  $rows = [Math]::Ceiling($Copy.mods.Count / 2)
  $ty2 += $rows * ($cardH + 12)

  # ---- #presetbar and #inputbar, anchored to the bottom edge of the panel
  #      exactly like the real layout (main is a flex column with the input
  #      area pinned last), so the mock reads as a full-height window ----
  $inH = 46
  $pbH = 40
  $bottom = $PY + $PH - 38
  $inY = $bottom - $inH
  $pbY = $inY - 12 - $pbH

  $pbF = NewFont $FAM 11 ([System.Drawing.FontStyle]::Regular)
  $vvF = NewFont $MONO 12 ([System.Drawing.FontStyle]::Regular)
  $sgF = NewFont $MONO 12 ([System.Drawing.FontStyle]::Regular)

  [DrawingKit]::FillRound($g, (SB (CA $p.raise 250)), $bx + 40, $pbY, $bodyW - 40, $pbH, 20)
  $pbp = New-Object System.Drawing.Pen((CA $p.border 140), 1.2)
  [DrawingKit]::StrokeRound($g, $pbp, $bx + 40, $pbY, $bodyW - 40, $pbH, 20); $pbp.Dispose()

  $pcx = $bx + 58
  $g.DrawString($Copy.labelVersion, $pbF, (SB (C $p.dim)), $pcx, $pbY + 14, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.labelVersion, $pbF, (Q $pcx ($pbY+14)), 'preset-label-ver', 'mock', $PMAX) | Out-Null
  $pcx += [DrawingKit]::InkBox($Copy.labelVersion, $pbF).Width + 10

  $vvInk = [DrawingKit]::InkBox($Copy.mockVersionInput, $vvF)
  [DrawingKit]::FillRound($g, (SB (CA $p.text 20)), $pcx, $pbY + 8, $vvInk.Width + 22, 24, 9)
  $g.DrawString($Copy.mockVersionInput, $vvF, (SB (C $p.text)), $pcx + 11, $pbY + 12, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.mockVersionInput, $vvF, (Q ($pcx+11) ($pbY+12)), 'preset-version', 'mock', $PMAX) | Out-Null
  $pcx += $vvInk.Width + 22 + 14

  $g.DrawLine((New-Object System.Drawing.Pen((CA $p.border 180), 1)), $pcx, $pbY + 10, $pcx, $pbY + 30)
  $pcx += 14

  $g.DrawString($Copy.labelLoader, $pbF, (SB (C $p.dim)), $pcx, $pbY + 14, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.labelLoader, $pbF, (Q $pcx ($pbY+14)), 'preset-label-loader', 'mock', $PMAX) | Out-Null
  $pcx += [DrawingKit]::InkBox($Copy.labelLoader, $pbF).Width + 10

  foreach ($loaderName in $Copy.loaders) {
    $lInk = [DrawingKit]::InkBox($loaderName, $sgF)
    $segW = $lInk.Width + 20
    $isActive = ($loaderName -eq $Copy.loaders[0])
    if ($isActive) {
      [DrawingKit]::FillRound($g, (SB (C $p.bubble)), $pcx, $pbY + 8, $segW, 24, 7)
      $segCol = $p.accentBright
    } else {
      $segCol = $p.dim
    }
    $g.DrawString($loaderName, $sgF, (SB (C $segCol)), $pcx + 10, $pbY + 12, $fmtNear)
    [DrawingKit]::Logged($g, $loaderName, $sgF, (Q ($pcx+10) ($pbY+12)), 'preset-loader', 'mock', $PMAX) | Out-Null
    $pcx += $segW + 4
  }
  $pbF.Dispose(); $vvF.Dispose(); $sgF.Dispose()

  # ---- #inputbar: textarea + gradient send button ----
  $inF = NewFont $FAM 14 ([System.Drawing.FontStyle]::Regular)
  $sendW = 92
  $taW = $bodyW - 40 - $sendW - 10
  [DrawingKit]::FillRound($g, (SB (C $p.card)), $bx + 40, $inY, $taW, $inH, 14)
  $tap = New-Object System.Drawing.Pen((C $p.border), 1.2)
  [DrawingKit]::StrokeRound($g, $tap, $bx + 40, $inY, $taW, $inH, 14); $tap.Dispose()
  $g.DrawString($Copy.inputPlaceholder, $inF, (SB (C $p.dim)), $bx + 54, $inY + 14, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.inputPlaceholder, $inF, (Q ($bx+54) ($inY+14)), 'input-placeholder', 'mock', ($bx + 40 + $taW - 14)) | Out-Null
  $inF.Dispose()

  $sb = [DrawingKit]::Linear((Rc ($bx + 40 + $taW + 10) $inY $sendW $inH), 0, (C $p.g1), (C $p.g2))
  [DrawingKit]::FillRound($g, $sb, $bx + 40 + $taW + 10, $inY, $sendW, $inH, 13); $sb.Dispose()
  $sendF = NewFont $FAM 15 ([System.Drawing.FontStyle]::Bold)
  $sendInk = [DrawingKit]::InkBox($Copy.sendLabel, $sendF)
  $sx2 = $bx + 40 + $taW + 10 + ($sendW - $sendInk.Width) / 2
  $g.DrawString($Copy.sendLabel, $sendF, (SB ([System.Drawing.Color]::White)), $sx2, $inY + 13, $fmtNear)
  [DrawingKit]::Logged($g, $Copy.sendLabel, $sendF, (Q $sx2 ($inY+13)), 'send-label', 'mock', ($bx + 40 + $taW + 10 + $sendW - 6)) | Out-Null
  $sendF.Dispose()

  # ============================================================
  #  5. footer cards
  # ============================================================
  $fy = 988
  $sep2 = New-Object System.Drawing.Pen((CA $p.border 200), 1.4)
  $g.DrawLine($sep2, 70, $fy, $W - 70, $fy); $sep2.Dispose()

  $colW = 431; $colGap = 18; $cardH = 96
  $colX = @(70, (70 + $colW + $colGap), (70 + ($colW + $colGap) * 2), (70 + ($colW + $colGap) * 3))
  $kf = NewFont $UI 13 ([System.Drawing.FontStyle]::Bold)
  for ($i = 0; $i -lt $Copy.cards.Count; $i++) {
    $footerCard = $Copy.cards[$i]
    $x0 = $colX[$i]
    $cmax = $x0 + $colW - 16

    [DrawingKit]::FillRound($g, (SB (C $p.card)), $x0, $fy + 16, $colW, $cardH, 14)
    $cp2 = New-Object System.Drawing.Pen((C $p.border), 1.3)
    [DrawingKit]::StrokeRound($g, $cp2, $x0, $fy + 16, $colW, $cardH, 14); $cp2.Dispose()

    $g.DrawString($footerCard.key, $kf, (SB (C $p.dim)), $x0 + 20, $fy + 30, $fmtNear)
    [DrawingKit]::Logged($g, $footerCard.key, $kf, (Q ($x0+20) ($fy+30)), 'card-key', ('card' + $i), $cmax) | Out-Null

    if ($footerCard.pills) {
      $px2 = $x0 + 20
      $fontProg2 = NewFont $MONO 14 ([System.Drawing.FontStyle]::Bold)
      foreach ($pl in $footerCard.pills) {
        $psz = $g.MeasureString($pl, $fontProg2, (Q 0 0), $fmtTypo)
        $pw3 = [Math]::Ceiling($psz.Width) + 26
        [DrawingKit]::FillRound($g, (SB (C $p.raise)), $px2, $fy + 54, $pw3, 30, 8)
        $ppp = New-Object System.Drawing.Pen((C $p.border), 1.1)
        [DrawingKit]::StrokeRound($g, $ppp, $px2, $fy + 54, $pw3, 30, 8); $ppp.Dispose()
        $g.DrawString($pl, $fontProg2, (SB (C $p.text)), $px2 + 13, $fy + 61, $fmtNear)
        [DrawingKit]::Logged($g, $pl, $fontProg2, (Q ($px2+13) ($fy+65)), 'pill', ('card' + $i), $cmax) | Out-Null
        $px2 += $pw3 + 8
      }
      $fontProg2.Dispose()
    }
    else {
      $fam1 = if ($footerCard.mono1) { $MONO } else { $FAM }
      $v1f = NewFont $fam1 18 ([System.Drawing.FontStyle]::Bold)
      $v1c = if ($footerCard.hl1) { C $p.accentBright } else { C $p.text }
      $g.DrawString($footerCard.v1, $v1f, (SB $v1c), $x0 + 20, $fy + 54, $fmtNear)
      [DrawingKit]::Logged($g, $footerCard.v1, $v1f, (Q ($x0+20) ($fy+58)), 'card-v1', ('card' + $i), $cmax) | Out-Null
      $v1f.Dispose()
      if ($footerCard.v2) {
        $w1 = [DrawingKit]::InkBox($footerCard.v1, (NewFont $fam1 18 ([System.Drawing.FontStyle]::Bold))).Width + 12
        $v2f = NewFont $FAM 15 ([System.Drawing.FontStyle]::Regular)
        $g.DrawString($footerCard.v2, $v2f, (SB (C $p.dim)), $x0 + 26 + $w1, $fy + 58, $fmtNear)
        [DrawingKit]::Logged($g, $footerCard.v2, $v2f, (Q ($x0+26+$w1) ($fy+58)), 'card-v2', ('card' + $i), $cmax) | Out-Null
        $v2f.Dispose()
      }
    }
  }
  $kf.Dispose()

  # ---------- save ----------
  $g.Dispose()
  $bmp.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()

  return [DrawingKit]::Audit($W, $H, 0)
}

# ============================================================
#  run
# ============================================================
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$copy = [System.IO.File]::ReadAllText((Join-Path $here 'copy.json'), [System.Text.Encoding]::UTF8) | ConvertFrom-Json

$destDir = if ($OutDir -ne '') { $OutDir } else { $here }
if (-not (Test-Path $destDir)) { New-Item -ItemType Directory -Path $destDir -Force | Out-Null }

$themeNames = if ($Theme -eq 'all') { @('dark', 'emerald', 'light') } else { @($Theme) }

foreach ($themeName in $themeNames) {
  $fileName = 'modsmith-cover-' + $themeName + '.png'
  $out = Join-Path $destDir $fileName

  # This workspace is labelled Low integrity + no-write-up, so a low-integrity
  # GDI+ process may be refused when writing inside the repo ("A generic error
  # occurred in GDI+"). Render into the sandbox temp area instead, then try to
  # move it into place; the file sandbox tool performs that move when this
  # process cannot.
  $stage = Join-Path $env:TEMP $fileName
  $issues = $null
  try {
    $issues = Render-Cover $themeName $out $copy ([bool]$Verify)
    $final = $out
  }
  catch [System.Runtime.InteropServices.ExternalException] {
    $issues = Render-Cover $themeName $stage $copy ([bool]$Verify)
    $final = $stage
    try { Copy-Item -LiteralPath $stage -Destination $out -Force -ErrorAction Stop } catch { }
  }
  if (Test-Path $out) { $final = $out }

  $bmp = [System.Drawing.Bitmap]::FromFile($final)
  $w = $bmp.Width; $h = $bmp.Height
  $s1 = $bmp.GetPixel(20, 20)       # background
  $s2 = $bmp.GetPixel(150, 290)     # wordmark gradient
  $s3 = $bmp.GetPixel(1450, 300)    # glass panel
  $s4 = $bmp.GetPixel(200, 1035)    # footer card
  $bmp.Dispose()

  $ok = if ($w -eq 1920 -and $h -eq 1080) { 'OK ' } else { 'BAD' }
  Write-Output ('{0,-8} {1}x{2} {3} audit-issues={4}' -f $themeName, $w, $h, $ok, $issues.Count)
  Write-Output ('         bg=#{0:X2}{1:X2}{2:X2} wordmark=#{3:X2}{4:X2}{5:X2} panel=#{6:X2}{7:X2}{8:X2} card=#{9:X2}{10:X2}{11:X2}' -f `
      $s1.R,$s1.G,$s1.B, $s2.R,$s2.G,$s2.B, $s3.R,$s3.G,$s3.B, $s4.R,$s4.G,$s4.B)
  if ($final -ne $out) { Write-Output ('         staged at: ' + $final) }

  if ($Verify -and $issues.Count -gt 0) {
    $issues | Select-Object -First 40 | ForEach-Object { Write-Output ('         ! ' + $_) }
  }
}
