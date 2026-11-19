//! Image spriting (`ImageCombineFilter`, id `is`).
//!
//! Collects CSS background images with known div dimensions,
//! combines them into vertical-strip PNG sprites, and rewrites
//! `url()`s plus `background-position`s. Sprited slots suppress
//! the image/cache-extension nested renders that share them.
#![allow(dead_code)]

use std::rc::Rc;

use ps_css::ast::Declarations;
use ps_css::identifier::Ident;
use ps_css::property::Prop;
use ps_css::value::{Unit, Value, ValueType, Values};
use ps_http::google_url::{GoogleUrl, UrlRelativity};

use crate::resource::Resource;
use crate::server_context::ResourceDriver;

/// Filter id (`kImageCombineId`).
pub const FILTER_ID: &str = "is";
/// Combined-files-saved stat (`kImageFileCountReduction`).
pub const IMAGE_FILE_COUNT_REDUCTION: &str = "image_file_count_reduction";

/// Background properties whose URLs sprite (the image-property set
/// `CssImageRewriter::RewriteCss` walks).
fn is_background_property(prop: Prop) -> bool {
    ps_css::rewrite::is_image_property(prop)
}

/// Whether a value can start a background position (`IsPositionValue`).
fn is_position_value(value: &Value) -> bool {
    if value.lexical_type() == ValueType::Number {
        return true;
    }
    if value.lexical_type() == ValueType::Ident {
        return matches!(
            value.identifier().ident(),
            Ident::Left | Ident::Right | Ident::Top | Ident::Bottom | Ident::Center
        );
    }
    false
}

/// Whether a numeric value is a valid position length
/// (`IsValidNumberPosition`).
fn is_valid_number_position(value: &Value) -> bool {
    if value.lexical_type() != ValueType::Number {
        return false;
    }
    // If the alignment is specified in pixels, or is 0, we can just use it.
    value.dimension() == Unit::Px || value.integer_value() == 0
}

/// Reads alignment from one value, inserting a centered partner
/// (`ReadSingleValue`). Only identifiers sprite alone. Returns the
/// semantic (x, y) value indices after the insert.
fn read_single_value(values: &mut Values, offset: usize) -> Option<(usize, usize)> {
    use ps_css::identifier::Identifier;
    if values[offset].lexical_type() != ValueType::Ident {
        // TODO(morlovich): A single-value position is fine, it's
        // the same as if it were followed by center.
        return None;
    }
    let extra_index = offset + 1;
    let indices = match values[offset].identifier().ident() {
        Ident::Left | Ident::Right | Ident::Center => (offset, extra_index),
        Ident::Top | Ident::Bottom => (extra_index, offset),
        _ => return None,
    };
    // Insert a new value after offset. It's always in descending order.
    let center = Value::ident(Identifier::from_ident(Ident::Center));
    values.insert(extra_index, center);
    Some(indices)
}

/// Reads alignment from two values (`ReadTwoValues`): ident-first or
/// number-pair forms. Returns the semantic (x, y) value indices.
fn read_two_values(values: &Values, offset: usize) -> Option<(usize, usize)> {
    let value = &values[offset];
    if value.lexical_type() == ValueType::Ident {
        match value.identifier().ident() {
            Ident::Left | Ident::Right => Some((offset, offset + 1)),
            Ident::Top | Ident::Bottom => Some((offset + 1, offset)),
            Ident::Center => {
                let other = &values[offset + 1];
                if other.lexical_type() == ValueType::Ident {
                    match other.identifier().ident() {
                        Ident::Left | Ident::Right => Some((offset + 1, offset)),
                        Ident::Top | Ident::Bottom | Ident::Center => Some((offset, offset + 1)),
                        _ => None,
                    }
                } else {
                    // TODO(nforman): Allow for mixing of alignment types,
                    // i.e. left 2px.
                    None
                }
            }
            _ => None,
        }
    } else {
        // If there are two values and neither is an identifier, x comes
        // first: e.g. "5px 6px" means x=5, y=6.
        // TODO(nforman): support % values.
        for index in [offset, offset + 1] {
            let value = &values[index];
            if value.lexical_type() == ValueType::Number && is_valid_number_position(value) {
                continue;
            }
            return None;
        }
        Some((offset, offset + 1))
    }
}

/// Resolves both alignment values (`SetAlignmentValues`). Non-number
/// non-identifier values keep the zero default (unreachable in
/// practice: callers only pass position values).
fn alignment_values(
    x_value: &Value,
    y_value: &Value,
    div_width: i32,
    div_height: i32,
    image_width: i32,
    image_height: i32,
) -> Option<(i32, i32)> {
    let mut x = 0;
    if x_value.lexical_type() == ValueType::Number {
        if !is_valid_number_position(x_value) {
            return None;
        }
        x = x_value.integer_value();
    } else if x_value.lexical_type() == ValueType::Ident {
        match x_value.identifier().ident() {
            Ident::Left => x = 0,
            Ident::Right => x = div_width - image_width,
            Ident::Center => x = (div_width - image_width) / 2,
            _ => return None,
        }
    }
    let mut y = 0;
    if y_value.lexical_type() == ValueType::Number {
        if !is_valid_number_position(y_value) {
            return None;
        }
        y = y_value.integer_value();
    } else if y_value.lexical_type() == ValueType::Ident {
        match y_value.identifier().ident() {
            Ident::Top => y = 0,
            Ident::Bottom => y = div_height - image_height,
            Ident::Center => y = (div_height - image_height) / 2,
            _ => return None,
        }
    }
    Some((x, y))
}

/// Reads a background position at an offset (`ReadBackgroundPosition`).
/// A lone trailing position (or one followed by a non-position)
/// gains an inserted centered partner. Returns the (x, y) pixel
/// alignment plus the value indices to overwrite on realize (always
/// offset and offset + 1: when sprited both are replaced with
/// absolute pixel values, so they need x-first, y-second order).
fn read_background_position(
    values: &mut Values,
    offset: usize,
    image_width: i32,
    image_height: i32,
    div_width: i32,
    div_height: i32,
) -> Option<((usize, usize), (i32, i32))> {
    // Parsing these values is trickier than you might think.  If either
    // of the two values is a non-center identifier, it determines which
    // is x and which is y.  So for example, "5px left" means x=0, y=5 but
    // "5px top" means x=5, y=0.
    // See: http://www.w3.org/TR/CSS21/colors.html#propdef-background-position
    // TODO(abliss): move this to webutil/css?
    if values.len() <= offset {
        return None;
    }
    let indices = if values.len() - offset == 1 || !is_position_value(&values[offset + 1]) {
        read_single_value(values, offset)?
    } else {
        read_two_values(values, offset)?
    };
    // Now that we know which value is which dimension, we can extract the
    // values in px.
    let (x_index, y_index) = (offset, offset + 1);
    let (x, y) = alignment_values(
        &values[indices.0],
        &values[indices.1],
        div_width,
        div_height,
        image_width,
        image_height,
    )?;
    Some(((x_index, y_index), (x, y)))
}

/// Parsed background position for one sprite candidate.
#[derive(Debug, Clone, Copy)]
struct PositionFix {
    /// Declaration holding the position values.
    declaration: usize,
    /// Value indices overwritten with sprite pixels.
    x_index: usize,
    y_index: usize,
    /// Original alignment in pixels.
    x: i32,
    y: i32,
    /// Whether a position was present (else Realize creates one).
    has_position: bool,
}

/// Finds (or notes the absence of) a background position
/// (`FindBackgroundPositionValues`). Single-axis positions and
/// malformed positions abort spriting for the value.
fn find_background_position_values(
    declarations: &mut Declarations,
    image_width: i32,
    image_height: i32,
    div_width: i32,
    div_height: i32,
) -> Option<PositionFix> {
    let mut fix = PositionFix {
        declaration: 0,
        x_index: 0,
        y_index: 0,
        x: 0,
        y: 0,
        has_position: false,
    };
    for declaration in 0..declarations.len() {
        if fix.has_position {
            break;
        }
        match declarations[declaration].prop() {
            Prop::BackgroundPosition => {
                let values = declarations[declaration].values_mut();
                if values.len() > 2 || values.is_empty() {
                    return None;
                }
                let ((x_index, y_index), (x, y)) = read_background_position(
                    values,
                    0,
                    image_width,
                    image_height,
                    div_width,
                    div_height,
                )?;
                fix.declaration = declaration;
                fix.x_index = x_index;
                fix.y_index = y_index;
                fix.x = x;
                fix.y = y;
                fix.has_position = true;
            }
            Prop::BackgroundPositionX | Prop::BackgroundPositionY => {
                // These are non-standard, though supported in IE and Chrome.
                // TODO(abliss): handle these.
                return None;
            }
            Prop::Background => {
                // The background shorthand can include many values in any
                // order. We'll look for two consecutive position values.
                // (If only one position value is present, the other is
                // considered to be CENTER which we don't support.)
                let mut index = 0;
                while index < declarations[declaration].values().len() {
                    if is_position_value(&declarations[declaration].values()[index]) {
                        let values = declarations[declaration].values_mut();
                        let ((x_index, y_index), (x, y)) = read_background_position(
                            values,
                            index,
                            image_width,
                            image_height,
                            div_width,
                            div_height,
                        )?;
                        fix.declaration = declaration;
                        fix.x_index = x_index;
                        fix.y_index = y_index;
                        fix.x = x;
                        fix.y = y;
                        fix.has_position = true;
                        break;
                    }
                    index += 1;
                }
            }
            _ => {}
        }
    }
    Some(fix)
}

/// Div dimensions (`GetDeclarationDimensions`): both width and height
/// must parse.
fn declaration_dimensions(declarations: &Declarations) -> Option<(i32, i32)> {
    let (state, width, height) = ps_css::util::get_dimensions(declarations);
    if state == ps_css::util::DimensionState::HasBothDimensions {
        Some((width, height))
    } else {
        None
    }
}

/// URL slack for later filters (`ResourceCombiner::kUrlSlack`).
const URL_SLACK: i64 = 100;
/// `.pagespeed.` name overhead (`ResourceNamer::kOverhead`).
const NAMER_OVERHEAD: usize = 13;

/// Accumulated multipart leaf sizing (`ResourceCombiner` leaf math).
struct LeafSizer {
    accumulated: usize,
    url_overhead: usize,
    hash_chars: usize,
}

impl LeafSizer {
    fn new(
        driver: &std::rc::Rc<crate::server_context::ResourceDriver>,
        filter_id: &str,
        hash_chars: usize,
    ) -> Self {
        LeafSizer {
            accumulated: 0,
            url_overhead: filter_id.len() + NAMER_OVERHEAD + driver.signature_length(),
            hash_chars,
        }
    }

    /// Sizes the first entry (`ComputeLeafSize`).
    fn compute_first(&mut self, relative_path: &str) {
        let segment = multipart_encode(&[relative_path]);
        self.accumulated = segment.len() + self.url_overhead + self.hash_chars;
    }

    /// Sizes another entry (`AccumulateLeafSize`).
    fn accumulate(&mut self, relative_path: &str) {
        let segment = ps_cache::namer::encode_url_segment_bytes(relative_path.as_bytes());
        // for the '+'
        self.accumulated += segment.len() + 1;
    }

    /// Whether the combination grew too big (`UrlTooBig`).
    fn url_too_big(
        &self,
        options: &ps_config::options::RewriteOptions,
        resolved_base: &str,
    ) -> bool {
        let expanded = self.accumulated as i64 + URL_SLACK;
        if expanded > options.max_url_segment_size() {
            return true;
        }
        if expanded + resolved_base.len() as i64 > options.max_url_size() {
            return true;
        }
        false
    }
}

/// Multipart-encodes relative paths (`UrlMultipartEncoder::Encode`).
fn multipart_encode(urls: &[&str]) -> String {
    use crate::url_segment_encoder::{MultipartEncoder, UrlSegmentEncoder};
    let owned: Vec<Vec<u8>> = urls.iter().map(|url| url.as_bytes().to_vec()).collect();
    String::from_utf8_lossy(&MultipartEncoder.encode(&owned)).into_owned()
}

/// Decodes a multipart segment (`UrlMultipartEncoder::Decode`).
fn multipart_decode(segment: &[u8]) -> Option<Vec<String>> {
    use crate::url_segment_encoder::{MultipartEncoder, UrlSegmentEncoder};
    MultipartEncoder.decode(segment).map(|urls| {
        urls.iter()
            .map(|url| String::from_utf8_lossy(url).into_owned())
            .collect()
    })
}

/// One sprite candidate (`SpriteFuture` + slot).
struct SpriteCandidate {
    /// Absolute input URL.
    url: String,
    /// Loaded input.
    input: crate::resource::InputResource,
    /// Decoded RGBA pixels.
    frame: ps_image::frame::Frame,
    /// Ruleset holding the URL.
    ruleset: usize,
    /// Declaration holding the URL.
    url_declaration: usize,
    /// Ordinal of the URI among the declaration's URIs (relocated
    /// after position edits shift indices).
    uri_ordinal: usize,
    /// Parsed position (declaration may differ from the URL's).
    position: PositionFix,
}

/// Loads an input inline, returning its bytes on cacheable success.
fn load_input_bytes(
    driver: &Rc<ResourceDriver>,
    input: &mut crate::resource::InputResource,
) -> Option<Vec<u8>> {
    use crate::cacheable_resource::{MockLoadCallback, NotCacheablePolicy};
    let mut ctx = driver.request_context().borrow().clone();
    let mut callback = MockLoadCallback::new();
    input.load(
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
        &mut callback,
    );
    let (safe, _) = input.is_safe_to_rewrite(false);
    if !input.loaded() || !input.http_status_ok() || !safe {
        return None;
    }
    Some(input.contents())
}

/// Decodes a spriteable image (`Library::Register`): PNG or GIF by
/// sniffed content (never MIME), within the resolution budget.
/// Returns the RGBA frame plus natural dimensions.
fn register_sprite_image(
    driver: &Rc<ResourceDriver>,
    contents: &[u8],
) -> Option<(ps_image::frame::Frame, i32, i32)> {
    use ps_image::format::{sniff, ImageType};
    let frame = match sniff(contents) {
        ImageType::Png => ps_image::png::decode(contents).ok()?,
        ImageType::Gif => ps_image::gif::first_frame(contents).ok()?,
        _ => return None,
    };
    let (width, height) = (frame.width() as i64, frame.height() as i64);
    // Same budget as the image-rewrite path, overflow-safe.
    let max_pixels = driver.options().image_resolution_limit_bytes() / 4;
    if height <= 0 || width > max_pixels / height.max(1) {
        return None;
    }
    let rgba = frame.expand(ps_image::frame::PixelFormat::Rgba8888).ok()?;
    Some((rgba, width as i32, height as i32))
}

/// Collects sprite candidates for one ruleset (`AddCssBackgroundContext`
/// per URI + `FindUnspritable` validation). URLs failing validation
/// land in `no_sprite` (per-URL exclusion); lone background positions
/// clear `spriting_ok` for the sheet.
#[allow(clippy::too_many_arguments)]
fn collect_ruleset_candidates(
    driver: &Rc<ResourceDriver>,
    sheet: &mut ps_css::ast::Stylesheet,
    ruleset: usize,
    css_base: &GoogleUrl,
    no_sprite: &mut std::collections::HashSet<String>,
    spriting_ok: &mut bool,
    candidates: &mut Vec<SpriteCandidate>,
) {
    // Lone background positions veto sheet-wide spriting.
    let (mut position_found, mut image_found) = (false, false);
    {
        let Some(ruleset) = sheet.rulesets().get(ruleset) else {
            return;
        };
        if ruleset.kind() != ps_css::ast::RulesetType::Ruleset {
            return;
        }
        for declaration in ruleset.declarations() {
            match declaration.prop() {
                Prop::BackgroundPosition
                | Prop::BackgroundPositionX
                | Prop::BackgroundPositionY => position_found = true,
                _ if is_background_property(declaration.prop()) => {
                    if declaration
                        .values()
                        .iter()
                        .any(|value| value.lexical_type() == ValueType::Uri)
                    {
                        image_found = true;
                    }
                }
                _ => {}
            }
        }
    }
    if position_found && !image_found {
        *spriting_ok = false;
        return;
    }
    let Some((div_width, div_height)) =
        declaration_dimensions(sheet.rulesets()[ruleset].declarations())
    else {
        return;
    };
    // Gather (declaration, uri ordinal, raw URL) triples first; the
    // sheet mutates during validation.
    let mut urls = Vec::new();
    {
        let declarations = sheet.rulesets()[ruleset].declarations();
        for (declaration, decl) in declarations.iter().enumerate() {
            if !is_background_property(decl.prop()) {
                continue;
            }
            let mut ordinal = 0;
            for value in decl.values() {
                if value.lexical_type() != ValueType::Uri {
                    continue;
                }
                urls.push((
                    declaration,
                    ordinal,
                    value.string_value().as_bytes().to_vec(),
                ));
                ordinal += 1;
            }
        }
    }
    for (url_declaration, uri_ordinal, raw) in urls {
        let original = GoogleUrl::resolve(css_base, &String::from_utf8_lossy(&raw));
        if !original.is_web_valid() || !driver.options().is_allowed(original.spec()) {
            continue;
        }
        let url = original.spec().to_string();
        if no_sprite.contains(&url) {
            continue;
        }
        let (resource, authorized) = driver.create_input_resource(
            &original,
            crate::server_context::InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
            crate::server_context::IntendedFor::General,
            crate::server_context::InputRole::Img,
        );
        let Some(mut input) = resource else {
            if !authorized {
                // Unauthorized URLs veto nothing; they just never sprite.
                // (The debug lands with the image pass.)
            }
            continue;
        };
        let Some(contents) = load_input_bytes(driver, &mut input) else {
            // Load failures (404s etc.) exclude the URL without veto.
            no_sprite.insert(url);
            continue;
        };
        let Some((frame, image_width, image_height)) = register_sprite_image(driver, &contents)
        else {
            no_sprite.insert(url);
            continue;
        };
        if image_width < div_width || image_height < div_height {
            no_sprite.insert(url);
            continue;
        }
        // Position parsing may insert CENTER values (shifting indices,
        // hence ordinal relocation at realize time).
        let position = {
            let Some(ruleset) = sheet.rulesets_mut().get_mut(ruleset) else {
                continue;
            };
            match find_background_position_values(
                ruleset.declarations_mut(),
                image_width,
                image_height,
                div_width,
                div_height,
            ) {
                Some(fix) => fix,
                None => {
                    no_sprite.insert(url);
                    continue;
                }
            }
        };
        // Re-resolve the URL value index post-edit (dedupes share it).
        candidates.push(SpriteCandidate {
            url,
            input,
            frame,
            ruleset,
            url_declaration,
            uri_ordinal,
            position,
        });
    }
}

/// Groups candidates into combinations (`CollectSlots`): same URL
/// joins its existing combination; otherwise the first combination
/// with room (domain partnership + URL budget) takes it; else a new
/// combination starts. Singletons are dropped (`Combine` needs 2+).
fn group_combinations(
    candidates: &[SpriteCandidate],
    no_sprite: &mut std::collections::HashSet<String>,
    driver: &Rc<crate::server_context::ResourceDriver>,
    hash_chars: usize,
) -> Vec<FinishedCombination> {
    /// Working combination (partnership borrows the driver).
    struct Working<'a> {
        members: Vec<usize>,
        relative_paths: Vec<String>,
        sizer: LeafSizer,
        partnership: crate::url_partnership::UrlPartnership<'a>,
        refused_for_size: bool,
    }
    let mut combinations: Vec<Working<'_>> = Vec::new();
    let mut urls_to_combos: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if no_sprite.contains(&candidate.url) {
            continue;
        }
        // Don't add the same url to a combination twice.
        if let Some(&combo) = urls_to_combos.get(&candidate.url) {
            combinations[combo].members.push(index);
            continue;
        }
        let mut added = false;
        for (combo_index, combo) in combinations.iter_mut().enumerate() {
            // Skip a combination that has refused an image for size:
            // it is full, and every refused add costs work
            // proportional to its size.
            if combo.refused_for_size {
                continue;
            }
            if !combo.partnership.add_url(&candidate.url) {
                continue;
            }
            let relative = combo
                .partnership
                .relative_path(combo.partnership.num_urls() - 1);
            if combo.relative_paths.is_empty() {
                combo.sizer.compute_first(&relative);
            } else {
                combo.sizer.accumulate(&relative);
            }
            if combo
                .sizer
                .url_too_big(&driver.options(), &combo.partnership.resolved_base())
            {
                combo.partnership.remove_last();
                combo.refused_for_size = true;
                continue;
            }
            combo.relative_paths.push(relative);
            combo.members.push(index);
            urls_to_combos.insert(candidate.url.clone(), combo_index);
            added = true;
            break;
        }
        if !added {
            let mut partnership = crate::url_partnership::UrlPartnership::new(driver);
            partnership.reset(driver.base_url());
            if !partnership.add_url(&candidate.url) {
                no_sprite.insert(candidate.url.clone());
                continue;
            }
            let relative = partnership.relative_path(0);
            let mut sizer = LeafSizer::new(driver, FILTER_ID, hash_chars);
            sizer.compute_first(&relative);
            if sizer.url_too_big(&driver.options(), &partnership.resolved_base()) {
                no_sprite.insert(candidate.url.clone());
                continue;
            }
            let combo_index = combinations.len();
            urls_to_combos.insert(candidate.url.clone(), combo_index);
            combinations.push(Working {
                members: vec![index],
                relative_paths: vec![relative],
                sizer,
                partnership,
                refused_for_size: false,
            });
        }
    }
    // Drop singletons (`Combine`: no point in combining).
    combinations
        .into_iter()
        .filter(|combo| combo.relative_paths.len() > 1)
        .map(|combo| FinishedCombination {
            members: combo.members,
            relative_paths: combo.relative_paths,
            resolved_base: combo.partnership.resolved_base(),
        })
        .collect()
}

/// Owned combination, ready to lay out.
struct FinishedCombination {
    members: Vec<usize>,
    relative_paths: Vec<String>,
    resolved_base: String,
}

/// Realizes one candidate (`SpriteFuture::Realize`): substitutes the
/// sprite URL (relocated n-th URI) and overwrites the position with
/// (original - sprite) pixels, creating the declaration when absent.
fn realize_candidate(
    sheet: &mut ps_css::ast::Stylesheet,
    candidate: &SpriteCandidate,
    sprite_url: &str,
    rect: &ps_image::spriter::PlacedRect,
) {
    let Some(ruleset) = sheet.rulesets_mut().get_mut(candidate.ruleset) else {
        return;
    };
    let declarations = ruleset.declarations_mut();
    // Relocate the n-th URI of the URL declaration (position edits
    // may have shifted indices).
    let Some(url_decl) = declarations.get_mut(candidate.url_declaration) else {
        return;
    };
    let mut seen = 0;
    let mut url_index = None;
    for (index, value) in url_decl.values().iter().enumerate() {
        if value.lexical_type() == ValueType::Uri {
            if seen == candidate.uri_ordinal {
                url_index = Some(index);
                break;
            }
            seen += 1;
        }
    }
    let Some(url_index) = url_index else {
        return;
    };
    let replacement = ps_css::unicode_text::UnicodeText::from_utf8(sprite_url.as_bytes());
    if let Some(text) =
        declarations[candidate.url_declaration].values_mut()[url_index].string_value_mut()
    {
        *text = replacement;
    }
    let position = &candidate.position;
    let new_x = position.x - rect.x;
    let new_y = position.y - rect.y;
    if position.has_position {
        if let Some(decl) = declarations.get_mut(position.declaration) {
            let values = decl.values_mut();
            if position.x_index < values.len() && position.y_index < values.len() {
                values[position.x_index] = Value::number(f64::from(new_x), Unit::Px);
                values[position.y_index] = Value::number(f64::from(new_y), Unit::Px);
            }
        }
    } else {
        let mut values = ps_css::value::Values::new();
        values.push(Value::number(f64::from(new_x), Unit::Px));
        values.push(Value::number(f64::from(new_y), Unit::Px));
        declarations.push(ps_css::ast::Declaration::new(
            ps_css::property::Property::from_prop(Prop::BackgroundPosition),
            values,
            false,
        ));
    }
}

/// Lays out, composes, and writes one combination (`WriteCombination`).
/// Returns the sprite bytes on success.
fn write_combination(
    driver: &Rc<crate::server_context::ResourceDriver>,
    candidates: &[SpriteCandidate],
    combo: &FinishedCombination,
) -> Option<(Vec<u8>, Vec<ps_image::spriter::PlacedRect>)> {
    // Layout follows URL order (deduped); members realize from it.
    let mut seen = std::collections::HashSet::new();
    let mut frames = Vec::new();
    for &member in &combo.members {
        let url = &candidates[member].url;
        if seen.insert(url.clone()) {
            frames.push(candidates[member].frame.clone());
        }
    }
    let (canvas, rects) = ps_image::spriter::compose_vertical_strip(&frames).ok()?;
    // The canvas as a whole must stay within the same budget.
    let max_pixels = driver.options().image_resolution_limit_bytes() / 4;
    let (width, height) = (i64::from(canvas.width()), i64::from(canvas.height()));
    if height <= 0 || width > max_pixels / height.max(1) {
        return None;
    }
    let bytes = ps_image::optimize::optimize_png_frame(&canvas, None, false).ok()?;
    Some((bytes, rects))
}

/// Sprites one CSS level (`CssImageRewriter::RewriteCss` sprite half):
/// collects candidates per ruleset, groups, lays out, writes, and
/// realizes. Returns the sprited candidate indexes (which skip the
/// image/cache-extension nested renders sharing their values).
#[allow(clippy::too_many_arguments)]
pub fn sprite_stylesheet_level(
    driver: &Rc<crate::server_context::ResourceDriver>,
    sheet: &mut ps_css::ast::Stylesheet,
    css_base: &GoogleUrl,
    css_resolution_base: &GoogleUrl,
    css_text: &[u8],
    css_dir: &str,
) -> std::collections::HashSet<usize> {
    use crate::metadata::{encode_partitions, CachedResult};
    let mut sprited = std::collections::HashSet::new();
    let rulesets = sheet.rulesets().len();
    let mut no_sprite = std::collections::HashSet::new();
    let mut spriting_ok = true;
    let mut candidates = Vec::new();
    for ruleset in 0..rulesets {
        collect_ruleset_candidates(
            driver,
            sheet,
            ruleset,
            css_resolution_base,
            &mut no_sprite,
            &mut spriting_ok,
            &mut candidates,
        );
    }
    if !spriting_ok || candidates.is_empty() {
        return sprited;
    }
    // Metadata: one table per level, keyed by inputs + css key, so
    // repeats realize without recomposing (corrupt tables miss).
    let mut urls: Vec<String> = candidates.iter().map(|c| c.url.clone()).collect();
    urls.sort();
    let key = combine_partition_key(driver, &urls, css_text, css_dir);
    if let Some(table) = read_combine_metadata(driver, &key, &candidates) {
        for partition in table.partition.iter() {
            if !partition.optimizable {
                continue;
            }
            for input in &partition.input {
                let Some(index) = input.index else { continue };
                let Ok(index) = usize::try_from(index) else {
                    continue;
                };
                let Some(candidate) = candidates.get(index) else {
                    continue;
                };
                let Some((_, rect)) = partition
                    .spriter_result
                    .iter()
                    .find(|(path, _)| *path == candidate.url)
                else {
                    continue;
                };
                let new_url = crate::resource_slot::relativize_or_passthrough(
                    &driver.options(),
                    &partition.url,
                    sprite_relativity(sheet, candidate),
                    css_base,
                );
                realize_candidate(
                    sheet,
                    candidate,
                    &new_url,
                    &ps_image::spriter::PlacedRect {
                        x: rect.x,
                        y: rect.y,
                        width: rect.width,
                        height: rect.height,
                    },
                );
                sprited.insert(index);
            }
        }
        if !sprited.is_empty() {
            return sprited;
        }
    }
    let hash_chars = driver.server_context().hasher().hash_size_in_chars();
    let combos = group_combinations(&candidates, &mut no_sprite.clone(), driver, hash_chars);
    let mut table = crate::metadata::OutputPartitions::default();
    for combo in &combos {
        let Some((bytes, rects)) = write_combination(driver, &candidates, combo) else {
            continue;
        };
        // Name from the multipart relative paths; base from the partnership.
        let refs: Vec<&str> = combo.relative_paths.iter().map(String::as_str).collect();
        let name = multipart_encode(&refs);
        let mut output = match driver.create_output_resource_with_mapped_path(
            &combo.resolved_base,
            &combo.resolved_base,
            FILTER_ID,
            name.as_bytes(),
            crate::output_resource::OutputResourceKind::Rewritten,
        ) {
            Ok(output) => output,
            Err(_) => continue,
        };
        let mut partition = CachedResult::new();
        let inputs: Vec<crate::resource::InputResource> = combo
            .members
            .iter()
            .map(|&member| candidates[member].input.clone())
            .collect();
        if !driver.write_output(
            &inputs,
            &bytes,
            ps_http::content_type::mime_to_content_type("image/png"),
            "",
            &mut output,
            &mut partition,
        ) {
            continue;
        }
        // Record rects per URL for cached realizes.
        let mut seen_urls = std::collections::HashSet::new();
        let mut rect_index = 0;
        for &member in &combo.members {
            let url = &candidates[member].url;
            if !seen_urls.insert(url.clone()) {
                continue;
            }
            let Some(rect) = rects.get(rect_index) else {
                break;
            };
            rect_index += 1;
            partition.spriter_result.push((
                url.clone(),
                crate::metadata::SpriteRect {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                },
            ));
        }
        for &member in &combo.members {
            let candidate = &candidates[member];
            let new_url = crate::resource_slot::relativize_or_passthrough(
                &driver.options(),
                output.url(),
                sprite_relativity(sheet, candidate),
                css_base,
            );
            let Some((_, rect)) = partition
                .spriter_result
                .iter()
                .find(|(path, _)| *path == candidate.url)
            else {
                continue;
            };
            realize_candidate(
                sheet,
                candidate,
                &new_url,
                &ps_image::spriter::PlacedRect {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                },
            );
            sprited.insert(member);
            push_combine_input(&mut partition, member);
        }
        let unique_urls = partition.spriter_result.len();
        if unique_urls > 1 {
            driver
                .server_context()
                .variable(IMAGE_FILE_COUNT_REDUCTION)
                .add(unique_urls as i64 - 1);
        }
        table.partition.push(partition);
    }
    if !table.partition.is_empty() {
        driver
            .server_context()
            .metadata_cache()
            .put(&key, &encode_partitions(&table));
    }
    sprited
}

/// Original relativity of a candidate's URL value.
fn sprite_relativity(
    sheet: &ps_css::ast::Stylesheet,
    candidate: &SpriteCandidate,
) -> UrlRelativity {
    let Some(ruleset) = sheet.rulesets().get(candidate.ruleset) else {
        return UrlRelativity::AbsoluteUrl;
    };
    let Some(declaration) = ruleset.declarations().get(candidate.url_declaration) else {
        return UrlRelativity::AbsoluteUrl;
    };
    let mut seen = 0;
    for value in declaration.values() {
        if value.lexical_type() != ValueType::Uri {
            continue;
        }
        if seen == candidate.uri_ordinal {
            return GoogleUrl::find_relativity(&String::from_utf8_lossy(
                value.string_value().as_bytes(),
            ));
        }
        seen += 1;
    }
    UrlRelativity::AbsoluteUrl
}

/// Records a member input (`AddResourceToPartition` shape).
fn push_combine_input(partition: &mut crate::metadata::CachedResult, index: usize) {
    let mut info = crate::cacheable_resource::InputInfo::default();
    info.index = Some(index as i32);
    partition.input.push(info);
}

/// Builds the combine metadata key (inputs + css key).
fn combine_partition_key(
    driver: &Rc<crate::server_context::ResourceDriver>,
    urls: &[String],
    css_text: &[u8],
    css_dir: &str,
) -> String {
    let hasher = driver.server_context().lock_hasher();
    use ps_cache::hash::Hasher;
    let inputs = urls.join("\n");
    format!(
        "is-combine:{}:css-key={}_{}",
        hasher.hash(inputs.as_bytes()),
        hasher.hash(css_text),
        hasher.hash(css_dir.as_bytes()),
    )
}

/// Reads combine metadata, validating slot indexes (out-of-bounds
/// entries miss instead of panicking).
fn read_combine_metadata(
    driver: &Rc<crate::server_context::ResourceDriver>,
    key: &str,
    candidates: &[SpriteCandidate],
) -> Option<crate::metadata::OutputPartitions> {
    use crate::metadata::decode_partitions;
    use ps_cache::cache::KeyState;
    let lookup = driver.server_context().metadata_cache().get(key);
    if lookup.state != KeyState::Available {
        return None;
    }
    let bytes = lookup.value?;
    let table = decode_partitions(&bytes)?;
    for partition in &table.partition {
        for input in &partition.input {
            let Some(index) = input.index else {
                return None;
            };
            if index < 0 || index as usize >= candidates.len() {
                return None;
            }
        }
    }
    Some(table)
}

/// Serves a sprite by decoding its multipart name and recomposing
/// (`is` fetch; no combinability rules on fetch).
fn fetch_sprite(
    driver: &Rc<ResourceDriver>,
    output: crate::output_resource::OutputResource,
    fetch: &mut dyn ps_http::fetch::AsyncFetch,
) -> bool {
    use crate::cacheable_resource::{MockLoadCallback, NotCacheablePolicy};
    let fail = |fetch: &mut dyn ps_http::fetch::AsyncFetch| {
        fetch.response_headers_mut().set_status_and_reason(404);
        fetch.headers_complete();
        fetch.handle_done(false);
        false
    };
    let name = output.full_name().name().to_string();
    let Some(urls) = multipart_decode(name.as_bytes()) else {
        return fail(fetch);
    };
    if urls.is_empty() {
        return fail(fetch);
    }
    let base = GoogleUrl::new(&output.decoded_base());
    let mut frames = Vec::new();
    let mut inputs = Vec::new();
    for relative in &urls {
        let url = GoogleUrl::resolve(&base, relative);
        if !url.is_web_valid() {
            return fail(fetch);
        }
        let (resource, _) = driver.create_input_resource(
            &url,
            crate::server_context::InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
            crate::server_context::IntendedFor::General,
            crate::server_context::InputRole::Reconstruction,
        );
        let Some(mut input) = resource else {
            return fail(fetch);
        };
        let mut ctx = driver.request_context().borrow().clone();
        let mut callback = MockLoadCallback::new();
        // Uncacheable inputs still load: spriting requires
        // cacheable bytes, but the fallback serves them anyway.
        input.load(
            NotCacheablePolicy::LoadEvenIfNotCacheable,
            &mut ctx,
            &mut callback,
        );
        if !input.loaded() || !input.http_status_ok() {
            return fail(fetch);
        }
        let contents = input.contents();
        // Spriting needs safe-to-rewrite bytes; anything else falls
        // back (or 404s) per input.
        let (safe, _) = input.is_safe_to_rewrite(false);
        let frame = if safe {
            match ps_image::format::sniff(&contents) {
                ps_image::format::ImageType::Png => ps_image::png::decode(&contents).ok(),
                ps_image::format::ImageType::Gif => ps_image::gif::first_frame(&contents).ok(),
                _ => None,
            }
        } else {
            None
        };
        let Some(frame) = frame else {
            return fetch_sprite_fallback(driver, &input, fetch);
        };
        let Ok(rgba) = frame.expand(ps_image::frame::PixelFormat::Rgba8888) else {
            return fail(fetch);
        };
        frames.push(rgba);
        inputs.push(input);
    }
    if frames.len() < 2 {
        return fetch_sprite_fallback(driver, inputs.first().unwrap(), fetch);
    }
    let Ok((canvas, _)) = ps_image::spriter::compose_vertical_strip(&frames) else {
        return fail(fetch);
    };
    let Ok(bytes) = ps_image::optimize::optimize_png_frame(&canvas, None, false) else {
        return fail(fetch);
    };
    let mut output = output;
    let mut partition = crate::metadata::CachedResult::new();
    if !driver.write_output(
        &inputs,
        &bytes,
        ps_http::content_type::mime_to_content_type("image/png"),
        "",
        &mut output,
        &mut partition,
    ) {
        return fail(fetch);
    }
    let headers = output.response_headers().clone();
    let body = output.body().to_vec();
    fetch.response_headers_mut().copy_from(&headers);
    fetch.headers_complete();
    let ok = fetch.handle_write(&body);
    fetch.handle_done(ok);
    true
}

/// Serves an unspritable single input, sanitized, when its type
/// allows (`SendFallbackResponse` for `is`).
fn fetch_sprite_fallback(
    _driver: &Rc<ResourceDriver>,
    input: &crate::resource::InputResource,
    fetch: &mut dyn ps_http::fetch::AsyncFetch,
) -> bool {
    let fail = |fetch: &mut dyn ps_http::fetch::AsyncFetch| {
        fetch.response_headers_mut().set_status_and_reason(404);
        fetch.headers_complete();
        fetch.handle_done(false);
        false
    };
    let allowed = input
        .response_headers()
        .determine_content_type()
        .is_some_and(|content_type| {
            use ps_http::content_type::ContentTypeKind;
            content_type.is_js_like()
                || content_type.is_css()
                || content_type.is_image()
                || content_type.kind() == ContentTypeKind::Pdf
        });
    if !allowed {
        return fail(fetch);
    }
    let mut headers = input.response_headers().clone();
    if headers.sanitize() {
        headers.compute_caching();
    }
    headers
        .headers_mut()
        .replace("X-Content-Type-Options", "nosniff");
    fetch.response_headers_mut().copy_from(&headers);
    fetch.headers_complete();
    let ok = fetch.handle_write(&input.contents());
    fetch.handle_done(ok);
    true
}

/// Image spriting filter (`ImageCombineFilter`): serving only (CSS
/// integration runs through the stylesheet walker, never HTML).
pub struct ImageCombineFilter {
    driver: Rc<ResourceDriver>,
}

impl ImageCombineFilter {
    /// Builds the filter over a driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        ImageCombineFilter { driver }
    }
}

impl crate::rewrite_filter::RewriteFilter for ImageCombineFilter {
    fn id(&self) -> &'static str {
        FILTER_ID
    }

    fn encoder(&self) -> &'static dyn crate::url_segment_encoder::UrlSegmentEncoder {
        use crate::url_segment_encoder::MultipartEncoder;
        const ENCODER: MultipartEncoder = MultipartEncoder;
        &ENCODER
    }

    fn fetch(
        &self,
        driver: &Rc<ResourceDriver>,
        output: crate::output_resource::OutputResource,
        fetch: &mut dyn ps_http::fetch::AsyncFetch,
    ) -> bool {
        fetch_sprite(driver, output, fetch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_css::parser::Parser;

    fn parse_one(input: &str) -> Declarations {
        let mut parser = Parser::new(input.as_bytes());
        parser.set_preservation_mode(true);
        parser.set_quirks_mode(false);
        parser.parse_raw_declarations()
    }

    #[test]
    fn position_keywords() {
        // left top: x=0, y=0.
        let mut decls = parse_one("background-position:left top");
        let fix =
            find_background_position_values(&mut decls, 100, 100, 50, 50).expect("spriteable");
        assert!(fix.has_position);
        assert_eq!((fix.x, fix.y), (0, 0));
        // right bottom: x=div-image on both axes.
        let mut decls = parse_one("background-position:right bottom");
        let fix = find_background_position_values(&mut decls, 100, 60, 50, 30).expect("spriteable");
        assert_eq!((fix.x, fix.y), (-50, -30));
        // Lone center gains a partner: x=(50-100)/2, y=(50-100)/2.
        let mut decls = parse_one("background-position:center");
        let fix =
            find_background_position_values(&mut decls, 100, 100, 50, 50).expect("spriteable");
        assert_eq!((fix.x, fix.y), (-25, -25));
        assert_eq!(decls[0].values().len(), 2);
    }

    #[test]
    fn position_lengths() {
        let mut decls = parse_one("background-position:5px 10px");
        let fix =
            find_background_position_values(&mut decls, 100, 100, 50, 50).expect("spriteable");
        assert_eq!((fix.x, fix.y), (5, 10));
        // A lone length is not spriteable (upstream TODO).
        let mut decls = parse_one("background-position:5px");
        assert!(find_background_position_values(&mut decls, 100, 100, 50, 50).is_none());
    }

    #[test]
    fn position_rejections() {
        // Three values in background-position abort spriting.
        let mut decls = parse_one("background-position:1px 2px 3px");
        assert!(find_background_position_values(&mut decls, 100, 100, 50, 50).is_none());
        // Mixed ident+length: not spriteable.
        let mut decls = parse_one("background-position:5px left");
        assert!(find_background_position_values(&mut decls, 100, 100, 50, 50).is_none());
        // Mixed center+length: not spriteable.
        let mut decls = parse_one("background-position:center 5px");
        assert!(find_background_position_values(&mut decls, 100, 100, 50, 50).is_none());
        // Single-axis properties never sprite.
        let mut decls = parse_one("background-position-x:5px");
        assert!(find_background_position_values(&mut decls, 100, 100, 50, 50).is_none());
        // Shorthand without positions stays positionless.
        let mut decls = parse_one("background:url(a.png) no-repeat");
        let fix =
            find_background_position_values(&mut decls, 100, 100, 50, 50).expect("spriteable");
        assert!(!fix.has_position);
    }

    #[test]
    fn div_dimensions_need_both() {
        let decls = parse_one("width:10px;height:20px;background:url(a.png)");
        assert_eq!(declaration_dimensions(&decls), Some((10, 20)));
        let decls = parse_one("width:10px;background:url(a.png)");
        assert_eq!(declaration_dimensions(&decls), None);
        let decls = parse_one("background:url(a.png)");
        assert_eq!(declaration_dimensions(&decls), None);
    }
}
