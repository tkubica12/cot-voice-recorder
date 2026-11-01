#Requires -Version 7.0
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-NearestRankP95 {
  param(
    [Parameter(Mandatory)]
    [ValidateCount(1, 2147483647)]
    [double[]]$Values
  )
  foreach ($value in $Values) {
    if (-not [double]::IsFinite($value)) { throw 'Percentile input must be finite.' }
  }
  $sorted = @($Values | Sort-Object)
  return $sorted[[int][Math]::Ceiling(0.95 * $sorted.Count) - 1]
}

function Write-MeasurementReport {
  param(
    [Parameter(Mandatory)]
    [ValidateNotNull()]
    [object]$Report,
    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string]$OutputPath
  )
  $output = [System.IO.Path]::GetFullPath($OutputPath)
  if (Test-Path -LiteralPath $output) { throw "Output file already exists: $output" }
  $temporary = $output + '.' + [guid]::NewGuid().ToString('N') + '.tmp'
  $owned = $false
  try {
    $stream = [System.IO.File]::Open($temporary, [System.IO.FileMode]::CreateNew,
      [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    $owned = $true
    try {
      $json = $Report | ConvertTo-Json -Depth 12
      $bytes = [System.Text.UTF8Encoding]::new($false).GetBytes($json + "`n")
      $stream.Write($bytes, 0, $bytes.Length)
      $stream.Flush($true)
    } finally {
      $stream.Dispose()
    }
    [System.IO.File]::Move($temporary, $output)
  } finally {
    if ($owned -and (Test-Path -LiteralPath $temporary)) {
      Remove-Item -LiteralPath $temporary
    }
  }
  return $output
}

Export-ModuleMember -Function Get-NearestRankP95, Write-MeasurementReport
