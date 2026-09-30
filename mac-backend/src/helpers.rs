pub fn average_u32<I>(values: I) -> u32
where
    I: IntoIterator<Item = u32>,
{
    let mut sum = 0u64;
    let mut count = 0u64;

    for value in values {
        sum += value as u64;
        count += 1;
    }

    if count == 0 {
        0
    } else {
        (sum / count) as u32
    }
}

pub fn copy_to_4_bytes(value: &[u8]) -> [u8; 4] {
    let mut out = [0u8; 4];
    let len = value.len().min(out.len());
    out[..len].copy_from_slice(&value[..len]);
    out
}
