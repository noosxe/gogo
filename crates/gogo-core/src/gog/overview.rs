//! A product's public store page data: `GET /v2/games/{id}` (design 25 §2).
//!
//! The call takes no credential. It is what a frontend shows about a game: names,
//! dates, genre, description and the links to its pictures. Every field is optional
//! in what is read, so a product missing one still shows the rest.

use serde::Deserialize;

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

#[derive(Deserialize, Default)]
#[serde(default)]
struct Page {
    description: Option<String>,
    #[serde(rename = "_links")]
    links: Links,
    #[serde(rename = "_embedded")]
    embedded: Embedded,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Links {
    store: Option<Link>,
    box_art_image: Option<Link>,
    background_image: Option<Link>,
    galaxy_background_image: Option<Link>,
}

#[derive(Deserialize)]
struct Link {
    href: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Embedded {
    product: Product,
    developers: Vec<Named>,
    publishers: Vec<Named>,
    publisher: Option<Named>,
    tags: Vec<Named>,
    localizations: Vec<Localization>,
    supported_operating_systems: Vec<Supported>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Product {
    title: Option<String>,
    global_release_date: Option<String>,
    gog_release_date: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Named {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Localization {
    #[serde(rename = "_embedded")]
    embedded: LocalizationEmbedded,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LocalizationEmbedded {
    language: Named,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Supported {
    operating_system: Named,
}

fn names(list: Vec<Named>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(list.len());
    for n in list {
        if !n.name.is_empty() && !out.contains(&n.name) {
            out.push(n.name);
        }
    }
    out
}

impl From<Page> for Overview {
    fn from(p: Page) -> Overview {
        let e = p.embedded;
        let publishers = if e.publishers.is_empty() {
            names(e.publisher.into_iter().collect())
        } else {
            names(e.publishers)
        };
        let href = |l: Option<Link>| l.map(|l| l.href);
        let slug = p.links.store.and_then(|l| {
            let path = l.href.split(['?', '#']).next()?.trim_end_matches('/');
            Some(path.rsplit('/').next()?.to_string())
        });
        Overview {
            title: e.product.title,
            release_date: e.product.global_release_date.or(e.product.gog_release_date),
            developers: names(e.developers),
            publishers,
            genres: names(e.tags),
            description: p.description,
            languages: names(
                e.localizations
                    .into_iter()
                    .map(|l| l.embedded.language)
                    .collect(),
            ),
            systems: names(
                e.supported_operating_systems
                    .into_iter()
                    .map(|s| s.operating_system)
                    .collect(),
            ),
            slug,
            cover: href(p.links.box_art_image),
            background: href(p.links.galaxy_background_image)
                .or_else(|| href(p.links.background_image)),
        }
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
    let page: Page = resp.json().await.map_err(ApiError::decode)?;
    Ok(page.into())
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
