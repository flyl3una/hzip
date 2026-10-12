use super::*;

#[test]
fn bit_flip_deterministic() {
    let mut a = vec![0u8; 64];
    let mut b = vec![0u8; 64];
    bit_flip(&mut a, 20, 42);
    bit_flip(&mut b, 20, 42);
    assert_eq!(a, b, "同种子两次翻转字节一致");
    let flips = a.iter().filter(|&&x| x != 0).count();
    assert!(flips > 0, "应发生翻转");
}

#[test]
fn panic_counter_counts() {
    let pc = PanicCounter::new();
    let ok: Result<(), HzipError> = pc.guard(|| Ok(()));
    assert!(ok.is_ok());
    assert_eq!(pc.count(), 0);
    let _ = pc.guard(|| -> Result<(), HzipError> { panic!("boom") });
    assert_eq!(pc.count(), 1, "panic 应计数");
}
