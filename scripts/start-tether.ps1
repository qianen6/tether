<#
.SYNOPSIS
  在后台（不开窗口）启动 tether serve。由 install.ps1 注册的计划任务在登录时运行。
  日志在数据目录的 tether.log 里。
#>
param([string]$DataDir = '')

$Exe = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\tether.exe'
$arguments = @()
if ($DataDir) { $arguments += @('--data-dir', "`"$DataDir`"") }
$arguments += 'serve'
Start-Process -FilePath $Exe -ArgumentList $arguments -WindowStyle Hidden
