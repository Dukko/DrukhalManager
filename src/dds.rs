//! Decoding the DirectDraw Surface textures the game uses, for previews:
//! DXT1, DXT3 and DXT5 (BC1–3) and uncompressed RGB(A), with or without a
//! DX10 header. Only the first mipmap is read.

/// A decoded image: `width * height` pixels of straight RGBA.
#[derive(Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Images bigger than this on a side aren't decoded.
const MAX_SIDE: u32 = 16384;

const DDPF_ALPHAPIXELS: u32 = 0x1;
const DDPF_FOURCC: u32 = 0x4;
const DDPF_RGB: u32 = 0x40;
const DDPF_LUMINANCE: u32 = 0x20000;

enum Format {
    Bc1,
    Bc2,
    Bc3,
    /// Bits per pixel and the R, G, B and A masks (A may be 0).
    Masked(u32, [u32; 4]),
}

pub fn decode(bytes: &[u8]) -> Result<Image, &'static str> {
    if bytes.len() < 128 || &bytes[..4] != b"DDS " {
        return Err("Not a DDS file");
    }
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
    let (height, width) = (u32_at(12), u32_at(16));
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err("Unsupported image size");
    }
    let (flags, four_cc) = (u32_at(80), &bytes[84..88]);
    let (format, data) = if flags & DDPF_FOURCC != 0 {
        match four_cc {
            b"DXT1" => (Format::Bc1, 128),
            b"DXT2" | b"DXT3" => (Format::Bc2, 128),
            b"DXT4" | b"DXT5" => (Format::Bc3, 128),
            b"DX10" if bytes.len() >= 148 => (
                match u32_at(128) {
                    70..=72 => Format::Bc1,
                    73..=75 => Format::Bc2,
                    76..=78 => Format::Bc3,
                    27..=29 => Format::Masked(32, [0xff, 0xff00, 0xff_0000, 0xff00_0000]),
                    87 | 90 | 91 => Format::Masked(32, [0xff_0000, 0xff00, 0xff, 0xff00_0000]),
                    88 | 92 | 93 => Format::Masked(32, [0xff_0000, 0xff00, 0xff, 0]),
                    _ => return Err("This kind of DDS can't be previewed"),
                },
                148,
            ),
            _ => return Err("This kind of DDS can't be previewed"),
        }
    } else if flags & (DDPF_RGB | DDPF_LUMINANCE) != 0 {
        let alpha = if flags & DDPF_ALPHAPIXELS != 0 { u32_at(104) } else { 0 };
        let bits = u32_at(88);
        if !matches!(bits, 8 | 16 | 24 | 32) {
            return Err("This kind of DDS can't be previewed");
        }
        let (r, g, b) = (u32_at(92), u32_at(96), u32_at(100));
        // Luminance puts the gray in the red mask.
        let masks = if flags & DDPF_LUMINANCE != 0 { [r, r, r, alpha] } else { [r, g, b, alpha] };
        (Format::Masked(bits, masks), 128)
    } else {
        return Err("This kind of DDS can't be previewed");
    };
    let data = &bytes[data..];
    let (w, h) = (width as usize, height as usize);
    let mut rgba = vec![0u8; w * h * 4];
    match format {
        Format::Masked(bits, masks) => {
            let step = bits as usize / 8;
            if data.len() < w * h * step {
                return Err("The file is cut short");
            }
            for (pixel, out) in data.chunks_exact(step).zip(rgba.as_chunks_mut::<4>().0) {
                let mut word = [0u8; 4];
                word[..step].copy_from_slice(pixel);
                let value = u32::from_le_bytes(word);
                for (channel, mask) in out.iter_mut().zip(masks) {
                    *channel = if mask == 0 { 255 } else { scale(value & mask, mask) };
                }
            }
        }
        Format::Bc1 | Format::Bc2 | Format::Bc3 => {
            let block_size = if matches!(format, Format::Bc1) { 8 } else { 16 };
            let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
            if data.len() < bw * bh * block_size {
                return Err("The file is cut short");
            }
            for (i, block) in data.chunks_exact(block_size).take(bw * bh).enumerate() {
                let pixels = match format {
                    Format::Bc1 => color_block(block, true),
                    Format::Bc2 => {
                        let mut p = color_block(&block[8..], false);
                        for (j, px) in p.iter_mut().enumerate() {
                            let nibble = (block[j / 2] >> (4 * (j % 2))) & 0xf;
                            px[3] = nibble * 17;
                        }
                        p
                    }
                    _ => {
                        let mut p = color_block(&block[8..], false);
                        let alphas = alpha_block(&block[..8]);
                        for (px, a) in p.iter_mut().zip(alphas) {
                            px[3] = a;
                        }
                        p
                    }
                };
                let (bx, by) = (i % bw * 4, i / bw * 4);
                for (j, px) in pixels.iter().enumerate() {
                    let (x, y) = (bx + j % 4, by + j / 4);
                    if x < w && y < h {
                        rgba[(y * w + x) * 4..][..4].copy_from_slice(px);
                    }
                }
            }
        }
    }
    Ok(Image { width, height, rgba })
}

/// A masked channel value scaled to 0–255.
fn scale(value: u32, mask: u32) -> u8 {
    let shifted = value >> mask.trailing_zeros();
    let max = mask >> mask.trailing_zeros();
    ((shifted * 255 + max / 2) / max) as u8
}

fn rgb565(c: u16) -> [u8; 4] {
    let (r, g, b) = ((c >> 11) & 0x1f, (c >> 5) & 0x3f, c & 0x1f);
    [scale(r.into(), 0x1f), scale(g.into(), 0x3f), scale(b.into(), 0x1f), 255]
}

/// The 16 pixels of a BC1 color block, row by row. `bc1` allows its
/// three-color mode with transparent black; BC2 and BC3 don't use it.
fn color_block(block: &[u8], bc1: bool) -> [[u8; 4]; 16] {
    let (c0, c1) = (u16::from_le_bytes([block[0], block[1]]), u16::from_le_bytes([block[2], block[3]]));
    let (a, b) = (rgb565(c0), rgb565(c1));
    let mix = |wa: u16, wb: u16, d: u16| -> [u8; 4] {
        let ch = |i: usize| ((a[i] as u16 * wa + b[i] as u16 * wb) / d) as u8;
        [ch(0), ch(1), ch(2), 255]
    };
    let palette = if c0 > c1 || !bc1 { [a, b, mix(2, 1, 3), mix(1, 2, 3)] } else { [a, b, mix(1, 1, 2), [0, 0, 0, 0]] };
    let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    std::array::from_fn(|j| palette[(indices >> (2 * j) & 3) as usize])
}

/// The 16 alphas of a BC3 alpha block.
fn alpha_block(block: &[u8]) -> [u8; 16] {
    let (a0, a1) = (block[0] as u16, block[1] as u16);
    let mut palette = [a0, a1, 0, 0, 0, 0, 0, 0];
    if a0 > a1 {
        for i in 1..7 {
            palette[i + 1] = ((7 - i as u16) * a0 + i as u16 * a1) / 7;
        }
    } else {
        for i in 1..5 {
            palette[i + 1] = ((5 - i as u16) * a0 + i as u16 * a1) / 5;
        }
        palette[7] = 255;
    }
    let mut bits = 0u64;
    for (i, &byte) in block[2..8].iter().enumerate() {
        bits |= (byte as u64) << (8 * i);
    }
    std::array::from_fn(|j| palette[(bits >> (3 * j) & 7) as usize] as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(width: u32, height: u32, flags: u32, four_cc: &[u8; 4], bits: u32, masks: [u32; 4]) -> Vec<u8> {
        let mut h = vec![0u8; 128];
        h[..4].copy_from_slice(b"DDS ");
        h[4..8].copy_from_slice(&124u32.to_le_bytes());
        h[12..16].copy_from_slice(&height.to_le_bytes());
        h[16..20].copy_from_slice(&width.to_le_bytes());
        h[76..80].copy_from_slice(&32u32.to_le_bytes());
        h[80..84].copy_from_slice(&flags.to_le_bytes());
        h[84..88].copy_from_slice(four_cc);
        h[88..92].copy_from_slice(&bits.to_le_bytes());
        for (i, m) in masks.iter().enumerate() {
            h[92 + 4 * i..96 + 4 * i].copy_from_slice(&m.to_le_bytes());
        }
        h
    }

    #[test]
    fn decodes_uncompressed_bgra() {
        let mut dds = header(2, 1, DDPF_RGB | DDPF_ALPHAPIXELS, &[0; 4], 32, [0xff_0000, 0xff00, 0xff, 0xff00_0000]);
        dds.extend([0x30, 0x20, 0x10, 0x80, 0, 0, 0xff, 0xff]);
        assert_eq!(decode(&dds).unwrap(), Image { width: 2, height: 1, rgba: vec![0x10, 0x20, 0x30, 0x80, 0xff, 0, 0, 0xff] });
    }

    #[test]
    fn decodes_dxt1_and_dxt5() {
        // Red and blue endpoints; every pixel uses color 0, then color 1.
        let block = [0x00, 0xf8, 0x1f, 0x00, 0x00, 0x00, 0x00, 0x00];
        let mut dds = header(4, 4, DDPF_FOURCC, b"DXT1", 0, [0; 4]);
        dds.extend(block);
        let image = decode(&dds).unwrap();
        assert_eq!((image.width, image.height), (4, 4));
        assert_eq!(&image.rgba[..4], [255, 0, 0, 255]);

        let mut dds = header(3, 2, DDPF_FOURCC, b"DXT5", 0, [0; 4]);
        // Alpha 200 and 100, all pixels index 1; colors all index 1 (blue).
        dds.extend([200, 100, 0x49, 0x92, 0x24, 0x49, 0x92, 0x24]);
        dds.extend([0x00, 0xf8, 0x1f, 0x00, 0x55, 0x55, 0x55, 0x55]);
        let image = decode(&dds).unwrap();
        assert_eq!((image.width, image.height, image.rgba.len()), (3, 2, 24));
        assert_eq!(&image.rgba[..4], [0, 0, 255, 100]);
    }

    #[test]
    fn refuses_what_it_cant_read() {
        assert!(decode(b"PNG").is_err());
        let dds = header(4, 4, DDPF_FOURCC, b"ATI2", 0, [0; 4]);
        assert!(decode(&dds).is_err());
        let dds = header(4, 4, DDPF_FOURCC, b"DXT1", 0, [0; 4]);
        assert_eq!(decode(&dds), Err("The file is cut short"));
    }
}

#[cfg(test)]
mod real_files {
    /// `DDS_LIST=<file of paths> cargo test real_dds -- --ignored`
    #[test]
    #[ignore]
    fn real_dds() {
        let list = std::fs::read_to_string(std::env::var("DDS_LIST").unwrap()).unwrap();
        let mut failed = Vec::new();
        for path in list.lines() {
            if let Err(e) = super::decode(&std::fs::read(path).unwrap()) {
                failed.push(format!("{path}: {e}"));
            }
        }
        assert!(failed.is_empty(), "{}", failed.join("\n"));
    }
}

