//! Rewrite metadata (`CachedResult`/`OutputPartitions`, no protos).
//!
//! The metadata cache maps partition keys to rewrite outcomes. The
//! encoding below is implementation-local (length-prefixed fields
//! with a version byte); entries never leave the local cache, so
//! byte parity with the C++ protobuf form is not required — only
//! round-trip fidelity, corruption rejection, and the validation
//! rules, which mirror C++ exactly.
//!
//! Only the context-machinery fields are modeled; filter-specific
//! payloads (sprites, inlined images, dependencies) land with their
//! filters.

use super::cacheable_resource::{InputInfo, InputInfoType};

/// Encoding version tag.
const ENCODING_VERSION: u8 = 6;

/// One cached rewrite result (`CachedResult`, core fields).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CachedResult {
    /// Whether the inputs rewrite at all.
    pub optimizable: bool,
    /// Rewritten URL (valid iff optimizable and fetched).
    pub url: String,
    /// Hash of the encoded output.
    pub hash: String,
    /// Output extension, sans dot.
    pub extension: String,
    /// Input validity records.
    pub input: Vec<InputInfo>,
    /// Whether later filters may relocate the URL.
    pub url_relocatable: bool,
    /// Debug messages for the HTML comments.
    pub debug_message: Vec<String>,
    /// Inlined bytes (inline rewrites only).
    pub inlined_data: Option<Vec<u8>>,
    /// The URL names a canonical library (render patches it in).
    pub canonicalize_url: bool,
    /// Post-optimization image dimensions (`image_file_dims`).
    pub image_file_dims: Option<(u32, u32)>,
    /// Inlined image MIME type (`inlined_image_type`, as MIME).
    pub inlined_image_mime: Option<String>,
    /// Low-resolution preview bytes
    /// (`low_resolution_inlined_data`).
    pub low_resolution_inlined_data: Option<Vec<u8>>,
    /// Low-resolution preview MIME type.
    pub low_resolution_inlined_mime: Option<String>,
    /// Optimized byte size (`size`).
    pub optimized_size: i64,
    /// Sprite clips by input URL (`spriter_result`).
    pub spriter_result: Vec<(String, SpriteRect)>,
    /// Post-optimization image format (`optimized_image_type`).
    pub optimized_image_type: Option<ps_image::format::ImageType>,
}

/// Sprite clip rectangle (`spriter::Rect`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpriteRect {
    /// Left edge in the canvas.
    pub x: i32,
    /// Top edge in the canvas.
    pub y: i32,
    /// Image width.
    pub width: i32,
    /// Image height.
    pub height: i32,
}

impl CachedResult {
    /// Builds a fresh unoptimized result (like `add_partition`).
    #[must_use]
    pub fn new() -> Self {
        CachedResult {
            optimizable: true,
            url_relocatable: true,
            ..CachedResult::default()
        }
    }
}

/// One partition table (`OutputPartitions`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputPartitions {
    /// Per-partition results.
    pub partition: Vec<CachedResult>,
    /// Non-input dependencies (forced rechecks).
    pub other_dependency: Vec<InputInfo>,
    /// Table-level debug messages.
    pub debug_message: Vec<String>,
}

impl OutputPartitions {
    /// Appends a fresh partition (`add_partition`).
    pub fn add_partition(&mut self) -> &mut CachedResult {
        self.partition.push(CachedResult::new());
        self.partition.last_mut().expect("just pushed")
    }

    /// Appends a forced-recheck dependency (`add_other_dependency`).
    pub fn add_other_dependency(&mut self) -> &mut InputInfo {
        self.other_dependency.push(InputInfo::default());
        self.other_dependency.last_mut().expect("just pushed")
    }

    /// Clears partitions (kept dependencies/messages, like `Clear`... no:
    /// `clear_partition` drops partitions only).
    pub fn clear_partition(&mut self) {
        self.partition.clear();
    }

    /// Clears everything (`Clear`).
    pub fn clear(&mut self) {
        self.partition.clear();
        self.other_dependency.clear();
        self.debug_message.clear();
    }
}

// -- Encoding -----------------------------------------------------------

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn put_string(out: &mut Vec<u8>, text: &str) {
    put_bytes(out, text.as_bytes());
}

fn put_opt_string(out: &mut Vec<u8>, value: &Option<String>) {
    match value {
        Some(text) => {
            out.push(1);
            put_string(out, text);
        }
        None => out.push(0),
    }
}

fn put_opt_i64(out: &mut Vec<u8>, value: &Option<i64>) {
    match value {
        Some(number) => {
            out.push(1);
            out.extend_from_slice(&number.to_le_bytes());
        }
        None => out.push(0),
    }
}

fn put_opt_i32(out: &mut Vec<u8>, value: &Option<i32>) {
    match value {
        Some(number) => {
            out.push(1);
            out.extend_from_slice(&number.to_le_bytes());
        }
        None => out.push(0),
    }
}

fn put_opt_bool(out: &mut Vec<u8>, value: &Option<bool>) {
    match value {
        Some(flag) => {
            out.push(1);
            out.push(u8::from(*flag));
        }
        None => out.push(0),
    }
}

fn put_input(out: &mut Vec<u8>, input: &InputInfo) {
    put_opt_i32(out, &input.index);
    out.push(match input.info_type {
        InputInfoType::Cached => 0,
        InputInfoType::FileBased => 1,
        InputInfoType::AlwaysValid => 2,
    });
    put_opt_i64(out, &input.last_modified_time_ms);
    put_opt_i64(out, &input.expiration_time_ms);
    put_opt_string(out, &input.filename);
    put_opt_i64(out, &input.date_ms);
    put_opt_string(out, &input.input_content_hash);
    put_opt_bool(out, &input.disable_further_processing);
    put_opt_string(out, &input.url);
}

fn put_cached(out: &mut Vec<u8>, partition: &CachedResult) {
    out.push(u8::from(partition.optimizable));
    put_string(out, &partition.url);
    put_string(out, &partition.hash);
    put_string(out, &partition.extension);
    out.extend_from_slice(&(partition.input.len() as u64).to_le_bytes());
    for input in &partition.input {
        put_input(out, input);
    }
    out.push(u8::from(partition.url_relocatable));
    out.extend_from_slice(&(partition.debug_message.len() as u64).to_le_bytes());
    for message in &partition.debug_message {
        put_string(out, message);
    }
    match &partition.inlined_data {
        None => out.push(0),
        Some(data) => {
            out.push(1);
            put_bytes(out, data);
        }
    }
    out.push(u8::from(partition.canonicalize_url));
    match &partition.image_file_dims {
        None => out.push(0),
        Some((width, height)) => {
            out.push(1);
            out.extend_from_slice(&width.to_le_bytes());
            out.extend_from_slice(&height.to_le_bytes());
        }
    }
    put_opt_string(out, &partition.inlined_image_mime);
    match &partition.low_resolution_inlined_data {
        None => out.push(0),
        Some(data) => {
            out.push(1);
            put_bytes(out, data);
        }
    }
    put_opt_string(out, &partition.low_resolution_inlined_mime);
    out.extend_from_slice(&partition.optimized_size.to_le_bytes());
    out.extend_from_slice(&(partition.spriter_result.len() as u64).to_le_bytes());
    for (path, rect) in &partition.spriter_result {
        put_string(out, path);
        out.extend_from_slice(&rect.x.to_le_bytes());
        out.extend_from_slice(&rect.y.to_le_bytes());
        out.extend_from_slice(&rect.width.to_le_bytes());
        out.extend_from_slice(&rect.height.to_le_bytes());
    }
    match &partition.optimized_image_type {
        None => out.push(0),
        Some(image_type) => {
            out.push(1);
            out.extend_from_slice(&(*image_type as i32).to_le_bytes());
        }
    }
}

/// Serializes a table (`SerializeToString`).
#[must_use]
pub fn encode_partitions(partitions: &OutputPartitions) -> Vec<u8> {
    let mut out = vec![ENCODING_VERSION];
    out.extend_from_slice(&(partitions.partition.len() as u64).to_le_bytes());
    for partition in &partitions.partition {
        put_cached(&mut out, partition);
    }
    out.extend_from_slice(&(partitions.other_dependency.len() as u64).to_le_bytes());
    for input in &partitions.other_dependency {
        put_input(&mut out, input);
    }
    out.extend_from_slice(&(partitions.debug_message.len() as u64).to_le_bytes());
    for message in &partitions.debug_message {
        put_string(&mut out, message);
    }
    out
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        if end > self.bytes.len() {
            return None;
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Some(slice)
    }

    fn byte(&mut self) -> Option<u8> {
        self.take(1).map(|slice| slice[0])
    }

    fn u64(&mut self) -> Option<u64> {
        let slice = self.take(8)?;
        let bytes: [u8; 8] = slice.try_into().ok()?;
        Some(u64::from_le_bytes(bytes))
    }

    fn u32_field(&mut self) -> Option<u32> {
        let slice = self.take(4)?;
        let bytes: [u8; 4] = slice.try_into().ok()?;
        Some(u32::from_le_bytes(bytes))
    }

    fn i32_field(&mut self) -> Option<i32> {
        let slice = self.take(4)?;
        let bytes: [u8; 4] = slice.try_into().ok()?;
        Some(i32::from_le_bytes(bytes))
    }

    fn i64_field(&mut self) -> Option<i64> {
        let slice = self.take(8)?;
        let bytes: [u8; 8] = slice.try_into().ok()?;
        Some(i64::from_le_bytes(bytes))
    }

    fn bytes_field(&mut self) -> Option<&'a [u8]> {
        let len = self.u64()? as usize;
        self.take(len)
    }

    fn string_field(&mut self) -> Option<String> {
        String::from_utf8(self.bytes_field()?.to_vec()).ok()
    }

    fn opt_string(&mut self) -> Option<Option<String>> {
        match self.byte()? {
            0 => Some(None),
            1 => Some(Some(self.string_field()?)),
            _ => None,
        }
    }

    fn opt_i64(&mut self) -> Option<Option<i64>> {
        match self.byte()? {
            0 => Some(None),
            1 => Some(Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))),
            _ => None,
        }
    }

    fn opt_i32(&mut self) -> Option<Option<i32>> {
        match self.byte()? {
            0 => Some(None),
            1 => Some(Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))),
            _ => None,
        }
    }

    fn opt_bool(&mut self) -> Option<Option<bool>> {
        match self.byte()? {
            0 => Some(None),
            1 => Some(Some(self.byte()? != 0)),
            _ => None,
        }
    }

    fn input(&mut self) -> Option<InputInfo> {
        Some(InputInfo {
            index: self.opt_i32()?,
            info_type: match self.byte()? {
                0 => InputInfoType::Cached,
                1 => InputInfoType::FileBased,
                2 => InputInfoType::AlwaysValid,
                _ => return None,
            },
            last_modified_time_ms: self.opt_i64()?,
            expiration_time_ms: self.opt_i64()?,
            filename: self.opt_string()?,
            date_ms: self.opt_i64()?,
            input_content_hash: self.opt_string()?,
            disable_further_processing: self.opt_bool()?,
            url: self.opt_string()?,
        })
    }

    fn cached(&mut self) -> Option<CachedResult> {
        let optimizable = self.byte()? != 0;
        let url = self.string_field()?;
        let hash = self.string_field()?;
        let extension = self.string_field()?;
        let inputs = self.u64()? as usize;
        let mut input = Vec::with_capacity(inputs.min(1024));
        for _ in 0..inputs {
            input.push(self.input()?);
        }
        let url_relocatable = self.byte()? != 0;
        let messages = self.u64()? as usize;
        let mut debug_message = Vec::with_capacity(messages.min(1024));
        for _ in 0..messages {
            debug_message.push(self.string_field()?);
        }
        let inlined_data = match self.byte()? {
            0 => None,
            1 => Some(self.bytes_field()?.to_vec()),
            _ => return None,
        };
        let canonicalize_url = self.byte()? != 0;
        let image_file_dims = match self.byte()? {
            0 => None,
            1 => {
                let width = self.u32_field()?;
                let height = self.u32_field()?;
                Some((width, height))
            }
            _ => return None,
        };
        let inlined_image_mime = self.opt_string()?;
        let low_resolution_inlined_data = match self.byte()? {
            0 => None,
            1 => Some(self.bytes_field()?.to_vec()),
            _ => return None,
        };
        let low_resolution_inlined_mime = self.opt_string()?;
        let optimized_size = self.i64_field()?;
        let sprites = self.u64()? as usize;
        let mut spriter_result = Vec::with_capacity(sprites.min(1024));
        for _ in 0..sprites {
            let path = self.string_field()?;
            let x = self.i32_field()?;
            let y = self.i32_field()?;
            let width = self.i32_field()?;
            let height = self.i32_field()?;
            spriter_result.push((
                path,
                SpriteRect {
                    x,
                    y,
                    width,
                    height,
                },
            ));
        }
        let optimized_image_type = match self.byte()? {
            0 => None,
            1 => Some(match self.i32_field()? {
                1 => ps_image::format::ImageType::Jpeg,
                2 => ps_image::format::ImageType::Png,
                3 => ps_image::format::ImageType::Gif,
                4 => ps_image::format::ImageType::Webp,
                5 => ps_image::format::ImageType::WebpLosslessOrAlpha,
                6 => ps_image::format::ImageType::WebpAnimated,
                7 => ps_image::format::ImageType::Avif,
                _ => ps_image::format::ImageType::Unknown,
            }),
            _ => return None,
        };
        Some(CachedResult {
            optimizable,
            url,
            hash,
            extension,
            input,
            url_relocatable,
            debug_message,
            inlined_data,
            canonicalize_url,
            image_file_dims,
            inlined_image_mime,
            low_resolution_inlined_data,
            low_resolution_inlined_mime,
            optimized_size,
            spriter_result,
            optimized_image_type,
        })
    }
}

/// Parses a table (`ParseFromString`); corruption fails closed.
#[must_use]
pub fn decode_partitions(bytes: &[u8]) -> Option<OutputPartitions> {
    let mut cursor = Cursor::new(bytes);
    if cursor.byte()? != ENCODING_VERSION {
        return None;
    }
    let partitions = cursor.u64()? as usize;
    let mut partition = Vec::with_capacity(partitions.min(1024));
    for _ in 0..partitions {
        partition.push(cursor.cached()?);
    }
    let deps = cursor.u64()? as usize;
    let mut other_dependency = Vec::with_capacity(deps.min(1024));
    for _ in 0..deps {
        other_dependency.push(cursor.input()?);
    }
    let messages = cursor.u64()? as usize;
    let mut debug_message = Vec::with_capacity(messages.min(1024));
    for _ in 0..messages {
        debug_message.push(cursor.string_field()?);
    }
    if cursor.pos != bytes.len() {
        return None;
    }
    Some(OutputPartitions {
        partition,
        other_dependency,
        debug_message,
    })
}

// -- Validation ----------------------------------------------------------

/// Input validity outcome (`IsInputValid`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputValidity {
    /// Fresh entry.
    Valid,
    /// Stale but within the staleness window (serves stale).
    Stale,
    /// Unusable (and whether a purge caused it).
    Invalid {
        /// Purged entries never revalidate.
        purged: bool,
    },
}

/// Checks one input record (`input_info_utils::IsInputValid`, minus
/// file mtimes, which land with the file inputs).
#[must_use]
pub fn is_input_valid(
    input: &InputInfo,
    now_ms: i64,
    nested_rewrite: bool,
    staleness_threshold_ms: i64,
    url_cache_valid: bool,
) -> InputValidity {
    match input.info_type {
        InputInfoType::Cached => {
            if input.url.is_some() && !url_cache_valid {
                return InputValidity::Invalid { purged: true };
            }
            let Some(expiry) = input.expiration_time_ms else {
                return InputValidity::Invalid { purged: false };
            };
            let ttl_ms = expiry - now_ms;
            if ttl_ms > 0 {
                InputValidity::Valid
            } else if !nested_rewrite && ttl_ms + staleness_threshold_ms > 0 {
                InputValidity::Stale
            } else {
                InputValidity::Invalid { purged: false }
            }
        }
        InputInfoType::AlwaysValid => InputValidity::Valid,
        // File mtimes need the file system; conservatively invalid
        // (and never revalidatable, like C++).
        InputInfoType::FileBased => InputValidity::Invalid { purged: false },
    }
}
