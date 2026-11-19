//! Минимальная программная реализация видеовыхода PS4.
//!
//! Реальный `libSceGnmDriver` управляет GPU AMD Liverpool (GCN) через
//! командные буферы. Здесь реализован «программный» растеризатор:
//!  * собственный компактный формат командных буферов (см. [`Command`]);
//!  * заливка треугольника/прямоугольника/линии с z-тестом и альфой;
//!  * захват гостевого кадрового буфера (`sceVideoOutSubmitFlip`);
//!  * сохранение скриншотов в PPM/BMP.
//!
//! Это позволяет проверять графический конвейер, снимать «скриншоты»
//! и отлаживать игры, которые рисуют простую графику (меню, отладка).

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b, a: 255 }
    }
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const RED: Color = Color::rgb(255, 0, 0);
    pub const GREEN: Color = Color::rgb(0, 255, 0);
    pub const BLUE: Color = Color::rgb(0, 0, 255);

    pub fn to_u32(self) -> u32 {
        (self.a as u32) << 24 | (self.b as u32) << 16 | (self.g as u32) << 8 | self.r as u32
    }

    pub fn from_u32(v: u32) -> Self {
        Color {
            r: v as u8,
            g: (v >> 8) as u8,
            b: (v >> 16) as u8,
            a: (v >> 24) as u8,
        }
    }

    fn lerp(self, other: Color, t: f32) -> Color {
        let f = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).clamp(0.0, 255.0) as u8;
        Color { r: f(self.r, other.r), g: f(self.g, other.g), b: f(self.b, other.b), a: f(self.a, other.a) }
    }
}

/// Команда графического буфера.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Смена кадрового буфера.
    SetFramebuffer { addr: u64, width: u32, height: u32, pitch: u32 },
    /// Очистка экрана.
    Clear(Color),
    /// Заливка прямоугольника.
    FillRect { x: i32, y: i32, w: i32, h: i32, color: Color },
    /// Линия (алгоритм Брезенхэма).
    DrawLine { x0: i32, y0: i32, x1: i32, y1: i32, color: Color },
    /// Треугольник с интерполяцией цвета по вершинам и z-тестом.
    DrawTri {
        v: [[f32; 4]; 3],
        color: [Color; 3],
    },
    /// Установить область отсечения.
    Scissor { x: i32, y: i32, w: i32, h: i32 },
    /// Вывести кадр на «экран» (запоминаем как последний кадр).
    Flip(u64),
    /// Конец буфера.
    End,
}

impl Command {
    /// Размер команды в байтах командного буфера (для разбора потока).
    pub fn encoded_len(&self) -> usize {
        match self {
            Command::SetFramebuffer { .. } => 8,
            Command::Clear(_) => 4,
            Command::FillRect { .. } => 12,
            Command::DrawLine { .. } => 10,
            Command::DrawTri { .. } => 16,
            Command::Scissor { .. } => 8,
            Command::Flip(_) => 4,
            Command::End => 1,
        }
    }
}

/// Простой программный растеризатор.
pub struct Gpu {
    pub width: u32,
    pub height: u32,
    /// Цветовые буферы (RGBA8888).
    pub color: Vec<u32>,
    pub depth: Vec<f32>,
    pub scissor: (i32, i32, i32, i32),
    pub frames: u64,
    pub draw_calls: u64,
    pub triangles: u64,
    pub pixels_drawn: u64,
    /// Последний снятый кадр (RGB).
    pub last_frame: Option<(u32, u32, Vec<u8>)>,
    pub flips: Vec<u64>,
    pub errors: Vec<String>,
}

impl Gpu {
    pub fn new(width: u32, height: u32) -> Self {
        let n = (width * height) as usize;
        Gpu {
            width,
            height,
            color: vec![0xFF00_0000; n],
            depth: vec![f32::INFINITY; n],
            scissor: (0, 0, width as i32, height as i32),
            frames: 0,
            draw_calls: 0,
            triangles: 0,
            pixels_drawn: 0,
            last_frame: None,
            flips: Vec::new(),
            errors: Vec::new(),
        }
    }

    #[inline]
    fn inside(&self, x: i32, y: i32) -> bool {
        let (sx, sy, sw, sh) = self.scissor;
        x >= sx && y >= sy && x < sx + sw && y < sy + sh && x >= 0 && y >= 0
            && (x as u32) < self.width
            && (y as u32) < self.height
    }

    #[inline]
    fn put(&mut self, x: i32, y: i32, c: Color) {
        if !self.inside(x, y) {
            return;
        }
        let idx = y as usize * self.width as usize + x as usize;
        self.color[idx] = c.to_u32();
        self.pixels_drawn += 1;
    }

    pub fn clear(&mut self, c: Color) {
        self.color.fill(c.to_u32());
        self.depth.fill(f32::INFINITY);
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.put(xx, yy, c);
            }
        }
        self.draw_calls += 1;
    }

    pub fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: Color) {
        let (mut x, mut y) = (x0, y0);
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            self.put(x, y, c);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
        self.draw_calls += 1;
    }

    /// Треугольник: перспективно-корректная интерполяция не нужна,
    /// вершины задаются уже в экранных координатах (x, y, z, w).
    pub fn draw_tri(&mut self, v: [[f32; 4]; 3], colors: [Color; 3]) {
        self.triangles += 1;
        self.draw_calls += 1;
        let minx = v.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min).floor() as i32;
        let maxx = v.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max).ceil() as i32;
        let miny = v.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min).floor() as i32;
        let maxy = v.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max).ceil() as i32;
        let area = (v[1][0] - v[0][0]) * (v[2][1] - v[0][1])
            - (v[2][0] - v[0][0]) * (v[1][1] - v[0][1]);
        if area.abs() < 1e-9 {
            return;
        }
        for y in miny..=maxy {
            for x in minx..=maxx {
                if !self.inside(x, y) {
                    continue;
                }
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let w0 = ((v[1][0] - px) * (v[2][1] - py) - (v[2][0] - px) * (v[1][1] - py)) / area;
                let w1 = ((v[2][0] - px) * (v[0][1] - py) - (v[0][0] - px) * (v[2][1] - py)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let z = w0 * v[0][2] + w1 * v[1][2] + w2 * v[2][2];
                let idx = y as usize * self.width as usize + x as usize;
                if z >= self.depth[idx] {
                    continue;
                }
                let c = colors[0].lerp(colors[1], w1).lerp(colors[2], w2);
                self.depth[idx] = z;
                self.color[idx] = c.to_u32();
                self.pixels_drawn += 1;
            }
        }
    }

    /// Разбор и исполнение собственного командного буфера.
    pub fn submit(&mut self, cmds: &[Command]) {
        for c in cmds {
            match *c {
                Command::SetFramebuffer { width, height, .. } => {
                    if width > 0 && height > 0 && (width != self.width || height != self.height) {
                        self.width = width;
                        self.height = height;
                        let n = (width * height) as usize;
                        self.color = vec![0xFF00_0000; n];
                        self.depth = vec![f32::INFINITY; n];
                        self.scissor = (0, 0, width as i32, height as i32);
                    }
                }
                Command::Clear(col) => self.clear(col),
                Command::FillRect { x, y, w, h, color } => self.fill_rect(x, y, w, h, color),
                Command::DrawLine { x0, y0, x1, y1, color } => self.draw_line(x0, y0, x1, y1, color),
                Command::DrawTri { v, color } => self.draw_tri(v, color),
                Command::Scissor { x, y, w, h } => self.scissor = (x, y, w, h),
                Command::Flip(id) => {
                    self.flips.push(id);
                    self.frames += 1;
                }
                Command::End => break,
            }
        }
    }

    /// Захват кадра из гостевой памяти (RGBA8888).
    pub fn capture_guest(&mut self, mem: &mut crate::mem::Memory, addr: u64, w: u32, h: u32, pitch: u32) {
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        let mut row = vec![0u8; (w * 4) as usize];
        let mut ok = 0usize;
        for y in 0..h {
            if mem.read_partial(addr + (y as u64) * pitch as u64, &mut row) < row.len() {
                break;
            }
            for x in 0..w {
                let i = (x * 4) as usize;
                rgb.push(row[i]);
                rgb.push(row[i + 1]);
                rgb.push(row[i + 2]);
            }
            ok += 1;
        }
        if ok == h as usize {
            self.last_frame = Some((w, h, rgb));
        } else {
            self.errors
                .push(format!("не удалось прочитать кадровый буфер {:#x} ({} из {} строк)", addr, ok, h));
        }
    }

    /// Сохранение кадра в PPM (P6).
    pub fn save_ppm(&self, path: &str) -> std::io::Result<()> {
        use std::io::Write;
        let (w, h, data) = match &self.last_frame {
            Some(f) => (f.0, f.1, f.2.clone()),
            None => {
                // кадр из нашего растеризатора
                let mut v = Vec::with_capacity((self.width * self.height * 3) as usize);
                for px in &self.color {
                    v.push(*px as u8);
                    v.push((*px >> 8) as u8);
                    v.push((*px >> 16) as u8);
                }
                (self.width, self.height, v)
            }
        };
        let mut f = std::fs::File::create(path)?;
        write!(f, "P6\n{} {}\n255\n", w, h)?;
        f.write_all(&data)?;
        Ok(())
    }

    /// Сохранение кадра в BMP (24 бита).
    pub fn save_bmp(&self, path: &str) -> std::io::Result<()> {
        use std::io::Write;
        let (w, h, data) = match &self.last_frame {
            Some(f) => (f.0, f.1, f.2.clone()),
            None => {
                let mut v = Vec::with_capacity((self.width * self.height * 3) as usize);
                for px in &self.color {
                    v.push(*px as u8);
                    v.push((*px >> 8) as u8);
                    v.push((*px >> 16) as u8);
                }
                (self.width, self.height, v)
            }
        };
        let row_size = ((w * 3 + 3) / 4) * 4;
        let image_size = row_size * h;
        let file_size = 54 + image_size;
        let mut f = std::fs::File::create(path)?;
        let mut hdr = Vec::with_capacity(54);
        hdr.extend_from_slice(b"BM");
        hdr.extend_from_slice(&file_size.to_le_bytes());
        hdr.extend_from_slice(&0u32.to_le_bytes());
        hdr.extend_from_slice(&54u32.to_le_bytes());
        hdr.extend_from_slice(&40u32.to_le_bytes());
        hdr.extend_from_slice(&(w as i32).to_le_bytes());
        hdr.extend_from_slice(&(h as i32).to_le_bytes());
        hdr.extend_from_slice(&1u16.to_le_bytes());
        hdr.extend_from_slice(&24u16.to_le_bytes());
        hdr.extend_from_slice(&0u32.to_le_bytes());
        hdr.extend_from_slice(&image_size.to_le_bytes());
        hdr.extend_from_slice(&2835u32.to_le_bytes());
        hdr.extend_from_slice(&2835u32.to_le_bytes());
        hdr.extend_from_slice(&0u32.to_le_bytes());
        hdr.extend_from_slice(&0u32.to_le_bytes());
        f.write_all(&hdr)?;
        let mut row = vec![0u8; row_size as usize];
        for y in (0..h).rev() {
            row.iter_mut().for_each(|b| *b = 0);
            for x in 0..w {
                let i = ((y * w + x) * 3) as usize;
                let o = (x * 3) as usize;
                row[o] = data[i + 2];
                row[o + 1] = data[i + 1];
                row[o + 2] = data[i];
            }
            f.write_all(&row)?;
        }
        Ok(())
    }

    pub fn stats(&self) -> String {
        format!(
            "GPU {}x{}: кадров {}, вызовов отрисовки {}, треугольников {}, пикселей {}",
            self.width, self.height, self.frames, self.draw_calls, self.triangles, self.pixels_drawn
        )
    }
}

impl fmt::Debug for Gpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Gpu({}x{}, кадров {})", self.width, self.height, self.frames)
    }
}

// ---------------------------------------------------------------------------
// Ввод: виртуальный контроллер DualShock 4
// ---------------------------------------------------------------------------

pub mod pad {
    /// Биты кнопок в том же порядке, что и `ScePadButtonDataOffset`.
    pub const L3: u32 = 1 << 0;
    pub const R3: u32 = 1 << 1;
    pub const OPTIONS: u32 = 1 << 2;
    pub const UP: u32 = 1 << 3;
    pub const RIGHT: u32 = 1 << 4;
    pub const DOWN: u32 = 1 << 5;
    pub const LEFT: u32 = 1 << 6;
    pub const L2: u32 = 1 << 7;
    pub const R2: u32 = 1 << 8;
    pub const L1: u32 = 1 << 9;
    pub const R1: u32 = 1 << 10;
    pub const TRIANGLE: u32 = 1 << 11;
    pub const CIRCLE: u32 = 1 << 12;
    pub const CROSS: u32 = 1 << 13;
    pub const SQUARE: u32 = 1 << 14;
    pub const TOUCH: u32 = 1 << 15;

    #[derive(Clone, Debug, Default)]
    pub struct PadState {
        pub buttons: u32,
        pub left_stick: (u8, u8),
        pub right_stick: (u8, u8),
        pub l2: u8,
        pub r2: u8,
        pub connected: bool,
        pub timestamp_us: u64,
    }

    impl PadState {
        pub fn connected() -> Self {
            PadState { connected: true, left_stick: (128, 128), right_stick: (128, 128), ..Default::default() }
        }

        pub fn press(&mut self, bit: u32) {
            self.buttons |= bit;
        }
        pub fn release(&mut self, bit: u32) {
            self.buttons &= !bit;
        }
        pub fn clear(&mut self) {
            self.buttons = 0;
        }

        /// Установить состояние по текстовому описанию, например
        /// `"cross+left"` или `"none"`.
        pub fn apply_script(&mut self, script: &str) {
            self.buttons = 0;
            for part in script.split(['+', ',', ' ']) {
                let p = part.trim().to_ascii_lowercase();
                let bit = match p.as_str() {
                    "l3" => L3,
                    "r3" => R3,
                    "options" | "start" => OPTIONS,
                    "up" => UP,
                    "right" => RIGHT,
                    "down" => DOWN,
                    "left" => LEFT,
                    "l1" => L1,
                    "r1" => R1,
                    "l2" => L2,
                    "r2" => R2,
                    "triangle" => TRIANGLE,
                    "circle" => CIRCLE,
                    "cross" | "x" => CROSS,
                    "square" => SQUARE,
                    "touch" => TOUCH,
                    _ => 0,
                };
                self.buttons |= bit;
            }
        }

        /// Записать структуру ScePadData в гостевую память (0x78 байт).
        pub fn write_to(&self, mem: &mut crate::mem::Memory, addr: u64) -> Result<(), crate::mem::MemError> {
            let mut buf = [0u8; 0x78];
            buf[0..4].copy_from_slice(&self.buttons.to_le_bytes());
            buf[4..5].copy_from_slice(&[self.left_stick.0]);
            buf[5..6].copy_from_slice(&[self.left_stick.1]);
            buf[6..7].copy_from_slice(&[self.right_stick.0]);
            buf[7..8].copy_from_slice(&[self.right_stick.1]);
            buf[8..9].copy_from_slice(&[self.l2]);
            buf[9..10].copy_from_slice(&[self.r2]);
            buf[10..12].copy_from_slice(&0u16.to_le_bytes()); // padding
            buf[12..16].copy_from_slice(&(if self.connected { 1u32 } else { 0 }).to_le_bytes());
            buf[16..24].copy_from_slice(&self.timestamp_us.to_le_bytes());
            mem.write_bytes(addr, &buf)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn script_sets_buttons() {
            let mut p = PadState::connected();
            p.apply_script("cross+left");
            assert_eq!(p.buttons, CROSS | LEFT);
            p.apply_script("none");
            assert_eq!(p.buttons, 0);
        }

        #[test]
        fn write_to_memory() {
            let mut mem = crate::mem::Memory::new();
            mem.map(0x1000, 0x1000, 3, "t").unwrap();
            let mut p = PadState::connected();
            p.press(TRIANGLE);
            p.write_to(&mut mem, 0x1000).unwrap();
            assert_eq!(mem.read_u32(0x1000).unwrap(), TRIANGLE as u32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_and_pixel() {
        let mut g = Gpu::new(8, 8);
        g.clear(Color::RED);
        assert_eq!(g.color[0], Color::RED.to_u32());
        assert_eq!(Color::from_u32(g.color[63]), Color::RED);
    }

    #[test]
    fn rect_respects_scissor() {
        let mut g = Gpu::new(16, 16);
        g.clear(Color::BLACK);
        g.scissor = (4, 4, 4, 4);
        g.fill_rect(0, 0, 16, 16, Color::GREEN);
        assert_eq!(Color::from_u32(g.color[0]), Color::BLACK);
        assert_eq!(Color::from_u32(g.color[4 * 16 + 4]), Color::GREEN);
        assert_eq!(Color::from_u32(g.color[5 * 16 + 5]), Color::GREEN);
        assert_eq!(Color::from_u32(g.color[9 * 16 + 9]), Color::BLACK);
    }

    #[test]
    fn triangle_rasterizes() {
        let mut g = Gpu::new(32, 32);
        g.clear(Color::BLACK);
        g.draw_tri(
            [[2.0, 2.0, 0.0, 1.0], [30.0, 2.0, 0.0, 1.0], [2.0, 30.0, 0.0, 1.0]],
            [Color::RED, Color::GREEN, Color::BLUE],
        );
        assert!(g.pixels_drawn > 100);
        // вблизи вершины (2,2) должен быть красный
        let c = Color::from_u32(g.color[3 * 32 + 3]);
        assert!(c.r > 150, "ожидался красный, получено {:?}", c);
    }

    #[test]
    fn depth_test_rejects_far_geometry() {
        let mut g = Gpu::new(8, 8);
        g.clear(Color::BLACK);
        let near = [[0.0, 0.0, 0.1, 1.0], [8.0, 0.0, 0.1, 1.0], [0.0, 8.0, 0.1, 1.0]];
        let far = [[0.0, 0.0, 0.9, 1.0], [8.0, 0.0, 0.9, 1.0], [0.0, 8.0, 0.9, 1.0]];
        g.draw_tri(near, [Color::RED; 3]);
        g.draw_tri(far, [Color::BLUE; 3]);
        assert_eq!(Color::from_u32(g.color[10]), Color::RED);
    }

    #[test]
    fn command_stream_and_ppm() {
        let mut g = Gpu::new(4, 4);
        g.submit(&[
            Command::Clear(Color::BLUE),
            Command::FillRect { x: 1, y: 1, w: 2, h: 2, color: Color::WHITE },
            Command::Flip(7),
            Command::End,
        ]);
        assert_eq!(g.frames, 1);
        assert_eq!(g.flips, vec![7]);
        assert_eq!(Color::from_u32(g.color[0]), Color::BLUE);
        assert_eq!(Color::from_u32(g.color[5]), Color::WHITE);
        let path = std::env::temp_dir().join("ps4emu_test.ppm");
        g.save_ppm(path.to_str().unwrap()).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"P6\n4 4\n255\n"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn capture_guest_framebuffer() {
        let mut mem = crate::mem::Memory::new();
        mem.map(0x1000, 0x1000, 3, "fb").unwrap();
        // 2x2: красный, зелёный / синий, белый
        let px = [
            Color::RED.to_u32(),
            Color::GREEN.to_u32(),
            Color::BLUE.to_u32(),
            Color::WHITE.to_u32(),
        ];
        for (i, p) in px.iter().enumerate() {
            mem.write_u32(0x1000 + i as u64 * 4, *p).unwrap();
        }
        let mut g = Gpu::new(2, 2);
        g.capture_guest(&mut mem, 0x1000, 2, 2, 8);
        let (w, h, data) = g.last_frame.clone().unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(&data[0..3], &[255, 0, 0]);
    }
}
