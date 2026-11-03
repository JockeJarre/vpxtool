$ErrorActionPreference = 'Continue'
$root = 'D:\Projects\Testbench'
$work = 'D:\git\Flipper\DMDConverter'
$bin = "$work\target\release\dmdconverter.exe"
$log = "$work\testbench"

$pre = Import-Csv "$log-pre.csv"

# ---- 1. changed files are backed up, untouched files are byte- and time-identical ----
$changed = @(); $untouched = @(); $badBak = @(); $badPlain = @(); $touched = @()
foreach ($r in $pre) {
  $bak = "$($r.Path).bak"
  if (Test-Path -LiteralPath $bak) {
    $changed += $r.Path
    $bh = (Get-FileHash -Algorithm SHA256 -LiteralPath $bak).Hash
    if ($bh -ne $r.Sha256) { $badBak += $r.Path }
  } else {
    $untouched += $r.Path
    $h = (Get-FileHash -Algorithm SHA256 -LiteralPath $r.Path).Hash
    if ($h -ne $r.Sha256) { $badPlain += $r.Path }
    if ((Get-Item -LiteralPath $r.Path).LastWriteTimeUtc.ToString('o') -ne $r.LastWriteTimeUtc) { $touched += $r.Path }
  }
}
".bak identical to the original: $($changed.Count - $badBak.Count) of $($changed.Count) changed files"
"untouched files (no .bak): $($untouched.Count)"
"untouched files with a new timestamp: $($touched.Count)"
if ($badBak.Count)   { "BAK HASH MISMATCH: " + ($badBak -join ', ') }
if ($badPlain.Count) { "UNTOUCHED FILE CHANGED: " + ($badPlain -join ', ') }
if ($touched.Count)  { "TOUCHED WITHOUT CHANGE: " + ($touched -join ', ') }

# ---- 2. the splice is confined to the tag: common prefix/suffix of .bak and file ----
function CommonPrefix([byte[]]$a, [byte[]]$b) {
  $n = [Math]::Min($a.Length, $b.Length); $block = 1MB; $p = 0
  while ($p -lt $n) {
    $len = [Math]::Min($block, $n - $p)
    if ([Convert]::ToBase64String($a, $p, $len) -ne [Convert]::ToBase64String($b, $p, $len)) { break }
    $p += $len
  }
  while ($p -lt $n -and $a[$p] -eq $b[$p]) { $p++ }
  return $p
}
function CommonSuffix([byte[]]$a, [byte[]]$b, [int]$floor) {
  $max = [Math]::Min($a.Length, $b.Length) - $floor
  $block = 1MB; $s = 0
  while ($s -lt $max) {
    $len = [Math]::Min($block, $max - $s)
    $oa = $a.Length - $s - $len; $ob = $b.Length - $s - $len
    if ([Convert]::ToBase64String($a, $oa, $len) -ne [Convert]::ToBase64String($b, $ob, $len)) { break }
    $s += $len
  }
  while ($s -lt $max -and $a[$a.Length - 1 - $s] -eq $b[$b.Length - 1 - $s]) { $s++ }
  return $s
}
function Slice([byte[]]$x, [int]$from, [int]$to) {
  if ($to -le $from) { return '' }
  return [Text.Encoding]::UTF8.GetString($x[$from..($to - 1)])
}
$pairs = Get-ChildItem $root -File |
  Where-Object { $_.Name -like '*.directb2s.bak' -or $_.Name -like '*.b2spro.bak' } |
  Sort-Object Length -Descending | Select-Object -First 6
foreach ($bak in $pairs) {
  $new = $bak.FullName -replace '\.bak$', ''
  $a = [IO.File]::ReadAllBytes($bak.FullName)
  $b = [IO.File]::ReadAllBytes($new)
  $p = CommonPrefix $a $b
  $s = CommonSuffix $a $b ($p + 1)
  $old = Slice $a $p ($a.Length - $s)
  $ins = Slice $b $p ($b.Length - $s)
  "{0}`n  bytes {1} -> {2}   prefix {3} identical, suffix {4} identical" -f $bak.Name, $a.Length, $b.Length, $p, $s
  "  old: " + ($old -replace "`r`n", '\n')
  "  new: " + ($ins -replace "`r`n", '\n')
}

# ---- 3. a second run writes nothing and leaves every timestamp alone ----
$before = @{}
Get-ChildItem $root -File |
  Where-Object { $_.Extension -match '^\.(directb2s|b2spro)$' } |
  ForEach-Object { $before[$_.FullName] = "$($_.Length)|$($_.LastWriteTimeUtc.ToString('o'))" }
& $bin scan $root --ini "$work\DmdDevice.ini" --method template --json "$log-second.json" --write `
  > "$log-second.log" 2>&1
"second run exit $LASTEXITCODE"
$written = (Select-String -Path "$log-second.log" -Pattern '^\s+written\s+(\d+)' |
  ForEach-Object { [int]$_.Matches[0].Groups[1].Value } | Measure-Object -Sum).Sum
if ($null -eq $written) { $written = 0 }
"second run written: $written"
$after = @{}
Get-ChildItem $root -File |
  Where-Object { $_.Extension -match '^\.(directb2s|b2spro)$' } |
  ForEach-Object { $after[$_.FullName] = "$($_.Length)|$($_.LastWriteTimeUtc.ToString('o'))" }
$moved = @($before.Keys | Where-Object { $after[$_] -ne $before[$_] })
"files changed by the second run: $($moved.Count)"
if ($moved.Count) { $moved | Select-Object -First 10 }
Get-Content "$log-second.log" -Tail 8

# ---- 4. every planned file parses and carries the stamped numbers ----
$json = Get-Content "$log-plan.json" -Raw | ConvertFrom-Json
$want = @{}
foreach ($r in $json) {
  if (-not $r.b2s -or -not $r.chosen) { continue }
  if (-not $want.ContainsKey($r.b2s) -or $r.match_kind -eq 'exact') { $want[$r.b2s] = $r }
}
"files in the plan with a rect: $($want.Count), changed on disk: $($changed.Count)"
$files = @($want.Keys | Sort-Object)
$parsed = $files | ForEach-Object -Parallel {
  $path = $_
  $dmd = $null; $err = $null
  try {
    $xr = [System.Xml.XmlReader]::Create($path)
    try {
      while ($xr.Read()) {
        if ($xr.NodeType -eq 'Element' -and $xr.Name -eq 'DMD' -and $xr.GetAttribute('Target') -eq 'Backglass') {
          $dmd = [pscustomobject]@{
            X = $xr.GetAttribute('X'); Y = $xr.GetAttribute('Y')
            W = $xr.GetAttribute('W'); H = $xr.GetAttribute('H')
            G = $xr.GetAttribute('GlassRoughness')
          }
          break
        }
      }
    } finally { $xr.Close() }
  } catch { $err = $_.Exception.Message }
  [pscustomobject]@{ Path = $path; Dmd = $dmd; Error = $err }
} -ThrottleLimit 6
$failed = @($parsed | Where-Object { $_.Error })
$missing = @($parsed | Where-Object { -not $_.Dmd })
"files parsed: $($parsed.Count), parse errors: $($failed.Count), with a DMD element: $($parsed.Count - $missing.Count)"
if ($failed.Count)  { "PARSE ERROR: " + (($failed | ForEach-Object { "$($_.Path): $($_.Error)" }) -join '; ') }
if ($missing.Count) { "NO DMD ELEMENT: " + (($missing | ForEach-Object { $_.Path }) -join ', ') }
$off = @()
foreach ($p in $parsed) {
  if (-not $p.Dmd) { continue }
  $w = $want[$p.Path].chosen
  $d = @(
    [Math]::Abs([double]$p.Dmd.X - [double]$w.x)
    [Math]::Abs([double]$p.Dmd.Y - [double]$w.y)
    [Math]::Abs([double]$p.Dmd.W - [double]$w.w)
    [Math]::Abs([double]$p.Dmd.H - [double]$w.h)
  )
  if (($d | Measure-Object -Maximum).Maximum -gt 0.5) {
    $off += "$($p.Path): xml $($p.Dmd.X),$($p.Dmd.Y) $($p.Dmd.W)x$($p.Dmd.H) vs plan $($w.x),$($w.y) $($w.w)x$($w.h)"
  }
}
"stamped numbers differ from the plan: $($off.Count)"
if ($off.Count) { $off | Select-Object -First 10 }
