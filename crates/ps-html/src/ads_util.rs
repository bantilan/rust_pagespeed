//! Ad snippet helpers (`ads_util`).
//!
//! Publisher-id extraction, adsbygoogle element validation, and script
//! source classification. The TODO in the C++ source notes the missing
//! upstream unit tests; the ported cases below are the whole upstream
//! suite plus hand tests for the untested helpers.

use crate::ads_attribute;
use crate::element::NodeId;
use crate::name::Keyword;
use crate::parser::Dom;

/// AdsByGoogle library source.
pub const ADS_BY_GOOGLE_JAVASCRIPT_SRC: &[u8] =
    b"//pagead2.googlesyndication.com/pagead/js/adsbygoogle.js";
/// AdsByGoogle API call snippet.
pub const ADS_BY_GOOGLE_API_CALL_JAVASCRIPT: &[u8] =
    b"(adsbygoogle = window.adsbygoogle || []).push({})";
/// AdsByGoogle element class.
pub const ADS_BY_GOOGLE_CLASS: &[u8] = b"adsbygoogle";

/// ShowAds API call library source.
pub const SHOW_ADS_API_CALL_JS_SRC: &[u8] = b"//pagead2.googlesyndication.com/pagead/show_ads.js";

/// Strips the product prefix from a publisher code
/// (`GetPublisherIdWithoutProductPrefix`): everything through the last
/// `-`; codes without a separator pass through unchanged.
#[must_use]
pub fn publisher_id_without_product_prefix(publisher_code: &[u8]) -> &[u8] {
    match publisher_code.iter().rposition(|byte| *byte == b'-') {
        Some(pos) => &publisher_code[pos + 1..],
        None => publisher_code,
    }
}

/// Whether the element is a valid adsbygoogle snippet for a publisher
/// (`IsValidAdsByGoogle`): an `<ins>` with exactly the adsbygoogle class,
/// a decodable `data-ad-slot`, and a `data-ad-client` containing the
/// publisher id (case-insensitive).
#[must_use]
pub fn is_valid_ads_by_google(dom: &mut Dom, id: NodeId, publisher_id: &[u8]) -> bool {
    if dom.element_keyword(id) != Keyword::Ins {
        return false;
    }
    let Some(class) = dom.find_attribute(id, Keyword::Class) else {
        return false;
    };
    let (decoded, _error) = dom.decoded_attribute(id, class);
    if decoded.as_deref() != Some(ADS_BY_GOOGLE_CLASS) {
        return false;
    }
    let Some(client) = dom.find_attribute_by_name(id, ads_attribute::DATA_AD_CLIENT) else {
        return false;
    };
    let (decoded, _error) = dom.decoded_attribute(id, client);
    let Some(decoded) = decoded else {
        return false;
    };
    if !contains_ignore_case(&decoded, publisher_id) {
        return false;
    }
    let Some(slot) = dom.find_attribute_by_name(id, ads_attribute::DATA_AD_SLOT) else {
        return false;
    };
    let (decoded, _error) = dom.decoded_attribute(id, slot);
    decoded.is_some()
}

/// Whether a script source loads the ShowAds API (`IsShowAdsApiCallJsSrc`).
#[must_use]
pub fn is_show_ads_api_call_js_src(src: &[u8]) -> bool {
    src.windows(SHOW_ADS_API_CALL_JS_SRC.len())
        .any(|window| window == SHOW_ADS_API_CALL_JS_SRC)
}

/// Whether a script source loads the AdsByGoogle library
/// (`IsAdsByGoogleJsSrc`).
#[must_use]
pub fn is_ads_by_google_js_src(src: &[u8]) -> bool {
    src.windows(ADS_BY_GOOGLE_JAVASCRIPT_SRC.len())
        .any(|window| window == ADS_BY_GOOGLE_JAVASCRIPT_SRC)
}

/// Case-insensitive substring search (`FindIgnoreCase`, ASCII).
fn contains_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle.iter())
            .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_id_match() {
        assert_eq!(publisher_id_without_product_prefix(b"ca-pub-1221"), b"1221");
    }

    #[test]
    fn publisher_id_no_match() {
        assert_eq!(
            publisher_id_without_product_prefix(b"capub1221"),
            b"capub1221"
        );
    }

    #[test]
    fn show_ads_src_match() {
        assert!(is_show_ads_api_call_js_src(
            b"//pagead2.googlesyndication.com/pagead/show_ads.js"
        ));
        assert!(is_show_ads_api_call_js_src(
            b"//pagead2.googlesyndication.com/pagead/show_ads.js?v=1"
        ));
        assert!(!is_show_ads_api_call_js_src(
            b"//pagead2.googlesyndication.com/pagead/js/adsbygoogle.js"
        ));
    }

    #[test]
    fn ads_by_google_src_match() {
        assert!(is_ads_by_google_js_src(
            b"//pagead2.googlesyndication.com/pagead/js/adsbygoogle.js"
        ));
        assert!(is_ads_by_google_js_src(
            b"//pagead2.googlesyndication.com/pagead/js/adsbygoogle.js?a=1"
        ));
        assert!(!is_ads_by_google_js_src(
            b"//pagead2.googlesyndication.com/pagead/showads.js"
        ));
    }

    #[test]
    fn valid_ads_by_google_matrix() {
        use crate::element::QuoteStyle;

        let mut dom = Dom::new();
        let ins = dom.new_element(None, b"ins");
        dom.add_escaped_attribute(
            ins,
            b"class",
            Some(b"adsbygoogle".as_slice()),
            QuoteStyle::DoubleQuote,
        );
        dom.add_escaped_attribute(
            ins,
            b"data-ad-client",
            Some(b"ca-pub-1221".as_slice()),
            QuoteStyle::DoubleQuote,
        );
        dom.add_escaped_attribute(
            ins,
            b"data-ad-slot",
            Some(b"7".as_slice()),
            QuoteStyle::DoubleQuote,
        );
        assert!(is_valid_ads_by_google(&mut dom, ins, b"1221"));
        // Publisher match is case-insensitive.
        assert!(is_valid_ads_by_google(&mut dom, ins, b"PUB-1221"));
        assert!(!is_valid_ads_by_google(&mut dom, ins, b"9999"));

        // Wrong tag, class, or missing slot fails.
        let div = dom.new_element(None, b"div");
        assert!(!is_valid_ads_by_google(&mut dom, div, b"1221"));
        let bare = dom.new_element(None, b"ins");
        assert!(!is_valid_ads_by_google(&mut dom, bare, b"1221"));
    }
}
