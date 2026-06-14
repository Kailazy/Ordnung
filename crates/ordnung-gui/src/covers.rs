//! Split out of `main.rs`; part of the GUI `App`.
use super::*;

impl App {
    /// Ask the worker to load `id`'s thumbnail unless it's already cached or in
    /// flight. Marks the entry `Loading` so we don't re-request it every frame
    /// while the decode runs; the actual disk read + PNG decode happen off-thread.
    pub(crate) fn request_thumb(&mut self, id: Id) {
        if self.cover_cache.contains_key(&id) {
            return;
        }
        self.cover_cache.insert(id, ThumbState::Loading);
        let _ = self.thumb_req_tx.send(id);
    }


    /// Drain finished thumbnail decodes from the worker, uploading each to a GPU
    /// texture (a UI-thread-only op) and caching it. Called once per frame.
    pub(crate) fn poll_thumbs(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.thumb_rx.try_recv() {
            let tex = msg.image.map(|img| {
                ctx.load_texture(format!("cover-{}", msg.id), img, egui::TextureOptions::LINEAR)
            });
            self.cover_cache.insert(msg.id, ThumbState::Ready(tex));
        }
    }


    /// Ask the vinyl-cover worker to decode `instance_id`'s cached cover unless
    /// it's already loaded or in flight. Mirrors `request_thumb` for table rows.
    pub(crate) fn request_vinyl_cover(&mut self, instance_id: u64) {
        if self.vinyl_covers.contains_key(&instance_id) {
            return;
        }
        self.vinyl_covers.insert(instance_id, ThumbState::Loading);
        let _ = self.vinyl_cover_req_tx.send(instance_id);
    }


    /// Drain finished vinyl-cover decodes, uploading each to a texture. Called
    /// once per frame alongside `poll_thumbs`.
    pub(crate) fn poll_vinyl_covers(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.vinyl_cover_rx.try_recv() {
            let tex = msg.image.map(|img| {
                ctx.load_texture(format!("vinyl-{}", msg.id), img, egui::TextureOptions::LINEAR)
            });
            self.vinyl_covers.insert(msg.id, ThumbState::Ready(tex));
        }
    }


    /// Full-resolution cover for the inspector preview, loaded asynchronously so
    /// a multi-megapixel decode never blocks the UI thread. Returns the cached
    /// texture once ready (`None` when no cover exists). On a cache miss it kicks
    /// off a background decode (deduplicated via `cover_inflight`) and returns
    /// `None` for now; the worker repaints when the texture is ready.
    /// `source_path` is needed because high-quality embedded art is read from
    /// the source file rather than the catalog's small thumbnail.
    pub(crate) fn cover_full_texture(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        source_path: &str,
    ) -> Option<egui::TextureHandle> {
        if let Some(entry) = self.cover_full_cache.get(&id) {
            return entry.clone();
        }
        // Not decoded yet — spawn a loader unless one is already running for it.
        if self.cover_inflight.insert(id) {
            let tx = self.cover_tx.clone();
            let ctx = ctx.clone();
            let db = self.db_path.clone();
            let path = source_path.to_string();
            thread::spawn(move || {
                let image = load_full_cover_image(&db, id, &path);
                let _ = tx.send(CoverLoaded { id, image });
                ctx.request_repaint();
            });
        }
        None
    }


    /// Drain finished cover decodes, turning each into a texture on the UI thread
    /// (texture upload must happen here) and caching it. Called once per frame.
    pub(crate) fn poll_covers(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.cover_rx.try_recv() {
            self.cover_inflight.remove(&msg.id);
            let tex = msg.image.map(|img| {
                ctx.load_texture(
                    format!("cover-full-{}", msg.id),
                    img,
                    egui::TextureOptions::LINEAR,
                )
            });
            self.cover_full_cache.insert(msg.id, tex);
        }
    }


}

/// Decode one candidate thumbnail PNG into an egui texture (or `None` on failure).
pub(crate) fn decode_thumb(
    ctx: &egui::Context,
    track_id: Id,
    idx: usize,
    png: &[u8],
) -> Option<egui::TextureHandle> {
    if png.is_empty() {
        return None;
    }
    let img = image::load_from_memory(png).ok()?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let pixels = rgba.into_raw();
    let color = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
    Some(ctx.load_texture(
        format!("art-cand-{track_id}-{idx}"),
        color,
        egui::TextureOptions::LINEAR,
    ))
}


/// Load and decode the best available cover for `id` into egui pixels. Runs on
/// a background thread (the decode of a multi-megapixel source is the expensive
/// part), so it takes only owned data and never touches the UI. Returns `None`
/// when the track has no usable cover.
///
/// Source preference — always falls back so the inspector shows a cover whenever
/// the table row does, just sharper when a full-res source exists:
///   1. embedded art read straight from the file (full quality, capped to 1024px)
///   2. full-resolution fetched (Discogs) art
///   3. the 96px embedded thumbnail stored at scan time
///   4. the small fetched external thumbnail
///
/// Steps 1–2 are the high-quality path; 3–4 match the table thumbnails. When the
/// user chose to overwrite this track's cover with fetched art (`prefer_external`),
/// the full-resolution external art is promoted ahead of the embedded file so the
/// inspector shows what will be embedded on the next write.
/// Pick the best available cover-art image bytes for `id`, honouring the track's
/// embedded-vs-external preference. Returns the raw encoded image (PNG/JPEG) bytes.
pub(crate) fn load_full_cover_bytes(db: &Path, id: Id, source_path: &str) -> Option<Vec<u8>> {
    let catalog = Catalog::open(db).ok();
    let prefer_external = catalog
        .as_ref()
        .map(|c| c.prefers_external_artwork(id).unwrap_or(false))
        .unwrap_or(false);
    let embedded_file = || scan::read_front_cover_png(source_path).ok().flatten();
    let external_full = || {
        catalog.as_ref().and_then(|c| c.get_external_artwork_full(id).ok().flatten())
    };
    let fallbacks = || {
        catalog.as_ref().and_then(|c| {
            c.get_cover_thumb(id)
                .ok()
                .flatten()
                .or_else(|| c.get_external_artwork(id).ok().flatten())
        })
    };
    if prefer_external {
        external_full().or_else(embedded_file).or_else(fallbacks)
    } else {
        embedded_file().or_else(external_full).or_else(fallbacks)
    }
}


pub(crate) fn load_full_cover_image(db: &Path, id: Id, source_path: &str) -> Option<egui::ColorImage> {
    let bytes = load_full_cover_bytes(db, id, source_path)?;
    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, &rgba.into_raw()))
}


/// Resolve the now-playing track's cover into a `file://` URL the OS media panel
/// can load: the cover bytes are written to a stable per-track temp file. Falls
/// back to the Ordnung logo when the track has no artwork of its own, so the OS
/// Now Playing panel shows our logo rather than the host process's icon (which is
/// a terminal placeholder when the binary runs unbundled).
pub(crate) fn now_playing_cover_url(db: &Path, id: Id, source_path: &str) -> Option<String> {
    if let Some(bytes) = load_full_cover_bytes(db, id, source_path) {
        let mut path = std::env::temp_dir();
        path.push(format!("ordnung-nowplaying-{id}.img"));
        if std::fs::write(&path, &bytes).is_ok() {
            return Some(format!("file://{}", path.to_string_lossy()));
        }
    }
    now_playing_logo_url()
}


/// Write the embedded app logo to a stable temp file (once) and return its
/// `file://` URL, used as the Now Playing cover for tracks without artwork.
pub(crate) fn now_playing_logo_url() -> Option<String> {
    const LOGO: &[u8] = include_bytes!("../assets/icon.png");
    let mut path = std::env::temp_dir();
    path.push("ordnung-nowplaying-logo.png");
    if !path.exists() {
        std::fs::write(&path, LOGO).ok()?;
    }
    Some(format!("file://{}", path.to_string_lossy()))
}


/// Spawn the persistent thumbnail loader. It opens the catalog once and then
/// serves load requests over `req_rx` forever, reading + decoding each cover off
/// the UI thread and handing the pixels back over `tx`. After every decode it
/// nudges a repaint so the new texture appears promptly. The thread exits only
/// when the request channel closes (i.e. the app is shutting down).
pub(crate) fn spawn_thumb_loader(
    db: PathBuf,
    ctx: egui::Context,
    req_rx: Receiver<Id>,
    tx: Sender<CoverLoaded>,
) {
    thread::spawn(move || {
        let catalog = match Catalog::open(&db) {
            Ok(c) => c,
            Err(_) => return,
        };
        while let Ok(id) = req_rx.recv() {
            let image = load_thumb_image(&catalog, id);
            if tx.send(CoverLoaded { id, image }).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
}


/// Persistent loader for vinyl-collection cover art: one long-lived catalog
/// connection decodes each record's cached cover PNG off the UI thread, keyed by
/// Discogs `instance_id` (carried in `CoverLoaded::id`).
pub(crate) fn spawn_vinyl_cover_loader(
    db: PathBuf,
    ctx: egui::Context,
    req_rx: Receiver<u64>,
    tx: Sender<CoverLoaded>,
) {
    thread::spawn(move || {
        let catalog = match Catalog::open(&db) {
            Ok(c) => c,
            Err(_) => return,
        };
        while let Ok(instance_id) = req_rx.recv() {
            let image = catalog
                .vinyl_cover(instance_id)
                .ok()
                .flatten()
                .and_then(|bytes| image::load_from_memory(&bytes).ok())
                .map(|img| {
                    let rgba = img.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    egui::ColorImage::from_rgba_unmultiplied(size, &rgba.into_raw())
                });
            if tx.send(CoverLoaded { id: instance_id, image }).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
}


/// Read and decode a track's small table thumbnail using an already-open catalog
/// connection. Embedded art (captured at scan time) wins, except when the user
/// chose to overwrite this track's cover with fetched art (`prefer_external`), in
/// which case the external art wins. `None` means the track has no cover.
pub(crate) fn load_thumb_image(catalog: &Catalog, id: Id) -> Option<egui::ColorImage> {
    let embedded = || catalog.get_cover_thumb(id).ok().flatten();
    let external = || catalog.get_external_artwork(id).ok().flatten();
    let bytes = if catalog.prefers_external_artwork(id).unwrap_or(false) {
        external().or_else(embedded)
    } else {
        embedded().or_else(external)
    }?;
    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, &rgba.into_raw()))
}


