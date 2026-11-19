//! Bot detection (`BotChecker`).
//!
//! Empty agents, `bot`/`crawl`/`spider` substrings (case-insensitive),
//! or exact bot-name tokens (case-sensitive).

/// Known bot names (`bot_checker.gperf`).
const BOT_NAMES: &[&str] = &[
    "about.ask.com",
    "A6-Indexer",
    "Apache-HttpClient",
    "Arachni",
    "AsyncHttpClient",
    "BackRub",
    "BingPreview",
    "expo9",
    "facebookexternalhit",
    "facebookplatform",
    "Feedfetcher-Google",
    "Genieo",
    "Google-StructuredDataTestingTool",
    "HttpClient",
    "Lycos",
    "Mediapartners-Google",
    "PHP",
    "proximic",
    "Python",
    "Python-urllib",
    "RPT-HTTPClient",
    "Ruby",
    "WordPress",
    "Yahoo!",
];

/// Whether the agent names a bot (`BotChecker::Lookup`).
#[must_use]
pub fn is_bot_user_agent(user_agent: &str) -> bool {
    // Check if the user agent is empty, or contains bot / crawl / spider.
    if user_agent.is_empty()
        || contains_ignore_case(user_agent, "bot")
        || contains_ignore_case(user_agent, "crawl")
        || contains_ignore_case(user_agent, "spider")
    {
        return true;
    }
    // Check whether the whole string is in database.
    if BOT_NAMES.contains(&user_agent) {
        return true;
    }
    // Get the application_name/domain_name/email.
    for token in user_agent.split([' ', '/', ',', ';', '+']) {
        if BOT_NAMES.contains(&token) {
            return true;
        }
    }
    false
}

/// Case-insensitive substring search (`FindIgnoreCase`).
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    haystack.len() >= needle.len()
        && haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}
