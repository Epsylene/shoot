use std::thread::sleep;
use std::time::Duration;

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::{COPY_FROM_PARENT, CURRENT_TIME, NONE};

use crate::{Error, Rect};

const FLASH_ALPHA: u32 = 0x60;
const FLASH_HOLD: Duration = Duration::from_millis(60);
const FLASH_STEPS: u32 = 8;
const FLASH_STEP: Duration = Duration::from_millis(18);
const DIM: u16 = 140;
const XK_ESCAPE: Keysym = 0xff1b;
const XC_CROSSHAIR: u16 = 34;

pub fn flash(conn: &RustConnection, screen_num: usize, r: &Rect) -> Result<(), Error> {
    let screen = &conn.setup().roots[screen_num];
    let cm = conn.intern_atom(false, format!("_NET_WM_CM_S{screen_num}").as_bytes())?.reply()?.atom;
    if conn.get_selection_owner(cm)?.reply()?.owner == NONE {
        return Ok(());
    }
    let Some(visual) = argb_visual(screen) else {
        return Ok(());
    };

    let colormap = conn.generate_id()?;
    conn.create_colormap(ColormapAlloc::NONE, colormap, screen.root, visual)?;
    let window = conn.generate_id()?;
    conn.create_window(
        32,
        window,
        screen.root,
        r.x as i16,
        r.y as i16,
        r.w as u16,
        r.h as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        visual,
        &CreateWindowAux::new()
            .background_pixel(FLASH_ALPHA << 24)
            .border_pixel(0)
            .colormap(colormap)
            .override_redirect(1),
    )?;
    conn.shape_rectangles(shape::SO::SET, shape::SK::INPUT, ClipOrdering::UNSORTED, window, 0, 0, &[])?;
    conn.map_window(window)?;
    conn.flush()?;
    sleep(FLASH_HOLD);
    for step in (0..FLASH_STEPS).rev() {
        let alpha = FLASH_ALPHA * step / FLASH_STEPS;
        conn.change_window_attributes(window, &ChangeWindowAttributesAux::new().background_pixel(alpha << 24))?;
        conn.clear_area(false, window, 0, 0, 0, 0)?;
        conn.flush()?;
        sleep(FLASH_STEP);
    }
    conn.destroy_window(window)?;
    conn.free_colormap(colormap)?;
    conn.flush()?;
    Ok(())
}

fn argb_visual(screen: &Screen) -> Option<Visualid> {
    screen
        .allowed_depths
        .iter()
        .filter(|d| d.depth == 32)
        .flat_map(|d| &d.visuals)
        .find(|v| v.class == VisualClass::TRUE_COLOR)
        .map(|v| v.visual_id)
}

struct Overlay<'a> {
    conn: &'a RustConnection,
    window: Window,
    bright: Pixmap,
    dark: Pixmap,
    back: Pixmap,
    gc: Gcontext,
    cursor: Cursor,
    w: u16,
    h: u16,
}

impl Overlay<'_> {
    fn redraw(&self, sel: Option<&Rect>) -> Result<(), Error> {
        let (conn, w, h) = (self.conn, self.w, self.h);
        conn.copy_area(self.dark, self.back, self.gc, 0, 0, 0, 0, w, h)?;
        if let Some(s) = sel.filter(|s| s.w > 0 && s.h > 0) {
            let (x, y) = (s.x as i16, s.y as i16);
            conn.copy_area(self.bright, self.back, self.gc, x, y, x, y, s.w as u16, s.h as u16)?;
            let outline = Rectangle { x, y, width: (s.w - 1) as u16, height: (s.h - 1) as u16 };
            conn.poly_rectangle(self.back, self.gc, &[outline])?;
        }
        conn.copy_area(self.back, self.window, self.gc, 0, 0, 0, 0, w, h)?;
        conn.flush()?;
        Ok(())
    }
}

impl Drop for Overlay<'_> {
    fn drop(&mut self) {
        let conn = self.conn;
        let _ = conn.ungrab_pointer(CURRENT_TIME);
        let _ = conn.ungrab_keyboard(CURRENT_TIME);
        let _ = conn.destroy_window(self.window);
        for pixmap in [self.bright, self.dark, self.back] {
            let _ = conn.free_pixmap(pixmap);
        }
        let _ = conn.free_gc(self.gc);
        let _ = conn.free_cursor(self.cursor);
        let _ = conn.flush();
    }
}

fn put(conn: &RustConnection, pixmap: Pixmap, gc: Gcontext, data: &[u8], w: u16, depth: u8) -> Result<(), Error> {
    let row = w as usize * 4;
    let rows = ((conn.maximum_request_bytes() - 64) / row).clamp(1, u16::MAX as usize);
    for (i, strip) in data.chunks(rows * row).enumerate() {
        let y = (i * rows) as i16;
        conn.put_image(ImageFormat::Z_PIXMAP, pixmap, gc, w, (strip.len() / row) as u16, 0, y, 0, depth, strip)?;
    }
    Ok(())
}

fn escape_keycode(conn: &RustConnection) -> Result<Option<Keycode>, Error> {
    let setup = conn.setup();
    let count = setup.max_keycode - setup.min_keycode + 1;
    let map = conn.get_keyboard_mapping(setup.min_keycode, count)?.reply()?;
    let per = map.keysyms_per_keycode.max(1) as usize;
    Ok(map
        .keysyms
        .iter()
        .position(|&k| k == XK_ESCAPE)
        .map(|i| setup.min_keycode + (i / per) as u8))
}

fn grab(conn: &RustConnection, window: Window, cursor: Cursor) -> Result<(), Error> {
    let mask = EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION;
    let mut pointer = false;
    let mut keyboard = false;
    for _ in 0..100 {
        if !pointer {
            let status = conn
                .grab_pointer(false, window, mask, GrabMode::ASYNC, GrabMode::ASYNC, window, cursor, CURRENT_TIME)?
                .reply()?
                .status;
            pointer = status == GrabStatus::SUCCESS;
        }
        if !keyboard {
            let status = conn
                .grab_keyboard(false, window, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)?
                .reply()?
                .status;
            keyboard = status == GrabStatus::SUCCESS;
        }
        if pointer && keyboard {
            return Ok(());
        }
        sleep(Duration::from_millis(10));
    }
    Err("could not grab pointer and keyboard".into())
}

fn span(a: (i16, i16), b: (i16, i16), w: u16, h: u16) -> Rect {
    let clamp = |v: i16, max: u16| (v as i32).clamp(0, max as i32);
    let x0 = clamp(a.0.min(b.0), w);
    let y0 = clamp(a.1.min(b.1), h);
    let x1 = clamp(a.0.max(b.0), w);
    let y1 = clamp(a.1.max(b.1), h);
    Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
}

pub fn select(conn: &RustConnection, screen: &Screen, raw: &[u8]) -> Result<Option<Rect>, Error> {
    let (w, h, depth, root) = (screen.width_in_pixels, screen.height_in_pixels, screen.root_depth, screen.root);

    let font = conn.generate_id()?;
    conn.open_font(font, b"cursor")?;
    let cursor = conn.generate_id()?;
    conn.create_glyph_cursor(cursor, font, font, XC_CROSSHAIR, XC_CROSSHAIR + 1, !0, !0, !0, 0, 0, 0)?;
    conn.close_font(font)?;

    let mut ids = [0; 5];
    for id in &mut ids {
        *id = conn.generate_id()?;
    }
    let [window, bright, dark, back, gc] = ids;
    for pixmap in [bright, dark, back] {
        conn.create_pixmap(depth, pixmap, root, w, h)?;
    }
    conn.create_gc(gc, root, &CreateGCAux::new().foreground(screen.white_pixel).graphics_exposures(0))?;
    let overlay = Overlay { conn, window, bright, dark, back, gc, cursor, w, h };

    put(conn, bright, gc, raw, w, depth)?;
    let dimmed: Vec<u8> = raw.iter().map(|&b| ((b as u16 * DIM) >> 8) as u8).collect();
    put(conn, dark, gc, &dimmed, w, depth)?;

    conn.create_window(
        COPY_FROM_PARENT as u8,
        window,
        root,
        0,
        0,
        w,
        h,
        0,
        WindowClass::INPUT_OUTPUT,
        COPY_FROM_PARENT,
        &CreateWindowAux::new()
            .background_pixmap(dark)
            .override_redirect(1)
            .cursor(cursor)
            .event_mask(EventMask::EXPOSURE | EventMask::KEY_PRESS),
    )?;
    conn.map_window(window)?;
    conn.flush()?;
    grab(conn, window, cursor)?;
    let escape = escape_keycode(conn)?;

    let mut anchor: Option<(i16, i16)> = None;
    let mut current = (0, 0);
    loop {
        let mut event = Some(conn.wait_for_event()?);
        let mut dirty = false;
        while let Some(e) = event {
            match e {
                Event::Expose(_) => dirty = true,
                Event::KeyPress(k) if Some(k.detail) == escape => return Ok(None),
                Event::ButtonPress(b) if b.detail == 1 => {
                    anchor = Some((b.root_x, b.root_y));
                    current = (b.root_x, b.root_y);
                    dirty = true;
                }
                Event::ButtonPress(b) if b.detail == 3 => return Ok(None),
                Event::MotionNotify(m) => {
                    current = (m.root_x, m.root_y);
                    dirty |= anchor.is_some();
                }
                Event::ButtonRelease(b) if b.detail == 1 => {
                    if let Some(a) = anchor.take() {
                        let sel = span(a, (b.root_x, b.root_y), w, h);
                        if sel.w > 1 && sel.h > 1 {
                            drop(overlay);
                            return Ok(Some(sel));
                        }
                        dirty = true;
                    }
                }
                _ => {}
            }
            event = conn.poll_for_event()?;
        }
        if dirty {
            overlay.redraw(anchor.map(|a| span(a, current, w, h)).as_ref())?;
        }
    }
}
