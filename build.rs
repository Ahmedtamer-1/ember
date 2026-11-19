//! Build script: embeds the Ember icon into the Windows executable.
//!
//! On Windows/MSVC targets it rasterizes the logo with the same procedural code the app uses
//! for its window icon (`src/brand_raster.rs`), writes a multi-size `.ico` (PNG-compressed
//! entries), compiles a one-line `.rc` with `rc.exe` and links the resulting `.res` into the
//! binaries. Other targets do nothing. A missing `rc.exe` only produces a warning.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

mod raster {
    include!("src/brand_raster.rs");
}

/// Sizes baked into `ember.ico` (Explorer, shortcuts, Alt+Tab, high-DPI taskbar).
const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/brand_raster.rs");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }
    if let Err(err) = embed_windows_icon() {
        println!("cargo:warning=Ember exe icon not embedded: {err}");
    }
}

fn embed_windows_icon() -> Result<(), String> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").map_err(|e| e.to_string())?);
    let ico_path = out_dir.join("ember.ico");
    let rc_path = out_dir.join("ember.rc");
    let res_path = out_dir.join("ember.res");

    fs::write(&ico_path, build_ico()).map_err(|e| format!("writing {ico_path:?}: {e}"))?;
    fs::write(&rc_path, "1 ICON \"ember.ico\"\r\n")
        .map_err(|e| format!("writing {rc_path:?}: {e}"))?;

    let rc = find_rc_exe().ok_or("rc.exe not found (install the Windows SDK)")?;
    let status = Command::new(&rc)
        .current_dir(&out_dir)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .status()
        .map_err(|e| format!("running {rc:?}: {e}"))?;
    if !status.success() {
        return Err(format!("{rc:?} failed with {status}"));
    }
    println!("cargo:rustc-link-arg-bins={}", res_path.display());
    Ok(())
}

fn find_rc_exe() -> Option<PathBuf> {
    let probe = |p: &Path| {
        Command::new(p)
            .arg("/?")
            .output()
            .map(|_| p.to_path_buf())
            .ok()
    };
    if let Some(found) = probe(Path::new("rc.exe")) {
        return Some(found);
    }
    let arch = match env::var("HOST").unwrap_or_default() {
        h if h.starts_with("aarch64") => "arm64",
        h if h.starts_with("i686") => "x86",
        _ => "x64",
    };
    let roots = [
        env::var("ProgramFiles(x86)").unwrap_or_else(|_| String::from(r"C:\Program Files (x86)")),
        env::var("ProgramFiles").unwrap_or_else(|_| String::from(r"C:\Program Files")),
    ];
    for root in roots {
        let bin = Path::new(&root).join(r"Windows Kits\10\bin");
        let Ok(entries) = fs::read_dir(&bin) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("10."))
            })
            .collect();
        // Newest SDK first (version directories sort lexicographically within 10.0.x).
        versions.sort();
        for dir in versions.iter().rev() {
            let candidate = dir.join(arch).join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Builds an `.ico` whose entries are PNG images (supported since Windows Vista).
fn build_ico() -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = ICO_SIZES
        .iter()
        .map(|&s| (s, encode_png(s, &raster::render_logo_rgba(s))))
        .collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, png) in &images {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // color planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in &images {
        out.extend_from_slice(png);
    }
    out
}

/// Minimal RGBA8 PNG encoder (filter "Up" + fixed-Huffman deflate with simple LZ77).
fn encode_png(size: u32, rgba: &[u8]) -> Vec<u8> {
    let stride = size as usize * 4;
    let mut raw = Vec::with_capacity((stride + 1) * size as usize);
    for y in 0..size as usize {
        raw.push(2); // filter: Up
        for x in 0..stride {
            let cur = rgba[y * stride + x];
            let up = if y > 0 { rgba[(y - 1) * stride + x] } else { 0 };
            raw.push(cur.wrapping_sub(up));
        }
    }

    let mut zlib = vec![0x78, 0x01];
    zlib.extend_from_slice(&deflate_fixed(&raw));
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace

    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &zlib);
    png_chunk(&mut png, b"IEND", &[]);
    png
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    bits: u32,
}

impl BitWriter {
    /// Writes `n` bits of `value`, least significant bit first.
    fn put(&mut self, value: u32, n: u32) {
        self.acc |= value << self.bits;
        self.bits += n;
        while self.bits >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.bits -= 8;
        }
    }

    /// Writes a Huffman code (most significant bit first).
    fn code(&mut self, code: u32, len: u32) {
        let mut rev = 0;
        for i in 0..len {
            rev |= ((code >> i) & 1) << (len - 1 - i);
        }
        self.put(rev, len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

fn put_literal(w: &mut BitWriter, sym: u32) {
    match sym {
        0..=143 => w.code(0x30 + sym, 8),
        144..=255 => w.code(0x190 + sym - 144, 9),
        256..=279 => w.code(sym - 256, 7),
        _ => w.code(0xC0 + sym - 280, 8),
    }
}

const LEN_BASE: [u32; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u32; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn put_match(w: &mut BitWriter, len: u32, dist: u32) {
    let li = LEN_BASE.iter().rposition(|&b| b <= len).unwrap_or(0);
    put_literal(w, 257 + li as u32);
    w.put(len - LEN_BASE[li], LEN_EXTRA[li]);
    let di = DIST_BASE.iter().rposition(|&b| b <= dist).unwrap_or(0);
    w.code(di as u32, 5);
    w.put(dist - DIST_BASE[di], DIST_EXTRA[di]);
}

/// Single fixed-Huffman deflate block; greedy matches at a few useful distances
/// (previous byte, previous pixel, and the matching pixel one row up).
fn deflate_fixed(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter {
        out: Vec::with_capacity(data.len() / 2),
        acc: 0,
        bits: 0,
    };
    w.put(1, 1); // BFINAL
    w.put(1, 2); // BTYPE = fixed Huffman
    let mut i = 0;
    while i < data.len() {
        let mut best = (0usize, 0usize);
        for dist in [1usize, 4, 8] {
            if dist > i {
                continue;
            }
            let max = (data.len() - i).min(258);
            let mut len = 0;
            while len < max && data[i + len] == data[i + len - dist] {
                len += 1;
            }
            if len > best.0 {
                best = (len, dist);
            }
        }
        if best.0 >= 3 {
            put_match(&mut w, best.0 as u32, best.1 as u32);
            i += best.0;
        } else {
            put_literal(&mut w, u32::from(data[i]));
            i += 1;
        }
    }
    put_literal(&mut w, 256);
    w.finish()
}
