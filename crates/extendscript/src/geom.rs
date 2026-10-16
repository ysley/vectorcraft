//! Coordinates. A script sees the page with y pointing up and the origin at the bottom-left
//! corner of the first artboard, and a rectangle as `[left, top, right, bottom]`: the layout the
//! scripting reference gives for documents a script works on (the origin at the bottom left of
//! the artboard, y increasing from bottom to top). The engine's page has y pointing down.

use serde_json::Value;

/// Where a script's (0, 0) is, in engine coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Origin {
    x: f64,
    y: f64,
}

impl Origin {
    /// The origin of the document summary's first artboard (the page's own origin without one).
    pub fn of(doc: &Value) -> Self {
        let first = doc.get("artboards").and_then(Value::as_array).and_then(|a| a.first());
        let num = |k: &str| first.and_then(|a| a.get(k)).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0);
        Self { x: num("x"), y: num("y") + num("height") }
    }

    pub fn to_engine(self, x: f64, y: f64) -> (f64, f64) {
        (self.x + x, self.y - y)
    }

    pub fn to_script(self, x: f64, y: f64) -> (f64, f64) {
        (x - self.x, self.y - y)
    }

    /// An engine rectangle `{x, y, width, height}` as `[left, top, right, bottom]`.
    pub fn rect_to_script(self, r: &Value) -> Option<[f64; 4]> {
        let num = |k: &str| r.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
        let (x, y, w, h) = (num("x")?, num("y")?, num("width")?, num("height")?);
        let (left, top) = self.to_script(x, y);
        let (right, bottom) = self.to_script(x + w, y + h);
        Some([left, top, right, bottom])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_origin_is_the_bottom_left_of_the_first_artboard_and_y_flips() {
        let doc = json!({"artboards": [{"x": 10.0, "y": 20.0, "width": 100.0, "height": 50.0}]});
        let o = Origin::of(&doc);
        assert_eq!(o.to_script(10.0, 70.0), (0.0, 0.0));
        assert_eq!(o.to_script(110.0, 20.0), (100.0, 50.0));
        assert_eq!(o.to_engine(100.0, 50.0), (110.0, 20.0));
        let r = json!({"x": 10.0, "y": 20.0, "width": 30.0, "height": 10.0});
        assert_eq!(o.rect_to_script(&r), Some([0.0, 50.0, 30.0, 40.0]));
    }

    #[test]
    fn a_document_without_artboards_uses_the_page_origin() {
        assert_eq!(Origin::of(&json!({})).to_script(5.0, 7.0), (5.0, -7.0));
    }
}
