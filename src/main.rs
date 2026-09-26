mod overlay;

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::NONE;

type Error = Box<dyn std::error::Error>;

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        CLIPBOARD,
        CLIPBOARD_MANAGER,
        SAVE_TARGETS,
        TARGETS,
        TIMESTAMP,
        MULTIPLE,
        ATOM_PAIR,
        INCR,
        IMAGE_PNG: b"image/png",
        SHOOT_SAVE: b"SHOOT_SAVE",
        _NET_ACTIVE_WINDOW,
        _NET_FRAME_EXTENTS,
        _GTK_FRAME_EXTENTS,
    }
}

struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

#[derive(PartialEq)]
enum Mode {
    Screen,
    Window,
    Area,
}

fn main() {
    let mode = match std::env::args().nth(1).as_deref() {
        None | Some("screen") => Mode::Screen,
        Some("window") => Mode::Window,
        Some("area") => Mode::Area,
        Some(other) => {
            eprintln!("usage: shoot [screen|window|area] (got {other:?})");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(mode) {
        eprintln!("shoot: {e}");
        let _ = Command::new("notify-send")
            .args(["-u", "critical", "Screenshot failed", &e.to_string()])
            .status();
        std::process::exit(1);
    }
}

fn run(mode: Mode) -> Result<(), Error> {
    let (conn, screen_num) = x11rb::connect(None)?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let atoms = Atoms::new(&conn)?.reply()?;

    let full = Rect { x: 0, y: 0, w: screen.width_in_pixels as i32, h: screen.height_in_pixels as i32 };
    let rect = if mode == Mode::Window {
        active_window_rect(&conn, root, &atoms, &full)?.unwrap_or(full)
    } else {
        full
    };

    let raw = capture(&conn, root, screen.root_depth, &rect)?;
    let (rgb, w, h) = if mode == Mode::Area {
        let Some(sel) = overlay::select(&conn, screen, &raw)? else {
            return Ok(());
        };
        overlay::flash(&conn, screen_num, &sel)?;
        (to_rgb(&raw, rect.w, &sel), sel.w, sel.h)
    } else {
        overlay::flash(&conn, screen_num, &rect)?;
        (to_rgb(&raw, rect.w, &Rect { x: 0, y: 0, w: rect.w, h: rect.h }), rect.w, rect.h)
    };
    let png = encode_png(&rgb, w as u32, h as u32)?;
    save(&png)?;
    serve_clipboard(&conn, root, &atoms, png)
}

fn get_cardinals(conn: &RustConnection, window: Window, property: Atom, ty: Atom) -> Result<Vec<u32>, Error> {
    let reply = conn.get_property(false, window, property, ty, 0, 64)?.reply()?;
    Ok(reply.value32().map(|v| v.collect()).unwrap_or_default())
}

fn active_window_rect(conn: &RustConnection, root: Window, atoms: &Atoms, full: &Rect) -> Result<Option<Rect>, Error> {
    let active = get_cardinals(conn, root, atoms._NET_ACTIVE_WINDOW, AtomEnum::WINDOW.into())?;
    let Some(&window) = active.first().filter(|&&w| w != NONE) else {
        return Ok(None);
    };
    let geometry = conn.get_geometry(window)?.reply()?;
    let origin = conn.translate_coordinates(window, root, 0, 0)?.reply()?;
    let mut r = Rect {
        x: origin.dst_x as i32,
        y: origin.dst_y as i32,
        w: geometry.width as i32,
        h: geometry.height as i32,
    };

    let frame = get_cardinals(conn, window, atoms._NET_FRAME_EXTENTS, AtomEnum::CARDINAL.into())?;
    if let [l, rt, t, b] = frame[..] {
        r.x -= l as i32;
        r.y -= t as i32;
        r.w += (l + rt) as i32;
        r.h += (t + b) as i32;
    }
    let gtk = get_cardinals(conn, window, atoms._GTK_FRAME_EXTENTS, AtomEnum::CARDINAL.into())?;
    if let [l, rt, t, b] = gtk[..] {
        r.x += l as i32;
        r.y += t as i32;
        r.w -= (l + rt) as i32;
        r.h -= (t + b) as i32;
    }

    let x0 = r.x.max(full.x);
    let y0 = r.y.max(full.y);
    let x1 = (r.x + r.w).min(full.x + full.w);
    let y1 = (r.y + r.h).min(full.y + full.h);
    if x1 <= x0 || y1 <= y0 {
        return Ok(None);
    }
    Ok(Some(Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }))
}

fn capture(conn: &RustConnection, root: Window, depth: u8, r: &Rect) -> Result<Vec<u8>, Error> {
    let setup = conn.setup();
    let bpp = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == depth)
        .map(|f| f.bits_per_pixel)
        .ok_or("no pixmap format for root depth")?;
    if bpp != 32 || setup.image_byte_order != ImageOrder::LSB_FIRST {
        return Err(format!("unsupported pixel layout: {bpp} bpp, {:?}", setup.image_byte_order).into());
    }

    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, root, r.x as i16, r.y as i16, r.w as u16, r.h as u16, !0)?
        .reply()?;
    Ok(image.data)
}

fn to_rgb(raw: &[u8], stride: i32, r: &Rect) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(r.w as usize * r.h as usize * 3);
    for y in r.y..r.y + r.h {
        let start = (y * stride + r.x) as usize * 4;
        for px in raw[start..start + r.w as usize * 4].chunks_exact(4) {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    rgb
}

fn encode_png(rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgb)?;
    writer.finish()?;
    Ok(out)
}

fn timestamp_name() -> String {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!(
            "Screenshot from {:04}-{:02}-{:02} {:02}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

fn save(png: &[u8]) -> Result<PathBuf, Error> {
    let dir = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?).join("Pictures");
    std::fs::create_dir_all(&dir)?;
    let base = timestamp_name();
    for n in 1.. {
        let name = if n == 1 { format!("{base}.png") } else { format!("{base} - {n}.png") };
        let path = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(png)?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!()
}

struct Transfer {
    offset: usize,
}

struct Server<'a> {
    conn: &'a RustConnection,
    atoms: &'a Atoms,
    png: Vec<u8>,
    time: Timestamp,
    chunk: usize,
    transfers: HashMap<(Window, Atom), Transfer>,
}

impl Server<'_> {
    fn convert(&mut self, requestor: Window, target: Atom, property: Atom) -> Result<bool, Error> {
        let a = self.atoms;
        if target == a.TARGETS {
            let targets = [a.TARGETS, a.TIMESTAMP, a.MULTIPLE, a.IMAGE_PNG];
            self.conn.change_property32(PropMode::REPLACE, requestor, property, AtomEnum::ATOM, &targets)?;
        } else if target == a.TIMESTAMP {
            self.conn.change_property32(PropMode::REPLACE, requestor, property, AtomEnum::INTEGER, &[self.time])?;
        } else if target == a.IMAGE_PNG {
            if self.png.len() <= self.chunk {
                self.conn.change_property8(PropMode::REPLACE, requestor, property, a.IMAGE_PNG, &self.png)?;
            } else {
                self.conn.change_window_attributes(
                    requestor,
                    &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
                )?;
                self.conn
                    .change_property32(PropMode::REPLACE, requestor, property, a.INCR, &[self.png.len() as u32])?;
                self.transfers.insert((requestor, property), Transfer { offset: 0 });
            }
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    fn request(&mut self, e: &SelectionRequestEvent) -> Result<(), Error> {
        let property = if e.property == NONE { e.target } else { e.property };
        let ok = if e.target == self.atoms.MULTIPLE {
            let reply = self
                .conn
                .get_property(false, e.requestor, property, self.atoms.ATOM_PAIR, 0, u32::MAX / 4)?
                .reply()?;
            let mut pairs: Vec<u32> = reply.value32().map(|v| v.collect()).unwrap_or_default();
            for pair in pairs.chunks_exact_mut(2) {
                if !self.convert(e.requestor, pair[0], pair[1])? {
                    pair[1] = NONE;
                }
            }
            self.conn
                .change_property32(PropMode::REPLACE, e.requestor, property, self.atoms.ATOM_PAIR, &pairs)?;
            true
        } else {
            self.convert(e.requestor, e.target, property)?
        };
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: e.time,
            requestor: e.requestor,
            selection: e.selection,
            target: e.target,
            property: if ok { property } else { NONE },
        };
        self.conn.send_event(false, e.requestor, EventMask::NO_EVENT, notify)?;
        self.conn.flush()?;
        Ok(())
    }

    fn property_deleted(&mut self, e: &PropertyNotifyEvent) -> Result<(), Error> {
        let key = (e.window, e.atom);
        let Some(t) = self.transfers.get_mut(&key) else {
            return Ok(());
        };
        let end = (t.offset + self.chunk).min(self.png.len());
        let data = &self.png[t.offset..end];
        self.conn
            .change_property8(PropMode::REPLACE, e.window, e.atom, self.atoms.IMAGE_PNG, data)?;
        if data.is_empty() {
            self.transfers.remove(&key);
        } else {
            t.offset = end;
        }
        self.conn.flush()?;
        Ok(())
    }
}

fn serve_clipboard(conn: &RustConnection, root: Window, atoms: &Atoms, png: Vec<u8>) -> Result<(), Error> {
    let window = conn.generate_id()?;
    conn.create_window(
        0,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )?;
    conn.change_property8(PropMode::APPEND, window, atoms.SHOOT_SAVE, AtomEnum::STRING, &[])?;
    conn.flush()?;
    let time = loop {
        if let Event::PropertyNotify(e) = conn.wait_for_event()? {
            if e.window == window {
                break e.time;
            }
        }
    };

    conn.set_selection_owner(window, atoms.CLIPBOARD, time)?;
    if conn.get_selection_owner(atoms.CLIPBOARD)?.reply()?.owner != window {
        return Err("could not take clipboard ownership".into());
    }

    let manager = conn.get_selection_owner(atoms.CLIPBOARD_MANAGER)?.reply()?.owner != NONE;
    if manager {
        conn.change_property32(PropMode::REPLACE, window, atoms.SHOOT_SAVE, AtomEnum::ATOM, &[atoms.IMAGE_PNG])?;
        conn.convert_selection(window, atoms.CLIPBOARD_MANAGER, atoms.SAVE_TARGETS, atoms.SHOOT_SAVE, time)?;
    }
    conn.flush()?;

    let chunk = conn.maximum_request_bytes() - 1024;
    let mut server = Server { conn, atoms, png, time, chunk, transfers: HashMap::new() };
    let mut done = false;
    loop {
        match conn.wait_for_event()? {
            Event::SelectionRequest(e) => server.request(&e)?,
            Event::PropertyNotify(e) if e.state == Property::DELETE => server.property_deleted(&e)?,
            Event::SelectionClear(e) if e.selection == atoms.CLIPBOARD => done = true,
            Event::SelectionNotify(e) if e.selection == atoms.CLIPBOARD_MANAGER && e.property != NONE => done = true,
            _ => {}
        }
        if done && server.transfers.is_empty() {
            return Ok(());
        }
    }
}
