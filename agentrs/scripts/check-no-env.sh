#!/usr/bin/env bash
# 边界判据（架构 §1.1）：
#   "如果加入 X 之后 agentrs 的单测需要网络、文件系统、真实时钟或 OS 进程，那么 X 放错地方了。"
# clippy.toml 已从类型层面禁掉 std::fs::File / std::process::Command / SystemTime::now 等；
# 本脚本兜住 clippy 覆盖不到的写法。
set -euo pipefail
cd "$(dirname "$0")/.."

# 两处受约束例外：
#   - agentrs-dev-adapter：显式的宿主参考实现，按设计触碰环境；
#   - agentrs-provider：LLM seam，其存在意义就是与模型 API 通话。
#     例外的只是"网络"一项——fs/process 对它依然禁止（见其 crate 文档的四条约束）。
# 注意粒度：禁的是**执行权与磁盘访问**，不是"名字里带 process"。
#   std::process::Command  → 派生进程 = 执行权，禁止
#   std::process::exit     → 进程退出，不构成执行权，允许（CLI 需要退出码）
patterns='std::fs::|std::process::Command|std::process::abort|tokio::fs::|tokio::process::'
net_patterns='std::net::|reqwest::'

# agentrs-dev-tui/src/host_io.rs 是第三处豁免，粒度是**单个文件**而不是整个 crate：
# 交互宿主确实需要时钟、配置文件与 $EDITOR，但把它们收进一个文件之后，
# crate 里其余二十来个模块仍然受本门禁保护，越界会立刻被照出来。
if hits=$(grep -rnE "$patterns" crates --include='*.rs' \
        | grep -v '^crates/agentrs-dev-adapter/' \
        | grep -v '^crates/agentrs-dev-tui/src/host_io.rs' \
        | grep -v '^\s*//'); then
    echo "内核中出现文件/进程依赖（应经 port 注入）："
    echo "$hits"
    exit 1
fi

if hits=$(grep -rnE "$net_patterns" crates --include='*.rs' \
        | grep -v '^crates/agentrs-dev-adapter/' \
        | grep -v '^crates/agentrs-provider/' \
        | grep -v '^\s*//'); then
    echo "内核中出现网络依赖（provider 之外应经 port 注入）："
    echo "$hits"
    exit 1
fi
# 凭据与配置一律注入，内核**库** crate 不得读环境变量。
#
# 两处豁免，都是 OS 与内核之间的翻译层：
#   agentrs-cli         —— 它的职责就是把命令行参数与环境翻译为 RunSpec（架构 §3.1）；
#   agentrs-dev-adapter —— 显式的宿主参考实现。
#   agentrs-dev-tui     —— 非生产交互宿主，把终端输入与 provider 配置翻译为 RunSpec。
# 内核库（contracts/types/runtime/provider/context/tools/...）依然禁止。
if hits=$(grep -rnE 'std::env::var' crates --include='*.rs' \
        | grep -v '/tests/' \
        | grep -v '^crates/agentrs-cli/' \
        | grep -v '^crates/agentrs-dev-adapter/' \
        | grep -v '^crates/agentrs-dev-tui/' \
        | grep -v '^\s*//'); then
    echo "内核中读取环境变量（凭据与配置必须由 RunSpec/构造参数注入）："
    echo "$hits"
    exit 1
fi

echo "无环境依赖检查通过"
