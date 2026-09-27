//! Bandcamp — whether a record can be bought there, in which formats, and
//! for what.
//!
//! Bandcamp has no public catalog API: its official one reports only on the
//! caller's own account (sales, merch orders). This reads what the website
//! itself runs on. The site search's autocomplete endpoint finds the album
//! page, and the page answers the rest: the schema.org JSON-LD every album
//! page carries lists each format with its price, currency and stock state,
//! and the page's `data-tralbum` blob says how many copies are left. Both are
//! undocumented, so every reading is best effort: a page that changed shape
//! reads as "not on Bandcamp", never as an error the user has to act on.
//!
//! Engine-shaped per `ordnung-architecture`: the caller decides when to ask.
//! One lookup is two requests (search, album page), meant for a record the
//! user opened, never a crawl. Prices are live answers and are never cached.

use crate::discogs::MarketPrice;
use crate::error::{Error, Result};
use crate::tracklist::fold_name;
use serde::Deserialize;

const SEARCH_URL: &str = "https://bandcamp.com/api/bcsearch_public_api/1/autocomplete_elastic";

/// Remaining-copy counts above this read as "plenty" and aren't worth a word.
pub const FEW_LEFT: u32 = 10;

/// An album page on Bandcamp and what it sells.
#[derive(Debug, Clone, PartialEq)]
pub struct Album {
    /// The album page.
    pub url: String,
    pub title: String,
    pub artist: String,
    /// Every format the page sells, digital first, in page order. Merch
    /// (shirts, bags) is left out: the question is whether the record can be
    /// bought.
    pub offers: Vec<Offer>,
}

/// One format an album page sells.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    /// As the page names it: `Black Vinyl`, `Cocoon Crush 2x LP`, the album
    /// title for the digital download.
    pub name: String,
    pub medium: Medium,
    /// A zero price on a digital offer is "name your price".
    pub price: MarketPrice,
    pub availability: Availability,
    /// Copies left, when the seller caps the stock and says so.
    pub remaining: Option<u32>,
    /// This format's buy link on the album page.
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Medium {
    Digital,
    Vinyl,
    Cd,
    Cassette,
    /// A physical format Bandcamp has a name for that isn't one of the above.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    InStock,
    SoldOut,
    PreOrder,
}

impl Album {
    /// The cheapest offer of `medium` that can be bought now (in stock or on
    /// preorder).
    pub fn cheapest(&self, medium: Medium) -> Option<&Offer> {
        self.offers
            .iter()
            .filter(|o| o.medium == medium && o.availability != Availability::SoldOut)
            .min_by(|a, b| a.price.value.total_cmp(&b.price.value))
    }

    /// Whether the page lists `medium` at all, bought out or not.
    pub fn has(&self, medium: Medium) -> bool {
        self.offers.iter().any(|o| o.medium == medium)
    }
}

/// One album the site search returned.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub title: String,
    /// The artist as the page credits it; on a label's account this is still
    /// the artist, not the label.
    pub artist: String,
    pub url: String,
}

impl SearchHit {
    /// The account the page lives on, `batutimedance.bandcamp.com`.
    pub fn host(&self) -> &str {
        host_of(&self.url).unwrap_or("")
    }
}

/// The host of an `http(s)://host/...` URL, lowercased by the caller's
/// comparison rather than here.
fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then_some(host)
}

/// The Bandcamp accounts among a Discogs profile's links, as hosts:
/// `https://hyperdub.bandcamp.com/` → `hyperdub.bandcamp.com`. What
/// [`pick`] trusts over a re-upload of the same album on someone else's
/// account.
pub fn bandcamp_hosts(urls: &[String]) -> Vec<String> {
    urls.iter()
        .filter_map(|u| host_of(u.trim()))
        .map(|h| h.to_ascii_lowercase())
        .filter(|h| h.ends_with(".bandcamp.com") && h != "www.bandcamp.com")
        .collect()
}

/// A title reduced to what two spellings of the same release share: folded
/// like any name, with a trailing `EP` / `LP` dropped, since Discogs and
/// Bandcamp disagree on whether the format is part of the title.
fn title_key(s: &str) -> String {
    let f = fold_name(s);
    for tail in [" ep", " lp", " e p", " 12"] {
        if let Some(t) = f.strip_suffix(tail) {
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    f
}

/// Letters and digits only, for comparing a name against an account
/// subdomain: `Call Super` → `callsuper`.
fn compact(s: &str) -> String {
    fold_name(s).chars().filter(|c| c.is_alphanumeric()).collect()
}

/// Whether a search hit is this release: the same title, by one of the
/// credited artists. `names` are the release's artists followed by its
/// labels; a label counts because a compilation's page is credited to the
/// label (or to "Various Artists"). Strict on purpose: a sheet that says a
/// record is on Bandcamp when it's a different record is worse than one that
/// says nothing, so `Arpo` never matches `Arpo Low`.
fn agrees(hit: &SearchHit, names: &[&str], title: &str) -> bool {
    let want = title_key(title);
    if want.is_empty() {
        return false;
    }
    let artist = fold_name(&hit.artist);
    let various = |s: &str| s == "various" || s == "various artists";
    let names: Vec<String> = names.iter().map(|n| fold_name(n)).filter(|n| !n.is_empty()).collect();
    let credited = names.iter().any(|n| *n == artist || (various(n) && various(&artist)));
    let got = title_key(&hit.title);
    if credited && got == want {
        return true;
    }
    // A label account that titles its pages `Artist - Title`.
    names.iter().any(|n| {
        got.strip_prefix(n.as_str())
            .and_then(|t| t.strip_prefix(' '))
            .is_some_and(|t| title_key(t) == want)
    })
}

/// The search hit that is this release, if any. Among several that agree
/// (the album on the artist's account, again on a label's, again as someone
/// else's re-upload) the one on an account in `official` wins, then one whose
/// subdomain is an artist's or label's name, then search order. `official` is
/// only asked when there is a choice to make: resolving it costs the caller a
/// request.
pub fn pick<'a>(
    hits: &'a [SearchHit],
    names: &[&str],
    title: &str,
    official: impl FnOnce() -> Vec<String>,
) -> Option<&'a SearchHit> {
    let agreeing: Vec<&SearchHit> = hits.iter().filter(|h| agrees(h, names, title)).collect();
    if agreeing.len() <= 1 {
        return agreeing.first().copied();
    }
    let official = official();
    let named: Vec<String> = names.iter().map(|n| compact(n)).filter(|n| !n.is_empty()).collect();
    let rank = |h: &SearchHit| {
        let host = h.host().to_ascii_lowercase();
        let sub = host.strip_suffix(".bandcamp.com").unwrap_or(&host);
        if official.iter().any(|o| *o == host) {
            0
        } else if named.iter().any(|n| n == sub) {
            1
        } else {
            2
        }
    };
    agreeing.into_iter().min_by_key(|h| rank(h))
}

/// Bandcamp over the shared HTTP pool.
#[derive(Clone)]
pub struct Client {
    user_agent: String,
    agent: ureq::Agent,
}

impl Client {
    pub fn new(user_agent: impl Into<String>) -> Self {
        Client {
            user_agent: user_agent.into(),
            agent: crate::discogs::shared_agent(),
        }
    }

    /// Albums the site search returns for `query`, best first. One request.
    pub fn search_albums(&self, query: &str) -> Result<Vec<SearchHit>> {
        let body = serde_json::json!({
            "search_text": query,
            "search_filter": "a",
            "full_page": false,
            "fan_id": null,
        });
        let resp: SearchResponse = self
            .agent
            .post(SEARCH_URL)
            .set("User-Agent", &self.user_agent)
            .send_json(body)
            .map_err(net)?
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Bandcamp search: {e}")))?;
        Ok(resp
            .auto
            .results
            .into_iter()
            .filter(|r| r.kind == "a" && !r.item_url_path.is_empty())
            .map(|r| SearchHit {
                title: r.name,
                artist: r.band_name,
                url: r.item_url_path,
            })
            .collect())
    }

    /// Read an album page. `Ok(None)` when the page is gone or no longer
    /// carries the data this reads. One request.
    pub fn fetch_album(&self, url: &str) -> Result<Option<Album>> {
        let resp = match self.agent.get(url).set("User-Agent", &self.user_agent).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(404 | 410, _)) => return Ok(None),
            Err(e) => return Err(net(e)),
        };
        let html = resp
            .into_string()
            .map_err(|e| Error::Network(format!("reading Bandcamp page: {e}")))?;
        Ok(parse_album_page(&html))
    }

    /// The Bandcamp page of a release, found by its artists, title and
    /// labels, and read. `artists` and `labels` as the release credits them;
    /// the first artist (the first label, on a compilation) and the title
    /// make the search. `official` supplies the Bandcamp hosts the artists'
    /// and labels' own profiles link to, asked only when two pages agree (see
    /// [`pick`]). Two requests; `Ok(None)` when Bandcamp doesn't have it.
    pub fn find_album(
        &self,
        artists: &[String],
        labels: &[String],
        title: &str,
        official: impl FnOnce() -> Vec<String>,
    ) -> Result<Option<Album>> {
        let lead = artists
            .iter()
            .map(|a| a.trim())
            .find(|a| !a.is_empty() && !fold_name(a).starts_with("various"))
            .or_else(|| labels.iter().map(|l| l.trim()).find(|l| !l.is_empty()));
        let query = match lead {
            Some(lead) => format!("{lead} {title}"),
            None => title.to_string(),
        };
        let hits = self.search_albums(query.trim())?;
        let names: Vec<&str> = artists.iter().chain(labels).map(String::as_str).collect();
        let Some(hit) = pick(&hits, &names, title, official) else {
            return Ok(None);
        };
        self.fetch_album(&hit.url)
    }
}

fn net(e: ureq::Error) -> Error {
    match e {
        ureq::Error::Status(code, _) => Error::Network(format!("Bandcamp answered HTTP {code}")),
        ureq::Error::Transport(_) => Error::Network("couldn't reach Bandcamp".to_string()),
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    auto: SearchAuto,
}

#[derive(Deserialize, Default)]
struct SearchAuto {
    #[serde(default)]
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    band_name: String,
    #[serde(default)]
    item_url_path: String,
}

/// An album page's JSON-LD and stock counts as an [`Album`]. `None` when the
/// page carries no album JSON-LD.
pub fn parse_album_page(html: &str) -> Option<Album> {
    let ld = json_ld(html)?;
    let url = ld.get("@id").and_then(|v| v.as_str())?.to_string();
    let title = ld.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let artist = ld
        .pointer("/byArtist/name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let left = remaining_by_package(html);
    let mut offers: Vec<Offer> = ld
        .get("albumRelease")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let medium = match r.get("musicReleaseFormat").and_then(|v| v.as_str())? {
                "DigitalFormat" => Medium::Digital,
                "VinylFormat" => Medium::Vinyl,
                "CDFormat" => Medium::Cd,
                "CassetteFormat" => Medium::Cassette,
                _ => Medium::Other,
            };
            let o = r.get("offers")?;
            let value = o.get("price").and_then(|v| v.as_f64())?;
            let currency = o.get("priceCurrency").and_then(|v| v.as_str())?.to_string();
            let availability = match o.get("availability").and_then(|v| v.as_str()) {
                Some(a) if a.ends_with("SoldOut") || a.ends_with("OutOfStock") => {
                    Availability::SoldOut
                }
                Some(a) if a.ends_with("PreOrder") => Availability::PreOrder,
                _ => Availability::InStock,
            };
            let buy = o
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or(&url)
                .to_string();
            // Package buy links end `#p{package id}-buy`.
            let remaining = buy
                .rsplit_once("#p")
                .and_then(|(_, t)| t.strip_suffix("-buy"))
                .and_then(|id| id.parse::<u64>().ok())
                .and_then(|id| left.iter().find(|(p, _)| *p == id))
                .map(|(_, n)| *n);
            Some(Offer {
                name: r.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                medium,
                price: MarketPrice { value, currency },
                availability: if remaining == Some(0) {
                    Availability::SoldOut
                } else {
                    availability
                },
                remaining,
                url: buy,
            })
        })
        .collect();
    // Digital first, then the page's own order.
    offers.sort_by_key(|o| o.medium != Medium::Digital);
    Some(Album {
        url,
        title,
        artist,
        offers,
    })
}

/// The album object among the page's JSON-LD scripts.
fn json_ld(html: &str) -> Option<serde_json::Value> {
    let mut rest = html;
    while let Some(at) = rest.find("application/ld+json") {
        rest = &rest[at..];
        let open = rest.find('>')? + 1;
        let close = rest[open..].find("</script>")? + open;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(rest[open..close].trim()) {
            if v.get("albumRelease").is_some() {
                return Some(v);
            }
        }
        rest = &rest[close..];
    }
    None
}

/// `(package id, copies left)` from the page's `data-tralbum` attribute, for
/// the packages that state a count.
fn remaining_by_package(html: &str) -> Vec<(u64, u32)> {
    let Some(at) = html.find("data-tralbum=\"") else {
        return Vec::new();
    };
    let start = at + "data-tralbum=\"".len();
    let Some(len) = html[start..].find('"') else {
        return Vec::new();
    };
    let json = unescape_attr(&html[start..start + len]);
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else {
        return Vec::new();
    };
    v.get("packages")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let id = p.get("id")?.as_u64()?;
            let n = p.get("quantity_available")?.as_u64()?;
            Some((id, u32::try_from(n).unwrap_or(u32::MAX)))
        })
        .collect()
}

/// Undo HTML attribute escaping: the named entities an attribute uses and
/// numeric references.
fn unescape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest.bytes().take(12).position(|b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let ch = match entity {
            "quot" => Some('"'),
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "apos" => Some('\''),
            e if e.starts_with("#x") || e.starts_with("#X") => {
                u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32)
            }
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(title: &str, artist: &str, url: &str) -> SearchHit {
        SearchHit {
            title: title.into(),
            artist: artist.into(),
            url: url.into(),
        }
    }

    /// Trimmed from a real album page: the JSON-LD with a digital offer, a
    /// vinyl in stock, a sold-out vinyl and a hoodie, plus the tralbum blob
    /// with the stock counts.
    const PAGE: &str = r#"<html><head>
<script type="application/ld+json">
{"@type":"MusicAlbum","@id":"https://batutimedance.bandcamp.com/album/opal","name":"Opal",
 "byArtist":{"@type":"MusicGroup","name":"Batu"},
 "albumRelease":[
  {"@id":"https://batutimedance.bandcamp.com/album/opal","name":"Opal","musicReleaseFormat":"DigitalFormat",
   "offers":{"@type":"Offer","url":"https://batutimedance.bandcamp.com/album/opal#a1655969835-buy","priceCurrency":"GBP","price":7.99,"availability":"OnlineOnly"}},
  {"@id":"https://batutimedance.bandcamp.com/album/opal#p3146011051","name":"Black Vinyl","musicReleaseFormat":"VinylFormat",
   "offers":{"@type":"Offer","url":"https://batutimedance.bandcamp.com/album/opal#p3146011051-buy","priceCurrency":"GBP","price":17.99,"availability":"InStock"}},
  {"@id":"https://batutimedance.bandcamp.com/album/opal#p757234295","name":"Hooded Sweatshirt",
   "offers":{"@type":"Offer","url":"https://batutimedance.bandcamp.com/album/opal#p757234295-buy","priceCurrency":"GBP","price":40.0,"availability":"InStock"}},
  {"@id":"https://batutimedance.bandcamp.com/album/opal#p294430682","name":"Marble Vinyl","musicReleaseFormat":"VinylFormat",
   "offers":{"@type":"Offer","url":"https://batutimedance.bandcamp.com/album/opal#p294430682-buy","priceCurrency":"GBP","price":21.99,"availability":"SoldOut"}},
  {"@id":"https://batutimedance.bandcamp.com/track/solace"}
 ]}
</script></head><body>
<script data-tralbum="{&quot;packages&quot;:[{&quot;id&quot;:3146011051,&quot;title&quot;:&quot;Black Vinyl&quot;,&quot;quantity_available&quot;:4},{&quot;id&quot;:294430682,&quot;quantity_available&quot;:0},{&quot;id&quot;:757234295,&quot;quantity_available&quot;:null}]}"></script>
</body></html>"#;

    #[test]
    fn album_page_reads_formats_prices_and_stock() {
        let a = parse_album_page(PAGE).expect("album");
        assert_eq!(a.url, "https://batutimedance.bandcamp.com/album/opal");
        assert_eq!(a.title, "Opal");
        assert_eq!(a.artist, "Batu");
        // The hoodie and the track entry are not formats of the record.
        let names: Vec<&str> = a.offers.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["Opal", "Black Vinyl", "Marble Vinyl"]);
        assert_eq!(a.offers[0].medium, Medium::Digital);
        assert_eq!(a.offers[0].price, MarketPrice { value: 7.99, currency: "GBP".into() });
        assert_eq!(a.offers[1].remaining, Some(4));
        assert_eq!(a.offers[1].availability, Availability::InStock);
        assert_eq!(a.offers[2].availability, Availability::SoldOut);
        assert!(a.offers[1].url.ends_with("#p3146011051-buy"));
        // Only the black one can be bought.
        assert_eq!(a.cheapest(Medium::Vinyl).map(|o| o.name.as_str()), Some("Black Vinyl"));
        assert!(a.has(Medium::Vinyl) && !a.has(Medium::Cd));
    }

    #[test]
    fn a_page_without_album_data_reads_as_nothing() {
        assert_eq!(parse_album_page("<html><body>Sorry, that something isn't here.</body></html>"), None);
    }

    #[test]
    fn a_stock_count_of_zero_is_sold_out_whatever_the_offer_says() {
        let page = PAGE.replace("quantity_available&quot;:4", "quantity_available&quot;:0");
        let a = parse_album_page(&page).unwrap();
        assert_eq!(a.offers[1].availability, Availability::SoldOut);
        assert_eq!(a.cheapest(Medium::Vinyl), None);
    }

    #[test]
    fn pick_is_strict_about_the_title() {
        let hits = [
            hit("Arpo Low", "Call Super", "https://callsuper.bandcamp.com/album/arpo-low"),
            hit("Arpo", "Call Super", "https://callsuper.bandcamp.com/album/arpo"),
        ];
        let got = pick(&hits, &["Call Super"], "Arpo", Vec::new).unwrap();
        assert_eq!(got.url, "https://callsuper.bandcamp.com/album/arpo");
        assert_eq!(pick(&hits[..1], &["Call Super"], "Arpo", Vec::new), None);
        // Another artist's album of the same name isn't this one.
        let other = [hit("Arpo", "Somebody", "https://somebody.bandcamp.com/album/arpo")];
        assert_eq!(pick(&other, &["Call Super"], "Arpo", Vec::new), None);
    }

    #[test]
    fn pick_reads_past_format_words_and_artist_prefixed_titles() {
        let hits = [hit("Kill Switch", "DJ Stingray", "https://x.bandcamp.com/album/kill-switch")];
        assert!(pick(&hits, &["DJ Stingray"], "Kill Switch EP", Vec::new).is_some());
        let label = [hit("DJ Stingray - Kill Switch", "Some Label", "https://somelabel.bandcamp.com/album/ks")];
        assert!(pick(&label, &["DJ Stingray", "Some Label"], "Kill Switch", Vec::new).is_some());
        // A compilation is credited to the label, or to Various Artists.
        let comp = [hit("Summer Sampler", "Various Artists", "https://l.bandcamp.com/album/s")];
        assert!(pick(&comp, &["Various", "L"], "Summer Sampler", Vec::new).is_some());
    }

    #[test]
    fn pick_prefers_the_official_account_over_a_reupload() {
        let hits = [
            hit("Untrue", "Burial", "https://dubstepessentials.bandcamp.com/album/untrue"),
            hit("Untrue", "Burial", "https://hyperdub.bandcamp.com/album/untrue"),
        ];
        let official = || vec!["hyperdub.bandcamp.com".to_string()];
        let got = pick(&hits, &["Burial", "Hyperdub"], "Untrue", official).unwrap();
        assert!(got.url.starts_with("https://hyperdub."));
        // Without a profile link, an account named after the artist wins.
        let hits = [hits[0].clone(), hit("Untrue", "Burial", "https://burial.bandcamp.com/album/untrue")];
        let got = pick(&hits, &["Burial", "Hyperdub"], "Untrue", Vec::new).unwrap();
        assert!(got.url.starts_with("https://burial."));
    }

    #[test]
    fn official_is_only_asked_when_there_is_a_choice() {
        let hits = [hit("Untrue", "Burial", "https://burial.bandcamp.com/album/untrue")];
        let got = pick(&hits, &["Burial"], "Untrue", || panic!("asked for one hit"));
        assert!(got.is_some());
    }

    #[test]
    fn bandcamp_hosts_come_out_of_profile_links() {
        let urls = vec![
            "https://hyperdub.bandcamp.com/".to_string(),
            "http://www.hyperdub.net".to_string(),
            "https://Timedance.Bandcamp.com/music".to_string(),
            "https://bandcamp.com/".to_string(),
        ];
        assert_eq!(bandcamp_hosts(&urls), ["hyperdub.bandcamp.com", "timedance.bandcamp.com"]);
    }

    #[test]
    fn attribute_unescape_handles_named_and_numeric_entities() {
        assert_eq!(unescape_attr("&quot;a&amp;b&#39;c&#x27;&lt;&gt;"), "\"a&b'c'<>");
        assert_eq!(unescape_attr("R&B &unknown; &"), "R&B &unknown; &");
        assert_eq!(unescape_attr("&ééééééé;"), "&ééééééé;");
    }
}
