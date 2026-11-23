const TABLE: [u32; 256] = build_table();

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 { (value >> 1) ^ 0xEDB8_8320 } else { value >> 1 };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut checksum = 0xFFFF_FFFFu32;
    for byte in bytes {
        checksum = TABLE[((checksum ^ u32::from(*byte)) & 0xFF) as usize] ^ (checksum >> 8);
    }
    checksum ^ 0xFFFF_FFFF
}
