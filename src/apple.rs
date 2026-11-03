use std::fmt;

use serde_json::{Map, Value};
use url::Url;

const MAX_IMAGE_DIMENSION: u64 = 10_000;
const MAX_FILE_STEM_BYTES: usize = 180;
const CDN_DOMAIN_SUFFIX: &str = ".mzstatic.com";
const SERIALIZED_DATA_MARKERS: [&str; 2] = [
    "id=\"serialized-server-data\"",
    "id='serialized-server-data'",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageError {
    InsecureUrl,
    NotAppleMusicUrl,
    MissingSerializedData,
    InvalidSerializedData,
    MissingAlbumHeader,
    MissingArtwork,
    MissingMetadata,
    InvalidImageUrl,
}

impl fmt::Display for PageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InsecureUrl => "Apple Music URLs must use https",
            Self::NotAppleMusicUrl => "not an Apple Music URL",
            Self::MissingSerializedData | Self::InvalidSerializedData => {
                "could not find album data on the page"
            }
            Self::MissingAlbumHeader | Self::MissingArtwork => {
                "could not find cover artwork on the page"
            }
            Self::MissingMetadata => "could not find the album title or artist on the page",
            Self::InvalidImageUrl => "cover artwork is not served from Apple's image CDN",
        };

        formatter.write_str(message)
    }
}

impl std::error::Error for PageError {}

#[derive(Debug, Clone)]
pub(crate) struct AppleMusicUrl(Url);

impl AppleMusicUrl {
    pub(crate) fn parse(raw: &str) -> Result<Self, PageError> {
        let url = Url::parse(raw).map_err(|_| PageError::NotAppleMusicUrl)?;

        if url.scheme() != "https" {
            return Err(PageError::InsecureUrl);
        }

        let host = url.host_str().ok_or(PageError::NotAppleMusicUrl)?;
        let valid_host = host.eq_ignore_ascii_case("music.apple.com")
            || host.eq_ignore_ascii_case("www.music.apple.com");

        if !valid_host
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || !valid_page_path(url.path())
        {
            return Err(PageError::NotAppleMusicUrl);
        }

        Ok(Self(url))
    }

    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

fn valid_page_path(path: &str) -> bool {
    let Some(path) = path.strip_prefix('/') else {
        return false;
    };
    let path = path.strip_suffix('/').unwrap_or(path);
    let mut segments = path.split('/');

    let Some(storefront) = segments.next() else {
        return false;
    };
    if storefront.len() != 2
        || !storefront
            .chars()
            .all(|character| character.is_ascii_alphabetic())
    {
        return false;
    }

    let Some(page_type) = segments.next() else {
        return false;
    };
    let numeric_identifier = match page_type {
        "album" | "song" => true,
        "playlist" => false,
        _ => return false,
    };

    let Some(first_item) = segments.next() else {
        return false;
    };
    if first_item.is_empty() {
        return false;
    }

    let identifier = if let Some(second_item) = segments.next() {
        if second_item.is_empty() || segments.next().is_some() {
            return false;
        }
        second_item
    } else {
        first_item
    };

    if numeric_identifier {
        identifier
            .chars()
            .all(|character| character.is_ascii_digit())
    } else {
        identifier.starts_with("pl.") && identifier.len() > 3
    }
}

#[derive(Debug)]
pub(crate) struct Cover {
    pub(crate) image_url: Url,
    pub(crate) file_name: String,
}

struct Artwork<'a> {
    template: &'a str,
    width: Option<u64>,
    height: Option<u64>,
}

pub(crate) fn parse_cover(html: &str) -> Result<Cover, PageError> {
    let serialized = find_serialized_data(html).ok_or(PageError::MissingSerializedData)?;
    let document: Value =
        serde_json::from_str(serialized).map_err(|_| PageError::InvalidSerializedData)?;
    let header = find_header(&document).ok_or(PageError::MissingAlbumHeader)?;
    let artwork = header_candidates(header)
        .find_map(artwork_from)
        .ok_or(PageError::MissingArtwork)?;
    let (title, artist) = header_candidates(header)
        .find_map(|object| {
            Some((
                string_field(object, "title")?,
                first_subtitle_title(object)?,
            ))
        })
        .ok_or(PageError::MissingMetadata)?;

    validate_image_url(artwork.template)?;
    let format = source_format(artwork.template);
    let image_url = build_image_url(artwork.template, artwork.width, artwork.height, format)?;
    let file_name = build_file_name(title, artist, format);

    Ok(Cover {
        image_url,
        file_name,
    })
}

fn find_serialized_data(html: &str) -> Option<&str> {
    let mut search_start = 0;

    while let Some(relative) = html[search_start..].find("<script") {
        let script_start = search_start + relative;
        let attributes_start = script_start + "<script".len();
        let Some(tag_end) = html[attributes_start..].find('>') else {
            break;
        };
        let tag = &html[attributes_start..attributes_start + tag_end];
        let content_start = attributes_start + tag_end + 1;
        let Some(close) = html[content_start..].find("</script") else {
            break;
        };
        let content_end = content_start + close;

        if tag
            .split_ascii_whitespace()
            .any(|attribute| SERIALIZED_DATA_MARKERS.contains(&attribute))
        {
            return Some(&html[content_start..content_end]);
        }

        search_start = content_end + "</script".len();
    }

    None
}

fn find_header(value: &Value) -> Option<&Map<String, Value>> {
    match value {
        Value::Object(object) => {
            if object.get("itemKind").and_then(Value::as_str) == Some("containerDetailHeaderLockup")
            {
                return Some(object);
            }

            object.values().find_map(find_header)
        }
        Value::Array(items) => items.iter().find_map(find_header),
        _ => None,
    }
}

fn header_candidates(header: &Map<String, Value>) -> impl Iterator<Item = &Map<String, Value>> {
    let item = header
        .get("items")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_object);

    item.into_iter().chain(std::iter::once(header))
}

fn artwork_from(object: &Map<String, Value>) -> Option<Artwork<'_>> {
    let artwork = object.get("artwork")?.as_object()?;
    let dictionary = artwork
        .get("dictionary")
        .and_then(Value::as_object)
        .unwrap_or(artwork);

    Some(Artwork {
        template: dictionary.get("url")?.as_str()?,
        width: dimension_field(dictionary, "width"),
        height: dimension_field(dictionary, "height"),
    })
}

fn string_field<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key)?.as_str()
}

fn dimension_field(object: &Map<String, Value>, key: &str) -> Option<u64> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
}

fn first_subtitle_title(header: &Map<String, Value>) -> Option<&str> {
    header
        .get("subtitleLinks")?
        .as_array()?
        .first()?
        .as_object()?
        .get("title")?
        .as_str()
}

fn validate_image_url(raw: &str) -> Result<Url, PageError> {
    let url = Url::parse(raw).map_err(|_| PageError::InvalidImageUrl)?;
    let host = url
        .host_str()
        .ok_or(PageError::InvalidImageUrl)?
        .to_ascii_lowercase();
    let valid_cdn_host = host.ends_with(CDN_DOMAIN_SUFFIX) && host.len() > CDN_DOMAIN_SUFFIX.len();

    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || !valid_cdn_host
    {
        return Err(PageError::InvalidImageUrl);
    }

    Ok(url)
}

fn source_format(template: &str) -> &'static str {
    let path = template.split(['?', '#']).next().unwrap_or(template);
    let before_dimensions = path.split("{w}").next().unwrap_or(path);
    let source_name = before_dimensions
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let extension = source_name
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or_default();

    if extension.eq_ignore_ascii_case("png") {
        "png"
    } else {
        "jpg"
    }
}

fn request_dimension(dimension: Option<u64>) -> u64 {
    dimension.map_or(MAX_IMAGE_DIMENSION, |value| value.min(MAX_IMAGE_DIMENSION))
}

fn build_image_url(
    template: &str,
    width: Option<u64>,
    height: Option<u64>,
    format: &str,
) -> Result<Url, PageError> {
    let width = request_dimension(width).to_string();
    let height = request_dimension(height).to_string();
    let expanded = template
        .replace("{w}", &width)
        .replace("{h}", &height)
        .replace("{f}", format);

    validate_image_url(&expanded)
}

fn build_file_name(title: &str, artist: &str, format: &str) -> String {
    let artist = sanitize_component(artist);
    let title = sanitize_component(title);
    let mut stem = match (artist.is_empty(), title.is_empty()) {
        (false, false) => format!("{artist} - {title}"),
        (false, true) => artist,
        (true, false) => title,
        (true, true) => String::from("cover"),
    };

    truncate_to_char_boundary(&mut stem, MAX_FILE_STEM_BYTES);

    if stem.starts_with('.') {
        stem.replace_range(..1, "\u{ff0e}");
    }

    format!("{stem}.{format}")
}

fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if is_illegal_file_character(character) {
                '_'
            } else {
                character
            }
        })
        .collect()
}

fn is_illegal_file_character(character: char) -> bool {
    matches!(
        character,
        '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*'
    ) || character.is_control()
}

fn truncate_to_char_boundary(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }

    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }

    text.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_PAGE: &str = r#"
<html><head><script type="application/json" id="serialized-server-data">
{"data":[{"data":{"sections":[{"itemKind":"squareLockup","items":[{"artwork":{"dictionary":{"width":316,"height":316,"url":"https://is1-ssl.mzstatic.com/image/thumb/Music221/v4/x/thumbnail.jpg/316x316bb.jpg"}}}]},{"itemKind":"containerDetailHeaderLockup","items":[{"title":"From the Toolkit of Lovers","subtitleLinks":[{"title":"Loidis"}],"artwork":{"dictionary":{"width":3000,"height":3000,"url":"https://is1-ssl.mzstatic.com/image/thumb/Music221/v4/x/4062548149920.png/{w}x{h}bb.{f}"}}}]}]}}]}
</script></head></html>
"#;

    #[test]
    fn accepts_apple_music_pages() {
        for url in [
            "https://music.apple.com/us/album/one-day/6769367453",
            "https://www.music.apple.com/us/album/one-day/6769367453",
            "https://music.apple.com/gb/song/one-day/1234567890?i=1234567891",
            "https://music.apple.com/us/playlist/essentials/pl.123456",
            "https://music.apple.com/us/album/6769367453/",
        ] {
            assert!(AppleMusicUrl::parse(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn rejects_invalid_apple_music_pages() {
        let invalid = [
            "https://music.apple.com/",
            "https://music.apple.com/us/artist/artist-name/1234567890",
            "https://music.apple.com/us/album/one-day",
            "https://music.apple.com/us/song/one-day/not-an-id",
            "https://music.apple.com/us/playlist/essentials/not-a-playlist-id",
            "https://music.apple.com/us/album/one-day/1234567890/extra",
            "https://user@music.apple.com/us/album/one-day/1234567890",
            "https://music.apple.com:8443/us/album/one-day/1234567890",
            "https://example.com/us/album/one-day/1234567890",
            "not a URL",
        ];

        for url in invalid {
            assert_eq!(
                AppleMusicUrl::parse(url).unwrap_err(),
                PageError::NotAppleMusicUrl,
                "{url}"
            );
        }

        assert_eq!(
            AppleMusicUrl::parse("http://music.apple.com/us/album/one-day/6769367453").unwrap_err(),
            PageError::InsecureUrl
        );
    }

    #[test]
    fn finds_serialized_server_data() {
        for html in [
            r#"<script type="application/json" id="serialized-server-data">{"data":[]}</script>"#,
            r#"<script type='application/json' id='serialized-server-data'>{"data":[]}</script>"#,
            r#"<script id="serialized-server-data" type="application/json">{"data":[]}</script>"#,
        ] {
            assert_eq!(find_serialized_data(html), Some(r#"{"data":[]}"#));
        }

        assert_eq!(find_serialized_data("<script>alert(1)</script>"), None);
        assert_eq!(
            find_serialized_data(
                r#"<script data-id="serialized-server-data">{"data":[]}</script>"#
            ),
            None
        );
    }

    #[test]
    fn selects_the_main_cover_over_a_thumbnail() {
        let cover = parse_cover(SAMPLE_PAGE).unwrap();

        assert_eq!(
            cover.image_url.as_str(),
            "https://is1-ssl.mzstatic.com/image/thumb/Music221/v4/x/4062548149920.png/3000x3000bb.png"
        );
        assert_eq!(cover.file_name, "Loidis - From the Toolkit of Lovers.png");
    }

    #[test]
    fn requests_the_largest_fallback_for_missing_dimensions() {
        let html = r#"
<script type="application/json" id="serialized-server-data">
{"data":[{"itemKind":"containerDetailHeaderLockup","title":"One Day","subtitleLinks":[{"title":"An Artist"}],"artwork":{"dictionary":{"url":"https://is1-ssl.mzstatic.com/image/thumb/Music221/v4/y/cover.jpg/{w}x{h}bb.{f}?l=en-US"}}}]}
</script>
"#;
        let cover = parse_cover(html).unwrap();

        assert_eq!(
            cover.image_url.as_str(),
            "https://is1-ssl.mzstatic.com/image/thumb/Music221/v4/y/cover.jpg/10000x10000bb.jpg?l=en-US"
        );
    }

    #[test]
    fn rejects_incomplete_cover_metadata() {
        let missing_artist = r#"
<script type="application/json" id="serialized-server-data">
{"data":[{"itemKind":"containerDetailHeaderLockup","title":"One Day","artwork":{"dictionary":{"url":"https://is1-ssl.mzstatic.com/image/thumb/Music221/v4/y/cover.jpg/{w}x{h}bb.{f}"}}}]}
</script>
"#;
        assert_eq!(
            parse_cover(missing_artist).unwrap_err(),
            PageError::MissingMetadata
        );

        let missing_artwork = r#"
<script type="application/json" id="serialized-server-data">
{"data":[{"itemKind":"containerDetailHeaderLockup","title":"One Day","subtitleLinks":[{"title":"An Artist"}]}]}
</script>
"#;
        assert_eq!(
            parse_cover(missing_artwork).unwrap_err(),
            PageError::MissingArtwork
        );

        let untrusted_artwork = r#"
<script type="application/json" id="serialized-server-data">
{"data":[{"itemKind":"containerDetailHeaderLockup","title":"One Day","subtitleLinks":[{"title":"An Artist"}],"artwork":{"dictionary":{"url":"https://example.com/cover.jpg/{w}x{h}bb.{f}"}}}]}
</script>
"#;
        assert_eq!(
            parse_cover(untrusted_artwork).unwrap_err(),
            PageError::InvalidImageUrl
        );
    }

    #[test]
    fn accepts_apple_image_cdn_urls() {
        for url in [
            "https://is1-ssl.mzstatic.com/image/thumb/Music221/v4/x/4062548149920.png/{w}x{h}bb.{f}",
            "https://is1-ssl.mzstatic.com/cover.jpg?l=en-GB",
        ] {
            assert!(validate_image_url(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn rejects_artwork_from_other_hosts() {
        for url in [
            "https://example.com/cover.jpg/{w}x{h}bb.{f}",
            "https://evilmzstatic.com/cover.jpg/{w}x{h}bb.{f}",
            "https://mzstatic.com/cover.jpg/{w}x{h}bb.{f}",
            "http://is1-ssl.mzstatic.com/cover.jpg/{w}x{h}bb.{f}",
            "https://is1-ssl.mzstatic.com:8443/cover.jpg/{w}x{h}bb.{f}",
            "https://user@is1-ssl.mzstatic.com/cover.jpg",
            "not a URL",
        ] {
            assert_eq!(
                validate_image_url(url).unwrap_err(),
                PageError::InvalidImageUrl,
                "{url}"
            );
        }
    }

    #[test]
    fn finds_the_source_image_format() {
        assert_eq!(
            source_format("https://example.com/cover.png/{w}x{h}bb.{f}"),
            "png"
        );
        assert_eq!(
            source_format("https://example.com/cover.jpg/{w}x{h}bb.{f}"),
            "jpg"
        );
        assert_eq!(
            source_format("https://example.com/cover.jpeg/{w}x{h}bb.{f}"),
            "jpg"
        );
        assert_eq!(
            source_format("https://example.com/cover/{w}x{h}bb.{f}"),
            "jpg"
        );
        assert_eq!(source_format("https://example.com/cover.png"), "png");
        assert_eq!(
            source_format("https://example.com/3000x3000bb.jpeg?quality=90"),
            "jpg"
        );
    }

    #[test]
    fn formats_cover_file_names_safely() {
        assert_eq!(
            build_file_name("From the Toolkit of Lovers", "Loidis", "png"),
            "Loidis - From the Toolkit of Lovers.png"
        );
        assert_eq!(
            build_file_name(r#"A/B:C*D?E"F<G>H|I\J"#, "Artist", "jpg"),
            "Artist - A_B_C_D_E_F_G_H_I_J.jpg"
        );
        assert_eq!(
            build_file_name("Title", ".hidden", "jpg"),
            "\u{ff0e}hidden - Title.jpg"
        );
        assert_eq!(
            build_file_name(".hidden", "Artist", "jpg"),
            "Artist - .hidden.jpg"
        );
        assert_eq!(build_file_name("", "", "jpg"), "cover.jpg");
    }

    #[test]
    fn truncates_long_names_without_splitting_utf8() {
        let title = "é".repeat(200);
        let file_name = build_file_name(&title, "Artist", "jpg");

        assert!(file_name.len() <= MAX_FILE_STEM_BYTES + ".jpg".len());
        assert!(
            std::path::Path::new(&file_name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jpg"))
        );
    }
}
