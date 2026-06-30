# viroh.net — marketing site

A static one-page site for **viroh**, served by a Cloudflare Worker (Workers
Static Assets). Styled to match the tinymoq / nvcmoq family: light technical
theme, the gradient-badge logo (a **vI** monogram), and monospace metric chips.

```
site/
├── wrangler.toml        # Worker config — serves ./public as static assets
└── public/
    ├── index.html       # the whole page (self-contained, inline CSS)
    └── favicon.svg      # the vI badge
```

## Preview locally

It's a single self-contained file — just open it:

```sh
open site/public/index.html        # macOS
```

Or run it through Wrangler exactly as it'll deploy:

```sh
cd site
npx wrangler dev          # serves on http://localhost:8787
```

## Deploy (replaces the current "Hello World!" worker on viroh.net)

```sh
cd site
npx wrangler deploy       # prompts a browser login the first time
```

Then point the domain at it: Cloudflare dashboard → Workers & Pages → the
`viroh` worker → **Settings → Domains & Routes** → add `viroh.net` as a custom
domain. (Or uncomment the `routes` block in `wrangler.toml` and redeploy.)

> Deploying needs your Cloudflare account, so it isn't done from CI here — run
> `wrangler deploy` from a machine logged into that account.

## Editing

Everything is in `public/index.html` — content, copy, and CSS in one `<style>`
block, no build step and no external dependencies. The logo lives both as
`public/favicon.svg` and as inline `<svg>` in the header/footer; keep them in
sync if you change the mark.
