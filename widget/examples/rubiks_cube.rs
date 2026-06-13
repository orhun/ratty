use std::{
    borrow::Cow,
    collections::VecDeque,
    fs,
    io::{self, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::window_size,
};
use ratatui::{
    DefaultTerminal, Frame,
    buffer::Buffer,
    layout::{Constraint, Position, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Widget},
};
use ratatui_ratty::{ObjectFormat, RattyGraphic, RattyGraphicSettings};

const TICK: Duration = Duration::from_millis(33);
const TURN_DURATION: f32 = 0.26;
const BODY_SCALE: f32 = 0.15;
const CUBIE_SPACING: f32 = 1.04;
const BASE_Z_UNITS: f32 = 2.7;
const GLB_JSON_CHUNK: u32 = 0x4e4f_534a;
const GLB_BIN_CHUNK: u32 = 0x004e_4942;
const ARRAY_BUFFER: u32 = 34_962;
const ELEMENT_ARRAY_BUFFER: u32 = 34_963;
const COMPONENT_F32: u32 = 5_126;
const COMPONENT_U16: u32 = 5_123;

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut app = RubiksApp::new()?;
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = app.run(terminal);
    let _ = app.clear();
    execute!(io::stdout(), DisableMouseCapture)?;
    result
}

struct RubiksApp {
    cubies: Vec<Cubie>,
    cube: SceneObject,
    active_turn: Option<ActiveTurn>,
    queued_turns: VecDeque<QueuedTurn>,
    history: Vec<MoveSpec>,
    rng: u32,
    model_revision: u32,
    palette: Palette,
    yaw: f32,
    pitch: f32,
    roll: f32,
    zoom: f32,
    spread: f32,
    viewport: Rect,
    placed_area: Option<Rect>,
    drag_start: Option<Position>,
    should_quit: bool,
}

impl RubiksApp {
    fn new() -> io::Result<Self> {
        clear_demo_objects()?;
        let mut app = Self {
            cubies: Vec::new(),
            cube: SceneObject::new_cube(900),
            active_turn: None,
            queued_turns: VecDeque::new(),
            history: Vec::new(),
            rng: 0x516f_6f74,
            model_revision: 0,
            palette: Palette::Classic,
            yaw: -0.58,
            pitch: -0.42,
            roll: 0.0,
            zoom: 1.0,
            spread: 1.0,
            viewport: Rect::default(),
            placed_area: None,
            drag_start: None,
            should_quit: false,
        };
        app.reset_cube();
        app.register_objects()?;
        Ok(app)
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

    fn reset_cube(&mut self) {
        self.cubies.clear();
        self.cube = SceneObject::new_cube(900);
        self.active_turn = None;
        self.queued_turns.clear();
        self.history.clear();
        self.placed_area = None;

        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    if x == 0 && y == 0 && z == 0 {
                        continue;
                    }

                    let pos = Vec3i::new(x, y, z);
                    let mut stickers = Vec::new();
                    for (normal, face) in exposed_faces(pos) {
                        stickers.push(Sticker { normal, face });
                    }

                    self.cubies.push(Cubie::new(pos, stickers));
                }
            }
        }
    }

    fn register_objects(&mut self) -> io::Result<()> {
        self.register_cube_model()
    }

    fn register_cube_model(&mut self) -> io::Result<()> {
        self.model_revision = self.model_revision.wrapping_add(1);
        let active = self.active_turn.as_ref().map(|turn| {
            let progress = smoothstep((turn.elapsed / turn.duration).clamp(0.0, 1.0));
            (
                turn.spec,
                Mat3::from_axis(
                    turn.spec.axis,
                    turn.spec.dir as f32 * progress * std::f32::consts::FRAC_PI_2,
                ),
            )
        });
        let payload = cube_glb(&self.cubies, self.palette, active);
        let path = cube_asset_path(self.model_revision, self.palette)?;
        fs::write(&path, payload)?;
        self.cube.graphic.settings_mut().path = Cow::Owned(path.to_string_lossy().into_owned());
        self.cube.graphic.register()?;
        Ok(())
    }

    fn clear(&self) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.write_all(self.cube.graphic.delete_sequence().as_bytes())?;
        stdout.flush()
    }

    fn render(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let header = Rect::new(area.x, area.y, area.width, 3);
        let body = Rect::new(
            area.x,
            area.y.saturating_add(3),
            area.width,
            area.height.saturating_sub(3),
        );

        Paragraph::new(Line::from(vec![
            Span::styled("mouse", Style::default().fg(Color::Cyan)),
            Span::raw(": orbit  "),
            Span::styled("u d l r f b", Style::default().fg(Color::Cyan)),
            Span::raw(": turn  "),
            Span::styled("shift", Style::default().fg(Color::Cyan)),
            Span::raw(": inverse  "),
            Span::styled("space", Style::default().fg(Color::Cyan)),
            Span::raw(": scramble  "),
            Span::styled("enter", Style::default().fg(Color::Cyan)),
            Span::raw(": solve  "),
            Span::styled("p", Style::default().fg(Color::Cyan)),
            Span::raw(format!(": palette {}  ", self.palette.name())),
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::raw(": quit"),
        ]))
        .block(Block::bordered().title(Span::styled(
            "RGP Rubik's Cube",
            Style::default().fg(Color::Yellow),
        )))
        .render(header, frame.buffer_mut());

        let block = Block::bordered()
            .title(self.status())
            .border_style(Style::default().fg(Color::White));
        self.viewport = block.inner(body);
        block.render(body, frame.buffer_mut());
        self.paint_backdrop(frame.buffer_mut());

        let cube_area = centered_cube_area(self.viewport);
        self.sync_scene_objects(cube_area);
        self.emit_rgp_sequences(frame.buffer_mut(), cube_area);
    }

    fn status(&self) -> String {
        let active = self
            .active_turn
            .as_ref()
            .map(|turn| turn.spec.label())
            .unwrap_or("idle");
        format!(
            "3D cube | cubies: {} | move: {active} | queued: {} | zoom: {:.2} | spread: {:.2}",
            self.cubies.len(),
            self.queued_turns.len(),
            self.zoom,
            self.spread
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

    fn emit_rgp_sequences(&mut self, buf: &mut Buffer, area: Rect) {
        if area.is_empty() {
            return;
        }

        let place_objects = self.placed_area != Some(area);
        emit_sequence(buf, area.x, area.y, &self.cube.transform_update_sequence());
        if place_objects {
            emit_sequence(buf, area.x, area.y, &self.cube.graphic.place_sequence(area));
        }

        if place_objects {
            self.placed_area = Some(area);
        }
    }

    fn sync_scene_objects(&mut self, area: Rect) {
        let metrics = SceneMetrics::new(area, self.zoom);
        let view =
            Mat3::rotation_x(self.pitch) * Mat3::rotation_y(self.yaw) * Mat3::rotation_z(self.roll);
        self.cube
            .apply(SceneUpdate::new(Vec3::new(0.0, 0.0, 0.0), view), &metrics);
    }

    fn handle_event(&mut self, event: Event) -> io::Result<()> {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Resize(_, _) => {
                self.placed_area = None;
                self.drag_start = None;
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
            KeyCode::Esc | KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char(' ') => self.queue_scramble(),
            KeyCode::Enter => self.queue_solve(),
            KeyCode::Backspace => self.queue_undo(),
            KeyCode::Char('0') => {
                self.reset_cube();
                let _ = self.register_objects();
            }
            KeyCode::Char('p') => {
                self.palette = self.palette.next();
                let _ = self.register_objects();
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.zoom = (self.zoom + 0.06).min(1.6);
            }
            KeyCode::Char('-') => {
                self.zoom = (self.zoom - 0.06).max(0.55);
            }
            KeyCode::Char(']') => {
                self.spread = (self.spread + 0.03).min(1.22);
            }
            KeyCode::Char('[') => {
                self.spread = (self.spread - 0.03).max(0.86);
            }
            KeyCode::Left => self.yaw -= 0.12,
            KeyCode::Right => self.yaw += 0.12,
            KeyCode::Up => self.pitch = (self.pitch - 0.10).max(-1.35),
            KeyCode::Down => self.pitch = (self.pitch + 0.10).min(1.35),
            KeyCode::Char(ch) => {
                if let Some(spec) = MoveSpec::from_char(ch) {
                    self.queue_turn(spec, true);
                }
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let pos = Position::new(mouse.column, mouse.row);
        let inside = contains(self.viewport, pos);
        match mouse.kind {
            MouseEventKind::Down(crossterm::event::MouseButton::Left) if inside => {
                self.drag_start = Some(pos);
            }
            MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                let Some(previous) = self.drag_start else {
                    self.drag_start = Some(pos);
                    return;
                };
                let dx = pos.x as i32 - previous.x as i32;
                let dy = pos.y as i32 - previous.y as i32;
                self.yaw += dx as f32 * 0.035;
                self.pitch = (self.pitch + dy as f32 * 0.035).clamp(-1.35, 1.35);
                self.drag_start = Some(pos);
            }
            MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                self.drag_start = None;
            }
            MouseEventKind::ScrollUp if inside => {
                self.zoom = (self.zoom + 0.05).min(1.6);
            }
            MouseEventKind::ScrollDown if inside => {
                self.zoom = (self.zoom - 0.05).max(0.55);
            }
            _ => {}
        }
    }

    fn tick(&mut self, delta: f32) {
        if self.active_turn.is_none() {
            if let Some(queued) = self.queued_turns.pop_front() {
                self.active_turn = Some(ActiveTurn {
                    spec: queued.spec,
                    record: queued.record,
                    elapsed: 0.0,
                    duration: TURN_DURATION,
                });
                let _ = self.register_cube_model();
            }
        }

        let Some(turn) = self.active_turn.as_mut() else {
            return;
        };
        turn.elapsed += delta;
        let completed = if turn.elapsed < turn.duration {
            None
        } else {
            Some(*turn)
        };

        if completed.is_none() {
            let _ = self.register_cube_model();
            return;
        };

        let completed = completed.expect("checked above");
        self.apply_move(completed.spec);
        if completed.record {
            self.history.push(completed.spec);
        }
        self.active_turn = None;
        let _ = self.register_cube_model();
    }

    fn queue_turn(&mut self, spec: MoveSpec, record: bool) {
        self.queued_turns.push_back(QueuedTurn { spec, record });
    }

    fn queue_scramble(&mut self) {
        let mut previous_axis = None;
        let mut count = 0;
        while count < 24 {
            let spec = self.random_move();
            if previous_axis == Some(spec.axis) {
                continue;
            }
            previous_axis = Some(spec.axis);
            self.queue_turn(spec, true);
            count += 1;
        }
    }

    fn queue_solve(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let solution = self
            .history
            .iter()
            .rev()
            .map(|spec| spec.inverse())
            .collect::<Vec<_>>();
        self.history.clear();
        for spec in solution {
            self.queue_turn(spec, false);
        }
    }

    fn queue_undo(&mut self) {
        let Some(spec) = self.history.pop() else {
            return;
        };
        self.queue_turn(spec.inverse(), false);
    }

    fn random_move(&mut self) -> MoveSpec {
        const MOVES: [MoveSpec; 6] = [
            MoveSpec::new(Axis::Y, 1, -1, "U"),
            MoveSpec::new(Axis::Y, -1, 1, "D"),
            MoveSpec::new(Axis::X, 1, -1, "R"),
            MoveSpec::new(Axis::X, -1, 1, "L"),
            MoveSpec::new(Axis::Z, 1, -1, "F"),
            MoveSpec::new(Axis::Z, -1, 1, "B"),
        ];
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let mut spec = MOVES[(self.rng as usize >> 16) % MOVES.len()];
        if self.rng & 1 == 1 {
            spec = spec.inverse();
        }
        spec
    }

    fn apply_move(&mut self, spec: MoveSpec) {
        for cubie in &mut self.cubies {
            if !spec.includes(cubie.pos) {
                continue;
            }
            cubie.pos = cubie.pos.rotate(spec.axis, spec.dir);
            cubie.orientation =
                Mat3::from_axis(spec.axis, spec.dir as f32 * std::f32::consts::FRAC_PI_2)
                    * cubie.orientation;
        }
    }
}

struct SceneObject {
    graphic: RattyGraphic<'static>,
}

impl SceneObject {
    fn new_cube(id: u32) -> Self {
        let settings = RattyGraphicSettings::new(format!("rubiks-{id}.glb"))
            .id(id)
            .format(ObjectFormat::Glb)
            .animate(false)
            .brightness(1.0)
            .depth(0.0);
        Self {
            graphic: RattyGraphic::new(settings),
        }
    }

    fn apply(&mut self, update: SceneUpdate, metrics: &SceneMetrics) {
        let settings = self.graphic.settings_mut();
        settings.animate = false;
        settings.depth = 0.0;
        settings.rotation = update.rotation.to_euler_degrees();
        settings.offset = [
            update.position.x * metrics.unit,
            update.position.y * metrics.unit,
            metrics.unit * BASE_Z_UNITS + update.position.z * metrics.unit,
        ];
        settings.color = None;
        settings.brightness = 1.0;
        settings.scale = metrics.object_scale;
        settings.scale3 = [1.0, 1.0, 1.0];
    }

    fn transform_update_sequence(&self) -> String {
        let settings = self.graphic.settings();
        format!(
            "\x1b_ratty;g;u;id={};animate={};scale={};px={};py={};pz={};rx={};ry={};rz={};sx={};sy={};sz={}\x1b\\",
            settings.id,
            u8::from(settings.animate),
            settings.scale,
            settings.offset[0],
            settings.offset[1],
            settings.offset[2],
            settings.rotation[0],
            settings.rotation[1],
            settings.rotation[2],
            settings.scale3[0],
            settings.scale3[1],
            settings.scale3[2],
        )
    }
}

struct SceneMetrics {
    unit: f32,
    object_scale: f32,
}

impl SceneMetrics {
    fn new(area: Rect, zoom: f32) -> Self {
        let (cell_width, cell_height) = terminal_cell_pixels();
        let base_scale = (area.width.max(1) as f32 * cell_width)
            .max(area.height.max(1) as f32 * cell_height)
            * 0.9;
        let object_scale = BODY_SCALE * zoom;
        Self {
            unit: base_scale * object_scale,
            object_scale,
        }
    }
}

#[derive(Clone, Copy)]
struct SceneUpdate {
    position: Vec3,
    rotation: Mat3,
}

impl SceneUpdate {
    fn new(position: Vec3, rotation: Mat3) -> Self {
        Self { position, rotation }
    }
}

#[derive(Clone)]
struct Cubie {
    pos: Vec3i,
    orientation: Mat3,
    stickers: Vec<Sticker>,
}

impl Cubie {
    fn new(pos: Vec3i, stickers: Vec<Sticker>) -> Self {
        Self {
            pos,
            orientation: Mat3::IDENTITY,
            stickers,
        }
    }
}

#[derive(Clone, Copy)]
struct Sticker {
    normal: Vec3i,
    face: Face,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Face {
    Up,
    Down,
    Front,
    Back,
    Right,
    Left,
}

#[derive(Clone, Copy)]
enum Palette {
    Classic,
    Pastel,
}

impl Palette {
    fn next(self) -> Self {
        match self {
            Self::Classic => Self::Pastel,
            Self::Pastel => Self::Classic,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Pastel => "pastel",
        }
    }

    fn color(self, face: Face) -> [u8; 3] {
        match self {
            Self::Classic => match face {
                Face::Up => [242, 242, 242],
                Face::Down => [255, 213, 0],
                Face::Front => [0, 155, 72],
                Face::Back => [0, 70, 173],
                Face::Right => [183, 18, 52],
                Face::Left => [255, 88, 0],
            },
            Self::Pastel => match face {
                Face::Up => [245, 236, 226],
                Face::Down => [231, 201, 103],
                Face::Front => [142, 204, 161],
                Face::Back => [135, 161, 213],
                Face::Right => [211, 128, 87],
                Face::Left => [190, 139, 183],
            },
        }
    }
}

#[derive(Clone, Copy)]
struct ActiveTurn {
    spec: MoveSpec,
    record: bool,
    elapsed: f32,
    duration: f32,
}

#[derive(Clone, Copy)]
struct QueuedTurn {
    spec: MoveSpec,
    record: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MoveSpec {
    axis: Axis,
    layer: i8,
    dir: i8,
    label: &'static str,
}

impl MoveSpec {
    const fn new(axis: Axis, layer: i8, dir: i8, label: &'static str) -> Self {
        Self {
            axis,
            layer,
            dir,
            label,
        }
    }

    fn from_char(ch: char) -> Option<Self> {
        let inverse = ch.is_ascii_uppercase();
        let base = match ch.to_ascii_lowercase() {
            'u' => Self::new(Axis::Y, 1, -1, "U"),
            'd' => Self::new(Axis::Y, -1, 1, "D"),
            'r' => Self::new(Axis::X, 1, -1, "R"),
            'l' => Self::new(Axis::X, -1, 1, "L"),
            'f' => Self::new(Axis::Z, 1, -1, "F"),
            'b' => Self::new(Axis::Z, -1, 1, "B"),
            _ => return None,
        };
        Some(if inverse { base.inverse() } else { base })
    }

    fn inverse(self) -> Self {
        Self {
            dir: -self.dir,
            label: match self.label {
                "U" => "U'",
                "D" => "D'",
                "R" => "R'",
                "L" => "L'",
                "F" => "F'",
                "B" => "B'",
                "U'" => "U",
                "D'" => "D",
                "R'" => "R",
                "L'" => "L",
                "F'" => "F",
                "B'" => "B",
                _ => self.label,
            },
            ..self
        }
    }

    fn includes(self, pos: Vec3i) -> bool {
        match self.axis {
            Axis::X => pos.x == self.layer,
            Axis::Y => pos.y == self.layer,
            Axis::Z => pos.z == self.layer,
        }
    }

    fn label(self) -> &'static str {
        self.label
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    X,
    Y,
    Z,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Vec3i {
    x: i8,
    y: i8,
    z: i8,
}

impl Vec3i {
    const fn new(x: i8, y: i8, z: i8) -> Self {
        Self { x, y, z }
    }

    fn rotate(self, axis: Axis, dir: i8) -> Self {
        match (axis, dir.signum()) {
            (Axis::X, 1) => Self::new(self.x, -self.z, self.y),
            (Axis::X, -1) => Self::new(self.x, self.z, -self.y),
            (Axis::Y, 1) => Self::new(self.z, self.y, -self.x),
            (Axis::Y, -1) => Self::new(-self.z, self.y, self.x),
            (Axis::Z, 1) => Self::new(-self.y, self.x, self.z),
            (Axis::Z, -1) => Self::new(self.y, -self.x, self.z),
            _ => self,
        }
    }

    fn to_vec3(self) -> Vec3 {
        Vec3::new(self.x as f32, self.y as f32, self.z as f32)
    }
}

#[derive(Clone, Copy, Default)]
struct Vec3 {
    x: f32,
    y: f32,
    z: f32,
}

impl Vec3 {
    const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
}

impl std::ops::Add for Vec3 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl std::ops::Mul<f32> for Vec3 {
    type Output = Self;

    fn mul(self, rhs: f32) -> Self::Output {
        Self::new(self.x * rhs, self.y * rhs, self.z * rhs)
    }
}

#[derive(Clone, Copy)]
struct Mat3 {
    m: [[f32; 3]; 3],
}

impl Mat3 {
    const IDENTITY: Self = Self {
        m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    };

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

    fn rotation_z(angle: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self {
            m: [[cos, -sin, 0.0], [sin, cos, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    fn from_axis(axis: Axis, angle: f32) -> Self {
        match axis {
            Axis::X => Self::rotation_x(angle),
            Axis::Y => Self::rotation_y(angle),
            Axis::Z => Self::rotation_z(angle),
        }
    }

    fn transform_vec(self, vec: Vec3) -> Vec3 {
        Vec3::new(
            self.m[0][0] * vec.x + self.m[0][1] * vec.y + self.m[0][2] * vec.z,
            self.m[1][0] * vec.x + self.m[1][1] * vec.y + self.m[1][2] * vec.z,
            self.m[2][0] * vec.x + self.m[2][1] * vec.y + self.m[2][2] * vec.z,
        )
    }

    fn to_euler_degrees(self) -> [f32; 3] {
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

fn exposed_faces(pos: Vec3i) -> Vec<(Vec3i, Face)> {
    let mut faces = Vec::new();
    if pos.y == 1 {
        faces.push((Vec3i::new(0, 1, 0), Face::Up));
    }
    if pos.y == -1 {
        faces.push((Vec3i::new(0, -1, 0), Face::Down));
    }
    if pos.z == 1 {
        faces.push((Vec3i::new(0, 0, 1), Face::Front));
    }
    if pos.z == -1 {
        faces.push((Vec3i::new(0, 0, -1), Face::Back));
    }
    if pos.x == 1 {
        faces.push((Vec3i::new(1, 0, 0), Face::Right));
    }
    if pos.x == -1 {
        faces.push((Vec3i::new(-1, 0, 0), Face::Left));
    }
    faces
}

fn cube_glb(cubies: &[Cubie], palette: Palette, active: Option<(MoveSpec, Mat3)>) -> Vec<u8> {
    let sticker_count = cubies.iter().map(|cubie| cubie.stickers.len()).sum::<usize>();
    let mut primitives = Vec::with_capacity(cubies.len() + sticker_count);

    for cubie in cubies {
        let turn_matrix = active
            .filter(|(spec, _)| spec.includes(cubie.pos))
            .map(|(_, matrix)| matrix)
            .unwrap_or(Mat3::IDENTITY);
        let center = turn_matrix.transform_vec(cubie.pos.to_vec3() * CUBIE_SPACING);
        let rotation = turn_matrix * cubie.orientation;
        primitives.push(cuboid_primitive(
            center,
            Vec3::new(0.49, 0.49, 0.49),
            0,
            rotation,
        ));

        for sticker in &cubie.stickers {
            let (local_center, half) = sticker_box(sticker.normal);
            primitives.push(cuboid_primitive(
                center + rotation.transform_vec(local_center),
                half,
                sticker.face.material_index(),
                rotation,
            ));
        }
    }

    build_glb(&primitives, palette)
}

fn cube_asset_path(revision: u32, palette: Palette) -> io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("ratty-rubiks-cube");
    fs::create_dir_all(&dir)?;
    Ok(dir.join(format!(
        "rubiks-cube-v4-{}-{revision}.glb",
        palette.name()
    )))
}

#[derive(Clone)]
struct MeshPrimitive {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u16>,
    material: usize,
}

#[derive(Clone, Copy)]
struct PrimitiveAccessors {
    position: usize,
    normal: usize,
    indices: usize,
    material: usize,
}

#[derive(Clone, Copy)]
struct BufferViewInfo {
    offset: usize,
    length: usize,
    target: u32,
}

enum AccessorInfo {
    Positions {
        view: usize,
        count: usize,
        min: [f32; 3],
        max: [f32; 3],
    },
    Normals {
        view: usize,
        count: usize,
    },
    Indices {
        view: usize,
        count: usize,
    },
}

impl Face {
    fn material_index(self) -> usize {
        match self {
            Self::Up => 1,
            Self::Down => 2,
            Self::Front => 3,
            Self::Back => 4,
            Self::Right => 5,
            Self::Left => 6,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Front => "front",
            Self::Back => "back",
            Self::Right => "right",
            Self::Left => "left",
        }
    }
}

fn sticker_box(normal: Vec3i) -> (Vec3, Vec3) {
    let face = 0.34;
    let thickness = 0.018;
    let lift = 0.516;
    match (normal.x, normal.y, normal.z) {
        (1, 0, 0) => (Vec3::new(lift, 0.0, 0.0), Vec3::new(thickness, face, face)),
        (-1, 0, 0) => (Vec3::new(-lift, 0.0, 0.0), Vec3::new(thickness, face, face)),
        (0, 1, 0) => (Vec3::new(0.0, lift, 0.0), Vec3::new(face, thickness, face)),
        (0, -1, 0) => (Vec3::new(0.0, -lift, 0.0), Vec3::new(face, thickness, face)),
        (0, 0, -1) => (Vec3::new(0.0, 0.0, -lift), Vec3::new(face, face, thickness)),
        _ => (Vec3::new(0.0, 0.0, lift), Vec3::new(face, face, thickness)),
    }
}

fn cuboid_primitive(center: Vec3, half: Vec3, material: usize, rotation: Mat3) -> MeshPrimitive {
    let faces = [
        (
            Vec3::new(0.0, 0.0, 1.0),
            [
                Vec3::new(-half.x, -half.y, half.z),
                Vec3::new(half.x, -half.y, half.z),
                Vec3::new(half.x, half.y, half.z),
                Vec3::new(-half.x, half.y, half.z),
            ],
        ),
        (
            Vec3::new(0.0, 0.0, -1.0),
            [
                Vec3::new(half.x, -half.y, -half.z),
                Vec3::new(-half.x, -half.y, -half.z),
                Vec3::new(-half.x, half.y, -half.z),
                Vec3::new(half.x, half.y, -half.z),
            ],
        ),
        (
            Vec3::new(1.0, 0.0, 0.0),
            [
                Vec3::new(half.x, -half.y, half.z),
                Vec3::new(half.x, -half.y, -half.z),
                Vec3::new(half.x, half.y, -half.z),
                Vec3::new(half.x, half.y, half.z),
            ],
        ),
        (
            Vec3::new(-1.0, 0.0, 0.0),
            [
                Vec3::new(-half.x, -half.y, -half.z),
                Vec3::new(-half.x, -half.y, half.z),
                Vec3::new(-half.x, half.y, half.z),
                Vec3::new(-half.x, half.y, -half.z),
            ],
        ),
        (
            Vec3::new(0.0, 1.0, 0.0),
            [
                Vec3::new(-half.x, half.y, half.z),
                Vec3::new(half.x, half.y, half.z),
                Vec3::new(half.x, half.y, -half.z),
                Vec3::new(-half.x, half.y, -half.z),
            ],
        ),
        (
            Vec3::new(0.0, -1.0, 0.0),
            [
                Vec3::new(-half.x, -half.y, -half.z),
                Vec3::new(half.x, -half.y, -half.z),
                Vec3::new(half.x, -half.y, half.z),
                Vec3::new(-half.x, -half.y, half.z),
            ],
        ),
    ];

    let mut positions = Vec::with_capacity(24);
    let mut normals = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (normal, corners) in faces {
        let base = positions.len() as u16;
        for corner in corners {
            positions.push((center + rotation.transform_vec(corner)).to_array());
        }
        let normal = rotation.transform_vec(normal).to_array();
        for _ in 0..4 {
            normals.push(normal);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    MeshPrimitive {
        positions,
        normals,
        indices,
        material,
    }
}

fn build_glb(primitives: &[MeshPrimitive], palette: Palette) -> Vec<u8> {
    let mut bin = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut primitive_accessors = Vec::new();

    for primitive in primitives {
        let (position_view, min, max) =
            push_vec3_buffer(&mut bin, &mut views, &primitive.positions, ARRAY_BUFFER);
        let position_accessor = accessors.len();
        accessors.push(AccessorInfo::Positions {
            view: position_view,
            count: primitive.positions.len(),
            min,
            max,
        });

        let (normal_view, _, _) =
            push_vec3_buffer(&mut bin, &mut views, &primitive.normals, ARRAY_BUFFER);
        let normal_accessor = accessors.len();
        accessors.push(AccessorInfo::Normals {
            view: normal_view,
            count: primitive.normals.len(),
        });

        let index_view = push_u16_buffer(
            &mut bin,
            &mut views,
            &primitive.indices,
            ELEMENT_ARRAY_BUFFER,
        );
        let index_accessor = accessors.len();
        accessors.push(AccessorInfo::Indices {
            view: index_view,
            count: primitive.indices.len(),
        });

        primitive_accessors.push(PrimitiveAccessors {
            position: position_accessor,
            normal: normal_accessor,
            indices: index_accessor,
            material: primitive.material,
        });
    }

    align4(&mut bin, 0);
    let mut json = gltf_json(&views, &accessors, &primitive_accessors, palette).into_bytes();
    align4(&mut json, b' ');

    let total_len = 12 + 8 + json.len() + 8 + bin.len();
    let mut glb = Vec::with_capacity(total_len);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total_len as u32).to_le_bytes());
    glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
    glb.extend_from_slice(&GLB_JSON_CHUNK.to_le_bytes());
    glb.extend_from_slice(&json);
    glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    glb.extend_from_slice(&GLB_BIN_CHUNK.to_le_bytes());
    glb.extend_from_slice(&bin);
    glb
}

fn push_vec3_buffer(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferViewInfo>,
    values: &[[f32; 3]],
    target: u32,
) -> (usize, [f32; 3], [f32; 3]) {
    align4(bin, 0);
    let offset = bin.len();
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for value in values {
        for index in 0..3 {
            min[index] = min[index].min(value[index]);
            max[index] = max[index].max(value[index]);
            bin.extend_from_slice(&value[index].to_le_bytes());
        }
    }
    let length = bin.len() - offset;
    let view = views.len();
    views.push(BufferViewInfo {
        offset,
        length,
        target,
    });
    (view, min, max)
}

fn push_u16_buffer(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferViewInfo>,
    values: &[u16],
    target: u32,
) -> usize {
    align4(bin, 0);
    let offset = bin.len();
    for value in values {
        bin.extend_from_slice(&value.to_le_bytes());
    }
    let length = bin.len() - offset;
    let view = views.len();
    views.push(BufferViewInfo {
        offset,
        length,
        target,
    });
    view
}

fn gltf_json(
    views: &[BufferViewInfo],
    accessors: &[AccessorInfo],
    primitives: &[PrimitiveAccessors],
    palette: Palette,
) -> String {
    let bin_len = views
        .iter()
        .map(|view| view.offset + view.length)
        .max()
        .unwrap_or(0);
    let mut json = String::new();
    json.push_str("{\"asset\":{\"version\":\"2.0\",\"generator\":\"ratty-rubiks-cube\"},");
    json.push_str("\"scene\":0,\"scenes\":[{\"nodes\":[0]}],\"nodes\":[{\"mesh\":0}],");
    json.push_str("\"meshes\":[{\"primitives\":[");
    for (index, primitive) in primitives.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "{{\"attributes\":{{\"POSITION\":{},\"NORMAL\":{}}},\"indices\":{},\"material\":{}}}",
            primitive.position, primitive.normal, primitive.indices, primitive.material
        ));
    }
    json.push_str("]}],\"materials\":[");
    for (index, material) in material_defs(palette).iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&material_json(material));
    }
    json.push_str("],\"buffers\":[{\"byteLength\":");
    json.push_str(&bin_len.to_string());
    json.push_str("}],\"bufferViews\":[");
    for (index, view) in views.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "{{\"buffer\":0,\"byteOffset\":{},\"byteLength\":{},\"target\":{}}}",
            view.offset, view.length, view.target
        ));
    }
    json.push_str("],\"accessors\":[");
    for (index, accessor) in accessors.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&accessor_json(accessor));
    }
    json.push_str("]}");
    json
}

struct MaterialDef {
    name: &'static str,
    color: [u8; 3],
    roughness: f32,
}

fn material_defs(palette: Palette) -> [MaterialDef; 7] {
    [
        MaterialDef {
            name: "body",
            color: [14, 15, 18],
            roughness: 0.68,
        },
        face_material(Face::Up, palette),
        face_material(Face::Down, palette),
        face_material(Face::Front, palette),
        face_material(Face::Back, palette),
        face_material(Face::Right, palette),
        face_material(Face::Left, palette),
    ]
}

fn face_material(face: Face, palette: Palette) -> MaterialDef {
    MaterialDef {
        name: face.name(),
        color: palette.color(face),
        roughness: 0.46,
    }
}

fn material_json(material: &MaterialDef) -> String {
    let [r, g, b] = material.color;
    format!(
        "{{\"name\":\"{}\",\"doubleSided\":true,\"pbrMetallicRoughness\":{{\"baseColorFactor\":[{:.4},{:.4},{:.4},1.0],\"metallicFactor\":0.0,\"roughnessFactor\":{:.3}}}}}",
        material.name,
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        material.roughness,
    )
}

fn accessor_json(accessor: &AccessorInfo) -> String {
    match accessor {
        AccessorInfo::Positions {
            view,
            count,
            min,
            max,
        } => format!(
            "{{\"bufferView\":{},\"componentType\":{},\"count\":{},\"type\":\"VEC3\",\"min\":[{:.4},{:.4},{:.4}],\"max\":[{:.4},{:.4},{:.4}]}}",
            view, COMPONENT_F32, count, min[0], min[1], min[2], max[0], max[1], max[2],
        ),
        AccessorInfo::Normals { view, count } => format!(
            "{{\"bufferView\":{},\"componentType\":{},\"count\":{},\"type\":\"VEC3\"}}",
            view, COMPONENT_F32, count
        ),
        AccessorInfo::Indices { view, count } => format!(
            "{{\"bufferView\":{},\"componentType\":{},\"count\":{},\"type\":\"SCALAR\"}}",
            view, COMPONENT_U16, count
        ),
    }
}

fn align4(bytes: &mut Vec<u8>, fill: u8) {
    while bytes.len() % 4 != 0 {
        bytes.push(fill);
    }
}

fn terminal_cell_pixels() -> (f32, f32) {
    let Ok(size) = window_size() else {
        return (9.0, 18.0);
    };
    let width = if size.columns > 0 && size.width > 0 {
        f32::from(size.width) / f32::from(size.columns)
    } else {
        9.0
    };
    let height = if size.rows > 0 && size.height > 0 {
        f32::from(size.height) / f32::from(size.rows)
    } else {
        18.0
    };
    (width.max(1.0), height.max(1.0))
}

fn centered_cube_area(bounds: Rect) -> Rect {
    if bounds.is_empty() {
        return bounds;
    }
    let width = bounds.width.saturating_sub(2).min(56).max(1);
    let height = bounds.height.saturating_sub(2).min(24).max(1);
    bounds.centered(Constraint::Length(width), Constraint::Length(height))
}

fn contains(area: Rect, position: Position) -> bool {
    position.x >= area.x
        && position.x < area.x.saturating_add(area.width)
        && position.y >= area.y
        && position.y < area.y.saturating_add(area.height)
}

fn emit_sequence(buf: &mut Buffer, x: u16, y: u16, sequence: &str) {
    let Some(cell) = buf.cell_mut((x, y)) else {
        return;
    };
    let existing = cell.symbol();
    let mut symbol = String::with_capacity(sequence.len() + existing.len());
    symbol.push_str(sequence);
    symbol.push_str(existing);
    cell.set_symbol(&symbol);
}

fn clear_demo_objects() -> io::Result<()> {
    let mut stdout = io::stdout();
    for id in 900..980 {
        stdout.write_all(format!("\x1b_ratty;g;d;id={id}\x1b\\").as_bytes())?;
    }
    stdout.flush()
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}
