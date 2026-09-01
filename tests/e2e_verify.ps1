# End-to-end verification for streamtop v1.3.x (hermetic, no paid services).
# Native PowerShell harness mirroring tests/e2e_verify.sh (no WSL/Git Bash).
$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

$Pass = 0
$Fail = 0
$Tmp = Join-Path $env:TEMP "streamtop-e2e-$PID"
New-Item -ItemType Directory -Force -Path $Tmp | Out-Null

$MockProc = $null
$PromProc = $null
$AgentProc = $null
$MockLog = $null

function Log([string]$Msg) { Write-Host "[e2e] $Msg" }
function Pass([string]$Msg) { $script:Pass++; Log "PASS: $Msg" }
function Fail([string]$Msg) { $script:Fail++; Write-Host "[e2e] FAIL: $Msg" -ForegroundColor Red }

function Need-Cmd([string]$Name) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        Fail "missing required command: $Name"
        exit 1
    }
}

function Get-JsonField([string]$Path, [string]$Expr) {
    $doc = [System.IO.File]::ReadAllText($Path) | ConvertFrom-Json
    $cur = $doc
    foreach ($part in $Expr.Trim('.').Split('.')) {
        if ($part) { $cur = $cur.$part }
    }
    if ($null -eq $cur) { return $null }
    if ($cur -is [bool]) { return $cur.ToString().ToLower() }
    return $cur.ToString()
}

function Run-Summary([string]$Url, [string[]]$ExtraArgs) {
    $Out = Join-Path $Tmp 'summary.json'
    $Err = Join-Path $Tmp 'stderr.txt'
    $args = @($Url) + $ExtraArgs + @('--summary', '--summary-format', 'json', '--timeout', '8')
    $stdout = & $Streamtop @args 2> $Err
    $exit = $LASTEXITCODE
    if ($stdout) {
        $jsonLine = $stdout | Where-Object { $_ -match '^\s*\{' } | Select-Object -First 1
        if ($jsonLine) {
            [System.IO.File]::WriteAllText($Out, $jsonLine.Trim(), [System.Text.UTF8Encoding]::new($false))
        }
    }
    if (-not (Test-Path $Out) -or (Get-Item $Out).Length -eq 0) {
        Fail "no summary JSON for $Url ($($ExtraArgs -join ' '))"
        if (Test-Path $Err) { Get-Content $Err | Write-Host }
        return $null
    }
    & python $Root/tests/e2e/validate_summary.py $Out | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Fail "schema validation for $Url"
        return $null
    }
    return $Out
}

function Wait-ForMock([string]$HealthUrl, [int]$TimeoutSec = 60) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        if ($MockProc.HasExited) {
            Fail "mock server exited early (code $($MockProc.ExitCode))"
            if ($MockLog -and (Test-Path $MockLog)) { Get-Content $MockLog | Write-Host }
            return $false
        }
        try {
            $r = Invoke-WebRequest -Uri $HealthUrl -UseBasicParsing -TimeoutSec 2
            if ($r.StatusCode -eq 200) { return $true }
        } catch {}
        Start-Sleep -Milliseconds 500
    }
    Fail "mock server not ready after ${TimeoutSec}s"
    if ($MockLog -and (Test-Path $MockLog)) { Get-Content $MockLog | Write-Host }
    return $false
}

function Wait-ForMetrics([string]$MetricsUrl, [int]$TimeoutSec = 30) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        if ($PromProc.HasExited) { return $false }
        try {
            Invoke-WebRequest -Uri $MetricsUrl -UseBasicParsing -TimeoutSec 2 | Out-Null
            return $true
        } catch {
            if ($_.Exception.Response -and [int]$_.Exception.Response.StatusCode -eq 401) {
                return $true
            }
        }
        Start-Sleep -Milliseconds 500
    }
    return $false
}

try {
    Need-Cmd python
    if (-not (Get-Command python3 -ErrorAction SilentlyContinue)) {
        Set-Alias -Name python3 -Value python -Scope Script -ErrorAction SilentlyContinue
    }

    Log 'Building streamtop release binary'
    cargo build --release --quiet
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

    $Streamtop = Join-Path $Root 'target/release/streamtop.exe'
    if (-not (Test-Path $Streamtop)) {
        $Streamtop = Join-Path $Root 'target/release/streamtop'
    }
    if (-not (Test-Path $Streamtop)) {
        Fail "binary missing at $Streamtop"
        exit 1
    }

    Log 'Starting hermetic mock servers (HTTP/SRT/RTMP)'
    $MockLog = Join-Path $Tmp 'mock.log'
    $MockProc = Start-Process -FilePath python `
        -ArgumentList @("$Root/tests/e2e/mock_all.py") `
        -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput $MockLog

    $Base = 'http://127.0.0.1:8765'
    if (-not (Wait-ForMock "$Base/health")) { exit 1 }

    $Tr101Url = "$Base/tr101290/live.m3u8"
    $SeiUrl = "$Base/sei/live.m3u8"
    $HlsUrl = "$Base/live.m3u8"
    $LlUrl = "$Base/ll-hls/master.m3u8"
    $DashUrl = "$Base/dash/live.mpd"
    $SrtUrl = 'srt://127.0.0.1:9000'
    $RtmpUrl = 'rtmp://127.0.0.1:1935/live/stream'

    Log 'TR 101 290 compliance summary'
    $Out = Run-Summary $Tr101Url @('--tr101290', '--probe-headers')
    if ($Out) {
        $P1 = Get-JsonField $Out 'tr101290.p1_violations'
        $P2 = Get-JsonField $Out 'tr101290.p2_violations'
        if (($P1 -match '^\d+$') -and ($P2 -match '^\d+$') -and ([int]$P1 -gt 0 -or [int]$P2 -gt 0)) {
            Pass "tr101290 violations reported (P1=$P1 P2=$P2)"
        } else {
            Fail "expected tr101290.p1_violations or p2_violations > 0 (P1=$P1 P2=$P2)"
        }
    }

    Log 'SEI probe summary'
    $Out = Run-Summary $SeiUrl @('--probe-sei', '--probe-headers')
    if ($Out) {
        $C608 = Get-JsonField $Out 'sei_metadata.cea608_present'
        $Hdr = Get-JsonField $Out 'sei_metadata.hdr10_present'
        $c608Ok = ($C608 -eq 'True' -or $C608 -eq 'true')
        $hdrOk = ($Hdr -eq 'True' -or $Hdr -eq 'true')
        if ($c608Ok -and $hdrOk) {
            Pass 'sei_metadata captions and HDR detected'
        } else {
            Fail "expected sei_metadata.cea608_present and hdr10_present (c608=$C608 hdr=$Hdr)"
        }
    }

    Log 'Synthetic QoE summary'
    $Out = Run-Summary $HlsUrl @('--simulate-player', '--throttle-kbps', '1500', '--simulated-rtt-ms', '120', '--probe-headers')
    if ($Out) {
        $Risk = Get-JsonField $Out 'synthetic_qoe.rebuffer_risk_score'
        if (($Risk -match '^\d+$') -and [int]$Risk -ge 0 -and [int]$Risk -le 100) {
            Pass "synthetic_qoe.rebuffer_risk_score=$Risk"
        } else {
            Fail "rebuffer_risk_score out of range: $Risk"
        }
    }

    Log 'Legacy SRT URL rejection'
    $ErrFile = Join-Path $Tmp 'srt_err.txt'
    & $Streamtop $SrtUrl '--summary' 2>$ErrFile | Out-Null
    if ($LASTEXITCODE -eq 0) {
        Fail 'expected srt:// to fail'
    } elseif (Select-String -Path $ErrFile -Pattern 'not supported' -Quiet) {
        Pass 'srt:// rejected with clear error'
    } else {
        Fail 'srt:// error message missing'
        Get-Content $ErrFile | Write-Host
    }

    Log 'Legacy RTMP URL rejection'
    $ErrFile = Join-Path $Tmp 'rtmp_err.txt'
    & $Streamtop $RtmpUrl '--summary' 2>$ErrFile | Out-Null
    if ($LASTEXITCODE -eq 0) {
        Fail 'expected rtmp:// to fail'
    } elseif (Select-String -Path $ErrFile -Pattern 'not supported' -Quiet) {
        Pass 'rtmp:// rejected with clear error'
    } else {
        Fail 'rtmp:// error message missing'
        Get-Content $ErrFile | Write-Host
    }

    Log 'LL-HLS fMP4 smoke'
    $Out = Run-Summary $LlUrl @('--probe-headers')
    if ($Out) {
        $Seg = Get-JsonField $Out 'saw_segment'
        if ($Seg -eq 'True' -or $Seg -eq 'true') {
            Pass 'LL-HLS saw_segment'
        } else {
            Fail 'LL-HLS did not fetch a segment'
        }
    }

    Log 'DASH live MPD smoke'
    $Out = Run-Summary $DashUrl @('--probe-headers', '--probe-drm')
    if ($Out) {
        Pass 'DASH summary schema valid'
        $Sv = Get-JsonField $Out 'schema_version'
        if ($Sv -eq '5') { Pass 'summary schema v5' } else { Fail "expected schema_version 5, got $Sv" }
    }

    Log 'ClearKey cbcs staging smoke'
    $Out = Run-Summary $DashUrl @(
        '--probe-headers', '--probe-drm',
        '--clearkey', '0123456789abcdef0123456789abcdef:fedcba9876543210fedcba9876543210'
    )
    if ($Out) { Pass 'ClearKey staging summary schema valid' }

    Log 'HTML export-report'
    $Report = Join-Path $Tmp 'test_report.html'
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & $Streamtop $HlsUrl '--export-report' $Report '--timeout' '5' 1>$null 2>$null
    $ErrorActionPreference = $prevEap
    if ((Test-Path $Report) -and (Get-Content $Report -TotalCount 1) -match '<!DOCTYPE html>') {
        Pass 'export-report HTML structure'
    } else {
        Fail 'export-report missing or invalid HTML'
    }
    $Side = Join-Path $Tmp 'test_report.incident.json'
    if ((Test-Path $Side) -and (Get-Item $Side).Length -gt 0) {
        Pass 'export-report incident sidecar'
    } else {
        Fail 'export-report incident sidecar missing'
    }

    Log 'Agent fleet metrics'
    $AgentPort = 19184
    $AgentCfg = Join-Path $Tmp 'agent.toml'
    @"
metrics_bind = "127.0.0.1"
metrics_port = $AgentPort

[[streams]]
id = "hls"
url = "$HlsUrl"
interval_ms = 500

[[streams]]
id = "dash"
url = "$DashUrl"
interval_ms = 500
"@ | Set-Content -Path $AgentCfg -Encoding utf8
    $AgentProc = Start-Process -FilePath $Streamtop -ArgumentList @('--agent', $AgentCfg) -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 5
    try {
        $AgentMetrics = (Invoke-WebRequest -Uri "http://127.0.0.1:$AgentPort/metrics" -UseBasicParsing -TimeoutSec 3).Content
        if ($AgentMetrics -match 'streamtop_agent_streams_active') {
            Pass 'agent aggregated metrics endpoint'
        } else {
            Fail 'agent metrics missing streamtop_agent_streams_active'
        }
        if ($AgentMetrics -match 'stream_id="hls"') {
            Pass 'agent stream_id label hls'
        } else {
            Fail 'agent missing stream_id label'
        }
    } catch {
        Fail "agent metrics probe failed: $_"
    }

    Log 'Webhook SSRF protection'
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & $Streamtop $HlsUrl --webhook 'http://169.254.169.254/latest/meta-data' --timeout 2 1>$null 2>$null
    $Rc = $LASTEXITCODE
    $ErrorActionPreference = $prevEap
    if ($Rc -ne 0) {
        Pass "metadata webhook blocked (exit $Rc)"
    } else {
        Fail 'metadata webhook should be blocked'
    }

    Log 'Invalid alert list rejection'
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & $Streamtop $HlsUrl --webhook "$Base/webhook" --allow-insecure-webhooks --alert-on typo --timeout 1 1>$null 2>$null
    $Rc = $LASTEXITCODE
    $ErrorActionPreference = $prevEap
    if ($Rc -ne 0) { Pass 'invalid --alert-on rejected' } else { Fail 'invalid --alert-on accepted' }

    Log 'VOD crawl and incident exports'
    & $Streamtop --vod $HlsUrl --summary --summary-format json 1>$null 2>$null
    if ($LASTEXITCODE -eq 0) { Pass 'VOD crawl command' } else { Fail 'VOD crawl command' }
    $Har = Join-Path $Tmp 'incident.har'
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & $Streamtop $HlsUrl --export-har $Har --timeout 2 1>$null 2>$null
    $ErrorActionPreference = $prevEap
    if ((Test-Path $Har) -and (Get-Item $Har).Length -gt 0) { Pass 'HAR export' } else { Fail 'HAR export' }

    Log 'Prometheus metrics auth'
    $MetricsPort = Get-Random -Minimum 20000 -Maximum 45000
    $MetricsUrl = "http://127.0.0.1:$MetricsPort/metrics"
    $PromProc = Start-Process -FilePath $Streamtop `
        -ArgumentList @(
            $HlsUrl, '--simulate-player', '--tr101290',
            '--prometheus', $MetricsPort, '--metrics-token', 'test-token',
            '--probe-headers'
        ) `
        -PassThru -WindowStyle Hidden
    if (-not (Wait-ForMetrics $MetricsUrl)) {
        Fail 'metrics endpoint not ready'
        throw 'metrics endpoint not ready'
    }

    try {
        try {
            Invoke-WebRequest -Uri $MetricsUrl -UseBasicParsing | Out-Null
            Fail 'expected metrics 401 without token, got 200'
        } catch {
            $code = [int]$_.Exception.Response.StatusCode
            if ($code -eq 401) {
                Pass 'metrics 401 without token'
            } else {
                Fail "expected metrics 401 without token, got $code"
            }
        }

        $Headers = @{ Authorization = 'Bearer test-token' }
        $Auth = Invoke-WebRequest -Uri $MetricsUrl -Headers $Headers -UseBasicParsing
        if ($Auth.StatusCode -eq 200) {
            Pass 'metrics 200 with bearer token'
        } else {
            Fail "expected metrics 200 with token, got $($Auth.StatusCode)"
        }

        $Metrics = $Auth.Content
        if ($Metrics -match 'streamtop_qoe_rebuffer_risk') {
            Pass 'metric streamtop_qoe_rebuffer_risk present'
        } else {
            Fail 'missing streamtop_qoe_rebuffer_risk'
        }
        if ($Metrics -match 'streamtop_tr101290_p1_violations_total') {
            Pass 'metric streamtop_tr101290_p1_violations_total present'
        } else {
            Fail 'missing tr101290 p1 metric'
        }
        if ($Metrics -match 'streamtop_inband_emsg_total') {
            Pass 'metric streamtop_inband_emsg_total present'
        } else {
            Fail 'missing inband emsg metric'
        }
        if ($Metrics -match 'streamtop_ad_mismatch_total') {
            Pass 'metric streamtop_ad_mismatch_total present'
        } else {
            Fail 'missing ad mismatch metric'
        }
        if ($Metrics -match 'streamtop_clearkey_decrypt_ok') {
            Pass 'metric streamtop_clearkey_decrypt_ok present'
        } else {
            Fail 'missing clearkey metric'
        }
    } catch {
        Fail "Prometheus probe failed: $_"
    }
}
finally {
    if ($AgentProc -and -not $AgentProc.HasExited) {
        Stop-Process -Id $AgentProc.Id -Force -ErrorAction SilentlyContinue
    }
    if ($PromProc -and -not $PromProc.HasExited) {
        Stop-Process -Id $PromProc.Id -Force -ErrorAction SilentlyContinue
    }
    if ($MockProc -and -not $MockProc.HasExited) {
        Stop-Process -Id $MockProc.Id -Force -ErrorAction SilentlyContinue
    }
    Remove-Item -Recurse -Force -Path $Tmp -ErrorAction SilentlyContinue
}

Log "Results: $Pass passed, $Fail failed"
if ($Fail -gt 0) { exit 1 }
exit 0
