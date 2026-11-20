# 下载并整理 EasyTier Windows x86_64 内核（供 VQA 安装包内置）
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$ver = '2.6.4'
$dir = Join-Path $root 'bin\easytier'
$zip = Join-Path $dir "easytier-windows-x86_64-v$ver.zip"
$tmp = Join-Path $dir 'extract'
$sha256 = '27AF91E270E554709B048BD32327FEFD2DFCE5062AE1E8701AF7550C6F525F84'

New-Item -ItemType Directory -Force -Path $dir | Out-Null
if (-not (Test-Path -LiteralPath $zip)) {
    Invoke-WebRequest -Uri "https://github.com/EasyTier/EasyTier/releases/download/v$ver/easytier-windows-x86_64-v$ver.zip" -OutFile $zip
}
$actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash
if ($actual -ne $sha256) {
    Remove-Item -LiteralPath $zip -Force
    throw "EasyTier 安装包校验失败：$actual，期望 $sha256"
}
if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Recurse -Force }
Expand-Archive -LiteralPath $zip -DestinationPath $tmp -Force

$src = Join-Path $tmp "easytier-windows-x86_64"
foreach ($name in 'easytier-core.exe', 'easytier-cli.exe', 'wintun.dll', 'Packet.dll', 'WinDivert64.sys') {
    Copy-Item -LiteralPath (Join-Path $src $name) -Destination (Join-Path $dir $name) -Force
}
"EasyTier $ver Windows x86_64 已整理到 $dir"
Get-ChildItem -LiteralPath $dir -File | Select-Object Name, Length
