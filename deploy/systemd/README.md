# systemd 用户服务

此部署方式面向 Linux 上启用了 systemd 用户管理器的登录用户。仓库发行归档当前为 Linux x86_64 和 AArch64 构建；其他系统服务管理器不在本指南支持范围内。

发行包是包含 `usr/bin/` 和 `usr/lib/systemd/user/` 路径的 `.tar.gz` 归档，本身不会安装文件。先从仓库根目录构建归档，再将归档内容解包到系统根目录：

```sh
cargo build --release --locked --workspace --bin mcbtodo --bin mcbtodo-daemon --bin mcbtodo-worker
cargo xtask package
sudo tar -xzf target/dist/mcbtodo-linux-$(uname -m).tar.gz -C /
```

`uname -m` 应为 `x86_64` 或 `aarch64`。解包会写入 `/usr/bin/` 和 `/usr/lib/systemd/user/`，需要管理员权限。安装后，在登录用户的终端运行：

归档同时包含 `/usr/bin/mcbtodo-worker`。Daemon 会为每个脚本或 Agent
执行尝试启动并监督该 Worker，因此该二进制必须与 Daemon 一起安装在
`/usr/bin/`。Worker 不是独立的 systemd 服务，也不单独常驻。

服务默认启动一个 Worker 槽。需要并行执行时，可在服务覆盖配置中为
`mcbtodo-daemon` 增加 `--worker-slots 2`，允许值为 1 到 4。槽位全部占用时，
后续工作保持 `pending`，重启恢复和 `outcome_unknown` 语义不变。

```sh
mcbtodo init
systemctl --user daemon-reload
systemctl --user enable --now mcbtodo-daemon.service
mcbtodo timer status --json
```

如果服务未启动，先查看服务状态和日志。

```sh
systemctl --user status mcbtodo-daemon.service
journalctl --user-unit mcbtodo-daemon.service -b -n 50
```

- `Failed to connect to bus` 表示当前终端无法连接用户管理器。在目标用户的登录会话中运行命令。
- `203/EXEC` 时检查 `/usr/bin/mcbtodo-daemon` 是否存在、可执行，并与系统架构匹配。
- 若日志提示状态目录缺失，先以服务用户运行 `mcbtodo init`。若提示目录权限错误，检查 `~/.mcbtodo/state` 的属主和权限。该目录应为当前用户所有，权限为 `0700`。
- 若日志提示已有守护进程持有锁，先停止在终端中手动启动的 `mcbtodo-daemon`，再重启用户服务。

从仓库根目录可运行实际的用户服务冒烟测试。`xtask` 用 Bubblewrap（`bwrap`）挂载临时 home，在 systemd 用户管理器中启动临时服务，验证计时检查点、服务自动重启以及 Markdown 和 JSON 报告。JSON 由 Rust 解析。

```sh
cargo build --release --locked --workspace --bin mcbtodo --bin mcbtodo-daemon --bin mcbtodo-worker
cargo xtask smoke-systemd
```
