//! Draws the Windows taskbar overlay badge: a coloured disc with the unread count.
//! Done in code so no per-count assets have to ship.

#![cfg_attr(not(windows), allow(dead_code))]

pub const SIZE: u32 = 32;

const RED: [u8; 3] = [0xE5, 0x48, 0x4D];
const AMBER: [u8; 3] = [0xF2, 0xA3, 0x3A];

// 3x5 pixel font: digits 0-9 and '+'. One row per entry, bit 2 = left column.
const GLYPHS: [[u8; 5]; 11] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
    [0b000, 0b010, 0b111, 0b010, 0b000],
];

fn put(buf: &mut [u8], x: i32, y: i32, rgba: [u8; 4]) {
    if x < 0 || y < 0 || x >= SIZE as i32 || y >= SIZE as i32 {
        return;
    }
    let i = ((y as u32 * SIZE + x as u32) * 4) as usize;
    buf[i..i + 4].copy_from_slice(&rgba);
}

fn glyph(buf: &mut [u8], index: usize, left: i32, top: i32, scale: i32) {
    for (row, bits) in GLYPHS[index].iter().enumerate() {
        for col in 0..3 {
            if bits & (0b100 >> col) != 0 {
                for dy in 0..scale {
                    for dx in 0..scale {
                        put(
                            buf,
                            left + col * scale + dx,
                            top + row as i32 * scale + dy,
                            [255, 255, 255, 255],
                        );
                    }
                }
            }
        }
    }
}

/// RGBA pixels (SIZE x SIZE). `count` 1-9 shows the digit, more shows "9+".
pub fn overlay_rgba(count: u32, mention: bool) -> Vec<u8> {
    let mut buf = vec![0u8; (SIZE * SIZE * 4) as usize];
    let colour = if mention { RED } else { AMBER };
    let centre = SIZE as f32 / 2.0 - 0.5;
    let radius = SIZE as f32 / 2.0 - 0.5;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 - centre).powi(2) + (y as f32 - centre).powi(2)).sqrt();
            let alpha = (radius + 1.0 - d).clamp(0.0, 1.0);
            if alpha > 0.0 {
                put(
                    &mut buf,
                    x as i32,
                    y as i32,
                    [colour[0], colour[1], colour[2], (alpha * 255.0) as u8],
                );
            }
        }
    }
    if count <= 9 {
        // One 12 x 20 glyph, centred.
        glyph(&mut buf, count as usize, 10, 6, 4);
    } else {
        // "9+": two 9 x 15 glyphs with a 3 px gap.
        glyph(&mut buf, 9, 4, 8, 3);
        glyph(&mut buf, 10, 19, 8, 3);
    }
    buf
}
