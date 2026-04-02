use ratatui::layout::Rect;

pub mod highlight;
pub mod line_utils;
pub mod renderable;

/// A monotonically increasing counter used as a cache invalidation key.
///
/// Any mutable operation that changes renderable state should call `bump()`.
/// The rendering layer compares the current generation against a cached one to
/// decide whether a recomputation is necessary.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RenderGeneration(u64);

#[allow(dead_code)]
impl RenderGeneration {
    /// Advance the generation counter, invalidating all caches that hold
    /// the previous value.
    pub fn bump(&mut self) {
        self.0 = self.0.wrapping_add(1);
    }

    /// Return `true` when `cached` was produced by a different generation.
    pub fn is_stale(&self, cached: &Self) -> bool {
        self.0 != cached.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Insets {
    left: u16,
    top: u16,
    right: u16,
    bottom: u16,
}

impl Insets {
    pub fn tlbr(top: u16, left: u16, bottom: u16, right: u16) -> Self {
        Self {
            top,
            left,
            bottom,
            right,
        }
    }

    pub fn vh(v: u16, h: u16) -> Self {
        Self {
            top: v,
            left: h,
            bottom: v,
            right: h,
        }
    }
}

pub trait RectExt {
    fn inset(&self, insets: Insets) -> Rect;
}

impl RectExt for Rect {
    fn inset(&self, insets: Insets) -> Rect {
        let horizontal = insets.left.saturating_add(insets.right);
        let vertical = insets.top.saturating_add(insets.bottom);
        Self {
            x: self.x.saturating_add(insets.left),
            y: self.y.saturating_add(insets.top),
            width: self.width.saturating_sub(horizontal),
            height: self.height.saturating_sub(vertical),
        }
    }
}
