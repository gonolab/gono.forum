//! Image inspection and re-encoding: runs in a killable, time-limited child process, and an attachment does not
//! become available until it passes.
//!
//! Only image uploads are allowed, so there are no structural checks for other file types. Images are stripped of
//! metadata and embedded content by decoding and re-encoding, with no separate virus scan.
use super::*;
use image::{AnimationDecoder, ImageDecoder, ImageFormat};
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
        let mut decoder = image::codecs::gif::GifDecoder::new(BufReader::new(File::open(source)?))?;
        decoder.set_limits(limits(config))?;
        let (w, h) = decoder.dimensions();
        dimensions(w, h, config)?;
        animation(decoder.into_frames(), destination, config, started)?;
        return Ok(("image/gif".into(), Some(w.into()), Some(h.into()), None));
    }
    if format == ImageFormat::WebP {
        let mut decoder =
            image::codecs::webp::WebPDecoder::new(BufReader::new(File::open(source)?))?;
        decoder.set_limits(limits(config))?;
        if decoder.has_animation() {
            let (w, h) = decoder.dimensions();
            dimensions(w, h, config)?;
            animation(decoder.into_frames(), destination, config, started)?;
            return Ok(("image/gif".into(), Some(w.into()), Some(h.into()), None));
        }
    }
    if format == ImageFormat::Png {
        let mut decoder = image::codecs::png::PngDecoder::new(BufReader::new(File::open(source)?))?;
        decoder.set_limits(limits(config))?;
        if decoder.is_apng()? {
            let (w, h) = decoder.dimensions();
            dimensions(w, h, config)?;
            animation(decoder.apng()?.into_frames(), destination, config, started)?;
            return Ok(("image/gif".into(), Some(w.into()), Some(h.into()), None));
        }
    }
    let mut decoder = reader.into_decoder()?;
    let (w, h) = decoder.dimensions();
    dimensions(w, h, config)?;
    let orientation = decoder.orientation()?;
    let mut image = image::DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
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
struct LimitedWriter<W> {
    inner: W,
    written: u64,
    limit: u64,
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.written.saturating_add(buffer.len() as u64) > self.limit {
            return Err(std::io::Error::other(animation_limit_message(self.limit)));
        }
        let count = self.inner.write(buffer)?;
        self.written += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn animation(
    frames: image::Frames<'_>,
    destination: &Path,
    config: &MediaConfig,
    started: Instant,
) -> anyhow::Result<()> {
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(LimitedWriter {
            inner: BufWriter::new(File::create(destination)?),
            written: 0,
            limit: config.file_max_bytes,
        });
        encoder.set_repeat(image::codecs::gif::Repeat::Infinite)?;
        let mut count = 0;
        let mut pixels = 0_u64;
        for frame in frames {
            deadline(started, config)?;
            let frame = frame?;
            count += 1;
            let (w, h) = frame.buffer().dimensions();
            dimensions(w, h, config)?;
            pixels = pixels.saturating_add(u64::from(w) * u64::from(h));
            anyhow::ensure!(
                count <= config.animation_max_frames && pixels <= config.animation_max_pixels,
                "the animation has too many frames or pixels"
            );
            encoder.encode_frame(frame)?;
        }
        anyhow::ensure!(count > 0, "the animation has no valid frames");
    }
    // The encoder writes the trailer when the block above ends; an over-limit error while writing the trailer is
    // discarded, so the size on disk is checked once more here.
    anyhow::ensure!(
        std::fs::metadata(destination)?.len() <= config.file_max_bytes,
        "{}",
        animation_limit_message(config.file_max_bytes)
    );
    Ok(())
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
