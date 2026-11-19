//! Browser sessions for the dashboard. The owner signs in once with the owner
//! token; the browser then holds only a random session id in an HttpOnly,
//! SameSite=Strict cookie. Sessions live in memory (a restart signs out),
//! store only a digest of the id, expire on idle and absolute limits, and
//! cookie-authenticated writes also need a per-session CSRF token.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use aster_runtime::DashboardSection;
use axum::http::{HeaderMap, HeaderValue, Method, header};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub const COOKIE: &str = "aster_session";
pub const CSRF_HEADER: &str = "x-aster-csrf";
const MAX_SESSIONS: usize = 32;
const MAX_LOCKOUT: Duration = Duration::from_secs(15 * 60);

type Settings = Arc<dyn Fn() -> DashboardSection + Send + Sync>;

#[derive(Clone)]
pub struct Sessions {
    inner: Arc<Mutex<Inner>>,
    settings: Settings,
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<[u8; 32], Session>,
    failures: u32,
    locked_until: Option<Instant>,
}

struct Session {
    csrf: String,
    created: Instant,
    seen: Instant,
}

/// A fresh sign-in: the cookie value (once, for `Set-Cookie`) and its CSRF token.
pub struct Issued {
    pub cookie: String,
    pub csrf: String,
    pub max_age: Duration,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No session cookie at all (fall back to bearer).
    Absent,
    Valid,
    /// Unknown, expired, or a write without a matching CSRF token / origin.
    Rejected,
}

/// 256 random bits from the OS generator, as 64 hex characters.
fn random_hex() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

impl Sessions {
    pub fn new(settings: impl Fn() -> DashboardSection + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            settings: Arc::new(settings),
        }
    }

    pub fn settings(&self) -> DashboardSection {
        (self.settings)()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Time left before another sign-in may be tried, if locked out.
    pub fn locked_for(&self) -> Option<Duration> {
        let inner = self.lock();
        inner
            .locked_until
            .and_then(|until| until.checked_duration_since(Instant::now()))
            .filter(|left| !left.is_zero())
    }

    /// Record a failed sign-in. After `login_max_failures`, each failure
    /// locks sign-in for 2^(extra failures) seconds, capped at 15 minutes.
    pub fn failed(&self) {
        let max = self.settings().login_max_failures;
        let mut inner = self.lock();
        inner.failures = inner.failures.saturating_add(1);
        if inner.failures >= max {
            let extra = (inner.failures - max).min(20);
            let wait = Duration::from_secs(1 << extra).min(MAX_LOCKOUT);
            inner.locked_until = Some(Instant::now() + wait);
        }
    }

    pub fn issue(&self) -> Issued {
        let settings = self.settings();
        let cookie = random_hex();
        let csrf = random_hex();
        let now = Instant::now();
        let mut inner = self.lock();
        inner.failures = 0;
        inner.locked_until = None;
        Self::prune(&mut inner, &settings, now);
        if inner.sessions.len() >= MAX_SESSIONS
            && let Some(oldest) = inner
                .sessions
                .iter()
                .min_by_key(|(_, s)| s.created)
                .map(|(k, _)| *k)
        {
            inner.sessions.remove(&oldest);
        }
        inner.sessions.insert(
            digest(&cookie),
            Session {
                csrf: csrf.clone(),
                created: now,
                seen: now,
            },
        );
        Issued {
            cookie,
            csrf,
            max_age: Duration::from_secs(settings.session_max_hours * 3600),
        }
    }

    fn prune(inner: &mut Inner, settings: &DashboardSection, now: Instant) {
        let idle = Duration::from_secs(settings.session_idle_minutes * 60);
        let max = Duration::from_secs(settings.session_max_hours * 3600);
        inner.sessions.retain(|_, s| {
            now.duration_since(s.seen) <= idle && now.duration_since(s.created) <= max
        });
    }

    /// The CSRF token of a live session (and refresh its idle timer).
    pub fn csrf(&self, headers: &HeaderMap) -> Option<String> {
        let id = cookie_value(headers)?;
        let settings = self.settings();
        let now = Instant::now();
        let mut inner = self.lock();
        Self::prune(&mut inner, &settings, now);
        let session = inner.sessions.get_mut(&digest(&id))?;
        session.seen = now;
        Some(session.csrf.clone())
    }

    /// Check a request's session cookie. Writes (anything but GET/HEAD) also
    /// need the CSRF header and, when the browser sends one, a same-host Origin.
    pub fn verify(&self, method: &Method, headers: &HeaderMap) -> Verdict {
        if cookie_value(headers).is_none() {
            return Verdict::Absent;
        }
        let Some(csrf) = self.csrf(headers) else {
            return Verdict::Rejected;
        };
        if method == Method::GET || method == Method::HEAD {
            return Verdict::Valid;
        }
        let sent = headers
            .get(CSRF_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !bool::from(digest(sent).ct_eq(&digest(&csrf))) || !same_origin(headers) {
            return Verdict::Rejected;
        }
        Verdict::Valid
    }

    pub fn revoke(&self, headers: &HeaderMap) {
        if let Some(id) = cookie_value(headers) {
            self.lock().sessions.remove(&digest(&id));
        }
    }

    pub fn set_cookie(&self, issued: &Issued) -> HeaderValue {
        let secure = if self.settings().cookie_secure {
            "; Secure"
        } else {
            ""
        };
        HeaderValue::from_str(&format!(
            "{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{secure}",
            issued.cookie,
            issued.max_age.as_secs()
        ))
        .expect("hex cookie is a valid header")
    }

    pub fn clear_cookie(&self) -> HeaderValue {
        let secure = if self.settings().cookie_secure {
            "; Secure"
        } else {
            ""
        };
        HeaderValue::from_str(&format!(
            "{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure}"
        ))
        .expect("static cookie is a valid header")
    }
}

/// The session id from the `Cookie` header(s), if well-formed.
fn cookie_value(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_string())
        .filter(|value| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// An `Origin` header, when present, must name the same host the request was
/// sent to (the proxy forwards the public `Host`).
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let authority = origin
        .split_once("://")
        .map(|(_, rest)| rest.trim_end_matches('/'));
    matches!((authority, host), (Some(a), Some(h)) if a.eq_ignore_ascii_case(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions() -> Sessions {
        Sessions::new(DashboardSection::default)
    }

    fn with_cookie(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("theme=dark; {COOKIE}={value}")).unwrap(),
        );
        headers.insert(header::HOST, HeaderValue::from_static("aster.example.com"));
        headers
    }

    #[test]
    fn reads_need_a_live_cookie_and_writes_need_csrf_and_origin() {
        let sessions = sessions();
        let issued = sessions.issue();
        let mut headers = with_cookie(&issued.cookie);
        assert_eq!(sessions.verify(&Method::GET, &headers), Verdict::Valid);
        assert_eq!(sessions.verify(&Method::POST, &headers), Verdict::Rejected);
        headers.insert(CSRF_HEADER, HeaderValue::from_str(&issued.csrf).unwrap());
        assert_eq!(sessions.verify(&Method::POST, &headers), Verdict::Valid);
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert_eq!(sessions.verify(&Method::POST, &headers), Verdict::Rejected);
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://aster.example.com"),
        );
        assert_eq!(sessions.verify(&Method::PATCH, &headers), Verdict::Valid);
    }

    #[test]
    fn unknown_revoked_or_malformed_cookies_are_rejected_or_absent() {
        let sessions = sessions();
        assert_eq!(
            sessions.verify(&Method::GET, &HeaderMap::new()),
            Verdict::Absent
        );
        assert_eq!(
            sessions.verify(&Method::GET, &with_cookie("not-hex")),
            Verdict::Absent
        );
        assert_eq!(
            sessions.verify(&Method::GET, &with_cookie(&"a".repeat(64))),
            Verdict::Rejected
        );
        let issued = sessions.issue();
        let headers = with_cookie(&issued.cookie);
        sessions.revoke(&headers);
        assert_eq!(sessions.verify(&Method::GET, &headers), Verdict::Rejected);
    }

    #[test]
    fn failures_lock_sign_in_with_growing_waits() {
        let sessions = Sessions::new(|| DashboardSection {
            login_max_failures: 2,
            ..Default::default()
        });
        sessions.failed();
        assert!(sessions.locked_for().is_none());
        sessions.failed();
        let first = sessions.locked_for().unwrap();
        sessions.failed();
        assert!(sessions.locked_for().unwrap() > first);
        sessions.issue();
        assert!(sessions.locked_for().is_none());
    }

    #[test]
    fn cookies_are_http_only_strict_and_secure_by_default() {
        let sessions = sessions();
        let issued = sessions.issue();
        let cookie = sessions.set_cookie(&issued);
        let cookie = cookie.to_str().unwrap();
        for part in ["HttpOnly", "SameSite=Strict", "Secure", "Path=/"] {
            assert!(cookie.contains(part), "{cookie}");
        }
        assert_ne!(issued.cookie, issued.csrf);
        assert_eq!(issued.cookie.len(), 64);
        let insecure = Sessions::new(|| DashboardSection {
            cookie_secure: false,
            ..Default::default()
        });
        assert!(
            !insecure
                .set_cookie(&insecure.issue())
                .to_str()
                .unwrap()
                .contains("Secure")
        );
    }
}
