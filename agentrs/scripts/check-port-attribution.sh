#!/usr/bin/env bash
# 移植合规门禁（架构 §3.2，Apache-2.0 §4b）。
# 任何从 aionrs 移植的文件必须在头部标注来源、commit 与修改摘要。
# 判定：文件含 "Ported from" 即视为移植文件，此时必须同时含 Source/Copied/Changes 三项，
# 且必须登记在 THIRD-PARTY-NOTICES.md 中。
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
notices="THIRD-PARTY-NOTICES.md"

while IFS= read -r f; do
    head -12 "$f" | grep -q "Ported from" || continue
    for field in "Source:" "Copied:" "Changes:"; do
        if ! head -12 "$f" | grep -q "$field"; then
            echo "缺少 $field 标注: $f"; fail=1
        fi
    done
    if ! grep -qF "$f" "$notices" 2>/dev/null; then
        echo "未登记进 $notices: $f"; fail=1
    fi
done < <(find crates -name '*.rs')

if [ "$fail" -ne 0 ]; then
    echo
    echo "移植清单必须在移植发生时逐条维护，不允许发布前突击补写。"
    exit 1
fi
echo "移植合规检查通过"
