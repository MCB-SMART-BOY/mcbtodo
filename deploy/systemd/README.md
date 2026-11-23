# systemd 用户服务

此部署方式面向 Linux 上启用了 systemd 用户管理器的登录用户。仓库发行归档当前为 Linux x86_64 和 AArch64 构建；其他系统服务管理器不在本指南支持范围内。

## 通过 Cargo 安装到当前用户

从任意目录运行以下命令。`--root` 将三个程序统一装到 `~/.cargo/bin`，与下方的 systemd 服务路径一致。终端的 `PATH` 需要包含 `~/.cargo/bin`。

```sh
cargo install --locked --root "$HOME/.cargo" \
  --git https://github.com/MCB-SMART-BOY/mcbtodo.git \
  mcbtodo mcbtodo-daemon mcbtodo-worker
```

也可以从本地工作树安装。在仓库根目录分别执行 `cargo install --locked --root "$HOME/.cargo" --path apps/mcbtodo`，并对 `apps/mcbtodo-daemon` 和 `apps/mcbtodo-worker` 执行相同命令。

首次安装时，在仓库根目录用随附的模板创建用户服务。已有用户服务可以用 `systemctl --user edit --full mcbtodo-daemon.service` 修改 `ExecStart`，保留其余设置。该命令也可用来将 `ExecStart` 指向自定义 `--root` 下的 `bin/mcbtodo-daemon`。

```sh
mkdir -p "$HOME/.config/systemd/user"
sed 's|^ExecStart=/usr/bin/mcbtodo-daemon$|ExecStart=%h/.cargo/bin/mcbtodo-daemon|' \
  deploy/systemd/mcbtodo-daemon.service > "$HOME/.config/systemd/user/mcbtodo-daemon.service"
```

只有安装程序的步骤需要 Cargo。服务配置和启动由 systemd 管理。
继续执行下方的“启动和使用”。更新时重新执行所选安装命令，然后执行
`systemctl --user restart mcbtodo-daemon.service`。从本地工作树重装同一版本时，给三条命令加上 `--force`。

## 从发行包安装到系统目录

发行包是包含 `usr/bin/` 和 `usr/lib/systemd/user/` 路径的 `.tar.gz` 归档，本身不会安装文件。先从仓库根目录构建归档，再将归档内容解包到系统根目录：

```sh
cargo build --release --locked --workspace --bin mcbtodo --bin mcbtodo-daemon --bin mcbtodo-worker
cargo xtask package
sudo tar -xzf target/dist/mcbtodo-linux-$(uname -m).tar.gz -C /
```

`uname -m` 应为 `x86_64` 或 `aarch64`。解包会写入 `/usr/bin/` 和 `/usr/lib/systemd/user/`，需要管理员权限。

两种安装方式都将 `mcbtodo-worker` 放在 Daemon 同一目录。Daemon 在脚本或 Agent 执行期间启动并监督 Worker。

## 启动和使用

```sh
mcbtodo init
systemctl --user daemon-reload
systemctl --user enable --now mcbtodo-daemon.service
mcbtodo timer status --json
```

服务启动后，在任意终端运行 `mcbtodo` 打开 TUI。`:timer status` 查询服务中的计时。在 Outline 选中 Project，按 `Space c t` 通过服务保存 Task。按 `q` 关闭 TUI，再次打开可以继续查看同一计时会话。服务跟随 systemd 用户管理器，在用户登录时启动。

服务默认使用一个 Worker 槽。可以在 TUI 的 Settings → Daemon 中设置 1 到 4
个槽，保存后运行 `systemctl --user restart mcbtodo-daemon.service`。服务覆盖
配置中的 `--worker-slots` 启动参数优先。槽位全部占用时，后续工作保持
`pending`，重启后已启动但未确认的尝试按 `outcome_unknown` 处理。

如果服务未启动，先查看服务状态和日志。

```sh
systemctl --user status mcbtodo-daemon.service
journalctl --user-unit mcbtodo-daemon.service -b -n 50
```

- `Failed to connect to bus` 表示当前终端无法连接用户管理器。在目标用户的登录会话中运行命令。
- `203/EXEC` 时使用 `systemctl --user cat mcbtodo-daemon.service` 查看 `ExecStart`，检查它指向的程序是否存在、可执行，并与系统架构匹配。
- 若日志提示状态目录缺失，先以服务用户运行 `mcbtodo init`。若提示目录权限错误，检查 `~/.mcbtodo/state` 的属主和权限。该目录应为当前用户所有，权限为 `0700`。
- 若日志提示已有守护进程持有锁，先停止在终端中手动启动的 `mcbtodo-daemon`，再重启用户服务。

从仓库根目录可运行实际的用户服务冒烟测试。`xtask` 用 Bubblewrap（`bwrap`）挂载临时 home，在 systemd 用户管理器中启动临时服务。测试在两个先后启动的 TUI 进程中读取同一个持续计时的会话，通过 TUI 创建 Task，再验证计时检查点、服务自动重启以及 Markdown 和 JSON 报告。JSON 由 Rust 解析。

```sh
cargo build --release --locked --workspace --bin mcbtodo --bin mcbtodo-daemon --bin mcbtodo-worker
cargo xtask smoke-systemd
```
