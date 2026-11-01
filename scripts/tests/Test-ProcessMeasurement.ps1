#Requires -Version 7.0
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Assert-Condition {
  param([bool]$Condition, [string]$Message)
  if (-not $Condition) { throw $Message }
}

$sampler = Join-Path $PSScriptRoot '..\Measure-VoicePromptProcess.ps1'
$prefix = Join-Path ([System.IO.Path]::GetTempPath()) ('VoicePrompt-process-test-' + [guid]::NewGuid().ToString('N'))
$output = $prefix + '.json'
$invalidOutput = $prefix + '-invalid.json'
$child = [System.Diagnostics.Process]::new()
$childStarted = $false
$child.StartInfo.FileName = (Get-Process -Id $PID).MainModule.FileName
$child.StartInfo.UseShellExecute = $false
$child.StartInfo.RedirectStandardOutput = $true
foreach ($argument in @('-NoProfile', '-NonInteractive', '-Command',
  "[Console]::WriteLine('ready'); Start-Sleep -Seconds 60")) {
  $child.StartInfo.ArgumentList.Add($argument)
}

try {
  $childStarted = $child.Start()
  Assert-Condition $childStarted 'Could not start the synthetic test process.'
  $ready = $child.StandardOutput.ReadLineAsync()
  Assert-Condition ($ready.Wait(10000) -and $ready.Result -eq 'ready') 'Test process did not become ready.'
  & $sampler -ProcessId $child.Id -Scenario 'synthetic-test' -SampleCount 3 `
    -IntervalMilliseconds 100 -OutputPath $output | Out-Null
  $report = Get-Content -LiteralPath $output -Raw | ConvertFrom-Json
  Assert-Condition ($report.status -eq 'complete') 'The report is not complete.'
  Assert-Condition ($report.samples.Count -eq 3) 'Expected three raw samples.'
  Assert-Condition ($report.process.id -eq $child.Id) 'The wrong process was measured.'
  Assert-Condition ($report.measurement.duration_seconds -ge 0.19) 'Actual elapsed time was not measured.'
  Assert-Condition ($report.samples[0].cpu_one_core_percent -eq $null) 'The initial CPU delta must be absent.'
  Assert-Condition ($report.summary.private_bytes_max -ge $report.summary.private_bytes_mean) 'Invalid memory summary.'
  $sorted = @($report.samples.private_bytes | Sort-Object)
  Assert-Condition ($report.summary.private_bytes_p95 -eq $sorted[2]) 'Incorrect nearest-rank percentile.'
  Assert-Condition ($report.process.binary_sha256.Length -eq 64) 'Binary identity was not captured.'
  Assert-Condition (-not $report.measurement.measures_process_startup) 'This sampler must not claim startup timing.'
  Assert-Condition (-not $child.HasExited) 'The sampler changed or stopped the measured process.'

  $original = [System.IO.File]::ReadAllText($output)
  $failure = $null
  try {
    & $sampler -ProcessId $child.Id -Scenario 'overwrite-test' -SampleCount 2 `
      -IntervalMilliseconds 100 -OutputPath $output | Out-Null
  } catch { $failure = $_ }
  Assert-Condition ($null -ne $failure) 'An existing report was overwritten.'
  Assert-Condition ($failure.Exception.Message -like 'Output file already exists:*') 'Unexpected overwrite error.'
  Assert-Condition ([System.IO.File]::ReadAllText($output) -eq $original) 'The original report changed.'

  $failure = $null
  try {
    & $sampler -ProcessId ([int]::MaxValue) -Scenario 'missing-process' -SampleCount 2 `
      -IntervalMilliseconds 100 -OutputPath $invalidOutput | Out-Null
  } catch { $failure = $_ }
  Assert-Condition ($null -ne $failure) 'A missing process was reported as successful.'
  Assert-Condition (-not (Test-Path -LiteralPath $invalidOutput)) 'A failed measurement published a complete report.'

  $failure = $null
  try {
    & $sampler -ProcessId $child.Id -Scenario 'invalid-sample-count' -SampleCount 1 `
      -IntervalMilliseconds 100 -OutputPath $invalidOutput | Out-Null
  } catch { $failure = $_ }
  Assert-Condition ($null -ne $failure) 'An invalid sample count was accepted.'
  Assert-Condition (-not (Test-Path -LiteralPath $invalidOutput)) 'Invalid parameters published a report.'

  $child.Kill($true)
  $child.WaitForExit()
  $child.StartInfo.ArgumentList[$child.StartInfo.ArgumentList.Count - 1] =
    "[Console]::WriteLine('ready'); Start-Sleep -Seconds 1"
  $childStarted = $child.Start()
  Assert-Condition $childStarted 'Could not start the short-lived test process.'
  $ready = $child.StandardOutput.ReadLineAsync()
  Assert-Condition ($ready.Wait(10000) -and $ready.Result -eq 'ready') 'Short-lived process did not become ready.'
  $failure = $null
  try {
    & $sampler -ProcessId $child.Id -Scenario 'exit-during-measurement' -SampleCount 21 `
      -IntervalMilliseconds 100 -OutputPath $invalidOutput | Out-Null
  } catch { $failure = $_ }
  Assert-Condition ($null -ne $failure) 'An exited process was reported as successful.'
  Assert-Condition (-not (Test-Path -LiteralPath $invalidOutput)) 'An incomplete measurement published a report.'
  Write-Output 'PASS identity, raw counters, CPU delta, percentiles, no overwrite, invalid parameters, and process exit.'
} finally {
  if ($childStarted -and -not $child.HasExited) {
    $child.Kill($true)
    $child.WaitForExit()
  }
  $child.Dispose()
  foreach ($path in @($output, $invalidOutput)) {
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path }
  }
}
