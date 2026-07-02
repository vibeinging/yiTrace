# 2026-07-02 带宽占用排查报告

## 结论

服务器 `116.204.69.92` 的带宽长期占用来自部署管理页面触发的 Gitee 拉取流量，不是业务 HTTP、Docker 容器或数据库流量。

核心链路：

- 前端 `static/index.html` 每 5 秒请求 `/api/status`
- `deploy-wy` / `deploy-wjm` 的 `/api/status` 每次都会调用 `engine.check_for_updates()`
- `check_for_updates()` 每次执行 `git fetch origin`
- `git fetch` 超时后，子进程 `ssh git@gitee.com git-upload-pack 'SmartDigit/agenticdata.git'` 没有被可靠清理，逐步堆积为孤儿进程
- 多条 SSH 到 `180.76.*:22` 的 Gitee 连接持续下载 pack 数据，造成入方向带宽占用

## 现场证据

排查时 `eth0` 入方向约 `10-12 Mbps`，出方向很低：

- `/proc/net/dev` 10 秒采样：`eth0 RX 11.07 Mbps，TX 0.34 Mbps`
- `iftop` 显示主要流量来自本机到 Gitee SSH：
  - `180.76.199.13:22`
  - `180.76.198.225:22`
  - `180.76.198.77:22`
- 进程为 `wujianming` 用户下的：
  - `ssh git@gitee.com git-upload-pack 'SmartDigit/agenticdata.git'`
  - `git fetch origin`
  - `git index-pack`

`deploy-wy/logs/service.log` 中从 2026-07-02 凌晨开始持续出现：

```text
Check update error: Command '['git', 'fetch', 'origin']' timed out after 60 seconds
```

上午 10 点后频率接近每分钟一次；同时页面前端配置了：

```text
setInterval(fetchStatus, 5000)
```

## 已执行处置

1. 清理了父进程已丢失的 Gitee SSH 孤儿进程。
2. 定位到三个部署管理实例：
   - `/home/agenticdata/deploy-wjm`
   - `/home/agenticdata/deploy-wy`
   - `/home/agenticdata/deploy-zww`
3. 热修三个实例的 `app.py`：
   - 状态检查路径从 `git fetch origin` 改为 `git ls-remote --heads origin <branch>`
   - 保留真实部署/checkout 时的 `git fetch`
4. 已备份原文件：
   - `/home/agenticdata/deploy-wy/app.py.bak.bandwidth.20260702114758`
   - `/home/agenticdata/deploy-wjm/app.py.bak.bandwidth.20260702114758`
   - `/home/agenticdata/deploy-zww/app.py.bak.bandwidth.20260702114758`
5. 通过 `python3 -m py_compile` 校验三个 `app.py`。
6. 通过 Supervisor 重启：
   - `deploy_web_wjm`
   - `deploy_web_wy`
   - `deploy_web_zww`

## 复测结果

重启并清理旧进程后：

- `ps` 中无 `git fetch origin` / `ssh git@gitee.com` / `git index-pack`
- `eth0` 15 秒采样降至：
  - `RX 0.03 Mbps`
  - `TX 0.03 Mbps`
- `iftop` 只剩零散 SSH 探测/登录流量，接收约 `12.8 Kbps`
- 30 秒后复查仍无新的 Gitee fetch/SSH 进程
- 三个部署管理服务均为 `RUNNING`

## 后续建议

这个热修直接改在服务器实例上，建议回写到部署管理系统源码，避免后续覆盖部署时回退。后续可以进一步把 `/api/status` 里的远端检查做成 60 秒缓存，或拆成独立的“手动检查更新”接口，避免状态接口承担网络 I/O。
