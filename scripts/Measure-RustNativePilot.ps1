#Requires -Version 7.0
[CmdletBinding()]
param(
  [Parameter(Mandatory)]
  [ValidateNotNullOrEmpty()]
  [string]$Executable,
  [Parameter(Mandatory)]
  [ValidateNotNullOrEmpty()]
  [string]$OutputDirectory,
  [Parameter(Mandatory)]
  [ValidateNotNullOrEmpty()]
  [string]$SourceRevision,
  [ValidateRange(1, 100)]
  [int]$StartupTrials = 30,
  [ValidateRange(2, 3301)]
  [int]$MemorySamples = 301
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Native pilot measurements require Windows.' }
Import-Module (Join-Path $PSScriptRoot 'MeasurementReport.psm1')
$binary = (Resolve-Path -LiteralPath $Executable).Path
if ([System.IO.Path]::GetFileName($binary) -ne 'VoicePromptRustPilot.exe') {
  throw 'This collector only runs the explicitly named VoicePromptRustPilot.exe.'
}
$directory = (Resolve-Path -LiteralPath $OutputDirectory).Path
if (-not (Test-Path -LiteralPath $directory -PathType Container)) {
  throw 'The output directory must already exist.'
}
$startupOutput = Join-Path $directory 'native-foundation-startup.json'
$memoryOutput = Join-Path $directory 'native-foundation-memory.json'
foreach ($path in @($startupOutput, $memoryOutput)) {
  if (Test-Path -LiteralPath $path) { throw "Output file already exists: $path" }
}

function Start-Pilot {
  $key = 'measure-' + [guid]::NewGuid().ToString('N')
  $readyFile = Join-Path $directory ($key + '.ready.json')
  $process = [System.Diagnostics.Process]::new()
  $process.StartInfo.FileName = $binary
  $process.StartInfo.UseShellExecute = $false
  $process.StartInfo.RedirectStandardOutput = $true
  $process.StartInfo.RedirectStandardError = $true
  foreach ($argument in @('--instance-key', $key, '--ready-file', $readyFile,
    '--run-for-seconds', '3600')) {
    $process.StartInfo.ArgumentList.Add($argument)
  }
  $clock = [System.Diagnostics.Stopwatch]::StartNew()
  $started = $false
  try {
    $started = $process.Start()
    if (-not $started) { throw 'Could not start the native pilot.' }
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    while (-not (Test-Path -LiteralPath $readyFile)) {
      if ($process.HasExited) {
        $process.WaitForExit()
        throw "Native startup exited with code $($process.ExitCode): $($stderr.Result)"
      }
      if ($clock.Elapsed.TotalSeconds -ge 10) {
        throw 'Native startup did not publish readiness within ten seconds.'
      }
      Start-Sleep -Milliseconds 10
    }
    $marker = Get-Content -LiteralPath $readyFile -Raw | ConvertFrom-Json
    if ($marker.status -ne 'ready' -or $marker.pid -ne $process.Id -or
      $marker.scope -ne 'native-tray-dpapi-only') {
      throw 'Native readiness does not match this process and the expected limited scope.'
    }
    return [pscustomobject]@{
      Process = $process
      Key = $key
      ReadyFile = $readyFile
      StartupMilliseconds = $clock.Elapsed.TotalMilliseconds
      Stdout = $stdout
      Stderr = $stderr
    }
  } catch {
    if ($started -and -not $process.HasExited) {
      $process.Kill($true)
      $process.WaitForExit()
    }
    $process.Dispose()
    if (Test-Path -LiteralPath $readyFile) { Remove-Item -LiteralPath $readyFile }
    throw
  }
}

function Stop-Pilot {
  param([Parameter(Mandatory)][object]$Pilot)
  $process = $Pilot.Process
  $quit = [System.Diagnostics.Process]::new()
  $quitStarted = $false
  try {
    if (-not $process.HasExited) {
      $quit.StartInfo.FileName = $binary
      $quit.StartInfo.UseShellExecute = $false
      $quit.StartInfo.RedirectStandardOutput = $true
      $quit.StartInfo.RedirectStandardError = $true
      foreach ($argument in @('--quit', '--instance-key', $Pilot.Key)) {
        $quit.StartInfo.ArgumentList.Add($argument)
      }
      $quitStarted = $quit.Start()
      if (-not $quitStarted) { throw 'Could not start the isolated quit command.' }
      $quitOutput = $quit.StandardOutput.ReadToEndAsync()
      $quitError = $quit.StandardError.ReadToEndAsync()
      if (-not $quit.WaitForExit(10000)) { throw 'The isolated quit command timed out.' }
      if ($quit.ExitCode -ne 0) {
        throw "Isolated quit failed: $($quitError.Result)"
      }
      if (-not $process.WaitForExit(10000)) { throw 'The native pilot did not exit gracefully.' }
    }
    $process.WaitForExit()
    if ($process.ExitCode -ne 0 -or $Pilot.Stderr.Result.Length -gt 0) {
      throw "Native pilot failed or reported cleanup errors: $($Pilot.Stderr.Result)"
    }
  } finally {
    if ($quitStarted -and -not $quit.HasExited) {
      $quit.Kill($true)
      $quit.WaitForExit()
    }
    $quit.Dispose()
    if (-not $process.HasExited) {
      $process.Kill($true)
      $process.WaitForExit()
    }
    $process.Dispose()
    if (Test-Path -LiteralPath $Pilot.ReadyFile) { Remove-Item -LiteralPath $Pilot.ReadyFile }
  }
}

$trials = [System.Collections.Generic.List[object]]::new()
for ($index = 1; $index -le $StartupTrials; $index++) {
  $pilot = Start-Pilot
  $trial = [ordered]@{
    trial = $index
    pid = $pilot.Process.Id
    process_start_to_tray_ready_milliseconds = $pilot.StartupMilliseconds
  }
  Stop-Pilot $pilot
  $trials.Add($trial)
  Write-Output "Native startup trial $index/$StartupTrials completed."
}
$durations = $trials | ForEach-Object { $_.process_start_to_tray_ready_milliseconds }
$report = [ordered]@{
  schema_version = 1
  status = 'complete'
  scope = 'native-tray-dpapi-only'
  source_revision = $SourceRevision
  binary_sha256 = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
  startup_trial_count = $trials.Count
  percentile_method = 'nearest-rank'
  includes_authentication_or_dictation = $false
  measures_os_cold_cache = $false
  comparison_with_complete_client_is_valid = $false
  summary = [ordered]@{
    process_start_to_tray_ready_mean_milliseconds = ($durations | Measure-Object -Average).Average
    process_start_to_tray_ready_p95_milliseconds = Get-NearestRankP95 $durations
    process_start_to_tray_ready_max_milliseconds = ($durations | Measure-Object -Maximum).Maximum
  }
  trials = @($trials.ToArray())
}
Write-MeasurementReport -Report $report -OutputPath $startupOutput
$pilot = Start-Pilot
try {
  Write-Output "Monitoring native foundation PID $($pilot.Process.Id); $MemorySamples memory samples."
  & (Join-Path $PSScriptRoot 'Measure-VoicePromptProcess.ps1') `
    -ProcessId $pilot.Process.Id -Scenario 'native-tray-dpapi-only-no-auth-no-dictation' `
    -SampleCount $MemorySamples -IntervalMilliseconds 1000 `
    -SourceRevision $SourceRevision -OutputPath $memoryOutput
} finally {
  Stop-Pilot $pilot
}
