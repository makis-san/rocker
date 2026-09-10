//! Bundled files that `install` writes to disk: the icon set, the desktop
//! entry, and the AppStream metadata. Only the PNG icon set is embedded raw;
//! the macOS `.icns` and Windows `.ico` containers are assembled on demand
//! from those same PNGs so nothing large rides in the binary.

/// `(size, png_bytes)` for every hicolor icon we ship, largest last.
pub const ICON_PNGS: &[(u32, &[u8])] = &[
    (
        16,
        include_bytes!("../../../assets/icons/hicolor/16x16.png"),
    ),
    (
        24,
        include_bytes!("../../../assets/icons/hicolor/24x24.png"),
    ),
    (
        32,
        include_bytes!("../../../assets/icons/hicolor/32x32.png"),
    ),
    (
        48,
        include_bytes!("../../../assets/icons/hicolor/48x48.png"),
    ),
    (
        64,
        include_bytes!("../../../assets/icons/hicolor/64x64.png"),
    ),
    (
        128,
        include_bytes!("../../../assets/icons/hicolor/128x128.png"),
    ),
    (
        256,
        include_bytes!("../../../assets/icons/hicolor/256x256.png"),
    ),
    (
        512,
        include_bytes!("../../../assets/icons/hicolor/512x512.png"),
    ),
];

/// The freedesktop desktop entry, verbatim from `packaging/linux/`. `install`
/// rewrites every `Exec=` line to an absolute path before writing it out.
pub const DESKTOP_ENTRY: &str =
    include_str!("../../../packaging/linux/io.github.makis_san.Rocker.desktop");

/// AppStream metadata, shared with the `.deb`/`.rpm`/Flatpak builds.
pub const METAINFO_XML: &str =
    include_str!("../../../packaging/linux/io.github.makis_san.Rocker.metainfo.xml");

/// The 512×512 PNG, used for the legacy `pixmaps/` fallback and (on macOS) as
/// the largest icns entry.
pub fn png_512() -> &'static [u8] {
    png_for(512)
}

fn png_for(size: u32) -> &'static [u8] {
    ICON_PNGS
        .iter()
        .find(|(s, _)| *s == size)
        .map(|(_, b)| *b)
        .unwrap_or(ICON_PNGS[ICON_PNGS.len() - 1].1)
}

/// Assemble a PNG-based `.icns` (Apple icon container) from the embedded PNGs.
/// Types per <https://en.wikipedia.org/wiki/Apple_Icon_Image_format>.
#[cfg(target_os = "macos")]
pub fn icns() -> Vec<u8> {
    const ENTRIES: &[(&[u8; 4], u32)] = &[
        (b"ic11", 32),
        (b"ic12", 64),
        (b"ic07", 128),
        (b"ic08", 256),
        (b"ic09", 512),
    ];
    let mut body = Vec::new();
    for (ostype, size) in ENTRIES {
        let png = png_for(*size);
        body.extend_from_slice(*ostype);
        body.extend_from_slice(&(png.len() as u32 + 8).to_be_bytes());
        body.extend_from_slice(png);
    }
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// Assemble a single-image `.ico` holding the 256×256 PNG. Valid on Windows
/// Vista and later, which read PNG-compressed icon entries.
#[cfg(windows)]
pub fn ico() -> Vec<u8> {
    let png = png_for(256);
    let mut out = Vec::with_capacity(png.len() + 22);
    // ICONDIR: reserved, type=1 (icon), count=1
    out.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    // ICONDIRENTRY: width/height 0 == 256, 0 colors, 0 reserved, 1 plane, 32bpp
    out.extend_from_slice(&[0, 0, 0, 0, 1, 0, 32, 0]);
    out.extend_from_slice(&(png.len() as u32).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes());
    out.extend_from_slice(png);
    out
}
