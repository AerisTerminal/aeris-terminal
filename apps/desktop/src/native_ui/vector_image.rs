use std::{
    collections::{HashMap, VecDeque},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    App, AssetSource, DevicePixels, Global, ImageSource, IntoElement, ObjectFit, Pixels,
    RenderImage, RenderOnce, SvgRenderer, SvgSize, Window, div, img, prelude::*, px, rgba, size,
};
use num_traits::ToPrimitive;

use crate::desktop::assets::{AxiusflowAssets, VectorAsset, VectorFit, validate_vector_asset};

const MAX_CONCURRENT_RASTER_JOBS: usize = 2;
const MAX_PENDING_RASTER_JOBS: usize = 32;
const MAX_CACHE_ENTRIES: usize = 96;
const MAX_CACHE_DECODED_BYTES: usize = 32 * 1024 * 1024;
const MAX_NEGATIVE_CACHE_ENTRIES: usize = 64;
const MAX_VECTOR_EDGE_PIXELS: u32 = 2_048;
const MAX_RENDITION_DECODED_BYTES: usize = 16 * 1024 * 1024;
const FAILURE_BACKOFF: Duration = Duration::from_secs(2);
const DIAGNOSTIC_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct RasterKey {
    asset: VectorAsset,
    revision: u64,
    width: u32,
    height: u32,
    fit: VectorFit,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum VectorImageFailure {
    UnknownEmbeddedAsset,
    InvalidMetadataOrDimensions,
    SvgParseFailure,
    RasterizationFailure,
    ResourceBoundsExceeded,
    WorkQueueSaturated,
    RetiredRequest,
    RendererUnavailable,
}

#[derive(Clone, Copy, Debug)]
struct RasterGeometry {
    key: RasterKey,
    logical_width: Pixels,
    logical_height: Pixels,
}

struct CacheEntry {
    image: Arc<RenderImage>,
    decoded_bytes: usize,
    last_used: u64,
}

#[derive(Clone, Copy)]
struct RasterJob {
    key: RasterKey,
    generation: u64,
}

struct VectorImageStore {
    renderer: Option<SvgRenderer>,
    completed: HashMap<RasterKey, CacheEntry>,
    pending: HashMap<RasterKey, u64>,
    queue: VecDeque<RasterJob>,
    negative: HashMap<RasterKey, (VectorImageFailure, Instant)>,
    diagnostics: HashMap<(VectorAsset, VectorImageFailure), Instant>,
    decoded_bytes: usize,
    active_jobs: usize,
    clock: u64,
    next_generation: u64,
    shutting_down: bool,
}

impl Global for VectorImageStore {}

enum RequestResult {
    Ready(Arc<RenderImage>),
    Pending(Option<Arc<RenderImage>>),
    Failed(VectorImageFailure),
}

impl VectorImageStore {
    fn new(renderer: SvgRenderer) -> Self {
        Self {
            renderer: Some(renderer),
            completed: HashMap::new(),
            pending: HashMap::new(),
            queue: VecDeque::new(),
            negative: HashMap::new(),
            diagnostics: HashMap::new(),
            decoded_bytes: 0,
            active_jobs: 0,
            clock: 0,
            next_generation: 1,
            shutting_down: false,
        }
    }

    fn request(&mut self, geometry: RasterGeometry) -> (RequestResult, Option<RasterJob>) {
        self.clock = self.clock.saturating_add(1);
        if let Some(entry) = self.completed.get_mut(&geometry.key) {
            entry.last_used = self.clock;
            return (RequestResult::Ready(Arc::clone(&entry.image)), None);
        }
        let temporary = self.best_higher_resolution(geometry.key);
        if self.shutting_down || self.renderer.is_none() {
            return (
                temporary.map_or(
                    RequestResult::Failed(VectorImageFailure::RendererUnavailable),
                    |image| RequestResult::Pending(Some(image)),
                ),
                None,
            );
        }
        if self.pending.contains_key(&geometry.key) {
            return (RequestResult::Pending(temporary), None);
        }
        if let Some((failure, until)) = self.negative.get(&geometry.key).copied() {
            if Instant::now() < until {
                return (
                    temporary.map_or(RequestResult::Failed(failure), |image| {
                        RequestResult::Pending(Some(image))
                    }),
                    None,
                );
            }
            self.negative.remove(&geometry.key);
        }
        if self.active_jobs + self.queue.len()
            >= MAX_CONCURRENT_RASTER_JOBS + MAX_PENDING_RASTER_JOBS
        {
            if let Some(position) = self
                .queue
                .iter()
                .position(|queued| queued.key.asset == geometry.key.asset)
                && let Some(retired) = self.queue.remove(position)
            {
                self.pending.remove(&retired.key);
                self.record_failure(retired.key, VectorImageFailure::RetiredRequest);
            } else {
                self.record_failure(geometry.key, VectorImageFailure::WorkQueueSaturated);
                return (
                    temporary.map_or(
                        RequestResult::Failed(VectorImageFailure::WorkQueueSaturated),
                        |image| RequestResult::Pending(Some(image)),
                    ),
                    None,
                );
            }
        }

        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        let job = RasterJob {
            key: geometry.key,
            generation,
        };
        self.pending.insert(geometry.key, generation);
        if self.active_jobs < MAX_CONCURRENT_RASTER_JOBS {
            self.active_jobs += 1;
            (RequestResult::Pending(temporary), Some(job))
        } else {
            self.queue.push_back(job);
            (RequestResult::Pending(temporary), None)
        }
    }

    fn best_higher_resolution(&self, requested: RasterKey) -> Option<Arc<RenderImage>> {
        self.completed
            .iter()
            .filter(|(key, _)| {
                key.asset == requested.asset
                    && key.revision == requested.revision
                    && key.fit == requested.fit
                    && key.width >= requested.width
                    && key.height >= requested.height
            })
            .min_by_key(|(key, _)| u64::from(key.width) * u64::from(key.height))
            .map(|(_, entry)| Arc::clone(&entry.image))
    }

    fn finish(
        &mut self,
        job: RasterJob,
        result: Result<Arc<RenderImage>, VectorImageFailure>,
    ) -> Option<RasterJob> {
        self.active_jobs = self.active_jobs.saturating_sub(1);
        let current_generation = self.pending.remove(&job.key);
        if current_generation == Some(job.generation) {
            match result {
                Ok(image) => self.insert_completed(job.key, image),
                Err(failure) => self.record_failure(job.key, failure),
            }
        } else {
            self.record_failure(job.key, VectorImageFailure::RetiredRequest);
        }

        if self.shutting_down {
            self.queue.clear();
            self.pending.clear();
            return None;
        }
        let next = self.queue.pop_front();
        if next.is_some() {
            self.active_jobs += 1;
        }
        next
    }

    fn insert_completed(&mut self, key: RasterKey, image: Arc<RenderImage>) {
        self.clock = self.clock.saturating_add(1);
        let decoded_bytes = decoded_bytes(key.width, key.height).unwrap_or(usize::MAX);
        if decoded_bytes > MAX_CACHE_DECODED_BYTES {
            self.record_failure(key, VectorImageFailure::ResourceBoundsExceeded);
            return;
        }
        self.negative.remove(&key);
        if let Some(replaced) = self.completed.insert(
            key,
            CacheEntry {
                image,
                decoded_bytes,
                last_used: self.clock,
            },
        ) {
            self.decoded_bytes = self.decoded_bytes.saturating_sub(replaced.decoded_bytes);
        }
        self.decoded_bytes = self.decoded_bytes.saturating_add(decoded_bytes);
        while self.completed.len() > MAX_CACHE_ENTRIES
            || self.decoded_bytes > MAX_CACHE_DECODED_BYTES
        {
            let Some(lru_key) = self
                .completed
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some(evicted) = self.completed.remove(&lru_key) {
                self.decoded_bytes = self.decoded_bytes.saturating_sub(evicted.decoded_bytes);
            }
        }
    }

    fn record_failure(&mut self, key: RasterKey, failure: VectorImageFailure) {
        let now = Instant::now();
        if self.negative.len() >= MAX_NEGATIVE_CACHE_ENTRIES
            && !self.negative.contains_key(&key)
            && let Some(oldest) = self
                .negative
                .iter()
                .min_by_key(|(_, (_, until))| *until)
                .map(|(key, _)| *key)
        {
            self.negative.remove(&oldest);
        }
        self.negative.insert(key, (failure, now + FAILURE_BACKOFF));
        let diagnostic_key = (key.asset, failure);
        let should_emit = self
            .diagnostics
            .get(&diagnostic_key)
            .is_none_or(|last| now.saturating_duration_since(*last) >= DIAGNOSTIC_BACKOFF);
        if should_emit {
            eprintln!(
                "Axiusflow vector image degraded: asset={} category={failure:?}",
                key.asset.identity()
            );
            self.diagnostics.insert(diagnostic_key, now);
        }
        if self.diagnostics.len() > VectorAsset::ALL.len() * 2 {
            self.diagnostics
                .retain(|_, last| now.saturating_duration_since(*last) < DIAGNOSTIC_BACKOFF);
        }
    }

    fn begin_shutdown(&mut self) {
        self.shutting_down = true;
        self.renderer = None;
        self.queue.clear();
        self.pending.clear();
    }
}

pub(crate) fn init(cx: &mut App) {
    if !cx.has_global::<VectorImageStore>() {
        cx.set_global(VectorImageStore::new(cx.svg_renderer()));
        cx.on_app_quit(|cx| {
            cx.update_global::<VectorImageStore, _>(|store, _| store.begin_shutdown());
            async {}
        })
        .detach();
    }
}

fn schedule(job: RasterJob, cx: &mut App) {
    let renderer = cx
        .try_global::<VectorImageStore>()
        .and_then(|store| store.renderer.clone());
    let Some(renderer) = renderer else {
        finish_job(job, Err(VectorImageFailure::RendererUnavailable), cx);
        return;
    };
    let task = cx.background_executor().spawn(async move {
        catch_unwind(AssertUnwindSafe(|| rasterize(job.key, &renderer)))
            .unwrap_or(Err(VectorImageFailure::RasterizationFailure))
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| finish_job(job, result, cx));
    })
    .detach();
}

fn finish_job(job: RasterJob, result: Result<Arc<RenderImage>, VectorImageFailure>, cx: &mut App) {
    let next = cx.update_global::<VectorImageStore, _>(|store, _| store.finish(job, result));
    cx.refresh_windows();
    if let Some(next) = next {
        schedule(next, cx);
    }
}

fn rasterize(
    key: RasterKey,
    renderer: &SvgRenderer,
) -> Result<Arc<RenderImage>, VectorImageFailure> {
    let spec = key.asset.spec();
    let bytes = AxiusflowAssets
        .load(spec.path)
        .map_err(|_| VectorImageFailure::UnknownEmbeddedAsset)?
        .ok_or(VectorImageFailure::UnknownEmbeddedAsset)?;
    validate_vector_asset(key.asset, &bytes)
        .map_err(|_| VectorImageFailure::InvalidMetadataOrDimensions)?;
    let parsed = renderer
        .parse_svg(&bytes)
        .map_err(|_| VectorImageFailure::SvgParseFailure)?;
    renderer
        .render_parsed(
            &parsed,
            SvgSize::ExactSize(size(
                DevicePixels::from(key.width),
                DevicePixels::from(key.height),
            )),
        )
        .map_err(|_| VectorImageFailure::RasterizationFailure)
}

fn decoded_bytes(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)
}

fn raster_geometry(
    asset: VectorAsset,
    logical_width: Pixels,
    logical_height: Pixels,
    window_scale: f32,
) -> Result<RasterGeometry, VectorImageFailure> {
    let width = f32::from(logical_width);
    let height = f32::from(logical_height);
    if !width.is_finite()
        || !height.is_finite()
        || !window_scale.is_finite()
        || width <= 0.0
        || height <= 0.0
        || window_scale <= 0.0
    {
        return Err(VectorImageFailure::InvalidMetadataOrDimensions);
    }
    let box_width = rounded_positive_u32(f64::from(width) * f64::from(window_scale))
        .ok_or(VectorImageFailure::InvalidMetadataOrDimensions)?;
    let box_height = rounded_positive_u32(f64::from(height) * f64::from(window_scale))
        .ok_or(VectorImageFailure::InvalidMetadataOrDimensions)?;
    let spec = asset.spec();
    if spec.view_box_width == 0 || spec.view_box_height == 0 {
        return Err(VectorImageFailure::InvalidMetadataOrDimensions);
    }
    let source_ratio = f64::from(spec.view_box_width) / f64::from(spec.view_box_height);
    let box_ratio = f64::from(box_width) / f64::from(box_height);
    let (raster_width, raster_height) = match spec.fit {
        VectorFit::Stretch => (box_width, box_height),
        VectorFit::Contain if box_ratio > source_ratio => (
            rounded_positive_u32(f64::from(box_height) * source_ratio)
                .ok_or(VectorImageFailure::InvalidMetadataOrDimensions)?,
            box_height,
        ),
        VectorFit::Contain => (
            box_width,
            rounded_positive_u32(f64::from(box_width) / source_ratio)
                .ok_or(VectorImageFailure::InvalidMetadataOrDimensions)?,
        ),
        VectorFit::Cover if box_ratio > source_ratio => (
            box_width,
            rounded_positive_u32(f64::from(box_width) / source_ratio)
                .ok_or(VectorImageFailure::InvalidMetadataOrDimensions)?,
        ),
        VectorFit::Cover => (
            rounded_positive_u32(f64::from(box_height) * source_ratio)
                .ok_or(VectorImageFailure::InvalidMetadataOrDimensions)?,
            box_height,
        ),
    };
    if raster_width > MAX_VECTOR_EDGE_PIXELS
        || raster_height > MAX_VECTOR_EDGE_PIXELS
        || decoded_bytes(raster_width, raster_height)
            .is_none_or(|bytes| bytes > MAX_RENDITION_DECODED_BYTES)
    {
        return Err(VectorImageFailure::ResourceBoundsExceeded);
    }
    Ok(RasterGeometry {
        key: RasterKey {
            asset,
            revision: spec.content_revision,
            width: raster_width,
            height: raster_height,
            fit: spec.fit,
        },
        logical_width: px(raster_width
            .to_f32()
            .ok_or(VectorImageFailure::InvalidMetadataOrDimensions)?
            / window_scale),
        logical_height: px(raster_height
            .to_f32()
            .ok_or(VectorImageFailure::InvalidMetadataOrDimensions)?
            / window_scale),
    })
}

fn rounded_positive_u32(value: f64) -> Option<u32> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    value.round().max(1.0).to_u32()
}

#[derive(Clone, IntoElement)]
pub(crate) struct VectorImage {
    asset: VectorAsset,
    width: Pixels,
    height: Pixels,
}

impl VectorImage {
    pub(crate) fn new(asset: impl Into<VectorAsset>, width: Pixels, height: Pixels) -> Self {
        Self {
            asset: asset.into(),
            width,
            height,
        }
    }

    pub(crate) fn square(asset: impl Into<VectorAsset>, size: Pixels) -> Self {
        let asset = asset.into();
        let spec = asset.spec();
        assert_eq!(
            spec.view_box_width, spec.view_box_height,
            "square vector constructor requires square typed metadata"
        );
        Self::new(asset, size, size)
    }
}

impl RenderOnce for VectorImage {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        init(cx);
        let geometry = raster_geometry(self.asset, self.width, self.height, window.scale_factor());
        let (geometry, request, job) = match geometry {
            Ok(geometry) => {
                let result =
                    cx.update_global::<VectorImageStore, _>(|store, _| store.request(geometry));
                (Some(geometry), result.0, result.1)
            }
            Err(failure) => (None, RequestResult::Failed(failure), None),
        };
        if let Some(job) = job {
            schedule(job, cx);
        }

        let container = div()
            .w(self.width)
            .h(self.height)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden();
        match (geometry, request) {
            (Some(geometry), RequestResult::Ready(image) | RequestResult::Pending(Some(image))) => {
                container.child(
                    img(ImageSource::Render(image))
                        .w(geometry.logical_width)
                        .h(geometry.logical_height)
                        .flex_none()
                        .object_fit(ObjectFit::Fill),
                )
            }
            (_, RequestResult::Failed(failure)) => {
                let color = placeholder_color(Some(failure));
                let marker =
                    px((f32::from(self.width).min(f32::from(self.height)) * 0.42).max(2.0));
                container.child(div().size(marker).rounded_full().bg(color))
            }
            (Some(_), RequestResult::Pending(None)) => {
                let color = placeholder_color(None);
                let marker =
                    px((f32::from(self.width).min(f32::from(self.height)) * 0.42).max(2.0));
                container.child(div().size(marker).rounded_full().bg(color))
            }
            (None, RequestResult::Ready(_) | RequestResult::Pending(_)) => {
                unreachable!("a raster result requires validated geometry")
            }
        }
    }
}

fn placeholder_color(failure: Option<VectorImageFailure>) -> gpui::Rgba {
    match failure {
        Some(_) => rgba(0xa855_4555),
        None => rgba(0x7f8a_9a55),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    fn geometry(asset: VectorAsset, width: f32, height: f32, scale: f32) -> RasterGeometry {
        raster_geometry(asset, px(width), px(height), scale).expect("valid raster geometry")
    }

    #[test]
    fn contain_preserves_square_assets_at_fractional_dpi() {
        let geometry = geometry(
            VectorAsset::from(crate::desktop::assets::BrandIcon::Mark),
            72.0,
            48.0,
            1.25,
        );
        assert_eq!((geometry.key.width, geometry.key.height), (60, 60));
        assert!((f32::from(geometry.logical_width) - 48.0).abs() < f32::EPSILON);
        assert!((f32::from(geometry.logical_height) - 48.0).abs() < f32::EPSILON);
    }

    #[test]
    fn wordmark_contain_preserves_declared_aspect_ratio() {
        let geometry = geometry(
            VectorAsset::from(crate::desktop::assets::BrandIcon::WordmarkDarkText),
            188.0,
            80.0,
            1.5,
        );
        assert_eq!(geometry.key.width, 282);
        assert_eq!(geometry.key.height, 81);
    }

    #[test]
    fn dimensions_are_exact_and_stable_across_supported_scales() {
        for (scale, expected) in [
            (1.0, 18),
            (1.25, 23),
            (1.5, 27),
            (1.75, 32),
            (2.0, 36),
            (3.0, 54),
        ] {
            let geometry = geometry(
                VectorAsset::from(crate::desktop::assets::SeriesIcon::Candlestick),
                18.0,
                18.0,
                scale,
            );
            assert_eq!(
                (geometry.key.width, geometry.key.height),
                (expected, expected)
            );
        }
    }

    #[test]
    fn every_vector_rasterizes_at_supported_windows_scales() {
        let renderer = SvgRenderer::new(Arc::new(AxiusflowAssets));
        for asset in VectorAsset::ALL {
            for scale in [1.0, 1.25, 1.5, 1.75, 2.0, 3.0] {
                let geometry = geometry(asset, 18.0, 18.0, scale);
                let image = rasterize(geometry.key, &renderer).unwrap_or_else(|failure| {
                    panic!("{} failed at {scale}: {failure:?}", asset.identity())
                });
                assert_eq!(
                    image.size(0),
                    size(
                        DevicePixels::from(geometry.key.width),
                        DevicePixels::from(geometry.key.height)
                    )
                );
            }
        }
    }

    #[test]
    fn invalid_and_oversized_requests_are_rejected() {
        let asset = VectorAsset::from(crate::desktop::assets::BrandIcon::Mark);
        assert!(raster_geometry(asset, px(0.0), px(20.0), 1.0).is_err());
        assert!(raster_geometry(asset, px(f32::NAN), px(20.0), 1.0).is_err());
        assert!(raster_geometry(asset, px(2_049.0), px(2_049.0), 1.0).is_err());
    }

    #[test]
    fn cache_is_bounded_by_entries_and_bytes() {
        let renderer = SvgRenderer::new(Arc::new(AxiusflowAssets));
        let mut store = VectorImageStore::new(renderer);
        let asset = VectorAsset::from(crate::desktop::assets::BrandIcon::Mark);
        for index in 0..=u32::try_from(MAX_CACHE_ENTRIES).expect("entry limit fits u32") {
            let logical_size = 8.0 + index.to_f32().expect("test index fits f32");
            let mut key = geometry(asset, logical_size, logical_size, 1.0).key;
            key.revision = u64::from(index);
            let image = rasterize(key, store.renderer.as_ref().expect("renderer")).expect("raster");
            store.insert_completed(key, image);
        }
        assert!(store.completed.len() <= MAX_CACHE_ENTRIES);
        assert!(store.decoded_bytes <= MAX_CACHE_DECODED_BYTES);
    }

    #[test]
    fn byte_eviction_releases_only_the_store_reference() {
        let renderer = SvgRenderer::new(Arc::new(AxiusflowAssets));
        let mut store = VectorImageStore::new(renderer.clone());
        let asset = VectorAsset::from(crate::desktop::assets::BrandIcon::Mark);
        let geometry = geometry(asset, 32.0, 32.0, 1.0);
        let image = rasterize(geometry.key, &renderer).expect("shared raster");
        let retained_by_element = Arc::clone(&image);
        for revision in 1..=3 {
            let mut key = geometry.key;
            key.revision = revision;
            key.width = 2_048;
            key.height = 2_048;
            store.insert_completed(key, Arc::clone(&image));
        }
        assert!(store.completed.len() <= 2);
        assert!(store.decoded_bytes <= MAX_CACHE_DECODED_BYTES);
        assert!(retained_by_element.as_bytes(0).is_some());
    }

    #[test]
    fn negative_cache_and_shutdown_are_bounded() {
        let renderer = SvgRenderer::new(Arc::new(AxiusflowAssets));
        let mut store = VectorImageStore::new(renderer);
        let asset = VectorAsset::from(crate::desktop::assets::BrandIcon::Mark);
        for revision in 0..(MAX_NEGATIVE_CACHE_ENTRIES + 10) {
            let mut key = geometry(asset, 24.0, 24.0, 1.0).key;
            key.revision = revision as u64;
            store.record_failure(key, VectorImageFailure::SvgParseFailure);
        }
        assert!(store.negative.len() <= MAX_NEGATIVE_CACHE_ENTRIES);
        store.begin_shutdown();
        let (request, job) = store.request(geometry(asset, 24.0, 24.0, 1.0));
        assert!(matches!(
            request,
            RequestResult::Failed(VectorImageFailure::RendererUnavailable)
        ));
        assert!(job.is_none());
        assert!(store.queue.is_empty() && store.pending.is_empty());
    }

    #[test]
    fn exact_requests_coalesce_and_queue_is_bounded() {
        let renderer = SvgRenderer::new(Arc::new(AxiusflowAssets));
        let mut store = VectorImageStore::new(renderer);
        let asset = VectorAsset::from(crate::desktop::assets::BrandIcon::Mark);
        let first = geometry(asset, 20.0, 20.0, 1.0);
        let (_, first_job) = store.request(first);
        assert!(first_job.is_some());
        let (_, duplicate_job) = store.request(first);
        assert!(duplicate_job.is_none());
        assert_eq!(store.pending.len(), 1);

        let request_count = u32::try_from(MAX_PENDING_RASTER_JOBS + MAX_CONCURRENT_RASTER_JOBS + 4)
            .expect("queue limit fits u32");
        for index in 1..=request_count {
            let logical_size = 20.0 + index.to_f32().expect("test index fits f32");
            let request = geometry(asset, logical_size, logical_size, 1.0);
            let _ = store.request(request);
        }
        assert!(store.queue.len() <= MAX_PENDING_RASTER_JOBS);
        assert!(store.pending.len() <= MAX_PENDING_RASTER_JOBS + MAX_CONCURRENT_RASTER_JOBS);
        assert!(
            store
                .negative
                .values()
                .any(|(failure, _)| *failure == VectorImageFailure::RetiredRequest)
        );
    }

    #[test]
    fn representative_raster_golden_digests_are_stable() {
        let renderer = SvgRenderer::new(Arc::new(AxiusflowAssets));
        for (asset, width, height, expected) in [
            (
                VectorAsset::from(crate::desktop::assets::BrandIcon::Mark),
                108,
                108,
                "dbe70845999322d4ba55fd44a275ec6323f6f62dc88ef6a5640eef628769e221",
            ),
            (
                VectorAsset::from(crate::desktop::assets::BrandIcon::WordmarkDarkText),
                376,
                108,
                "838003fca3f915f83ae190bc1da3aa6b0b9f6a3ae104ec57badf50ac6c0006c9",
            ),
            (
                VectorAsset::from(crate::desktop::assets::ExchangeLogo::Rithmic),
                64,
                64,
                "5079e77fd56c16f56fa85c3ce110cf0e42738288b9d4de844051af70ba16acca",
            ),
        ] {
            let spec = asset.spec();
            let key = RasterKey {
                asset,
                revision: spec.content_revision,
                width,
                height,
                fit: spec.fit,
            };
            let image = rasterize(key, &renderer).expect("golden raster");
            let digest = Sha256::digest(image.as_bytes(0).unwrap());
            let mut digest_hex = String::with_capacity(digest.len() * 2);
            for byte in digest {
                write!(&mut digest_hex, "{byte:02x}").expect("write digest");
            }
            assert_eq!(
                digest_hex,
                expected,
                "raster golden changed for {}",
                asset.identity()
            );
        }
    }

    #[test]
    #[ignore = "run in release mode to report representative vector performance"]
    fn measure_vector_raster_performance() {
        let renderer = SvgRenderer::new(Arc::new(AxiusflowAssets));
        for asset in [
            VectorAsset::from(crate::desktop::assets::ExchangeLogo::Rithmic),
            VectorAsset::from(crate::desktop::assets::BrandIcon::Mark),
            VectorAsset::from(crate::desktop::assets::ExchangeLogo::Binance),
        ] {
            let geometry = geometry(asset, 64.0, 64.0, 2.0);
            let mut samples = Vec::new();
            for _ in 0..20 {
                let started = Instant::now();
                let image = rasterize(geometry.key, &renderer).expect("benchmark raster");
                std::hint::black_box(image.as_bytes(0));
                samples.push(started.elapsed());
            }
            samples.sort_unstable();
            println!(
                "vector cold asset={} median_us={} p95_us={}",
                asset.identity(),
                samples[samples.len() / 2].as_micros(),
                samples[samples.len() * 19 / 20].as_micros()
            );
        }

        let geometry = geometry(
            VectorAsset::from(crate::desktop::assets::BrandIcon::Mark),
            64.0,
            64.0,
            2.0,
        );
        let mut store = VectorImageStore::new(renderer.clone());
        let image = rasterize(geometry.key, &renderer).expect("warm benchmark raster");
        store.insert_completed(geometry.key, image);
        let started = Instant::now();
        for _ in 0..100_000 {
            let (result, job) = store.request(geometry);
            assert!(matches!(result, RequestResult::Ready(_)) && job.is_none());
        }
        println!(
            "vector warm cache average_ns={}",
            started.elapsed().as_nanos() / 100_000
        );
    }
}
