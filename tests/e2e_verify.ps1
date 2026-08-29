# End-to-end verification for streamtop v1.1.x (hermetic, no paid services).
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
    $MockProc = Start-Process -FilePath python `
        -ArgumentList @("$Root/tests/e2e/mock_all.py") `
        -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 2

    $Base = 'http://127.0.0.1:8765'
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
    $Out = Run-Summary $HlsUrl @('--simulate-player', '--throttle-kbps', '2000', '--simulated-rtt-ms', '120', '--probe-headers')
    if ($Out) {
        $Risk = Get-JsonField $Out 'synthetic_qoe.rebuffer_risk_score'
        if (($Risk -match '^\d+$') -and [int]$Risk -ge 0 -and [int]$Risk -le 100) {
            Pass "synthetic_qoe.rebuffer_risk_score=$Risk"
        } else {
            Fail "rebuffer_risk_score out of range: $Risk"
        }
    }

    Log 'SRT ingest summary'
    $Out = Run-Summary $SrtUrl @()
    if ($Out) {
        $Proto = Get-JsonField $Out 'ingest_stats.protocol'
        $Rtt = Get-JsonField $Out 'ingest_stats.rtt_ms'
        if ($Proto -eq 'srt' -and $Rtt -and $Rtt -ne 'null') {
            Pass "SRT ingest_stats protocol=$Proto rtt_ms=$Rtt"
        } else {
            Fail "SRT ingest_stats missing (protocol=$Proto rtt=$Rtt)"
        }
    }

    Log 'RTMP ingest summary'
    $Out = Run-Summary $RtmpUrl @()
    if ($Out) {
        $Proto = Get-JsonField $Out 'ingest_stats.protocol'
        $Conn = Get-JsonField $Out 'ingest_stats.connected'
        $connOk = ($Conn -eq 'True' -or $Conn -eq 'true')
        if ($Proto -eq 'rtmp' -and $connOk) {
            Pass 'RTMP ingest connected'
        } else {
            Fail "RTMP ingest_stats (protocol=$Proto connected=$Conn)"
        }
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
    if ($Out) { Pass 'DASH summary schema valid' }

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

    Log 'Prometheus metrics auth'
    $PromProc = Start-Process -FilePath $Streamtop `
        -ArgumentList @(
            $HlsUrl, '--simulate-player', '--tr101290',
            '--prometheus', '9184', '--metrics-token', 'test-token',
            '--probe-headers'
        ) `
        -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 4

    try {
        try {
            Invoke-WebRequest -Uri 'http://127.0.0.1:9184/metrics' -UseBasicParsing | Out-Null
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
        $Auth = Invoke-WebRequest -Uri 'http://127.0.0.1:9184/metrics' -Headers $Headers -UseBasicParsing
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
    } catch {
        Fail "Prometheus probe failed: $_"
    }
}
finally {
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
