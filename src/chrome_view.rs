//! Chrome CDP screencast presented as an ordinary GPUI pane (not another window).
//! The Chrome process owns page pixels; Herdr owns focus, pane layout and UI.
use gpui::prelude::*;
use gpui::*;
use std::{
    cell::RefCell, rc::Rc,
    sync::{Arc, atomic::{AtomicU64, Ordering}},
    time::Duration,
};
use serde_json::json;
use crate::chrome_cdp::{self, Session};

pub struct ChromeView {
    pane_id: String,
    url: String,
    session: Option<Arc<Session>>,
    frame: Option<Arc<RenderImage>>,
    error: Option<String>,
    focus: FocusHandle,
    bounds: Rc<RefCell<Option<Bounds<Pixels>>>>,
    viewport: Arc<AtomicU64>,
    mouse_pressed: bool,
    last_pointer: Option<(f64, f64)>,
    visible: bool,
    _load: Option<Task<()>>,
    _frames: Option<Task<()>>,
}

impl ChromeView {
    /// A Herdr pane owns one Chrome session and never reveals its raw CDP target.
    pub fn session(&self) -> Option<Arc<Session>> {
        self.session.clone()
    }

    /// Return the actual launch failure to the caller. A failed async
    /// CDP startup must not be reported as "still starting" indefinitely.
    /// This contains only browser startup diagnostics, never page content.
    pub fn startup_error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn new(pane_id: String, url: String, cx: &mut Context<Self>) -> Self {
        let open_url = url.clone();
        let loader = cx.spawn(async move |this, cx| {
            let launched_url = open_url.clone();
            let ready = cx.background_executor().spawn(async move {
                let client = chrome_cdp::shared_client()?;
                let pane = client.open(&open_url)?;
                Ok::<_, String>(Arc::new(pane))
            }).await;
            let _ = this.update(cx, |this, cx| {
                match ready {
                    Ok(session) => {
                        let receiver = session.clone();
                        this._frames = Some(cx.spawn(async move |this, cx| {
                            loop {
                                let reader = receiver.clone();
                                // Return only the paint-ready image to the foreground.
                                let decoded = cx.background_executor().spawn(async move {
                                    let frame = reader.frame_timeout(Duration::from_millis(800))?;
                                    decode_frame(&frame)
                                }).await;
                                if decoded.is_err() && this.upgrade().is_none() { break }
                                if let Ok(frame) = decoded
                                    && this.update(cx, |this, cx| {
                                        this.frame = Some(frame);
                                        cx.notify();
                                    }).is_err()
                                {
                                    break;
                                }
                            }
                        }));
                        if !this.visible {
                            let _ = session.post("Page.stopScreencast", json!({}));
                        }
                        // An Agent can navigate while Chrome is still starting.
                        // Never replace that newer desired URL with the launch URL.
                        if this.url != launched_url {
                            let newer_url = this.url.clone();
                            let newer_session = session.clone();
                            cx.spawn(async move |this, cx| {
                                let result = cx.background_executor().spawn(async move {
                                    newer_session.navigate(&newer_url)
                                }).await;
                                if let Err(error) = result {
                                    let _ = this.update(cx, |this, cx| {
                                        this.error = Some(error);
                                        cx.notify();
                                    });
                                }
                            }).detach();
                        }
                        this.session = Some(session);
                        this.error = None;
                    }
                    Err(error) => {
                        tracing::warn!("Chrome CDP pane {} failed to initialize: {error}", this.pane_id);
                        this.error = Some(error);
                    },
                }
                cx.notify();
            });
        });
        Self {
            pane_id, url, session: None, frame: None, error: None,
            focus: cx.focus_handle(), bounds: Rc::new(RefCell::new(None)),
            viewport: Arc::new(AtomicU64::new(0)), mouse_pressed: false,
            last_pointer: None, visible: true,
            _load: Some(loader), _frames: None,
        }
    }

    pub fn navigate(&mut self, url: &str, cx: &mut Context<Self>) {
        if self.url == url { return }
        self.url = url.to_owned();
        let next = url.to_owned();
        if let Some(pane) = self.session.clone() {
            cx.spawn(async move |this, cx| {
                let response = cx.background_executor().spawn(async move {pane.navigate(&next)}).await;
                let _ = this.update(cx, |this, cx| {
                    this.error = response.err();
                    cx.notify();
                });
            }).detach();
        }
    }

    pub fn action(&mut self, action: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let pane = self.session.clone().ok_or("Chrome pane is still starting")?;
        let action = action.to_owned();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                match action.as_str() {
                    "browser.back" => pane.history(false),
                    "browser.forward" => pane.history(true),
                    "browser.reload" => pane.reload(),
                    _ => Err("unsupported Chrome browser action".into()),
                }
            }).await;
            if let Err(error) = result {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(error);
                    cx.notify();
                });
            }
        }).detach();
        Ok(())
    }

    fn relative(&self, position: Point<Pixels>) -> Option<(f64, f64)> {
        let bounds = (*self.bounds.borrow())?;
        let x = f32::from(position.x - bounds.origin.x);
        let y = f32::from(position.y - bounds.origin.y);
        let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        (x >= 0.0 && y >= 0.0 && x < width && y < height).then_some((x as f64, y as f64))
    }

    /// Pause live frame capture while a tab is hidden; the Chrome target and
    /// its DOM remain alive for background navigation and Agent operations.
    pub fn set_visible(&mut self, visible: bool) {
        if self.visible == visible { return }
        self.visible = visible;
        if let Some(session) = &self.session {
            let (method, args) = if visible {
                ("Page.startScreencast", json!({"format":"png","everyNthFrame":1}))
            } else {
                ("Page.stopScreencast", json!({}))
            };
            let _ = session.post(method, args);
        }
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        let Some((x, y)) = self.relative(ev.position) else { return };
        let Some(pane) = &self.session else { return };
        let _ = pane.post("Input.dispatchMouseEvent",
            json!({"type":"mousePressed","x":x,"y":y,"button":"left","clickCount":1}));
        self.mouse_pressed = true;
        self.last_pointer = Some((x, y));
        cx.stop_propagation();
    }

    fn mouse_up(&mut self, ev: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.mouse_pressed { return }
        self.mouse_pressed = false;
        let target = self.relative(ev.position).or(self.last_pointer);
        self.last_pointer = None;
        let Some((x, y)) = target else { return };
        let Some(pane) = &self.session else { return };
        let _ = pane.post("Input.dispatchMouseEvent",
            json!({"type":"mouseReleased","x":x,"y":y,"button":"left","clickCount":1}));
        cx.stop_propagation();
    }

    fn move_pointer(&mut self, ev: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        let Some((x, y)) = self.relative(ev.position) else { return };
        self.last_pointer = Some((x, y));
        let pressed = self.mouse_pressed;
        let Some(pane) = &self.session else { return };
        let _ = pane.post("Input.dispatchMouseEvent", json!({
            "type":"mouseMoved","x":x,"y":y,
            "button": if pressed { "left" } else { "none" },
            "buttons": if pressed { 1 } else { 0 }
        }));
    }

    fn wheel(&mut self, ev: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some((x, y)) = self.relative(window.mouse_position()) else { return };
        let Some(pane) = &self.session else { return };
        let delta = ev.delta.pixel_delta(window.line_height());
        let _ = pane.post("Input.dispatchMouseEvent", json!({
            "type":"mouseWheel","x":x,"y":y,
            "deltaX":f32::from(delta.x),"deltaY":f32::from(delta.y),
        }));
        cx.stop_propagation();
    }

    fn key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = &self.session else { return };
        if !self.focus.is_focused(window) { return }
        let key = ev.keystroke.key.as_str();
        match key {
            "enter" | "return" | "tab" | "backspace" | "delete" | "escape" | "up" | "down" | "left" | "right" => {
                let windows_key = match key {
                    "enter" | "return" => "Enter", "tab" => "Tab",
                    "backspace" => "Backspace", "delete" => "Delete", "escape" => "Escape",
                    "up" => "ArrowUp", "down" => "ArrowDown",
                    "left" => "ArrowLeft", "right" => "ArrowRight", _ => key,
                };
                let _ = pane.post("Input.dispatchKeyEvent",
                    json!({"type":"rawKeyDown","key":windows_key}));
                let _ = pane.post("Input.dispatchKeyEvent",
                    json!({"type":"keyUp","key":windows_key}));
                cx.stop_propagation();
            }
            _ if !ev.keystroke.modifiers.control && !ev.keystroke.modifiers.platform && !ev.keystroke.modifiers.alt => {
                if let Some(text) = ev.keystroke.key_char.as_deref() {
                    let _ = pane.post("Input.insertText", json!({"text":text}));
                    cx.stop_propagation();
                }
            }
            _ => {}
        }
    }
}

fn decode_frame_pixels(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    if bytes.len() > 12 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("invalid or oversized Chrome PNG frame".into());
    }
    let mut image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|e| format!("Chrome PNG decode failed: {e}"))?
        .into_rgba8();
    // gpui-pre's atlas expects BGRA byte ordering, unlike PNG's RGBA.
    for pixel in image.chunks_exact_mut(4) { pixel.swap(0, 2) }
    Ok(image)
}

fn decode_frame(bytes: &[u8]) -> Result<Arc<RenderImage>, String> {
    let image = decode_frame_pixels(bytes)?;
    Ok(Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(image)])))
}

impl Render for ChromeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let screen: AnyElement = match &self.frame {
            Some(frame) => {
                let painted = frame.clone();
                let bounds = self.bounds.clone();
                let viewport = self.viewport.clone();
                let session = self.session.clone();
                canvas(move |box_bounds, _, _| {
                    *bounds.borrow_mut() = Some(box_bounds);
                    let width = f32::from(box_bounds.size.width).round().clamp(1.0, 8192.0) as u32;
                    let height = f32::from(box_bounds.size.height).round().clamp(1.0, 8192.0) as u32;
                    let current = ((width as u64) << 32) | height as u64;
                    if viewport.swap(current, Ordering::Relaxed) != current {
                        if let Some(session) = session {
                            std::thread::spawn(move || {
                                let _ = session.resize(width, height);
                            });
                        }
                    }
                }, move |box_bounds, _, window, _| {
                    let _ = window.paint_image(box_bounds, box_bounds, Corners::default(), painted, 0, false);
                }).size_full().into_any_element()
            }
            None => div().size_full().p_3().child(
                self.error.as_deref().unwrap_or("Starting local Chrome and waiting for a CDP frame").to_owned()
            ).into_any_element(),
        };
        div()
            .id(SharedString::from(format!("chrome-view-{}", self.pane_id)))
            .size_full().relative().bg(rgb(0xffffff))
            .track_focus(&self.focus)
            .key_context("Chrome")
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::move_pointer))
            .on_scroll_wheel(cx.listener(Self::wheel))
            .on_key_down(cx.listener(Self::key))
            .child(screen)
    }
}


#[cfg(test)]
mod frame_tests {
    use super::{decode_frame, decode_frame_pixels};
    #[test]
    fn png_conversion_preserves_bgra_pixels() {
        let rgba = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(3, 2, image::Rgba([17, 91, 201, 113])));
        let rgb = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(3, 2, image::Rgb([17, 91, 201])));
        for source in [rgba, rgb] {
            let mut encoded = std::io::Cursor::new(Vec::new());
            source.write_to(&mut encoded, image::ImageFormat::Png).unwrap();
            let mut expected = source.to_rgba8();
            for pixel in expected.chunks_exact_mut(4) { pixel.swap(0, 2) }
            assert_eq!(decode_frame_pixels(encoded.get_ref()).unwrap(), expected);
            assert!(decode_frame(encoded.get_ref()).is_ok());
        }
        assert!(decode_frame_pixels(b"invalid").is_err());
    }
}
