hzip {{VERSION}} — macOS 安装说明
================================

1) 安装：把左侧的 hzip.app 拖到右侧的「应用程序」(Applications) 文件夹。
2) 首次启动：从「应用程序」或启动台打开；hzip 未签名/未公证的构建会被 Gatekeeper
   拦下，此时右键图标选「打开」，或在「系统设置 → 隐私与安全性」里放行。

命令行工具（可选，供脚本与终端使用）：

    ln -sf /Applications/hzip.app/Contents/MacOS/hzip /usr/local/bin/hzip
    hzip --version

卸载：删除 /Applications/hzip.app（配置与日志留在 ~/Library/Application Support/hzip，
      需要时手动删除）。
