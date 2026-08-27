//! The core document model: a stack of same-sized RGBA layers.
//!
//! Layer 0 is the bottom of the stack; the last layer is the top. Every layer
//! owns a document-sized, non-premultiplied RGBA8 buffer, which keeps the
//! compositor free of per-layer bounds arithmetic. Source images smaller than
//! the document are pasted at the origin and padded with transparency; larger
//! ones are clipped.

use serde::{Deserialize, Serialize};

use crate::blend::BlendMode;

pub type LayerId = u64;

/// Bytes per pixel in every buffer this module produces or accepts.
pub const CHANNELS: usize = 4;

#[derive(Debug, Clone)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    /// `0.0..=1.0`, multiplied into the layer's own alpha at composite time.
    pub opacity: f32,
    pub blend_mode: BlendMode,
    /// Document-sized, non-premultiplied RGBA8.
    pub pixels: Vec<u8>,
}

/// The subset of a layer the UI needs — everything except the pixels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LayerView {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub blend_mode: BlendMode,
}

impl Layer {
    pub fn view(&self) -> LayerView {
        LayerView {
            id: self.id,
            name: self.name.clone(),
            visible: self.visible,
            opacity: self.opacity,
            blend_mode: self.blend_mode,
        }
    }

    /// Whether this layer can affect the composite at all.
    pub fn contributes(&self) -> bool {
        self.visible && self.opacity > 0.0
    }
}

#[derive(Debug, Clone)]
pub struct Document {
    width: u32,
    height: u32,
    /// Bottom-to-top.
    layers: Vec<Layer>,
    next_id: LayerId,
}

/// Where a layer should move in the stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MoveDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DocumentView {
    pub width: u32,
    pub height: u32,
    /// Bottom-to-top, matching the model. The UI reverses this for display.
    pub layers: Vec<LayerView>,
}

impl Document {
    /// A document with no layers. Flattening it yields fully transparent pixels.
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err(format!("A document cannot be {width}x{height}."));
        }
        Ok(Self {
            width,
            height,
            layers: Vec::new(),
            next_id: 1,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    pub fn view(&self) -> DocumentView {
        DocumentView {
            width: self.width,
            height: self.height,
            layers: self.layers.iter().map(Layer::view).collect(),
        }
    }

    /// Number of bytes in a document-sized RGBA8 buffer.
    pub fn buffer_len(&self) -> usize {
        self.width as usize * self.height as usize * CHANNELS
    }

    /// Add `source` (an RGBA8 buffer of `source_width` x `source_height`) as a new
    /// top layer, pasted at the origin and clipped or padded to document size.
    pub fn add_layer(
        &mut self,
        name: impl Into<String>,
        source: &[u8],
        source_width: u32,
        source_height: u32,
    ) -> Result<LayerId, String> {
        let expected = source_width as usize * source_height as usize * CHANNELS;
        if source.len() != expected {
            return Err(format!(
                "Expected {expected} bytes for a {source_width}x{source_height} RGBA image, got {}.",
                source.len()
            ));
        }

        let mut pixels = vec![0u8; self.buffer_len()];
        let copy_width = source_width.min(self.width) as usize * CHANNELS;
        let copy_height = source_height.min(self.height) as usize;
        let src_stride = source_width as usize * CHANNELS;
        let dst_stride = self.width as usize * CHANNELS;
        for row in 0..copy_height {
            let src = row * src_stride;
            let dst = row * dst_stride;
            pixels[dst..dst + copy_width].copy_from_slice(&source[src..src + copy_width]);
        }

        let id = self.next_id;
        self.next_id += 1;
        self.layers.push(Layer {
            id,
            name: name.into(),
            visible: true,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            pixels,
        });
        Ok(id)
    }

    fn index_of(&self, id: LayerId) -> Result<usize, String> {
        self.layers
            .iter()
            .position(|layer| layer.id == id)
            .ok_or_else(|| format!("No layer with id {id}."))
    }

    fn layer_mut(&mut self, id: LayerId) -> Result<&mut Layer, String> {
        let index = self.index_of(id)?;
        Ok(&mut self.layers[index])
    }

    pub fn set_visible(&mut self, id: LayerId, visible: bool) -> Result<(), String> {
        self.layer_mut(id)?.visible = visible;
        Ok(())
    }

    /// Values outside `0.0..=1.0` are clamped rather than rejected, so a slider
    /// that overshoots by a rounding step is not an error.
    pub fn set_opacity(&mut self, id: LayerId, opacity: f32) -> Result<(), String> {
        if !opacity.is_finite() {
            return Err(format!("Opacity must be a number, got {opacity}."));
        }
        self.layer_mut(id)?.opacity = opacity.clamp(0.0, 1.0);
        Ok(())
    }

    pub fn set_blend_mode(&mut self, id: LayerId, blend_mode: BlendMode) -> Result<(), String> {
        self.layer_mut(id)?.blend_mode = blend_mode;
        Ok(())
    }

    pub fn remove_layer(&mut self, id: LayerId) -> Result<(), String> {
        let index = self.index_of(id)?;
        self.layers.remove(index);
        Ok(())
    }

    /// Move a layer one step through the stack. Moving the top layer up (or the
    /// bottom layer down) is a no-op rather than an error — it is what a UI
    /// button press at the end of the stack should do.
    pub fn move_layer(&mut self, id: LayerId, direction: MoveDirection) -> Result<(), String> {
        let index = self.index_of(id)?;
        let target = match direction {
            MoveDirection::Up if index + 1 < self.layers.len() => index + 1,
            MoveDirection::Down if index > 0 => index - 1,
            _ => return Ok(()),
        };
        self.layers.swap(index, target);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `width` x `height` buffer where every pixel is `rgba`.
    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter()
            .copied()
            .cycle()
            .take(width as usize * height as usize * CHANNELS)
            .collect()
    }

    fn doc_with_one_layer() -> (Document, LayerId) {
        let mut doc = Document::new(2, 2).unwrap();
        let id = doc
            .add_layer("base", &solid(2, 2, [10, 20, 30, 255]), 2, 2)
            .unwrap();
        (doc, id)
    }

    #[test]
    fn a_new_document_has_no_layers() {
        let doc = Document::new(4, 3).unwrap();
        assert!(doc.layers().is_empty());
        assert_eq!((doc.width(), doc.height()), (4, 3));
        assert_eq!(doc.buffer_len(), 4 * 3 * 4);
    }

    #[test]
    fn zero_sized_documents_are_rejected() {
        assert!(Document::new(0, 5).is_err());
        assert!(Document::new(5, 0).is_err());
    }

    #[test]
    fn layers_default_to_visible_opaque_and_normal() {
        let (doc, id) = doc_with_one_layer();
        let layer = &doc.layers()[0];
        assert_eq!(layer.id, id);
        assert_eq!(layer.name, "base");
        assert!(layer.visible);
        assert_eq!(layer.opacity, 1.0);
        assert_eq!(layer.blend_mode, BlendMode::Normal);
        assert!(layer.contributes());
    }

    #[test]
    fn layer_ids_are_unique_even_after_removal() {
        let mut doc = Document::new(1, 1).unwrap();
        let first = doc.add_layer("a", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        doc.remove_layer(first).unwrap();
        let second = doc.add_layer("b", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn a_mismatched_source_buffer_is_rejected() {
        let mut doc = Document::new(2, 2).unwrap();
        let err = doc.add_layer("bad", &[0, 0, 0], 2, 2).unwrap_err();
        assert!(err.contains("Expected 16 bytes"), "{err}");
    }

    #[test]
    fn a_smaller_source_is_pasted_at_the_origin_and_padded() {
        let mut doc = Document::new(2, 2).unwrap();
        doc.add_layer("small", &solid(1, 1, [9, 9, 9, 255]), 1, 1)
            .unwrap();

        let pixels = &doc.layers()[0].pixels;
        assert_eq!(pixels.len(), 16);
        assert_eq!(&pixels[0..4], &[9, 9, 9, 255]); // top-left got the pixel
        assert_eq!(&pixels[4..16], &[0; 12]); // the rest is transparent
    }

    #[test]
    fn a_larger_source_is_clipped() {
        let mut doc = Document::new(1, 1).unwrap();
        doc.add_layer("big", &solid(2, 2, [7, 7, 7, 255]), 2, 2)
            .unwrap();

        let pixels = &doc.layers()[0].pixels;
        assert_eq!(pixels.len(), 4);
        assert_eq!(pixels, &[7, 7, 7, 255]);
    }

    #[test]
    fn a_wider_source_does_not_bleed_into_the_next_row() {
        // A 3x2 source into a 2x2 document: row 1 of the source must land on row 1
        // of the layer, not continue filling row 0.
        let mut doc = Document::new(2, 2).unwrap();
        let mut source = solid(3, 2, [1, 1, 1, 255]);
        // Mark the first pixel of the source's second row.
        source[3 * CHANNELS..3 * CHANNELS + CHANNELS].copy_from_slice(&[5, 5, 5, 255]);
        doc.add_layer("wide", &source, 3, 2).unwrap();

        let pixels = &doc.layers()[0].pixels;
        assert_eq!(&pixels[0..4], &[1, 1, 1, 255]);
        assert_eq!(&pixels[8..12], &[5, 5, 5, 255]); // start of layer row 1
    }

    #[test]
    fn visibility_opacity_and_blend_mode_round_trip() {
        let (mut doc, id) = doc_with_one_layer();

        doc.set_visible(id, false).unwrap();
        doc.set_opacity(id, 0.25).unwrap();
        doc.set_blend_mode(id, BlendMode::Multiply).unwrap();

        let view = &doc.view().layers[0];
        assert!(!view.visible);
        assert_eq!(view.opacity, 0.25);
        assert_eq!(view.blend_mode, BlendMode::Multiply);
        assert!(!doc.layers()[0].contributes());
    }

    #[test]
    fn opacity_is_clamped_and_nan_is_rejected() {
        let (mut doc, id) = doc_with_one_layer();

        doc.set_opacity(id, 1.5).unwrap();
        assert_eq!(doc.layers()[0].opacity, 1.0);
        doc.set_opacity(id, -0.5).unwrap();
        assert_eq!(doc.layers()[0].opacity, 0.0);
        assert!(doc.set_opacity(id, f32::NAN).is_err());
    }

    #[test]
    fn a_fully_transparent_layer_does_not_contribute() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_opacity(id, 0.0).unwrap();
        assert!(!doc.layers()[0].contributes());
    }

    #[test]
    fn operations_on_an_unknown_layer_are_errors() {
        let (mut doc, _) = doc_with_one_layer();
        assert!(doc.set_visible(999, true).is_err());
        assert!(doc.set_opacity(999, 0.5).is_err());
        assert!(doc.set_blend_mode(999, BlendMode::Screen).is_err());
        assert!(doc.remove_layer(999).is_err());
        assert!(doc.move_layer(999, MoveDirection::Up).is_err());
    }

    #[test]
    fn new_layers_go_on_top() {
        let mut doc = Document::new(1, 1).unwrap();
        let bottom = doc.add_layer("bottom", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let top = doc.add_layer("top", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let ids: Vec<_> = doc.layers().iter().map(|l| l.id).collect();
        assert_eq!(ids, vec![bottom, top]);
    }

    #[test]
    fn moving_reorders_the_stack() {
        let mut doc = Document::new(1, 1).unwrap();
        let a = doc.add_layer("a", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let b = doc.add_layer("b", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let c = doc.add_layer("c", &solid(1, 1, [0; 4]), 1, 1).unwrap();

        doc.move_layer(a, MoveDirection::Up).unwrap();
        assert_eq!(ids(&doc), vec![b, a, c]);

        doc.move_layer(c, MoveDirection::Down).unwrap();
        assert_eq!(ids(&doc), vec![b, c, a]);
    }

    #[test]
    fn moving_past_the_end_of_the_stack_is_a_no_op() {
        let mut doc = Document::new(1, 1).unwrap();
        let a = doc.add_layer("a", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let b = doc.add_layer("b", &solid(1, 1, [0; 4]), 1, 1).unwrap();

        doc.move_layer(b, MoveDirection::Up).unwrap();
        doc.move_layer(a, MoveDirection::Down).unwrap();
        assert_eq!(ids(&doc), vec![a, b]);
    }

    #[test]
    fn removing_takes_the_right_layer_out() {
        let mut doc = Document::new(1, 1).unwrap();
        let a = doc.add_layer("a", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let b = doc.add_layer("b", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        doc.remove_layer(a).unwrap();
        assert_eq!(ids(&doc), vec![b]);
    }

    fn ids(doc: &Document) -> Vec<LayerId> {
        doc.layers().iter().map(|l| l.id).collect()
    }
}
