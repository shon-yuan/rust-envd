# rust-envd

[English](README.md) | 简体中文

使用 Rust 实现的 [e2b](https://github.com/e2b-dev/runtime) envd 沙箱守护进程，功能对齐固定的上游版本
`0.10.2`（提交 `51083a83cc0903185d992725ea6bb33683814b29`）。覆盖进程运行时、
文件系统 REST 与 Connect RPC、PTY 与文件监听、MMDS 元数据生命周期、初始化操作
（时钟、CA 证书包、hyperloop、cgroups 与冻结）、端口转发、升级交接和守护进程 CLI。

本中文版本对应同目录的[英文 README](README.md)；命令、配置项、接口名称和版本号保持一致。

项目采用 [MIT 许可证](LICENSE)，但以下第三方内容保留各自的许可证与归属说明：

- `third_party/e2b/` 包含按原始字节重新分发的上游 protobuf schema，采用
  [Apache-2.0 许可证](third_party/e2b/LICENSE)，并附有完整的
  [来源记录](third_party/e2b/PROVENANCE)。基于这些 schema 生成的代码保留相同归属说明。
- `src/platform/linux/mmds/go_url.rs` 中源自 Go 的重定向 URL 辅助代码保留
  The Go Authors 的 BSD-3-Clause 声明。
- `vendor/connectrpc/` 是仓库内维护的 `connectrpc` 0.9.1 分支，采用 Apache-2.0，
  用于对齐 connect-go 的错误文本；修改记录见 `vendor/connectrpc/PATCHES.md`。
- `vendor/hyper/` 是仓库内维护的 `hyper` 1.12.0 分支，采用 MIT，
  实现了 Go `net/http` 的 HTTP/1.1 trailer 写入路径和分块请求体限制；修改记录见
  `vendor/hyper/PATCHES.md`。
- `vendor/cua-driver/` 包含采用 MIT 许可证的 Cua Driver crate，供可选桌面构建和
  `coorworker-cua` worker 使用；来源记录见 `vendor/cua-driver/PROVENANCE.md`。
  不包含采用 AGPL 的 perception crate 和采用 FSL 的 `cua-spacesd`。

可分发的第三方声明包包括 [`third_party/NOTICES.md`](third_party/NOTICES.md) 和
`third_party/licenses/`；worker 的独立声明包位于 `third_party/cua-worker/`。
声明包与发布清单的生成流程见 [`third_party/PACKAGING.md`](third_party/PACKAGING.md)。
依赖安全更新、仍存在的维护告警，以及 Linux 升级交接的文件描述符所有权迁移，记录在
[`docs/dependency-security.md`](docs/dependency-security.md)。

## 支持的平台

Linux x86_64 是差分一致性验证的目标平台。下文的差分对比及
[`docs/acceptance-notes.md`](docs/acceptance-notes.md) 中的差分验收均为 Linux 证据。

Windows x86_64 是受支持的客户机目标平台，已在 Windows 11 上测试，通过原生 Windows 后端
提供相同 API。两种平台的守护进程均提供全部 13 项 REST 操作和 17 项 Connect 过程，
使用相同的 schema、状态码、错误信封和 Connect 帧格式：

- **进程**：通过 `CreateProcessW` 和 Job Object 管理进程，使用 ConPTY 伪终端，
  通过 `CTRL_BREAK`、ETX 和 `WM_CLOSE` 处理终止信号，并保留已退出进程的记录。
- **文件系统**：提供 REST 和 RPC 接口，文件监听基于 `ReadDirectoryChangesW`。
- **资源**：通过 Win32 计数器获取指标，通过挂起线程冻结与解冻，并实现 collapse 和 fsfreeze 后端。
- **端口**：通过 `GetExtendedTcpTable` 扫描端口，并在 `169.254.0.21` 上提供进程内回环转发器。
- **`/init`**：配置 token 和默认值，将 CA 安装到 Windows 证书存储，通过 `SetSystemTime`
  校正与调整时钟，执行解冻，并通过 DPAPI 持久化元数据。
- **生命周期**：接管传入的交接状态、执行向外的在线升级，以及 SCM 服务生命周期。
  服务启动天然属于客户机模式；控制台启动需要 `COORWORKER_ENVD=1`，
  并与参考实现一样绑定 `0.0.0.0:49983`。

使用 Windows MSVC 工具链构建，并将 `PROTOC` 指向 `protoc` 可执行文件：

```sh
cargo build --locked --bin coorworker-envd
```

验收证据：[依赖维护回归](docs/windows-dependency-regression.md)在 Windows 11 上原生执行，
测试提交为 `c3cb81a`。321 项库测试、基础构建的 5 项或桌面构建的 9 项集成测试，
以及独立 worker 的 16 项测试均通过。官方 E2B Python SDK 2.62.1 使用自身的编解码器、
生成客户端和 `CommandHandle` 驱动实际客户机，通过 27/27 项测试；测试脚本位于 agent 仓库的
`scripts/tests/envd-parity/sdk_conformance.py`。

Windows 没有对应的 Go 参考实现，因此上述结果属于接口契约验证，不构成差分一致性声明。
已声明的差异包括：冻结通过挂起线程实现，不使用 cgroup v2；进程以守护进程自身账户运行，
不支持任意用户身份模拟；SDK 的 `commands.run` 硬编码了 `/bin/bash -l -c`，
Windows 客户机没有 POSIX shell，因此以 `invalid_argument` 拒绝该调用。

完整 API 矩阵、语义、原生验收记录和待决事项见
[`docs/windows-port.md`](docs/windows-port.md)。在 Cloud Hypervisor/Hyper-V 下启动 Windows
的客户机镜像流水线仍是独立的待完成工作；守护进程本身已可在现有 Windows VM 中运行。

## 扩展

上述官方 envd 接口是保持冻结的兼容性契约。扩展在 envd 入口与授权层之后，
挂载到各自独立的一级路径下，并自行定义认证策略。显式启用的共享 CUA 模式读取当前受保护的
envd 凭据，不改变 S0 的请求头、例外规则或错误行为。未配置扩展时，路由仍为兼容性守护进程。

CUA 支持以下部署模式：

| `COORWORKER_CUA_MODE` | 执行方式 | 客户端凭据 |
| --- | --- | --- |
| 未设置或 `legacy` | 沿用受监督或由 envd 接入的独立 `coorworker-cua` worker | 原有 worker/proxy token |
| `embedded` | 在桌面用户会话中的 envd 进程内执行 | 当前 envd `/init` token，以 Bearer 形式发送 |
| `shared-external` | 由部署方管理的用户会话 helper | 当前 envd `/init` token，以 Bearer 形式发送 |

桌面可执行文件的构建命令为 `cargo build --locked -j4 --features cua-desktop`。
Windows 原生桌面构建和独立 worker 构建还需要安装与所选工具集匹配的 MSVC v143 x64/x86
Spectre 缓解库，详见[构建前置条件](third_party/PACKAGING.md#canonical-build)。
默认构建不启用桌面依赖，并支持静态 musl 打包。

embedded 模式在 envd 自身的监听端口上提供 `POST /cua/mcp`，不创建 worker 子进程或额外端口。
安装 envd token 之前，CUA 拒绝访问；凭据轮换和经授权的重置会影响后续请求。
Windows 原生 DPAPI provisioning 路径支持轮换，但空元数据不能授权在线清空凭据：
`/init {}` 返回 401，并保留当前 token。详见
[原生 CUA 验收限制](docs/windows-cua-acceptance.md#native-live-reset-limitation)。
S0 仍使用 `X-Access-Token`。

Windows 服务启动、服务交接的后继进程、Session 0 和会话未知的进程均不能初始化 embedded 桌面执行。
这些场景应使用 `shared-external`：在已登录的用户会话中启动 `coorworker-cua`，
通过 `CUA_ENV_TOKEN` 提供匹配的本地传输凭据；helper 也可通过 `--token-file` 读取该凭据。
外部客户端只需 envd 凭据，envd 凭据不会发送给 helper。
[Windows 原生 CUA 验收](docs/windows-cua-acceptance.md)在一次性 Windows 11 副本中验证了
embedded 截图、输入和窗口操作，以及实际 SCM/helper 生命周期、恢复和凭据边界。

旧模式的 `COORWORKER_CUA_BIN`、`COORWORKER_CUA_EXTERNAL`、proxy token、重启预算和
观测到的恢复行为保持不变。共享模式需要显式选择迁移。运行时发现、就绪状态、指标、配置和
许可证详情见 [`docs/extensions.md`](docs/extensions.md)。

## 与 Go 守护进程的差异

在 [`docs/acceptance-notes.md`](docs/acceptance-notes.md) 所列的 Linux 差分测试语料范围内，
本守护进程与固定版本的 Go 二进制在语义上保持一致：状态行、请求头与响应头的值、帧格式、
trailer 和请求体、响应体均匹配，同一个候选版本通过了全部 VM 验收。仍存在以下差异。

### 有意保留的行为差异

| 差异 | Go envd | 本守护进程 |
| --- | --- | --- |
| 启动模式 | 启动即执行客户机路径；`--isnotfc` 仅跳过 MMDS 和日志导出器 | 需要 `--guest`、root、PID 1，以及内核命令行中的 `coorworker.envd=1`。单纯照搬 Go 启动方式，例如 `coorworker-envd --port 49983`，会拒绝启动；替换镜像时也需调整启动命令和内核参数。本地运行使用 `--development`，要求普通用户和回环地址。 |
| 端口转发器可用性 | `forwardingAvailable()` 无条件返回 nil（`internal/port/forward.go:77`） | 只有 `--guest` 守护进程会扫描端口并启动 `socat`；开发模式不访问宿主机回环服务。客户机模式的行为一致。 |
| `cpu-online` 辅助进程身份 | reexec 辅助进程写入 online 文件，不检查启动环境 | 额外要求 root、内核显式启用，以及 `/proc/1/exe` 指向辅助进程自身（`src/guest.rs`）；这是有意增加的加固差异。 |

三个额外参数 `--development`、`--guest`、`--listen` 不出现在帮助列表中。
`-h` 与参考实现一样只列出八个参数，除首行程序名外，帮助文本也保持一致。
同时支持参考实现的全部参数、单横线与双横线形式、`=value`、按前缀识别进制的 int64 解析，
以及 Go `flag` 的诊断文本和退出码。

### Windows 行为

Windows 没有 Go 参考实现，因此通过契约测试和官方 SDK 验证 Windows 后端，
不进行差分比较。已声明的差异为：

- **冻结**：挂起线程，而非冻结 cgroup v2 子树。
- **进程**：始终以守护进程自身账户运行，不支持任意用户身份模拟。
- **`X-Envd-Memory`**：返回 `partial: true`，各项数值为零，因为没有可读取的 cgroup。
- **`permissions`**：仅包含 `r`、`w`、`x` 三元组，Windows 没有 POSIX 特殊权限位。
- **SDK 的 `commands.run`**：拒绝执行，因为它硬编码 `/bin/bash -l -c`，而 Windows 客户机没有 POSIX shell。

### 比较粒度

- 比较请求头与响应头名称时忽略大小写和顺序：Go 使用规范的 MIME 大小写并按排序写出，
  本守护进程使用小写并按插入顺序写出；字段值仍参与比较。
- `Date` 和易变的运行时值（时钟、PID、CPU/内存采样、延迟）经过归一化处理；请求体和响应体逐字节比较。
- 不对已收集语料之外的输入作出一致性声明，包括任意畸形协议流量和未枚举的系统行为。

### 不在一致性声明范围内的内容

- TLS 和完整的 h2c 协议切换。Go envd 本身提供明文 HTTP/1.1（`main.go:328-335`），
  测试语料比较了两侧的拒绝路径。
- 连续超过八次的升级切换链路；更长链路会重复相同的导出与重定位路径。
- 生产 Firecracker 传输路径、宿主机 facade，以及 Go 仓库中独立的宿主机代理
  （`client-proxy` 和 orchestrator 的 `tcpfirewall`），这些属于不同程序。
- 可分发的发布产物；必须从已发布提交的干净检出中重新生成来源清单。

## 构建与测试

```sh
cargo build --locked -j4
cargo test -j4 --locked
cargo clippy -j4 --locked --all-targets -- -D warnings
cargo fmt --check
```

代码生成由 `build.rs` 在本地调用 `protoc` 和 `connectrpc-build` 完成，不下载额外内容。
工程验收记录，包括能力说明、差分证据和当前限制，见
[docs/acceptance-notes.md](docs/acceptance-notes.md)。NFS `volumeMounts` 通过实际的
`mount.nfs` 路径挂载，并通过了与固定 Go 参考实现之间的隔离 QEMU 差分验收；
提供沙箱服务的镜像尚未切换。

## 使用方式

### 开发实例

```sh
cargo run -j4 --locked -p coorworker-envd -- --development --listen 127.0.0.1:0
```

以当前普通用户在回环地址上运行，拒绝 root；可用于在本地验证 HTTP/Connect 接口。

### microVM 内的客户机实例

生产形态下，守护进程是客户机的 PID 1。启动时强制检查四个条件：
`--guest` 参数、有效用户为 root、PID 为 1，以及内核命令行包含 `coorworker.envd=1`。
先构建静态客户机二进制：

```sh
CC_x86_64_unknown_linux_musl=/usr/bin/clang \
CFLAGS_x86_64_unknown_linux_musl='-ffreestanding -nostdlibinc -DRING_CORE_NOSTDLIBINC=1' \
cargo build -j4 --locked -p coorworker-envd \
  --target x86_64-unknown-linux-musl --release --bin coorworker-envd
```

最小 initramfs 客户机的结构如下，仅以 busybox 和 musl 加载器作为用户空间，
包含 root 账户和一个网卡驱动：

```
/envd                              # the static binary built above
/bin/busybox + /bin/{sh,mount,...} # busybox applet symlinks
/lib/ld-musl-x86_64.so.1
/lib/e1000.ko                      # unpacked from the host's e1000.ko.zst
/etc/passwd, /etc/group            # root account entries
/init:
  #!/bin/sh
  export PATH=/bin
  mount -t proc proc /proc
  mount -t sysfs sysfs /sys
  mount -t devtmpfs devtmpfs /dev
  mkdir -p /dev/pts && mount -t devpts devpts /dev/pts
  insmod /lib/e1000.ko || poweroff -f
  ip link set lo up
  ip link set eth0 up
  ip addr add 10.0.2.15/24 dev eth0
  exec /envd --guest --isnotfc --listen 0.0.0.0:49983
```

使用任意 x86_64 bzImage 启动。没有 PVH note 的 ELF `vmlinux`，例如 Firecracker
快速入门使用的内核，无法由 `qemu -kernel` 加载：

```sh
qemu-system-x86_64 -no-user-config -nodefaults -display none -serial stdio \
  -no-reboot -accel kvm -m 384 -smp 2 \
  -kernel /boot/vmlinuz-linux -initrd initramfs.cpio.gz \
  -append "console=ttyS0 rdinit=/init panic=-1 coorworker.envd=1" \
  -netdev user,id=n,restrict=on,hostfwd=tcp:127.0.0.1:49983-:49983 \
  -device e1000,netdev=n
```

- `--isnotfc` 跳过 MMDS 轮询和 HTTP 日志导出器，因此实例以全新状态启动，
  并接受首次 `/init` 提供的访问 token。
- 上述网络配置没有默认路由，客户机无法访问元数据、云端或公共服务，只保留一个宿主机端口转发。
- 转发后的 `GET /health` 返回 `204` 表示就绪。
- 前置条件：`qemu-system-x86_64`、用于解包模块的 `zstd`，以及普通用户对 `/dev/kvm` 的访问权限。
  没有 KVM 时可使用 `-accel tcg`，但速度较慢。

同时启动 Go 参考实现与本二进制的双 VM 差分测试工具位于内部工作区的一致性测试套件中，
不包含在本仓库。生产 Firecracker 启动方式，包括持久化 ext4 rootfs 和 MMDS 接线，
属于独立的部署工作。
