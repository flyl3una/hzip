use super::*;

#[test]
fn zipx_method_routing() {
    // 基础方法：Native
    assert_eq!(route(0), MethodRoute::Native);
    assert_eq!(route(8), MethodRoute::Native);
    assert_eq!(route(9), MethodRoute::Native);
    // AES 包封
    assert_eq!(route(99), MethodRoute::Aes);
    // PPMd → Unsupported（5001 语义）
    assert!(matches!(route(98), MethodRoute::Unsupported(_)));

    #[cfg(feature = "zipx")]
    {
        assert_eq!(route(12), MethodRoute::Zipx("bzip2".into()));
        assert_eq!(route(14), MethodRoute::Zipx("lzma".into()));
        assert_eq!(route(93), MethodRoute::Zipx("zstd".into()));
    }
    #[cfg(not(feature = "zipx"))]
    {
        // feature 关闭：12/14/93 明确 Unsupported（上层 → 2003/5001），不静默
        assert!(matches!(route(12), MethodRoute::Unsupported(_)));
        assert!(matches!(route(14), MethodRoute::Unsupported(_)));
        assert!(matches!(route(93), MethodRoute::Unsupported(_)));
        assert!(zipx_disabled());
    }
    // 未知方法族明确 Unsupported
    assert!(matches!(route(95), MethodRoute::Unsupported(_)));
    assert!(matches!(route(1), MethodRoute::Unsupported(_)));
}
