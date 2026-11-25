# Teste do perfil Series S do LumeGX.
# Uso:  .\scripts\teste-series-s.ps1 -Game 'D:\caminho\do\seu\dump\game_scarlett_master.exe'
# Opcional: -FidelityFx 'D:\pasta\com\amd_fidelityfx_upscaler_dx12.dll'   -NoBuild
param(
    [Parameter(Mandatory)] [string]$Game,
    [string]$FidelityFx = '',
    [switch]$NoBuild
)
$ErrorActionPreference = 'Stop'
$Repo = Split-Path -Parent $PSScriptRoot
if (-not (Test-Path $Game)) { throw "Executavel do jogo nao encontrado: $Game" }

if (-not $NoBuild) {
    Push-Location $Repo
    cargo build --profile diag -p victory --features diagnostics
    if ($LASTEXITCODE) { Pop-Location; throw 'build falhou' }
    Pop-Location
}
$Exe = Join-Path $Repo 'target\diag\victory.exe'

# Correcoes do Series S (todas opt-in):
$env:VICTORY_RGB9E5_AS_R11G11B10 = '1'            # alvo HDR R9G9B9E5 que o PC recusa -> R11G11B10
$env:VICTORY_PRESENT_PLANES      = '1'            # compoe cena (plane 0) + interface (plane 1), como o display do Xbox
if ($FidelityFx) { $env:VICTORY_FIDELITYFX_DIR = $FidelityFx }
$env:VICTORY_NO_WATCHDOG = '1'
$env:VICTORY_LOG         = 'info'
$env:NO_COLOR            = '1'

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$Logs  = Join-Path $env:LOCALAPPDATA "victory\logs\series-s-$stamp"
New-Item -ItemType Directory -Force -Path $Logs | Out-Null
Write-Host "Logs: $Logs" -ForegroundColor Cyan

$p = Start-Process -FilePath $Exe -ArgumentList @('run', "`"$Game`"", '--execute', '--console', 'series-s') `
        -WorkingDirectory $Repo -PassThru -NoNewWindow `
        -RedirectStandardOutput "$Logs\stdout.log" -RedirectStandardError "$Logs\stderr.log"
$peak = 0
while (-not $p.HasExited) {
    Start-Sleep -Seconds 5
    try { $p.Refresh(); $c = [int]($p.PrivateMemorySize64 / 1MB) } catch { continue }
    if ($c -gt $peak) { $peak = $c }
    $fps = (Select-String -Path "$Logs\stderr.log" -Pattern 'fps,' -ErrorAction SilentlyContinue | Select-Object -Last 1).Line
    Write-Host ("commit {0,6} MB  {1}" -f $c, ($fps -replace '^.*INFO [^:]+: ',''))
}
$crash = Select-String -Path "$Logs\stderr.log" -Pattern 'host GPU failure|panicked|0x8007000E' -ErrorAction SilentlyContinue | Select-Object -First 1
Write-Host "`nfim. pico commit: $peak MB" -ForegroundColor Cyan
if ($crash) { Write-Host "erro: $($crash.Line)" -ForegroundColor Red }
Write-Host "Mande a pasta de logs: $Logs" -ForegroundColor Yellow
