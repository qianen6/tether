<#
.SYNOPSIS
  在这台 Windows 电脑上从源码安装 Tether 的电脑端服务。

.DESCRIPTION
  依次：检查 Rust 和 C++ 编译工具；下载 TailPocket 隧道程序并核对仓库里记录的
  SHA-256；编译 tether（命令行版，不打安装包）；注册"登录时运行"的计划任务并立即
  在后台启动；列出能用的 agent；在浏览器里打开配对二维码。

  可以重复运行：更新代码（git pull）后再运行一次就是升级。

  不注册成 Windows 服务：服务以系统账户运行，读不到你自己的 Codex、Claude、DSH
  的登录信息和配置。

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\install.ps1
  powershell -ExecutionPolicy Bypass -File scripts\install.ps1 -NoPair
  powershell -ExecutionPolicy Bypass -File scripts\install.ps1 -Uninstall
#>
param(
  # 不注册登录时自动运行，只编译并启动这一次。
  [switch]$NoAutostart,
  # 不打开配对二维码（之后运行 tether pair --open）。
  [switch]$NoPair,
  # 停止 Tether 并删除自动运行；配对和设置留在数据目录里。
  [switch]$Uninstall,
  # 数据目录（默认 %APPDATA%\Tether，和桌面版相同）。
  [string]$DataDir = '',
  # 计划任务的名字。
  [string]$TaskName = 'Tether'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$Root = Split-Path -Parent $PSScriptRoot
$Exe = Join-Path $Root 'target\release\tether.exe'
$Starter = Join-Path $PSScriptRoot 'start-tether.ps1'
$DataArgs = @()
if ($DataDir) { $DataArgs = @('--data-dir', $DataDir) }

function Step($text) { Write-Host "==> $text" -ForegroundColor Cyan }

# Windows PowerShell 5.1 在 'Stop' 下会把外部程序写到 stderr 的内容（例如 cargo
# 的进度）当成错误抛出：外部程序一律在 'Continue' 下运行，只看退出码。
function Invoke-Native([scriptblock]$Block) {
  $saved = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try { & $Block } finally { $ErrorActionPreference = $saved }
}

function Stop-Tether {
  if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
    Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
  }
  # 隧道程序会随它一起退出。
  Get-Process tether -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -eq $Exe } |
    Stop-Process -Force -ErrorAction SilentlyContinue
}

if ($Uninstall) {
  Step '停止 Tether 并删除自动运行'
  Stop-Tether
  if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
  }
  Write-Host '已卸载。配对过的手机和设置还在数据目录里；源码目录可以直接删除。'
  return
}

# 桌面版和服务用同一个数据目录，同时运行会互相抢。
if (-not $DataDir -and (Get-Process tether-desktop -ErrorAction SilentlyContinue)) {
  throw '电脑上正在运行 Tether 桌面版。请先在托盘里退出它（之后用这里装的服务代替它），再运行本脚本。'
}

Step '检查编译工具'
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
  if (Test-Path (Join-Path $cargoBin 'cargo.exe')) {
    $env:Path = "$cargoBin;$env:Path"
  } else {
    throw '没有找到 Rust（cargo）。先安装：winget install --id Rustlang.Rustup -e ，装好后重新打开终端，再运行本脚本。'
  }
}
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vc = $null
if (Test-Path $vswhere) {
  $vc = Invoke-Native { & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath }
}
if (-not $vc) {
  throw '没有找到 C++ 编译工具（Rust 在 Windows 上要用它链接）。先安装：winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended" ，装好后再运行本脚本。'
}

Step '下载 TailPocket 隧道程序'
# 版本和 scripts/fetch-vendor.sh 保持一致；校验值用仓库里提交的，不用现下载的。
$fetch = Get-Content (Join-Path $PSScriptRoot 'fetch-vendor.sh') -Raw
if ($fetch -notmatch 'TAILPOCKET_VERSION:-(v[0-9.]+)') { throw 'scripts/fetch-vendor.sh 里没有找到 TailPocket 的版本' }
$version = $Matches[1]
$file = 'tailpocket-host-windows-amd64.exe'
$want = $null
foreach ($line in Get-Content (Join-Path $Root 'vendor\tailpocket\SHA256SUMS')) {
  $parts = $line.Trim() -split '\s+'
  if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $file) { $want = $parts[0].ToLower() }
}
if (-not $want) { throw "vendor/tailpocket/SHA256SUMS 里没有 $file" }
$bin = Join-Path $Root 'vendor\tailpocket\bin'
$dest = Join-Path $bin $file
if ((Test-Path $dest) -and ((Get-FileHash $dest -Algorithm SHA256).Hash.ToLower() -eq $want)) {
  Write-Host "已有 $file（$version，校验通过）"
} else {
  New-Item -ItemType Directory -Force $bin | Out-Null
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
  $part = "$dest.part"
  Invoke-WebRequest -UseBasicParsing "https://github.com/jt-wang/tailpocket/releases/download/$version/$file" -OutFile $part
  $got = (Get-FileHash $part -Algorithm SHA256).Hash.ToLower()
  if ($got -ne $want) {
    Remove-Item $part -Force
    throw "下载的 $file 校验不对：应为 $want，实际 $got。没有使用它。"
  }
  Move-Item -Force $part $dest
  Write-Host "已下载 $file（$version，校验通过）"
}

Step '停止正在运行的 Tether（如果有）'
Stop-Tether

Step '编译 tether（第一次要几分钟）'
Push-Location $Root
try {
  Invoke-Native { cargo build --release --locked -p tether }
  if ($LASTEXITCODE -ne 0) { throw "编译失败（cargo 退出码 $LASTEXITCODE），见上面的输出" }
} finally {
  Pop-Location
}

$starterArgs = "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$Starter`""
if ($DataDir) { $starterArgs += " -DataDir `"$DataDir`"" }
if ($NoAutostart) {
  Step '在后台启动 Tether'
  Start-Process powershell.exe -ArgumentList $starterArgs -WindowStyle Hidden
} else {
  Step "注册登录时自动运行（计划任务 $TaskName）并启动"
  $user = "$env:USERDOMAIN\$env:USERNAME"
  $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $starterArgs
  $trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
  $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
  $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
  Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger -Principal $principal `
    -Settings $settings -Description 'Tether：让手机控制这台电脑上的编程 agent' -Force | Out-Null
  Start-ScheduledTask -TaskName $TaskName
}

Step '等待 Tether 就绪'
$ready = $false
for ($i = 0; $i -lt 60; $i++) {
  Start-Sleep -Seconds 1
  Invoke-Native { & $Exe @DataArgs agents 2>$null | Out-Null }
  if ($LASTEXITCODE -eq 0) { $ready = $true; break }
}
if (-not $ready) {
  $log = if ($DataDir) { Join-Path $DataDir 'tether.log' } else { Join-Path $env:APPDATA 'Tether\tether.log' }
  throw "Tether 60 秒内没有就绪。看日志：$log"
}
Invoke-Native { & $Exe @DataArgs agents }

if (-not $NoPair) {
  Step '配对手机'
  Invoke-Native { & $Exe @DataArgs pair --open }
  if ($LASTEXITCODE -ne 0) { Write-Warning '配对二维码没有打开，稍后运行 targetelease	ether.exe pair --open' }
}

Write-Host ''
Write-Host 'Tether 已在后台运行。常用命令（在源码目录里）：' -ForegroundColor Green
Write-Host '  target\release\tether.exe pair --open      再配对一台手机'
Write-Host '  target\release\tether.exe agents           看哪些 agent 可用'
Write-Host '  target\release\tether.exe locate <agent> <位置>   手动指定程序位置（DSH 也可以填 dsh web 打印的地址）'
Write-Host '  powershell -ExecutionPolicy Bypass -File scripts\install.ps1 -Uninstall   卸载'
