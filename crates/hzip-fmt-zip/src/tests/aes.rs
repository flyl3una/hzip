use super::*;
use hzip_format::PasswordRef;

fn chain(version: u16, vendor: &[u8; 2], strength: u8, method: u16) -> Vec<u8> {
    let mut body = Vec::with_capacity(7);
    body.extend_from_slice(&version.to_le_bytes());
    body.extend_from_slice(vendor);
    body.push(strength);
    body.extend_from_slice(&method.to_le_bytes());
    let mut out = Vec::with_capacity(11);
    out.extend_from_slice(&EXTRA_ID_9901.to_le_bytes());
    out.extend_from_slice(&(body.len() as u16).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

#[test]
fn aes_extra_field_parse() {
    // 布局逐字段：version/vendor/strength/real_method
    let ef = chain(0x0002, b"AE", 3, 8);
    let info = parse_extra_field(&ef).unwrap();
    assert_eq!(info.version, AesVersion::Aes2);
    assert_eq!(info.strength, AesStrength::Aes256);
    assert_eq!(info.real_method, 8);

    // 7 字节数据区直接形态
    let info2 = parse_extra_field(&ef[4..]).unwrap();
    assert_eq!(info2.strength, AesStrength::Aes256);
    assert_eq!(info2.real_method, 8);

    // AE-1 + strength 1 + real_method 12（bzip2）
    let ae1 = chain(0x0001, b"AE", 1, 12);
    let i3 = parse_extra_field(&ae1).unwrap();
    assert_eq!(i3.version, AesVersion::Aes1);
    assert_eq!(i3.strength, AesStrength::Aes128);
    assert_eq!(i3.real_method, 12);

    // vendor 非 AE → 3103
    assert_eq!(
        parse_extra_field(&chain(0x0002, b"XX", 3, 8))
            .unwrap_err()
            .code(),
        3103
    );
    // version 未知 → 3103
    assert_eq!(
        parse_extra_field(&chain(0x0003, b"AE", 3, 8))
            .unwrap_err()
            .code(),
        3103
    );
    // strength 非法 → 3103
    assert_eq!(
        parse_extra_field(&chain(0x0001, b"AE", 9, 8))
            .unwrap_err()
            .code(),
        3103
    );
    // 数据区长度 ≠7 → 3103
    assert_eq!(parse_extra_field(b"").unwrap_err().code(), 3103);
    let mut short = chain(0x0002, b"AE", 3, 8);
    short.pop();
    assert_eq!(parse_extra_field(&short).unwrap_err().code(), 3103);
    // 链中无 0x9901（他字段）→ 3103
    let mut other = Vec::new();
    other.extend_from_slice(&0x5455u16.to_le_bytes());
    other.extend_from_slice(&0u16.to_le_bytes());
    assert_eq!(parse_extra_field(&other).unwrap_err().code(), 3103);
}

#[test]
fn aes_key_derivation_kv() {
    // 三档：dkLen 26/38/50、salt 8/12/16、拆包偏移（K ‖ V ‖ verifier）
    for (s, key_len, salt_len) in [
        (AesStrength::Aes128, 16usize, 8usize),
        (AesStrength::Aes192, 24usize, 12usize),
        (AesStrength::Aes256, 32usize, 16usize),
    ] {
        assert_eq!(s.key_len(), key_len);
        assert_eq!(s.salt_len(), salt_len);
        let salt = vec![7u8; salt_len];
        let k1 = derive_keys("同输入-密码", &salt, s);
        assert_eq!(k1.enc_key().len(), key_len);
        assert_eq!(k1.mac_key().len(), key_len / 2);
        // 确定性：同输入同输出
        let k2 = derive_keys("同输入-密码", &salt, s);
        assert_eq!(k1.enc_key(), k2.enc_key());
        assert_eq!(k1.mac_key(), k2.mac_key());
        assert_eq!(k1.verifier, k2.verifier);
        // 不同密码 → 全段不同
        let k3 = derive_keys("不同输入", &salt, s);
        assert_ne!(k1.verifier, k3.verifier);
        assert_ne!(k1.enc_key(), k3.enc_key());
        // 不同 salt → verifier 不同
        let k4 = derive_keys("同输入-密码", &vec![9u8; salt_len], s);
        assert_ne!(k1.verifier, k4.verifier);
    }
}

#[test]
fn aes_strength_mapping() {
    for (b, s) in [
        (1u8, AesStrength::Aes128),
        (2, AesStrength::Aes192),
        (3, AesStrength::Aes256),
    ] {
        assert_eq!(AesStrength::from_byte(b), Some(s));
        assert_eq!(s.strength_byte(), b);
    }
    assert_eq!(AesStrength::from_byte(0), None);
    assert_eq!(AesStrength::from_byte(4), None);
    assert_eq!(AesStrength::from_key_len(16), Some(AesStrength::Aes128));
    assert_eq!(AesStrength::from_key_len(24), Some(AesStrength::Aes192));
    assert_eq!(AesStrength::from_key_len(32), Some(AesStrength::Aes256));
    assert_eq!(AesStrength::from_key_len(8), None);
}

#[test]
fn reject_nul_password() {
    let z = Zeroizing::new("ok-pw".to_string());
    assert!(reject_nul(z.as_str()).is_ok());
    // 空密码合法（需要密码语义由调用方处理）；NUL 明确拒绝 3101
    assert!(reject_nul("").is_ok());
    let bad = Zeroizing::new("pa\0ss".to_string());
    let err = reject_nul(bad.as_str()).unwrap_err();
    assert_eq!(err.code(), 3101);
    // 不 panic（catch_unwind 计数 0 由调用方集成测试承载——本函数纯返回）
    let _ = PasswordRef::<'_>::none();
}
