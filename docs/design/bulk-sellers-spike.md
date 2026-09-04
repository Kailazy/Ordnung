# Spike: "Bulk Sellers" — surface sellers holding several wantlist records

**Status:** investigation only, nothing implemented.
**Goal:** a new tab in the Vinyl view that answers *"who is selling more than one
record I want, so I can combine an order and pay shipping once?"* — the local
equivalent of Discogs's `/sell/mywants` seller facet.

## Verdict up front

The obvious implementation is **not available**: Discogs removed the public
release→sellers endpoint. But a genuinely useful version *is* buildable from
endpoints that still work, by inverting the query — with one honest limitation
that has to be a product decision, not a hidden caveat.

## What was probed (live, against the real token, 2026-09-03)

| Endpoint | Result | Use |
|---|---|---|
| `GET /marketplace/search?release_id=` | **404** *"resource was not found"* (401 unauthenticated) | Gone. This was the endpoint that did exactly what we want. |
| `GET /marketplace/stats/{id}` | **200** `num_for_sale`, `lowest_price` | Already used by the app. Counts only — **no seller identity**. |
| `GET /marketplace/listings/{id}` | 200 for a known id | Needs a listing id we can't enumerate. |
| `GET /users/{seller}/inventory` | **200 — works with a plain token** | **The way in.** `juno_records` → 43,917 listings. |
| `GET /users/{u}/wants` | 200 | Already implemented (`fetch_wantlist_for`). Real wantlist: **106 releases**. |
| `www.discogs.com/sell/release/{id}` (HTML) | **403 Cloudflare managed challenge** | Even `/robots.txt` is challenged. |

Sampling 12 real wantlist releases: **9 of 12 had copies for sale, 77 listings
total** — the demand side of this feature is real, the data is just addressed
from the wrong end.

### The shape of the problem

Discogs will tell us **seller → what they stock**, never **release → who stocks it**.
`/marketplace/stats` proves a release *has* 18 copies but names none of the sellers.
So there is no direct query; the index has to be built by inversion.

### Why scraping is the wrong answer here

The one library that solves this properly (`michaelhball/discogs_alert`) documents
the cost in its own source: `/sell/release/{id}` sits behind Cloudflare TLS
fingerprinting, so it needs `curl_cffi` to impersonate Chrome's JA3 signature.
Verified: a normal `curl` with a real Chrome UA gets **403**. Adopting that means
shipping a fingerprint-spoofing HTTP stack in a Rust desktop app, in an arms race
with Cloudflare, against Discogs's ToS — for a feature that breaks silently the
day the markup or the challenge changes. Its own README notes the API token
"can only be used to access the music database features, not the marketplace."
**Recommend against.**

## Recommended approach — "Seller sweep" (inventory inversion)

Query the sellers instead of the releases:

1. Take the wantlist (already cached locally — no new fetch).
2. Maintain a **seller pool**: usernames worth checking. Seeded and grown from
   sellers the user has actually bought from (`GET /marketplace/orders` — works,
   returns 200), plus any seller the user pins by name.
3. For each seller, page `/users/{seller}/inventory?status=For Sale` and intersect
   `listing.release.id` against the wantlist release ids.
4. Persist the intersection in a new `vinyl_bulk_sellers` cache table.
5. Rank sellers by *how many* wants they hold, then by combined price / ships-from.

Every listing carries what the UI needs, confirmed live:
`price {value,currency}`, `condition`, `sleeve_condition`, `ships_from`,
`seller {id, username, shipping}`, `allow_offers`, `uri`, and full `release`
metadata (id, artist, title, thumbnail) — so a result row can render as a proper
record card with cover art, reusing the existing vinyl cover cache.

### The honest limitation

This finds bulk opportunities **among sellers we know to look at** — it cannot
discover an unknown seller in the long tail the way `/sell/mywants` can, because
that requires the endpoint Discogs removed. This is the tradeoff to accept or
reject; it should be stated in the UI, not papered over.

It is still worth building: the sellers a digger repeatedly buys from *are* the
ones they'd combine an order with, and Discogs's own page can't be sorted,
filtered by condition, or cross-referenced against the local catalog — which this
can.

### Cost (measured, not estimated)

Rate limit confirmed at **60 authenticated req/min** (`x-discogs-ratelimit: 60`),
and `per_page` is **hard-capped at 100** — asking for 250 or 500 silently returns 100.

- A 5,000-item seller = 50 requests ≈ **50 s** at the existing throttle.
- A large distributor (Juno, 43,917 items) = **440 requests ≈ 7.5 min**.

So this must be an **explicit, user-invoked, backgrounded sweep with visible
progress and a cancel** — never an automatic refresh. That also keeps it on the
right side of hard rule #1 (*explicit-only*): a sweep happens because the user
pressed "Check sellers", never as a side effect of opening a tab. Cache results
with a timestamp and show their age, so the tab is instant on open and re-sweeps
only on request.

A per-seller cap (e.g. stop after N pages) keeps the worst case bounded, and
sellers can be swept newest-inventory-first.

## Where the code goes (per `ordnung-architecture`)

- **`ordnung-core/src/discogs.rs`** — `fn seller_inventory(&self, username, page) -> Result<InventoryPage>`
  and a `Listing` struct, alongside the existing `marketplace_price`. Reuses
  `call_with_retry` + the global throttle, so pacing and 429 backoff come free.
- **`ordnung-core/src/catalog.rs`** — `vinyl_bulk_sellers` table (seller, release_id,
  price, currency, condition, ships_from, listing_uri, fetched_at) + the query that
  ranks sellers by want-count. All reusable logic stays in core, per the crate rules.
- **`ordnung-gui`** — a third tab beside Collection/Wantlist in `vinyl_tabs()`
  (`views.rs:67`), rendering grouped-by-seller rows. The sweep runs on the existing
  background-job pattern in `jobs.rs`; `LibraryView` needs no new variant since the
  Vinyl view already owns its tab state (`vinyl_tab: VinylList`) — that enum gains a
  `BulkSellers` variant, or the tab state widens beside it.
- **No new dependencies.** `ureq` + the existing throttle cover it.

## UI sketch

A third tab `Bulk Sellers (N)`. Each row: seller name, "**4 of your wants**",
combined subtotal, ships-from, and the four covers inline — click a cover to open
the existing record sheet, click the row to open the seller's Discogs page in a
browser. Sort by want-count (default), subtotal, or ships-from. Header line states
the scope honestly: *"Checked 12 sellers · updated 2h ago"* with a **Check sellers**
button.

## Recommendation

Build it as scoped above (core client + cache + tab, no scraping), and be explicit
in the UI that it sweeps known sellers rather than all of Discogs. If full
long-tail discovery is essential, the only route is the Cloudflare-challenged HTML
path, which I'd advise against shipping — the maintenance and ToS exposure outweigh
the feature.

An alternative worth noting: the app already embeds a `WKWebView` (`webview.rs`,
used for YouTube). Pointing it at `discogs.com/sell/mywants` would render the real
page, logged in as the user, with zero scraping and zero ToS risk — but it's a
browser panel, not integrated data: no sorting against the local catalog, no cover
grid, no cross-referencing. Cheap fallback, materially weaker feature.
