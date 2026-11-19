//! CSS combining core (`css_combine_filter.cc` pure parts).
//!
//! Stitches already-fetched stylesheets (BOM stripping, newline
//! separators, per-piece URL resolution) and gates combinations by
//! media, attributes, and size. Fetching and HTML flow live in later
//! steps.
#![forbid(unsafe_code)]

/// Strips a UTF-8 BOM (`StripUtf8Bom`).
#[must_use]
pub fn strip_utf8_bom(mut contents: &[u8]) -> (&[u8], bool) {
    if let Some(rest) = contents.strip_prefix(b"\xef\xbb\xbf") {
        contents = rest;
        return (contents, true);
    }
    (contents, false)
}

/// Per-piece URL resolution outcome (`ResolveCssUrls` result).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PieceResolution {
    /// No resolution needed; use the raw piece.
    NoResolutionNeeded,
    /// Resolution failed; the whole combination fails.
    WriteFailed,
    /// Resolved bytes to emit instead.
    Resolved(Vec<u8>),
}

/// Stitches pieces (`CssCombiner::WritePiece` loop): strips the BOM
/// off non-first pieces, resolves each piece's URLs, and separates
/// pieces lacking a trailing newline (except after the last).
/// Returns `None` when any piece fails resolution.
#[must_use]
pub fn stitch(
    pieces: &[&[u8]],
    resolve_piece: &mut dyn FnMut(usize, &[u8]) -> PieceResolution,
) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        let mut contents = *piece;
        if index != 0 {
            contents = strip_utf8_bom(contents).0;
        }
        match resolve_piece(index, contents) {
            PieceResolution::WriteFailed => return None,
            PieceResolution::Resolved(resolved) => {
                out.extend_from_slice(&resolved);
            }
            PieceResolution::NoResolutionNeeded => {
                out.extend_from_slice(contents);
                if index + 1 != pieces.len() && !contents.ends_with(b"\n") {
                    out.push(b'\n');
                }
            }
        }
    }
    Some(out)
}

/// Whether two media attributes combine (exact match, `media()`).
#[must_use]
pub fn media_match(first: &[u8], next: &[u8]) -> bool {
    first == next
}

/// Removes a permitted `id` from nonstandard attributes
/// (`PermitIdsForCssCombining`): the attribute NAME matches
/// case-insensitively and the VALUE must satisfy `permit`.
pub fn remove_permitted_id(
    attributes: &mut Vec<Vec<u8>>,
    id_value: Option<&[u8]>,
    permit: &dyn Fn(&[u8]) -> bool,
) {
    let Some(value) = id_value else {
        return;
    };
    if !permit(value) {
        return;
    }
    if let Some(index) = attributes
        .iter()
        .position(|name| name.eq_ignore_ascii_case(b"id"))
    {
        attributes.remove(index);
    }
}

/// Whether the accumulated size exceeds the cap
/// (`max_combined_css_bytes < 0` disables it).
#[must_use]
pub fn over_combined_cap(combined_bytes: i64, max_bytes: i64) -> bool {
    max_bytes >= 0 && max_bytes < combined_bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stitch_vectors() {
        // Plain join with newline separators.
        let out = stitch(&[b"a{color:red}", b".b{color:blue}"], &mut |_, _| {
            PieceResolution::NoResolutionNeeded
        })
        .expect("stitched");
        assert_eq!(out, b"a{color:red}\n.b{color:blue}");
        // Trailing newline suppresses the separator.
        let out = stitch(&[b"a{color:red}\n", b".b{color:blue}\n"], &mut |_, _| {
            PieceResolution::NoResolutionNeeded
        })
        .expect("stitched");
        assert_eq!(out, b"a{color:red}\n.b{color:blue}\n");
        // BOM stripped off non-first pieces only.
        let out = stitch(&[b"\xef\xbb\xbfa{}", b"\xef\xbb\xbf.b{}"], &mut |_, _| {
            PieceResolution::NoResolutionNeeded
        })
        .expect("stitched");
        assert_eq!(out, b"\xef\xbb\xbfa{}\n.b{}");
        // Resolved pieces emit as-is (no separator fixup).
        let out = stitch(&[b"a{}", b"b{}"], &mut |index, _contents| {
            if index == 0 {
                PieceResolution::Resolved(b"A{}".to_vec())
            } else {
                PieceResolution::NoResolutionNeeded
            }
        })
        .expect("stitched");
        assert_eq!(out, b"A{}b{}");
        // Resolution failure fails the combination.
        let out = stitch(&[b"a{}"], &mut |_, _| PieceResolution::WriteFailed);
        assert_eq!(out, None);
        // The resolver sees BOM-stripped contents for non-first pieces.
        let mut seen = Vec::new();
        let _ = stitch(&[b"a{}", b"\xef\xbb\xbf.b{}"], &mut |_, contents: &[u8]| {
            seen.push(contents.to_vec());
            PieceResolution::NoResolutionNeeded
        });
        assert_eq!(seen, vec![b"a{}".to_vec(), b".b{}".to_vec()]);
    }

    #[test]
    fn eligibility_vectors() {
        assert!(media_match(b"screen", b"screen"));
        assert!(!media_match(b"screen", b"print"));
        assert!(!media_match(b"", b"screen"));
        let mut attributes = vec![b"id".to_vec(), b"data-x".to_vec()];
        remove_permitted_id(&mut attributes, Some(b"theme"), &|value| value == b"theme");
        assert_eq!(attributes, vec![b"data-x".to_vec()]);
        let mut attributes = vec![b"ID".to_vec()];
        remove_permitted_id(&mut attributes, Some(b"other"), &|_| false);
        assert_eq!(attributes, vec![b"ID".to_vec()]);
        assert!(over_combined_cap(101, 100));
        assert!(!over_combined_cap(100, 100));
        assert!(!over_combined_cap(100000, -1));
    }
}
