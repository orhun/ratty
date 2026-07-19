use std::f32::consts::TAU;
use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent};
use crossterm::execute;
use ratatui::{
    DefaultTerminal, Frame as TuiFrame,
    buffer::Buffer,
    layout::{Constraint, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Widget},
};
use ratatui_ratty::{RattyGraphic, RattyGraphicSettings};

const TICK: Duration = Duration::from_millis(33);
// Radians/sec the board auto-spins at when not paused.
const SPIN_RATE: f32 = 0.35;

// A Möbius strip's standard parametrization already closes on itself after a
// SINGLE loop (u in [0,1)); the half-twist means the row at the seam glues
// to the mirrored row, not to itself, but you don't need to loop twice to
// get a closed surface. 8x8 = 64 squares, one loop.
const BOARD_COLS: usize = 8;
const BOARD_ROWS: usize = 8;
// Extra quads per square along the loop direction only, purely for a smooth
// curve - this does not change the number of chess squares (still 64), just
// how many polygons each square is built from.
const CURVE_SUBDIV: usize = 6;

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut app = MobiusChessApp::new()?;
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = app.run(terminal);
    let _ = app.clear();
    execute!(io::stdout(), DisableMouseCapture)?;
    result
}

struct MobiusChessApp {
    scene: RattyGraphic<'static>,
    view: SceneView,
    viewport: Rect,
    should_quit: bool,
}

impl MobiusChessApp {
    fn new() -> io::Result<Self> {
        RattyGraphic::clear_all()?;

        let scene = RattyGraphic::new(
            RattyGraphicSettings::new(String::from("mobius_chess.obj"))
                .id(1)
                .normalize(false)
                .animate(false),
        );

        let obj_data = build_mobius_scene();
        scene.register_payload_with_name(obj_data.as_bytes(), Some("mobius_chess.obj"))?;

        Ok(Self {
            scene,
            view: SceneView::new(),
            viewport: Rect::default(),
            should_quit: false,
        })
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let mut last_tick = Instant::now();
        while !self.should_quit {
            terminal.draw(|frame| self.render(frame))?;

            let timeout = TICK.saturating_sub(last_tick.elapsed());
            if event::poll(timeout)? {
                self.handle_event(event::read()?)?;
            }

            let now = Instant::now();
            let delta = now.duration_since(last_tick);
            last_tick = now;
            self.tick(delta.as_secs_f32());
        }
        Ok(())
    }

    fn clear(&self) -> io::Result<()> {
        self.scene.clear()
    }

    fn render(&mut self, frame: &mut TuiFrame<'_>) {
        let area = frame.area();
        let header = Rect::new(area.x, area.y, area.width, 3);
        let body = Rect::new(
            area.x,
            area.y.saturating_add(3),
            area.width,
            area.height.saturating_sub(3),
        );

        Paragraph::new(Line::from(vec![
            Span::styled("\u{2190} \u{2192}", Style::default().fg(Color::Cyan)),
            Span::raw(": yaw  "),
            Span::styled("\u{2191} \u{2193}", Style::default().fg(Color::Cyan)),
            Span::raw(": pitch  "),
            Span::styled("space", Style::default().fg(Color::Cyan)),
            Span::raw(": pause spin  "),
            Span::styled("+ -", Style::default().fg(Color::Cyan)),
            Span::raw(": zoom  "),
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::raw(": quit"),
        ]))
        .block(Block::bordered().title(Span::styled(
            "Ratty Mobius Chess",
            Style::default().fg(Color::Yellow),
        )))
        .render(header, frame.buffer_mut());

        let block = Block::bordered()
            .title(self.status())
            .border_style(Style::default().fg(Color::White));
        self.viewport = block.inner(body);
        block.render(body, frame.buffer_mut());
        self.paint_backdrop(frame.buffer_mut());

        let scene_area = self.view.scene_area(self.viewport);
        self.sync_scene();
        self.emit_rgp_sequences(frame.buffer_mut(), scene_area);
    }

    fn status(&self) -> String {
        format!(
            "64 squares | 32 pieces | spin: {} | zoom: {:.2}",
            if self.view.auto_rotate {
                "on"
            } else {
                "paused"
            },
            self.view.zoom,
        )
    }

    fn paint_backdrop(&self, buf: &mut Buffer) {
        let style = Style::default().fg(Color::Indexed(8));
        for y in self.viewport.y..self.viewport.y.saturating_add(self.viewport.height) {
            for x in self.viewport.x..self.viewport.x.saturating_add(self.viewport.width) {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    let shade = if (u32::from(x) + u32::from(y) * 2) % 7 == 0 {
                        '.'
                    } else {
                        ' '
                    };
                    cell.set_char(shade).set_style(style);
                }
            }
        }
    }

    fn sync_scene(&mut self) {
        let rot = self.view.rotation().to_euler_degrees();
        let settings = self.scene.settings_mut();
        settings.animate = false; // re-assert every frame, same as the Rubik's cube example
        settings.rotation = rot;
        settings.scale = 0.85 * self.view.zoom; // Scale the entire universe
    }

    fn emit_rgp_sequences(&mut self, buf: &mut Buffer, area: Rect) {
        if area.is_empty() {
            return;
        }

        let place_objects = self.view.placed_area != Some(area);
        emit_sequence(buf, area.x, area.y, &self.scene.update_sequence());
        if place_objects {
            emit_sequence(buf, area.x, area.y, &self.scene.place_sequence(area));
        }

        if place_objects {
            self.view.placed_area = Some(area);
        }
    }

    fn handle_event(&mut self, event: Event) -> io::Result<()> {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Resize(_, _) => {
                self.view.placed_area = None;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if !key.is_press() {
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char(' ') => self.view.auto_rotate = !self.view.auto_rotate,
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.view.zoom = (self.view.zoom + 0.08).min(3.0)
            }
            KeyCode::Char('-') => self.view.zoom = (self.view.zoom - 0.08).max(0.2),
            KeyCode::Left => self.view.yaw -= 0.12,
            KeyCode::Right => self.view.yaw += 0.12,
            KeyCode::Up => self.view.pitch = (self.view.pitch - 0.10).max(-1.35),
            KeyCode::Down => self.view.pitch = (self.view.pitch + 0.10).min(1.35),
            _ => {}
        }
    }

    fn tick(&mut self, delta: f32) {
        if self.view.auto_rotate {
            self.view.yaw += delta * SPIN_RATE;
        }
    }
}

// --- Mesh building ---

/// Accumulates the growing OBJ text plus the running vertex/normal index
/// counters. Bundling these together (instead of passing `obj`, `v_idx`, and
/// `vn_idx` as three separate params everywhere) is what keeps `push_quad`/
/// `push_tri`/`lathe_piece` under clippy's argument-count limit.
struct MeshBuilder {
    obj: String,
    v_idx: usize,
    vn_idx: usize,
}

impl MeshBuilder {
    fn new() -> Self {
        Self {
            obj: String::from("# ratty-mobius-chess\n"),
            v_idx: 1,
            vn_idx: 1,
        }
    }

    fn push_quad(&mut self, pts: [[f32; 3]; 4], color: (f32, f32, f32)) {
        let n = calc_normal(pts[0], pts[1], pts[3]);
        for p in pts {
            self.obj.push_str(&format!(
                "v {:.5} {:.5} {:.5} {:.2} {:.2} {:.2}\n",
                p[0], p[1], p[2], color.0, color.1, color.2
            ));
        }
        self.obj
            .push_str(&format!("vn {:.5} {:.5} {:.5}\n", n[0], n[1], n[2]));
        let (v, vn) = (self.v_idx, self.vn_idx);
        self.obj.push_str(&format!(
            "f {}//{} {}//{} {}//{}\n",
            v,
            vn,
            v + 1,
            vn,
            v + 2,
            vn
        ));
        self.obj.push_str(&format!(
            "f {}//{} {}//{} {}//{}\n",
            v,
            vn,
            v + 2,
            vn,
            v + 3,
            vn
        ));
        self.v_idx += 4;
        self.vn_idx += 1;
    }

    fn push_tri(&mut self, pts: [[f32; 3]; 3], color: (f32, f32, f32)) {
        let n = calc_normal(pts[0], pts[1], pts[2]);
        for p in pts {
            self.obj.push_str(&format!(
                "v {:.5} {:.5} {:.5} {:.2} {:.2} {:.2}\n",
                p[0], p[1], p[2], color.0, color.1, color.2
            ));
        }
        self.obj
            .push_str(&format!("vn {:.5} {:.5} {:.5}\n", n[0], n[1], n[2]));
        let (v, vn) = (self.v_idx, self.vn_idx);
        self.obj.push_str(&format!(
            "f {}//{} {}//{} {}//{}\n",
            v,
            vn,
            v + 1,
            vn,
            v + 2,
            vn
        ));
        self.v_idx += 3;
        self.vn_idx += 1;
    }

    fn into_obj(self) -> String {
        self.obj
    }
}

fn calc_normal(p0: [f32; 3], p1: [f32; 3], p2: [f32; 3]) -> [f32; 3] {
    let dx1 = p1[0] - p0[0];
    let dy1 = p1[1] - p0[1];
    let dz1 = p1[2] - p0[2];
    let dx2 = p2[0] - p0[0];
    let dy2 = p2[1] - p0[1];
    let dz2 = p2[2] - p0[2];
    let nx = dy1 * dz2 - dz1 * dy2;
    let ny = dz1 * dx2 - dx1 * dz2;
    let nz = dx1 * dy2 - dy1 * dx2;
    let len = (nx * nx + ny * ny + nz * nz).sqrt();
    if len > 0.0 {
        [nx / len, ny / len, nz / len]
    } else {
        [0.0, 0.0, 1.0]
    }
}

// --- Möbius surface math ---

fn mobius_raw(angle: f32, width: f32) -> [f32; 3] {
    let ring = 0.24 + width * (angle * 0.5).cos();
    [
        ring * angle.cos(),
        ring * angle.sin(),
        width * (angle * 0.5).sin(),
    ]
}

fn mobius_normal(angle: f32, width: f32) -> [f32; 3] {
    let eps = 0.001;
    let p0 = mobius_raw(angle, width);
    let p_dx = mobius_raw(angle + eps, width);
    let p_dy = mobius_raw(angle, width + eps);
    calc_normal(p0, p_dx, p_dy)
}

fn mobius_surface_point(local_x: f32, local_y: f32, depth: f32) -> [f32; 3] {
    let angle = (local_x + 0.5) * TAU;
    let width = local_y * 0.42;
    let base = mobius_raw(angle, width);

    if depth == 0.0 {
        return base;
    }

    let n = mobius_normal(angle, width);
    [
        base[0] + n[0] * depth,
        base[1] + n[1] * depth,
        base[2] + n[2] * depth,
    ]
}

// --- Vector helpers for building oriented piece geometry ---

fn v_add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn v_sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn v_scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn v_dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn v_cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn v_normalize(a: [f32; 3]) -> [f32; 3] {
    let len = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    if len > 1e-6 {
        [a[0] / len, a[1] / len, a[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    }
}

/// A local orthonormal frame anchored on the Möbius surface at (u, v):
/// `base` sits on the surface, `up` is the outward surface normal (a piece's
/// height axis), and `t1`/`t2` span the tangent plane a lathed piece gets
/// revolved around.
struct Frame {
    base: [f32; 3],
    up: [f32; 3],
    t1: [f32; 3],
    t2: [f32; 3],
}

fn local_frame(u: f32, v: f32) -> Frame {
    let angle = (u + 0.5) * TAU;
    let width = v * 0.42;
    let base = mobius_raw(angle, width);
    let n = mobius_normal(angle, width);

    let eps = 0.001;
    let raw_t1 = v_normalize(v_sub(mobius_raw(angle + eps, width), base));
    // Gram-Schmidt so t1 is exactly perpendicular to the surface normal
    let t1 = v_normalize(v_sub(raw_t1, v_scale(n, v_dot(raw_t1, n))));
    let t2 = v_normalize(v_cross(n, t1));

    Frame {
        base,
        up: n,
        t1,
        t2,
    }
}

fn ring_pt(frame: &Frame, height: f32, radius: f32, theta: f32) -> [f32; 3] {
    v_add(
        frame.base,
        v_add(
            v_scale(frame.up, height),
            v_add(
                v_scale(frame.t1, radius * theta.cos()),
                v_scale(frame.t2, radius * theta.sin()),
            ),
        ),
    )
}

// --- Piece silhouettes (height, radius) pairs, bottom to top, sized to sit
// inside one board square (row spacing is ~0.05 world units wide) ---

fn pawn_profile() -> Vec<(f32, f32)> {
    vec![
        (0.000, 0.018),
        (0.004, 0.018),
        (0.010, 0.012),
        (0.020, 0.009),
        (0.026, 0.007),
        (0.032, 0.009),
        (0.040, 0.006),
        (0.050, 0.0),
    ]
}
fn bishop_profile() -> Vec<(f32, f32)> {
    vec![
        (0.000, 0.019),
        (0.004, 0.019),
        (0.010, 0.013),
        (0.020, 0.009),
        (0.030, 0.007),
        (0.042, 0.009),
        (0.052, 0.006),
        (0.062, 0.0),
    ]
}
fn queen_profile() -> Vec<(f32, f32)> {
    vec![
        (0.000, 0.020),
        (0.004, 0.020),
        (0.010, 0.014),
        (0.022, 0.009),
        (0.034, 0.008),
        (0.048, 0.010),
        (0.060, 0.011),
        (0.068, 0.0),
    ]
}
fn king_profile() -> Vec<(f32, f32)> {
    vec![
        (0.000, 0.020),
        (0.004, 0.020),
        (0.010, 0.014),
        (0.022, 0.009),
        (0.036, 0.008),
        (0.050, 0.010),
        (0.064, 0.011),
        (0.072, 0.005),
        (0.078, 0.0),
    ]
}
fn rook_profile() -> Vec<(f32, f32)> {
    // Ends on a flat rim (no closing to radius 0) - crenellations sit on top of it.
    vec![
        (0.000, 0.019),
        (0.004, 0.019),
        (0.010, 0.013),
        (0.020, 0.010),
        (0.030, 0.011),
        (0.038, 0.013),
    ]
}

/// Revolves a (height, radius) profile around `frame.up`, emitting colored
/// triangles directly into `builder`. Used for pawn/bishop/queen/king and as
/// the cylindrical base of the rook.
fn lathe_piece(
    builder: &mut MeshBuilder,
    frame: &Frame,
    profile: &[(f32, f32)],
    segments: usize,
    color: (f32, f32, f32),
    cap_top: bool,
) {
    let (h0, r0) = profile[0];
    let bottom_center = v_add(frame.base, v_scale(frame.up, h0));
    for s in 0..segments {
        let theta_a = (s as f32 / segments as f32) * TAU;
        let theta_b = ((s + 1) as f32 / segments as f32) * TAU;
        let a = ring_pt(frame, h0, r0, theta_a);
        let b = ring_pt(frame, h0, r0, theta_b);
        builder.push_tri([bottom_center, b, a], color);
    }

    for w in profile.windows(2) {
        let (h0, r0) = w[0];
        let (h1, r1) = w[1];
        for s in 0..segments {
            let theta_a = (s as f32 / segments as f32) * TAU;
            let theta_b = ((s + 1) as f32 / segments as f32) * TAU;
            let a = ring_pt(frame, h0, r0, theta_a);
            let b = ring_pt(frame, h0, r0, theta_b);
            let c = ring_pt(frame, h1, r1, theta_a);
            let d = ring_pt(frame, h1, r1, theta_b);
            builder.push_tri([a, b, d], color);
            builder.push_tri([a, d, c], color);
        }
    }

    let (ht, rt) = *profile.last().unwrap();
    if cap_top && rt > 1e-4 {
        let top_center = v_add(frame.base, v_scale(frame.up, ht));
        for s in 0..segments {
            let theta_a = (s as f32 / segments as f32) * TAU;
            let theta_b = ((s + 1) as f32 / segments as f32) * TAU;
            let a = ring_pt(frame, ht, rt, theta_a);
            let b = ring_pt(frame, ht, rt, theta_b);
            builder.push_tri([top_center, a, b], color);
        }
    }
}

/// Rook: lathed cylindrical body up to a flat rim, then a ring of small
/// battlement blocks stitched onto that rim.
fn rook_piece(builder: &mut MeshBuilder, frame: &Frame, segments: usize, color: (f32, f32, f32)) {
    let profile = rook_profile();
    lathe_piece(builder, frame, &profile, segments, color, false);

    let (top_h, top_r) = *profile.last().unwrap();
    let inner_r = top_r * 0.65;
    let block_h = top_r * 0.9;
    let notches = 8;
    let block_w = 0.55;

    for n in 0..notches {
        let theta0 = (n as f32 / notches as f32) * TAU;
        let theta1 = theta0 + (TAU / notches as f32) * block_w;

        let outer0 = ring_pt(frame, top_h, top_r, theta0);
        let outer1 = ring_pt(frame, top_h, top_r, theta1);
        let inner0 = ring_pt(frame, top_h, inner_r, theta0);
        let inner1 = ring_pt(frame, top_h, inner_r, theta1);
        let outer0t = ring_pt(frame, top_h + block_h, top_r, theta0);
        let outer1t = ring_pt(frame, top_h + block_h, top_r, theta1);
        let inner0t = ring_pt(frame, top_h + block_h, inner_r, theta0);
        let inner1t = ring_pt(frame, top_h + block_h, inner_r, theta1);

        builder.push_quad([outer0, outer1, inner1, inner0], color); // bottom
        builder.push_quad([outer0t, inner0t, inner1t, outer1t], color); // top
        builder.push_quad([outer0, outer0t, outer1t, outer1], color); // outer wall
        builder.push_quad([inner0, inner1, inner1t, inner0t], color); // inner wall
        builder.push_quad([outer0, inner0, inner0t, outer0t], color); // side theta0
        builder.push_quad([outer1, outer1t, inner1t, inner1], color); // side theta1
    }
}

/// Knight: a small lathed base plus a handful of hand-placed boxes that lean
/// forward (along the local `t1` tangent) to suggest a horse's neck/head/ear.
/// Not radially symmetric, so a lathe alone can't make one.
fn knight_piece(builder: &mut MeshBuilder, frame: &Frame, segments: usize, color: (f32, f32, f32)) {
    let base_profile = vec![
        (0.000, 0.018),
        (0.004, 0.018),
        (0.010, 0.013),
        (0.018, 0.010),
        (0.026, 0.009),
    ];
    lathe_piece(builder, frame, &base_profile, segments, color, true);

    let mk = |t1v: f32, upv: f32, t2v: f32| -> [f32; 3] {
        v_add(
            frame.base,
            v_add(
                v_scale(frame.up, upv),
                v_add(v_scale(frame.t1, t1v), v_scale(frame.t2, t2v)),
            ),
        )
    };

    // (t1_min, up_min, t2_min, t1_max, up_max, t2_max)
    let boxes = [
        (-0.006, 0.010, -0.006, 0.006, 0.020, 0.006), // neck base
        (-0.003, 0.018, -0.005, 0.012, 0.028, 0.005), // neck leaning forward (+t1)
        (0.006, 0.024, -0.005, 0.020, 0.032, 0.005),  // head / muzzle
        (0.005, 0.030, -0.006, 0.011, 0.038, -0.001), // ear
    ];

    for (t1min, upmin, t2min, t1max, upmax, t2max) in boxes {
        let c = [
            mk(t1min, upmin, t2min),
            mk(t1max, upmin, t2min),
            mk(t1max, upmin, t2max),
            mk(t1min, upmin, t2max),
            mk(t1min, upmax, t2min),
            mk(t1max, upmax, t2min),
            mk(t1max, upmax, t2max),
            mk(t1min, upmax, t2max),
        ];
        let quads = [
            [c[0], c[1], c[2], c[3]], // bottom
            [c[4], c[7], c[6], c[5]], // top
            [c[0], c[4], c[5], c[1]],
            [c[1], c[5], c[6], c[2]],
            [c[2], c[6], c[7], c[3]],
            [c[3], c[7], c[4], c[0]],
        ];
        for q in quads {
            builder.push_quad(q, color);
        }
    }
}

fn place_piece(builder: &mut MeshBuilder, name: &str, col: usize, row: usize, is_white: bool) {
    let uc = (col as f32 + 0.5) / BOARD_COLS as f32 - 0.5;
    let vc = (row as f32 + 0.5) / BOARD_ROWS as f32 - 0.5;
    let frame = local_frame(uc, vc);
    let color = if is_white {
        (0.95, 0.94, 0.90)
    } else {
        (0.08, 0.08, 0.10)
    };

    match name {
        "pawn" => lathe_piece(builder, &frame, &pawn_profile(), 10, color, true),
        "bishop" => lathe_piece(builder, &frame, &bishop_profile(), 12, color, true),
        "queen" => lathe_piece(builder, &frame, &queen_profile(), 14, color, true),
        "king" => lathe_piece(builder, &frame, &king_profile(), 14, color, true),
        "rook" => rook_piece(builder, &frame, 10, color),
        "knight" => knight_piece(builder, &frame, 8, color),
        _ => {}
    }
}

fn build_mobius_scene() -> String {
    let mut builder = MeshBuilder::new();

    // --- Board: exactly BOARD_COLS x BOARD_ROWS = 64 chess squares, each
    // built from CURVE_SUBDIV sub-quads along the loop just to keep the
    // curve smooth (that subdivision does not add extra squares/colors).
    let x_segments = BOARD_COLS * CURVE_SUBDIV;
    let y_segments = BOARD_ROWS;

    for y in 0..y_segments {
        for x in 0..x_segments {
            let u0 = x as f32 / x_segments as f32 - 0.5;
            let u1 = (x + 1) as f32 / x_segments as f32 - 0.5;
            let v0 = y as f32 / y_segments as f32 - 0.5;
            let v1 = (y + 1) as f32 / y_segments as f32 - 0.5;

            let p0 = mobius_surface_point(u0, v0, 0.0);
            let p1 = mobius_surface_point(u1, v0, 0.0);
            let p2 = mobius_surface_point(u1, v1, 0.0);
            let p3 = mobius_surface_point(u0, v1, 0.0);

            let square_col = x / CURVE_SUBDIV;
            let square_row = y;
            let is_white_square = (square_col + square_row).is_multiple_of(2);
            let color = if is_white_square {
                (0.90, 0.88, 0.80)
            } else {
                (0.14, 0.16, 0.20)
            };

            builder.push_quad([p0, p1, p2, p3], color);
        }
    }

    // --- Pieces: a standard start position compressed onto 4 of the 8 rows
    // (back rank / pawns / pawns / back rank), across all 8 columns.
    let back_rank = [
        "rook", "knight", "bishop", "queen", "king", "bishop", "knight", "rook",
    ];
    for (col, piece_name) in back_rank.into_iter().enumerate() {
        place_piece(&mut builder, piece_name, col, 0, true);
        place_piece(&mut builder, "pawn", col, 1, true);
        place_piece(&mut builder, "pawn", col, BOARD_ROWS - 2, false);
        place_piece(&mut builder, piece_name, col, BOARD_ROWS - 1, false);
    }

    builder.into_obj()
}

// --- App Math & Plumbing ---

struct SceneView {
    yaw: f32,
    pitch: f32,
    auto_rotate: bool,
    zoom: f32,
    placed_area: Option<Rect>,
}

impl SceneView {
    fn new() -> Self {
        Self {
            yaw: -0.58,
            pitch: -0.42,
            auto_rotate: true,
            zoom: 1.0,
            placed_area: None,
        }
    }

    fn rotation(&self) -> Mat3 {
        Mat3::rotation_x(self.pitch) * Mat3::rotation_y(self.yaw)
    }

    fn scene_area(&self, viewport: Rect) -> Rect {
        if viewport.is_empty() {
            return viewport;
        }
        let width = viewport.width.saturating_sub(2).clamp(1, 70);
        let height = viewport.height.saturating_sub(2).clamp(1, 30);
        viewport.centered(Constraint::Length(width), Constraint::Length(height))
    }
}

struct Mat3 {
    m: [[f32; 3]; 3],
}

impl Mat3 {
    fn rotation_x(angle: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self {
            m: [[1.0, 0.0, 0.0], [0.0, cos, -sin], [0.0, sin, cos]],
        }
    }
    fn rotation_y(angle: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self {
            m: [[cos, 0.0, sin], [0.0, 1.0, 0.0], [-sin, 0.0, cos]],
        }
    }
    fn to_euler_degrees(&self) -> [f32; 3] {
        let cy = (self.m[0][0] * self.m[0][0] + self.m[0][1] * self.m[0][1]).sqrt();
        let (x, y, z) = if cy > 16.0 * f32::EPSILON {
            (
                -self.m[1][2].atan2(self.m[2][2]),
                self.m[0][2].atan2(cy),
                -self.m[0][1].atan2(self.m[0][0]),
            )
        } else {
            (
                self.m[1][0].atan2(self.m[1][1]),
                self.m[0][2].atan2(cy),
                0.0,
            )
        };
        [x.to_degrees(), y.to_degrees(), z.to_degrees()]
    }
}

impl std::ops::Mul for Mat3 {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self::Output {
        let mut m = [[0.0; 3]; 3];
        for (row, values) in m.iter_mut().enumerate() {
            for (col, value) in values.iter_mut().enumerate() {
                *value = self.m[row][0] * rhs.m[0][col]
                    + self.m[row][1] * rhs.m[1][col]
                    + self.m[row][2] * rhs.m[2][col];
            }
        }
        Self { m }
    }
}

fn emit_sequence(buf: &mut Buffer, x: u16, y: u16, sequence: &str) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        let existing = cell.symbol();
        let mut symbol = String::with_capacity(sequence.len() + existing.len());
        symbol.push_str(sequence);
        symbol.push_str(existing);
        cell.set_symbol(&symbol);
    }
}
