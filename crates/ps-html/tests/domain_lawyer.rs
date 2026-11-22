//! DomainLawyer core tests ported from `domain_lawyer_test.cc`
//! (authorization, rewrite mappings, sharding, signatures; origin
//! mappings, proxy support, and merges port with their slices).
use ps_config::message_handler::MockMessageHandler;
use ps_html::domain_lawyer::DomainLawyer;
use ps_http::google_url::GoogleUrl;

const RESOURCE_URL: &str = "styles/style.css?appearance=reader";
const CDN_PREFIX: &str = "http://graphics8.nytimes.com/";
const REQUEST_DOMAIN: &str = "http://www.nytimes.com/";
const REQUEST_DOMAIN_PORT: &str = "http://www.nytimes.com:8080/";
const ORIG_REQUEST: &str = "http://www.nytimes.com/index.html";
const PORT_REQUEST: &str = "http://www.nytimes.com:8080/index.html";
const HTTPS_REQUEST: &str = "https://www.nytimes.com/index.html";

struct Fixture {
    lawyer: DomainLawyer,
    handler: MockMessageHandler,
}

impl Fixture {
    fn new() -> Self {
        Fixture {
            lawyer: DomainLawyer::new(),
            handler: MockMessageHandler::new(),
        }
    }

    fn add_domain(&mut self, name: &str) -> bool {
        self.lawyer.add_domain(name, Some(&mut self.handler))
    }

    fn add_rewrite_mapping(&mut self, to: &str, from: &str) -> bool {
        self.lawyer
            .add_rewrite_domain_mapping(to, from, Some(&mut self.handler))
    }

    fn add_shard(&mut self, domain: &str, shards: &str) -> bool {
        self.lawyer
            .add_shard(domain, shards, Some(&mut self.handler))
    }

    fn is_authorized(&self, context: &str, url: &str) -> bool {
        self.lawyer
            .is_domain_authorized(&GoogleUrl::new(context), &GoogleUrl::new(url))
    }

    fn map_request(&self, original: &str, resource: &str) -> Option<(String, GoogleUrl)> {
        self.lawyer
            .map_request_to_domain(&GoogleUrl::new(original), resource)
    }

    fn will_change(&self, url: &str) -> bool {
        let normalized = DomainLawyer::normalize_domain_name(url);
        self.lawyer.will_domain_change(&GoogleUrl::new(&normalized))
    }
}

#[test]
fn relative_domain() {
    let fixture = Fixture::new();
    let (mapped, _) = fixture.map_request(ORIG_REQUEST, RESOURCE_URL).unwrap();
    assert_eq!(mapped, REQUEST_DOMAIN);
    assert!(!fixture.lawyer.can_rewrite_domains());
}

#[test]
fn absolute_domain() {
    let fixture = Fixture::new();
    let (mapped, _) = fixture
        .map_request(ORIG_REQUEST, &format!("{REQUEST_DOMAIN}{RESOURCE_URL}"))
        .unwrap();
    assert_eq!(mapped, REQUEST_DOMAIN);
    assert!(!fixture.lawyer.can_rewrite_domains());
}

#[test]
fn external_domain_not_declared() {
    let fixture = Fixture::new();
    assert!(fixture
        .map_request(ORIG_REQUEST, &format!("{CDN_PREFIX}{RESOURCE_URL}"))
        .is_none());
}

#[test]
fn external_domain_declared() {
    let mut fixture = Fixture::new();
    assert!(fixture.is_authorized(ORIG_REQUEST, "http://www.nytimes.com"));
    assert!(!fixture.is_authorized(ORIG_REQUEST, CDN_PREFIX));
    assert!(fixture.add_domain(CDN_PREFIX));
    assert!(fixture.is_authorized(ORIG_REQUEST, CDN_PREFIX));
    let (mapped, _) = fixture
        .map_request(ORIG_REQUEST, &format!("{CDN_PREFIX}{RESOURCE_URL}"))
        .unwrap();
    assert_eq!(mapped, CDN_PREFIX);

    // A declared origin does not cover its ports.
    let port_cdn = format!("{}:8080/", &CDN_PREFIX[..CDN_PREFIX.len() - 1]);
    assert!(fixture
        .map_request(ORIG_REQUEST, &format!("{port_cdn}/{RESOURCE_URL}"))
        .is_none());
    assert!(!fixture
        .lawyer
        .do_domains_serve_same_content(&port_cdn, CDN_PREFIX));
}

#[test]
fn external_upper_case_domain_declared() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain(&CDN_PREFIX.to_ascii_uppercase()));
    let (mapped, _) = fixture
        .map_request(ORIG_REQUEST, &format!("{CDN_PREFIX}{RESOURCE_URL}"))
        .unwrap();
    assert_eq!(mapped, CDN_PREFIX);
    let port_cdn = format!("{}:8080/", &CDN_PREFIX[..CDN_PREFIX.len() - 1]);
    assert!(fixture
        .map_request(ORIG_REQUEST, &format!("{port_cdn}/{RESOURCE_URL}"))
        .is_none());
}

#[test]
fn mixed_case_path() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("EXAMPLE.com/HI/lo"));
    assert!(fixture.is_authorized(
        "http://origin.com/index.html",
        "http://example.com/HI/lo/file"
    ));
    assert!(!fixture.is_authorized(
        "http://origin.com/index.html",
        "http://example.com/hi/lo/file"
    ));
    assert!(!fixture.is_authorized(
        "http://origin.com/index.html",
        "https://example.com/HI/lo/file"
    ));
}

#[test]
fn redundant_ports_on_declaration() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("http://a.com:80"));
    assert!(fixture.add_domain("https://b.com:443"));
    assert!(fixture.is_authorized("http://origin.com/index.html", "http://a.com/file"));
    assert!(fixture.is_authorized("http://origin.com/index.html", "https://b.com/file"));
    assert!(!fixture.is_authorized("http://origin.com/index.html", "http://b.com/file"));
    assert!(!fixture.is_authorized("http://origin.com/index.html", "https://a.com/file"));
}

#[test]
fn redundant_ports_on_test() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("http://a.com"));
    assert!(fixture.add_domain("https://b.com"));
    assert!(fixture.is_authorized("http://origin.com/index.html", "http://a.com:80/file"));
    assert!(fixture.is_authorized("http://origin.com/index.html", "https://b.com:443/file"));
    assert!(!fixture.is_authorized("http://origin.com/index.html", "http://a.com:443/file"));
    assert!(!fixture.is_authorized("http://origin.com/index.html", "http://b.com:443/file"));
    assert!(!fixture.is_authorized("http://origin.com/index.html", "http://b.com:80/file"));
    assert!(!fixture.is_authorized("http://origin.com/index.html", "https://a.com:443/file"));
    assert!(!fixture.is_authorized("http://origin.com/index.html", "https://a.com:80/file"));
    assert!(!fixture.is_authorized("http://origin.com/index.html", "https://b.com:80/file"));
}

#[test]
fn external_domain_declared_without_scheme() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain(&CDN_PREFIX["http://".len()..]));
    let (mapped, _) = fixture
        .map_request(ORIG_REQUEST, &format!("{CDN_PREFIX}{RESOURCE_URL}"))
        .unwrap();
    assert_eq!(mapped, CDN_PREFIX);
}

#[test]
fn external_domain_declared_without_trailing_slash() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain(&CDN_PREFIX[..CDN_PREFIX.len() - 1]));
    let (mapped, _) = fixture
        .map_request(ORIG_REQUEST, &format!("{CDN_PREFIX}{RESOURCE_URL}"))
        .unwrap();
    assert_eq!(mapped, CDN_PREFIX);
}

#[test]
fn wildcard_domain_declared() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("*.nytimes.com"));
    let (mapped, _) = fixture
        .map_request(ORIG_REQUEST, &format!("{CDN_PREFIX}{RESOURCE_URL}"))
        .unwrap();
    assert_eq!(mapped, CDN_PREFIX);
}

#[test]
fn relative_domain_port() {
    let fixture = Fixture::new();
    let (mapped, _) = fixture.map_request(PORT_REQUEST, RESOURCE_URL).unwrap();
    assert_eq!(mapped, REQUEST_DOMAIN_PORT);
}

#[test]
fn absolute_domain_port() {
    let fixture = Fixture::new();
    let (mapped, _) = fixture
        .map_request(
            PORT_REQUEST,
            &format!("{REQUEST_DOMAIN_PORT}{RESOURCE_URL}"),
        )
        .unwrap();
    assert_eq!(mapped, REQUEST_DOMAIN_PORT);
}

#[test]
fn port_external_domain_not_declared() {
    let fixture = Fixture::new();
    assert!(fixture
        .map_request(PORT_REQUEST, &format!("{CDN_PREFIX}{RESOURCE_URL}"))
        .is_none());
}

#[test]
fn port_external_domain_declared() {
    let mut fixture = Fixture::new();
    let port_cdn = format!("{}:8080/", &CDN_PREFIX[..CDN_PREFIX.len() - 1]);
    assert!(fixture.add_domain(&port_cdn));
    let (mapped, _) = fixture
        .map_request(PORT_REQUEST, &format!("{port_cdn}{RESOURCE_URL}"))
        .unwrap();
    assert_eq!(mapped, port_cdn);
    assert!(fixture
        .map_request(PORT_REQUEST, &format!("{CDN_PREFIX}{RESOURCE_URL}"))
        .is_none());
}

#[test]
fn port_wildcard_domain_declared() {
    let mut fixture = Fixture::new();
    let port_cdn = format!("{}:8080/", &CDN_PREFIX[..CDN_PREFIX.len() - 1]);
    assert!(fixture.add_domain("*.nytimes.com:*"));
    let (mapped, _) = fixture
        .map_request(PORT_REQUEST, &format!("{port_cdn}{RESOURCE_URL}"))
        .unwrap();
    assert_eq!(mapped, port_cdn);
}

#[test]
fn https_domain() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("https://nytimes.com"));
}

#[test]
fn resource_from_https_page() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("www.nytimes.com"));
    assert!(fixture.map_request(HTTPS_REQUEST, RESOURCE_URL).is_some());
    assert!(fixture
        .map_request(HTTPS_REQUEST, &format!("{REQUEST_DOMAIN}{RESOURCE_URL}"))
        .is_some());
}

#[test]
fn rewrite_https_across_hosts() {
    let mut fixture = Fixture::new();
    assert!(
        fixture.add_rewrite_mapping("http://insecure.nytimes.com", "https://secure.nytimes.com")
    );
    assert!(fixture
        .lawyer
        .do_domains_serve_same_content("insecure.nytimes.com", "https://secure.nytimes.com"));
    assert!(fixture.lawyer.can_rewrite_domains());
    let (mapped, _) = fixture
        .map_request(
            "http://insecure.nytimes.com/index.html",
            "https://secure.nytimes.com/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://insecure.nytimes.com/");
    let (mapped, _) = fixture
        .map_request(
            "https://secure.nytimes.com/index.html",
            "http://insecure.nytimes.com/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://insecure.nytimes.com/");
    let (mapped, _) = fixture
        .map_request(
            "https://secure.nytimes.com/index.html",
            "https://secure.nytimes.com/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://insecure.nytimes.com/");
}

#[test]
fn rewrite_https_across_ports() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_rewrite_mapping("http://nytimes.com:8181", "https://nytimes.com"));
    assert!(fixture.lawyer.can_rewrite_domains());
    let (mapped, _) = fixture
        .map_request(
            "http://nytimes.com/index.html",
            "https://nytimes.com/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://nytimes.com:8181/");
    assert!(fixture
        .map_request(
            "https://nytimes.com/index.html",
            "http://nytimes.com/css/stylesheet.css",
        )
        .is_none());
    let (mapped, _) = fixture
        .map_request(
            "https://nytimes.com/index.html",
            "http://nytimes.com:8181/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://nytimes.com:8181/");
    let (mapped, _) = fixture
        .map_request(
            "https://nytimes.com/index.html",
            "https://nytimes.com/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://nytimes.com:8181/");
}

#[test]
fn rewrite_https_across_schemes() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_rewrite_mapping("http://nytimes.com", "https://nytimes.com"));
    assert!(fixture.lawyer.can_rewrite_domains());
    let (mapped, _) = fixture
        .map_request(
            "http://nytimes.com/index.html",
            "https://nytimes.com/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://nytimes.com/");
    let (mapped, _) = fixture
        .map_request(
            "https://nytimes.com/index.html",
            "http://nytimes.com/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://nytimes.com/");
    let (mapped, _) = fixture
        .map_request(
            "https://nytimes.com/index.html",
            "https://nytimes.com/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://nytimes.com/");
}

#[test]
fn rewrite_https_across_schemes_and_ports() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_rewrite_mapping("http://localhost:8080", "https://nytimes.com:8443"));
    assert!(fixture.lawyer.can_rewrite_domains());
    let (mapped, _) = fixture
        .map_request(
            "http://localhost:8080/index.html",
            "https://nytimes.com:8443/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://localhost:8080/");
    let (mapped, _) = fixture
        .map_request(
            "https://nytimes.com:8443/index.html",
            "http://localhost:8080/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://localhost:8080/");
    let (mapped, _) = fixture
        .map_request(
            "https://nytimes.com:8443/index.html",
            "https://nytimes.com:8443/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://localhost:8080/");
    let (mapped, _) = fixture
        .map_request("https://nytimes.com:8443/index.html", "css/stylesheet.css")
        .unwrap();
    assert_eq!(mapped, "http://localhost:8080/");
}

#[test]
fn rewrite_https_to_https() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_rewrite_mapping("https://localhost:8443", "https://nytimes.com:8443"));
    assert!(fixture.lawyer.can_rewrite_domains());
    let (mapped, _) = fixture
        .map_request(
            "https://localhost:8443/index.html",
            "https://nytimes.com:8443/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "https://localhost:8443/");
    let (mapped, _) = fixture
        .map_request(
            "https://nytimes.com:8443/index.html",
            "https://localhost:8443/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "https://localhost:8443/");
    let (mapped, _) = fixture
        .map_request(
            "https://nytimes.com:8443/index.html",
            "https://nytimes.com:8443/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "https://localhost:8443/");
    let (mapped, _) = fixture
        .map_request("https://nytimes.com:8443/index.html", "css/stylesheet.css")
        .unwrap();
    assert_eq!(mapped, "https://localhost:8443/");
}

#[test]
fn add_two_protocol_rewrite_domain_mapping() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_two_protocol_rewrite_domain_mapping(
        "www.nytimes.com",
        "ref.nytimes.com",
        Some(&mut fixture.handler)
    ));
    assert!(fixture.lawyer.can_rewrite_domains());
    let (mapped, _) = fixture
        .map_request(
            "http://www.nytimes.com/index.html",
            "http://ref.nytimes.com/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://www.nytimes.com/");
    let (mapped, _) = fixture
        .map_request(
            "http://www.nytimes.com/index.html",
            "https://ref.nytimes.com/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "https://www.nytimes.com/");
    let (mapped, _) = fixture
        .map_request(
            "https://www.nytimes.com/index.html",
            "https://ref.nytimes.com/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "https://www.nytimes.com/");
    let (mapped, _) = fixture
        .map_request(
            "https://www.nytimes.com/index.html",
            "http://ref.nytimes.com/css/stylesheet.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://www.nytimes.com/");
}

#[test]
fn find_domains_rewritten_to() {
    let mut fixture = Fixture::new();
    assert!(fixture
        .lawyer
        .find_domains_rewritten_to(&GoogleUrl::new("http://www1.example.com/"))
        .is_empty());
    assert!(fixture.lawyer.add_two_protocol_rewrite_domain_mapping(
        "www1.example.com",
        "www.example.com",
        Some(&mut fixture.handler)
    ));
    assert!(fixture.lawyer.add_two_protocol_rewrite_domain_mapping(
        "www1.example.com",
        "xyz.example.com",
        Some(&mut fixture.handler)
    ));
    assert_eq!(
        fixture
            .lawyer
            .find_domains_rewritten_to(&GoogleUrl::new("http://www1.example.com/")),
        vec!["http://www.example.com/", "http://xyz.example.com/"]
    );
}

#[test]
fn add_domain_redundantly() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("www.nytimes.com"));
    assert!(!fixture.add_domain("www.nytimes.com"));
    assert!(fixture.add_domain("*"));
    assert!(!fixture.add_domain("*"));
}

#[test]
fn verify_port_is_distinct_1() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("www.example.com"));
    assert!(fixture
        .map_request(
            "http://www.other.com/index.html",
            "http://www.example.com:81/styles.css"
        )
        .is_none());
}

#[test]
fn verify_port_is_distinct_2() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("www.example.com:81"));
    assert!(fixture
        .map_request(
            "http://www.other.com/index.html",
            "http://www.example.com/styles.css"
        )
        .is_none());
}

#[test]
fn verify_wildcarded_port_spec() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("www.example.com*"));
    assert!(fixture
        .map_request(
            "http://www.origin.com/index.html",
            "http://www.example.com/styles.css"
        )
        .is_some());
    assert!(fixture
        .map_request(
            "http://www.origin.com/index.html",
            "http://www.example.com:81/styles.css"
        )
        .is_some());
}

#[test]
fn map_rewrite_domain() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("http://cdn.com/"));
    assert!(fixture.add_domain("http://origin.com/"));
    assert!(!fixture
        .lawyer
        .do_domains_serve_same_content("cdn.com", "origin.com"));
    assert!(fixture.add_rewrite_mapping("http://cdn.com", "http://origin.com"));
    assert!(fixture
        .lawyer
        .do_domains_serve_same_content("cdn.com", "origin.com"));
    assert!(fixture.lawyer.can_rewrite_domains());
    let (mapped, _) = fixture
        .map_request(
            "http://www.origin.com/index.html",
            "http://origin.com/styles/blue.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://cdn.com/");
    let (mapped, _) = fixture
        .map_request("http://www.origin.com/index.html", "styles/blue.css")
        .unwrap();
    assert_eq!(mapped, "http://www.origin.com/");
    assert!(fixture.add_rewrite_mapping("http://cdn.com", "http://www.origin.com"));
    let (mapped, _) = fixture
        .map_request("http://www.origin.com/index.html", "styles/blue.css")
        .unwrap();
    assert_eq!(mapped, "http://cdn.com/");
}

#[test]
fn map_rewrite_domain_and_path() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("http://cdn.com/origin/"));
    assert!(fixture.add_domain("http://origin.com/"));
    assert!(fixture.add_rewrite_mapping("http://cdn.com/origin", "http://origin.com"));
    assert!(fixture.lawyer.can_rewrite_domains());
    let (mapped, resolved) = fixture
        .map_request(
            "http://www.origin.com/index.html",
            "http://origin.com/styles/blue.css",
        )
        .unwrap();
    assert_eq!(mapped, "http://cdn.com/origin/");
    assert_eq!(resolved.spec(), "http://cdn.com/origin/styles/blue.css");
    let (mapped, resolved) = fixture
        .map_request("http://www.origin.com/index.html", "styles/blue.css")
        .unwrap();
    assert_eq!(mapped, "http://www.origin.com/");
    assert_eq!(resolved.spec(), "http://www.origin.com/styles/blue.css");
    assert!(fixture.add_rewrite_mapping("http://cdn.com/origin", "http://www.origin.com"));
    let (mapped, resolved) = fixture
        .map_request("http://www.origin.com/index.html", "styles/blue.css")
        .unwrap();
    assert_eq!(mapped, "http://cdn.com/origin/");
    assert_eq!(resolved.spec(), "http://cdn.com/origin/styles/blue.css");
}

#[test]
fn rewrite_with_path() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_rewrite_mapping(
        "http://example.com/static/images/",
        "http://static.com/images/"
    ));
    let (mapped, resolved) = fixture
        .map_request(
            "http://example.com/index.html",
            "http://static.com/images/teapot.png",
        )
        .unwrap();
    assert_eq!(mapped, "http://example.com/static/images/");
    assert_eq!(
        resolved.spec(),
        "http://example.com/static/images/teapot.png"
    );
}

#[test]
fn add_mapping_failures() {
    let mut fixture = Fixture::new();
    assert!(!fixture.add_rewrite_mapping("", "http://origin.com"));
    assert!(!fixture.add_rewrite_mapping("http://cdn.com", ""));
    assert!(!fixture.add_rewrite_mapping("http://cdn.com", ","));
    assert!(!fixture.add_rewrite_mapping("http://origin.com", "http://origin.com"));
    assert!(!fixture.lawyer.can_rewrite_domains());
    assert!(!fixture.add_rewrite_mapping("http://origin.com/newroot", "http://origin.com"));
    assert!(!fixture.lawyer.can_rewrite_domains());
    assert!(!fixture.add_rewrite_mapping("http://origin.com", "http://origin.com,"));
    assert!(!fixture.add_rewrite_mapping("http://origin.com", ",http://origin.com"));
    assert!(!fixture.add_rewrite_mapping("http://origin.com/newroot", "http://origin.com,"));
    assert!(!fixture.add_rewrite_mapping("http://origin.com/newroot", ",http://origin.com"));
    assert!(!fixture.add_rewrite_mapping("foo*.com", "bar.com"));
    // NOTE: the origin-mapping wildcard line ports with the origin slice.
    assert!(!fixture.add_shard("foo*.com", "bar.com"));
    assert!(fixture.add_rewrite_mapping("foo.com", "bar*.com"));
    assert!(!fixture.add_shard("foo.com", "bar*.com"));
    assert!(fixture.add_shard("foo.com", "bar1.com,bar2.com"));
}

#[test]
fn rejected_mapping_leaves_domains_unauthorized() {
    let mut fixture = Fixture::new();
    assert!(!fixture.add_shard("*.foo.com", "bar.com"));
    assert!(!fixture.is_authorized(ORIG_REQUEST, "http://baz.foo.com/"));
    assert!(!fixture.is_authorized(ORIG_REQUEST, "http://bar.com/"));
    assert!(!fixture.add_shard("foo.com", "*.bar.com"));
    assert!(!fixture.is_authorized(ORIG_REQUEST, "http://cdn.bar.com/"));
    assert!(!fixture.add_shard("*", "bar.com"));
    assert!(!fixture.is_authorized(ORIG_REQUEST, "http://unrelated.com/"));
}

#[test]
fn shard() {
    let mut fixture = Fixture::new();
    assert!(!fixture.lawyer.can_rewrite_domains());
    assert!(fixture.add_shard("foo.com", "bar1.com,bar2.com"));
    assert!(fixture.lawyer.can_rewrite_domains());
    assert_eq!(
        fixture.lawyer.shard_domain("http://foo.com/", 0).as_deref(),
        Some("http://bar1.com/")
    );
    assert_eq!(
        fixture.lawyer.shard_domain("http://foo.com/", 1).as_deref(),
        Some("http://bar2.com/")
    );
    assert_eq!(fixture.lawyer.shard_domain("http://other.com/", 0), None);
}

#[test]
fn shard_https() {
    let mut fixture = Fixture::new();
    assert!(!fixture.lawyer.can_rewrite_domains());
    assert!(fixture.add_shard("https://foo.com", "https://bar1.com,https://bar2.com"));
    assert!(fixture.lawyer.can_rewrite_domains());
    assert_eq!(
        fixture
            .lawyer
            .shard_domain("https://foo.com/", 0)
            .as_deref(),
        Some("https://bar1.com/")
    );
    assert_eq!(
        fixture
            .lawyer
            .shard_domain("https://foo.com/", 1)
            .as_deref(),
        Some("https://bar2.com/")
    );
    assert_eq!(fixture.lawyer.shard_domain("https://other.com/", 0), None);
}

#[test]
fn will_domain_change() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_shard("foo.com", "bar1.com,bar2.com"));
    assert!(fixture.add_rewrite_mapping("http://cdn.com", "http://origin.com"));
    assert!(fixture.will_change("http://foo.com/"));
    assert!(fixture.will_change("foo.com/"));
    assert!(fixture.will_change("http://foo.com"));
    assert!(fixture.will_change("foo.com"));
    assert!(fixture.will_change("http://origin.com/"));
    assert!(fixture.will_change("http://bar1.com/"));
    assert!(fixture.will_change("http://bar2.com/"));
    assert!(!fixture.will_change("http://cdn.com/"));
    assert!(!fixture.will_change("http://other_domain.com/"));
}

#[test]
fn will_domain_change_subdirectory() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_rewrite_mapping("http://cdn.com", "http://origin.com/subdir"));
    assert!(!fixture.will_change("http://origin.com/"));
    assert!(!fixture.will_change("http://origin.com/subdirx"));
    assert!(fixture.will_change("http://origin.com/subdir/x"));
}

#[test]
fn will_domain_change_only_one_shard() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_shard("foo.com", "bar1.com"));
    assert!(fixture.add_rewrite_mapping("http://cdn.com", "http://origin.com"));
    assert!(fixture.will_change("http://foo.com/"));
    assert!(fixture.will_change("foo.com/"));
    assert!(fixture.will_change("http://foo.com"));
    assert!(fixture.will_change("foo.com"));
    assert!(fixture.will_change("http://origin.com/"));
    assert!(!fixture.will_change("http://bar1.com/"));
    assert!(!fixture.will_change("http://cdn.com/"));
    assert!(!fixture.will_change("http://other_domain.com/"));
}

#[test]
fn shard_conflict_1() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_shard("cdn1.com", "s1.com,s2.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(!fixture.add_shard("cdn2.com", "s2.com,s3.com"));
    assert_eq!(fixture.handler.serious_messages(), 1);
}

#[test]
fn compute_signature_rewrite_and_shards() {
    // The origin-mapping half of ComputeSignatureTest ports with the
    // origin slice; this covers the rewrite/shard half verbatim.
    let mut fixture = Fixture::new();
    assert!(fixture.add_rewrite_mapping("cdn.com", "myhost1.com,myhost2.com"));
    assert_eq!(
        fixture.lawyer.signature(),
        "D:http://cdn.com/__a_-D:http://myhost1.com/__a_R:http://cdn.com/_-D:http://myhost2.com/__a_R:http://cdn.com/_-"
    );
}

#[test]
fn to_string_rewrite_and_shards() {
    // The origin-mapping half of ToStringTest ports with the origin
    // slice; this covers the rewrite/shard half verbatim.
    let mut fixture = Fixture::new();
    assert!(fixture.add_rewrite_mapping("myhost.cdn.com", "myhost1.com,myhost2.com"));
    assert!(fixture.add_shard("domain1", "shard,shard2"));
    assert_eq!(
        fixture.lawyer.to_string_list(),
        "http://domain1/ Auth Shards:{http://shard/, http://shard2/}\nhttp://myhost.cdn.com/ Auth\nhttp://myhost1.com/ Auth RewriteDomain:http://myhost.cdn.com/\nhttp://myhost2.com/ Auth RewriteDomain:http://myhost.cdn.com/\nhttp://shard/ Auth RewriteDomain:http://domain1/\nhttp://shard2/ Auth RewriteDomain:http://domain1/\n"
    );
}

impl Fixture {
    fn add_origin_mapping(&mut self, to: &str, from: &str) -> bool {
        self.lawyer
            .add_origin_domain_mapping(to, from, "", Some(&mut self.handler))
    }

    fn map_origin(&self, url: &str) -> Option<String> {
        let (out, _, is_proxy) = self.lawyer.map_origin(url)?;
        (!is_proxy).then_some(out)
    }

    fn map_origin_and_host(&self, url: &str) -> Option<(String, String)> {
        let (out, host, is_proxy) = self.lawyer.map_origin(url)?;
        (!is_proxy).then_some((out, host))
    }

    fn map_proxy(&self, url: &str) -> Option<String> {
        let (out, _, is_proxy) = self.lawyer.map_origin(url)?;
        is_proxy.then_some(out)
    }
}

#[test]
fn map_https_across_hosts() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("http://insecure.nytimes.com", "https://secure.nytimes.com"));
    assert!(fixture.add_origin_mapping("https://secure.nytimes.com", "http://insecure.nytimes.com"));
    assert_eq!(
        fixture
            .map_origin("https://secure.nytimes.com/css/stylesheet.css")
            .as_deref(),
        Some("http://insecure.nytimes.com/css/stylesheet.css")
    );
    assert_eq!(
        fixture
            .map_origin("http://insecure.nytimes.com/css/stylesheet.css")
            .as_deref(),
        Some("https://secure.nytimes.com/css/stylesheet.css")
    );
}

#[test]
fn map_https_across_schemes() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("http://nytimes.com", "https://nytimes.com"));
    assert!(fixture.add_origin_mapping("https://nytimes.com", "http://nytimes.com"));
    assert_eq!(
        fixture
            .map_origin("https://nytimes.com/css/stylesheet.css")
            .as_deref(),
        Some("http://nytimes.com/css/stylesheet.css")
    );
    assert_eq!(
        fixture
            .map_origin("http://nytimes.com/css/stylesheet.css")
            .as_deref(),
        Some("https://nytimes.com/css/stylesheet.css")
    );
}

#[test]
fn map_https_across_ports() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("http://nytimes.com:8181", "https://nytimes.com"));
    assert_eq!(
        fixture
            .map_origin("https://nytimes.com/css/stylesheet.css")
            .as_deref(),
        Some("http://nytimes.com:8181/css/stylesheet.css")
    );
}

#[test]
fn map_https_across_schemes_and_ports() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("http://localhost:8080", "https://nytimes.com:8443"));
    assert_eq!(
        fixture
            .map_origin("https://nytimes.com:8443/css/stylesheet.css")
            .as_deref(),
        Some("http://localhost:8080/css/stylesheet.css")
    );
}

#[test]
fn failed_origin_mapping_leaves_no_host_header() {
    let mut fixture = Fixture::new();
    assert!(!fixture.lawyer.add_origin_domain_mapping(
        "http://a.example.com",
        "http://a.example.com",
        "stray.example.org",
        Some(&mut fixture.handler)
    ));
    assert!(!fixture.lawyer.signature().contains("stray.example.org"));
    assert!(!fixture
        .lawyer
        .to_string_list()
        .contains("stray.example.org"));
}

#[test]
fn add_two_protocol_origin_domain_mapping() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_two_protocol_origin_domain_mapping(
        "ref.nytimes.com",
        "www.nytimes.com",
        "",
        Some(&mut fixture.handler)
    ));
    assert!(!fixture.lawyer.can_rewrite_domains());
    assert_eq!(
        fixture.map_origin_and_host("http://www.nytimes.com/index.html"),
        Some((
            "http://ref.nytimes.com/index.html".to_string(),
            "www.nytimes.com".to_string()
        ))
    );
    assert_eq!(
        fixture.map_origin_and_host("https://www.nytimes.com/index.html"),
        Some((
            "https://ref.nytimes.com/index.html".to_string(),
            "www.nytimes.com".to_string()
        ))
    );
}

#[test]
fn add_two_protocol_origin_with_ref_port() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_two_protocol_origin_domain_mapping(
        "ref.nytimes.com:8089",
        "www.nytimes.com",
        "",
        Some(&mut fixture.handler)
    ));
    assert!(!fixture.lawyer.can_rewrite_domains());
    assert_eq!(
        fixture.map_origin_and_host("http://www.nytimes.com/index.html"),
        Some((
            "http://ref.nytimes.com:8089/index.html".to_string(),
            "www.nytimes.com".to_string()
        ))
    );
    assert_eq!(
        fixture.map_origin_and_host("https://www.nytimes.com/index.html"),
        Some((
            "https://ref.nytimes.com:8089/index.html".to_string(),
            "www.nytimes.com".to_string()
        ))
    );
}

#[test]
fn add_two_protocol_origin_with_serving_port() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_two_protocol_origin_domain_mapping(
        "ref.nytimes.com",
        "www.nytimes.com:8080",
        "",
        Some(&mut fixture.handler)
    ));
    assert!(!fixture.lawyer.can_rewrite_domains());
    assert_eq!(
        fixture.map_origin_and_host("http://www.nytimes.com:8080/index.html"),
        Some((
            "http://ref.nytimes.com/index.html".to_string(),
            "www.nytimes.com:8080".to_string()
        ))
    );
    assert_eq!(
        fixture.map_origin_and_host("http://www.nytimes.com/index.html"),
        Some((
            "http://www.nytimes.com/index.html".to_string(),
            "www.nytimes.com".to_string()
        ))
    );
    assert_eq!(
        fixture.map_origin_and_host("https://www.nytimes.com:8080/index.html"),
        Some((
            "https://ref.nytimes.com/index.html".to_string(),
            "www.nytimes.com:8080".to_string()
        ))
    );
    assert_eq!(
        fixture.map_origin_and_host("https://www.nytimes.com/index.html"),
        Some((
            "https://www.nytimes.com/index.html".to_string(),
            "www.nytimes.com".to_string()
        ))
    );
}

#[test]
fn add_two_protocol_origin_with_both_ports() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_two_protocol_origin_domain_mapping(
        "ref.nytimes.com:9999",
        "www.nytimes.com:8080",
        "",
        Some(&mut fixture.handler)
    ));
    assert!(!fixture.lawyer.can_rewrite_domains());
    assert_eq!(
        fixture.map_origin_and_host("http://www.nytimes.com:8080/index.html"),
        Some((
            "http://ref.nytimes.com:9999/index.html".to_string(),
            "www.nytimes.com:8080".to_string()
        ))
    );
    assert_eq!(
        fixture.map_origin_and_host("https://www.nytimes.com:8080/index.html"),
        Some((
            "https://ref.nytimes.com:9999/index.html".to_string(),
            "www.nytimes.com:8080".to_string()
        ))
    );
}

#[test]
fn add_two_protocol_origin_with_host_header() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_two_protocol_origin_domain_mapping(
        "ref.nytimes.com",
        "www.nytimes.com",
        "host.nytimes.com",
        Some(&mut fixture.handler)
    ));
    assert!(!fixture.lawyer.can_rewrite_domains());
    assert_eq!(
        fixture.map_origin_and_host("http://www.nytimes.com/index.html"),
        Some((
            "http://ref.nytimes.com/index.html".to_string(),
            "host.nytimes.com".to_string()
        ))
    );
    assert_eq!(
        fixture.map_origin_and_host("https://www.nytimes.com/index.html"),
        Some((
            "https://ref.nytimes.com/index.html".to_string(),
            "host.nytimes.com".to_string()
        ))
    );
}

#[test]
fn map_origin_explicit_host() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_origin_domain_mapping(
        "origin",
        "*domain",
        "host",
        Some(&mut fixture.handler)
    ));
    assert_eq!(
        fixture.map_origin_and_host("http://www.domain/foo.css"),
        Some(("http://origin/foo.css".to_string(), "host".to_string()))
    );
}

#[test]
fn map_origin_without_explicit_host() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("origin", "*domain"));
    assert_eq!(
        fixture.map_origin_and_host("http://www.domain/foo.css"),
        Some((
            "http://origin/foo.css".to_string(),
            "www.domain".to_string()
        ))
    );
}

#[test]
fn origin_with_path() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("http://origin.com/subdir/", "http://external.com"));
    assert_eq!(
        fixture
            .map_origin("http://external.com/styles/main.css")
            .as_deref(),
        Some("http://origin.com/subdir/styles/main.css")
    );
}

#[test]
fn origin_with_path_on_source() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("http://origin.com/subdir/", "http://external.com/path"));
    assert_eq!(
        fixture
            .map_origin("http://external.com/path/styles/main.css")
            .as_deref(),
        Some("http://origin.com/subdir/styles/main.css")
    );
}

#[test]
fn origin_and_extern_with_paths() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("http://origin.com/subdir/", "http://external.com/static/"));
    assert_eq!(
        fixture
            .map_origin("http://external.com/static/styles/main.css")
            .as_deref(),
        Some("http://origin.com/subdir/styles/main.css")
    );
}

#[test]
fn origin_and_extern_with_multiple_matches() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("http://origin.com"));
    assert!(fixture.add_domain("http://origin.com/a/b"));
    assert!(fixture.add_domain("http://external.com"));
    assert!(fixture.add_origin_mapping("http://origin.com/a/", "http://external.com/static/"));
    assert_eq!(
        fixture
            .map_origin("http://external.com/static/styles/main.css")
            .as_deref(),
        Some("http://origin.com/a/styles/main.css")
    );
    assert_eq!(
        fixture
            .map_origin("http://external.com/index.html")
            .as_deref(),
        Some("http://external.com/index.html")
    );
}

#[test]
fn root_domain_of_proxy_source_not_authorized() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("http://origin.com/a/", "http://external.com/static/"));
    assert!(!fixture.is_authorized("http://origin.com/index.html", "http://external.com"));
    let mut permissive = Fixture::new();
    assert!(permissive.add_domain("*"));
    assert!(permissive.is_authorized("http://origin.com/index.html", "http://external.com"));
    assert!(fixture.is_authorized(
        "http://origin.com/index.html",
        "http://external.com/static/"
    ));
}

#[test]
fn origin_and_extern_double_slash() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("http://origin.com"));
    assert!(fixture.add_domain("http://external.com"));
    assert!(fixture.add_origin_mapping("http://origin.com/subdir/", "http://external.com/static/"));
    assert_eq!(
        fixture
            .map_origin("http://external.com/static/styles//main.css")
            .as_deref(),
        Some("http://origin.com/subdir/styles//main.css")
    );
}

#[test]
fn map_origin_domain() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("http://localhost:8080", "http://origin.com:8080"));
    assert_eq!(
        fixture
            .map_origin("http://origin.com:8080/a/b/c?d=f")
            .as_deref(),
        Some("http://localhost:8080/a/b/c?d=f")
    );
    assert!(fixture
        .map_request(
            "http://origin.com:8080/index.html",
            "http://localhost:8080/blue.css"
        )
        .is_none());
    assert!(!fixture.is_authorized("http://origin.com:8080", "http://localhost:8080"));
    assert!(fixture.add_domain("localhost*"));
    assert!(fixture
        .map_request(
            "http://origin.com:8080/index.html",
            "http://localhost:8080/blue.css"
        )
        .is_none());
    assert!(fixture.add_domain("localhost:8080"));
    assert!(fixture
        .map_request(
            "http://origin.com:8080/index.html",
            "http://localhost:8080/blue.css"
        )
        .is_some());
}

#[test]
fn proxy_external_resource() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_proxy_domain_mapping(
        "http://origin.com/external",
        "http://external.com/static",
        "",
        Some(&mut fixture.handler)
    ));
    let (mapped, resolved) = fixture
        .map_request(
            "http://origin.com/index.html",
            "http://external.com/static/images/proxy_this.png",
        )
        .unwrap();
    assert_eq!(mapped, "http://origin.com/external/");
    assert_eq!(
        resolved.spec(),
        "http://origin.com/external/images/proxy_this.png"
    );
    assert_eq!(
        fixture.map_proxy(resolved.spec()).as_deref(),
        Some("http://external.com/static/images/proxy_this.png")
    );
    assert!(fixture
        .map_request(
            "http://origin.com/index.html",
            "http://external.com/evil/gifar.gif"
        )
        .is_none());
    assert!(fixture
        .map_request(
            "http://origin.com/index.html",
            "http://external.com/gifar.gif"
        )
        .is_none());
}

#[test]
fn proxy_external_resource_to_cdn() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_proxy_domain_mapping(
        "http://proxy.com/external",
        "http://origin.com/static",
        "http://cdn.com/external",
        Some(&mut fixture.handler)
    ));
    let (_, resolved) = fixture
        .map_request(
            "http://proxy.com/index.html",
            "http://origin.com/static/images/proxy_this.png",
        )
        .unwrap();
    assert_eq!(
        resolved.spec(),
        "http://cdn.com/external/images/proxy_this.png"
    );
    let (_, resolved) = fixture
        .map_request(
            "http://proxy.com/index.html",
            "http://proxy.com/external/images/proxy_this.png",
        )
        .unwrap();
    assert_eq!(
        resolved.spec(),
        "http://cdn.com/external/images/proxy_this.png"
    );
    assert_eq!(
        fixture
            .map_proxy("http://cdn.com/external/images/proxy_this.png")
            .as_deref(),
        Some("http://origin.com/static/images/proxy_this.png")
    );
    assert_eq!(
        fixture
            .map_proxy("http://proxy.com/external/images/proxy_this.png")
            .as_deref(),
        Some("http://origin.com/static/images/proxy_this.png")
    );
    assert!(fixture
        .map_request(
            "http://proxy.com/index.html",
            "http://origin.com/evil/gifar.gif"
        )
        .is_none());
    assert!(fixture
        .map_request("http://proxy.com/index.html", "http://origin.com/gifar.gif")
        .is_none());
    assert!(fixture
        .lawyer
        .is_proxy_mapped(&GoogleUrl::new("http://proxy.com/external/a.b")));
    assert!(!fixture
        .lawyer
        .is_proxy_mapped(&GoogleUrl::new("http://proxy.com/a.b")));
    assert!(!fixture
        .lawyer
        .is_proxy_mapped(&GoogleUrl::new("http://origin.com/static/a.b")));
    assert!(!fixture
        .lawyer
        .is_proxy_mapped(&GoogleUrl::new("http://origin.com/a.b")));
    assert!(fixture
        .lawyer
        .is_proxy_mapped(&GoogleUrl::new("http://cdn.com/external/a.b")));
    assert!(!fixture
        .lawyer
        .is_proxy_mapped(&GoogleUrl::new("http://cdn.com/a.b")));
}

#[test]
fn proxy_external_resource_from_https() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_proxy_domain_mapping(
        "http://origin.com/external",
        "https://external.com/static",
        "",
        Some(&mut fixture.handler)
    ));
    let (mapped, resolved) = fixture
        .map_request(
            "http://origin.com/index.html",
            "https://external.com/static/images/proxy_this.png",
        )
        .unwrap();
    assert_eq!(mapped, "http://origin.com/external/");
    assert_eq!(
        resolved.spec(),
        "http://origin.com/external/images/proxy_this.png"
    );
    assert_eq!(
        fixture.map_proxy(resolved.spec()).as_deref(),
        Some("https://external.com/static/images/proxy_this.png")
    );
    assert!(fixture
        .map_request(
            "http://origin.com/index.html",
            "https://external.com/evil/gifar.gif"
        )
        .is_none());
    assert!(fixture
        .map_request(
            "http://origin.com/index.html",
            "https://external.com/gifar.gif"
        )
        .is_none());
}

#[test]
fn proxy_ambiguous() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_proxy_domain_mapping(
        "http://proxy.com/origin",
        "http://origin.com",
        "",
        Some(&mut fixture.handler)
    ));
    assert_eq!(
        fixture.map_proxy("http://proxy.com/origin/x").as_deref(),
        Some("http://origin.com/x")
    );
    assert!(!fixture.lawyer.add_proxy_domain_mapping(
        "http://proxy.com/origin",
        "http://ambiguous.com",
        "",
        Some(&mut fixture.handler)
    ));
    assert_eq!(
        fixture.map_proxy("http://proxy.com/origin/x").as_deref(),
        Some("http://origin.com/x")
    );
    assert!(!fixture.add_origin_mapping("http://ambiguous.com", "http://proxy.com/origin"));
    assert_eq!(
        fixture.map_proxy("http://proxy.com/origin/x").as_deref(),
        Some("http://origin.com/x")
    );
    assert!(fixture.add_origin_mapping("http://origin1.com", "http://x.com"));
    assert_eq!(
        fixture.map_origin("http://x.com/y").as_deref(),
        Some("http://origin1.com/y")
    );
    assert!(fixture.add_origin_mapping("http://origin2.com", "http://x.com"));
    assert_eq!(
        fixture.map_origin("http://x.com/y").as_deref(),
        Some("http://origin2.com/y")
    );
    assert!(!fixture.lawyer.add_proxy_domain_mapping(
        "http://proxy2.com/origin",
        "http://origin.com",
        "",
        Some(&mut fixture.handler)
    ));
}

#[test]
fn merge() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("http://d1.com/"));
    assert!(fixture.add_rewrite_mapping("http://cdn1.com", "http://www.o1.com"));
    assert!(fixture.add_origin_mapping("http://localhost:8080", "http://o1.com:8080"));
    assert!(fixture.lawyer.add_proxy_domain_mapping(
        "http://proxy.com/origin",
        "http://origin.com",
        "",
        Some(&mut fixture.handler)
    ));
    assert!(fixture.add_origin_mapping("http://dest1/", "http://common_src1"));
    assert!(fixture.add_origin_mapping("http://dest2/", "http://common_src2"));
    assert!(fixture.add_shard("foo.com", "bar1.com,bar2.com"));
    assert!(fixture
        .lawyer
        .do_domains_serve_same_content("foo.com", "bar1.com"));
    assert!(fixture
        .lawyer
        .do_domains_serve_same_content("foo.com", "bar2.com"));
    assert!(fixture
        .lawyer
        .do_domains_serve_same_content("bar1.com", "bar2.com"));
    assert!(fixture
        .lawyer
        .do_domains_serve_same_content("bar1.com", "foo.com"));
    assert!(fixture
        .lawyer
        .do_domains_serve_same_content("bar2.com", "foo.com"));
    assert!(fixture
        .lawyer
        .do_domains_serve_same_content("bar2.com", "bar1.com"));
    assert_eq!(
        fixture.map_proxy("http://proxy.com/origin/x").as_deref(),
        Some("http://origin.com/x")
    );

    let mut merged = Fixture::new();
    assert!(merged.add_domain("http://d2.com/"));
    assert!(merged.add_rewrite_mapping("http://cdn2.com", "http://www.o2.com"));
    assert!(merged.add_origin_mapping("http://localhost:8080", "http://o2.com:8080"));
    assert!(merged.add_origin_mapping("http://dest3/", "http://common_src1"));
    assert!(fixture.add_origin_mapping("http://dest4/", "http://common_src3"));
    merged.lawyer.merge(&fixture.lawyer);

    let (mapped, _) = merged
        .map_request("http://www.o1.com/index.html", "styles/blue.css")
        .unwrap();
    assert_eq!(mapped, "http://cdn1.com/");
    let (mapped, _) = merged
        .map_request("http://www.o2.com/index.html", "styles/blue.css")
        .unwrap();
    assert_eq!(mapped, "http://cdn2.com/");
    assert_eq!(
        merged.map_origin_and_host("http://o1.com:8080/a/b/c?d=f"),
        Some((
            "http://localhost:8080/a/b/c?d=f".to_string(),
            "o1.com:8080".to_string()
        ))
    );
    assert_eq!(
        merged.map_origin_and_host("http://o2.com:8080/a/b/c?d=f"),
        Some((
            "http://localhost:8080/a/b/c?d=f".to_string(),
            "o2.com:8080".to_string()
        ))
    );
    assert_eq!(
        merged.map_origin_and_host("http://common_src1"),
        Some(("http://dest1/".to_string(), "common_src1".to_string()))
    );
    assert_eq!(
        merged.map_origin_and_host("http://common_src2"),
        Some(("http://dest2/".to_string(), "common_src2".to_string()))
    );
    assert_eq!(
        merged.map_origin_and_host("http://common_src3"),
        Some(("http://dest4/".to_string(), "common_src3".to_string()))
    );
    assert_eq!(
        merged.lawyer.shard_domain("http://foo.com/", 0).as_deref(),
        Some("http://bar1.com/")
    );
    assert!(merged
        .lawyer
        .do_domains_serve_same_content("foo.com", "bar1.com"));
    assert!(merged
        .lawyer
        .do_domains_serve_same_content("foo.com", "bar2.com"));
    assert!(merged
        .lawyer
        .do_domains_serve_same_content("bar1.com", "bar2.com"));
    assert!(merged
        .lawyer
        .do_domains_serve_same_content("bar1.com", "foo.com"));
    assert!(merged
        .lawyer
        .do_domains_serve_same_content("bar2.com", "foo.com"));
    assert!(merged
        .lawyer
        .do_domains_serve_same_content("bar2.com", "bar1.com"));
    assert!(merged
        .lawyer
        .do_domains_serve_same_content("cdn1.com", "www.o1.com"));
    assert!(merged
        .lawyer
        .do_domains_serve_same_content("cdn2.com", "www.o2.com"));
    assert!(!merged
        .lawyer
        .do_domains_serve_same_content("cdn1.com", "cdn2.com"));
    let (out, host, is_proxy) = merged
        .lawyer
        .map_origin("http://proxy.com/origin/x")
        .map(|(out, host, proxy)| (out, host, proxy))
        .unwrap();
    assert_eq!(out, "http://origin.com/x");
    assert!(is_proxy);
    assert_eq!(host, "proxy.com");
}

#[test]
fn map_rewrite_to_origin_domain() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_rewrite_mapping("rewrite.com", "myhost.com"));
    assert!(fixture.add_origin_mapping("localhost", "myhost.com"));
    assert_eq!(
        fixture
            .map_origin("http://rewrite.com/a/b/c?d=f")
            .as_deref(),
        Some("http://localhost/a/b/c?d=f")
    );
}

#[test]
fn map_shard_to_origin_domain() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_rewrite_mapping("cdn.myhost.com", "myhost.com"));
    assert!(fixture.add_origin_mapping("localhost", "myhost.com"));
    assert!(fixture.add_shard("cdn.myhost.com", "s1.com,s2.com"));
    assert_eq!(
        fixture.map_origin("http://s1.com/a/b/c?d=f").as_deref(),
        Some("http://localhost/a/b/c?d=f")
    );
    assert_eq!(
        fixture.map_origin("http://s2.com/a/b/c?d=f").as_deref(),
        Some("http://localhost/a/b/c?d=f")
    );
}

#[test]
fn conflicted_origin_1() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("localhost", "myhost.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_origin_mapping("other", "myhost.com"));
    assert_eq!(fixture.handler.serious_messages(), 1);
    assert_eq!(
        fixture.map_origin("http://myhost.com/x").as_deref(),
        Some("http://other/x")
    );
}

#[test]
fn no_conflict_on_merge_1() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("localhost", "myhost1.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_origin_mapping("localhost", "myhost2.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_rewrite_mapping("cdn.com", "myhost1.com,myhost2.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert_eq!(
        fixture.map_origin("http://myhost1.com/x").as_deref(),
        Some("http://localhost/x")
    );
    assert_eq!(
        fixture.map_origin("http://myhost2.com/y").as_deref(),
        Some("http://localhost/y")
    );
    assert_eq!(
        fixture.map_origin("http://cdn.com/z").as_deref(),
        Some("http://localhost/z")
    );
}

#[test]
fn conflicted_origin_2() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("origin1.com", "myhost1.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_origin_mapping("origin2.com", "myhost2.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_rewrite_mapping("cdn.com", "myhost1.com,myhost2.com"));
    assert_eq!(fixture.handler.serious_messages(), 1);
    assert_eq!(
        fixture.map_origin("http://cdn.com/x").as_deref(),
        Some("http://origin2.com/x")
    );
    assert_eq!(
        fixture.map_origin("http://myhost1.com/y").as_deref(),
        Some("http://origin1.com/y")
    );
}

#[test]
fn no_shard_conflict() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("localhost", "myhost1.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_origin_mapping("localhost", "myhost2.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_rewrite_mapping("cdn.com", "myhost1.com,myhost2.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_shard("cdn.com", "s1.com,s2.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert_eq!(
        fixture.map_origin("http://cdn.com/x").as_deref(),
        Some("http://localhost/x")
    );
    assert_eq!(
        fixture.map_origin("http://s1.com/x").as_deref(),
        Some("http://localhost/x")
    );
    assert_eq!(
        fixture.map_origin("http://s2.com/x").as_deref(),
        Some("http://localhost/x")
    );
}

#[test]
fn no_shard_conflict_reverse() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_shard("cdn.com", "s1.com,s2.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_rewrite_mapping("cdn.com", "myhost1.com,myhost2.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_origin_mapping("localhost", "myhost1.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_origin_mapping("localhost", "myhost2.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert_eq!(
        fixture.map_origin("http://cdn.com/x").as_deref(),
        Some("http://localhost/x")
    );
    assert_eq!(
        fixture.map_origin("http://s1.com/x").as_deref(),
        Some("http://localhost/x")
    );
    assert_eq!(
        fixture.map_origin("http://s2.com/x").as_deref(),
        Some("http://localhost/x")
    );
}

#[test]
fn no_shard_conflict_scramble() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_shard("cdn.com", "s1.com,s2.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_origin_mapping("localhost", "myhost1.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_origin_mapping("localhost", "myhost2.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_rewrite_mapping("cdn.com", "myhost1.com,myhost2.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert_eq!(
        fixture.map_origin("http://cdn.com/x").as_deref(),
        Some("http://localhost/x")
    );
    assert_eq!(
        fixture.map_origin("http://s1.com/x").as_deref(),
        Some("http://localhost/x")
    );
    assert_eq!(
        fixture.map_origin("http://s2.com/x").as_deref(),
        Some("http://localhost/x")
    );
}

#[test]
fn rewrite_origin_cycle() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_shard("b.com", "a.com"));
    assert!(fixture.add_rewrite_mapping("b.com", "a.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_origin_mapping("origin1.com", "a.com"));
    assert_eq!(fixture.handler.serious_messages(), 0);
    assert!(fixture.add_origin_mapping("origin2.com", "b.com"));
    assert_eq!(fixture.handler.serious_messages(), 2);
}

#[test]
fn wildcard_order() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("host1", "abc*.com"));
    assert!(fixture.add_origin_mapping("host2", "*z.com"));
    assert_eq!(
        fixture.map_origin("http://abc.com/x").as_deref(),
        Some("http://host1/x")
    );
    assert_eq!(
        fixture.map_origin("http://z.com/x").as_deref(),
        Some("http://host2/x")
    );
    let mut second = Fixture::new();
    assert!(second.add_origin_mapping("host3", "*abc*.com"));
    assert!(second.add_origin_mapping("host1", "abc*.com"));
    let mut merged = Fixture::new();
    merged.lawyer.merge(&fixture.lawyer);
    merged.lawyer.merge(&second.lawyer);
    assert_eq!(merged.lawyer.num_wildcarded_domains(), 3);
    assert_eq!(
        merged.map_origin("http://abc.com/x").as_deref(),
        Some("http://host1/x")
    );
    assert_eq!(
        merged.map_origin("http://xyz.com/x").as_deref(),
        Some("http://host2/x")
    );
    assert_eq!(
        merged.map_origin("http://xabc.com/x").as_deref(),
        Some("http://host3/x")
    );
}

#[test]
fn compute_signature_with_origins() {
    let mut first = Fixture::new();
    assert!(first.add_origin_mapping("host1", "*abc*.com"));
    assert!(first.lawyer.add_origin_domain_mapping(
        "host2",
        "*def*.com",
        "h2",
        Some(&mut first.handler)
    ));
    assert_eq!(
        first.lawyer.signature(),
        "D:http://*abc*.com/__a_O:http://host1/_-D:http://*def*.com/__a_O:http://host2/_-D:http://host1/__n_-D:http://host2/__n_H:h2|-"
    );
    assert!(first.add_shard("domain1", "shard"));
    assert_eq!(
        first.lawyer.signature(),
        "D:http://*abc*.com/__a_O:http://host1/_-D:http://*def*.com/__a_O:http://host2/_-D:http://domain1/__a_S:http://shard/_-D:http://host1/__n_-D:http://host2/__n_H:h2|-D:http://shard/__a_R:http://domain1/_-"
    );
}

#[test]
fn to_string_with_origins() {
    let mut first = Fixture::new();
    assert!(first.add_domain("static.example.com"));
    assert!(first.add_origin_mapping("host1", "*abc*.com"));
    assert_eq!(
        first.lawyer.to_string_list(),
        "http://*abc*.com/ Auth OriginDomain:http://host1/\nhttp://host1/\nhttp://static.example.com/ Auth\n"
    );
}

#[test]
fn is_origin_known_test() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_domain("a.com"));
    assert!(fixture.add_domain("a.com:42"));
    assert!(fixture.add_domain("https://a.com:43"));
    assert!(fixture.add_rewrite_mapping("b.com", "c.com"));
    assert!(fixture.add_origin_mapping("e.com", "d.com"));
    assert!(fixture.add_shard("f.com", "s1.f.com,s2.f.com"));
    let known = |url: &str| fixture.lawyer.is_origin_known(&GoogleUrl::new(url));
    assert!(!known("http://z.com"));
    assert!(known("http://a.com"));
    assert!(known("http://a.com:42/sardine"));
    assert!(!known("http://a.com:43/bass"));
    assert!(known("https://a.com:43/bass"));
    assert!(!known("https://a.com:44/bass"));
    assert!(known("http://b.com"));
    assert!(known("http://c.com"));
    assert!(known("http://d.com"));
    assert!(known("http://e.com"));
    assert!(known("http://f.com"));
    assert!(known("http://s1.f.com"));
    assert!(known("http://s2.f.com"));
}

#[test]
fn no_absolute_url_path() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("b.com", "a.com"));
    let gurl = GoogleUrl::new("http://a.com/foo");
    let (out, _, is_proxy) = fixture.lawyer.map_origin_url(&gurl).unwrap();
    assert_eq!(out, "http://b.com/foo");
    assert!(!is_proxy);
    let data = GoogleUrl::new("http://a.com/data:image/jpeg");
    let (out, _, is_proxy) = fixture.lawyer.map_origin_url(&data).unwrap();
    assert_eq!(out, "http://b.com/data:image/jpeg");
    assert!(!is_proxy);
}

#[test]
fn about_blank() {
    let mut fixture = Fixture::new();
    assert!(fixture.add_origin_mapping("b.com", "a.com"));
    assert!(fixture
        .lawyer
        .map_origin_url(&GoogleUrl::new("about:blank"))
        .is_none());
}

#[test]
fn strip_proxy_suffix() {
    let fixture = Fixture::new();
    let gurl = GoogleUrl::new("http://example.com.suffix/path");
    assert!(!fixture.lawyer.can_rewrite_domains());
    assert!(fixture.lawyer.strip_proxy_suffix(&gurl).is_none());
    let mut fixture = fixture;
    fixture.lawyer.set_proxy_suffix(".suffix");
    assert!(fixture.lawyer.can_rewrite_domains());
    assert_eq!(
        fixture.lawyer.strip_proxy_suffix(&gurl),
        Some((
            "http://example.com/path".to_string(),
            "example.com".to_string()
        ))
    );
    // `:80` canonicalizes away, so the suffix still matches.
    let gurl_80 = GoogleUrl::new("http://example.com.suffix:80/path");
    assert_eq!(
        fixture.lawyer.strip_proxy_suffix(&gurl_80),
        Some((
            "http://example.com/path".to_string(),
            "example.com".to_string()
        ))
    );
    // Any other port breaks the suffix match.
    assert!(fixture
        .lawyer
        .strip_proxy_suffix(&GoogleUrl::new("http://example.com.suffix:81/path"))
        .is_none());
    assert!(fixture
        .lawyer
        .strip_proxy_suffix(&GoogleUrl::new("http://example.com.suffix:443/path"))
        .is_none());
    assert_eq!(
        fixture
            .lawyer
            .strip_proxy_suffix(&GoogleUrl::new("https://example.com.suffix:443/path")),
        Some((
            "https://example.com/path".to_string(),
            "example.com".to_string()
        ))
    );
    assert_eq!(
        fixture
            .lawyer
            .strip_proxy_suffix(&GoogleUrl::new("https://example.com.suffix/path")),
        Some((
            "https://example.com/path".to_string(),
            "example.com".to_string()
        ))
    );
}

#[test]
fn map_new_url_domain() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_two_protocol_origin_domain_mapping(
        "www.xyz.com",
        "www.foo.com/123/www.xyz.com/",
        "",
        Some(&mut fixture.handler)
    ));
    assert_eq!(
        fixture
            .map_origin("http://www.foo.com/123/www.xyz.com/")
            .as_deref(),
        Some("http://www.xyz.com/")
    );
    assert_eq!(
        fixture
            .map_origin("http://www.foo.com/123/www.xyz.com/a/b")
            .as_deref(),
        Some("http://www.xyz.com/a/b")
    );
    assert_eq!(
        fixture
            .map_origin("https://www.foo.com/123/www.xyz.com/a/b")
            .as_deref(),
        Some("https://www.xyz.com/a/b")
    );
    assert_eq!(
        fixture
            .map_origin("http://www.foo.com/123/www.xyz.com/#fragment")
            .as_deref(),
        Some("http://www.xyz.com/#fragment")
    );
}

#[test]
fn map_new_url_domain_without_domain_suffix() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_two_protocol_origin_domain_mapping(
        "www.baz.com",
        "www.foo.com/www.baz.com/",
        "",
        Some(&mut fixture.handler)
    ));
    assert_eq!(
        fixture
            .map_origin("http://www.foo.com/www.baz.com/bar")
            .as_deref(),
        Some("http://www.baz.com/bar")
    );
}

#[test]
fn map_url_domain_with_leaf() {
    let mut fixture = Fixture::new();
    assert!(fixture.lawyer.add_two_protocol_origin_domain_mapping(
        "www.baz.com",
        "www.foo.com",
        "",
        Some(&mut fixture.handler)
    ));
    assert_eq!(
        fixture.map_origin("http://www.foo.com/bar").as_deref(),
        Some("http://www.baz.com/bar")
    );
}

#[test]
fn origin_wildcard_target_rejected() {
    // The origin-mapping line of AddMappingFailures.
    let mut fixture = Fixture::new();
    assert!(!fixture.lawyer.add_origin_domain_mapping(
        "foo*.com",
        "bar.com",
        "",
        Some(&mut fixture.handler)
    ));
}

#[test]
fn add_proxy_suffix() {
    let mut fixture = Fixture::new();
    let base = GoogleUrl::new("http://www.example.com.suffix");
    fixture.lawyer.set_proxy_suffix(".suffix");
    assert!(fixture.lawyer.can_rewrite_domains());
    assert_eq!(
        fixture.lawyer.add_proxy_suffix(&base, "relative.html"),
        None
    );
    assert_eq!(
        fixture
            .lawyer
            .add_proxy_suffix(&base, "http://www.example.com/absolute.html")
            .as_deref(),
        Some("http://www.example.com.suffix/absolute.html")
    );
    assert_eq!(
        fixture
            .lawyer
            .add_proxy_suffix(&base, "http://other.example.com/absolute.html")
            .as_deref(),
        Some("http://other.example.com.suffix/absolute.html")
    );
    assert_eq!(
        fixture
            .lawyer
            .add_proxy_suffix(&base, "http://other.com/x.html"),
        None
    );
    assert_eq!(
        fixture
            .lawyer
            .add_proxy_suffix(&base, "https://www.example.com/absolute.html")
            .as_deref(),
        Some("https://www.example.com.suffix/absolute.html")
    );
}

#[test]
fn rebuild_lawyer_threads_proxy_mappings_with_cdn_leg() {
    use ps_cache::cache::MemoryCache;
    use ps_config::options::RewriteOptions;
    use ps_html::server_context::{ResourceDriver, ServerContext};
    use ps_http::mock::MockUrlFetcher;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .add_proxy_domain_mapping("http://proxy.com/external", "http://origin.com/static");
    // The third `MapProxyDomain` arg re-bases onto a CDN.
    options
        .borrow_mut()
        .parse_and_set_3(
            "MapProxyDomain",
            "http://proxy2.com/e",
            "http://origin2.com/s",
            "http://cdn2.com/e",
        )
        .expect("3-arg proxy mapping parses");
    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(MemoryCache::new()),
        Rc::new(MockUrlFetcher::new()),
        0,
    ));
    let driver = ResourceDriver::new(server, options, "http://proxy.com/");
    let lawyer = driver.lawyer();
    assert!(lawyer.is_proxy_mapped(&GoogleUrl::new("http://proxy.com/external/a.css")));
    assert!(!lawyer.is_proxy_mapped(&GoogleUrl::new("http://proxy.com/a.css")));
    assert!(lawyer.is_proxy_mapped(&GoogleUrl::new("http://proxy2.com/e/a.css")));
    // The CDN leg maps proxy and CDN paths back to the origin.
    let (mapped, _, is_proxy) = lawyer
        .map_origin_url(&GoogleUrl::new("http://proxy2.com/e/a.css"))
        .expect("proxy maps");
    assert!(is_proxy);
    assert_eq!(mapped, "http://origin2.com/s/a.css");
    let (mapped, _, is_proxy) = lawyer
        .map_origin_url(&GoogleUrl::new("http://cdn2.com/e/a.css"))
        .expect("CDN maps");
    assert!(is_proxy);
    assert_eq!(mapped, "http://origin2.com/s/a.css");
}

#[test]
fn rebuild_lawyer_threads_rewrite_direction() {
    use ps_cache::cache::MemoryCache;
    use ps_config::options::RewriteOptions;
    use ps_html::server_context::{ResourceDriver, ServerContext};
    use ps_http::mock::MockUrlFetcher;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    // C++ order is (to, from): origin.com rewrites onto cdn.com.
    options
        .borrow_mut()
        .parse_and_set_2("MapRewriteDomain", "http://cdn.com/", "http://origin.com/")
        .unwrap();
    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(MemoryCache::new()),
        Rc::new(MockUrlFetcher::new()),
        0,
    ));
    let driver = ResourceDriver::new(server, options, "http://origin.com/");
    let lawyer = driver.lawyer();
    assert!(lawyer.do_domains_serve_same_content("http://origin.com/", "http://cdn.com/"));
    // Directional: origin.com points at cdn.com, not the reverse.
    assert_eq!(
        lawyer.find_domains_rewritten_to(&GoogleUrl::new("http://cdn.com/a.css")),
        vec!["http://origin.com/".to_string()]
    );
    assert!(lawyer
        .find_domains_rewritten_to(&GoogleUrl::new("http://origin.com/a.css"))
        .is_empty());
}

#[test]
fn rebuild_lawyer_threads_origin_mappings() {
    use ps_cache::cache::MemoryCache;
    use ps_config::options::RewriteOptions;
    use ps_html::server_context::{ResourceDriver, ServerContext};
    use ps_http::mock::MockUrlFetcher;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .parse_and_set_2("MapOriginDomain", "http://origin.com/", "http://example.com/")
        .unwrap();
    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(MemoryCache::new()),
        Rc::new(MockUrlFetcher::new()),
        0,
    ));
    let driver = ResourceDriver::new(server, options, "http://example.com/");
    let lawyer = driver.lawyer();
    let (mapped, host, is_proxy) = lawyer
        .map_origin_url(&GoogleUrl::new("http://example.com/a.css"))
        .expect("origin maps");
    assert!(!is_proxy);
    assert_eq!(mapped, "http://origin.com/a.css");
    assert_eq!(host, "example.com");
}
