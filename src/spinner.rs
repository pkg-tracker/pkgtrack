//! Animated "packages on a turntable" loading indicator, drawn on a canvas.
use crate::Message;
use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Theme};
use std::f32::consts::TAU;
use std::sync::OnceLock;
use std::time::Instant;

fn start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// Seconds since the app started; the animation is a pure function of time.
fn t() -> f32 {
    start().elapsed().as_secs_f32()
}

struct Spinner;

fn rgb(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::from_rgba8(r, g, b, a)
}

/// An isometric package box (same colours as the app icon) with a tape stripe.
fn cube(frame: &mut Frame, c: Point, s: f32, alpha: f32) {
    let p = |dx: f32, dy: f32| Point::new(c.x + dx * s, c.y + dy * s);
    let poly = |pts: &[Point]| {
        Path::new(|b| {
            b.move_to(pts[0]);
            for q in &pts[1..] {
                b.line_to(*q);
            }
            b.close();
        })
    };
    let top = poly(&[p(0.0, -1.0), p(1.0, -0.5), p(0.0, 0.0), p(-1.0, -0.5)]);
    let left = poly(&[p(-1.0, -0.5), p(0.0, 0.0), p(0.0, 1.0), p(-1.0, 0.5)]);
    let right = poly(&[p(0.0, 0.0), p(1.0, -0.5), p(1.0, 0.5), p(0.0, 1.0)]);
    frame.fill(&top, rgb(0xff, 0xff, 0xff, alpha));
    frame.fill(&left, rgb(0xe0, 0xe7, 0xff, alpha));
    frame.fill(&right, rgb(0xc4, 0xb5, 0xfd, alpha));
    // tape across the top face
    let tape = poly(&[p(-0.5, -0.75), p(-0.3, -0.85), p(0.7, -0.35), p(0.5, -0.25)]);
    frame.fill(&tape, rgb(0xa7, 0x8b, 0xfa, alpha * 0.9));
    let edge = Stroke::default().with_width((s * 0.06).max(1.0)).with_color(rgb(0x4c, 0x1d, 0x95, alpha * 0.75));
    for f in [&top, &left, &right] {
        frame.stroke(f, edge);
    }
}

fn ellipse(c: Point, rx: f32, ry: f32) -> Path {
    Path::new(|b| {
        for i in 0..=48 {
            let a = i as f32 / 48.0 * TAU;
            let q = Point::new(c.x + rx * a.cos(), c.y + ry * a.sin());
            if i == 0 {
                b.move_to(q);
            } else {
                b.line_to(q);
            }
        }
        b.close();
    })
}

impl canvas::Program<Message> for Spinner {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, theme: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let size = bounds.width.min(bounds.height);
        let c = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let t = t();
        let r = size * 0.34; // orbit radius
        let tilt = 0.38; // flattening: we look at the turntable from above
        let s = size * 0.09; // box size

        // Turntable (pallet) under the boxes.
        let accent = theme.extended_palette().primary.base.color;
        frame.fill(&ellipse(Point::new(c.x, c.y + s * 1.2), r * 1.18, r * tilt * 1.18 + s * 0.3), Color { a: 0.18, ..accent });
        frame.stroke(
            &ellipse(Point::new(c.x, c.y + s * 1.2), r * 1.18, r * tilt * 1.18 + s * 0.3),
            Stroke::default().with_width(2.0).with_color(Color { a: 0.45, ..accent }),
        );

        // Orbiting boxes, sorted back to front.
        let n = 7;
        let mut boxes: Vec<(f32, Point, f32)> = (0..n)
            .map(|i| {
                let a = t * 1.3 + i as f32 / n as f32 * TAU;
                let depth = a.sin(); // -1 back .. 1 front
                let hop = ((t * 4.0 + i as f32 * 1.7).sin()).max(0.0) * s * 0.35;
                let p = Point::new(c.x + r * a.cos(), c.y + r * tilt * a.sin() - hop);
                (depth, p, s * (0.8 + 0.2 * depth))
            })
            .collect();
        boxes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let center_y = c.y - s * 1.1 + (t * 2.2).sin() * s * 0.15;
        let mut center_drawn = false;
        for (depth, p, sz) in boxes {
            if depth > 0.0 && !center_drawn {
                cube(&mut frame, Point::new(c.x, center_y), s * 2.2, 1.0);
                center_drawn = true;
            }
            cube(&mut frame, p, sz, 0.55 + 0.45 * (depth + 1.0) / 2.0);
        }
        if !center_drawn {
            cube(&mut frame, Point::new(c.x, center_y), s * 2.2, 1.0);
        }
        vec![frame.into_geometry()]
    }
}

/// The animation at a given size (pixels).
pub fn spinner<'a>(size: f32) -> Element<'a, Message> {
    let _ = start();
    canvas::Canvas::new(Spinner).width(Length::Fixed(size)).height(Length::Fixed(size)).into()
}
