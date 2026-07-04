//! Deferred-drop GPU textures.
//!
//! Dropping an `egui::TextureHandle` frees the GPU texture immediately. When
//! that happens mid-frame — after the texture was painted or uploaded earlier
//! in the same `update()` pass — egui-wgpu destroys it *before* the frame's
//! commands are submitted, and the submit panics with "Texture ... has been
//! destroyed". This crashed the app repeatedly, each time from a different
//! eviction site that forgot the rule: fast typing into the filter, closing
//! the release picker, the vinyl grid's jump-to-catalog badge.
//!
//! The fix is architectural rather than per-call-site: cached textures are
//! never held as raw `TextureHandle`s but as [`Tex`], whose `Drop` parks the
//! handle in the shared [`TexGraveyard`] instead of freeing it. The `App`
//! empties the graveyard exactly once per frame, at the top of `update()` —
//! at that point the frame that last painted those textures has already been
//! submitted to the GPU, so the frees can no longer race it.
//!
//! Consequently, evicting cache entries with plain `remove` / `retain` /
//! `clear` / overwrite is always safe, from any point in the frame. There is
//! no destruction discipline left for call sites to forget: the only rule is
//! to store [`Tex`] (wrap handles at creation with [`TexGraveyard::wrap`]),
//! and the type system enforces that wherever a cache field says so.

use eframe::egui;
use std::sync::{Arc, Mutex};

/// Shared parking lot for retired texture handles. Cheap to clone (one `Arc`);
/// every [`Tex`] carries a clone so its `Drop` knows where to park.
#[derive(Clone, Default)]
pub(crate) struct TexGraveyard(Arc<Mutex<Vec<egui::TextureHandle>>>);

impl TexGraveyard {
    /// Wrap a freshly created texture so its eventual drop is deferred to the
    /// next frame boundary. Every `ctx.load_texture` result that gets cached
    /// must pass through here.
    pub(crate) fn wrap(&self, handle: egui::TextureHandle) -> Tex {
        Tex {
            handle: Some(handle),
            graveyard: self.clone(),
        }
    }

    /// Free every parked texture. Called only at the top of `update()`,
    /// before anything paints or uploads.
    pub(crate) fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

/// A GPU texture whose destruction is deferred to the next frame boundary
/// (see the module docs). Behaves like `egui::TextureHandle` for painting:
/// it derefs to one, and `&Tex` converts into an `egui::ImageSource`.
pub(crate) struct Tex {
    /// `Some` until dropped; taken exactly once, by `Drop`.
    handle: Option<egui::TextureHandle>,
    graveyard: TexGraveyard,
}

impl Drop for Tex {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            self.graveyard.0.lock().unwrap().push(h);
        }
    }
}

impl Clone for Tex {
    fn clone(&self) -> Self {
        self.graveyard.wrap(self.raw().clone())
    }
}

impl Tex {
    fn raw(&self) -> &egui::TextureHandle {
        // Invariant: `handle` is only `None` after `Drop` ran.
        self.handle.as_ref().expect("Tex used after drop")
    }
}

impl std::ops::Deref for Tex {
    type Target = egui::TextureHandle;
    fn deref(&self) -> &egui::TextureHandle {
        self.raw()
    }
}

/// Lets paint sites pass `&Tex` straight to `egui::Image::new`.
impl<'a> From<&'a Tex> for egui::ImageSource<'a> {
    fn from(t: &'a Tex) -> Self {
        egui::ImageSource::Texture(egui::load::SizedTexture::from(t.raw()))
    }
}
