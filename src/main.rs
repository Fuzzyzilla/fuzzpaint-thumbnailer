//! Thumbnailer for `.fzp` files.
//!
//! Reads the input file path (arg1), searching the first and second blocks for the "thmb" type. It will *not* generate
//! thumbnails for files that do not have this field, as it is a high-overhead task to generate these images and this
//! thumbnailer is designed to be run dozens of times in a short timespan.
//!
//! Reads a desired size from arg2, fitting the read image into a square of that size. Filtering mode is undefined.
//!
//! Reads a file path from arg3, writing a PNG of the resized image to that location.
//!
//! Todo[XDG]: Accept file URI instead of path
//! Todo[XDG]: write failure logs to $XDG_CACHE_HOME/thumbnails/fail/fuzzpaint-thumbnailer
//!
//! Todo[WINDOWS]: implement IThumbnailProvider
//! Todo[WINDOWS]: allow RGB8 images
use az::{CheckedAs, SaturatingAs};
use std::borrow::Cow;
use std::io::{BufRead, BufReader, Error as IOError, Read, Result as IOResult, Seek};

/// Bail if the thumb image is larger than this.
const MAX_INPUT_IMAGE_DIMENSION: u32 = 1024;
const MIME_TYPE: &'static str = "application/x.fuzzpaint-doc";

/// std::io::Take, except it's Seek. Not sure why std's isn't D:
///
/// If the base reader is Seek, it shifts the basis of it
/// such that the position at the time of MyTake's construction is the start,
/// and that position + len is the end. Seeks past-the-end are clamped to the end.
struct MyTake<R> {
    reader: R,
    cursor: u64,
    len: u64,
}
impl<R> MyTake<R> {
    pub fn new(reader: R, len: u64) -> Self {
        Self {
            reader,
            len,
            cursor: 0,
        }
    }
    pub fn remaining(&self) -> u64 {
        self.len
            .checked_sub(self.cursor)
            .expect("cursor past the end")
    }
    pub fn into_inner(self) -> R {
        self.reader
    }
}
impl<R: Read> Read for MyTake<R> {
    fn read(&mut self, buf: &mut [u8]) -> IOResult<usize> {
        let trimmed_len: usize = (buf.len() as u64).min(self.remaining()).saturating_as();
        let buf = &mut buf[..trimmed_len];
        // Short circuit if we can't read any more data
        if buf.is_empty() {
            return Ok(0);
        }
        let num_read = self.reader.read(buf)?;
        // Defensive checks for bad inner reader impl
        // (or my own bugs :P)
        let new_cursor = self
            .cursor
            .checked_add(num_read as u64)
            .ok_or_else(|| IOError::other("inner reader overflowed MyTake cursor"))?;
        debug_assert!(new_cursor <= self.len);
        self.cursor = new_cursor;

        Ok(num_read)
    }
}
impl<R: BufRead> BufRead for MyTake<R> {
    fn consume(&mut self, amt: usize) {
        // Only allow consuming as much as we're allowed to view.
        let trimmed_amt = (amt as u64).min(self.remaining());
        self.cursor = self
            .cursor
            .checked_add(trimmed_amt)
            .expect("consume overflowed cursor");
        debug_assert!(self.cursor <= self.len);

        let trimmed_amt: usize = trimmed_amt.saturating_as();
        self.reader.consume(trimmed_amt)
    }
    fn fill_buf(&mut self) -> IOResult<&[u8]> {
        // Early call. Borrow weirdness.
        let remaining = self.remaining();

        let buf = self.reader.fill_buf()?;

        // Limit buffer's size, prevent user from seeing past-the-end
        let trimmed_len: usize = (buf.len() as u64).min(remaining).saturating_as();
        let buf = &buf[..trimmed_len];

        Ok(buf)
    }
}
impl<R: Seek> Seek for MyTake<R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> IOResult<u64> {
        use std::io::SeekFrom;
        let err_past_the_start = "seek offset past-the-start";
        let err_overflow_cursor = "seek offset overflows cursor";
        let new_cursor: u64 = match pos {
            SeekFrom::Current(delta) => {
                // Clamp upper bound to self length
                let delta = if delta > 0 {
                    // Saturate OK - we're taking the min with a i64 anyway
                    delta.min(self.remaining().saturating_as())
                } else {
                    delta
                };
                self.cursor
                    .checked_add_signed(delta)
                    // Also catches past-the-start
                    .ok_or_else(|| IOError::other(err_overflow_cursor))?
            }
            SeekFrom::Start(pos) => pos.min(self.remaining()),
            SeekFrom::End(pos) => {
                // Clamp upper bound, flip to positive for subtraction
                let pos = pos.max(0).unsigned_abs();
                self.len
                    .checked_sub(pos)
                    .ok_or_else(|| IOError::other(err_past_the_start))?
            }
        };

        // Each branch checks this individually. Still, make very sure.
        debug_assert!(new_cursor <= self.len);

        // We must seek the underlying reader with a Relative seek, as we
        // don't know what it's End and Start are relative to ours
        let delta: i64 = new_cursor
            .checked_as::<i64>()
            .zip(self.cursor.checked_as::<i64>())
            .and_then(|(new, old)| new.checked_sub(old))
            .ok_or_else(|| IOError::other("delta seek overflows"))?;

        self.reader.seek(SeekFrom::Current(delta))?;
        self.cursor = new_cursor;

        Ok(self.cursor)
    }
    fn stream_position(&mut self) -> IOResult<u64> {
        Ok(self.cursor)
    }
}

/// Given a reader of fzp data, create a reader of the thumbnail data.
/// Does not allocate except for errors.
// A lot of this logic can be recycled from fuzzpaint-vk, with a shared library crate.
fn read_fzp_thmb<R: Read + BufRead + Seek>(mut r: R) -> IOResult<MyTake<R>> {
    let mut fzp_header = [0; 12];
    r.read_exact(&mut fzp_header)?;
    if &fzp_header[0..4] != b"RIFF" || &fzp_header[8..12] != b"fzp " {
        return Err(IOError::other("unrecognized file type"));
    }
    let mut remaining_file_size = u32::from_le_bytes(fzp_header[4..8].try_into().unwrap());

    // Reads a header and size
    let read_block = |r: &mut R| -> IOResult<([u8; 4], u32)> {
        let mut block_header = [0; 8];
        r.read_exact(&mut block_header)?;

        let block_size = u32::from_le_bytes(block_header[4..8].try_into().unwrap());

        Ok((block_header[0..4].try_into().unwrap(), block_size))
    };

    // Read first block. If not `LIST INFO` chunk, thumb will be here.
    let (block_header, block_size) = read_block(&mut r)?;
    if block_header == *b"thmb" {
        // Found thmb! Take only the reported data length.
        return Ok(MyTake::new(r, block_size.min(remaining_file_size) as u64));
    }

    // Wasn't the first one. fastforward, check second one.
    r.seek(std::io::SeekFrom::Current(block_size as i64))?;
    // We read a header and many bytes, update remaining file size.
    remaining_file_size = remaining_file_size
        .saturating_sub(block_size)
        .saturating_sub(8);

    // Read second block. last chance, if not here then nowhere!
    let (block_header, block_size) = read_block(&mut r)?;
    if block_header == *b"thmb" {
        // Found thmb! Take only the reported data length.
        Ok(MyTake::new(r, block_size.min(remaining_file_size) as u64))
    } else {
        // So sad :(
        Err(IOError::other("document does not contain a thumbnail"))
    }
}
#[derive(bytemuck::Pod, bytemuck::Zeroable, Clone, Copy)]
#[repr(C, align(4))]
struct U8x4(pub [u8; 4]);
fn main() -> Result<(), Cow<'static, str>> {
    let args: Vec<_> = std::env::args().skip(1).take(4).collect();
    let Ok([in_path, size, out_path, in_uri]): Result<[String; 4], _> = args.try_into() else {
        return Err(
            "Usage: fuzzpaint-thumbnailer <in_path> <size in px> <out_path> <in_uri>".into(),
        );
    };

    let Ok(size): Result<u32, _> = size.parse() else {
        return Err("<size> parameter must be a non-negative integer".into());
    };
    if size == 0 {
        return Err("<size> parameter must not be zero".into());
    }
    // We only have so much input data to work with!
    // I don't believe any shell would request anything much larger than 512,
    // but just in case to avoid expensive calc and lots of mem for an accidental request.
    if size > 2048 {
        return Err("<size> parameter larger than reasonable".into());
    }

    // ========== Read FZP ============
    let (modified_unix_time, qoi_reader) = {
        // Open file and stat modification time (both required for thumbnailing according to XDG)
        let (file, mod_time) = std::fs::File::open(in_path)
            .and_then(|file| {
                file.metadata()
                    .and_then(|meta| meta.modified())
                    .map(|time| (file, time))
            })
            .map_err(|io| Cow::Owned(format!("failed to access in_path: {io}")))?;

        let unix_time = mod_time
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            // Unlikely err lol
            .map_err(|e| Cow::Owned(e.to_string()))?;

        // Fetch a reader of the raw image data.
        let qoi_reader = read_fzp_thmb(BufReader::new(file))
            .map_err(|io| Cow::Owned(format!("failed to parse input file: {io}").into()))?;

        (unix_time, qoi_reader)
    };
    // ========== Read QOI ============
    let (width, height, colorspace, rgba, rgba_len_bytes) = {
        let mut image_decoder = qoi::Decoder::from_stream(qoi_reader)
            .map_err(|img| Cow::Owned(format!("failed to parse thumbnail header: {img}")))?
            // XDG thumbnailer requires RGBA8
            .with_channels(qoi::Channels::Rgba);

        let qoi::Header {
            width,
            height,
            colorspace,
            ..
        } = *image_decoder.header();
        if width > MAX_INPUT_IMAGE_DIMENSION || height > MAX_INPUT_IMAGE_DIMENSION {
            return Err("thumbnail size exceeds limit".into());
        }
        let (width, height) = std::num::NonZeroU32::new(width)
            .zip(std::num::NonZeroU32::new(height))
            .ok_or_else(|| Cow::Borrowed("thumbnail has zero size"))?;

        // Force align of buffer to 4, for SIMD resize later
        let len_bytes = image_decoder.required_buf_len();
        // Round up length
        let mut data = vec![U8x4([0u8; 4]); len_bytes.div_ceil(4)];
        // take exact number of bytes requested (decode fails otherwise)
        // OK - we're casing to bytes, no align requirement
        let data_slice = &mut bytemuck::cast_slice_mut(&mut data)[..len_bytes];
        image_decoder
            .decode_to_buf(data_slice)
            .map_err(|img| Cow::Owned(format!("failed to parse thumbnail data: {img}")))?;

        (width, height, colorspace, data, len_bytes)
    };

    // ============= Scale ===============
    let (scaled_width, scaled_height) = {
        let max_dim = width.max(height);
        let scale_factor = size as f32 / max_dim.get() as f32;
        let scaled_width = (width.get() as f32 * scale_factor).ceil() as u32;
        let scaled_height = (height.get() as f32 * scale_factor).ceil() as u32;

        std::num::NonZeroU32::new(scaled_width)
            .zip(std::num::NonZeroU32::new(scaled_height))
            .ok_or_else(|| Cow::Borrowed("scaled thumbnail has zero size"))?
    };

    let scaled_rgba = {
        use fast_image_resize as fr;
        let mut resizer = fr::Resizer::new(fr::ResizeAlg::Convolution(fr::FilterType::Bilinear));

        let source_view = fr::ImageView::<'_, fr::pixels::U8x4>::from_buffer(
            width,
            height,
            // OK - we're casing to bytes, no align requirement
            &bytemuck::cast_slice(&rgba)[..rgba_len_bytes],
        )
        // OK - we manually aligned rgba to 4.
        .unwrap();
        // Alloc destination buffer
        let mut destination = fr::Image::new(scaled_width, scaled_height, fr::PixelType::U8x4);

        // TODO: Wrong interp for sRGB
        resizer
            .resize(
                &fr::DynamicImageView::U8x4(source_view),
                &mut destination.view_mut(),
            )
            // Unwrap ok - we unconditionally use the same pixel type constant for both.
            .unwrap();

        destination.into_vec()
    };
    // Dealloc unscaled image asap
    drop(rgba);

    // ============= Write PNG ===============
    let file = std::fs::File::create(out_path)
        .map_err(|io| Cow::Owned(format!("failed to open out_path for writing: {io}")))?;
    let mut png = png::Encoder::new(file, scaled_width.get(), scaled_height.get());
    png.set_color(png::ColorType::Rgba);
    png.set_depth(png::BitDepth::Eight);
    if colorspace == qoi::ColorSpace::Srgb {
        png.set_srgb(png::SrgbRenderingIntent::Perceptual);
    }
    // Write XDG Metas (https://specifications.freedesktop.org/thumbnail-spec/thumbnail-spec-latest.html#CREATION)
    let try_metas = || -> Result<(), png::EncodingError> {
        // PNG
        png.add_text_chunk("Software".into(), "Fuzzpaint".into())?;
        // XDG required
        png.add_text_chunk("Thumb::URI".into(), in_uri)?;
        png.add_text_chunk(
            "Thumb::MTime".into(),
            modified_unix_time.as_secs().to_string(),
        )?;
        // XDG Additional
        png.add_text_chunk("Thumb::Mimetype".into(), MIME_TYPE.into())?;
        // XDG Filetype specific
        png.add_text_chunk("Thumb::Image::Width".into(), "1080".into())?;
        png.add_text_chunk("Thumb::Image::Height".into(), "1080".into())?;
        // XDG Fuzzpaint ext
        png.add_text_chunk("X-Fuzzpaint::Soup".into(), "very good".into())?;

        Ok(())
    };
    try_metas().map_err(|enc| Cow::Owned(format!("failed to write metadata: {enc}")))?;
    // compression. We're using fairly small images, so it shouldn't take long.
    // but at the same time, we're using fairly small images, so compression won't be super important.
    // Agh!
    png.set_compression(png::Compression::Best);
    png.write_header()
        .and_then(|mut png| png.write_image_data(&scaled_rgba))
        .map_err(|enc| Cow::Owned(format!("failed to write png: {enc}")))
}
