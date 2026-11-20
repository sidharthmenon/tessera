use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// Overlap of two rects; `None` when the overlap has no area.
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = (self.x + self.w).min(o.x + o.w);
        let b = (self.y + self.h).min(o.y + o.h);
        (r > x && b > y).then(|| Rect { x, y, w: r - x, h: b - y })
    }

    /// Affine map of `self` from `from`-space into `to`-space.
    pub fn map_between(&self, from: &Rect, to: &Rect) -> Rect {
        let (sx, sy) = (to.w / from.w, to.h / from.h);
        Rect {
            x: to.x + (self.x - from.x) * sx,
            y: to.y + (self.y - from.y) * sy,
            w: self.w * sx,
            h: self.h * sy,
        }
    }

    /// Normalized (0..1) coordinates of `self` within `outer`.
    pub fn uv_in(&self, outer: &Rect) -> Rect {
        self.map_between(outer, &Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn r(x: f32, y: f32, w: f32, h: f32) -> Rect { Rect { x, y, w, h } }

    #[test]
    fn rect_intersect_disjoint_is_none() {
        assert_eq!(r(0., 0., 10., 10.).intersect(&r(20., 20., 5., 5.)), None);
    }
    #[test]
    fn rect_intersect_overlap() {
        assert_eq!(r(0., 0., 10., 10.).intersect(&r(5., 5., 10., 10.)), Some(r(5., 5., 5., 5.)));
    }
    #[test]
    fn rect_map_between_half_split() {
        let full = r(0., 0., 1920., 1080.);
        assert_eq!(full.map_between(&full, &full), full);
        assert_eq!(
            r(960., 0., 960., 1080.).map_between(&full, &r(100., 0., 960., 540.)),
            r(580., 0., 480., 540.)
        );
    }
    #[test]
    fn rect_uv_in() {
        assert_eq!(
            r(960., 0., 960., 1080.).uv_in(&r(0., 0., 3840., 1080.)),
            r(0.25, 0., 0.25, 1.)
        );
    }
}
