use std::sync::LazyLock;
use std::time::Duration;

/// Retries after the first attempt fails (so up to 4 attempts in total).
pub const MAX_RETRIES: u32 = 3;

/// Errors worth another try: rate limiting, server trouble, and network hiccups.
pub fn is_transient(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::StatusCode(code) => matches!(code, 429 | 500 | 502 | 503 | 504),
        ureq::Error::Timeout(_) | ureq::Error::Io(_) | ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => true,
        _ => false,
    }
}

/// Calls `f`, retrying transient failures up to `MAX_RETRIES` times with 2 s, 4 s, 6 s pauses.
/// `sleep` is injected so tests don't wait.
pub fn with_retry<T>(mut f: impl FnMut() -> Result<T, ureq::Error>, sleep: impl Fn(Duration)) -> Result<T, ureq::Error> {
    let mut retry = 0;
    loop {
        match f() {
            Err(e) if retry < MAX_RETRIES && is_transient(&e) => {
                retry += 1;
                sleep(Duration::from_secs(2 * retry as u64));
            }
            other => return other,
        }
    }
}

static USER_AGENT: LazyLock<String> = LazyLock::new(|| {
    let contact = option_env!("HECKTUNES_CONTACT").map(str::trim).filter(|c| !c.is_empty()).unwrap_or("hecktunes-app");
    format!("HeckTunes/{} ( {contact} )", env!("CARGO_PKG_VERSION"))
});

/// `HeckTunes/<version> ( <contact> )`; contact comes from the build-time `HECKTUNES_CONTACT`.
pub fn user_agent() -> &'static str {
    &USER_AGENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn transient_errors_are_retried_with_growing_pauses() {
        let calls = RefCell::new(0);
        let slept = RefCell::new(Vec::new());
        let result = with_retry(
            || {
                *calls.borrow_mut() += 1;
                if *calls.borrow() < 3 { Err(ureq::Error::StatusCode(503)) } else { Ok(7) }
            },
            |d| slept.borrow_mut().push(d.as_secs()),
        );
        assert_eq!(result.unwrap(), 7);
        assert_eq!(*calls.borrow(), 3);
        assert_eq!(*slept.borrow(), vec![2, 4]);
    }

    #[test]
    fn gives_up_after_three_retries() {
        let calls = RefCell::new(0);
        let result: Result<(), _> = with_retry(
            || {
                *calls.borrow_mut() += 1;
                Err(ureq::Error::ConnectionFailed)
            },
            |_| {},
        );
        assert!(matches!(result, Err(ureq::Error::ConnectionFailed)));
        assert_eq!(*calls.borrow(), 1 + MAX_RETRIES);
    }

    #[test]
    fn permanent_errors_fail_at_once() {
        let calls = RefCell::new(0);
        let result: Result<(), _> = with_retry(
            || {
                *calls.borrow_mut() += 1;
                Err(ureq::Error::StatusCode(404))
            },
            |_| panic!("must not sleep"),
        );
        assert!(matches!(result, Err(ureq::Error::StatusCode(404))));
        assert_eq!(*calls.borrow(), 1);
    }

    #[test]
    fn classifies_errors() {
        for code in [429, 500, 502, 503, 504] {
            assert!(is_transient(&ureq::Error::StatusCode(code)), "{code}");
        }
        for code in [400, 401, 404] {
            assert!(!is_transient(&ureq::Error::StatusCode(code)), "{code}");
        }
        assert!(is_transient(&ureq::Error::HostNotFound));
        assert!(!is_transient(&ureq::Error::TooManyRedirects));
    }

    #[test]
    fn user_agent_names_the_app_and_version() {
        let ua = user_agent();
        assert!(ua.starts_with(&format!("HeckTunes/{} ( ", env!("CARGO_PKG_VERSION"))), "{ua}");
        assert!(ua.ends_with(" )"), "{ua}");
    }
}
