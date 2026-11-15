# mem.ex8.ca — project landing page

Static, single-file site. No build step: `index.html` plus the four screenshots in `img/`.

## Deploy

Deployed to Cloudflare Pages project `memex8` (custom domain `mem.ex8.ca`) by direct upload:

```bash
cd site
CLOUDFLARE_ACCOUNT_ID=... CLOUDFLARE_API_TOKEN=... \
  npx wrangler pages deploy . --project-name=memex8 --branch=main --commit-dirty=true
```

## Conventions

- **Content mirrors the repo and GitHub.** Release notes, benchmarks and feature list come from
  `README.md`, `/releases` and merged PRs — update them here in the same PR that changes them.
- **Theme:** `data-theme="light|dark"` on `<html>`. The three-way switch in the header stores
  `memex8-theme` in `localStorage` (`system` | `light` | `dark`) and an inline `<head>` script applies
  it before first paint, so there is no flash. `system` follows `prefers-color-scheme` live.
- **Palette and type** follow chillygeek.com: Newsreader (display), DM Sans (body), DM Mono (labels,
  code). Colours live in the `:root` / `html[data-theme="dark"]` custom properties — change them
  there, not in individual rules.
- No frameworks, no icon font, no analytics. Keep it that way: the page is embedded-adjacent branding
  for a project whose selling point is "no CDN, no build step".
