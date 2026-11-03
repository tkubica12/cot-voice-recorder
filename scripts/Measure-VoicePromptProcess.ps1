#Requires -Version 7.0
[CmdletBinding()]
param(
  [Parameter(Mandatory)]
  [ValidateRange(1, 2147483647)]
  [int]$ProcessId,

  [Parameter(Mandatory)]
  [ValidateScript({ -not [string]::IsNullOrWhiteSpace($_) })]
  [string]$Scenario,

  [Parameter(Mandatory)]
  [ValidateNotNullOrEmpty()]
  [string]$OutputPath,

  [ValidateRange(2, 3601)]
  [int]$SampleCount = 301,

  [ValidateRange(100, 60000)]
  [int]$IntervalMilliseconds = 1000,

  [string]$SourceRevision,

  [string]$SampleJournalPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Import-Module (Join-Path $PSScriptRoot 'MeasurementReport.psm1')

$output = [System.IO.Path]::GetFullPath($OutputPath)
if (Test-Path -LiteralPath $output) {
  throw "Output file already exists: $output"
}
if (-not (Test-Path -LiteralPath ([System.IO.Path]::GetDirectoryName($output)) -PathType Container)) {
  throw 'The output directory must already exist.'
}

$process = Get-Process -Id $ProcessId
$journal = $null
try {
  $processStarted = $process.StartTime.ToUniversalTime()
  $module = $process.MainModule
  $binaryHash = (Get-FileHash -LiteralPath $module.FileName -Algorithm SHA256).Hash
  $productVersion = $module.FileVersionInfo.ProductVersion
  $startedUtc = [DateTimeOffset]::UtcNow
  $clock = [System.Diagnostics.Stopwatch]::StartNew()
  $samples = [System.Collections.Generic.List[object]]::new()
  $logicalProcessors = [Environment]::ProcessorCount
  if (-not [string]::IsNullOrEmpty($SampleJournalPath)) {
    if ([string]::IsNullOrWhiteSpace($SampleJournalPath)) { throw 'A sample journal path cannot be blank.' }
    $journalPath = [System.IO.Path]::GetFullPath($SampleJournalPath)
    if (-not (Test-Path -LiteralPath ([System.IO.Path]::GetDirectoryName($journalPath)) -PathType Container)) {
      throw 'The sample journal directory must already exist.'
    }
    $stream = [System.IO.FileStream]::new($journalPath, [System.IO.FileMode]::CreateNew,
      [System.IO.FileAccess]::Write, [System.IO.FileShare]::Read)
    $journal = [System.IO.StreamWriter]::new($stream, [System.Text.UTF8Encoding]::new($false))
    $journal.WriteLine((@{schema_version=1;status='sampling';process_id=$ProcessId;
      binary_sha256=$binaryHash;source_revision=$SourceRevision;scenario=$Scenario;
      requested_samples=$SampleCount;interval_milliseconds=$IntervalMilliseconds} | ConvertTo-Json -Compress))
    $journal.Flush()
    $journal.BaseStream.Flush($true)
  }

  for ($index = 0; $index -lt $SampleCount; $index++) {
    if ($index -gt 0) {
      $remaining = $index * $IntervalMilliseconds - $clock.Elapsed.TotalMilliseconds
      if ($remaining -gt 0) {
        Start-Sleep -Milliseconds ([int][Math]::Ceiling($remaining))
      }
    }
    $process.Refresh()
    if ($process.HasExited -or $process.StartTime.ToUniversalTime() -ne $processStarted) {
      throw 'The measured process exited or changed identity; no complete report was written.'
    }
    $cpuSeconds = $process.TotalProcessorTime.TotalSeconds
    $elapsed = $clock.Elapsed.TotalSeconds
    $oneCorePercent = $null
    if ($index -gt 0) {
      $previous = $samples[$index - 1]
      $cpuDelta = $cpuSeconds - $previous.cpu_seconds
      $elapsedDelta = $elapsed - $previous.elapsed_seconds
      if ($cpuDelta -lt 0 -or $elapsedDelta -le 0) {
        throw 'The process counters or measurement clock moved backwards.'
      }
      $oneCorePercent = 100 * $cpuDelta / $elapsedDelta
    }
    $samples.Add([ordered]@{
      elapsed_seconds = $elapsed
      working_set_bytes = $process.WorkingSet64
      private_bytes = $process.PrivateMemorySize64
      cpu_seconds = $cpuSeconds
      cpu_one_core_percent = $oneCorePercent
      thread_count = $process.Threads.Count
      handle_count = $process.HandleCount
    })
    if ($null -ne $journal) {
      $journal.WriteLine((@{index=$index;sample=$samples[$index]} | ConvertTo-Json -Compress))
      $journal.Flush()
      $journal.BaseStream.Flush($true)
    }
  }

  $first = $samples[0]
  $last = $samples[$samples.Count - 1]
  $duration = $last.elapsed_seconds - $first.elapsed_seconds
  $cpuMean = 100 * ($last.cpu_seconds - $first.cpu_seconds) / $duration
  $private = $samples | ForEach-Object { $_.private_bytes }
  $workingSet = $samples | ForEach-Object { $_.working_set_bytes }
  $intervalCpu = $samples | Select-Object -Skip 1 | ForEach-Object { $_.cpu_one_core_percent }
  $report = [ordered]@{
    schema_version = 1
    status = 'complete'
    scenario = $Scenario
    source_revision = $SourceRevision
    started_utc = $startedUtc.ToString('O')
    finished_utc = [DateTimeOffset]::UtcNow.ToString('O')
    process = [ordered]@{
      id = $ProcessId
      name = $process.ProcessName
      started_utc = $processStarted.ToString('O')
      product_version = $productVersion
      binary_sha256 = $binaryHash
    }
    environment = [ordered]@{
      os = $PSVersionTable.OS
      logical_processors = $logicalProcessors
      sampler_architecture = [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()
      powershell_version = $PSVersionTable.PSVersion.ToString()
    }
    measurement = [ordered]@{
      sample_count = $samples.Count
      requested_interval_milliseconds = $IntervalMilliseconds
      duration_seconds = $duration
      measures_process_startup = $false
      measures_child_processes = $false
      percentile_method = 'nearest-rank'
      cpu_one_core_definition = '100 percent is one logical processor fully occupied.'
      private_bytes_definition = 'Committed private memory, not resident physical memory.'
    }
    summary = [ordered]@{
      private_bytes_mean = ($private | Measure-Object -Average).Average
      private_bytes_p95 = Get-NearestRankP95 $private
      private_bytes_max = ($private | Measure-Object -Maximum).Maximum
      working_set_bytes_mean = ($workingSet | Measure-Object -Average).Average
      working_set_bytes_p95 = Get-NearestRankP95 $workingSet
      working_set_bytes_max = ($workingSet | Measure-Object -Maximum).Maximum
      cpu_one_core_percent_mean = $cpuMean
      cpu_one_core_percent_p95 = Get-NearestRankP95 $intervalCpu
      cpu_machine_percent_mean = $cpuMean / $logicalProcessors
    }
    samples = @($samples.ToArray())
  }
  Write-MeasurementReport -Report $report -OutputPath $output
} finally {
  if ($null -ne $journal) { $journal.Dispose() }
  $process.Dispose()
}
