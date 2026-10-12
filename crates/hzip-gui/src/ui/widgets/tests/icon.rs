use super::*;
use crate::ui::assets::HzipAssets;

// 「枚举 → 文件名 → 资产」链路钉死：`path()` 拼错文件名时资产源静默 miss，
// 界面表现为「图标不渲染」（不报错、不 panic）。逐变体断言自有资产命中。
#[test]
fn every_app_icon_path_resolves_to_an_embedded_asset() {
    let all = [
        AppIcon::Folder,
        AppIcon::FolderOpen,
        AppIcon::FolderClosed,
        AppIcon::File,
        AppIcon::FileText,
        AppIcon::Extract,
        AppIcon::Archive,
        AppIcon::Compress,
        AppIcon::PackageMinus,
        AppIcon::Eye,
        AppIcon::EyeOff,
        AppIcon::CircleHelp,
        AppIcon::Refresh,
        AppIcon::ShieldCheck,
        AppIcon::Settings,
        AppIcon::ArrowUp,
        AppIcon::ArrowDown,
        AppIcon::ChevronRight,
        AppIcon::ChevronDown,
        AppIcon::Search,
        AppIcon::X,
        AppIcon::Lock,
        AppIcon::Warn,
        AppIcon::OctagonAlert,
        AppIcon::HardDrive,
        AppIcon::List,
        AppIcon::Table,
        AppIcon::Columns,
        AppIcon::LayoutList,
        AppIcon::Languages,
        AppIcon::Moon,
        AppIcon::Sun,
        AppIcon::Monitor,
        AppIcon::MousePointerClick,
        AppIcon::ExternalLink,
        AppIcon::Link,
        AppIcon::Check,
        AppIcon::Minus,
        AppIcon::RotateCw,
        AppIcon::Inbox,
        AppIcon::Plus,
        AppIcon::Replace,
        AppIcon::Info,
        AppIcon::CircleCheck,
        AppIcon::LoaderCircle,
        AppIcon::FolderZip,
    ];
    for icon in all {
        let path = icon.path();
        assert!(
            HzipAssets::get(path.as_ref()).is_some(),
            "资产缺失: {path}（文件名拼错或未入库，界面会渲染成没有图标）"
        );
    }
}
