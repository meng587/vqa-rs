//! ISO/IEC 13818-1 的 CRC-32/MPEG-2（PSI/SI section 校验）。
//!
//! poly=0x04C11DB7, init=0xFFFFFFFF, 非反射、无输出反转。
//! 校验向量 '123456789' → 0x0376E6E7。

pub fn crc32_mpeg(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_vector() {
        // 标准校验向量
        assert_eq!(crc32_mpeg(b"123456789"), 0x0376_E6E7);
    }

    #[test]
    fn empty_is_init() {
        assert_eq!(crc32_mpeg(&[]), 0xFFFF_FFFF);
    }
}
