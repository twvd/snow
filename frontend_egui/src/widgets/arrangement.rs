//! Where the displays of a machine with several go, as the guest arranges them

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};
use snow_core::emulator::comm::GuestScreen;

/// Where each display goes, in its pixels, and the size of the whole: the
/// displays the guest uses where it puts them, and the others in a row under
/// them, in their order. A display with no picture yet takes no room.
pub fn arrange(sizes: &[Vec2], screens: &[GuestScreen]) -> (Vec<Option<Rect>>, Vec2) {
    let mut places: Vec<Option<Rect>> = sizes
        .iter()
        .enumerate()
        .map(|(i, &size)| {
            let screen = screens.iter().find(|s| s.display == i)?;
            let at = pos2(f32::from(screen.left), f32::from(screen.top));
            Some(Rect::from_min_size(at, size)).filter(|_| size.x > 0.0 && size.y > 0.0)
        })
        .collect();

    // From the top left of the guest's screens
    let origin = places
        .iter()
        .flatten()
        .fold(None, |o: Option<Pos2>, r| {
            Some(o.map_or(r.min, |o| o.min(r.min)))
        })
        .unwrap_or(Pos2::ZERO);
    let mut whole = Vec2::ZERO;
    for r in places.iter_mut().flatten() {
        *r = r.translate(-origin.to_vec2());
        whole = whole.max(r.max.to_vec2());
    }

    let mut x = 0.0;
    let mut row = 0.0f32;
    for (place, &size) in places.iter_mut().zip(sizes) {
        if place.is_some() || size.x <= 0.0 || size.y <= 0.0 {
            continue;
        }
        *place = Some(Rect::from_min_size(pos2(x, whole.y), size));
        x += size.x;
        row = row.max(size.y);
    }
    (places, vec2(whole.x.max(x), whole.y + row))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(display: usize, left: i16, top: i16, size: Vec2) -> GuestScreen {
        GuestScreen {
            display,
            left,
            top,
            right: left + size.x as i16,
            bottom: top + size.y as i16,
            main: display == 0,
            active: true,
        }
    }

    fn rect(x: f32, y: f32, size: Vec2) -> Option<Rect> {
        Some(Rect::from_min_size(pos2(x, y), size))
    }

    const VGA: Vec2 = vec2(640.0, 480.0);
    const RGB12: Vec2 = vec2(512.0, 384.0);

    #[test]
    fn the_displays_go_where_the_guest_puts_them() {
        // The second screen left of the main one and lower: the whole starts at its left
        let (places, whole) = arrange(
            &[VGA, RGB12],
            &[screen(0, 0, 0, VGA), screen(1, -512, 100, RGB12)],
        );
        assert_eq!(places, [rect(512.0, 0.0, VGA), rect(0.0, 100.0, RGB12)]);
        assert_eq!(whole, vec2(1152.0, 484.0));
    }

    #[test]
    fn a_display_the_guest_doesnt_use_goes_under() {
        let (places, whole) = arrange(&[VGA, RGB12, VGA], &[screen(0, 0, 0, VGA)]);
        assert_eq!(
            places,
            [
                rect(0.0, 0.0, VGA),
                rect(0.0, 480.0, RGB12),
                rect(512.0, 480.0, VGA)
            ]
        );
        assert_eq!(whole, vec2(1152.0, 960.0));
    }

    #[test]
    fn a_display_without_a_picture_takes_no_room() {
        let (places, whole) = arrange(&[Vec2::ZERO, VGA], &[]);
        assert_eq!(places, [None, rect(0.0, 0.0, VGA)]);
        assert_eq!(whole, VGA);
    }
}
