//! Inline object state and APC handling.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use bevy::prelude::*;

use crate::camera::{OptionalVec3, TerminalCameraUpdate};
use crate::kitty::{KittyOperation, KittyParserState, refresh_kitty_placeholder_anchors};
use crate::model::{
    ObjectLoadOptions, load_object_source_from_bytes_with_options,
    load_rgp_object_source_with_options,
};
use crate::rgp::{
    RgpOperation, RgpPlacementStyle, RgpPlacementUpdate, RgpRegisterSource,
    consume_sequence as consume_rgp_sequence, support_reply,
};
use crate::runtime::TerminalRuntime;
use crate::screen::ScreenView;

const APC_START: &[u8] = b"\x1b_";
const ITERM_START: &[u8] = b"\x1b]1337;File=";
const CLEAR_SCREEN: &[u8] = b"\x1b[2J";
const SYNC_UPDATE_END: &[u8] = b"\x1b[?2026l";
const ST: &[u8] = b"\x1b\\";
const C1_ST: u8 = 0x9c;
const MAX_QUEUED_ITERM_FRAMES: usize = 4;

/// Marker for 2D inline object sprites.
#[derive(Component)]
pub struct TerminalInlineObjectSprite;

/// Marker for 3D inline object planes.
#[derive(Component)]
pub struct TerminalInlineObjectPlane;

/// Layout data used to animate Kitty image planes on the warped terminal surface.
#[derive(Component, Clone, Copy)]
pub(crate) struct InlineKittyPlaneLayout {
    /// Normalized horizontal center within the terminal plane.
    pub local_x: f32,
    /// Normalized vertical center within the terminal plane.
    pub local_y: f32,
    /// Normalized width within the terminal plane.
    pub local_width: f32,
    /// Normalized height within the terminal plane.
    pub local_height: f32,
    /// Horizontal mesh subdivision count.
    pub x_segments: u32,
    /// Vertical mesh subdivision count.
    pub y_segments: u32,
}

/// Cached GPU assets for a Kitty image plane attached to the terminal surface.
pub(crate) struct KittyPlaneCache {
    /// Cached horizontal mesh subdivision count.
    pub x_segments: u32,
    /// Cached vertical mesh subdivision count.
    pub y_segments: u32,
    /// Cached plane mesh handle.
    pub mesh: Handle<Mesh>,
    /// Cached plane material handle.
    pub material: Handle<StandardMaterial>,
}

/// Marker for RGP-backed inline objects.
#[derive(Component)]
pub struct TerminalRgpObject {
    /// Registered object identifier.
    pub object_id: u32,
}

/// Inline object registry and anchor state.
#[derive(Resource, Default)]
pub struct TerminalInlineObjects {
    pending_bytes: Vec<u8>,
    pending_control: Option<PendingControl>,
    pending_screen_clear: Option<PendingScreenClear>,
    pending_iterm_images: Mutex<Vec<PendingItermImage>>,
    iterm_image_cache: HashMap<u64, u32>,
    screen_generation: u64,
    pending_rgp_payloads: HashMap<u32, PendingRgpPayload>,
    kitty: KittyParserState,
    dirty: bool,
    last_viewport_size: Vec2,
    last_cols: u16,
    last_rows: u16,
    pub(crate) objects: HashMap<u32, InlineObject>,
    pub(crate) anchors: HashMap<u32, InlineAnchor>,
}

struct PendingScreenClear {
    previous_ids: Vec<u32>,
    completed_at: Option<Instant>,
}

impl TerminalInlineObjects {
    /// Consumes PTY output and extracts inline object control sequences.
    pub fn consume_pty_output(
        &mut self,
        chunk: &[u8],
        runtime: &mut TerminalRuntime,
        camera_updates: &mut Vec<TerminalCameraUpdate>,
        terminal_output: &mut bool,
    ) -> Vec<Vec<u8>> {
        self.pending_bytes.extend_from_slice(chunk);
        let mut replies = Vec::new();

        let mut cursor = 0;
        loop {
            if let Some(mut pending) = self.pending_control.take() {
                if pending.is_iterm && !pending.stream_checked {
                    self.start_iterm_gif_stream(&mut pending, runtime.screen().cursor_position());
                }
                let payload_start = if pending.is_iterm {
                    ITERM_START.len()
                } else {
                    APC_START.len()
                };
                let Some(end) = apc_end_from(&self.pending_bytes, payload_start, pending.scan_from)
                else {
                    if pending.stream_sender.is_some() {
                        let safe_end = self.pending_bytes.len().saturating_sub(1);
                        self.send_iterm_stream_bytes(&mut pending, safe_end);
                    }
                    // Preserve the sequence and resume at its unfinished tail
                    // next time. Scanning the whole accumulated image payload
                    // on every 16 KiB PTY chunk made large GIFs quadratic to
                    // parse before decoding even started.
                    pending.scan_from = self.pending_bytes.len().saturating_sub(1);
                    self.pending_control = Some(pending);
                    return replies;
                };
                if pending.is_iterm {
                    if pending.stream_sender.is_some() {
                        let terminator_start = if self.pending_bytes[end - 2..end] == *ST {
                            end - 2
                        } else {
                            end - 1
                        };
                        self.send_iterm_stream_bytes(&mut pending, terminator_start);
                        pending.stream_sender = None;
                        if let (Some(object_id), Some((header, encoded))) = (
                            pending.stream_object_id,
                            parse_iterm_sequence_parts(&self.pending_bytes[..end]),
                        ) {
                            let key =
                                iterm_image_cache_key(&header, &encoded, pending.stream_cursor);
                            if self.reuse_cached_iterm_image(key, &header, pending.stream_cursor) {
                                self.cancel_pending_iterm_image(object_id);
                            } else {
                                self.set_pending_iterm_image_key(object_id, key);
                            }
                        }
                    } else if let Some((header, encoded)) =
                        parse_iterm_sequence(&self.pending_bytes[..end])
                    {
                        self.queue_iterm_image(header, encoded, runtime.screen().cursor_position());
                    }
                } else {
                    let sequence = self.pending_bytes[..end].to_vec();
                    let (handled, reply) = self.handle_apc_sequence(
                        &sequence,
                        runtime.screen().cursor_position(),
                        runtime.local_file_root(),
                        camera_updates,
                    );
                    if let Some(reply) = reply {
                        replies.push(reply);
                    }
                    if !handled {
                        *terminal_output = true;
                        runtime.process(&sequence);
                    }
                }
                cursor = end;
                continue;
            }

            let apc = self.pending_bytes[cursor..]
                .windows(APC_START.len())
                .position(|window| window == APC_START)
                .map(|offset| (offset, 0));
            let iterm = self.pending_bytes[cursor..]
                .windows(ITERM_START.len())
                .position(|window| window == ITERM_START)
                .map(|offset| (offset, 1));
            let clear_screen = self.pending_bytes[cursor..]
                .windows(CLEAR_SCREEN.len())
                .position(|window| window == CLEAR_SCREEN)
                .map(|offset| (offset, 2));
            let redraw_complete = self.pending_bytes[cursor..]
                .windows(SYNC_UPDATE_END.len())
                .position(|window| window == SYNC_UPDATE_END)
                .map(|offset| (offset, 3));
            let Some((start_offset, kind)) = [apc, iterm, clear_screen, redraw_complete]
                .into_iter()
                .flatten()
                .min_by_key(|(offset, _)| *offset)
            else {
                let pending_len = self.pending_bytes.len();
                let keep_from = pending_apc_prefix_start(&self.pending_bytes, cursor);
                if cursor < keep_from {
                    *terminal_output = true;
                    runtime.process(&self.pending_bytes[cursor..keep_from]);
                }
                if keep_from < pending_len {
                    self.pending_bytes.drain(..keep_from);
                } else {
                    self.pending_bytes.clear();
                }
                return replies;
            };
            let start = cursor + start_offset;
            if cursor < start {
                *terminal_output = true;
                runtime.process(&self.pending_bytes[cursor..start]);
            }

            if kind == 2 {
                let sequence = CLEAR_SCREEN;
                for image in self.pending_iterm_images.lock().unwrap().iter() {
                    // Keep decoders for already placed images alive until the
                    // redraw tells us whether the same image is being reused.
                    // Cached GIFs have looping workers, and pausing a slide
                    // redraws it with the same payload.
                    if !self.objects.contains_key(&image.object_id) {
                        image.cancelled.store(true, Ordering::Relaxed);
                    }
                }
                self.screen_generation = self.screen_generation.wrapping_add(1);
                // A second clear means the terminal has started another full
                // redraw. Refresh the snapshot so placements added since the
                // previous clear are retired with this redraw as well.
                if !self.anchors.is_empty() || self.pending_screen_clear.is_some() {
                    let mut previous_ids = self
                        .pending_screen_clear
                        .take()
                        .map(|pending| pending.previous_ids)
                        .unwrap_or_default();
                    previous_ids.extend(self.anchors.keys().copied());
                    previous_ids.sort_unstable();
                    previous_ids.dedup();
                    self.pending_screen_clear = Some(PendingScreenClear {
                        previous_ids,
                        completed_at: None,
                    });
                }
                *terminal_output = true;
                runtime.process(sequence);
                cursor = start + sequence.len();
                continue;
            }
            if kind == 3 {
                if let Some(pending) = self.pending_screen_clear.as_mut() {
                    pending.completed_at = Some(Instant::now());
                }
                *terminal_output = true;
                runtime.process(SYNC_UPDATE_END);
                cursor = start + SYNC_UPDATE_END.len();
                continue;
            }
            let is_iterm = kind == 1;
            let payload_start = start
                + if is_iterm {
                    ITERM_START.len()
                } else {
                    APC_START.len()
                };
            let Some(end) = apc_end_from(&self.pending_bytes, payload_start, payload_start) else {
                self.pending_bytes.drain(..start);
                self.pending_control = Some(PendingControl {
                    is_iterm,
                    scan_from: self.pending_bytes.len().saturating_sub(1),
                    payload_start: 0,
                    stream_sent_until: 0,
                    stream_sender: None,
                    stream_object_id: None,
                    stream_cursor: runtime.screen().cursor_position(),
                    stream_checked: false,
                });
                return replies;
            };
            if is_iterm {
                if let Some((header, encoded)) =
                    parse_iterm_sequence(&self.pending_bytes[start..end])
                {
                    self.queue_iterm_image(header, encoded, runtime.screen().cursor_position());
                }
                cursor = end;
                continue;
            }
            let sequence = self.pending_bytes[start..end].to_vec();
            let (handled, reply) = self.handle_apc_sequence(
                &sequence,
                runtime.screen().cursor_position(),
                runtime.local_file_root(),
                camera_updates,
            );
            if let Some(reply) = reply {
                replies.push(reply);
            }
            if !handled {
                *terminal_output = true;
                runtime.process(&sequence);
            }
            cursor = end;
        }
    }

    /// Applies CSI 2J cleanup after Presenterm ends its synchronized redraw.
    pub(crate) fn finish_screen_redraw(&mut self) {
        self.poll_pending_iterm_images();
        let Some(pending) = self.pending_screen_clear.take() else {
            return;
        };
        let Some(completed_at) = pending.completed_at else {
            self.pending_screen_clear = Some(pending);
            return;
        };
        // Keep the previous image while the replacement's first frame is being
        // decoded, but do not let a broken image hold the old slide forever.
        let waiting_for_replacement =
            self.pending_iterm_images
                .lock()
                .unwrap()
                .iter()
                .any(|image| {
                    image.generation == self.screen_generation
                        && !image.cancelled.load(Ordering::Relaxed)
                        && !self.anchors.contains_key(&image.object_id)
                });
        if waiting_for_replacement && completed_at.elapsed() < Duration::from_secs(2) {
            self.pending_screen_clear = Some(pending);
            return;
        }
        let mut removed = false;
        for object_id in &pending.previous_ids {
            if self.anchors.contains_key(object_id) || self.objects.contains_key(object_id) {
                self.remove_object(*object_id);
                removed = true;
            }
        }
        if removed {
            self.dirty = true;
            bevy::log::debug!(
                "cleared inline placements after screen content changed: {:?}",
                pending.previous_ids
            );
        }
    }

    fn queue_iterm_image(&mut self, header: String, encoded: Vec<u8>, cursor: (u16, u16)) {
        if encoded.len() > 90_000_000 {
            return;
        }
        let cache_key = iterm_image_cache_key(&header, &encoded, cursor);
        if self.reuse_cached_iterm_image(cache_key, &header, cursor) {
            bevy::log::debug!(
                "reused decoded iTerm2 image at row {}, column {}",
                cursor.0,
                cursor.1
            );
            return;
        }
        let mut pending = self.pending_iterm_images.lock().unwrap();
        let object_id = self
            .objects
            .keys()
            .copied()
            .chain(pending.iter().map(|image| image.object_id))
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let (sender, receiver) = mpsc::sync_channel(2);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let generation = self.screen_generation;
        if std::thread::Builder::new()
            .name(format!("ratty-iterm-image-{object_id}"))
            .spawn(move || {
                decode_iterm_image(&encoded, &sender, &worker_cancelled, false);
            })
            .is_err()
        {
            bevy::log::warn!("could not start background decode for iTerm2 image {object_id}");
            return;
        }
        pending.push(PendingItermImage {
            object_id,
            cursor,
            header,
            generation,
            cache_key: Some(cache_key),
            defer_events: false,
            buffered_events: Vec::new(),
            cancelled,
            receiver,
        });
        bevy::log::debug!("queued iTerm2 inline image {object_id} for background decoding");
    }

    fn reuse_cached_iterm_image(
        &mut self,
        cache_key: u64,
        header: &str,
        cursor: (u16, u16),
    ) -> bool {
        let Some(&object_id) = self.iterm_image_cache.get(&cache_key) else {
            return false;
        };
        let Some(InlineObject::KittyImage(image)) = self.objects.get(&object_id) else {
            self.iterm_image_cache.remove(&cache_key);
            return false;
        };
        if let Some(pending_image) = self
            .pending_iterm_images
            .lock()
            .unwrap()
            .iter_mut()
            .find(|pending_image| pending_image.object_id == object_id)
        {
            pending_image.generation = self.screen_generation;
            pending_image.cancelled.store(false, Ordering::Relaxed);
        }
        if let Some(pending) = self.pending_screen_clear.as_mut() {
            pending
                .previous_ids
                .retain(|previous_id| *previous_id != object_id);
        }
        let (columns, rows) = iterm_cells(header, image.raster.width, image.raster.height);
        self.set_anchor(
            object_id,
            InlineAnchor {
                row: cursor.0,
                col: cursor.1,
                columns: u32::from(columns),
                rows: u32::from(rows),
                style: InlineStyle::default(),
            },
        );
        true
    }

    fn set_pending_iterm_image_key(&mut self, object_id: u32, cache_key: u64) {
        self.iterm_image_cache.insert(cache_key, object_id);
        if let Some(InlineObject::KittyImage(_)) = self.objects.get(&object_id) {
            return;
        }
        if let Some(image) = self
            .pending_iterm_images
            .lock()
            .unwrap()
            .iter_mut()
            .find(|image| image.object_id == object_id)
        {
            image.cache_key = Some(cache_key);
        }
    }

    fn cancel_pending_iterm_image(&mut self, object_id: u32) {
        self.pending_iterm_images.lock().unwrap().retain(|image| {
            if image.object_id == object_id {
                image.cancelled.store(true, Ordering::Relaxed);
                false
            } else {
                true
            }
        });
    }

    fn start_iterm_gif_stream(&mut self, control: &mut PendingControl, cursor: (u16, u16)) {
        use base64::Engine as _;

        let header_start = ITERM_START.len();
        let Some(header_end) = self.pending_bytes[header_start..]
            .iter()
            .position(|byte| *byte == b':')
            .map(|offset| header_start + offset)
        else {
            return;
        };
        let payload_start = header_end + 1;
        if self.pending_bytes.len() < payload_start + 8 {
            return;
        }
        control.stream_checked = true;
        let Ok(magic) = base64::engine::general_purpose::STANDARD
            .decode(&self.pending_bytes[payload_start..payload_start + 8])
        else {
            return;
        };
        if !magic.starts_with(b"GIF87a") && !magic.starts_with(b"GIF89a") {
            return;
        }
        let Ok(header) = std::str::from_utf8(&self.pending_bytes[header_start..header_end]) else {
            return;
        };
        let header = header.to_owned();
        let Some((object_id, stream_sender)) = self.queue_iterm_gif_stream(header, cursor) else {
            return;
        };
        control.payload_start = payload_start;
        control.stream_sent_until = payload_start;
        control.stream_sender = Some(stream_sender);
        control.stream_object_id = Some(object_id);
        control.stream_cursor = cursor;
        bevy::log::debug!("streaming iTerm2 GIF as PTY data arrives");
    }

    fn send_iterm_stream_bytes(&self, control: &mut PendingControl, end: usize) {
        let Some(sender) = control.stream_sender.as_ref() else {
            return;
        };
        let start = control.stream_sent_until.max(control.payload_start);
        if end <= start {
            return;
        }
        if sender.send(self.pending_bytes[start..end].to_vec()).is_ok() {
            control.stream_sent_until = end;
        } else {
            control.stream_sender = None;
        }
    }

    fn queue_iterm_gif_stream(
        &mut self,
        header: String,
        cursor: (u16, u16),
    ) -> Option<(u32, mpsc::Sender<Vec<u8>>)> {
        let mut pending = self.pending_iterm_images.lock().unwrap();
        let object_id = self
            .objects
            .keys()
            .copied()
            .chain(pending.iter().map(|image| image.object_id))
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let (event_sender, receiver) = mpsc::sync_channel(2);
        let (stream_sender, stream_receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        if std::thread::Builder::new()
            .name(format!("ratty-iterm-gif-{object_id}"))
            .spawn(move || {
                decode_iterm_gif_stream(stream_receiver, &event_sender, &worker_cancelled);
            })
            .is_err()
        {
            bevy::log::warn!("could not start streaming decode for iTerm2 GIF {object_id}");
            return None;
        }
        pending.push(PendingItermImage {
            object_id,
            cursor,
            header,
            generation: self.screen_generation,
            cache_key: None,
            defer_events: self
                .anchors
                .values()
                .any(|anchor| anchor.row == cursor.0 && anchor.col == cursor.1),
            buffered_events: Vec::new(),
            cancelled,
            receiver,
        });
        bevy::log::debug!("started streaming iTerm2 GIF {object_id} from PTY output");
        Some((object_id, stream_sender))
    }

    fn poll_pending_iterm_images(&mut self) {
        let mut completed = Vec::new();
        {
            let mut pending = self.pending_iterm_images.lock().unwrap();
            pending.retain_mut(|image| {
                let mut disconnected = false;
                let mut released_frames = 0;
                let queued_frames = match self.objects.get(&image.object_id) {
                    Some(InlineObject::KittyImage(object)) => object.frames.len(),
                    _ => 0,
                };
                if image.defer_events && image.cache_key.is_none() {
                    while image.buffered_events.len() < MAX_QUEUED_ITERM_FRAMES {
                        match image.receiver.try_recv() {
                            Ok(event) => image.buffered_events.push(event),
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                disconnected = true;
                                break;
                            }
                        }
                    }
                    return !disconnected;
                }

                while !image.buffered_events.is_empty() {
                    let event = image.buffered_events.remove(0);
                    if matches!(&event, ItermDecodeEvent::Frame(_))
                        && queued_frames + released_frames >= MAX_QUEUED_ITERM_FRAMES
                    {
                        image.buffered_events.insert(0, event);
                        break;
                    }
                    if matches!(&event, ItermDecodeEvent::Frame(_)) {
                        released_frames += 1;
                    }
                    completed.push((
                        image.object_id,
                        image.cursor,
                        image.header.clone(),
                        image.generation,
                        image.cache_key,
                        event,
                    ));
                }
                loop {
                    match image.receiver.try_recv() {
                        Ok(event)
                            if matches!(&event, ItermDecodeEvent::Frame(_))
                                && queued_frames + released_frames >= MAX_QUEUED_ITERM_FRAMES =>
                        {
                            image.buffered_events.push(event);
                            break;
                        }
                        Ok(event) => {
                            if matches!(&event, ItermDecodeEvent::Frame(_)) {
                                released_frames += 1;
                            }
                            completed.push((
                                image.object_id,
                                image.cursor,
                                image.header.clone(),
                                image.generation,
                                image.cache_key,
                                event,
                            ));
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
                !disconnected
            });
        }
        for (object_id, cursor, header, generation, cache_key, event) in completed {
            if generation != self.screen_generation {
                bevy::log::debug!("discarding stale iTerm2 image {object_id}");
                continue;
            }
            match event {
                ItermDecodeEvent::Root {
                    width: pixel_width,
                    height: pixel_height,
                    rgba,
                    delay_secs,
                    looping,
                } => {
                    let (width, height) = iterm_cells(&header, pixel_width, pixel_height);
                    bevy::log::debug!(
                        "placed iTerm2 inline image {object_id} at row {}, column {} ({}x{} cells)",
                        cursor.0,
                        cursor.1,
                        width,
                        height
                    );
                    let image = KittyInlineObject {
                        raster: RasterObject {
                            width: pixel_width,
                            height: pixel_height,
                            rgba,
                            handle: None,
                        },
                        uses_placeholders: false,
                        tracks_scroll: false,
                        plane: None,
                        frames: VecDeque::new(),
                        animation_root: None,
                        animation_root_delay_secs: delay_secs,
                        streaming_animation: looping,
                        current_frame_delay_secs: delay_secs,
                        playing: looping,
                        animation_finished: !looping,
                        frame_index: 0,
                        frame_elapsed: 0.0,
                    };
                    self.remove_objects_at(&InlineAnchor {
                        row: cursor.0,
                        col: cursor.1,
                        columns: u32::from(width),
                        rows: u32::from(height),
                        style: InlineStyle::default(),
                    });
                    self.objects
                        .insert(object_id, InlineObject::KittyImage(image));
                    if let Some(cache_key) = cache_key {
                        self.iterm_image_cache.insert(cache_key, object_id);
                    }
                    self.set_anchor(
                        object_id,
                        InlineAnchor {
                            row: cursor.0,
                            col: cursor.1,
                            columns: u32::from(width),
                            rows: u32::from(height),
                            style: InlineStyle::default(),
                        },
                    );
                }
                ItermDecodeEvent::Frame(frame) => {
                    if let Some(InlineObject::KittyImage(image)) = self.objects.get_mut(&object_id)
                    {
                        image.frames.push_back(frame);
                        image.playing = true;
                    }
                }
                ItermDecodeEvent::Failed => {
                    bevy::log::warn!("failed to decode iTerm2 image {object_id}");
                }
            }
        }
    }

    /// Returns whether inline objects need synchronization.
    pub fn needs_sync(&self, viewport_size: Vec2, cols: u16, rows: u16) -> bool {
        self.dirty
            || self.last_viewport_size != viewport_size
            || self.last_cols != cols
            || self.last_rows != rows
    }

    /// Marks synchronization as complete.
    pub fn finish_sync(&mut self, viewport_size: Vec2, cols: u16, rows: u16) {
        self.dirty = false;
        self.last_viewport_size = viewport_size;
        self.last_cols = cols;
        self.last_rows = rows;
    }

    /// Applies upward scroll to anchored objects.
    pub fn apply_scroll(&mut self, rows_scrolled: u16) {
        if rows_scrolled == 0 || self.anchors.is_empty() {
            return;
        }

        self.anchors.retain(|object_id, anchor| {
            if self
                .objects
                .get(object_id)
                .is_some_and(|object| !object.scrolls_with_text())
            {
                return true;
            }
            let new_row = anchor.row as i32 - rows_scrolled as i32;
            if new_row + anchor.rows as i32 <= 0 {
                return false;
            }
            anchor.row = new_row.max(0) as u16;
            true
        });
        self.dirty = true;
    }

    /// Returns whether any anchors need scroll tracking.
    pub fn has_scroll_tracked_anchors(&self) -> bool {
        self.anchors.keys().any(|object_id| {
            self.objects
                .get(object_id)
                .is_some_and(InlineObject::scrolls_with_text)
        })
    }

    /// Refreshes placeholder-derived Kitty anchors.
    pub fn refresh_placeholder_anchors(&mut self, screen: ScreenView<'_>) {
        if refresh_kitty_placeholder_anchors(&self.objects, &mut self.anchors, screen) {
            self.dirty = true;
        }
    }

    fn set_anchor(&mut self, object_id: u32, anchor: InlineAnchor) {
        self.anchors.insert(object_id, anchor);
        self.dirty = true;
    }

    fn remove_object(&mut self, object_id: u32) {
        self.objects.remove(&object_id);
        self.anchors.remove(&object_id);
        self.cancel_pending_iterm_image(object_id);
        self.pending_rgp_payloads.remove(&object_id);
        self.iterm_image_cache
            .retain(|_, cached_object_id| *cached_object_id != object_id);
        self.dirty = true;
    }

    fn clear_objects(&mut self) {
        let mut pending_images = self.pending_iterm_images.lock().unwrap();
        for image in pending_images.iter() {
            image.cancelled.store(true, Ordering::Relaxed);
        }
        pending_images.clear();
        self.objects.clear();
        self.anchors.clear();
        self.iterm_image_cache.clear();
        self.pending_rgp_payloads.clear();
        self.dirty = true;
    }

    fn handle_apc_sequence(
        &mut self,
        sequence: &[u8],
        cursor_position: (u16, u16),
        local_file_root: &Path,
        camera_updates: &mut Vec<TerminalCameraUpdate>,
    ) -> (bool, Option<Vec<u8>>) {
        if let Some(reply) = self.handle_rgp_sequence(sequence, camera_updates) {
            return (true, reply);
        }

        let Some(operation) =
            self.kitty
                .consume_sequence(sequence, cursor_position, local_file_root)
        else {
            return (false, None);
        };

        match operation {
            KittyOperation::Pending | KittyOperation::Ignored => (true, None),
            KittyOperation::Query { image_id } => (
                true,
                Some(format!("\x1b_Gi={image_id};OK\x1b\\").into_bytes()),
            ),
            KittyOperation::AddFrame {
                object_id,
                image,
                delay_ms,
            } => {
                if let Some(InlineObject::KittyImage(object)) = self.objects.get_mut(&object_id) {
                    object.frames.push_back(KittyAnimationFrame {
                        rgba: image.rgba,
                        delay_secs: (delay_ms.max(10) as f32) / 1000.0,
                    });
                    self.dirty = true;
                }
                (true, None)
            }
            KittyOperation::AnimationControl { object_id, start } => {
                if let Some(InlineObject::KittyImage(object)) = self.objects.get_mut(&object_id) {
                    object.playing = start;
                    object.animation_finished = true;
                    object.frame_elapsed = 0.0;
                }
                (true, None)
            }
            KittyOperation::TransmitOnly { object_id, image } => {
                self.objects
                    .insert(object_id, InlineObject::KittyImage(image.rasterize()));
                self.dirty = true;
                (true, None)
            }
            KittyOperation::TransmitAndPlace {
                object_id,
                image,
                anchor,
            } => {
                self.remove_objects_at(&InlineAnchor {
                    row: anchor.row,
                    col: anchor.col,
                    columns: anchor.columns,
                    rows: anchor.rows,
                    style: InlineStyle::default(),
                });
                self.objects
                    .insert(object_id, InlineObject::KittyImage(image.rasterize()));
                self.set_anchor(
                    object_id,
                    InlineAnchor {
                        row: anchor.row,
                        col: anchor.col,
                        columns: anchor.columns,
                        rows: anchor.rows,
                        style: InlineStyle::default(),
                    },
                );
                (true, None)
            }
            KittyOperation::PlaceExisting { object_id, anchor } => {
                if self.objects.contains_key(&object_id) {
                    self.set_anchor(
                        object_id,
                        InlineAnchor {
                            row: anchor.row,
                            col: anchor.col,
                            columns: anchor.columns,
                            rows: anchor.rows,
                            style: InlineStyle::default(),
                        },
                    );
                }
                (true, None)
            }
            KittyOperation::Delete { object_id } => {
                if let Some(object_id) = object_id {
                    self.remove_object(object_id);
                }
                (true, None)
            }
            KittyOperation::DeletePlacements { object_id } => {
                if self.anchors.remove(&object_id).is_some() {
                    self.dirty = true;
                }
                (true, None)
            }
            KittyOperation::DeleteAllPlacements => {
                self.anchors.clear();
                self.dirty = true;
                (true, None)
            }
            KittyOperation::DeleteAll => {
                self.clear_objects();
                (true, None)
            }
        }
    }

    fn handle_rgp_sequence(
        &mut self,
        sequence: &[u8],
        camera_updates: &mut Vec<TerminalCameraUpdate>,
    ) -> Option<Option<Vec<u8>>> {
        let operation = consume_rgp_sequence(sequence)?;
        Some(match operation {
            RgpOperation::SupportQuery => Some(support_reply()),
            RgpOperation::Camera {
                camera_slot,
                switch_immediately,
                settings,
            } => {
                camera_updates.push(TerminalCameraUpdate {
                    slot: camera_slot as usize,
                    activate: switch_immediately,
                    mode: settings.camera_type,
                    scale: settings.scale,
                    fov: settings.fov,
                    translation: OptionalVec3::from(settings.offset),
                    rotation_degrees: OptionalVec3::from(settings.rotation),
                });
                None
            }
            RgpOperation::Register {
                object_id,
                format,
                options,
                source,
            } => {
                let load_options = ObjectLoadOptions {
                    normalize: options.normalize,
                };
                if format != "obj" && format != "glb" && format != "stl" {
                    warn!("unsupported RGP object format `{format}` for object {object_id}");
                    None
                } else {
                    match source {
                        RgpRegisterSource::Path { path } => {
                            self.pending_rgp_payloads.remove(&object_id);
                            match load_rgp_object_source_with_options(
                                Path::new(&path),
                                load_options,
                            ) {
                                Ok((source, source_data)) => {
                                    info!("registered RGP object {} from {}", object_id, source);
                                    self.objects.insert(object_id, source_data.into());
                                    self.dirty = true;
                                    None
                                }
                                Err(error) => {
                                    warn!("failed to load RGP object {object_id}: {error:#}");
                                    None
                                }
                            }
                        }
                        RgpRegisterSource::Payload { name, more, data } => self
                            .handle_rgp_payload_chunk(
                                object_id,
                                &format,
                                name,
                                more,
                                data,
                                load_options,
                            ),
                    }
                }
            }
            RgpOperation::Place { object_id, anchor } => {
                if self.objects.contains_key(&object_id) {
                    let row = anchor
                        .row
                        .saturating_sub(anchor.rows.saturating_sub(1).div_ceil(2) as u16);
                    let col = anchor
                        .col
                        .saturating_sub(anchor.columns.saturating_sub(1).div_ceil(2) as u16);
                    self.set_anchor(
                        object_id,
                        InlineAnchor {
                            row,
                            col,
                            columns: anchor.columns,
                            rows: anchor.rows,
                            style: anchor.style.into(),
                        },
                    );
                }
                None
            }
            RgpOperation::Update { object_id, update } => {
                if let Some(anchor) = self.anchors.get_mut(&object_id) {
                    let needs_respawn = update.depth.is_some()
                        || update.color.is_some()
                        || update.brightness.is_some();
                    apply_rgp_update(&mut anchor.style, update);
                    if needs_respawn {
                        self.dirty = true;
                    }
                }
                None
            }
            RgpOperation::Delete { object_id } => {
                if let Some(object_id) = object_id {
                    self.remove_object(object_id);
                } else {
                    self.clear_objects();
                }
                None
            }
            RgpOperation::Ignored => None,
        })
    }

    fn remove_objects_at(&mut self, new_anchor: &InlineAnchor) {
        let row_start = new_anchor.row as i32;
        let row_end = row_start + new_anchor.rows as i32;
        let col_start = new_anchor.col as i32;
        let col_end = col_start + new_anchor.columns as i32;

        let overlapping_ids = self
            .anchors
            .iter()
            .filter_map(|(object_id, anchor)| {
                let anchor_row_start = anchor.row as i32;
                let anchor_row_end = anchor_row_start + anchor.rows as i32;
                let anchor_col_start = anchor.col as i32;
                let anchor_col_end = anchor_col_start + anchor.columns as i32;

                (anchor_row_start < row_end
                    && anchor_row_end > row_start
                    && anchor_col_start < col_end
                    && anchor_col_end > col_start)
                    .then_some(*object_id)
            })
            .collect::<Vec<_>>();

        for object_id in overlapping_ids {
            bevy::log::debug!("replacing overlapping inline object {object_id}");
            self.remove_object(object_id);
        }
    }

    // Buffers chunked payload registrations until the final chunk arrives, then loads and registers the object.
    fn handle_rgp_payload_chunk(
        &mut self,
        object_id: u32,
        format: &str,
        name: Option<String>,
        more: bool,
        data: Vec<u8>,
        options: ObjectLoadOptions,
    ) -> Option<Vec<u8>> {
        let pending = self
            .pending_rgp_payloads
            .entry(object_id)
            .or_insert_with(|| PendingRgpPayload {
                format: format.to_string(),
                name: name.clone(),
                data: Vec::new(),
                options,
            });
        if pending.format != format {
            warn!(
                "ignoring RGP payload chunk for object {} due to format mismatch ({} vs {})",
                object_id, pending.format, format
            );
            return None;
        }
        if pending.name.is_none() {
            pending.name = name;
        }
        pending.data.extend_from_slice(&data);
        info!(
            "received RGP payload chunk for object {} (format={}, accumulated={} bytes, more={})",
            object_id,
            pending.format,
            pending.data.len(),
            more
        );
        if more {
            return None;
        }

        let pending = self.pending_rgp_payloads.remove(&object_id)?;
        info!(
            "finalizing RGP payload for object {} (format={}, total={} bytes)",
            object_id,
            pending.format,
            pending.data.len()
        );
        match load_object_source_from_bytes_with_options(
            &pending.format,
            pending.name.as_deref(),
            &pending.data,
            pending.options,
        ) {
            Ok((source, source_data)) => {
                info!("registered RGP object {} from {}", object_id, source);
                self.objects.insert(object_id, source_data.into());
                self.dirty = true;
                None
            }
            Err(error) => {
                warn!("failed to load RGP object {object_id}: {error:#}");
                None
            }
        }
    }
}

struct PendingControl {
    is_iterm: bool,
    scan_from: usize,
    payload_start: usize,
    stream_sent_until: usize,
    stream_sender: Option<mpsc::Sender<Vec<u8>>>,
    stream_object_id: Option<u32>,
    stream_cursor: (u16, u16),
    stream_checked: bool,
}

struct PendingRgpPayload {
    format: String,
    name: Option<String>,
    data: Vec<u8>,
    options: ObjectLoadOptions,
}

struct PendingItermImage {
    object_id: u32,
    cursor: (u16, u16),
    header: String,
    generation: u64,
    cache_key: Option<u64>,
    defer_events: bool,
    buffered_events: Vec<ItermDecodeEvent>,
    cancelled: Arc<AtomicBool>,
    receiver: mpsc::Receiver<ItermDecodeEvent>,
}

enum ItermDecodeEvent {
    Root {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        delay_secs: f32,
        looping: bool,
    },
    Frame(KittyAnimationFrame),
    Failed,
}

fn pending_apc_prefix_start(bytes: &[u8], cursor: usize) -> usize {
    let start = cursor.min(bytes.len());
    let tail = &bytes[start..];
    for prefix in [
        CLEAR_SCREEN,
        SYNC_UPDATE_END,
        b"\x1b[".as_slice(),
        b"\x1b".as_slice(),
    ] {
        for length in (1..=tail.len().min(prefix.len())).rev() {
            if prefix.starts_with(&tail[tail.len() - length..]) {
                return bytes.len() - length;
            }
        }
    }
    (1..=tail.len().min(ITERM_START.len()))
        .rev()
        .find(|length| ITERM_START.starts_with(&tail[tail.len() - length..]))
        .map(|length| bytes.len() - length)
        .or_else(|| tail.ends_with(&APC_START[..1]).then_some(bytes.len() - 1))
        .unwrap_or(bytes.len())
}

fn apc_end_from(bytes: &[u8], payload_start: usize, scan_from: usize) -> Option<usize> {
    let mut index = payload_start.max(scan_from);
    loop {
        if index >= bytes.len() {
            return None;
        }
        if bytes[index] == b'\x07' {
            return Some(index + 1);
        }
        if bytes[index] == C1_ST {
            return Some(index + 1);
        }
        if index + 1 < bytes.len() && bytes[index] == ST[0] && bytes[index + 1] == ST[1] {
            return Some(index + 2);
        }
        index += 1;
    }
}

fn iterm_cells(header: &str, pixel_width: u32, pixel_height: u32) -> (u16, u16) {
    let mut width = None;
    let mut height = None;
    for field in header.split(';') {
        if let Some(value) = field.strip_prefix("width=") {
            width = value.parse::<u16>().ok();
        }
        if let Some(value) = field.strip_prefix("height=") {
            height = value.parse::<u16>().ok();
        }
    }
    let aspect = pixel_height.max(1) as f32 / pixel_width.max(1) as f32;
    let columns = width.unwrap_or(30).max(1);
    let rows = height.unwrap_or_else(|| ((columns as f32 * aspect * 0.5).ceil() as u16).max(1));
    (columns, rows)
}

fn parse_iterm_sequence(sequence: &[u8]) -> Option<(String, Vec<u8>)> {
    let (header, encoded) = parse_iterm_sequence_parts(sequence)?;
    Some((header, encoded.to_vec()))
}

fn parse_iterm_sequence_parts(sequence: &[u8]) -> Option<(String, &[u8])> {
    let end = sequence
        .iter()
        .position(|byte| *byte == b'\x07' || *byte == C1_ST)
        .or_else(|| sequence.windows(2).position(|pair| pair == ST))?;
    let start = ITERM_START.len();
    let separator = sequence
        .get(start..end)?
        .iter()
        .position(|byte| *byte == b':')?
        + start;
    let header = std::str::from_utf8(sequence.get(start..separator)?)
        .ok()?
        .to_owned();
    let encoded = sequence.get(separator + 1..end)?;
    Some((header, encoded))
}

fn iterm_image_cache_key(header: &str, encoded: &[u8], cursor: (u16, u16)) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    header.hash(&mut hasher);
    cursor.hash(&mut hasher);
    encoded.hash(&mut hasher);
    hasher.finish()
}

fn decode_iterm_image(
    encoded: &[u8],
    sender: &mpsc::SyncSender<ItermDecodeEvent>,
    cancelled: &AtomicBool,
    root_already_sent: bool,
) {
    use base64::Engine as _;
    if cancelled.load(Ordering::Relaxed) {
        return;
    }
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        let _ = sender.send(ItermDecodeEvent::Failed);
        return;
    };
    if bytes.len() > 64 * 1024 * 1024 {
        let _ = sender.send(ItermDecodeEvent::Failed);
        return;
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        if !decode_inline_image(&bytes, sender, cancelled, root_already_sent) {
            let _ = sender.send(ItermDecodeEvent::Failed);
        }
        return;
    }
    let Ok(mut reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()
    else {
        let _ = sender.send(ItermDecodeEvent::Failed);
        return;
    };
    reader.limits(inline_image_limits());
    let Some(decoded) = reader
        .decode()
        .ok()
        .map(|image| fit_still_image(image.to_rgba8()))
    else {
        let _ = sender.send(ItermDecodeEvent::Failed);
        return;
    };
    let _ = sender.send(ItermDecodeEvent::Root {
        width: decoded.width(),
        height: decoded.height(),
        rgba: decoded.into_raw(),
        delay_secs: 0.1,
        looping: false,
    });
}

fn decode_iterm_gif_stream(
    receiver: mpsc::Receiver<Vec<u8>>,
    sender: &mpsc::SyncSender<ItermDecodeEvent>,
    cancelled: &AtomicBool,
) {
    let started = Instant::now();
    let mut encoded = Vec::new();
    let mut root_sent = false;
    let mut next_preview_at = 1024usize;
    for chunk in receiver {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        encoded.extend_from_slice(&chunk);
        if !root_sent && encoded.len() >= next_preview_at {
            next_preview_at = next_preview_at.saturating_mul(2);
            if let Some(root) = try_decode_gif_root(&encoded) {
                if sender.send(root).is_err() {
                    return;
                }
                root_sent = true;
                bevy::log::debug!(
                    "decoded progressive iTerm2 GIF first frame in {:?} ({} encoded bytes)",
                    started.elapsed(),
                    encoded.len()
                );
            }
        }
        if encoded.len() > 90_000_000 {
            let _ = sender.send(ItermDecodeEvent::Failed);
            return;
        }
    }
    if !cancelled.load(Ordering::Relaxed) {
        decode_iterm_image(&encoded, sender, cancelled, root_sent);
    }
}

fn try_decode_gif_root(encoded: &[u8]) -> Option<ItermDecodeEvent> {
    use base64::Engine as _;
    use image::{AnimationDecoder as _, ImageDecoder as _};

    let complete_quanta = encoded.len() / 4 * 4;
    if complete_quanta < 12 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&encoded[..complete_quanta])
        .ok()?;
    if !bytes.starts_with(b"GIF87a") && !bytes.starts_with(b"GIF89a") {
        return None;
    }
    let mut decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    decoder.set_limits(inline_image_limits()).ok()?;
    let mut frames = decoder.into_frames();
    let frame = frames.next()?.ok()?;
    let delay = frame.delay();
    let image = fit_gif_frame(frame.into_buffer());
    Some(ItermDecodeEvent::Root {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
        delay_secs: (delay.numer_denom_ms().0 as f32
            / delay.numer_denom_ms().1.max(1) as f32
            / 1000.0)
            .max(0.01),
        looping: true,
    })
}

fn decode_inline_image(
    bytes: &[u8],
    sender: &mpsc::SyncSender<ItermDecodeEvent>,
    cancelled: &AtomicBool,
    root_already_sent: bool,
) -> bool {
    use image::{AnimationDecoder as _, ImageDecoder as _};
    let mut first_cycle = true;
    let mut root_sent = root_already_sent;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return true;
        }
        let Ok(mut decoder) = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes))
        else {
            return false;
        };
        if decoder.set_limits(inline_image_limits()).is_err() {
            return false;
        }
        let mut frames = decoder.into_frames();
        let Some(Ok(first)) = frames.next() else {
            return false;
        };
        let first_delay = gif_frame_delay(&first);
        let first_image = fit_gif_frame(first.into_buffer());
        if !root_sent {
            if sender
                .send(ItermDecodeEvent::Root {
                    width: first_image.width(),
                    height: first_image.height(),
                    rgba: first_image.into_raw(),
                    delay_secs: first_delay,
                    looping: true,
                })
                .is_err()
            {
                return true;
            }
            root_sent = true;
        } else if !first_cycle || !root_already_sent {
            if !send_gif_frame(first_image, first_delay, sender) {
                return true;
            }
        }
        first_cycle = false;
        for frame in frames {
            if cancelled.load(Ordering::Relaxed) {
                return true;
            }
            let Ok(frame) = frame else { return false };
            let delay = gif_frame_delay(&frame);
            if !send_gif_frame(fit_gif_frame(frame.into_buffer()), delay, sender) {
                return true;
            }
        }
    }
}

fn gif_frame_delay(frame: &image::Frame) -> f32 {
    let delay = frame.delay();
    (delay.numer_denom_ms().0 as f32 / delay.numer_denom_ms().1.max(1) as f32 / 1000.0).max(0.01)
}

fn send_gif_frame(
    image: image::RgbaImage,
    delay_secs: f32,
    sender: &mpsc::SyncSender<ItermDecodeEvent>,
) -> bool {
    sender
        .send(ItermDecodeEvent::Frame(KittyAnimationFrame {
            rgba: image.into_raw(),
            delay_secs,
        }))
        .is_ok()
}

fn fit_gif_frame(frame: image::RgbaImage) -> image::RgbaImage {
    let (width, height) = fit_gif_dimensions(frame.dimensions());
    if width == frame.width() && height == frame.height() {
        return frame;
    }
    image::imageops::resize(&frame, width, height, image::imageops::FilterType::Triangle)
}

fn fit_still_image(image: image::RgbaImage) -> image::RgbaImage {
    const MAX_EDGE: u32 = 1600;
    let (width, height) = image.dimensions();
    let longest_edge = width.max(height);
    if longest_edge <= MAX_EDGE {
        return image;
    }
    let scale = MAX_EDGE as f32 / longest_edge as f32;
    let width = (width as f32 * scale).round().max(1.0) as u32;
    let height = (height as f32 * scale).round().max(1.0) as u32;
    image::imageops::resize(&image, width, height, image::imageops::FilterType::Triangle)
}

fn fit_gif_dimensions((width, height): (u32, u32)) -> (u32, u32) {
    const MAX_EDGE: u32 = 640;
    let longest_edge = width.max(height);
    if longest_edge <= MAX_EDGE {
        return (width, height);
    }
    let scale = MAX_EDGE as f32 / longest_edge as f32;
    let width = (width as f32 * scale).round().max(1.0) as u32;
    let height = (height as f32 * scale).round().max(1.0) as u32;
    (width, height)
}

fn inline_image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits
}

/// Registered inline object.
pub enum InlineObject {
    /// Kitty image object.
    KittyImage(KittyInlineObject),
    /// Ratty graphics object.
    RgpObject(RgpInlineObject),
}

/// Raster image payload.
pub struct RasterObject {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// RGBA image bytes.
    pub rgba: Vec<u8>,
    /// Uploaded image handle.
    pub handle: Option<Handle<Image>>,
}

/// Kitty-backed inline object.
pub struct KittyInlineObject {
    /// Raster image payload.
    pub raster: RasterObject,
    /// Indicates placeholder-driven placement.
    pub uses_placeholders: bool,
    /// Whether the placement follows inferred terminal scrolling.
    pub tracks_scroll: bool,
    /// Cached plane mesh and material for 3D presentation.
    pub(crate) plane: Option<KittyPlaneCache>,
    /// Animation frames following the base image.
    pub(crate) frames: VecDeque<KittyAnimationFrame>,
    /// Retained base pixels for looping back to frame one.
    pub(crate) animation_root: Option<Vec<u8>>,
    /// Duration of the base image before advancing to the first animation frame.
    pub(crate) animation_root_delay_secs: f32,
    /// Whether frames arrive continuously from a bounded background decoder.
    pub(crate) streaming_animation: bool,
    /// Delay for the currently displayed frame in a streaming animation.
    pub(crate) current_frame_delay_secs: f32,
    /// Whether all animation frames have arrived and looping can begin.
    pub(crate) animation_finished: bool,
    /// Whether terminal-side animation playback is active.
    pub(crate) playing: bool,
    /// Current animation frame index, where zero is the base image.
    pub(crate) frame_index: usize,
    /// Time accumulated toward the next frame.
    pub(crate) frame_elapsed: f32,
}

/// Decoded Kitty animation frame.
pub(crate) struct KittyAnimationFrame {
    /// RGBA pixel data.
    pub rgba: Vec<u8>,
    /// Display time before advancing.
    pub delay_secs: f32,
}

/// RGP-backed inline object.
pub enum RgpInlineObject {
    /// STL mesh payload.
    Stl {
        /// The loaded mesh
        mesh: Mesh,
        /// Cached extruded mesh handle keyed by extrusion depth.
        handle: Option<(u32, Handle<Mesh>)>,
    },
    /// OBJ mesh payload.
    Obj {
        /// Loaded mesh parts.
        meshes: Vec<Mesh>,
        /// Cached mesh handles keyed by depth.
        handles: Option<(u32, Vec<Handle<Mesh>>)>,
    },
    /// glTF scene payload.
    Gltf {
        /// Scene asset path.
        asset_path: String,
        /// Cached scene handle.
        handle: Option<Handle<WorldAsset>>,
    },
}

impl InlineObject {
    fn scrolls_with_text(&self) -> bool {
        match self {
            InlineObject::KittyImage(object) => object.tracks_scroll,
            InlineObject::RgpObject(_) => true,
        }
    }
}

/// Inline object anchor.
pub struct InlineAnchor {
    /// Anchor row.
    pub row: u16,
    /// Anchor column.
    pub col: u16,
    /// Object width in cells.
    pub columns: u32,
    /// Object height in cells.
    pub rows: u32,
    /// Inline styling.
    pub style: InlineStyle,
}

/// Inline object style.
#[derive(Clone, Copy, Default)]
pub struct InlineStyle {
    /// Enables default animation.
    pub animate: bool,
    /// Scale multiplier.
    pub scale: f32,
    /// Extrusion depth.
    pub depth: f32,
    /// Optional object color.
    pub color: Option<[u8; 3]>,
    /// Brightness multiplier.
    pub brightness: f32,
    /// Translation offset relative to the anchor.
    pub offset: Vec3,
    /// Rotation in degrees.
    pub rotation: Vec3,
    /// Non-uniform scale multiplier.
    pub scale3: Vec3,
}

impl From<RgpPlacementStyle> for InlineStyle {
    fn from(value: RgpPlacementStyle) -> Self {
        Self {
            animate: value.animate,
            scale: value.scale,
            depth: value.depth,
            color: value.color,
            brightness: value.brightness,
            offset: Vec3::from_array(value.offset),
            rotation: Vec3::from_array(value.rotation),
            scale3: Vec3::from_array(value.scale3),
        }
    }
}

fn apply_rgp_update(style: &mut InlineStyle, update: RgpPlacementUpdate) {
    if let Some(animate) = update.animate {
        style.animate = animate;
    }
    if let Some(scale) = update.scale {
        style.scale = scale;
    }
    if let Some(depth) = update.depth {
        style.depth = depth;
    }
    if let Some(color) = update.color {
        style.color = Some(color);
    }
    if let Some(brightness) = update.brightness {
        style.brightness = brightness;
    }
    apply_vec3_update(&mut style.offset, update.offset);
    apply_vec3_update(&mut style.rotation, update.rotation);
    apply_vec3_update(&mut style.scale3, update.scale3);
}

fn apply_vec3_update(target: &mut Vec3, update: [Option<f32>; 3]) {
    if let Some(x) = update[0] {
        target.x = x;
    }
    if let Some(y) = update[1] {
        target.y = y;
    }
    if let Some(z) = update[2] {
        target.z = z;
    }
}
