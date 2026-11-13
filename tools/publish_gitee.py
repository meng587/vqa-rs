# -*- coding: utf-8 -*-
"""Gitee 发布流水线（纯 API）——建仓 / Release / 便携包附件上传。

git 推送不在本脚本内（由发布会话直接执行 git 命令完成）。

★ 前置：需要**账号级**私人令牌（仓库级令牌无权建仓/发版）。
  Gitee 网页 → 设置 → 安全设置 → 私人令牌 → 生成新令牌
  → 勾选 `projects`（建仓与 Release 必需）
  → 用后即可在网页上删除该令牌。

用法（仓库根目录执行）：
  python tools/publish_gitee.py --token <令牌>                 # 建仓+发版+传附件
  python tools/publish_gitee.py --token <令牌> --skip-create   # 仓库已存在
  python tools/publish_gitee.py --token <令牌> --skip-release  # 只建仓

安全边界（硬编码，调用方不可放宽）：
  * 仅允许 https://gitee.com/api/v5 固定基址；域名白名单 + 解析 IP 私网阻断
  * 令牌只经参数/环境变量进入进程内存，不落盘、不写入 git remote
"""
import argparse
import ipaddress
import json
import os
import socket
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

API = "https://gitee.com/api/v5"
ALLOWED_HOST = "gitee.com"
ROOT = Path(__file__).resolve().parent.parent


def _guard_url(url: str) -> str:
    """SSRF 守卫：协议/域名白名单 + 解析 IP 必须公网。所有网络调用必经。"""
    p = urllib.parse.urlsplit(url)
    if p.scheme != "https" or p.hostname != ALLOWED_HOST:
        raise RuntimeError(f"拒绝非白名单地址：{url}")
    infos = socket.getaddrinfo(p.hostname, 443, proto=socket.IPPROTO_TCP)
    for info in infos:
        ip = ipaddress.ip_address(info[4][0])
        if not ip.is_global:
            raise RuntimeError(f"{p.hostname} 解析到非公网地址 {ip}，拒绝请求")
    return url


def api(path: str, token: str, data: dict | None = None) -> tuple[int, dict]:
    """调 Gitee API。path 必须以 / 开头（相对基址）。"""
    assert path.startswith("/")
    base = _guard_url(f"{API}{path}")
    if data is None:
        url = base + ("&" if "?" in base else "?") + urllib.parse.urlencode(
            {"access_token": token})
        body = None
    else:
        url = base
        body = urllib.parse.urlencode(dict(data, access_token=token)).encode()
    req = urllib.request.Request(url, data=body)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, json.load(r)
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read().decode("utf-8", "replace"))
        except Exception:
            return e.code, {}


def whoami(token: str) -> str:
    code, me = api("/user", token)
    if code != 200 or not me.get("login"):
        raise RuntimeError(f"查询账号失败 HTTP {code}（令牌无效或权限不足）")
    return me["login"]


def create_repo(token: str, repo: str, description: str) -> bool:
    """建私有仓库。已存在返回 False。"""
    code, d = api("/user/repos", token, {
        "name": repo,
        "description": description,
        "private": "true",
        "auto_init": "false",
    })
    if code == 201:
        print(f"建仓成功：{d.get('full_name')}（private={d.get('private')}）")
        return True
    msg = json.dumps(d, ensure_ascii=False)
    if code == 400 and ("exist" in msg or "已存在" in msg):
        print("仓库已存在，跳过创建")
        return False
    raise RuntimeError(f"建仓失败 HTTP {code}: {msg[:300]}")


def create_release(token: str, owner: str, repo: str, tag: str,
                   name: str, body: str) -> int:
    code, d = api(f"/repos/{owner}/{repo}/releases", token, {
        "tag_name": tag,
        "name": name,
        "body": body,
        "prerelease": "false",
    })
    if code != 201:
        raise RuntimeError(
            f"建 Release 失败 HTTP {code}: {json.dumps(d, ensure_ascii=False)[:300]}")
    print(f"Release {tag} 已创建（id={d['id']}）")
    return d["id"]


def upload_attachment(token: str, owner: str, repo: str, release_id: int,
                      zip_path: Path) -> None:
    data = zip_path.read_bytes()
    boundary = "----vqa" + zip_path.stem.replace(".", "")
    payload = (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="file"; filename="{zip_path.name}"\r\n'
        f"Content-Type: application/zip\r\n\r\n"
    ).encode() + data + f"\r\n--{boundary}--\r\n".encode()
    url = _guard_url(
        f"{API}/repos/{owner}/{repo}/releases/{release_id}/attach_files")
    req = urllib.request.Request(url, data=payload)
    req.add_header("Content-Type", f"multipart/form-data; boundary={boundary}")
    try:
        with urllib.request.urlopen(req, timeout=300) as r:
            print(f"附件已上传：{zip_path.name}（{r.status}）")
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"附件上传失败 HTTP {e.code}") from e


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--token", default=None,
                    help="账号级私人令牌（缺省读环境变量 VQA_GITEE_TOKEN）")
    ap.add_argument("--repo", default="video-analysis-toolkit-rs")
    ap.add_argument("--skip-create", action="store_true")
    ap.add_argument("--skip-release", action="store_true")
    ap.add_argument("--zip", default=None, help="便携包路径（缺省 dist/ 自动）")
    args = ap.parse_args()
    token = args.token or os.environ.get("VQA_GITEE_TOKEN", "")
    if not token:
        print("缺令牌：--token 或环境变量 VQA_GITEE_TOKEN（需账号级、勾 projects）")
        return 2
    version = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    version = version.split('version = "')[1].split('"')[0]

    if not args.skip_create:
        create_repo(token, args.repo,
                    "视听质量检测工作站（Rust 版）—— Python 版 video-analysis-toolkit 的 Rust 重写")
    owner = whoami(token)
    print(f"账号：{owner}")

    if not args.skip_release:
        zip_path = (Path(args.zip) if args.zip else next(
            iter(sorted((ROOT / "dist").glob("*-portable.zip"))), None))
        if zip_path is None or not zip_path.exists():
            print("未找到便携包（dist/*-portable.zip）")
            return 1
        print(f"便携包：{zip_path.name}（{zip_path.stat().st_size / 1e6:.2f} MB）")
        rid = create_release(
            token, owner, args.repo, f"v{version}",
            f"v{version} 首期：播放器 / 视频分析",
            "Rust 重写第一期交付。\n\n"
            "- vqa-core：TS 分析（TR101290/PCR-PLL）与 Python 版对拍一致\n"
            "- 播放器页：mpv 进程外 IPC + AVS 自动转码桥\n"
            "- 视频分析页：7 页签 + 实时 UDP/RTP + Markdown 报告\n\n"
            "附件为 exe 便携包（工具链不入包，见包内说明）。",
        )
        upload_attachment(token, owner, args.repo, rid, zip_path)
    print("完成。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
