use eframe::egui;
use std::{collections::VecDeque, path::Path};

const ICON_CACHE_LIMIT: usize = 128;
#[cfg(any(windows, target_os = "macos"))]
const ICON_SIZE: usize = 32;

#[derive(Default)]
pub(super) struct ApplicationIcons {
    entries: VecDeque<(String, Option<egui::TextureHandle>)>,
}

impl ApplicationIcons {
    pub(super) fn get(
        &mut self,
        ctx: &egui::Context,
        path: Option<&str>,
    ) -> Option<&egui::TextureHandle> {
        let path = path?;
        if let Some(index) = self.entries.iter().position(|(key, _)| key == path) {
            if index != 0 {
                let entry = self.entries.remove(index).unwrap();
                self.entries.push_front(entry);
            }
        } else {
            if self.entries.len() == ICON_CACHE_LIMIT {
                self.entries.pop_back();
            }
            let texture = read_icon(Path::new(path)).map(|icon| {
                ctx.load_texture("application icon", icon, egui::TextureOptions::LINEAR)
            });
            self.entries.push_front((path.to_owned(), texture));
        }
        self.entries
            .front()
            .and_then(|(_, texture)| texture.as_ref())
    }
}

#[cfg(windows)]
fn read_icon(path: &Path) -> Option<egui::ColorImage> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::UI::{Shell::SHDefExtractIconW, WindowsAndMessaging::DestroyIcon};

    if !path.is_absolute() || !path.is_file() {
        return None;
    }
    let wide: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut icon = ptr::null_mut();
    unsafe {
        let status = SHDefExtractIconW(
            wide.as_ptr(),
            0,
            0,
            &mut icon,
            ptr::null_mut(),
            ICON_SIZE as u32,
        );
        if icon.is_null() {
            return None;
        }
        let result = (status == 0).then(|| draw_windows_icon(icon)).flatten();
        DestroyIcon(icon);
        result
    }
}

#[cfg(windows)]
fn draw_windows_icon(
    icon: windows_sys::Win32::UI::WindowsAndMessaging::HICON,
) -> Option<egui::ColorImage> {
    use std::ptr;
    use windows_sys::Win32::{
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
            DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, SelectObject,
        },
        UI::WindowsAndMessaging::{DI_NORMAL, DrawIconEx},
    };

    let size = ICON_SIZE as i32;
    let byte_count = ICON_SIZE * ICON_SIZE * 4;
    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        let dc = CreateCompatibleDC(ptr::null_mut());
        if dc.is_null() {
            return None;
        }
        let mut pixels = ptr::null_mut();
        let bitmap = CreateDIBSection(
            dc,
            &bitmap_info,
            DIB_RGB_COLORS,
            &mut pixels,
            ptr::null_mut(),
            0,
        );
        if bitmap.is_null() {
            DeleteDC(dc);
            return None;
        }
        let previous = SelectObject(dc, bitmap);
        if previous.is_null() {
            DeleteObject(bitmap);
            DeleteDC(dc);
            return None;
        }
        let draw = |background: u8| {
            ptr::write_bytes(pixels.cast::<u8>(), background, byte_count);
            let drawn = DrawIconEx(dc, 0, 0, icon, size, size, 0, ptr::null_mut(), DI_NORMAL);
            GdiFlush();
            (drawn != 0)
                .then(|| std::slice::from_raw_parts(pixels.cast::<u8>(), byte_count).to_vec())
        };
        // Drawing over black and white recovers transparency from both modern and mask-based icons.
        let rgba = draw(0).zip(draw(255)).map(|(black, white)| {
            black
                .chunks_exact(4)
                .zip(white.chunks_exact(4))
                .flat_map(|(black, white)| {
                    let alpha = 255 - white[1].saturating_sub(black[1]);
                    [
                        black[2].min(alpha),
                        black[1].min(alpha),
                        black[0].min(alpha),
                        alpha,
                    ]
                })
                .collect::<Vec<_>>()
        });
        SelectObject(dc, previous);
        DeleteObject(bitmap);
        DeleteDC(dc);
        rgba.map(|rgba| egui::ColorImage::from_rgba_premultiplied([ICON_SIZE, ICON_SIZE], &rgba))
    }
}

#[cfg(target_os = "macos")]
fn read_icon(path: &Path) -> Option<egui::ColorImage> {
    use objc2::AnyThread as _;
    use objc2_app_kit::{
        NSBitmapFormat, NSBitmapImageRep, NSCalibratedRGBColorSpace, NSCompositingOperation,
        NSGraphicsContext, NSWorkspace,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    if !path.is_absolute() || !path.is_file() {
        return None;
    }
    let bundle = path
        .ancestors()
        .filter(|parent| parent.extension().is_some_and(|ext| ext == "app"))
        .last()
        .unwrap_or(path);
    let image = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(bundle.to_str()?));
    let size = ICON_SIZE as isize;
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), size, size, 8, 4, true, false,
            NSCalibratedRGBColorSpace, NSBitmapFormat::empty(), size * 4, 32,
        )?
    };
    let graphics = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&graphics));
    let origin = NSPoint::new(0.0, 0.0);
    image.drawInRect_fromRect_operation_fraction(
        NSRect::new(origin, NSSize::new(size as f64, size as f64)),
        NSRect::new(origin, NSSize::new(0.0, 0.0)),
        NSCompositingOperation::Copy,
        1.0,
    );
    NSGraphicsContext::restoreGraphicsState_class();
    let pixels = bitmap.bitmapData();
    if pixels.is_null() {
        return None;
    }
    let rgba = unsafe { std::slice::from_raw_parts(pixels, ICON_SIZE * ICON_SIZE * 4) };
    Some(egui::ColorImage::from_rgba_premultiplied(
        [ICON_SIZE, ICON_SIZE],
        rgba,
    ))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn read_icon(_path: &Path) -> Option<egui::ColorImage> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn reads_a_windows_icon_with_color_and_transparency() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons/recho.ico");
        let image = read_icon(&path).expect("the bundled Recho icon must be readable");
        assert_eq!(image.size, [ICON_SIZE; 2]);
        assert!(image.pixels.iter().any(|pixel| pixel.g() > pixel.r()));
        assert!(image.pixels.iter().any(|pixel| pixel.a() == 0));
        assert!(image.pixels.iter().any(|pixel| pixel.a() == 255));
    }
}
