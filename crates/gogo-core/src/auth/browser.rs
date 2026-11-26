//! The browser paste-code flow's two ends (design 02 §2), shared by every frontend:
//! the address to open, and what the user pastes back.

use zeroize::{Zeroize as _, Zeroizing};

/// Where GOG sends the browser after sign-in, with the code in its query. gog-warp's
/// token exchange sends this value verbatim.
pub const REDIRECT_URI: &str = "https://embed.gog.com/on_login_success?origin=client";

/// The longest pasted text looked at.
const MAX_INPUT: usize = 4096;

/// The sign-in page. It holds GOG Galaxy's public client id and no secret.
pub fn authorize_url() -> String {
    format!(
        "https://auth.gog.com/auth?client_id={}&redirect_uri={REDIRECT_URI}&response_type=code&layout=client2",
        gog_warp::constants::GALAXY_CLIENT_ID,
    )
}

/// The sign-in code in what the user pasted: either the whole address of the page the
/// sign-in ends on, or the bare code. `None` for anything else.
pub fn code_from_input(input: &str) -> Option<Zeroizing<String>> {
    let input = input.trim();
    if input.is_empty() || input.len() > MAX_INPUT {
        return None;
    }
    if input.contains("://") {
        let url = reqwest::Url::parse(input).ok()?;
        let code = redirect_code(&url);
        // The parsed address holds the code too.
        String::from(url).zeroize();
        return code.and_then(plain);
    }
    plain(Zeroizing::new(input.to_string()))
}

/// The `code` of the address the sign-in ends on.
fn redirect_code(url: &reqwest::Url) -> Option<Zeroizing<String>> {
    if url.scheme() != "https"
        || url.host_str() != Some("embed.gog.com")
        || url.path() != "/on_login_success"
    {
        return None;
    }
    let mut pairs = url.query_pairs();
    let code = pairs.find(|(name, _)| name == "code")?.1;
    // A decoded value is taken over as it is, a borrowed one is copied once.
    Some(Zeroizing::new(code.into_owned()))
}

/// A code is one word of characters that need no escaping in a query.
fn plain(code: Zeroizing<String>) -> Option<Zeroizing<String>> {
    let ok = !code.is_empty()
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.~".contains(&b));
    ok.then_some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(input: &str) -> Option<String> {
        code_from_input(input).map(|c| c.to_string())
    }

    #[test]
    fn a_redirect_address_or_a_bare_code_is_read() {
        let url = "https://embed.gog.com/on_login_success?origin=client&code=FAKE-Code_1.~";
        assert_eq!(read(url).as_deref(), Some("FAKE-Code_1.~"));
        // What follows the code is not part of it.
        let more = "https://embed.gog.com/on_login_success?code=abc&state=x#frag\n";
        assert_eq!(read(more).as_deref(), Some("abc"));
        assert_eq!(read("  FAKE-Code_1  ").as_deref(), Some("FAKE-Code_1"));
    }

    #[test]
    fn anything_else_is_not() {
        for no in [
            "",
            "two words",
            "code=abc",
            "http://embed.gog.com/on_login_success?code=abc",
            "https://embed.gog.com.example.com/on_login_success?code=abc",
            "https://example.com/on_login_success?code=abc",
            "https://embed.gog.com/other?code=abc",
            "https://embed.gog.com/on_login_success?origin=client",
            "https://embed.gog.com/on_login_success?code=",
            "https://embed.gog.com/on_login_success?code=a%20b",
        ] {
            assert_eq!(read(no), None, "{no:?}");
        }
        assert_eq!(read(&"a".repeat(MAX_INPUT + 1)), None);
    }

    #[test]
    fn the_page_to_open_names_the_redirect() {
        let url = authorize_url();
        assert!(
            url.starts_with("https://auth.gog.com/auth?client_id="),
            "{url}"
        );
        assert!(url.contains(REDIRECT_URI), "{url}");
        assert!(!url.contains("secret"), "{url}");
    }
}
