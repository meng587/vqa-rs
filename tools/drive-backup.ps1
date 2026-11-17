# 豆包云盘（飞书 Drive 后端）按版本备份 —— 每次发版把「源码快照 + 安装包 + 清单」推到 <根目录>/<版本号>/ 子目录
#
# 用法（必须用 pwsh，Windows PowerShell 5.1 下原生命令输出捕获会失效）：
#   pwsh -File tools\drive-backup.ps1                 # 按 Cargo.toml 当前版本备份，版本目录自动创建，同名文件跳过
#   pwsh -File tools\drive-backup.ps1 -Force          # 同名文件原地覆盖（保留 file_token）
#   pwsh -File tools\drive-backup.ps1 -CheckOnly      # 只列远程目录，不上传
#
# 前置：lark-cli 已登录（身份 user），且对目标文件夹有写权限。
# 备份根目录：02.程序开发软件包 / vqa-rs（folder_token V7t8f98Volf0P6dfLIDcqQw3nIg）

param(
  [string]$Version = "",
  [string]$FolderToken = "V7t8f98Volf0P6dfLIDcqQw3nIg",
  [switch]$Force,
  [switch]$CheckOnly
)

# 原生命令 stderr 在 PS 里会产生 NativeCommandError，统一降级为 Continue 并自行检查 ok 字段
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $root

# ---- 1. 版本（唯一来源 crates/vqa-tauri/Cargo.toml）----
if (-not $Version) {
  $toml = Get-Content -LiteralPath (Join-Path $root 'crates\vqa-tauri\Cargo.toml') -Raw
  $Version = [regex]::Match($toml, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
}
if (-not $Version) { throw '读不到版本号' }
Write-Host "版本：$Version"

# ---- 2. 远程读写工具 ----
function Invoke-Lark {
  param([string[]]$CliArgs)
  $raw = & lark-cli @CliArgs 2>$null
  return ($raw | Out-String)
}

function Get-FolderFiles([string]$token) {
  $raw = Invoke-Lark @('drive', 'files', 'list', '--params', "{`"folder_token`":`"$token`",`"page_size`":200}", '--format', 'json')
  $json = $raw | ConvertFrom-Json
  if (-not $json.ok) { throw "lark-cli 读目录失败（$token）：$raw" }
  return @($json.data.files)
}

# ---- 3. 定位 / 创建版本子目录 ----
$siblings = Get-FolderFiles $FolderToken
$vFolder = $siblings | Where-Object { $_.type -eq 'folder' -and $_.name -eq $Version } | Select-Object -First 1
if (-not $vFolder) {
  if ($CheckOnly) {
    Write-Host "版本目录 $Version 尚不存在（正式执行时会自动创建）"
    $Target = $FolderToken
  } else {
    $raw = Invoke-Lark @('drive', '+create-folder', '--folder-token', $FolderToken, '--name', $Version, '--format', 'json')
    $json = $raw | ConvertFrom-Json
    if (-not $json.ok) { throw "创建版本目录失败：$raw" }
    $Target = $json.data.folder_token
    Write-Host "已创建版本目录：$Version  ($Target)"
  }
} else {
  $Target = $vFolder.token
  Write-Host "版本目录已存在：$Version  ($Target)"
}

$remote = Get-FolderFiles $Target
Write-Host "远程 $Version 目录现有 $($remote.Count) 个文件"
foreach ($f in $remote) { Write-Host ("  - {0}  {1}" -f $f.name, $f.token) }
if ($CheckOnly) {
  Write-Host "根目录下的版本子目录："
  foreach ($f in ($siblings | Where-Object { $_.type -eq 'folder' })) { Write-Host ("  - {0}" -f $f.name) }
  return
}

# ---- 4. 本地产物：dist/<版本>/ 下的安装包副本 + 源码包 + 清单 ----
$verDir = Join-Path $root "dist\$Version"
New-Item -ItemType Directory -Force -Path $verDir | Out-Null

$instSrc = Join-Path $root "target\release\bundle\nsis\视听质量检测工作站_${Version}_x64-setup.exe"
if (-not (Test-Path -LiteralPath $instSrc)) {
  throw "找不到安装包：$instSrc（先跑 tools\release.ps1）"
}
$instDst = Join-Path $verDir "vqa-workbench-$Version-setup.exe"
Copy-Item -LiteralPath $instSrc -Destination $instDst -Force

$zip = Join-Path $verDir "vqa-rs_${Version}_source.zip"
$sha = (git stash create).Trim()
if (-not $sha) { $sha = (git rev-parse HEAD).Trim() }
git archive --format=zip -o $zip $sha
if (-not (Test-Path -LiteralPath $zip)) { throw "源码包生成失败：$zip" }

$head = (git rev-parse HEAD).Trim()
$h1 = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash
$h2 = (Get-FileHash -LiteralPath $instDst -Algorithm SHA256).Hash
$txt = @"
vqa-rs 备份清单
生成时间：$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')
版本：$Version（version 单一来源 crates/vqa-tauri/Cargo.toml）
Git HEAD：$head
打包快照：$sha（git stash create，含未提交改动，不含 target/node_modules）

文件：
  vqa-rs_${Version}_source.zip        源码快照
    SHA256 $h1
  vqa-workbench-$Version-setup.exe    Windows 安装包 NSIS x64（已签名）
    SHA256 $h2
"@
$man = Join-Path $verDir "备份清单_$Version.txt"
[IO.File]::WriteAllText($man, $txt, (New-Object Text.UTF8Encoding $true))

# ---- 5. 上传到版本目录 ----
function Send-One([string]$path, [string]$name) {
  $exist = $remote | Where-Object { $_.name -eq $name } | Select-Object -First 1
  if ($exist -and -not $Force) {
    Write-Host "跳过（已存在）：$name"
    return
  }
  $cliArgs = @('drive', '+upload', '--file', $path, '--folder-token', $Target, '--name', $name, '--format', 'json')
  if ($exist -and $Force) { $cliArgs += @('--file-token', $exist.token) }
  $raw = Invoke-Lark $cliArgs
  $json = $raw | ConvertFrom-Json
  if (-not $json.ok) { throw "上传失败 $name：$raw" }
  Write-Host ("上传完成：{0}  {1}  {2}" -f $json.data.file_name, $json.data.size, $json.data.url)
}

Send-One $zip ("vqa-rs_${Version}_source.zip")
Send-One $instDst ("vqa-workbench-$Version-setup.exe")
Send-One $man ("备份清单_$Version.txt")

Write-Host "---- 完成 ----"
Write-Host "版本目录：https://www.doubao.com/chat/drive/folder/$Target"
