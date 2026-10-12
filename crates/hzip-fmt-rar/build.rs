// hzip-fmt-rar build script。
//
// unrar_sys（C++ unrar 静态绑定）在 Windows 的 build.rs 只声明了 powrprof /
// shell32；unrar C++ 源码（GetRarDataPath / IsUserAdmin / GetRnd / ACL 提取）
// 还引用 advapi32.lib（RegCloseKey、OpenProcessToken、CryptAcquireContextW、
// SetFileSecurityW…）。MinGW 默认链入 advapi32 故上游 CI 未暴露；MSVC 不会——
// 这里由本 crate 补声明（链接指令经 rlib 元数据向最终目标传递，下游 hzip-core
// 等依赖本 crate 的二进制同样受益）。属环境适配，不改任何外部 crate。

fn main() {
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rustc-link-lib=advapi32");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
