use std::{
    borrow::Cow,
    collections::VecDeque,
    fs, io,
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
const BODY_SCALE: f32 = 0.10;
const CUBIE_SPACING: f32 = 1.04;
const BASE_Z_UNITS: f32 = 2.7;

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
    cube: RubiksCube,
    object: SceneObject,
    view: SceneView,
    viewport: Rect,
    drag_start: Option<Position>,
    should_quit: bool,
}

impl RubiksApp {
    fn new() -> io::Result<Self> {
        RattyGraphic::clear_all()?;
        let mut app = Self {
            cube: RubiksCube::new(),
            object: SceneObject::new_cube(900),
            view: SceneView::new(),
            viewport: Rect::default(),
            drag_start: None,
            should_quit: false,
        };
        app.object.register_cube(&app.cube)?;
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

    fn clear(&self) -> io::Result<()> {
        self.object.clear()
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
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::raw(": quit"),
        ]))
        .block(Block::bordered().title(Span::styled(
            "Ratty Rubik's Cube",
            Style::default().fg(Color::Yellow),
        )))
        .render(header, frame.buffer_mut());

        let block = Block::bordered()
            .title(self.status())
            .border_style(Style::default().fg(Color::White));
        self.viewport = block.inner(body);
        block.render(body, frame.buffer_mut());
        self.paint_backdrop(frame.buffer_mut());

        let cube_area = self.view.cube_area(self.viewport);
        self.sync_scene_objects(cube_area);
        self.emit_rgp_sequences(frame.buffer_mut(), cube_area);
    }

    fn status(&self) -> String {
        format!(
            "3D cube | cubies: {} | move: {active} | queued: {} | zoom: {:.2}",
            self.cube.cubie_count(),
            self.cube.queued_count(),
            self.view.zoom,
            active = self.cube.active_label(),
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

        let place_objects = self.view.placed_area != Some(area);
        emit_sequence(buf, area.x, area.y, &self.object.graphic.update_sequence());
        if place_objects {
            emit_sequence(
                buf,
                area.x,
                area.y,
                &self.object.graphic.place_sequence(area),
            );
        }

        if place_objects {
            self.view.placed_area = Some(area);
        }
    }

    fn sync_scene_objects(&mut self, area: Rect) {
        self.object.apply(
            self.view.rotation(),
            &SceneMetrics::new(area, self.view.zoom),
        );
    }

    fn handle_event(&mut self, event: Event) -> io::Result<()> {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Resize(_, _) => {
                self.view.placed_area = None;
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
            KeyCode::Char(' ') => self.cube.queue_scramble(),
            KeyCode::Enter => self.cube.queue_solve(),
            KeyCode::Backspace => self.cube.queue_undo(),
            KeyCode::Char('0') => {
                self.cube.reset();
                self.view.placed_area = None;
                let _ = self.object.register_cube(&self.cube);
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.view.zoom += 0.06;
            }
            KeyCode::Char('-') => {
                self.view.zoom -= 0.06;
            }
            KeyCode::Left => self.view.yaw -= 0.12,
            KeyCode::Right => self.view.yaw += 0.12,
            KeyCode::Up => self.view.pitch = (self.view.pitch - 0.10).max(-1.35),
            KeyCode::Down => self.view.pitch = (self.view.pitch + 0.10).min(1.35),
            KeyCode::Char(ch) => {
                if let Some(spec) = MoveSpec::from_char(ch) {
                    self.cube.queue_turn(spec, true);
                }
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let pos = Position::new(mouse.column, mouse.row);
        let inside = pos.x >= self.viewport.x
            && pos.x < self.viewport.right()
            && pos.y >= self.viewport.y
            && pos.y < self.viewport.bottom();
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
                self.view.yaw += dx as f32 * 0.035;
                self.view.pitch = (self.view.pitch + dy as f32 * 0.035).clamp(-1.35, 1.35);
                self.drag_start = Some(pos);
            }
            MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                self.drag_start = None;
            }
            MouseEventKind::ScrollUp if inside => {
                self.view.zoom += 0.05;
            }
            MouseEventKind::ScrollDown if inside => {
                self.view.zoom -= 0.05;
            }
            _ => {}
        }
    }

    fn tick(&mut self, delta: f32) {
        if self.cube.tick(delta) {
            let _ = self.object.register_cube(&self.cube);
        }
    }
}

struct RubiksCube {
    cubies: Vec<Cubie>,
    active_turn: Option<ActiveTurn>,
    queued_turns: VecDeque<QueuedTurn>,
    history: Vec<MoveSpec>,
    rng: u32,
}

impl RubiksCube {
    fn new() -> Self {
        let mut cube = Self {
            cubies: Vec::new(),
            active_turn: None,
            queued_turns: VecDeque::new(),
            history: Vec::new(),
            rng: 0x516f_6f74,
        };
        cube.reset();
        cube
    }

    fn reset(&mut self) {
        self.cubies.clear();
        self.active_turn = None;
        self.queued_turns.clear();
        self.history.clear();

        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    if x != 0 || y != 0 || z != 0 {
                        self.cubies.push(Cubie::new(Vec3i::new(x, y, z)));
                    }
                }
            }
        }
    }

    fn cubie_count(&self) -> usize {
        self.cubies.len()
    }

    fn queued_count(&self) -> usize {
        self.queued_turns.len()
    }

    fn active_label(&self) -> &'static str {
        self.active_turn
            .as_ref()
            .map(|turn| turn.spec.label)
            .unwrap_or("idle")
    }

    fn active_transform(&self) -> Option<(MoveSpec, Mat3)> {
        self.active_turn.as_ref().map(|turn| {
            let t = (turn.elapsed / turn.duration).clamp(0.0, 1.0);
            let progress = t * t * (3.0 - 2.0 * t);
            (
                turn.spec,
                Mat3::from_axis(
                    turn.spec.axis,
                    turn.spec.dir as f32 * progress * std::f32::consts::FRAC_PI_2,
                ),
            )
        })
    }

    fn tick(&mut self, delta: f32) -> bool {
        let mut changed = false;
        if self.active_turn.is_none()
            && let Some(queued) = self.queued_turns.pop_front()
        {
            self.active_turn = Some(ActiveTurn {
                spec: queued.spec,
                record: queued.record,
                elapsed: 0.0,
                duration: TURN_DURATION,
            });
            changed = true;
        }

        let Some(turn) = self.active_turn.as_mut() else {
            return changed;
        };
        turn.elapsed += delta;
        if turn.elapsed < turn.duration {
            return true;
        }

        let completed = *turn;
        self.apply_move(completed.spec);
        if completed.record {
            self.history.push(completed.spec);
        }
        self.active_turn = None;
        true
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

struct SceneView {
    yaw: f32,
    pitch: f32,
    roll: f32,
    zoom: f32,
    placed_area: Option<Rect>,
}

impl SceneView {
    fn new() -> Self {
        Self {
            yaw: -0.58,
            pitch: -0.42,
            roll: 0.0,
            zoom: 1.0,
            placed_area: None,
        }
    }

    fn rotation(&self) -> Mat3 {
        Mat3::rotation_x(self.pitch) * Mat3::rotation_y(self.yaw) * Mat3::rotation_z(self.roll)
    }

    fn cube_area(&self, viewport: Rect) -> Rect {
        if viewport.is_empty() {
            return viewport;
        }
        let width = viewport.width.saturating_sub(2).clamp(1, 56);
        let height = viewport.height.saturating_sub(2).clamp(1, 24);
        viewport.centered(Constraint::Length(width), Constraint::Length(height))
    }
}

struct SceneObject {
    graphic: RattyGraphic<'static>,
    model_revision: u32,
}

impl SceneObject {
    fn new_cube(id: u32) -> Self {
        let settings = RattyGraphicSettings::new(format!("rubiks-{id}.obj"))
            .id(id)
            .format(ObjectFormat::Obj)
            .normalize(false)
            .animate(false)
            .brightness(1.0)
            .depth(0.0);
        Self {
            graphic: RattyGraphic::new(settings),
            model_revision: 0,
        }
    }

    fn register_cube(&mut self, cube: &RubiksCube) -> io::Result<()> {
        self.model_revision = self.model_revision.wrapping_add(1);
        let path = self.asset_path()?;
        fs::write(&path, CubeObj::from_cube(cube).into_bytes())?;
        self.graphic.settings_mut().path = Cow::Owned(path.to_string_lossy().into_owned());
        self.graphic.register()
    }

    fn apply(&mut self, rotation: Mat3, metrics: &SceneMetrics) {
        let settings = self.graphic.settings_mut();
        settings.animate = false;
        settings.depth = 0.0;
        settings.rotation = rotation.to_euler_degrees();
        settings.offset = [0.0, 0.0, metrics.unit * BASE_Z_UNITS];
        settings.color = None;
        settings.brightness = 1.0;
        settings.scale = metrics.object_scale;
        settings.scale3 = [1.0, 1.0, 1.0];
    }

    fn clear(&self) -> io::Result<()> {
        self.graphic.clear()
    }

    fn asset_path(&self) -> io::Result<PathBuf> {
        let dir = std::env::temp_dir().join("ratty-rubiks-cube");
        fs::create_dir_all(&dir)?;
        Ok(dir.join(format!("rubiks-cube-v5-{}.obj", self.model_revision)))
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

#[derive(Clone)]
struct Cubie {
    pos: Vec3i,
    orientation: Mat3,
    stickers: Vec<Sticker>,
}

impl Cubie {
    fn new(pos: Vec3i) -> Self {
        Self {
            pos,
            orientation: Mat3::IDENTITY,
            stickers: Sticker::for_cubie(pos),
        }
    }

    fn primitives(&self, active: Option<(MoveSpec, Mat3)>) -> Vec<MeshPrimitive> {
        let turn_matrix = active
            .filter(|(spec, _)| spec.includes(self.pos))
            .map(|(_, matrix)| matrix)
            .unwrap_or(Mat3::IDENTITY);
        let center = turn_matrix.transform_vec(self.pos.to_vec3() * CUBIE_SPACING);
        let rotation = turn_matrix * self.orientation;
        let mut primitives = Vec::with_capacity(self.stickers.len() + 1);
        primitives.push(MeshPrimitive::cuboid(
            center,
            Vec3::new(0.49, 0.49, 0.49),
            [14, 15, 18],
            rotation,
        ));
        primitives.extend(
            self.stickers
                .iter()
                .map(|sticker| sticker.primitive(center, rotation)),
        );
        primitives
    }
}

#[derive(Clone, Copy)]
struct Sticker {
    normal: Vec3i,
    face: Face,
}

impl Sticker {
    fn new(normal: Vec3i, face: Face) -> Self {
        Self { normal, face }
    }

    fn for_cubie(pos: Vec3i) -> Vec<Self> {
        let mut stickers = Vec::new();
        if pos.y == 1 {
            stickers.push(Self::new(Vec3i::new(0, 1, 0), Face::Up));
        }
        if pos.y == -1 {
            stickers.push(Self::new(Vec3i::new(0, -1, 0), Face::Down));
        }
        if pos.z == 1 {
            stickers.push(Self::new(Vec3i::new(0, 0, 1), Face::Front));
        }
        if pos.z == -1 {
            stickers.push(Self::new(Vec3i::new(0, 0, -1), Face::Back));
        }
        if pos.x == 1 {
            stickers.push(Self::new(Vec3i::new(1, 0, 0), Face::Right));
        }
        if pos.x == -1 {
            stickers.push(Self::new(Vec3i::new(-1, 0, 0), Face::Left));
        }
        stickers
    }

    fn primitive(self, cubie_center: Vec3, cubie_rotation: Mat3) -> MeshPrimitive {
        let (local_center, half) = self.geometry();
        MeshPrimitive::cuboid(
            cubie_center + cubie_rotation.transform_vec(local_center),
            half,
            self.face.color(),
            cubie_rotation,
        )
    }

    fn geometry(self) -> (Vec3, Vec3) {
        let face = 0.34;
        let thickness = 0.018;
        let lift = 0.516;
        match (self.normal.x, self.normal.y, self.normal.z) {
            (1, 0, 0) => (Vec3::new(lift, 0.0, 0.0), Vec3::new(thickness, face, face)),
            (-1, 0, 0) => (Vec3::new(-lift, 0.0, 0.0), Vec3::new(thickness, face, face)),
            (0, 1, 0) => (Vec3::new(0.0, lift, 0.0), Vec3::new(face, thickness, face)),
            (0, -1, 0) => (Vec3::new(0.0, -lift, 0.0), Vec3::new(face, thickness, face)),
            (0, 0, -1) => (Vec3::new(0.0, 0.0, -lift), Vec3::new(face, face, thickness)),
            _ => (Vec3::new(0.0, 0.0, lift), Vec3::new(face, face, thickness)),
        }
    }
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

impl Face {
    fn color(self) -> [u8; 3] {
        match self {
            Self::Up => [242, 242, 242],
            Self::Down => [255, 213, 0],
            Self::Front => [0, 155, 72],
            Self::Back => [0, 70, 173],
            Self::Right => [183, 18, 52],
            Self::Left => [255, 88, 0],
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

struct CubeObj {
    primitives: Vec<MeshPrimitive>,
}

impl CubeObj {
    fn from_cube(cube: &RubiksCube) -> Self {
        let sticker_count = cube
            .cubies
            .iter()
            .map(|cubie| cubie.stickers.len())
            .sum::<usize>();
        let mut primitives = Vec::with_capacity(cube.cubies.len() + sticker_count);
        for cubie in &cube.cubies {
            primitives.extend(cubie.primitives(cube.active_transform()));
        }
        Self { primitives }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.to_obj_string().into_bytes()
    }

    fn to_obj_string(&self) -> String {
        let mut obj = String::from("# ratty-rubiks-cube\n");
        let mut vertex_offset = 1usize;
        let mut normal_offset = 1usize;
        for primitive in &self.primitives {
            primitive.write_obj(&mut obj, vertex_offset, normal_offset);
            vertex_offset += primitive.positions.len();
            normal_offset += primitive.normals.len();
        }
        obj
    }
}

#[derive(Clone)]
struct MeshPrimitive {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[u8; 3]>,
    indices: Vec<u16>,
}

impl MeshPrimitive {
    fn cuboid(center: Vec3, half: Vec3, color: [u8; 3], rotation: Mat3) -> Self {
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
        let mut colors = Vec::with_capacity(24);
        let mut indices = Vec::with_capacity(36);
        for (normal, corners) in faces {
            let base = positions.len() as u16;
            for corner in corners {
                positions.push((center + rotation.transform_vec(corner)).to_array());
                colors.push(color);
            }
            let normal = rotation.transform_vec(normal).to_array();
            for _ in 0..4 {
                normals.push(normal);
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }

        Self {
            positions,
            normals,
            colors,
            indices,
        }
    }

    fn write_obj(&self, obj: &mut String, vertex_offset: usize, normal_offset: usize) {
        for (position, color) in self.positions.iter().zip(&self.colors) {
            let [r, g, b] = *color;
            obj.push_str(&format!(
                "v {:.5} {:.5} {:.5} {:.5} {:.5} {:.5}\n",
                position[0],
                position[1],
                position[2],
                f32::from(r) / 255.0,
                f32::from(g) / 255.0,
                f32::from(b) / 255.0,
            ));
        }
        for normal in &self.normals {
            obj.push_str(&format!(
                "vn {:.5} {:.5} {:.5}\n",
                normal[0], normal[1], normal[2],
            ));
        }
        for triangle in self.indices.chunks_exact(3) {
            let a = usize::from(triangle[0]);
            let b = usize::from(triangle[1]);
            let c = usize::from(triangle[2]);
            obj.push_str(&format!(
                "f {}//{} {}//{} {}//{}\n",
                vertex_offset + a,
                normal_offset + a,
                vertex_offset + b,
                normal_offset + b,
                vertex_offset + c,
                normal_offset + c,
            ));
        }
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
