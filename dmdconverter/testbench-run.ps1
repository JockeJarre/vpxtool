$ErrorActionPreference = 'Continue'
$root = 'D:\Projects\Testbench'
$work = 'D:\git\Flipper\DMDConverter'
$bin = "$work\target\release\dmdconverter.exe"
$log = "$work\testbench"

# 1. the state of every backglass file before anything is touched
$pre = "$log-pre.csv"
Get-ChildItem $root -File |
  Where-Object { $_.Extension -match '^\.(directb2s|b2spro)$' } |
  ForEach-Object { [pscustomobject]@{
    Path = $_.FullName
    Length = $_.Length
    LastWriteTimeUtc = $_.LastWriteTimeUtc.ToString('o')
    Sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash
  } } |
  Export-Csv -NoTypeInformation -Encoding utf8 $pre
"pre-state: " + (Import-Csv $pre).Count + " files hashed"

# 2. dry run, the plan
& $bin scan $root --ini "$work\DmdDevice.ini" --method template --json "$log-plan.json" `
  > "$log-dry.log" 2>&1
"dry run exit $LASTEXITCODE"

# 3. the real write
& $bin scan $root --ini "$work\DmdDevice.ini" --method template --json "$log-write.json" --write `
  > "$log-write.log" 2>&1
"write exit $LASTEXITCODE"

Get-Content "$log-write.log" -Tail 14
