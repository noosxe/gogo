//! A product's public store page data: `GET /v2/games/{id}` (design 25 §2).
//!
//! The call takes no credential. It is what a frontend shows about a game: names,
//! dates, genre, description and the links to its pictures. Every field is optional
//! in what is read, so a product missing one still shows the rest.

use serde_json::Value;

use crate::gog::error::ApiError;

/// What the store says about a product. The picture links are the server's text and
/// are not checked here: a caller that fetches them decides which hosts it trusts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overview {
    pub title: Option<String>,
    /// ISO 8601, as the store writes it.
    pub release_date: Option<String>,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    /// The store's genre tags, the main one first.
    pub genres: Vec<String>,
    /// HTML.
    pub description: Option<String>,
    /// English language names, without repeats.
    pub languages: Vec<String>,
    /// `windows`, `osx` and `linux`, as the store writes them.
    pub systems: Vec<String>,
    /// The last segment of the store page's address.
    pub slug: Option<String>,
    pub cover: Option<String>,
    pub background: Option<String>,
}

// Read from untyped JSON, one field at a time: a `null`, a missing key or a value of
// another type loses that field only, never the page.

/// A text with something in it.
fn text(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// The texts `name` finds in the entries of a list, without repeats.
fn names(list: &Value, name: impl Fn(&Value) -> &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in list.as_array().into_iter().flatten() {
        if let Some(text) = text(name(entry))
            && !out.contains(&text)
        {
            out.push(text);
        }
    }
    out
}

fn read(page: &Value) -> Overview {
    let links = &page["_links"];
    let href = |name: &str| text(&links[name]["href"]);
    let embedded = &page["_embedded"];
    let product = &embedded["product"];
    let mut publishers = names(&embedded["publishers"], |p| &p["name"]);
    if publishers.is_empty() {
        publishers.extend(text(&embedded["publisher"]["name"]));
    }
    let slug = href("store").and_then(|address| {
        let path = address.split(['?', '#']).next()?.trim_end_matches('/');
        Some(path.rsplit('/').next()?.to_string())
    });
    Overview {
        title: text(&product["title"]),
        release_date: text(&product["globalReleaseDate"])
            .or_else(|| text(&product["gogReleaseDate"])),
        developers: names(&embedded["developers"], |d| &d["name"]),
        publishers,
        genres: names(&embedded["tags"], |t| &t["name"]),
        description: text(&page["description"]),
        languages: names(&embedded["localizations"], |l| {
            &l["_embedded"]["language"]["name"]
        }),
        systems: names(&embedded["supportedOperatingSystems"], |s| {
            &s["operatingSystem"]["name"]
        }),
        slug,
        cover: href("boxArtImage"),
        background: href("galaxyBackgroundImage").or_else(|| href("backgroundImage")),
    }
}

/// Fetch a product's public page data.
pub(crate) async fn overview_at(
    http: &reqwest::Client,
    product_id: u64,
    base: &str,
) -> Result<Overview, ApiError> {
    let url = format!("{base}/v2/games/{product_id}?locale=en-US");
    let resp = ApiError::check(http.get(url).send().await?).await?;
    let page: Value = resp.json().await.map_err(ApiError::decode)?;
    Ok(read(&page))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header_exists, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A made-up product in the shape the endpoint answers with.
    const PAGE: &str = r#"{
        "description": "<p>A made-up game.</p>",
        "_links": {
            "store": {"href": "https://www.gog.com/en/game/made_up_game"},
            "boxArtImage": {"href": "https://images.gog-statics.com/box.jpg"},
            "backgroundImage": {"href": "https://images.gog-statics.com/back.jpg"}
        },
        "_embedded": {
            "product": {"title": "Made Up", "globalReleaseDate": "2001-02-03T00:00:00+02:00"},
            "developers": [{"name": "Nobody"}],
            "publisher": {"name": "No One"},
            "tags": [{"name": "Puzzle"}, {"name": "Puzzle"}, {"name": "Indie"}],
            "localizations": [
                {"_embedded": {"language": {"name": "English"}}},
                {"_embedded": {"language": {"name": "English"}}}
            ],
            "supportedOperatingSystems": [{"operatingSystem": {"name": "windows"}}]
        }
    }"#;

    #[tokio::test]
    async fn reads_the_page_and_sends_no_credential() {
        let server = MockServer::start().await;
        Mock::given(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(path("/v2/games/7"))
            .respond_with(ResponseTemplate::new(200).set_body_string(PAGE))
            .mount(&server)
            .await;
        let http = crate::gog::http().unwrap();
        let o = overview_at(&http, 7, &server.uri()).await.unwrap();
        assert_eq!(o.title.as_deref(), Some("Made Up"));
        assert_eq!(o.publishers, ["No One"]);
        assert_eq!(o.genres, ["Puzzle", "Indie"]);
        assert_eq!(o.languages, ["English"]);
        assert_eq!(o.systems, ["windows"]);
        assert_eq!(o.slug.as_deref(), Some("made_up_game"));
        assert_eq!(
            o.background.as_deref(),
            Some("https://images.gog-statics.com/back.jpg")
        );
    }

    #[test]
    fn a_bad_field_loses_only_itself() {
        let page = serde_json::json!({
            "description": 7,
            "_links": {
                "store": {"href": null},
                "boxArtImage": {"href": "https://images.gog-statics.com/box.jpg"},
                "backgroundImage": "not a link"
            },
            "_embedded": {
                "product": {"title": "Made Up", "globalReleaseDate": null},
                "developers": [{"name": null}, {"name": "Nobody"}, "x", null],
                "publishers": {"name": "not a list"},
                "publisher": {"name": "No One"},
                "tags": {"name": "an object, not a list"},
                "localizations": "none",
                "supportedOperatingSystems": [{"operatingSystem": null}]
            }
        });
        let o = read(&page);
        assert_eq!(o.title.as_deref(), Some("Made Up"));
        assert_eq!(o.developers, ["Nobody"]);
        assert_eq!(o.publishers, ["No One"]);
        assert!(o.genres.is_empty() && o.languages.is_empty() && o.systems.is_empty());
        assert_eq!((o.description, o.slug, o.background), (None, None, None));
        assert!(o.cover.is_some());
        assert_eq!(read(&serde_json::json!([1])), Overview::default());
    }

    #[tokio::test]
    async fn an_empty_page_is_not_an_error() {
        let server = MockServer::start().await;
        Mock::given(path("/v2/games/7"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let http = crate::gog::http().unwrap();
        let o = overview_at(&http, 7, &server.uri()).await.unwrap();
        assert_eq!(o, Overview::default());
    }
}
