use super::*;

#[test]
fn spawn_detached_smoke() {
    let (cmd, args) = if cfg!(windows) {
        (
            OsStr::new("cmd.exe"),
            vec![OsStr::new("/c"), OsStr::new("exit 0")],
        )
    } else {
        (
            OsStr::new("sh"),
            vec![OsStr::new("-c"), OsStr::new("exit 0")],
        )
    };
    let args: Vec<&OsStr> = args.iter().map(|a| a as &OsStr).collect();
    // 派生成功即可（不验证子进程结果——已脱离）
    spawn_detached(cmd, &args).unwrap();
}
