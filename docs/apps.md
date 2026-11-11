# Flipper applications (FAP) and app sources

The **Apps** tab and the `flipper apps` commands browse and install Flipper
applications.  Two repository types are supported.

## 1. Official catalog

The official catalog is queried through its JSON API at
`https://catalog.flipperzero.one/api`:

- `GET /v0/0/category?api=<sdk>&target=<f7|f18>` — categories;
- `POST /v0/1/application` — application list (search, category, target, API);
- `GET /v0/0/application/<id>?target=&api=` — details;
- `GET /v0/0/application/version/<version>/build/compatible?target=&api=` —
  the compiled `.fap`.

The client derives `target` from `hardware.target` and the SDK API from
`firmware.api.major` / `firmware.api.minor` in the device's `devinfo`
property.  Apps whose build state is not `READY` are hidden.  The list API
caps a page at 500 entries, so the client follows pagination until the whole
catalog is loaded.

Official downloads are verified against the `fap_hash` published for the
build, so a corrupted or truncated bundle is rejected before anything is
written to the SD card.

Installed files are written to:

```text
/ext/apps/<Category>/<alias>.fap
```

## 2. Custom JSON sources

A custom source is a JSON document served over HTTP(S):

```json
{
  "name": "My Flipper apps",
  "apps": [
    {
      "id": "my_timer",
      "alias": "my_timer",
      "name": "My Timer",
      "version": "1.2",
      "category": "Tools",
      "author": "example",
      "description": "A small timer.",
      "target": "f7",
      "api": "88.2",
      "url": "https://example.com/my_timer.fap",
      "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    }
  ]
}
```

Fields:

| Field | Required | Notes |
|---|---|---|
| `name` (top level) | no | Displayed as the source name |
| `id` | no | Defaults to empty; used by `apps install` |
| `alias` | no | Used for the local `.fap` file name |
| `name` | yes | Application name |
| `version` | yes | Displayed version |
| `category` | no | Defaults to `Tools`; becomes the remote folder |
| `description` | no | Shown in the Apps tab |
| `target` | no | `any`, `f7`, `f18`; sources are filtered by device target |
| `api` | no | `any` or a specific SDK API; filtered by device API |
| `url` | yes | Direct HTTP(S) link to a `.fap` |
| `sha256` | no | Verified before installation |

Add a source with:

```sh
flipper apps sources add https://example.com/apps.json
flipper apps sources list
```

Sources are stored in:

```text
~/.config/flipper-companion/app-sources.txt
```

(one URL per line, `#` comments allowed).

## 3. Selecting a repository

The Apps tab shows a repository selector next to the Catalog/Installed switch:

- **All sources** — the official catalog plus every configured JSON index;
- **Official catalog** — only `catalog.flipperzero.one`;
- **`https://…`** — one custom index.

Selecting a repository reloads the list from it, and the selector always shows
the repository currently in use.  The `− Source` button removes the selected
custom index (the official catalog cannot be removed).

## 4. Installed apps

The **Installed** page lists every `.fap` found under `/ext/apps`, with its
category, size and remote path.  Select one and press **🗑 Uninstall** to
remove it; the list refreshes after the operation.

## 5. Updates

The **Installed** page can compare the SD card with the catalog:

- **Check updates** runs in the background: every installed `.fap` is read
  once and its SHA-256 compared with the catalog `fap_hash`.  The status bar
  shows `Calculating checksum for evaluating updates — 26/162: name` with a
  progress bar, and the list is refreshed after each comparison, so rows turn
  from `✓ installed` to `✓ up to date` or `⇧ update available` as they are
  processed.  Rows with an available update get a per-app **Update** button.
- Each installed row shows the **remote version** next to the local category
  and size (`remote N/A` when the app is not in the selected repository) and
  carries its own **Verify** button for a one-off exact check.

The CLI keeps a fast size-based comparison in `flipper apps outdated`.

Sizes are a fast heuristic; the SHA-256 verification is authoritative.

## 6. CLI

```sh
flipper apps list
flipper apps list --query timer --category Tools
flipper apps list --source official
flipper apps install cube_timer --yes
flipper apps installed
flipper apps outdated
flipper apps uninstall cube_timer --yes
flipper apps sources add https://example.com/apps.json
flipper apps sources list
```

`apps list` detects the connected device's target/API.  When no device is
connected, pass `--target f7 --api 88.2` explicitly.

## 7. GUI

The **Apps** tab is a single table built by merging the selected repository
with the applications present on the SD card.  Columns are **Application**,
**Version**, **Size**, **Repository** and **Update**; clicking a header sorts
by that column (click again to reverse).  Each row has a checkbox on the left (enabled when
an update is available): tick several apps and press **Update selected**, or
use the header checkbox to select every visible update.  Right-clicking a row
opens the actions menu:

- **Install** (download icon) when the app is not installed;
- **Verify** (document icon) when it is installed — exact SHA-256 check;
- **Update** (accented download icon) when the installed file differs;
- **Uninstall** (trash icon) when it is installed.

The toolbar holds the search field, a filter (*All apps*, *Installed*,
*Updates*), `Refresh`, `Check updates` and `Repositories`.  Repository
management (selector, remove, add a JSON index) lives in the collapsible
**Repositories** panel, so it stays out of the way.

## 8. Safety

- Only install FAPs built for the device's SDK API and target.  The official
  catalog is filtered automatically; custom sources are filtered when they
  provide `target`/`api`.
- A checksum mismatch aborts the installation before any write.
- The `.fap` is written with `Storage_write`; the device firmware only answers
  the last chunk, so multi-chunk writes are sent back-to-back and acknowledged
  once at the end.
