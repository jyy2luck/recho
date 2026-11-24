use eframe::egui::{self, Align2, Color32, Pos2, Rect, Ui, Vec2, ViewportCommand};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{cell::Cell, io, rc::Rc};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::ScreenToClient,
    UI::{
        HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GetWindowRect, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT,
            HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, IsZoomed, SM_CXPADDEDBORDER, SM_CXSIZEFRAME,
            SM_CYSIZEFRAME, WM_NCDESTROY, WM_NCHITTEST,
        },
    },
};

pub(super) const HEIGHT: f32 = 36.0;
const BUTTON_WIDTH: f32 = 46.0;

pub(super) struct TitleBar {
    drag_area: Rc<Cell<Rect>>,
    icon: egui::TextureHandle,
}

impl TitleBar {
    pub(super) fn attach(
        cc: &eframe::CreationContext<'_>,
        icon: &egui::IconData,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let RawWindowHandle::Win32(handle) = cc.window_handle()?.as_raw() else {
            return Err(io::Error::other("expected a Win32 window handle").into());
        };
        let drag_area = Rc::new(Cell::new(Rect::NOTHING));
        let reference = Rc::into_raw(drag_area.clone());
        // The window owns this reference until WM_NCDESTROY.
        if unsafe {
            SetWindowSubclass(
                handle.hwnd.get() as HWND,
                Some(window_proc),
                0,
                reference as usize,
            )
        } == 0
        {
            unsafe { drop(Rc::from_raw(reference)) };
            return Err(io::Error::other("could not attach the Windows title bar").into());
        }
        let icon = cc.egui_ctx.load_texture(
            "window-icon",
            egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            ),
            egui::TextureOptions::LINEAR,
        );
        Ok(Self { drag_area, icon })
    }

    pub(super) fn menu_ui(ui: &mut Ui, row: Rect) -> Ui {
        let height = super::workbench::MENU_HEIGHT;
        let bounds = Rect::from_min_max(
            egui::pos2(row.left() + 12.0, row.center().y - height / 2.0),
            egui::pos2(
                row.right() - 3.0 * BUTTON_WIDTH,
                row.center().y + height / 2.0,
            ),
        );
        ui.new_child(
            egui::UiBuilder::new()
                .id_salt("title-menu")
                .max_rect(bounds),
        )
    }

    pub(super) fn icon(&self, ui: &mut Ui) {
        ui.image((self.icon.id(), Vec2::splat(22.0)));
    }

    pub(super) fn show(&self, ui: &mut Ui, row: Rect, menu_end: f32) {
        let controls = row.right() - 3.0 * BUTTON_WIDTH;
        let drag = Rect::from_min_max(
            egui::pos2(menu_end, row.top()),
            egui::pos2(controls, row.bottom()),
        );
        let hit_area = if egui::Popup::is_any_open(ui.ctx()) {
            Rect::NOTHING
        } else {
            drag * ui.ctx().pixels_per_point()
        };
        self.drag_area.set(hit_area);

        if drag.width() >= 60.0 {
            let x = row
                .center()
                .x
                .clamp(drag.left() + 30.0, drag.right() - 30.0);
            ui.painter().text(
                egui::pos2(x, row.center().y),
                Align2::CENTER_CENTER,
                "recho",
                egui::FontId::proportional(13.0),
                ui.visuals().text_color(),
            );
        }

        let maximized = ui.input(|input| input.viewport().maximized == Some(true));
        let (maximize_icon, maximize_label) = if maximized {
            ("\u{e3e0}", "还原")
        } else {
            ("\u{e3c6}", "最大化")
        };
        for (index, (glyph, label, command)) in [
            ("\u{e15b}", "最小化", ViewportCommand::Minimized(true)),
            (
                maximize_icon,
                maximize_label,
                ViewportCommand::Maximized(!maximized),
            ),
            ("\u{e5cd}", "关闭窗口", ViewportCommand::Close),
        ]
        .into_iter()
        .enumerate()
        {
            let bounds = Rect::from_min_size(
                egui::pos2(controls + index as f32 * BUTTON_WIDTH, row.top()),
                egui::vec2(BUTTON_WIDTH, row.height()),
            );
            let mut button_ui =
                ui.new_child(egui::UiBuilder::new().id_salt(index).max_rect(bounds));
            if command == ViewportCommand::Close {
                let widgets = &mut button_ui.visuals_mut().widgets;
                for state in [&mut widgets.hovered, &mut widgets.active] {
                    state.weak_bg_fill = Color32::from_rgb(232, 17, 35);
                    state.fg_stroke.color = Color32::WHITE;
                }
            }
            let response = button_ui.put(
                bounds,
                egui::Button::new(super::workbench::icon(glyph))
                    .min_size(bounds.size())
                    .frame_when_inactive(false)
                    .corner_radius(0),
            );
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
            if response.on_hover_text(label).clicked() {
                ui.ctx().send_viewport_cmd(command);
            }
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    reference: usize,
) -> LRESULT {
    let drag_area = reference as *const Cell<Rect>;
    match message {
        WM_NCHITTEST => {
            let mut point = POINT {
                x: lparam as i16 as i32,
                y: (lparam >> 16) as i16 as i32,
            };
            if unsafe { IsZoomed(hwnd) } == 0 {
                let mut window = RECT::default();
                if unsafe { GetWindowRect(hwnd, &mut window) } != 0 {
                    let dpi = unsafe { GetDpiForWindow(hwnd) };
                    let padding = unsafe { GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) };
                    let x = unsafe { GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi) } + padding;
                    let y = unsafe { GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) } + padding;
                    let hit = resize_hit(window, point, x, y);
                    if hit != HTCLIENT {
                        return hit as LRESULT;
                    }
                }
            }
            if unsafe { ScreenToClient(hwnd, &mut point) } != 0
                && unsafe { (*drag_area).get() }.contains(Pos2::new(point.x as f32, point.y as f32))
            {
                return HTCAPTION as LRESULT;
            }
        }
        WM_NCDESTROY => unsafe {
            RemoveWindowSubclass(hwnd, Some(window_proc), subclass);
            drop(Rc::from_raw(drag_area));
        },
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

fn resize_hit(window: RECT, point: POINT, x: i32, y: i32) -> u32 {
    let horizontal = if point.x < window.left + x {
        HTLEFT
    } else if point.x >= window.right - x {
        HTRIGHT
    } else {
        HTCLIENT
    };
    if point.y < window.top + y {
        match horizontal {
            HTLEFT => HTTOPLEFT,
            HTRIGHT => HTTOPRIGHT,
            _ => HTTOP,
        }
    } else if point.y >= window.bottom - y {
        match horizontal {
            HTLEFT => HTBOTTOMLEFT,
            HTRIGHT => HTBOTTOMRIGHT,
            _ => HTBOTTOM,
        }
    } else {
        horizontal
    }
}

#[cfg(test)]
#[test]
fn native_resize_preserves_edges_and_title_bar_interior() {
    let window = RECT {
        left: -400,
        top: -100,
        right: 800,
        bottom: 700,
    };
    for (x, y, expected) in [
        (-397, -99, HTTOPLEFT),
        (794, -90, HTTOPRIGHT),
        (0, -93, HTTOP),
        (-399, 0, HTLEFT),
        (799, 0, HTRIGHT),
        (-391, 688, HTBOTTOMLEFT),
        (790, 688, HTBOTTOMRIGHT),
        (0, 699, HTBOTTOM),
        (0, -82, HTCLIENT),
    ] {
        assert_eq!(resize_hit(window, POINT { x, y }, 10, 12), expected);
    }
}
