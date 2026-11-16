# 一键打包 + 签名 + 生成 latest.json
# 用法： powershell -File tools\release.ps1 [-Notes "更新说明"]
# 口令来源优先级：-Pass 参数 > 环境变量 VQA_SIGN_PASS > %APPDATA%\vqa\signing.txt
param(
  [string]$Notes = "",
  [string]$Pass = ""
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $root

# ---- 1. 读口令 ----
if (-not $Pass) { $Pass = $env:VQA_SIGN_PASS }
if (-not $Pass) {
  $pf = Join-Path $env:APPDATA 'vqa\signing.txt'
  if (Test-Path -LiteralPath $pf) { $Pass = (Get-Content -LiteralPath $pf -Raw).Trim() }
}
if (-not $Pass) { throw '缺少签名口令：设置 VQA_SIGN_PASS，或写入 %APPDATA%\vqa\signing.txt' }

$key = Join-Path $root 'keys\tauri.key'
if (-not (Test-Path -LiteralPath $key)) { throw "找不到签名私钥：$key" }

# ---- 2. 读版本（唯一来源：crates/vqa-tauri/Cargo.toml） ----
$toml = Get-Content -LiteralPath (Join-Path $root 'crates\vqa-tauri\Cargo.toml') -Raw
$ver = [regex]::Match($toml, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $ver) { throw 'Cargo.toml 里没读到 version' }
Write-Host "版本：$ver"

# ---- 3. 前端 + 打包（带签名） ----
$ui = Join-Path $root 'crates\vqa-tauri\ui'
Push-Location -LiteralPath $ui
& npm run build
Pop-Location

$env:TAURI_SIGNING_PRIVATE_KEY = $key
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $Pass
Push-Location -LiteralPath (Join-Path $root 'crates\vqa-tauri')
& '.\ui\node_modules\.bin\tauri.cmd' build
$buildCode = $LASTEXITCODE
Pop-Location
if ($buildCode -ne 0) { throw "tauri build 失败（exit $buildCode）" }

# ---- 4. 产物：中文名 → 英文名（Gitee 资产名利于下载） ----
$nsis = Join-Path $root 'target\release\bundle\nsis'
$cn = Join-Path $nsis "视听质量检测工作站_${ver}_x64-setup.exe"
$en = Join-Path $nsis "vqa-workbench-${ver}-setup.exe"
if (-not (Test-Path -LiteralPath $cn)) { throw "没找到安装包：$cn" }
Copy-Item -LiteralPath $cn -Destination $en -Force

$sigFile = "$cn.sig"
if (-not (Test-Path -LiteralPath $sigFile)) { throw "没生成签名文件：$sigFile（检查 tauri.conf.json 的 createUpdaterArtifacts）" }
$sig = ((Get-Content -LiteralPath $sigFile -Raw).Trim()) -replace "`r", ""

# ---- 5. 写 latest.json ----
if (-not $Notes) { $Notes = "视听质量检测工作站 $ver" }
$doc = [ordered]@{
  version   = $ver
  notes     = $Notes
  pub_date  = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
  platforms = [ordered]@{
    'windows-x86_64' = [ordered]@{
      signature = $sig
      url       = "https://gitee.com/meng587/vqa-rs/releases/download/v$ver/vqa-workbench-$ver-setup.exe"
    }
  }
}
$json = $doc | ConvertTo-Json -Depth 6
[IO.File]::WriteAllText((Join-Path $root 'crates\vqa-tauri\update\latest.json'), $json + "`n", (New-Object Text.UTF8Encoding $false))

"---- 完成 ----"
"安装包 : $en  ({0:N1} MB)" -f ((Get-Item -LiteralPath $en).Length / 1MB)
"签名   : $sigFile  ($($sig.Length) 字符)"
"清单   : crates\vqa-tauri\update\latest.json (version=$ver)"
"下一步 : 上传安装包到 Release v$ver，然后 git 提交 latest.json"
