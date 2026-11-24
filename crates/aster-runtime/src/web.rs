//! Fetching and reading web pages. Only ever called for a URL the owner sent
//! or approved. Every hop (including each redirect) is resolved and checked
//! before connecting, and the checked addresses are the only ones used, so a
//! link can never reach the server's own network unless `web.allow_private`.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use aster_core::SourceLink;
use reqwest::{header, redirect::Policy};
use scraper::{ElementRef, Html, Selector};
use url::Url;

use crate::config::WebSection;

/// Most links kept from one page.
pub const MAX_LINKS: usize = 100;
/// Elements whose text is page furniture, not content.
const SKIP: [&str; 9] = [
    "script", "style", "noscript", "nav", "footer", "header", "aside", "form", "template",
];
/// Elements read as one passage each.
const BLOCKS: &str = "p, h1, h2, h3, h4, h5, h6, li, pre, blockquote, dd, dt, td, figcaption";

#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// Where the page ended up, after redirects.
    pub url: String,
    pub title: String,
    /// Passages separated by blank lines.
    pub text: String,
    pub links: Vec<SourceLink>,
    /// The body was longer than `web.max_bytes` and was cut off.
    pub truncated: bool,
}

/// Parse an owner-supplied URL: http or https, a host, no credentials.
pub fn parse_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|e| format!("not a URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https URLs can be read".into());
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("URL has no host".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs with credentials are not read".into());
    }
    Ok(url)
}

/// Whether an address is on the public internet (not loopback, private,
/// link-local, shared, documentation, multicast, or reserved).
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public_v4(v4);
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00 // unique local
                || (first & 0xffc0) == 0xfe80 // link-local
                || (first == 0x2001 && v6.segments()[1] == 0x0db8) // documentation
                || v6.segments()[..6] == [0x64, 0xff9b, 0, 0, 0, 0] // NAT64
                || v6 == Ipv6Addr::from([0u8; 16]))
        }
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // shared (CGNAT)
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 198 && (18..20).contains(&b)) // benchmarking
        || a >= 240) // reserved
}

/// Resolve `url`'s host and refuse it if any address is not public.
async fn checked_addrs(url: &Url, web: &WebSection) -> Result<Vec<SocketAddr>, String> {
    let host = url.host_str().ok_or("URL has no host")?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = url.port_or_known_default().ok_or("URL has no port")?;
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| format!("could not resolve {host}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("could not resolve {host}"));
    }
    if !web.allow_private && addrs.iter().any(|a| !is_public(a.ip())) {
        return Err(format!(
            "{host} is a private or local address (set web.allow_private to read it)"
        ));
    }
    Ok(addrs)
}

/// Fetch and read a page, following up to `web.max_redirects` redirects.
pub async fn fetch(raw: &str, web: &WebSection) -> Result<Page, String> {
    if !web.enabled {
        return Err("reading web pages is turned off (web.enabled)".into());
    }
    let timeout = Duration::from_secs(web.timeout_secs);
    tokio::time::timeout(timeout, fetch_inner(raw, web, timeout))
        .await
        .map_err(|_| format!("timed out after {}s", web.timeout_secs))?
}

async fn fetch_inner(raw: &str, web: &WebSection, timeout: Duration) -> Result<Page, String> {
    let mut url = parse_url(raw)?;
    for _ in 0..=web.max_redirects {
        let addrs = checked_addrs(&url, web).await?;
        let host = url.host_str().unwrap_or_default().to_string();
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .timeout(timeout)
            .user_agent(concat!("aster/", env!("CARGO_PKG_VERSION")))
            // Connect only to the addresses checked above.
            .resolve_to_addrs(&host, &addrs)
            .build()
            .map_err(|e| format!("could not start a client: {e}"))?;
        let mut response = client
            .get(url.clone())
            .header(
                header::ACCEPT,
                "text/html,application/xhtml+xml,text/plain;q=0.9,text/markdown;q=0.9",
            )
            .send()
            .await
            .map_err(|e| format!("could not fetch {host}: {}", without_url(&e)))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| format!("{status} without a location"))?;
            let next = url
                .join(location)
                .map_err(|_| "redirected to an invalid URL".to_string())?;
            url = parse_url(next.as_str())?;
            continue;
        }
        if !status.is_success() {
            return Err(format!("{host} answered {status}"));
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/html")
            .to_ascii_lowercase();
        let html = content_type.contains("html");
        if !html && !content_type.starts_with("text/") {
            return Err(format!("{content_type} is not text Aster can read"));
        }
        let mut body = Vec::new();
        let mut truncated = false;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("reading {host} failed: {}", without_url(&e)))?
        {
            let room = web.max_bytes - body.len();
            if chunk.len() > room {
                body.extend_from_slice(&chunk[..room]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }
        let text = String::from_utf8_lossy(&body);
        let mut page = if html {
            read_html(&text, Some(&url))
        } else {
            read_plain(&text)
        };
        page.url = url.to_string();
        page.truncated = truncated;
        if page.title.is_empty() {
            page.title = host;
        }
        return Ok(page);
    }
    Err(format!("more than {} redirects", web.max_redirects))
}

fn without_url(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    if let Some(url) = error.url() {
        message = message.replace(&format!(" ({url})"), "");
    }
    message
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn skipped(element: &ElementRef) -> bool {
    element
        .ancestors()
        .filter_map(ElementRef::wrap)
        .any(|a| SKIP.contains(&a.value().name()))
}

/// Title, content passages, and outgoing links of an HTML document. Relative
/// links need `base`; without one only absolute links are kept.
pub fn read_html(html: &str, base: Option<&Url>) -> Page {
    let document = Html::parse_document(html);
    let select = |css: &str| Selector::parse(css).expect("static selector");
    let title = document
        .select(&select("title"))
        .chain(document.select(&select("h1")))
        .map(|t| collapse(&t.text().collect::<String>()))
        .find(|t| !t.is_empty())
        .unwrap_or_default();

    let blocks = select(BLOCKS);
    let block_names: Vec<&str> = BLOCKS.split(", ").collect();
    let mut passages: Vec<String> = Vec::new();
    for element in document.select(&blocks) {
        // A block inside another block is read as part of the outer one.
        let nested = element
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|a| block_names.contains(&a.value().name()));
        if nested || skipped(&element) {
            continue;
        }
        let text = collapse(&element.text().collect::<String>());
        if text.split(' ').count() >= 3 && passages.last() != Some(&text) {
            passages.push(text);
        }
    }
    if passages.is_empty() {
        // No paragraphs (a bare page): fall back to the body's lines.
        if let Some(body) = document.select(&select("body")).next() {
            passages = body
                .text()
                .map(collapse)
                .filter(|l| l.split(' ').count() >= 3)
                .collect();
        }
    }

    let mut links: Vec<SourceLink> = Vec::new();
    for anchor in document.select(&select("a[href]")) {
        if links.len() >= MAX_LINKS {
            break;
        }
        if skipped(&anchor) {
            continue;
        }
        let href = anchor.value().attr("href").unwrap_or_default();
        let resolved = match base {
            Some(base) => base.join(href).ok(),
            None => Url::parse(href).ok(),
        };
        let Some(mut url) = resolved else { continue };
        if !matches!(url.scheme(), "http" | "https") {
            continue;
        }
        url.set_fragment(None);
        if base.is_some_and(|b| {
            let mut b = b.clone();
            b.set_fragment(None);
            b == url
        }) {
            continue;
        }
        let text = collapse(&anchor.text().collect::<String>());
        let url = url.to_string();
        if text.is_empty() || links.iter().any(|l| l.url == url) {
            continue;
        }
        links.push(SourceLink { url, text });
    }

    Page {
        url: base.map(Url::to_string).unwrap_or_default(),
        title,
        text: passages.join("\n\n"),
        links,
        truncated: false,
    }
}

/// Plain text or Markdown: the first line is the title, the rest is as-is.
pub fn read_plain(text: &str) -> Page {
    let title = text
        .lines()
        .map(|l| collapse(l.trim_start_matches('#')))
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    Page {
        url: String::new(),
        title,
        text: text.replace("\r\n", "\n").trim().to_string(),
        links: Vec::new(),
        truncated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_pass() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in ["93.184.216.34", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn urls_must_be_web_urls() {
        assert!(parse_url("https://example.org/a").is_ok());
        assert!(parse_url("file:///etc/passwd").is_err());
        assert!(parse_url("ftp://example.org").is_err());
        assert!(parse_url("https://user:pw@example.org").is_err());
        assert!(parse_url("not a url").is_err());
    }

    #[test]
    fn html_gives_title_passages_and_links() {
        let html = r##"<html><head><title> Nebulae </title><style>p{}</style></head>
            <body><nav><a href="/home">Home page link</a><p>skip this menu text</p></nav>
            <h1>Nebula</h1>
            <p>A nebula is a giant cloud of dust and gas in space.</p>
            <ul><li><p>Some are made of <a href="/wiki/Supernova#x">a dying star</a> exploding.</p></li></ul>
            <script>var x = "not text at all here";</script>
            <p>Read about <a href="https://example.org/stars">how stars form</a> next.</p>
            <a href="mailto:a@b.c">mail me now</a><a href="#top">top of page</a>
            <footer><p>copyright footer words here</p></footer></body></html>"##;
        let base = Url::parse("https://wiki.test/wiki/Nebula").unwrap();
        let page = read_html(html, Some(&base));
        assert_eq!(page.title, "Nebulae");
        let passages: Vec<&str> = page.text.split("\n\n").collect();
        assert_eq!(
            passages,
            [
                "A nebula is a giant cloud of dust and gas in space.",
                "Some are made of a dying star exploding.",
                "Read about how stars form next.",
            ]
        );
        assert_eq!(
            page.links,
            [
                SourceLink {
                    url: "https://wiki.test/wiki/Supernova".into(),
                    text: "a dying star".into()
                },
                SourceLink {
                    url: "https://example.org/stars".into(),
                    text: "how stars form".into()
                },
            ]
        );
    }

    #[test]
    fn plain_text_takes_its_first_line_as_title() {
        let page = read_plain("# My notes\r\n\r\nRust is my main language.\n");
        assert_eq!(page.title, "My notes");
        assert!(page.text.ends_with("Rust is my main language."));
    }

    #[tokio::test]
    async fn local_addresses_are_refused_unless_allowed() {
        let web = WebSection::default();
        let error = fetch("http://127.0.0.1:9/", &web).await.unwrap_err();
        assert!(error.contains("private or local"), "{error}");
        let error = fetch("http://localhost:9/", &web).await.unwrap_err();
        assert!(error.contains("private or local"), "{error}");
        let off = WebSection {
            enabled: false,
            ..WebSection::default()
        };
        assert!(fetch("https://example.org", &off).await.is_err());
    }

    /// A tiny one-shot HTTP server on loopback that answers with `response`.
    async fn serve(responses: Vec<String>) -> SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        addr
    }

    fn ok(content_type: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn fetches_follow_redirects_and_cap_size() {
        let allowed = WebSection {
            allow_private: true,
            max_bytes: 1024,
            ..WebSection::default()
        };
        let long = format!("<p>{}</p>", "word ".repeat(1000));
        let addr = serve(vec![
            "HTTP/1.1 302 Found\r\nlocation: /page\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
            ok("text/html; charset=utf-8", &long),
        ])
        .await;
        let page = fetch(&format!("http://{addr}/start"), &allowed)
            .await
            .unwrap();
        assert_eq!(page.url, format!("http://{addr}/page"));
        assert!(page.truncated);
        assert!(page.text.len() <= 1024);

        let addr = serve(vec![ok("image/png", "x")]).await;
        let error = fetch(&format!("http://{addr}/"), &allowed)
            .await
            .unwrap_err();
        assert!(error.contains("not text"), "{error}");
    }

    #[tokio::test]
    async fn every_hop_is_checked_including_redirect_targets() {
        // fetch() runs this check on each hop, so a public page that redirects
        // into the local network is refused at the redirect.
        let web = WebSection::default();
        for inward in [
            "http://10.0.0.1/admin",
            "http://[::1]:8080/",
            "http://169.254.169.254/",
        ] {
            let url = parse_url(inward).unwrap();
            let error = checked_addrs(&url, &web).await.unwrap_err();
            assert!(error.contains("private or local"), "{inward}: {error}");
        }
        let addr = serve(vec![
            "HTTP/1.1 302 Found\r\nlocation: http://localhost:9/admin\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
        ])
        .await;
        let error = fetch(&format!("http://{addr}/"), &web).await.unwrap_err();
        assert!(error.contains("private or local"), "{error}");
    }
}
