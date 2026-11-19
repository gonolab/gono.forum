//! Image inspection and re-encoding: runs in a killable, time-limited child process, and an attachment does not
//! become available until it passes.
//!
//! Only image uploads are allowed, so there are no structural checks for other file types. Images are stripped of
//! metadata and embedded content by decoding and re-encoding, with no separate virus scan.
use super::*;
use image::{
    AnimationDecoder, ImageDecoder, ImageFormat, codecs::gif::Repeat, metadata::LoopCount,
};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Write},
    time::Instant,
};

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Inspection {
    pub mime_type: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub filesize: u64,
    pub content_sha256: String,
    /// List thumbnail: for a static image a `.thumb` is written next to `destination`, and this is its type; GIFs
    /// have none.
    #[serde(default)]
    pub thumbnail: Option<String>,
}

/// Thumbnail file: the `.thumb` next to the processing result.
pub(super) fn thumbnail_path(destination: &Path) -> PathBuf {
    destination.with_extension("thumb")
}

pub(super) fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, s)| s.to_ascii_lowercase())
        .unwrap_or_default()
}
/// Extensions allowed for upload: images only.
pub(super) fn allowed_extension(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg" | "png" | "webp" | "gif")
}

pub(super) fn inspect(
    source: &Path,
    destination: &Path,
    filename: &str,
    config: &MediaConfig,
) -> anyhow::Result<Inspection> {
    let started = Instant::now();
    let ext = extension(filename);
    // A non-image attachment that reaches the queue (for example one queued by an older version) is rejected as an
    // unsupported format.
    anyhow::ensure!(
        allowed_extension(&ext),
        "this format is no longer supported"
    );
    let metadata = std::fs::metadata(source)?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() > 0 && metadata.len() <= config.image_max_bytes,
        "the file is empty or over the limit"
    );
    let (mime, width, height, thumbnail) = image_file(source, destination, &ext, config, started)?;
    deadline(started, config)?;
    let mut file = File::open(destination)?;
    let mut sha = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        sha.update(&buffer[..count]);
    }
    File::options().write(true).open(destination)?.sync_all()?;
    Ok(Inspection {
        mime_type: mime,
        width,
        height,
        filesize: std::fs::metadata(destination)?.len(),
        content_sha256: format!("{:x}", sha.finalize()),
        thumbnail,
    })
}

fn deadline(started: Instant, config: &MediaConfig) -> anyhow::Result<()> {
    anyhow::ensure!(
        started.elapsed().as_secs() < config.processing_timeout_seconds,
        "file processing timed out"
    );
    Ok(())
}
pub(super) fn limits(config: &MediaConfig) -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(config.image_max_pixels.saturating_mul(8));
    limits.max_image_width = Some(config.image_max_pixels.min(u32::MAX as u64) as u32);
    limits.max_image_height = limits.max_image_width;
    limits
}
fn dimensions(w: u32, h: u32, config: &MediaConfig) -> anyhow::Result<()> {
    anyhow::ensure!(
        w > 0 && h > 0 && u64::from(w) * u64::from(h) <= config.image_max_pixels,
        "the image has too many pixels"
    );
    Ok(())
}

/// Quality for re-encoding JPEG (higher than the library default of 75).
const JPEG_QUALITY: u8 = 85;

/// Result of the image check: the processed type, width and height, and the thumbnail type (GIFs have no
/// thumbnail).
type ImageOutcome = (String, Option<i64>, Option<i64>, Option<String>);

fn image_file(
    source: &Path,
    destination: &Path,
    ext: &str,
    config: &MediaConfig,
    started: Instant,
) -> anyhow::Result<ImageOutcome> {
    let mut reader = image::ImageReader::open(source)?.with_guessed_format()?;
    reader.limits(limits(config));
    let format = reader.format().context("unrecognized image format")?;
    anyhow::ensure!(
        matches!(
            (ext, format),
            ("jpg" | "jpeg", ImageFormat::Jpeg)
                | ("png", ImageFormat::Png)
                | ("gif", ImageFormat::Gif)
                | ("webp", ImageFormat::WebP)
        ),
        "the image extension does not match its actual format"
    );
    if format == ImageFormat::Gif {
        let repeat = gif_repeat(source)?;
        let mut decoder = image::codecs::gif::GifDecoder::new(BufReader::new(File::open(source)?))?;
        decoder.set_limits(limits(config))?;
        let (w, h) = decoder.dimensions();
        dimensions(w, h, config)?;
        let color = SrgbConversion::from_icc(decoder.icc_profile());
        let frames = decoder.into_frames();
        animation(frames, repeat, color, destination, config, started)?;
        return Ok(("image/gif".into(), Some(w.into()), Some(h.into()), None));
    }
    if format == ImageFormat::WebP {
        let mut decoder =
            image::codecs::webp::WebPDecoder::new(BufReader::new(File::open(source)?))?;
        decoder.set_limits(limits(config))?;
        if decoder.has_animation() {
            let (w, h) = decoder.dimensions();
            dimensions(w, h, config)?;
            let repeat = plays_as_gif_repeat(decoder.loop_count());
            let color = SrgbConversion::from_icc(decoder.icc_profile());
            let frames = decoder.into_frames();
            animation(frames, repeat, color, destination, config, started)?;
            return Ok(("image/gif".into(), Some(w.into()), Some(h.into()), None));
        }
    }
    if format == ImageFormat::Png {
        let mut decoder = image::codecs::png::PngDecoder::new(BufReader::new(File::open(source)?))?;
        decoder.set_limits(limits(config))?;
        if decoder.is_apng()? {
            let (w, h) = decoder.dimensions();
            dimensions(w, h, config)?;
            let color = SrgbConversion::from_icc(decoder.icc_profile());
            let apng = decoder.apng()?;
            let repeat = plays_as_gif_repeat(apng.loop_count());
            animation(
                apng.into_frames(),
                repeat,
                color,
                destination,
                config,
                started,
            )?;
            return Ok(("image/gif".into(), Some(w.into()), Some(h.into()), None));
        }
    }
    let mut decoder = reader.into_decoder()?;
    let (w, h) = decoder.dimensions();
    dimensions(w, h, config)?;
    let orientation = decoder.orientation()?;
    let color = SrgbConversion::from_icc(decoder.icc_profile());
    let mut image = image::DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    // The output carries no profile, so its pixels have to be sRGB already; the thumbnail is scaled from the same
    // converted image.
    if let Some(color) = &color {
        color.apply(&mut image);
    }
    let (w, h) = (image.width(), image.height());
    deadline(started, config)?;
    let mut output = BufWriter::new(File::create(destination)?);
    // JPEG and static WebP without an alpha channel are output as JPEG (quality 85), everything else as PNG. The
    // pure-Rust WebP encoder can only do lossless, and PNG would make opaque WebP photos several times larger.
    let mime = if format == ImageFormat::Jpeg
        || (format == ImageFormat::WebP && !image.color().has_alpha())
    {
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, JPEG_QUALITY);
        image.to_rgb8().write_with_encoder(encoder)?;
        "image/jpeg"
    } else {
        image.write_to(&mut output, ImageFormat::Png)?;
        "image/png"
    };
    output.flush()?;
    // List thumbnail: the same decoded image is scaled down along the way, and the processing task stores it as a
    // second object.
    deadline(started, config)?;
    let (thumb, thumb_mime) = super::thumbnail::render(&image)?;
    std::fs::write(thumbnail_path(destination), thumb)?;
    Ok((
        mime.into(),
        Some(w.into()),
        Some(h.into()),
        Some(thumb_mime.into()),
    ))
}

/// The GIF converted from an animation may be at most this large: the same `file_max_bytes` as ordinary files.
/// Converting animated WebP and APNG to GIF can inflate them many times over, and limits on frame count and total
/// pixels do not bound the output bytes; static images have no such limit.
fn animation_limit_message(limit: u64) -> String {
    crate::i18n::current().media_error_animation_size(limit.div_ceil(1024 * 1024))
}

/// Output that fails once the limit is reached: encoding aborts as soon as it is exceeded, instead of judging
/// after the whole GIF is written.
///
/// The GIF encoder writes the trailer when it is dropped and discards any error, so the writer keeps the first
/// error itself (over the limit, or the disk refusing a write) for [`animation`] to check afterwards.
struct LimitedWriter<W> {
    inner: W,
    written: u64,
    limit: u64,
    failed: Option<std::io::Error>,
}

impl<W: Write> LimitedWriter<W> {
    fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            written: 0,
            limit,
            failed: None,
        }
    }

    fn keep<T>(&mut self, result: std::io::Result<T>) -> std::io::Result<T> {
        if let Err(error) = &result
            && self.failed.is_none()
        {
            self.failed = Some(std::io::Error::new(error.kind(), error.to_string()));
        }
        result
    }
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.written.saturating_add(buffer.len() as u64) > self.limit {
            let exceeded = Err(std::io::Error::other(animation_limit_message(self.limit)));
            return self.keep(exceeded);
        }
        let written = self.inner.write(buffer);
        let count = self.keep(written)?;
        self.written += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let flushed = self.inner.flush();
        self.keep(flushed)
    }
}

/// The loop extension of an uploaded GIF, read as it is in the file: `None` when it has none and plays once.
/// The image decoder reports such a GIF as looping forever, the same as a loop count of 0, so the count is read
/// with the gif crate directly.
fn gif_repeat(source: &Path) -> anyhow::Result<Option<Repeat>> {
    let decoder = gif::DecodeOptions::new().read_info(BufReader::new(File::open(source)?))?;
    Ok(match decoder.repeat() {
        gif::Repeat::Infinite => Some(Repeat::Infinite),
        gif::Repeat::Finite(0) => None,
        gif::Repeat::Finite(count) => Some(Repeat::Finite(count)),
    })
}

/// The GIF loop extension for an animated WebP or APNG. Those count plays (1 plays once, 0 forever); a GIF counts
/// repeats after the first play (0 forever), and one without the extension plays once.
fn plays_as_gif_repeat(plays: LoopCount) -> Option<Repeat> {
    match plays {
        LoopCount::Infinite => Some(Repeat::Infinite),
        LoopCount::Finite(plays) => match plays.get() - 1 {
            0 => None,
            repeats => Some(Repeat::Finite(u16::try_from(repeats).unwrap_or(u16::MAX))),
        },
    }
}

fn animation(
    frames: image::Frames<'_>,
    repeat: Option<Repeat>,
    color: Option<SrgbConversion>,
    destination: &Path,
    config: &MediaConfig,
    started: Instant,
) -> anyhow::Result<()> {
    let mut output = LimitedWriter::new(
        BufWriter::new(File::create(destination)?),
        config.file_max_bytes,
    );
    let encoded: anyhow::Result<()> = (|| {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut output);
        if let Some(repeat) = repeat {
            encoder.set_repeat(repeat)?;
        }
        let mut count = 0;
        let mut pixels = 0_u64;
        for frame in frames {
            deadline(started, config)?;
            let mut frame = frame?;
            count += 1;
            let (w, h) = frame.buffer().dimensions();
            dimensions(w, h, config)?;
            pixels = pixels.saturating_add(u64::from(w) * u64::from(h));
            anyhow::ensure!(
                count <= config.animation_max_frames && pixels <= config.animation_max_pixels,
                "the animation has too many frames or pixels"
            );
            if let Some(color) = &color {
                color.apply_rgba8(frame.buffer_mut());
            }
            encoder.encode_frame(frame)?;
        }
        anyhow::ensure!(count > 0, "the animation has no valid frames");
        // Dropping the encoder writes the trailer.
        Ok(())
    })();
    // A write that failed, the trailer's included, counts even when the encoder discarded it.
    if let Some(error) = output.failed.take() {
        return Err(error.into());
    }
    encoded?;
    output.flush()?;
    if let Some(error) = output.failed.take() {
        return Err(error.into());
    }
    Ok(())
}

/// Conversion of an image's pixels from the color space of its embedded ICC profile to sRGB. Re-encoding drops the
/// profile and browsers read an untagged image as sRGB, so a Display P3 photo would otherwise show shifted colors.
/// Only RGB profiles are converted. A profile that does not parse, that moxcms cannot build a transform for, or
/// that maps sample colors onto themselves (an sRGB profile) leaves the pixels as they are.
struct SrgbConversion {
    profile: moxcms::ColorProfile,
}

impl SrgbConversion {
    /// Pixels converted per call: the transform writes into a separate buffer, and converting in chunks keeps that
    /// buffer small instead of a second copy of the whole image.
    const CHUNK_PIXELS: usize = 16 * 1024;

    fn from_icc(icc: image::ImageResult<Option<Vec<u8>>>) -> Option<Self> {
        let icc = icc.ok().flatten()?;
        let profile = moxcms::ColorProfile::new_from_slice(&icc).ok()?;
        if profile.color_space != moxcms::DataColorSpace::Rgb {
            return None;
        }
        let conversion = Self { profile };
        // Already sRGB: sample a grid of colors and leave the pixels alone when none moves by more than rounding.
        let samples: Vec<u8> = (0..9_u16)
            .flat_map(|r| (0..9_u16).flat_map(move |g| (0..9_u16).map(move |b| (r, g, b))))
            .flat_map(|(r, g, b)| [r, g, b].map(|v| (v * 255 / 8) as u8))
            .collect();
        let transform = conversion.transform_8bit(moxcms::Layout::Rgb)?;
        let mut converted = vec![0; samples.len()];
        transform.transform(&samples, &mut converted).ok()?;
        let moved = samples
            .iter()
            .zip(&converted)
            .any(|(before, after)| before.abs_diff(*after) > 1);
        moved.then_some(conversion)
    }

    fn transform_8bit(&self, layout: moxcms::Layout) -> Option<Arc<moxcms::Transform8BitExecutor>> {
        let srgb = moxcms::ColorProfile::new_srgb();
        self.profile
            .create_transform_8bit(layout, &srgb, layout, moxcms::TransformOptions::default())
            .ok()
    }

    fn transform_16bit(
        &self,
        layout: moxcms::Layout,
    ) -> Option<Arc<moxcms::Transform16BitExecutor>> {
        let srgb = moxcms::ColorProfile::new_srgb();
        self.profile
            .create_transform_16bit(layout, &srgb, layout, moxcms::TransformOptions::default())
            .ok()
    }

    /// Converts in place; layouts other than 8- and 16-bit RGB(A) are left as they are.
    fn apply(&self, image: &mut image::DynamicImage) {
        use moxcms::Layout;
        match image {
            image::DynamicImage::ImageRgb8(buffer) => {
                if let Some(transform) = self.transform_8bit(Layout::Rgb) {
                    convert(transform.as_ref(), buffer, 3);
                }
            }
            image::DynamicImage::ImageRgba8(buffer) => self.apply_rgba8(buffer),
            image::DynamicImage::ImageRgb16(buffer) => {
                if let Some(transform) = self.transform_16bit(Layout::Rgb) {
                    convert(transform.as_ref(), buffer, 3);
                }
            }
            image::DynamicImage::ImageRgba16(buffer) => {
                if let Some(transform) = self.transform_16bit(Layout::Rgba) {
                    convert(transform.as_ref(), buffer, 4);
                }
            }
            _ => {}
        }
    }

    fn apply_rgba8(&self, buffer: &mut image::RgbaImage) {
        if let Some(transform) = self.transform_8bit(moxcms::Layout::Rgba) {
            convert(transform.as_ref(), buffer, 4);
        }
    }
}

/// Runs `transform` over the samples chunk by chunk; a chunk the transform refuses stays as it was.
fn convert<T: Copy + Default>(
    transform: &(dyn moxcms::TransformExecutor<T> + Send + Sync),
    samples: &mut [T],
    channels: usize,
) {
    let mut scratch = vec![T::default(); SrgbConversion::CHUNK_PIXELS * channels];
    for chunk in samples.chunks_mut(SrgbConversion::CHUNK_PIXELS * channels) {
        let target = &mut scratch[..chunk.len()];
        if transform.transform(chunk, target).is_ok() {
            chunk.copy_from_slice(target);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animated_images_enforce_frame_and_pixel_budgets() {
        let root =
            std::env::temp_dir().join(format!("gonof-animation-{}", identity::random_token()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("image.gif");
        let mut encoder = image::codecs::gif::GifEncoder::new(File::create(&source).unwrap());
        for _ in 0..2 {
            encoder
                .encode_frame(image::Frame::new(image::RgbaImage::new(2, 2)))
                .unwrap();
        }
        drop(encoder);
        for config in [
            MediaConfig {
                animation_max_frames: 1,
                ..Default::default()
            },
            MediaConfig {
                animation_max_pixels: 7,
                ..Default::default()
            },
            MediaConfig {
                image_max_pixels: 3,
                ..Default::default()
            },
        ] {
            assert!(inspect(&source, &root.join("result"), "image.gif", &config).is_err());
        }
        assert!(
            inspect(
                &source,
                &root.join("result"),
                "image.gif",
                &MediaConfig::default()
            )
            .is_ok()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Static WebP without an alpha channel is output as JPEG, with alpha as PNG; static images all get a
    /// thumbnail, animations do not.
    #[test]
    fn static_webp_without_alpha_becomes_jpeg() {
        let root =
            std::env::temp_dir().join(format!("gono-forum-webp-{}", identity::random_token()));
        std::fs::create_dir_all(&root).unwrap();
        let write = |name: &str, image: image::DynamicImage| {
            let path = root.join(name);
            let mut bytes = Vec::new();
            image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
                .encode(
                    image.as_bytes(),
                    image.width(),
                    image.height(),
                    image.color().into(),
                )
                .unwrap();
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let opaque = write(
            "photo.webp",
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                64,
                48,
                image::Rgb([200, 100, 50]),
            )),
        );
        let result = inspect(
            &opaque,
            &root.join("opaque"),
            "photo.webp",
            &MediaConfig::default(),
        )
        .unwrap();
        assert_eq!(result.mime_type, "image/jpeg");
        assert_eq!(
            image::guess_format(&std::fs::read(root.join("opaque")).unwrap()).unwrap(),
            ImageFormat::Jpeg
        );
        assert!(result.thumbnail.is_some());
        let transparent = write(
            "logo.webp",
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                64,
                48,
                image::Rgba([200, 100, 50, 10]),
            )),
        );
        let result = inspect(
            &transparent,
            &root.join("transparent"),
            "logo.webp",
            &MediaConfig::default(),
        )
        .unwrap();
        assert_eq!(result.mime_type, "image/png");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The loop count read from the source is written to the output: a GIF that repeats twice still repeats twice, one
    /// without a loop extension still plays once, and WebP and APNG play counts become GIF repeat counts.
    #[test]
    fn animations_keep_their_loop_count() {
        let root = std::env::temp_dir().join(format!("gonof-loops-{}", identity::random_token()));
        std::fs::create_dir_all(&root).unwrap();
        for (name, repeat, expected) in [
            ("twice.gif", Some(Repeat::Finite(2)), gif::Repeat::Finite(2)),
            ("once.gif", None, gif::Repeat::Finite(0)),
            ("forever.gif", Some(Repeat::Infinite), gif::Repeat::Infinite),
        ] {
            let source = root.join(name);
            let mut encoder = image::codecs::gif::GifEncoder::new(File::create(&source).unwrap());
            if let Some(repeat) = repeat {
                encoder.set_repeat(repeat).unwrap();
            }
            for shade in [0, 255] {
                let frame = image::RgbaImage::from_pixel(2, 2, image::Rgba([shade, 0, 0, 255]));
                encoder.encode_frame(image::Frame::new(frame)).unwrap();
            }
            drop(encoder);
            let output = root.join(format!("{name}.out"));
            inspect(&source, &output, name, &MediaConfig::default()).unwrap();
            let decoder = gif::DecodeOptions::new()
                .read_info(File::open(&output).unwrap())
                .unwrap();
            assert_eq!(decoder.repeat(), expected, "{name}");
        }
        let plays = |count: u32| LoopCount::Finite(std::num::NonZeroU32::new(count).unwrap());
        assert!(matches!(
            plays_as_gif_repeat(plays(3)),
            Some(Repeat::Finite(2))
        ));
        assert!(plays_as_gif_repeat(plays(1)).is_none());
        assert!(matches!(
            plays_as_gif_repeat(plays(u32::MAX)),
            Some(Repeat::Finite(u16::MAX))
        ));
        assert!(matches!(
            plays_as_gif_repeat(LoopCount::Infinite),
            Some(Repeat::Infinite)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The GIF trailer is part of the output: one byte over the limit rejects the animation instead of passing a
    /// file whose trailer write failed unnoticed.
    #[test]
    fn animated_output_one_byte_over_the_limit_is_rejected() {
        let root = std::env::temp_dir().join(format!("gonof-trailer-{}", identity::random_token()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("two.gif");
        let mut encoder = image::codecs::gif::GifEncoder::new(File::create(&source).unwrap());
        for shade in [0, 255] {
            let frame = image::RgbaImage::from_pixel(2, 2, image::Rgba([0, shade, 0, 255]));
            encoder.encode_frame(image::Frame::new(frame)).unwrap();
        }
        drop(encoder);
        let full = inspect(
            &source,
            &root.join("full"),
            "two.gif",
            &MediaConfig::default(),
        )
        .unwrap()
        .filesize;
        let at_limit = MediaConfig {
            file_max_bytes: full,
            ..Default::default()
        };
        inspect(&source, &root.join("exact"), "two.gif", &at_limit).unwrap();
        assert_eq!(
            std::fs::read(root.join("exact")).unwrap().last(),
            Some(&0x3B)
        );
        let one_short = MediaConfig {
            file_max_bytes: full - 1,
            ..Default::default()
        };
        let error = inspect(&source, &root.join("short"), "two.gif", &one_short).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("After processing, the animation is over the"),
            "{error:#}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Pixels described by a Display P3 profile are converted to sRGB, for the image, its thumbnail and animation
    /// frames alike; an sRGB profile and a profile that does not parse leave the pixels as they are.
    #[test]
    fn display_p3_images_are_converted_to_srgb() {
        let root = std::env::temp_dir().join(format!("gonof-p3-{}", identity::random_token()));
        std::fs::create_dir_all(&root).unwrap();
        let p3 = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        let srgb = moxcms::ColorProfile::new_srgb().encode().unwrap();
        let png = |name: &str, icc: &[u8]| {
            let path = root.join(name);
            let image = image::RgbImage::from_pixel(64, 48, image::Rgb([200, 127, 0]));
            let mut bytes = Vec::new();
            let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
            image::ImageEncoder::set_icc_profile(&mut encoder, icc.to_vec()).unwrap();
            image::ImageEncoder::write_image(
                encoder,
                image.as_raw(),
                64,
                48,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let pixel = |path: &Path| {
            image::ImageReader::open(path)
                .unwrap()
                .with_guessed_format()
                .unwrap()
                .decode()
                .unwrap()
                .to_rgb8()
                .get_pixel(10, 10)
                .0
        };
        let near = |actual: [u8; 3], expected: [u8; 3], slack: u8| {
            actual
                .iter()
                .zip(expected)
                .all(|(a, e)| a.abs_diff(e) <= slack)
        };
        let output = root.join("p3.out");
        inspect(
            &png("p3.png", &p3),
            &output,
            "p3.png",
            &MediaConfig::default(),
        )
        .unwrap();
        assert!(
            near(pixel(&output), [212, 123, 0], 1),
            "{:?}",
            pixel(&output)
        );
        let thumbnail = pixel(&thumbnail_path(&output));
        assert!(near(thumbnail, [212, 123, 0], 3), "{thumbnail:?}");
        for (name, icc) in [
            ("srgb.png", &srgb[..]),
            ("broken.png", &b"not a profile"[..]),
        ] {
            let output = root.join(format!("{name}.out"));
            inspect(&png(name, icc), &output, name, &MediaConfig::default()).unwrap();
            assert_eq!(pixel(&output), [200, 127, 0], "{name}");
        }
        // An animated GIF with an ICC application extension: every frame is converted.
        let source = root.join("p3.gif");
        {
            let mut file = File::create(&source).unwrap();
            let mut encoder = gif::Encoder::new(&mut file, 2, 2, &[]).unwrap();
            encoder
                .write_raw_extension(gif::AnyExtension(0xFF), &[b"ICCRGBG1012", &p3])
                .unwrap();
            for _ in 0..2 {
                let mut pixels = [200, 127, 0, 255].repeat(4);
                encoder
                    .write_frame(&gif::Frame::from_rgba(2, 2, &mut pixels))
                    .unwrap();
            }
        }
        let output = root.join("p3-gif.out");
        inspect(&source, &output, "p3.gif", &MediaConfig::default()).unwrap();
        let decoder =
            image::codecs::gif::GifDecoder::new(BufReader::new(File::open(&output).unwrap()))
                .unwrap();
        let frames = decoder.into_frames().collect_frames().unwrap();
        assert_eq!(frames.len(), 2);
        for frame in frames {
            let [r, g, b, _] = frame.buffer().get_pixel(1, 1).0;
            assert!(near([r, g, b], [212, 123, 0], 1), "{:?}", [r, g, b]);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A GIF converted from an animation is rejected if it exceeds `file_max_bytes`; static images that grow after
    /// processing are not bound by this limit.
    #[test]
    fn animated_output_is_capped_but_static_output_is_not() {
        let root =
            std::env::temp_dir().join(format!("gonof-animation-cap-{}", identity::random_token()));
        std::fs::create_dir_all(&root).unwrap();
        let noise = |seed: u32| {
            image::RgbaImage::from_fn(64, 64, |x, y| {
                let v = (x * 31 + y * 17 + seed * 101).wrapping_mul(2_654_435_761) >> 8;
                image::Rgba([v as u8, (v >> 8) as u8, (v >> 16) as u8, 255])
            })
        };
        let source = root.join("noise.gif");
        let mut encoder = image::codecs::gif::GifEncoder::new(File::create(&source).unwrap());
        for seed in 0..4 {
            encoder
                .encode_frame(image::Frame::new(noise(seed)))
                .unwrap();
        }
        drop(encoder);
        let size = std::fs::metadata(&source).unwrap().len();
        let capped = MediaConfig {
            file_max_bytes: size / 2,
            ..Default::default()
        };
        let error = inspect(&source, &root.join("capped"), "noise.gif", &capped).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("After processing, the animation is over the"),
            "an animation over the limit must be rejected: {error:#}"
        );
        assert!(
            inspect(
                &source,
                &root.join("full"),
                "noise.gif",
                &MediaConfig::default()
            )
            .is_ok()
        );
        // A static PNG that is larger than file_max_bytes after processing still passes.
        let png = root.join("noise.png");
        noise(9).save(&png).unwrap();
        let processed = inspect(&png, &root.join("static"), "noise.png", &capped).unwrap();
        assert!(processed.filesize > capped.file_max_bytes);
        std::fs::remove_dir_all(root).unwrap();
    }
}
