#!/usr/bin/env bash
# T-G50 §3.1：lucide 图标资产入库脚本（一次性工具；构建期零网络依赖）。
# 来源：https://icon-sets.iconify.design/lucide/（经 api.iconify.design）。
# 用法：build/icons/fetch-lucide-icons.sh   （在 crates/hzip-gui 下执行；结果提交 git）
set -euo pipefail
cd "$(dirname "$0")/../../crates/hzip-gui/assets/icons"

# 白名单：hzip 已用 + R2 可预见（T-G51~G54 需新图标时 append-only 追加）。
ICONS=(
  folder folder-open folder-closed file file-text file-output archive
  package-plus package-minus eye rotate-cw shield-check settings
  arrow-up chevron-right chevron-down search x lock triangle-alert
  hard-drive list table columns languages moon sun monitor
  mouse-pointer-click external-link link check minus refresh-cw inbox plus replace
  info circle-check loader-circle
  # T-BAR（2026-09-29）：工具栏「查看」按钮
  layout-list
  # 标准消息弹窗（2026-09-29）：错误级别图标
  octagon-alert
)
for name in "${ICONS[@]}"; do
  if [ -f "$name.svg" ]; then
    echo "skip $name (exists)"
    continue
  fi
  echo "fetch $name"
  curl -fsSL "https://api.iconify.design/lucide/$name.svg" -o "$name.svg"
done
echo "done: $(ls *.svg | wc -l) icons"
