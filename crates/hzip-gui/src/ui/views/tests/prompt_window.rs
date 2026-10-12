use super::*;

fn conflict_prompt() -> UiDialogPrompt {
    UiDialogPrompt::Conflict {
        job_id: 7,
        path: "C:/out/a.txt".into(),
        existing_is_dir: false,
        existing_size: 12,
    }
}

fn password_prompt(attempts: u8) -> UiDialogPrompt {
    UiDialogPrompt::Password {
        job_id: 7,
        target: None,
        attempts,
    }
}

/// 冲突摘要与密码说明行：由提示内容派生（i18n 在渲染侧，不落数据面）。
#[test]
fn prompt_body_follows_kind() {
    let mut w = PromptWindow {
        kind: PromptKind::Password {
            error: String::new(),
        },
        apply_all: true,
        password_input: None,
        on_event: noop_on_event(),
    };
    w.set_prompt(&conflict_prompt());
    match &w.kind {
        PromptKind::Conflict {
            path,
            existing_brief,
            ..
        } => {
            assert_eq!(path, "C:/out/a.txt");
            assert!(!existing_brief.is_empty(), "现有文件摘要必须有文案");
        }
        _ => panic!("应切到冲突态"),
    }
    // 换提示：数据面跟着换
    w.set_prompt(&password_prompt(2));
    match &w.kind {
        PromptKind::Password { error } => {
            assert!(error.contains('2'), "重试说明应含次数：{error}");
        }
        _ => panic!("应切到密码态"),
    }
}
